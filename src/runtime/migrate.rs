use crate::manifest::Manifest;

use super::exec::{PgExec, PgSession};
use super::grants::{parse_grants, pg_grant_statements};
use super::slugs::valid_tenant;
use super::sql::{bootstrap_statements, quote, rls_statements, tenant_schema_statements, Col};

/// Problems with a tenant's pre-created roles, as seen by the owner role
/// (decision 97). `$1` is the tenant id; one row per problem.
pub const TENANT_ROLE_SETUP_SQL: &str = "\
WITH want(name) AS (SELECT unnest(ARRAY[ \
   'tenant_' || $1::text || '_rw', 'tenant_' || $1::text || '_public_reader', \
   'tenant_' || $1::text || '_public_writer'])) \
SELECT format('%s: %s', w.name, p) FROM want w LEFT JOIN pg_roles r ON r.rolname = w.name, \
 LATERAL (VALUES \
   (CASE WHEN r.oid IS NULL THEN 'does not exist' END), \
   (CASE WHEN NOT r.rolcanlogin THEN 'is not LOGIN' END), \
   (CASE WHEN r.rolinherit THEN 'is not NOINHERIT' END), \
   (CASE WHEN r.rolsuper THEN 'is a superuser' END), \
   (CASE WHEN r.rolbypassrls THEN 'has BYPASSRLS' END), \
   (CASE WHEN r.rolcreaterole OR r.rolcreatedb OR r.rolreplication \
     THEN 'has CREATEROLE, CREATEDB or REPLICATION' END), \
   (CASE WHEN EXISTS (SELECT 1 FROM pg_auth_members m WHERE m.member = r.oid OR m.roleid = r.oid) \
     THEN 'is a member of another role or has members' END)) v(p) \
WHERE p IS NOT NULL ORDER BY 1";

/// Problems with the migration login for tenant `$1` (decision 97): it must
/// be that tenant's own migration role, so it can neither own nor use any
/// other tenant's schema, and it must not be (or be able to become) a
/// superuser, a BYPASSRLS role, or any tenant runtime role.
pub const MIGRATE_ROLE_SQL: &str = "\
SELECT x FROM ( \
 SELECT 'is or can become a superuser or BYPASSRLS role ' || quote_ident(m.rolname) AS x \
   FROM pg_roles m WHERE (m.rolsuper OR m.rolbypassrls) \
   AND pg_has_role(current_user, m.oid, 'MEMBER') \
 UNION ALL \
 SELECT format('can own schema %I', n.nspname) FROM pg_namespace n \
   WHERE n.nspname LIKE 't\\_%' AND n.nspname <> 't_' || $1::text \
   AND pg_has_role(current_user, n.nspowner, 'MEMBER') \
 UNION ALL \
 SELECT format('can use schema %I', n.nspname) FROM pg_namespace n \
   WHERE n.nspname LIKE 't\\_%' AND n.nspname <> 't_' || $1::text \
   AND NOT pg_has_role(current_user, n.nspowner, 'MEMBER') \
   AND has_schema_privilege(current_user, n.oid, 'USAGE') \
 UNION ALL \
 SELECT format('is or can become tenant role %I', r.rolname) FROM pg_roles r \
   WHERE r.rolname LIKE 'tenant\\_%' AND pg_has_role(current_user, r.oid, 'MEMBER') \
) y ORDER BY 1";

/// Bootstraps one tenant schema and applies migrations, connected as the
/// owner role. Grants only that tenant's own roles (`tenant_<id>_rw`,
/// `tenant_<id>_public_reader`, `tenant_<id>_public_writer`, see
/// [`super::grants::tenant_role`]) SELECT/INSERT/UPDATE per the model
/// grants. The roles are created by an operator beforehand; migrate refuses
/// to run if any is missing or unsafe. Returns progress lines.
///
/// Runs in one transaction: if any step (including the RLS checks) fails,
/// nothing is committed, so a failed migration never leaves a table without
/// row level security.
pub async fn migrate_tenant(
    exec: &PgExec,
    manifest: &Manifest,
    tenant: &str,
) -> Result<Vec<String>, String> {
    if !valid_tenant(tenant) {
        return Err("tenant id must match ^[a-z0-9]{8,32}$".into());
    }
    let problems = exec.query_texts(MIGRATE_ROLE_SQL, tenant).await?;
    if !problems.is_empty() {
        return Err(format!(
            "refusing to migrate t_{tenant}: the migration login {}; use a migration role that owns only this tenant's schema (VIBE_MIGRATE_DATABASE_URL, decision 97)",
            problems.join(", ")
        ));
    }
    let problems = exec.query_texts(TENANT_ROLE_SETUP_SQL, tenant).await?;
    if !problems.is_empty() {
        return Err(format!(
            "tenant roles are not set up safely: {}; create each as `CREATE ROLE <name> LOGIN NOINHERIT PASSWORD '...'` with no other attributes and no role memberships (decision 97)",
            problems.join("; ")
        ));
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
    // The pool hands out a different connection per call; pin one so the
    // transaction covers every statement.
    let mut session = exec.session().await?;
    // Until COMMIT/ROLLBACK succeeds, dropping the session (error, panic,
    // cancelled future) closes the connection instead of pooling it.
    session.in_transaction(true);
    // REPEATABLE READ: the catalog snapshot used for the cross-tenant checks
    // below must not move while other tenants migrate concurrently.
    session
        .batch(&["BEGIN ISOLATION LEVEL REPEATABLE READ".to_string()])
        .await?;
    // Marker that only lives as long as this transaction. If it is gone at
    // the end, the transaction was ended (and maybe restarted) in between.
    let marker = uuid::Uuid::new_v4().simple().to_string();
    let result = async {
        session
            .batch(&[format!("SET LOCAL vibe.migrate_tx = '{marker}'")])
            .await?;
        let log = migrate_in_tx(&session, manifest, tenant).await?;
        still_in_transaction(&session, &marker).await?;
        Ok::<_, String>(log)
    }
    .await;
    match result {
        Ok(log) => {
            session.batch(&["COMMIT".to_string()]).await?;
            session.in_transaction(false);
            Ok(log)
        }
        Err(err) => {
            if session.batch(&["ROLLBACK".to_string()]).await.is_ok() {
                session.in_transaction(false);
            }
            Err(err)
        }
    }
}

/// Fails unless the transaction opened by `migrate_tenant` is still open.
async fn still_in_transaction(exec: &PgSession, marker: &str) -> Result<(), String> {
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

/// The leading keyword of `sql` if it ends, starts or otherwise controls a
/// transaction, or switches role. Migration statements run one at a time
/// over the extended protocol, which the server refuses for more than one
/// command, so only the first keyword (after leading comments) matters.
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
    let mut words = strip_leading_comments(sql)
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase());
    let first = words.next()?;
    if WORDS.contains(&first.as_str()) {
        return Some(first.to_uppercase());
    }
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
    None
}

fn strip_leading_comments(mut sql: &str) -> &str {
    loop {
        sql = sql.trim_start();
        if let Some(rest) = sql.strip_prefix("--") {
            sql = rest.split_once('\n').map_or("", |(_, r)| r);
        } else if sql.starts_with("/*") {
            // Nested block comments, as in Postgres.
            let bytes = sql.as_bytes();
            let (mut i, mut depth) = (0, 0);
            while i + 1 < bytes.len() {
                if &bytes[i..i + 2] == b"/*" {
                    depth += 1;
                    i += 2;
                } else if &bytes[i..i + 2] == b"*/" {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            sql = if depth == 0 { &sql[i..] } else { "" };
        } else {
            return sql;
        }
    }
}

async fn migrate_in_tx(
    exec: &PgSession,
    manifest: &Manifest,
    tenant: &str,
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
            tenant,
            &model.table,
            &fields,
            &uniques,
        )?)
        .await?;
        exec.batch(&tenant_schema_statements(tenant)?).await?;
        log.push(format!("bootstrapped {schema}.{}", model.table));
    }
    // Migration DDL names tables unqualified; resolve them in the tenant
    // schema; each batch sets it for the migration statements that follow.
    for migration in &manifest.migrations {
        exec.batch(&[format!("SET LOCAL search_path TO {}", quote(&schema)?)])
            .await?;
        for statement in &migration.statements {
            // One command per call (extended protocol): the server rejects a
            // statement that smuggles `; COMMIT; ...` after the first command.
            exec.execute_one(statement)
                .await
                .map_err(|err| format!("{}: {err}", migration.id))?;
        }
        exec.batch(&["RESET search_path".to_string()]).await?;
        log.push(format!("applied {}", migration.id));
    }
    // The tenant's own roles get exactly the effective grants from the lock,
    // on every run (a changed grant is applied by the next migrate). No other
    // role is granted anything on this schema.
    for model in &manifest.models {
        let grants = parse_grants(&model.grants)
            .map_err(|err| format!("model {}: grants: {err}", model.id))?;
        exec.batch(&pg_grant_statements(tenant, model, &grants)?)
            .await?;
        log.push(format!("granted {schema}.{}", model.table));
    }
    log.extend(protect_schema(exec, tenant).await?);
    // Fail closed: any RLS problem in any tenant schema fails the migration,
    // whether this run introduced it or it was already there (for example
    // left by an earlier, unprotected migration). `vibe serve` refuses to
    // start on the same problems.
    let problems = rls_problems(exec, &schema).await?;
    if !problems.is_empty() {
        let new: Vec<&String> = problems
            .iter()
            .filter(|p| !problems_before.contains(p))
            .collect();
        let old: Vec<&String> = problems
            .iter()
            .filter(|p| problems_before.contains(p))
            .collect();
        let mut msg = format!("row level security check failed after migrating {schema}:");
        if !new.is_empty() {
            msg.push_str(&format!(" introduced by this migration: {};", join(&new)));
        }
        if !old.is_empty() {
            msg.push_str(&format!(
                " already present before this run (fix these tenant schemas first): {};",
                join(&old)
            ));
        }
        return Err(msg.trim_end_matches(';').to_string());
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
async fn protect_schema(exec: &PgSession, tenant: &str) -> Result<Vec<String>, String> {
    let schema = &format!("t_{tenant}");
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
    // SECURITY DEFINER code runs as its owner, so it can change
    // `app.tenant_id` mid-transaction and read or copy other tenants' rows.
    let definer = exec
        .query_texts(
            "SELECT format('%s %I(%s)', CASE p.prokind WHEN 'p' THEN 'procedure' ELSE 'function' END, p.proname, pg_get_function_identity_arguments(p.oid)) \
             FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = $1 AND p.prosecdef \
             UNION ALL \
             SELECT format('trigger %I on %I (%s)', t.tgname, c.relname, t.tgfoid::regproc::text) \
             FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace JOIN pg_proc p ON p.oid = t.tgfoid \
             WHERE n.nspname = $1 AND NOT t.tgisinternal AND p.prosecdef ORDER BY 1",
            schema,
        )
        .await?;
    if !definer.is_empty() {
        return Err(format!(
            "{schema}: SECURITY DEFINER functions, procedures and triggers are not allowed; they run as their owner and can switch app.tenant_id ({})",
            definer.join(", ")
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
        exec.batch(&rls_statements(tenant, &table)?).await?;
        log.push(format!("row level security on {schema}.{table}"));
    }
    Ok(log)
}

/// RLS problems in every tenant schema, as `schema.table: problem`.
/// Every table in every tenant schema (`t_%`), not just the one being
/// migrated, must have ENABLE + FORCE RLS and exactly one policy:
/// `tenant_isolation`, permissive, for all commands, applying exactly to
/// that schema's three tenant roles, comparing `tenant_id` with that
/// schema's tenant id as a constant (decision 97). The owner role may own
/// every tenant schema, so a migration for tenant A could otherwise weaken
/// tenant B (e.g. `CREATE POLICY open ON t_b.notes USING (true)` or
/// `ALTER TABLE ... NO FORCE ROW LEVEL SECURITY`), and `vibe serve` only
/// checks at startup.
async fn rls_problems(exec: &PgSession, own_schema: &str) -> Result<Vec<String>, String> {
    exec.query_texts(
        &format!(
            r#"WITH rels AS (
                 SELECT c.oid, format('%I.%I', n.nspname, c.relname) AS name, c.relkind,
                        c.relrowsecurity, c.relforcerowsecurity, substr(n.nspname, 3) AS tenant
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE n.nspname LIKE 't\_%' AND c.relkind IN ('r','p','v','m','f')
                 AND $1::text IS NOT NULL)
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
                 FROM rels r JOIN pg_policy p ON p.polrelid = r.oid
                 WHERE p.polname = 'tenant_isolation' AND NOT ({canonical})
               UNION ALL
               SELECT r.name, 'tenant_isolation policy is missing'
                 FROM rels r WHERE r.relkind IN ('r','p') AND NOT EXISTS (
                   SELECT 1 FROM pg_policy p WHERE p.polrelid = r.oid
                   AND p.polname = 'tenant_isolation')
             ) x ORDER BY 1"#,
            canonical = canonical_policy_sql("p", "r.tenant")
        ),
        own_schema,
    )
    .await
}

/// SQL condition: policy `p` is the canonical `tenant_isolation` for the
/// tenant id in SQL expression `tenant` (see `sql::rls_statements`).
pub fn canonical_policy_sql(p: &str, tenant: &str) -> String {
    format!(
        "({p}.polpermissive AND {p}.polcmd = '*' \
         AND cardinality({p}.polroles) = 3 \
         AND (SELECT count(*) FROM pg_roles WHERE oid = ANY({p}.polroles) AND rolname IN ( \
           'tenant_' || {tenant} || '_rw', 'tenant_' || {tenant} || '_public_reader', \
           'tenant_' || {tenant} || '_public_writer')) = 3 \
         AND pg_get_expr({p}.polqual, {p}.polrelid) = format('(tenant_id = %L::text)', {tenant}) \
         AND pg_get_expr({p}.polwithcheck, {p}.polrelid) = format('(tenant_id = %L::text)', {tenant}))"
    )
}

/// A digest of every other tenant schema's tables, columns, policies,
/// triggers, rules, privileges and functions. Migrations run as the owner of
/// all tenant schemas; this catches changes outside the tenant being migrated.
async fn tenant_fingerprint(exec: &PgSession, own_schema: &str) -> Result<String, String> {
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
               SELECT 'y ' || s.nspname || '.' || t.typname || ' ' || t.typtype::text || ' '
                      || t.typowner::regrole::text || ' ' || coalesce(t.typacl::text, '')
                      || ' ' || format_type(t.typbasetype, t.typtypmod)
                      || ' ' || coalesce(t.typdefault, '')
                 FROM pg_type t JOIN s ON s.oid = t.typnamespace
               UNION ALL
               SELECT 'k ' || s.nspname || ' ' || con.conname || ' ' || pg_get_constraintdef(con.oid)
                 FROM pg_constraint con JOIN s ON s.oid = con.connamespace
               UNION ALL
               SELECT 'd ' || s.nspname || ' ' || d.defaclrole::regrole::text || ' '
                      || d.defaclobjtype::text || ' ' || coalesce(d.defaclacl::text, '')
                 FROM pg_default_acl d JOIN s ON s.oid = d.defaclnamespace
               UNION ALL
               SELECT 'e ' || s.nspname || '.' || c.relname || ' ' || coalesce(pg_get_indexdef(c.oid), '')
                 FROM pg_class c JOIN s ON s.oid = c.relnamespace WHERE c.relkind IN ('i','I')
               UNION ALL
               SELECT 'q ' || s.nspname || '.' || c.relname || ' ' || seq.seqincrement::text
                      || ' ' || seq.seqmin::text || ' ' || seq.seqmax::text
                 FROM pg_sequence seq JOIN pg_class c ON c.oid = seq.seqrelid
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

fn join(items: &[&String]) -> String {
    items
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::transaction_control;

    #[test]
    fn transaction_control_is_detected() {
        for sql in [
            "COMMIT",
            "commit;",
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
