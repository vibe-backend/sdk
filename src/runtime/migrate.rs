use crate::manifest::Manifest;

use super::exec::PgExec;
use super::slugs::valid_tenant;
use super::sql::{
    bootstrap_statements, quote, rls_statements, runtime_grant_statements,
    runtime_schema_statements, Col,
};

/// Bootstraps one tenant schema and applies migrations, connected as the
/// owner role. Grants the runtime role SELECT/INSERT/UPDATE only.
/// Returns progress lines.
///
/// Runs in one transaction: if any step (including the RLS checks) fails,
/// nothing is committed, so a failed migration never leaves a table without
/// row level security.
pub async fn migrate_tenant(
    exec: &PgExec,
    manifest: &Manifest,
    tenant: &str,
    runtime_role: &str,
) -> Result<Vec<String>, String> {
    if !valid_tenant(tenant) {
        return Err("tenant id must match ^[a-z0-9]{8,32}$".into());
    }
    // Refuse transaction control up front: a COMMIT inside migration SQL
    // would end vibe's transaction and commit tables before RLS is applied.
    for migration in &manifest.migrations {
        for statement in &migration.statements {
            if let Some(word) = transaction_control(statement) {
                return Err(format!(
                    "{}: transaction control ({word}) is not allowed in migrations; vibe runs every migration in its own transaction",
                    migration.id
                ));
            }
        }
    }
    // REPEATABLE READ: the catalog snapshot used for the cross-tenant checks
    // below must not move while other tenants migrate concurrently.
    exec.batch(&["BEGIN ISOLATION LEVEL REPEATABLE READ".to_string()])
        .await?;
    // Marker that only lives as long as this transaction. If it is gone at
    // the end, the transaction was ended (and maybe restarted) in between.
    let marker = uuid::Uuid::new_v4().simple().to_string();
    let result = async {
        exec.batch(&[format!("SET LOCAL vibe.migrate_tx = '{marker}'")])
            .await?;
        let log = migrate_in_tx(exec, manifest, tenant, runtime_role).await?;
        still_in_transaction(exec, &marker).await?;
        Ok::<_, String>(log)
    }
    .await;
    match result {
        Ok(log) => {
            exec.batch(&["COMMIT".to_string()]).await?;
            Ok(log)
        }
        Err(err) => {
            let _ = exec.batch(&["ROLLBACK".to_string()]).await;
            Err(err)
        }
    }
}

/// Fails unless the transaction opened by `migrate_tenant` is still open.
async fn still_in_transaction(exec: &PgExec, marker: &str) -> Result<(), String> {
    let got = exec
        .query_texts(
            "SELECT coalesce(current_setting('vibe.migrate_tx', true), '') WHERE $1::text IS NOT NULL",
            "",
        )
        .await?;
    if got.first().map(String::as_str) == Some(marker) {
        Ok(())
    } else {
        Err("the migration transaction was ended early (COMMIT/ROLLBACK in migration SQL?); refusing to continue".into())
    }
}

/// First keyword of every SQL statement in `sql` that ends, starts or
/// otherwise controls a transaction. Comments, string literals, quoted
/// identifiers and dollar-quoted bodies are skipped.
pub fn transaction_control(sql: &str) -> Option<String> {
    const WORDS: &[&str] = &[
        "begin",
        "start",
        "commit",
        "end",
        "rollback",
        "abort",
        "savepoint",
        "release",
        "prepare",
    ];
    for statement in split_statements(sql) {
        let mut words = statement
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .filter(|w| !w.is_empty())
            .map(|w| w.to_ascii_lowercase());
        let Some(first) = words.next() else { continue };
        if WORDS.contains(&first.as_str()) {
            return Some(first.to_uppercase());
        }
        // `SET TRANSACTION` / `SET SESSION CHARACTERISTICS` are harmless, but
        // switching role or session authorization is not allowed either.
        if first == "set" || first == "reset" {
            let second = words.next().unwrap_or_default();
            let third = words.next().unwrap_or_default();
            let target = if second == "local" || second == "session" {
                third
            } else {
                second
            };
            if target == "role" || target == "authorization" {
                return Some(format!("{} ROLE", first.to_uppercase()));
            }
        }
    }
    None
}

/// Splits SQL on top-level semicolons, with literals and comments blanked.
fn split_statements(sql: &str) -> Vec<String> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '-' && next == Some('-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            cur.push(' ');
            continue;
        }
        if c == '/' && next == Some('*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            cur.push(' ');
            continue;
        }
        if c == '\'' || c == '"' {
            i += 1;
            while i < chars.len() {
                if chars[i] == c {
                    if chars.get(i + 1) == Some(&c) {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
            cur.push_str(" x ");
            continue;
        }
        if c == '$' {
            // $tag$ ... $tag$ (tag may be empty); a `$1` parameter is not one.
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let tag_ok = j < chars.len()
                && chars[j] == '$'
                && !chars.get(i + 1).is_some_and(|d| d.is_ascii_digit());
            if tag_ok {
                let tag: String = chars[i..=j].iter().collect();
                let rest: String = chars[j + 1..].iter().collect();
                match rest.find(&tag) {
                    Some(end) => i = j + 1 + rest[..end].chars().count() + tag.chars().count(),
                    None => i = chars.len(),
                }
                cur.push_str(" x ");
                continue;
            }
        }
        if c == ';' {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
        i += 1;
    }
    out.push(cur);
    out
}

async fn migrate_in_tx(
    exec: &PgExec,
    manifest: &Manifest,
    tenant: &str,
    runtime_role: &str,
) -> Result<Vec<String>, String> {
    let mut log = Vec::new();
    let schema = format!("t_{tenant}");
    let others_before = tenant_fingerprint(exec, &schema).await?;
    let problems_before = rls_problems(exec, &schema).await?;
    for model in &manifest.models {
        let fields: Vec<Col> = model
            .fields
            .iter()
            .map(|field| Col {
                name: field.name.clone(),
                ty: field.ty.clone(),
            })
            .collect();
        let uniques: Vec<String> = model
            .fields
            .iter()
            .filter(|field| field.unique)
            .map(|field| field.name.clone())
            .collect();
        exec.batch(&bootstrap_statements(
            &schema,
            &model.table,
            &fields,
            &uniques,
        )?)
        .await?;
        exec.batch(&runtime_schema_statements(&schema, runtime_role)?)
            .await?;
        exec.batch(&runtime_grant_statements(
            &schema,
            &model.table,
            runtime_role,
        )?)
        .await?;
        log.push(format!("bootstrapped {schema}.{}", model.table));
    }
    // Migration DDL names tables unqualified; resolve them in the tenant schema.
    exec.batch(&[format!("SET LOCAL search_path TO {}", quote(&schema)?)])
        .await?;
    for migration in &manifest.migrations {
        exec.batch(&migration.statements).await?;
        log.push(format!("applied {}", migration.id));
    }
    exec.batch(&["RESET search_path".to_string()]).await?;
    log.extend(protect_schema(exec, &schema).await?);
    // Any RLS problem in this tenant, or a new one in any other tenant, fails.
    // Problems that predate this run are reported but left to the operator
    // (`vibe serve` refuses to start on them).
    let own_prefix = format!("{schema}.");
    let mut new_problems = Vec::new();
    for problem in rls_problems(exec, &schema).await? {
        if problem.starts_with(&own_prefix) || !problems_before.contains(&problem) {
            new_problems.push(problem);
        } else {
            log.push(format!(
                "warning: pre-existing problem in another tenant: {problem}"
            ));
        }
    }
    if !new_problems.is_empty() {
        return Err(format!(
            "row level security check failed after migrating {schema}: {}",
            new_problems.join("; ")
        ));
    }
    if others_before != tenant_fingerprint(exec, &schema).await? {
        return Err(format!(
            "migrations for {schema} changed another tenant's schema; a migration may only change its own tenant schema"
        ));
    }
    Ok(log)
}

/// After migrations: every table in the tenant schema gets ENABLE + FORCE RLS
/// and the tenant policy. A table without `tenant_id`, or any view,
/// materialized view or foreign table (which RLS cannot protect), fails.
/// Any policy other than `tenant_isolation` fails too: permissive policies
/// are OR-combined, so `CREATE POLICY open ON t USING (true)` in a migration
/// would expose every tenant's rows.
async fn protect_schema(exec: &PgExec, schema: &str) -> Result<Vec<String>, String> {
    let unsafe_rels = exec
        .query_texts(
            "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relkind IN ('v','m','f') ORDER BY 1",
            schema,
        )
        .await?;
    if !unsafe_rels.is_empty() {
        return Err(format!(
            "{schema}: views, materialized views and foreign tables bypass row level security and are not allowed ({})",
            unsafe_rels.join(", ")
        ));
    }
    let missing = exec
        .query_texts(
            "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relkind IN ('r','p') AND NOT EXISTS ( \
               SELECT 1 FROM pg_attribute a WHERE a.attrelid = c.oid \
               AND a.attname = 'tenant_id' AND NOT a.attisdropped) ORDER BY 1",
            schema,
        )
        .await?;
    if !missing.is_empty() {
        return Err(format!(
            "{schema}: table(s) without a tenant_id column cannot get the tenant policy ({}); add tenant_id in the migration",
            missing.join(", ")
        ));
    }
    let extra = exec
        .query_texts(
            "SELECT format('%I.%I', c.relname, p.polname) FROM pg_policy p \
             JOIN pg_class c ON c.oid = p.polrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND p.polname <> 'tenant_isolation' ORDER BY 1",
            schema,
        )
        .await?;
    if !extra.is_empty() {
        return Err(format!(
            "{schema}: migrations must not create row level security policies ({}); vibe installs the only allowed policy, tenant_isolation, and extra permissive policies would be OR-combined with it and leak rows across tenants",
            extra.join(", ")
        ));
    }
    let tables = exec
        .query_texts(
            "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relkind IN ('r','p') ORDER BY 1",
            schema,
        )
        .await?;
    let mut log = Vec::new();
    for table in tables {
        exec.batch(&rls_statements(schema, &table)?).await?;
        log.push(format!("row level security on {schema}.{table}"));
    }
    Ok(log)
}

/// RLS problems in every tenant schema, as `schema.table: problem`.
/// Every table in every tenant schema (`t_%`), not just the one being
/// migrated, must have ENABLE + FORCE RLS and exactly one policy:
/// `tenant_isolation`, permissive, for all commands and roles, with the same
/// expressions vibe installs. The owner role owns every tenant schema, so a
/// migration for tenant A could otherwise weaken tenant B (e.g. `CREATE POLICY
/// open ON t_b.notes USING (true)` or `ALTER TABLE ... NO FORCE ROW LEVEL
/// SECURITY`), and `vibe serve` only checks at startup.
async fn rls_problems(exec: &PgExec, own_schema: &str) -> Result<Vec<String>, String> {
    exec.query_texts(
        r#"WITH ref AS (
                 -- The expression vibe installs: this tenant's, or before it
                 -- exists, the most common one among tenant schemas.
                 SELECT q, w FROM (
                   SELECT pg_get_expr(p.polqual, p.polrelid) AS q,
                          pg_get_expr(p.polwithcheck, p.polrelid) AS w,
                          n.nspname = $1 AS own
                   FROM pg_policy p JOIN pg_class c ON c.oid = p.polrelid
                   JOIN pg_namespace n ON n.oid = c.relnamespace
                   WHERE n.nspname LIKE 't\_%' AND p.polname = 'tenant_isolation') x
                 GROUP BY q, w ORDER BY bool_or(own) DESC, count(*) DESC LIMIT 1),
               rels AS (
                 SELECT c.oid, format('%I.%I', n.nspname, c.relname) AS name, c.relkind,
                        c.relrowsecurity, c.relforcerowsecurity
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE n.nspname LIKE 't\_%' AND c.relkind IN ('r','p','v','m','f'))
             SELECT name || ': ' || problem FROM (
               SELECT name, 'view, materialized view or foreign table' AS problem
                 FROM rels WHERE relkind IN ('v','m','f')
               UNION ALL
               SELECT name, 'row level security not enabled and forced'
                 FROM rels WHERE relkind IN ('r','p')
                 AND NOT (relrowsecurity AND relforcerowsecurity)
               UNION ALL
               SELECT r.name, 'unexpected policy ' || quote_ident(p.polname)
                 FROM rels r JOIN pg_policy p ON p.polrelid = r.oid
                 WHERE p.polname <> 'tenant_isolation'
               UNION ALL
               SELECT r.name, 'tenant_isolation policy was altered'
                 FROM rels r JOIN pg_policy p ON p.polrelid = r.oid, ref
                 WHERE p.polname = 'tenant_isolation' AND (
                   NOT p.polpermissive OR p.polcmd <> '*' OR p.polroles <> '{0}'::oid[]
                   OR pg_get_expr(p.polqual, p.polrelid) IS DISTINCT FROM ref.q
                   OR pg_get_expr(p.polwithcheck, p.polrelid) IS DISTINCT FROM ref.w)
               UNION ALL
               SELECT r.name, 'tenant_isolation policy is missing'
                 FROM rels r WHERE r.relkind IN ('r','p') AND NOT EXISTS (
                   SELECT 1 FROM pg_policy p WHERE p.polrelid = r.oid
                   AND p.polname = 'tenant_isolation')
             ) x ORDER BY 1"#,
        own_schema,
    )
    .await
}

/// A digest of every other tenant schema's tables, columns, policies,
/// triggers, rules, privileges and functions. Migrations run as the owner of
/// all tenant schemas; this catches changes outside the tenant being migrated.
async fn tenant_fingerprint(exec: &PgExec, own_schema: &str) -> Result<String, String> {
    let rows = exec
        .query_texts(
            r#"WITH s AS (SELECT oid, nspname FROM pg_namespace
                          WHERE nspname LIKE 't\_%' AND nspname <> $1)
             SELECT coalesce(md5(string_agg(item, E'\n' ORDER BY item)), '') FROM (
               SELECT 'n ' || s.nspname || ' ' || coalesce(n.nspacl::text, '')
                 FROM s JOIN pg_namespace n ON n.oid = s.oid
               UNION ALL
               SELECT 'c ' || s.nspname || '.' || c.relname || ' ' || c.relkind::text || ' '
                      || c.relrowsecurity::text || c.relforcerowsecurity::text || ' '
                      || c.relowner::regrole::text || ' ' || coalesce(c.relacl::text, '')
                 FROM pg_class c JOIN s ON s.oid = c.relnamespace
               UNION ALL
               SELECT 'a ' || s.nspname || '.' || c.relname || '.' || a.attname || ' '
                      || format_type(a.atttypid, a.atttypmod) || ' ' || a.attnotnull::text
                      || ' ' || coalesce(pg_get_expr(d.adbin, d.adrelid), '')
                 FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid
                 JOIN s ON s.oid = c.relnamespace
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
                 WHERE a.attnum > 0 AND NOT a.attisdropped
               UNION ALL
               SELECT 'p ' || s.nspname || '.' || c.relname || '.' || p.polname || ' '
                      || p.polpermissive::text || p.polcmd::text || ' ' || p.polroles::text || ' '
                      || coalesce(pg_get_expr(p.polqual, p.polrelid), '') || ' '
                      || coalesce(pg_get_expr(p.polwithcheck, p.polrelid), '')
                 FROM pg_policy p JOIN pg_class c ON c.oid = p.polrelid
                 JOIN s ON s.oid = c.relnamespace
               UNION ALL
               SELECT 't ' || s.nspname || '.' || c.relname || ' ' || pg_get_triggerdef(t.oid)
                 FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid
                 JOIN s ON s.oid = c.relnamespace
               UNION ALL
               SELECT 'r ' || s.nspname || '.' || c.relname || ' ' || pg_get_ruledef(r.oid)
                 FROM pg_rewrite r JOIN pg_class c ON c.oid = r.ev_class
                 JOIN s ON s.oid = c.relnamespace
               UNION ALL
               SELECT 'f ' || s.nspname || '.' || p.oid::regprocedure::text || ' '
                      || md5(coalesce(p.prosrc, '')) || ' ' || p.prosecdef::text
                      || ' ' || coalesce(p.proacl::text, '')
                 FROM pg_proc p JOIN s ON s.oid = p.pronamespace
             ) x(item)"#,
            own_schema,
        )
        .await?;
    Ok(rows.into_iter().next().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::transaction_control;

    #[test]
    fn transaction_control_is_detected() {
        for sql in [
            "COMMIT",
            "commit;",
            "CREATE TABLE a (tenant_id text); COMMIT; CREATE TABLE b (x int)",
            "END",
            "rollback",
            "ABORT",
            "BEGIN",
            "START TRANSACTION",
            "SAVEPOINT s",
            "RELEASE SAVEPOINT s",
            "PREPARE TRANSACTION 'x'",
            "COMMIT PREPARED 'x'",
            "/* c */ COMMIT",
            "-- c\nCOMMIT",
            "SET ROLE postgres",
            "SET SESSION AUTHORIZATION postgres",
            "RESET ROLE",
            "SET LOCAL ROLE postgres",
        ] {
            assert!(transaction_control(sql).is_some(), "{sql}");
        }
    }

    #[test]
    fn ordinary_sql_is_allowed() {
        for sql in [
            "CREATE TABLE audit (tenant_id text, note text)",
            "ALTER TABLE products ADD COLUMN ends_at timestamptz",
            "INSERT INTO t VALUES ('commit; rollback')",
            "COMMENT ON TABLE t IS 'begin; end'",
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS $$ SELECT 1; $$",
            "CREATE FUNCTION g() RETURNS int LANGUAGE plpgsql AS $body$ BEGIN RETURN 1; END $body$",
            "UPDATE t SET note = $1",
            "SELECT 1 -- ; commit",
            "SET search_path TO x",
            "CREATE INDEX ON t (\"end\")",
        ] {
            assert_eq!(transaction_control(sql), None, "{sql}");
        }
    }
}
