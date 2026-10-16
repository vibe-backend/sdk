//! Real PostgreSQL tests. They run when `VIBE_TEST_PG_ADMIN_URL` points at a
//! superuser connection (CI sets it via a service container). Outside CI they
//! are skipped with a SKIPPED note; in CI (`CI` set) a missing URL fails.

use serde_json::{json, Value};
use tokio_postgres::{Client, NoTls};
use vibe::manifest::Manifest;
use vibe::runtime::{migrate_tenant, Col, PgExec, QueryExec, Stmt};

fn admin_url() -> Option<String> {
    std::env::var("VIBE_TEST_PG_ADMIN_URL").ok()
}

async fn connect(url: &str) -> Client {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await.unwrap();
    tokio::spawn(conn);
    client
}

/// Unique lowercase suffix so tests can run in parallel against one cluster.
fn suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..10].to_string()
}

/// Same server and database as the admin URL, logged in as `user`.
fn url_as(admin: &str, user: &str) -> String {
    let mut url = url::Url::parse(admin).unwrap();
    url.set_username(user).unwrap();
    url.set_password(Some("pw")).unwrap();
    url.to_string()
}

async fn create_login(admin: &Client, name: &str, extra: &str) {
    admin
        .batch_execute(&format!("CREATE ROLE {name} LOGIN PASSWORD 'pw' {extra}"))
        .await
        .unwrap();
}

async fn check_as(admin_url: &str, user: &str) -> Result<(), String> {
    let exec = PgExec::connect(&url_as(admin_url, user)).await.unwrap();
    exec.check_runtime_role().await
}

/// Grants of the test model: full access for `app_rw`, column-limited
/// public roles.
const GRANTS: &str =
    "app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT(body)], public_writer [INSERT(body)]";

/// Runtime roles (cluster-wide, NOLOGIN). Every runtime login is granted
/// all three and runs each statement after `SET LOCAL ROLE`.
const RUNTIME_ROLES: [&str; 3] = ["app_rw", "public_reader", "public_writer"];

async fn create_runtime_roles(admin: &Client, login: &str) {
    for role in RUNTIME_ROLES {
        admin
            .batch_execute(&format!(
                "DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{role}') \
                 THEN CREATE ROLE {role} NOLOGIN; END IF; END $$; \
                 GRANT {role} TO {login} WITH INHERIT FALSE, SET TRUE"
            ))
            .await
            .unwrap();
    }
}

fn manifest() -> Manifest {
    serde_json::from_value(json!({
        "manifest_version": 1,
        "routes": [],
        "methods": [],
        "signature": "",
        "models": [{
            "id": "note", "provider": "main", "table": "notes",
            "crud": [], "sensitive_output": [], "ownership": "",
            "grants": GRANTS,
            "rls": "enable, force",
            "policy": "tenant_id = current_setting('app.tenant_id', false)",
            "list": {"page_size": 20, "max_page_size": 100, "sort": [], "filter": []},
            "fields": [
                field("id", "uuid"), field("tenant_id", "string"), field("body", "string"),
                field("created_at", "timestamp"), field("updated_at", "timestamp"),
                field("deleted_at", "timestamp")
            ]
        }],
        "migrations": [{
            "id": "migrations/main/0002-audit", "provider": "main", "seq": "0002",
            "statements": ["CREATE TABLE IF NOT EXISTS audit_notes (tenant_id text, note text)"]
        }]
    }))
    .unwrap()
}

fn field(name: &str, ty: &str) -> Value {
    json!({"name": name, "ty": ty, "required": false, "unique": false, "primary": name == "id",
           "system": false, "sensitive": false, "nullable": true, "min": null, "max": null,
           "format": ""})
}

struct Setup {
    admin_url: String,
    admin: Client,
    owner: String,
    app: String,
    tenant: String,
    _guard: Guard,
}

/// Migrations fail closed on any RLS problem in any tenant schema, and all
/// tests share one cluster. Tests that deliberately break RLS take this lock
/// exclusively (and repair before returning).
///
/// Shared holders still interfered with each other (catalog-wide checks seeing
/// another test's half-built tenant, and lock-order deadlocks between
/// concurrent migrations; #110), so every test now takes it exclusively and
/// the Postgres tests run one at a time.
static CORRUPTION: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

#[allow(dead_code)]
enum Guard {
    Shared(tokio::sync::RwLockReadGuard<'static, ()>),
    Exclusive(tokio::sync::RwLockWriteGuard<'static, ()>),
}

/// Catalog-wide DDL (GRANT ON DATABASE, CREATE ROLE) races between parallel
/// tests ("tuple concurrently updated"), so setup runs one at a time.
static SETUP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup() -> Option<Setup> {
    setup_exclusive().await
}

/// For tests that leave RLS broken for a while.
async fn setup_exclusive() -> Option<Setup> {
    let admin_url = admin_url()?;
    setup_with(admin_url, Guard::Exclusive(CORRUPTION.write().await)).await
}

async fn setup_with(admin_url: String, guard: Guard) -> Option<Setup> {
    let _guard = SETUP.lock().await;
    let admin = connect(&admin_url).await;
    let s = suffix();
    let owner = format!("own_{s}");
    let app = format!("app_{s}");
    create_login(&admin, &owner, "").await;
    create_login(&admin, &app, "").await;
    create_runtime_roles(&admin, &app).await;
    admin
        .batch_execute(&format!(
            "GRANT CREATE ON DATABASE {} TO {owner}",
            admin_url
                .rsplit('/')
                .next()
                .unwrap()
                .split('?')
                .next()
                .unwrap()
        ))
        .await
        .unwrap();
    let tenant = format!("tn{s}");
    let owner_exec = PgExec::connect(&url_as(&admin_url, &owner)).await.unwrap();
    migrate_tenant(&owner_exec, &manifest(), &tenant, &app)
        .await
        .unwrap();
    // `visible_rows` reads the migration table as app_rw; the runtime only
    // ever touches model tables, so migrate grants nothing on it.
    admin
        .batch_execute(&format!("GRANT SELECT ON t_{tenant}.audit_notes TO app_rw"))
        .await
        .unwrap();
    Some(Setup {
        admin_url,
        admin,
        owner,
        app,
        tenant,
        _guard: guard,
    })
}

macro_rules! require_pg {
    ($e:expr) => {
        match $e {
            Some(v) => v,
            None => {
                // CI must run these tests; a missing database is a failure there.
                if std::env::var_os("CI").is_some() {
                    panic!("VIBE_TEST_PG_ADMIN_URL is not set in CI; the Postgres tests cannot be skipped");
                }
                eprintln!("SKIPPED: VIBE_TEST_PG_ADMIN_URL not set; skipping Postgres test");
                return;
            }
        }
    };
}

#[tokio::test]
async fn non_owner_runtime_role_is_accepted_and_limited() {
    let s = require_pg!(setup().await);
    check_as(&s.admin_url, &s.app).await.unwrap();
    let schema = format!("t_{}", s.tenant);
    let row = s
        .admin
        .query_one(
            "SELECT has_table_privilege($1, $2, 'SELECT'), has_table_privilege($1, $2, 'INSERT'), \
             has_table_privilege($1, $2, 'UPDATE'), has_table_privilege($1, $2, 'DELETE'), \
             has_table_privilege($1, $2, 'TRUNCATE'), has_schema_privilege($1, $3, 'CREATE'), \
             has_table_privilege($1, $4, 'SELECT')",
            &[
                &s.app,
                &format!("{schema}.notes"),
                &schema,
                &format!("{schema}.audit_notes"),
            ],
        )
        .await
        .unwrap();
    let flags: Vec<bool> = (0..7).map(|i| row.get(i)).collect();
    assert_eq!(
        flags,
        vec![false, false, false, false, false, false, false],
        "the login itself has no table privileges and no CREATE"
    );
    // Its statements run as app_rw, which has exactly the model grants.
    let row = s
        .admin
        .query_one(
            "SELECT has_table_privilege('app_rw', $1, 'SELECT'), has_table_privilege('app_rw', $1, 'INSERT'), \
             has_table_privilege('app_rw', $1, 'UPDATE'), has_table_privilege('app_rw', $1, 'DELETE'), \
             has_table_privilege('app_rw', $1, 'TRUNCATE'), \
             has_column_privilege('public_reader', $1, 'body', 'SELECT'), \
             has_column_privilege('public_reader', $1, 'tenant_id', 'SELECT'), \
             has_column_privilege('public_writer', $1, 'body', 'SELECT')",
            &[&format!("{schema}.notes")],
        )
        .await
        .unwrap();
    let flags: Vec<bool> = (0..8).map(|i| row.get(i)).collect();
    assert_eq!(
        flags,
        vec![true, true, false, false, false, true, false, false],
        "table-wide UPDATE is narrowed to columns (never id/tenant_id/created_at)"
    );
}

#[tokio::test]
async fn rls_isolates_tenants_for_the_runtime_role() {
    let s = require_pg!(setup().await);
    let exec = PgExec::connect(&url_as(&s.admin_url, &s.app))
        .await
        .unwrap();
    let cols = |names: &[&str]| -> Vec<Col> {
        names
            .iter()
            .map(|n| Col {
                name: n.to_string(),
                ty: "string".into(),
            })
            .collect()
    };
    let now = chrono::Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();
    exec.execute(
        &s.tenant,
        "app_rw",
        Stmt::Insert {
            table: "notes".into(),
            values: vec![
                (
                    Col {
                        name: "id".into(),
                        ty: "uuid".into(),
                    },
                    json!(id),
                ),
                (cols(&["body"])[0].clone(), json!("secret")),
                (
                    Col {
                        name: "created_at".into(),
                        ty: "timestamp".into(),
                    },
                    json!(now),
                ),
                (
                    Col {
                        name: "updated_at".into(),
                        ty: "timestamp".into(),
                    },
                    json!(now),
                ),
            ],
            returning: cols(&["body"]),
        },
    )
    .await
    .unwrap();
    // The table lives in t_<tenant>; a different tenant id points the session
    // at another schema and a different RLS value, so it must see nothing.
    // Writing a row with someone else's tenant_id must fail WITH CHECK.
    let admin = &s.admin;
    let schema = format!("t_{}", s.tenant);
    let forged = admin
        .batch_execute(&format!(
            "SET ROLE {app}; BEGIN; SET LOCAL ROLE app_rw; SELECT set_config('app.tenant_id', 'othertenant1', true); \
             INSERT INTO {schema}.notes (id, tenant_id, created_at, updated_at) \
             VALUES (gen_random_uuid(), '{t}', now(), now()); COMMIT;",
            app = s.app,
            t = s.tenant
        ))
        .await;
    let _ = admin.batch_execute("ROLLBACK; RESET ROLE").await;
    assert!(
        forged.is_err(),
        "RLS WITH CHECK must reject a forged tenant_id"
    );
    let visible = admin
        .query_one(&format!("SELECT count(*) FROM {schema}.notes"), &[])
        .await
        .unwrap()
        .get::<_, i64>(0);
    assert_eq!(visible, 1);
    let _ = &s.owner;
}

#[tokio::test]
async fn privileged_or_owning_roles_are_rejected() {
    let s = require_pg!(setup().await);
    let x = suffix();
    let cases = [
        ("su", format!("su_{x}"), "SUPERUSER".to_string()),
        ("bypass", format!("by_{x}"), "BYPASSRLS".to_string()),
        (
            "member-of-superuser",
            format!("ms_{x}"),
            "IN ROLE postgres".to_string(),
        ),
        (
            "noinherit-member-of-owner",
            format!("mo_{x}"),
            format!("NOINHERIT IN ROLE {}", s.owner),
        ),
    ];
    for (label, name, extra) in &cases {
        create_login(&s.admin, name, extra).await;
        let err = check_as(&s.admin_url, name)
            .await
            .expect_err(&format!("{label} must be rejected"));
        assert!(err.contains("refusing to serve"), "{label}: {err}");
    }
    // The owner itself.
    let err = check_as(&s.admin_url, &s.owner).await.unwrap_err();
    assert!(
        err.contains("tenant relation") && err.contains("tenant schema"),
        "{err}"
    );
    // NOINHERIT member of a BYPASSRLS group role.
    let group = format!("grp_{x}");
    s.admin
        .batch_execute(&format!("CREATE ROLE {group} NOLOGIN BYPASSRLS"))
        .await
        .unwrap();
    let member = format!("mb_{x}");
    create_login(&s.admin, &member, &format!("NOINHERIT IN ROLE {group}")).await;
    let err = check_as(&s.admin_url, &member).await.unwrap_err();
    assert!(err.contains(&group), "{err}");
}

#[tokio::test]
async fn migration_tables_get_forced_rls() {
    let s = require_pg!(setup().await);
    let row = s
        .admin
        .query_one(
            "SELECT c.relrowsecurity, c.relforcerowsecurity, \
             EXISTS (SELECT 1 FROM pg_policy p WHERE p.polrelid = c.oid AND p.polname = 'tenant_isolation') \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = 'audit_notes'",
            &[&format!("t_{}", s.tenant)],
        )
        .await
        .unwrap();
    assert!(row.get::<_, bool>(0) && row.get::<_, bool>(1) && row.get::<_, bool>(2));
    // And the runtime role is still accepted.
    check_as(&s.admin_url, &s.app).await.unwrap();
}

#[tokio::test]
async fn migration_without_tenant_id_or_with_a_view_fails() {
    let s = require_pg!(setup().await);
    let owner_exec = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    for (stmt, expect) in [
        (
            "CREATE TABLE IF NOT EXISTS loose (x text)",
            "without a tenant_id",
        ),
        ("CREATE VIEW v_notes AS SELECT * FROM notes", "views"),
        (
            "CREATE FUNCTION steal() RETURNS bigint LANGUAGE sql SECURITY DEFINER \
             AS 'SELECT count(*) FROM notes'",
            "SECURITY DEFINER",
        ),
        (
            "CREATE FUNCTION steal_trg() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER \
             AS 'BEGIN PERFORM set_config(''app.tenant_id'', ''other'', true); RETURN NEW; END'",
            "SECURITY DEFINER",
        ),
    ] {
        let mut m = manifest();
        m.migrations[0].statements = vec![stmt.to_string()];
        let tenant = format!("tv{}", suffix());
        let err = migrate_tenant(&owner_exec, &m, &tenant, &s.app)
            .await
            .unwrap_err();
        assert!(err.contains(expect), "{stmt}: {err}");
    }
}

#[tokio::test]
async fn serve_refuses_tables_without_forced_rls() {
    let s = require_pg!(setup_exclusive().await);
    let schema = format!("t_{}", s.tenant);
    // A table added behind migrate's back gets no grants by default; one
    // that a runtime role was granted (even a single column) is refused.
    s.admin
        .batch_execute(&format!(
            "SET ROLE {owner}; CREATE TABLE {schema}.later (tenant_id text); RESET ROLE; \
             GRANT SELECT (tenant_id) ON {schema}.later TO public_reader",
            owner = s.owner
        ))
        .await
        .unwrap();
    let err = check_as(&s.admin_url, &s.app).await.unwrap_err();
    assert!(err.contains(&format!("{schema}.later")), "{err}");
    s.admin
        .batch_execute(&format!(
            "ALTER TABLE {schema}.later ENABLE ROW LEVEL SECURITY"
        ))
        .await
        .unwrap();
    let err = check_as(&s.admin_url, &s.app).await.unwrap_err();
    assert!(
        err.contains("later"),
        "ENABLE without FORCE is not enough: {err}"
    );
    s.admin
        .batch_execute(&format!(
            "ALTER TABLE {schema}.later FORCE ROW LEVEL SECURITY"
        ))
        .await
        .unwrap();
    check_as(&s.admin_url, &s.app).await.unwrap();
    // Leave no RLS problem behind for other tests (it has no policy).
    s.admin
        .batch_execute(&format!("DROP TABLE {schema}.later"))
        .await
        .unwrap();
}

#[tokio::test]
async fn slow_queries_time_out_and_do_not_block_other_requests() {
    let s = require_pg!(setup().await);
    let exec = std::sync::Arc::new(
        PgExec::connect_with(
            &url_as(&s.admin_url, &s.app),
            vibe::runtime::PgOptions {
                pool_size: 4,
                statement_timeout_ms: 300,
                pool_timeout_ms: 2_000,
            },
        )
        .await
        .unwrap(),
    );
    // A request statement over the timeout is cancelled by the server: a
    // trigger makes inserts into this tenant's notes table sleep for 5s.
    let schema = format!("t_{}", s.tenant);
    s.admin
        .batch_execute(&format!(
            "CREATE FUNCTION {schema}.slow() RETURNS trigger LANGUAGE plpgsql AS \
             $$BEGIN PERFORM pg_sleep(5); RETURN NEW; END$$; \
             CREATE TRIGGER slow BEFORE INSERT ON {schema}.notes \
             FOR EACH ROW EXECUTE FUNCTION {schema}.slow()"
        ))
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let err = exec
        .execute(
            &s.tenant,
            "app_rw",
            vibe::runtime::Stmt::Insert {
                table: "notes".into(),
                values: vec![
                    (col("id", "uuid"), json!(uuid::Uuid::new_v4().to_string())),
                    (col("tenant_id", "string"), json!(s.tenant)),
                    (col("body", "string"), json!("slow")),
                ],
                returning: vec![col("id", "uuid")],
            },
        )
        .await;
    assert!(err.is_err(), "the insert should be cancelled");
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    // While one connection is busy, requests on others still complete.
    let busy = {
        let exec = exec.clone();
        tokio::spawn(async move { exec.batch(&["SELECT pg_sleep(1)".into()]).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let quick = std::time::Instant::now();
    let rows = exec
        .execute(
            &s.tenant,
            "app_rw",
            vibe::runtime::Stmt::Select {
                table: "notes".into(),
                columns: vec![col("id", "uuid")],
                id: None,
                filters: vec![],
                sort: None,
                limit: 1,
                offset: 0,
            },
        )
        .await;
    assert!(rows.is_ok(), "{rows:?}");
    assert!(quick.elapsed() < std::time::Duration::from_millis(800));
    busy.await.unwrap().unwrap();
}

fn col(name: &str, ty: &str) -> vibe::runtime::Col {
    vibe::runtime::Col {
        name: name.into(),
        ty: ty.into(),
    }
}

#[tokio::test]
async fn migration_policies_are_refused_and_nothing_is_committed() {
    let s = require_pg!(setup().await);
    let owner_exec = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    for stmt in [
        "CREATE POLICY open ON audit_notes USING (true)",
        "CREATE POLICY narrow ON audit_notes AS RESTRICTIVE USING (true)",
    ] {
        let mut m = manifest();
        m.migrations[0].statements.push(stmt.to_string());
        let tenant = format!("tp{}", suffix());
        let err = migrate_tenant(&owner_exec, &m, &tenant, &s.app)
            .await
            .unwrap_err();
        assert!(
            err.contains("must not create row level security policies"),
            "{stmt}: {err}"
        );
        // Transactional: the failed migration left no schema or tables behind.
        let left = s
            .admin
            .query_one(
                "SELECT count(*) FROM pg_namespace WHERE nspname = $1",
                &[&format!("t_{tenant}")],
            )
            .await
            .unwrap()
            .get::<_, i64>(0);
        assert_eq!(left, 0, "{stmt}: schema must be rolled back");
    }
    // The executor connection is still usable after the rollback.
    migrate_tenant(&owner_exec, &manifest(), &format!("tq{}", suffix()), &s.app)
        .await
        .unwrap();
}

#[tokio::test]
async fn serve_refuses_extra_or_altered_policies() {
    let s = require_pg!(setup_exclusive().await);
    let schema = format!("t_{}", s.tenant);
    check_as(&s.admin_url, &s.app).await.unwrap();
    // Reproduce the leak first: an extra permissive policy lets the runtime
    // role read another tenant's row.
    s.admin
        .batch_execute(&format!(
            "INSERT INTO {schema}.audit_notes VALUES ('othertenant1', 'leak'); \
             CREATE POLICY open ON {schema}.audit_notes USING (true)"
        ))
        .await
        .unwrap();
    let leaked = s
        .admin
        .batch_execute(&format!(
            "SET ROLE {app}; BEGIN; SET LOCAL ROLE app_rw; SELECT set_config('app.tenant_id', '{t}', true); \
             DO $$ BEGIN IF (SELECT count(*) FROM {schema}.audit_notes) = 0 THEN RAISE EXCEPTION 'none'; END IF; END $$; \
             COMMIT; RESET ROLE",
            app = s.app,
            t = s.tenant
        ))
        .await;
    let _ = s.admin.batch_execute("ROLLBACK; RESET ROLE").await;
    assert!(leaked.is_ok(), "extra permissive policy widens access");
    let err = check_as(&s.admin_url, &s.app).await.unwrap_err();
    assert!(
        err.contains(&format!("{schema}.audit_notes:open")) && err.contains("OR-combined"),
        "{err}"
    );
    s.admin
        .batch_execute(&format!("DROP POLICY open ON {schema}.audit_notes"))
        .await
        .unwrap();
    check_as(&s.admin_url, &s.app).await.unwrap();
    // A tenant_isolation whose expression was altered is refused as well.
    s.admin
        .batch_execute(&format!(
            "ALTER POLICY tenant_isolation ON {schema}.notes USING (true)"
        ))
        .await
        .unwrap();
    let err = check_as(&s.admin_url, &s.app).await.unwrap_err();
    assert!(err.contains("notes:tenant_isolation"), "{err}");
    // Restricted to a role instead of PUBLIC: refused.
    s.admin
        .batch_execute(&format!(
            "DROP POLICY tenant_isolation ON {schema}.notes; \
             CREATE POLICY tenant_isolation ON {schema}.notes TO {app} \
             USING (\"tenant_id\" = current_setting('app.tenant_id', false)) \
             WITH CHECK (\"tenant_id\" = current_setting('app.tenant_id', false))",
            app = s.app
        ))
        .await
        .unwrap();
    assert!(check_as(&s.admin_url, &s.app).await.is_err());
    // Re-running migrate restores the canonical policy.
    let owner_exec = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    migrate_tenant(&owner_exec, &manifest(), &s.tenant, &s.app)
        .await
        .unwrap();
    check_as(&s.admin_url, &s.app).await.unwrap();
}

/// Count of rows the runtime role sees in `table` while acting as `tenant`.
async fn visible_rows(s: &Setup, table: &str, tenant: &str) -> i64 {
    let app = connect(&url_as(&s.admin_url, &s.app)).await;
    app.batch_execute("BEGIN; SET LOCAL ROLE app_rw")
        .await
        .unwrap();
    app.query("SELECT set_config('app.tenant_id', $1, true)", &[&tenant])
        .await
        .unwrap();
    let n: i64 = app
        .query_one(&format!("SELECT count(*) FROM {table}"), &[])
        .await
        .unwrap()
        .get(0);
    app.batch_execute("ROLLBACK").await.unwrap();
    n
}

#[tokio::test]
async fn migration_cannot_weaken_another_tenant() {
    let s = require_pg!(setup().await);
    let victim = format!("t_{}", s.tenant);
    s.admin
        .batch_execute(&format!(
            "INSERT INTO {victim}.audit_notes VALUES ('{t}', 'secret')",
            t = s.tenant
        ))
        .await
        .unwrap();
    let owner_exec = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    let attacks = [
        format!("CREATE POLICY open ON {victim}.audit_notes USING (true)"),
        format!("CREATE POLICY open ON {victim}.audit_notes AS RESTRICTIVE USING (true)"),
        format!("ALTER TABLE {victim}.audit_notes NO FORCE ROW LEVEL SECURITY"),
        format!("ALTER TABLE {victim}.audit_notes DISABLE ROW LEVEL SECURITY"),
        format!("ALTER POLICY tenant_isolation ON {victim}.audit_notes USING (true)"),
        format!("CREATE VIEW {victim}.peek AS SELECT * FROM {victim}.audit_notes"),
        format!("ALTER TABLE {victim}.audit_notes ADD COLUMN extra text"),
        format!("GRANT DELETE ON {victim}.audit_notes TO PUBLIC"),
    ];
    for attack in attacks {
        let mut m = manifest();
        m.migrations[0].statements.push(attack.clone());
        let tenant = format!("ta{}", suffix());
        let err = migrate_tenant(&owner_exec, &m, &tenant, &s.app)
            .await
            .unwrap_err();
        assert!(
            err.contains("row level security check failed") || err.contains("another tenant"),
            "{attack}: {err}"
        );
        // Nothing was committed: the attacker's schema does not exist and
        // the victim is unchanged.
        let left: i64 = s
            .admin
            .query_one(
                "SELECT count(*) FROM pg_namespace WHERE nspname = $1",
                &[&format!("t_{tenant}")],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(left, 0, "{attack}");
        assert_eq!(
            visible_rows(&s, &format!("{victim}.audit_notes"), "othertenant1").await,
            0,
            "{attack}: victim rows must stay hidden from other tenants"
        );
        check_as(&s.admin_url, &s.app).await.unwrap();
    }
}

#[tokio::test]
async fn migration_cannot_end_the_transaction() {
    let s = require_pg!(setup().await);
    let owner_exec = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    for attack in [
        "COMMIT",
        "END",
        "ROLLBACK",
        "begin",
        "START TRANSACTION",
        "SAVEPOINT s",
        "SET ROLE postgres",
    ] {
        let mut m = manifest();
        m.migrations[0].statements.push(attack.to_string());
        let tenant = format!("tx{}", suffix());
        let err = migrate_tenant(&owner_exec, &m, &tenant, &s.app)
            .await
            .unwrap_err();
        assert!(err.contains("not allowed in migrations"), "{attack}: {err}");
        let left: i64 = s
            .admin
            .query_one(
                "SELECT count(*) FROM pg_namespace WHERE nspname = $1",
                &[&format!("t_{tenant}")],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(left, 0, "{attack}: nothing may be committed");
    }
    // Procedures that COMMIT cannot end the transaction either.
    let mut m = manifest();
    m.migrations[0]
        .statements
        .push("CREATE PROCEDURE sneaky() LANGUAGE plpgsql AS $$ BEGIN COMMIT; END $$".into());
    m.migrations[0].statements.push("CALL sneaky()".into());
    let tenant = format!("ty{}", suffix());
    migrate_tenant(&owner_exec, &m, &tenant, &s.app)
        .await
        .unwrap_err();
    let left: i64 = s
        .admin
        .query_one(
            "SELECT count(*) FROM pg_namespace WHERE nspname = $1",
            &[&format!("t_{tenant}")],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(left, 0);
    // The executor connection still works afterwards.
    migrate_tenant(&owner_exec, &manifest(), &format!("tz{}", suffix()), &s.app)
        .await
        .unwrap();
}

#[tokio::test]
async fn postgres_errors_carry_detail() {
    let s = require_pg!(setup().await);
    let owner_exec = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    let mut m = manifest();
    m.migrations[0]
        .statements
        .push("ALTER TABLE no_such_table ADD COLUMN x text".into());
    let err = migrate_tenant(&owner_exec, &m, &format!("te{}", suffix()), &s.app)
        .await
        .unwrap_err();
    assert!(
        err.contains("no_such_table") && err.contains("42P01"),
        "{err}"
    );
}

#[tokio::test]
async fn pooled_connections_come_back_clean() {
    let s = require_pg!(setup().await);
    let exec = PgExec::connect_with(
        &url_as(&s.admin_url, &s.app),
        vibe::runtime::PgOptions {
            pool_size: 1,
            statement_timeout_ms: 5_000,
            pool_timeout_ms: 2_000,
        },
    )
    .await
    .unwrap();
    {
        // Dropped mid-transaction with session state changed.
        let session = exec.session().await.unwrap();
        session
            .batch(&[
                "BEGIN".into(),
                "CREATE TEMP TABLE leak (x int)".into(),
                "SET app.tenant_id = 'leaked'".into(),
            ])
            .await
            .unwrap();
    }
    let got = exec
        .query_texts(
            "SELECT (to_regclass('pg_temp.leak') IS NULL)::text || '|' || \
             coalesce(current_setting('app.tenant_id', true), '') || '|' || \
             coalesce(txid_current_if_assigned()::text, 'none') WHERE $1 = ''",
            "",
        )
        .await
        .unwrap();
    assert_eq!(got, vec!["true||none".to_string()]);
}

#[tokio::test]
async fn smuggled_commit_after_a_dollar_identifier_is_refused() {
    // Review repro: `$x$` inside identifiers looked like dollar quoting to the
    // old lexer, hiding a COMMIT and a cross-tenant policy.
    let s = require_pg!(setup().await);
    let victim = format!("t_{}", s.tenant);
    let owner_exec = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    for attack in [
        format!(
            "CREATE TABLE zq (tenant_id text, a$x$ int); COMMIT; \
             CREATE POLICY open ON {victim}.audit_notes USING (true); SELECT 1 AS b$x$"
        ),
        "CREATE TABLE leaky (tenant_id text, x text); COMMIT; BEGIN".to_string(),
    ] {
        let mut m = manifest();
        m.migrations[0].statements.push(attack.clone());
        let tenant = format!("td{}", suffix());
        let err = migrate_tenant(&owner_exec, &m, &tenant, &s.app)
            .await
            .unwrap_err();
        assert!(
            err.contains("42601") || err.contains("multiple commands"),
            "{attack}: {err}"
        );
        let left: i64 = s
            .admin
            .query_one(
                "SELECT count(*) FROM pg_namespace WHERE nspname = $1",
                &[&format!("t_{tenant}")],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(left, 0, "{attack}: nothing may be committed");
        let policies: i64 = s
            .admin
            .query_one(
                "SELECT count(*) FROM pg_policy p JOIN pg_class c ON c.oid = p.polrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = $1 AND p.polname = 'open'",
                &[&victim],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(policies, 0, "{attack}: no cross-tenant policy");
    }
}

#[tokio::test]
async fn migration_cannot_touch_another_tenants_types_or_default_privileges() {
    let s = require_pg!(setup().await);
    let victim = format!("t_{}", s.tenant);
    let owner_exec = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    for attack in [
        format!("ALTER DEFAULT PRIVILEGES IN SCHEMA {victim} GRANT DELETE ON TABLES TO PUBLIC"),
        format!("CREATE TYPE {victim}.mood AS ENUM ('a')"),
        format!("CREATE DOMAIN {victim}.d AS text"),
        format!("CREATE SEQUENCE {victim}.s"),
        format!("CREATE INDEX ON {victim}.audit_notes (note)"),
        format!("ALTER TABLE {victim}.audit_notes ADD CONSTRAINT c CHECK (true)"),
    ] {
        let mut m = manifest();
        m.migrations[0].statements.push(attack.clone());
        let err = migrate_tenant(&owner_exec, &m, &format!("tf{}", suffix()), &s.app)
            .await
            .unwrap_err();
        assert!(err.contains("another tenant"), "{attack}: {err}");
    }
}

#[tokio::test]
async fn existing_rls_problems_fail_every_migration() {
    let s = require_pg!(setup_exclusive().await);
    let victim = format!("t_{}", s.tenant);
    // Left behind outside vibe (e.g. by an older, unprotected migration).
    s.admin
        .batch_execute(&format!(
            "ALTER TABLE {victim}.audit_notes NO FORCE ROW LEVEL SECURITY"
        ))
        .await
        .unwrap();
    assert!(
        check_as(&s.admin_url, &s.app).await.is_err(),
        "serve refuses"
    );
    let owner_exec = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    let err = migrate_tenant(&owner_exec, &manifest(), &format!("tg{}", suffix()), &s.app)
        .await
        .unwrap_err();
    assert!(err.contains("already present before this run"), "{err}");
    assert!(err.contains(&victim), "{err}");
    // Migrating the affected tenant repairs it.
    migrate_tenant(&owner_exec, &manifest(), &s.tenant, &s.app)
        .await
        .unwrap();
    check_as(&s.admin_url, &s.app).await.unwrap();
}

const PENDING_ROLE_SPEC: &str =
    "pending spec PR + PM approval: per-tenant migrate role (SET LOCAL ROLE tenant_<id>, NOINHERIT, own-schema grants only)";

/// Migrates a fresh attacker tenant with `attack` appended; returns its schema.
async fn attack_data(s: &Setup, attack: &[String]) -> String {
    let owner_exec = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    let mut m = manifest();
    m.migrations[0].statements.extend(attack.iter().cloned());
    let tenant = format!("th{}", suffix());
    let _ = migrate_tenant(&owner_exec, &m, &tenant, &s.app).await;
    format!("t_{tenant}")
}

#[tokio::test]
#[ignore = "pending spec: per-tenant migrate role; see PENDING_ROLE_SPEC"]
async fn migration_cannot_read_another_tenants_rows() {
    let s = require_pg!(setup().await);
    let victim = format!("t_{}", s.tenant);
    s.admin
        .batch_execute(&format!(
            "INSERT INTO {victim}.audit_notes VALUES ('{}', 'secret')",
            s.tenant
        ))
        .await
        .unwrap();
    let loot = attack_data(
        &s,
        &[
            format!("SELECT set_config('app.tenant_id', '{}', true)", s.tenant),
            format!("INSERT INTO audit_notes SELECT 'x', note FROM {victim}.audit_notes"),
        ],
    )
    .await;
    let copied: i64 = s
        .admin
        .query_one(
            "SELECT count(*) FROM pg_namespace WHERE nspname = $1",
            &[&loot],
        )
        .await
        .unwrap()
        .get(0);
    let leaked = if copied == 0 {
        0
    } else {
        s.admin
            .query_one(
                &format!("SELECT count(*) FROM {loot}.audit_notes WHERE note = 'secret'"),
                &[],
            )
            .await
            .unwrap()
            .get::<_, i64>(0)
    };
    assert_eq!(leaked, 0, "{PENDING_ROLE_SPEC}");
}

#[tokio::test]
#[ignore = "pending spec: per-tenant migrate role; see PENDING_ROLE_SPEC"]
async fn migration_cannot_write_another_tenants_rows() {
    let s = require_pg!(setup().await);
    let victim = format!("t_{}", s.tenant);
    s.admin
        .batch_execute(&format!(
            "INSERT INTO {victim}.audit_notes VALUES ('{}', 'secret')",
            s.tenant
        ))
        .await
        .unwrap();
    attack_data(
        &s,
        &[
            format!("SELECT set_config('app.tenant_id', '{}', true)", s.tenant),
            format!("UPDATE {victim}.audit_notes SET note = 'pwned'"),
        ],
    )
    .await;
    let mut m = manifest();
    m.migrations[0].statements.extend([
        format!("ALTER TABLE {victim}.audit_notes NO FORCE ROW LEVEL SECURITY"),
        format!("UPDATE {victim}.audit_notes SET note = 'pwned'"),
        format!("ALTER TABLE {victim}.audit_notes FORCE ROW LEVEL SECURITY"),
    ]);
    let owner_exec = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    let _ = migrate_tenant(&owner_exec, &m, &format!("ti{}", suffix()), &s.app).await;
    let note: String = s
        .admin
        .query_one(
            &format!("SELECT note FROM {victim}.audit_notes LIMIT 1"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(note, "secret", "{PENDING_ROLE_SPEC}");
}

/// Outcome class of a statement, comparable across executors.
fn outcome(
    result: Result<Vec<serde_json::Map<String, Value>>, vibe::runtime::ExecError>,
) -> String {
    use vibe::runtime::ExecError;
    match result {
        Ok(rows) => format!("ok:{}", rows.len()),
        Err(ExecError::Denied(_)) => "denied".into(),
        Err(ExecError::NotFound) => "not_found".into(),
        Err(ExecError::Precondition) => "precondition".into(),
        Err(ExecError::Conflict) => "conflict".into(),
        Err(ExecError::Message(_)) => "error".into(),
    }
}

fn pcol(name: &str) -> Col {
    let ty = match name {
        "id" => "uuid",
        "created_at" | "updated_at" | "deleted_at" => "timestamp",
        _ => "string",
    };
    Col {
        name: name.into(),
        ty: ty.into(),
    }
}

fn pcols(names: &[&str]) -> Vec<Col> {
    names.iter().map(|n| pcol(n)).collect()
}

fn p_insert(id: &str, extra: &[(&str, &str)], returning: &[&str]) -> Stmt {
    let now = chrono::Utc::now().to_rfc3339();
    let mut values = vec![
        (pcol("id"), json!(id)),
        (pcol("created_at"), json!(now)),
        (pcol("updated_at"), json!(now)),
    ];
    for (c, v) in extra {
        values.push((pcol(c), json!(v)));
    }
    Stmt::Insert {
        table: "notes".into(),
        values,
        returning: pcols(returning),
    }
}

fn p_select(columns: &[&str], filter: Option<(&str, &str)>, sort: Option<&str>) -> Stmt {
    Stmt::Select {
        table: "notes".into(),
        columns: pcols(columns),
        id: None,
        filters: filter
            .map(|(c, v)| vec![(pcol(c), json!(v))])
            .unwrap_or_default(),
        sort: sort.map(|c| vibe::runtime::Sort {
            column: c.into(),
            desc: false,
        }),
        limit: 100,
        offset: 0,
    }
}

fn p_update(id: &str, column: &str, expected: &str) -> Stmt {
    Stmt::Update {
        table: "notes".into(),
        id: id.into(),
        sets: vec![(pcol(column), json!("x"))],
        expected_updated_at: expected.into(),
        touched_at: chrono::Utc::now().to_rfc3339(),
        returning: pcols(&["id"]),
        owner: None,
    }
}

/// Runs the same statements through the in-memory executor and PostgreSQL
/// (as the real runtime login, with `SET LOCAL ROLE`) and requires the same
/// outcome class for each.
#[tokio::test]
async fn memory_and_postgres_agree() {
    let s = require_pg!(setup().await);
    let other = format!("to{}", suffix());
    let owner_exec = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    migrate_tenant(&owner_exec, &manifest(), &other, &s.app)
        .await
        .unwrap();
    let pg = PgExec::connect(&url_as(&s.admin_url, &s.app))
        .await
        .unwrap();
    pg.check_runtime_role().await.unwrap();
    let mem = vibe::runtime::MemoryExec::from_manifest(&manifest()).unwrap();
    let t = s.tenant.clone();
    let row1 = uuid::Uuid::new_v4().to_string();
    let missing = uuid::Uuid::new_v4().to_string();
    let w1 = uuid::Uuid::new_v4().to_string();
    let w2 = uuid::Uuid::new_v4().to_string();
    let w3 = uuid::Uuid::new_v4().to_string();
    let r1 = uuid::Uuid::new_v4().to_string();
    // (name, tenant, role, statement, expected outcome)
    let cases: Vec<(&str, &str, &str, Stmt, &str)> = vec![
        (
            "rw insert",
            &t,
            "app_rw",
            p_insert(&row1, &[("body", "b")], &["id", "body"]),
            "ok:1",
        ),
        (
            "rw select",
            &t,
            "app_rw",
            p_select(&["id", "body"], None, Some("created_at")),
            "ok:1",
        ),
        (
            "other tenant sees nothing",
            &other,
            "app_rw",
            p_select(&["id"], None, None),
            "ok:0",
        ),
        (
            "other tenant cannot update",
            &other,
            "app_rw",
            p_update(&row1, "body", "2000-01-01T00:00:00Z"),
            "precondition",
        ),
        (
            "writer insert returning an unreadable column",
            &t,
            "public_writer",
            p_insert(&w1, &[("body", "b")], &["id", "body"]),
            "denied",
        ),
        (
            "writer insert returning readable columns",
            &t,
            "public_writer",
            p_insert(&w2, &[("body", "b")], &["id", "updated_at"]),
            "ok:1",
        ),
        (
            "writer insert with tenant_id",
            &t,
            "public_writer",
            p_insert(&w3, &[("tenant_id", &t)], &["id"]),
            "denied",
        ),
        (
            "writer cannot update",
            &t,
            "public_writer",
            p_update(&row1, "body", "2000-01-01T00:00:00Z"),
            "denied",
        ),
        (
            "writer cannot select",
            &t,
            "public_writer",
            p_select(&["body"], None, None),
            "denied",
        ),
        (
            "rw update tenant_id",
            &t,
            "app_rw",
            p_update(&row1, "tenant_id", "2000-01-01T00:00:00Z"),
            "denied",
        ),
        (
            "rw stale update",
            &t,
            "app_rw",
            p_update(&row1, "body", "2000-01-01T00:00:00Z"),
            "precondition",
        ),
        (
            "rw update missing row",
            &t,
            "app_rw",
            p_update(&missing, "body", "2000-01-01T00:00:00Z"),
            "precondition",
        ),
        (
            "rw soft delete missing row",
            &t,
            "app_rw",
            Stmt::SoftDelete {
                table: "notes".into(),
                id: missing.clone(),
                touched_at: "2000-01-01T00:00:00Z".into(),
                owner: None,
            },
            "not_found",
        ),
        (
            "reader select granted column",
            &t,
            "public_reader",
            p_select(&["body"], Some(("body", "b")), None),
            "ok:2",
        ),
        (
            "reader select ungranted column",
            &t,
            "public_reader",
            p_select(&["tenant_id"], None, None),
            "denied",
        ),
        (
            "reader sort by ungranted column",
            &t,
            "public_reader",
            p_select(&["body"], None, Some("created_at")),
            "denied",
        ),
        (
            "reader cannot insert",
            &t,
            "public_reader",
            p_insert(&r1, &[("body", "b")], &["id"]),
            "denied",
        ),
        (
            "writer cannot soft delete",
            &t,
            "public_writer",
            Stmt::SoftDelete {
                table: "notes".into(),
                id: row1.clone(),
                touched_at: "2000-01-01T00:00:00Z".into(),
                owner: None,
            },
            "denied",
        ),
        (
            "invalid tenant",
            "",
            "app_rw",
            p_select(&["id"], None, None),
            "error",
        ),
        (
            "rw soft delete",
            &t,
            "app_rw",
            Stmt::SoftDelete {
                table: "notes".into(),
                id: row1.clone(),
                touched_at: "2000-01-01T00:00:00Z".into(),
                owner: None,
            },
            "ok:1",
        ),
        (
            "deleted rows are hidden",
            &t,
            "app_rw",
            p_select(&["id"], None, None),
            "ok:1",
        ),
    ];
    let mut mismatches = Vec::new();
    for (name, tenant, role, stmt, expected) in cases {
        let got_mem = outcome(mem.execute(tenant, role, stmt.clone()).await);
        let got_pg = outcome(pg.execute(tenant, role, stmt).await);
        if got_mem != expected || got_pg != expected {
            mismatches.push(format!(
                "{name}: expected {expected}, memory {got_mem}, postgres {got_pg}"
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    // A real update with the current version succeeds in both.
    for (label, exec) in [("memory", &mem as &dyn QueryExec), ("postgres", &pg)] {
        let current = exec
            .execute(
                &t,
                "app_rw",
                p_select(&["updated_at"], Some(("body", "b")), None),
            )
            .await
            .unwrap();
        let ts = current[0]["updated_at"].as_str().unwrap().to_string();
        let id_rows = exec
            .execute(&t, "app_rw", p_select(&["id"], Some(("body", "b")), None))
            .await
            .unwrap();
        let id = id_rows[0]["id"].as_str().unwrap().to_string();
        let got = outcome(exec.execute(&t, "app_rw", p_update(&id, "body", &ts)).await);
        assert_eq!(got, "ok:1", "{label}");
    }
}

/// What PostgreSQL itself does when a statement runs without the tenant
/// setting. The executors never do this (they refuse an invalid tenant up
/// front); the test documents the database-level behaviour on a pooled
/// connection, where an earlier transaction already defined the setting.
#[tokio::test]
async fn missing_tenant_setting_in_postgres() {
    let s = require_pg!(setup().await);
    let schema = format!("t_{}", s.tenant);
    let app = connect(&url_as(&s.admin_url, &s.app)).await;
    // Fresh session: the setting was never defined, so the policy errors.
    let fresh = app
        .batch_execute(&format!(
            "BEGIN; SET LOCAL ROLE app_rw; SELECT count(*) FROM {schema}.notes"
        ))
        .await;
    app.batch_execute("ROLLBACK").await.unwrap();
    assert!(fresh.is_err(), "a fresh session must error");
    // Reused session: `set_config(.., true)` left the setting defined as
    // '' after commit, so the same query matches no tenant and returns 0
    // rows instead of failing.
    app.batch_execute(&format!(
        "BEGIN; SELECT set_config('app.tenant_id', '{}', true); COMMIT",
        s.tenant
    ))
    .await
    .unwrap();
    app.batch_execute("BEGIN; SET LOCAL ROLE app_rw")
        .await
        .unwrap();
    let n: i64 = app
        .query_one(&format!("SELECT count(*) FROM {schema}.notes"), &[])
        .await
        .unwrap()
        .get(0);
    app.batch_execute("ROLLBACK").await.unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn cancelled_migration_does_not_return_its_connection_mid_transaction() {
    let s = require_pg!(setup().await);
    let exec = PgExec::connect_with(
        &url_as(&s.admin_url, &s.owner),
        vibe::runtime::PgOptions {
            pool_size: 1,
            statement_timeout_ms: 30_000,
            pool_timeout_ms: 5_000,
        },
    )
    .await
    .unwrap();
    let mut m = manifest();
    m.migrations[0].statements.push("SELECT pg_sleep(5)".into());
    let tenant = format!("tc{}", suffix());
    // Cancel the migration while it sleeps inside its transaction, after
    // reading the pid of the backend running it from pg_stat_activity.
    let find_pid = async {
        loop {
            let rows = s
                .admin
                .query(
                    "SELECT pid FROM pg_stat_activity WHERE usename = $1 \
                     AND state = 'active' AND query LIKE 'SELECT pg_sleep(5)%'",
                    &[&s.owner],
                )
                .await
                .unwrap();
            if let Some(row) = rows.first() {
                return row.get::<_, i32>(0);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    };
    let migration = migrate_tenant(&exec, &m, &tenant, &s.app);
    let sleeping_pid = tokio::select! {
        pid = tokio::time::timeout(std::time::Duration::from_secs(4), find_pid) => {
            pid.expect("the migration's pg_sleep never showed up in pg_stat_activity")
        }
        done = migration => panic!("the migration should still be running: {done:?}"),
    };
    // The cancelled future is dropped here. The next query must not wait for
    // that backend or reuse it: the pool hands out a different backend while
    // the old one is still sleeping in its transaction.
    let started = std::time::Instant::now();
    let pid = exec
        .query_texts("SELECT pg_backend_pid()::text || $1::text", "")
        .await
        .unwrap();
    assert_ne!(
        pid,
        vec![sleeping_pid.to_string()],
        "the connection was returned to the pool mid-transaction"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "the next query waited for the cancelled transaction"
    );
    let old_state = s
        .admin
        .query(
            "SELECT state FROM pg_stat_activity WHERE pid = $1",
            &[&sleeping_pid],
        )
        .await
        .unwrap();
    assert!(
        old_state
            .first()
            .is_none_or(|r| r.get::<_, Option<String>>(0).as_deref() != Some("idle")),
        "the old backend went back to the pool as idle"
    );
    // The pool's only connection was discarded, not reused mid-transaction:
    // a fresh session sees no open transaction and the schema never existed.
    let got = exec
        .query_texts(
            "SELECT coalesce(txid_current_if_assigned()::text, 'none') || '|' || \
             (SELECT count(*) FROM pg_namespace WHERE nspname = $1)::text",
            &format!("t_{tenant}"),
        )
        .await
        .unwrap();
    assert_eq!(got, vec!["none|0".to_string()]);
}
