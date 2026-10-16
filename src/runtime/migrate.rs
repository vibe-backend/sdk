use crate::manifest::Manifest;

use super::allowlist::{classify, rebuild_placement, Placement};
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
     THEN 'is a member of another role or has members' END), \
   (CASE WHEN EXISTS (SELECT 1 FROM pg_class WHERE relowner = r.oid) \
     OR EXISTS (SELECT 1 FROM pg_namespace WHERE nspowner = r.oid) \
     OR EXISTS (SELECT 1 FROM pg_proc WHERE proowner = r.oid) \
     OR EXISTS (SELECT 1 FROM pg_type WHERE typowner = r.oid) \
     THEN 'owns database objects (runtime roles must not be owners)' END)) v(p) \
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
   WHERE r.rolname LIKE 'tenant\\_%' AND r.rolname <> current_user \
   AND pg_has_role(current_user, r.oid, 'MEMBER') \
) y ORDER BY 1";

/// Bootstraps one tenant schema and applies migrations, connected as the
/// tenant's owner role `tenant_<id>_owner` (required by name). Grants only that tenant's own roles (`tenant_<id>_rw`,
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
        return Err(super::slugs::TENANT_ID_RULE.into());
    }
    // The migration login must be this tenant's owner role by name.
    let owner = super::grants::tenant_owner_role(tenant)?;
    super::grants::tenant_roles(tenant)?;
    let me = exec
        .query_texts("SELECT session_user::text WHERE $1::text IS NOT NULL", "")
        .await?;
    if me.first() != Some(&owner) {
        return Err(format!(
            "refusing to migrate t_{tenant}: VIBE_MIGRATE_DATABASE_URL must log in as {owner} (logged in as {})",
            me.first().map(String::as_str).unwrap_or("?")
        ));
    }
    let schema_owner = exec
        .query_texts(
            "SELECT nspowner::regrole::text FROM pg_namespace WHERE nspname = 't_' || $1::text",
            tenant,
        )
        .await?;
    if let Some(other) = schema_owner.first().filter(|o| **o != owner) {
        return Err(format!(
            "refusing to migrate t_{tenant}: the schema is owned by {other}, not {owner}"
        ));
    }
    // The lock's policy must be the one vibe installs.
    for model in &manifest.models {
        if model
            .policy
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            != super::sql::POLICY_TEMPLATE
        {
            return Err(format!(
                "model {}: lock policy is not the per-tenant policy vibe installs (`{}`); recompile the lock",
                model.id,
                super::sql::POLICY_TEMPLATE
            ));
        }
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
    // Allowlist (spec §2B): every statement is checked, and each index is
    // placed, before anything runs.
    let mut placed: Vec<(String, Vec<Placement>)> = Vec::new();
    for migration in &manifest.migrations {
        let mut steps = Vec::new();
        for statement in &migration.statements {
            steps.push(classify(statement).map_err(|err| format!("{}: {err}", migration.id))?);
        }
        placed.push((migration.id.clone(), steps));
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
        let (mut log, deferred) = migrate_in_tx(&session, manifest, &placed, tenant).await?;
        log.extend(verify_tenant(&session, tenant).await?);
        still_in_transaction(&session, &marker).await?;
        Ok::<_, String>((log, deferred))
    }
    .await;
    match result {
        Ok((mut log, deferred)) => {
            session.batch(&["COMMIT".to_string()]).await?;
            session.in_transaction(false);
            log.extend(build_indexes_after_commit(&session, tenant, &deferred).await?);
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

/// After migrating (same transaction): the schema is owned by
/// `tenant_<id>_owner`, the runtime roles own nothing, and every table in the
/// schema has ENABLE + FORCE row level security. Any failure rolls back.
async fn verify_tenant(exec: &PgSession, tenant: &str) -> Result<Vec<String>, String> {
    let owner = super::grants::tenant_owner_role(tenant)?;
    let mut problems = exec
        .query_texts(
            "SELECT format('schema %I is owned by %s', n.nspname, n.nspowner::regrole::text) \
             FROM pg_namespace n WHERE n.nspname = 't_' || $1::text \
             AND n.nspowner::regrole::text <> 'tenant_' || $1::text || '_owner' \
             UNION ALL \
             SELECT format('table %I.%I lacks ENABLE + FORCE ROW LEVEL SECURITY', n.nspname, c.relname) \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 't_' || $1::text AND c.relkind IN ('r','p') \
             AND NOT (c.relrowsecurity AND c.relforcerowsecurity) ORDER BY 1",
            tenant,
        )
        .await?;
    problems.extend(
        exec.query_texts(TENANT_ROLE_SETUP_SQL, tenant)
            .await?
            .into_iter()
            .filter(|p| p.contains("owns database objects")),
    );
    if problems.is_empty() {
        Ok(vec![format!(
            "verified t_{tenant}: owned by {owner}, runtime roles own nothing, every table has FORCE ROW LEVEL SECURITY"
        )])
    } else {
        Err(format!(
            "post-migration check failed for t_{tenant}: {}",
            problems.join("; ")
        ))
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

/// A non-unique index built `CONCURRENTLY` after the main transaction
/// commits: (migration id or `rebuild`, SQL).
type Deferred = Vec<(String, String)>;

/// Prefix of log lines that `vibe migrate` also writes to the audit table.
pub const AUDIT_PREFIX: &str = "audit: ";

async fn migrate_in_tx(
    exec: &PgSession,
    manifest: &Manifest,
    placed: &[(String, Vec<Placement>)],
    tenant: &str,
) -> Result<(Vec<String>, Deferred), String> {
    let mut deferred: Deferred = Vec::new();
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
    // An index left INVALID by a failed concurrent build is dropped and
    // rebuilt (spec §2B), with an audit entry.
    log.extend(drop_invalid_indexes(exec, &schema, &mut deferred).await?);
    // Migration DDL names tables unqualified; resolve them in the tenant
    // schema; each batch sets it for the migration statements that follow.
    for (id, steps) in placed {
        exec.batch(&[format!("SET LOCAL search_path TO {}", quote(&schema)?)])
            .await?;
        for step in steps {
            match step {
                // One command per call (extended protocol): the server
                // rejects a statement that smuggles `; COMMIT; ...`.
                Placement::InTransaction(sql) => exec
                    .execute_one(sql)
                    .await
                    .map_err(|err| format!("{id}: {err}"))?,
                Placement::AfterCommit(sql) => deferred.push((id.clone(), sql.clone())),
            }
        }
        exec.batch(&["RESET search_path".to_string()]).await?;
        log.push(format!("applied {id}"));
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
    Ok((log, deferred))
}

/// Indexes in `schema` that are INVALID (a failed `CREATE INDEX
/// CONCURRENTLY`): each is dropped in the main transaction and rebuilt,
/// UNIQUE ones right away in the transaction, others after commit.
async fn drop_invalid_indexes(
    exec: &PgSession,
    schema: &str,
    deferred: &mut Deferred,
) -> Result<Vec<String>, String> {
    let rows = (**exec)
        .query(
            "SELECT c.relname::text, pg_get_indexdef(c.oid) FROM pg_index i \
             JOIN pg_class c ON c.oid = i.indexrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND NOT i.indisvalid ORDER BY 1",
            &[&schema],
        )
        .await
        .map_err(super::exec::pg_error)?;
    let mut log = Vec::new();
    for row in rows {
        let (name, def): (String, String) = (row.get(0), row.get(1));
        exec.batch(&[format!("DROP INDEX {}.{}", quote(schema)?, quote(&name)?)])
            .await?;
        let placement = rebuild_placement(&def);
        let when = match &placement {
            Placement::InTransaction(sql) => {
                exec.execute_one(sql).await?;
                "in the migration transaction"
            }
            Placement::AfterCommit(sql) => {
                deferred.push(("rebuild".into(), sql.clone()));
                "concurrently after commit"
            }
        };
        log.push(format!(
            "{AUDIT_PREFIX}dropped invalid index {schema}.{name}; rebuilding {when}"
        ));
    }
    Ok(log)
}

/// Builds the deferred non-unique indexes, one audited step each, after the
/// main transaction committed (still inside the owner login window). A
/// failure is reported; the index it leaves INVALID is dropped and rebuilt
/// on the next migrate.
async fn build_indexes_after_commit(
    exec: &PgSession,
    tenant: &str,
    deferred: &Deferred,
) -> Result<Vec<String>, String> {
    let schema = format!("t_{tenant}");
    let mut log = Vec::new();
    if deferred.is_empty() {
        return Ok(log);
    }
    exec.batch(&[format!("SET search_path TO {}", quote(&schema)?)])
        .await?;
    for (id, sql) in deferred {
        if let Err(err) = exec.execute_one(sql).await {
            let _ = exec.batch(&["RESET search_path".to_string()]).await;
            return Err(format!(
                "{id}: the main migration transaction for {schema} committed, but the concurrent index step failed: {err}; an INVALID index it left is dropped and rebuilt on the next migrate"
            ));
        }
        log.push(format!(
            "{AUDIT_PREFIX}built index concurrently after commit ({id}) in {schema}"
        ));
    }
    exec.batch(&["RESET search_path".to_string()]).await?;
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

/// Advisory lock class held by `vibe migrate` (on the admin connection)
/// for the whole owner login window; the second key is the owner role oid.
pub const MIGRATE_LOCK_CLASS: i32 = 0x5649_4245; // "VIBE"

/// Owner roles that can log in while no `vibe migrate` holds their window
/// lock: `$1` is unused (pass "").
pub const OWNER_LOGIN_SQL: &str = "\
SELECT r.rolname::text FROM pg_roles r \
 WHERE r.rolname ~ '^tenant_[a-z0-9_]+_owner$' AND r.rolcanlogin AND $1::text IS NOT NULL \
 AND NOT EXISTS (SELECT 1 FROM pg_locks l WHERE l.locktype = 'advisory' \
   AND l.classid::bigint = 1447641669 AND l.objid = r.oid AND l.objsubid = 2 AND l.granted) \
 ORDER BY 1";

/// SQL that ends an owner login window: NOLOGIN, password removed, and any
/// remaining owner sessions terminated.
pub fn close_owner_window_sql(owner: &str) -> Result<Vec<String>, String> {
    let role = quote(owner)?;
    Ok(vec![
        format!("ALTER ROLE {role} NOLOGIN PASSWORD NULL"),
        format!(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename = '{owner}' AND pid <> pg_backend_pid()"
        ),
    ])
}

/// A random one-time password (256 bits, hex). Only its SCRAM verifier is
/// sent to the server; the password itself is never logged.
fn one_time_password() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `admin_url` with the user and password replaced.
fn url_with_login(admin_url: &str, user: &str, password: &str) -> Result<String, String> {
    let mut url =
        url::Url::parse(admin_url).map_err(|err| format!("invalid admin database URL: {err}"))?;
    url.set_username(user)
        .map_err(|_| "admin database URL cannot carry a user".to_string())?;
    url.set_password(Some(password))
        .map_err(|_| "admin database URL cannot carry a password".to_string())?;
    Ok(url.to_string())
}

/// `vibe migrate` (spec §2B, D129): the owner role `tenant_<id>_owner` is
/// NOLOGIN outside migrate. On the platform-admin connection this takes the
/// tenant's window lock, gives the owner a one-time password and LOGIN, runs
/// [`migrate_tenant`] logged in as the owner (one transaction, then the
/// deferred concurrent indexes), and then — whether that succeeded or
/// failed — on a separate admin connection sets the owner NOLOGIN again,
/// removes the password, terminates owner sessions and writes the audit
/// entries to `public.vibe_migrate_audit`.
pub async fn migrate_with_owner_window(
    admin_url: &str,
    manifest: &Manifest,
    tenant: &str,
) -> Result<Vec<String>, String> {
    if !valid_tenant(tenant) {
        return Err(super::slugs::TENANT_ID_RULE.into());
    }
    let owner = super::grants::tenant_owner_role(tenant)?;
    let admin = PgExec::connect(admin_url).await?;
    let lock = admin.session().await?;
    let state = (*lock)
        .query(
            "SELECT r.oid::bigint, r.rolcanlogin, pg_try_advisory_lock($2, r.oid::bigint::bit(32)::int) \
             FROM pg_roles r WHERE r.rolname = $1",
            &[&owner, &MIGRATE_LOCK_CLASS],
        )
        .await
        .map_err(super::exec::pg_error)?;
    let Some(row) = state.first() else {
        return Err(format!(
            "refusing to migrate t_{tenant}: owner role {owner} does not exist; create it NOLOGIN (decision 129)"
        ));
    };
    let (can_login, locked): (bool, bool) = (row.get(1), row.get(2));
    if !locked {
        return Err(format!(
            "refusing to migrate t_{tenant}: another vibe migrate holds the owner login window"
        ));
    }
    if can_login {
        return Err(format!(
            "refusing to migrate t_{tenant}: {owner} can already log in outside a migrate window; set it NOLOGIN and investigate (see the owner login check)"
        ));
    }
    let password = one_time_password();
    let verifier = postgres_protocol::password::scram_sha_256(password.as_bytes());
    let opened = lock
        .batch(&[format!(
            "ALTER ROLE {} LOGIN PASSWORD '{verifier}'",
            quote(&owner)?
        )])
        .await;
    let result = match opened {
        Err(err) => Err(format!("could not open the owner login window: {err}")),
        Ok(()) => {
            async {
                let owner_url = url_with_login(admin_url, &owner, &password)?;
                let exec = PgExec::connect(&owner_url).await?;
                migrate_tenant(&exec, manifest, tenant).await
            }
            .await
        }
    };
    drop(password);
    // finally: always, on a separate admin connection.
    let closed = close_owner_window(admin_url, &owner, tenant, &result).await;
    let _ = lock
        .batch(&["SELECT pg_advisory_unlock_all()".to_string()])
        .await;
    drop(lock);
    match (result, closed) {
        (Ok(log), Ok(())) => Ok(log),
        (Ok(_), Err(err)) => Err(err),
        (Err(err), Ok(())) => Err(err),
        (Err(err), Err(close)) => Err(format!("{err}; additionally: {close}")),
    }
}

async fn close_owner_window(
    admin_url: &str,
    owner: &str,
    tenant: &str,
    result: &Result<Vec<String>, String>,
) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..3 {
        let attempt = async {
            let admin = PgExec::connect(admin_url).await?;
            let session = admin.session().await?;
            session.batch(&close_owner_window_sql(owner)?).await?;
            let mut events: Vec<String> = match result {
                Ok(log) => log
                    .iter()
                    .filter_map(|l| l.strip_prefix(AUDIT_PREFIX).map(str::to_string))
                    .collect(),
                Err(err) => vec![format!("migrate failed: {err}")],
            };
            if result.is_ok() {
                events.push("migrate committed".into());
            }
            events.push(format!(
                "owner login window closed: {owner} NOLOGIN, password removed"
            ));
            session.batch(&[AUDIT_TABLE_SQL.to_string()]).await?;
            for event in events {
                (*session)
                    .execute(
                        "INSERT INTO public.vibe_migrate_audit (tenant, event) VALUES ($1, $2)",
                        &[&tenant, &event],
                    )
                    .await
                    .map_err(super::exec::pg_error)?;
            }
            Ok::<_, String>(())
        }
        .await;
        match attempt {
            Ok(()) => return Ok(()),
            Err(err) => last = err,
        }
    }
    tracing::error!(
        owner,
        "owner login window could not be closed; the owner may still log in"
    );
    Err(format!(
        "SECURITY: could not set {owner} back to NOLOGIN after migrate ({last}); run `ALTER ROLE {owner} NOLOGIN PASSWORD NULL` now"
    ))
}

/// Audit table written by the platform-admin connection.
pub const AUDIT_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS public.vibe_migrate_audit (\
 at timestamptz NOT NULL DEFAULT now(), tenant text NOT NULL, event text NOT NULL); \
 REVOKE ALL ON public.vibe_migrate_audit FROM PUBLIC";

/// Owner login alerts raised since process start (`vibe check-owners`).
static OWNER_LOGIN_ALERTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Metric `vibe_db_owner_login_alerts_total`.
pub fn owner_login_alerts_total() -> u64 {
    OWNER_LOGIN_ALERTS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Periodic owner check (spec §2B): every owner role that can log in while
/// no migrate window is open is set NOLOGIN (password removed) and reported
/// with an alert. Returns the roles found.
pub async fn check_owner_logins(admin: &PgExec) -> Result<Vec<String>, String> {
    let found = admin.query_texts(OWNER_LOGIN_SQL, "").await?;
    for owner in &found {
        OWNER_LOGIN_ALERTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tracing::error!(
            owner = owner.as_str(),
            vibe_db_owner_login_alerts_total = owner_login_alerts_total(),
            "ALERT: tenant owner role can log in outside vibe migrate; setting NOLOGIN"
        );
        admin.batch(&close_owner_window_sql(owner)?).await?;
    }
    Ok(found)
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
