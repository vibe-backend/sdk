//! Real PostgreSQL tests. They run when `VIBE_TEST_PG_ADMIN_URL` points at a
//! superuser connection (CI sets it via a service container). Outside CI they
//! are skipped with a SKIPPED note; in CI (`CI` set) a missing URL fails.

use serde_json::{json, Value};
use tokio_postgres::{Client, NoTls};
use vibe::manifest::Manifest;
use vibe::runtime::challenge::ChallengeError;
use vibe::runtime::replay::{token_key, Outcome, PgReplayStore, ReplayStore};
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

fn manifest() -> Manifest {
    serde_json::from_value(json!({
        "manifest_version": 1,
        "routes": [],
        "methods": [],
        "signature": "",
        "models": [{
            "id": "note", "provider": "main", "table": "notes",
            "crud": [], "sensitive_output": [], "ownership": "",
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
    // Every test has its own owner role; the exclusive CORRUPTION guard
    // isolates this shared platform schema as well as tenant DDL.
    admin
        .batch_execute("DROP SCHEMA IF EXISTS vibe_platform CASCADE")
        .await
        .unwrap();
    let s = suffix();
    let owner = format!("own_{s}");
    let app = format!("app_{s}");
    create_login(&admin, &owner, "").await;
    create_login(&admin, &app, "").await;
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
        vec![true, true, true, false, false, false, true],
        "select/insert/update only, no create, migration tables covered by default privileges"
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
                (cols(&["tenant_id"])[0].clone(), json!(s.tenant)),
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
            "SET ROLE {app}; BEGIN; SELECT set_config('app.tenant_id', 'othertenant1', true); \
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
        (
            "CREATE PROCEDURE steal_proc() LANGUAGE sql SECURITY DEFINER              AS 'SELECT count(*) FROM notes'",
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
        if stmt.contains("CREATE PROCEDURE") {
            assert!(
                err.contains("procedure "),
                "procedure listing should say 'procedure ': {err}"
            );
            assert!(
                err.contains("functions, procedures and triggers"),
                "outer message should name procedures: {err}"
            );
        }
    }
}

#[tokio::test]
async fn serve_refuses_tables_without_forced_rls() {
    let s = require_pg!(setup_exclusive().await);
    let schema = format!("t_{}", s.tenant);
    // A table added behind migrate's back inherits the default grants.
    s.admin
        .batch_execute(&format!(
            "SET ROLE {owner}; CREATE TABLE {schema}.later (tenant_id text); RESET ROLE",
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
            "SET ROLE {app}; BEGIN; SELECT set_config('app.tenant_id', '{t}', true); \
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
    app.batch_execute("BEGIN").await.unwrap();
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

// #157: challenge replay store (one table per tenant schema).

const IP_SECRET: &[u8] = b"test-ip-secret-test-ip-secret-00";

async fn replay_pool(s: &Setup) -> deadpool_postgres::Pool {
    PgExec::connect(&url_as(&s.admin_url, &s.app))
        .await
        .unwrap()
        .pool()
}

/// Two "instances": separate pools as the runtime role.
async fn replay_stores(s: &Setup) -> (PgReplayStore, PgReplayStore) {
    (
        PgReplayStore::new(replay_pool(s).await, IP_SECRET),
        PgReplayStore::new(replay_pool(s).await, IP_SECRET),
    )
}

/// A second tenant migrated by the same owner and runtime roles.
async fn second_tenant(s: &Setup) -> String {
    let _guard = SETUP.lock().await;
    let tenant = format!("tm{}", suffix());
    let owner_exec = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    migrate_tenant(&owner_exec, &manifest(), &tenant, &s.app)
        .await
        .unwrap();
    tenant
}

fn is_rejected<T>(r: Result<T, ChallengeError>) -> bool {
    matches!(r, Err(ChallengeError::Rejected(_)))
}

fn is_unavailable<T>(r: Result<T, ChallengeError>) -> bool {
    matches!(r, Err(ChallengeError::Unavailable(_)))
}

#[tokio::test]
async fn replay_store_is_single_use_across_instances() {
    let s = require_pg!(setup().await);
    let (a, b) = replay_stores(&s).await;
    let t = Some(s.tenant.as_str());
    a.check(std::slice::from_ref(&s.tenant)).await.unwrap();
    let k = token_key("tok-1");
    let slot = a.reserve(&k, Some("203.0.113.1"), t).await.unwrap();
    // Pending on A: B refuses concurrent use.
    assert!(is_rejected(b.reserve(&k, Some("203.0.113.2"), t).await));
    a.settle(&slot, Outcome::Verified).await.unwrap();
    assert!(is_rejected(b.reserve(&k, None, t).await));
    // Releasing a verified reservation afterwards changes nothing.
    b.settle(&slot, Outcome::Released).await.unwrap();
    assert!(is_rejected(b.reserve(&k, None, t).await));
    // Released and rejected (with no hold) slots are free again.
    let k2 = token_key("tok-2");
    let s1 = a.reserve(&k2, None, t).await.unwrap();
    a.settle(&s1, Outcome::Released).await.unwrap();
    let s2 = b.reserve(&k2, None, t).await.unwrap();
    // A stale reservation cannot settle a newer one.
    assert!(is_unavailable(a.settle(&s1, Outcome::Verified).await));
    let b0 = PgReplayStore::new(replay_pool(&s).await, IP_SECRET)
        .with_rejected_hold(std::time::Duration::ZERO);
    b0.settle(&s2, Outcome::Rejected).await.unwrap();
    let s3 = a.reserve(&k2, None, t).await.unwrap();
    // With a hold, a rejected token stays blocked.
    a.settle(&s3, Outcome::Rejected).await.unwrap();
    assert!(is_rejected(b.reserve(&k2, None, t).await));
}

#[tokio::test]
async fn replay_store_concurrent_reservations_admit_one() {
    let s = require_pg!(setup().await);
    let (a, b) = replay_stores(&s).await;
    let t = Some(s.tenant.as_str());
    for i in 0..5 {
        let k = token_key(&format!("race-{i}"));
        let (ra, rb) = tokio::join!(a.reserve(&k, None, t), b.reserve(&k, None, t));
        assert_eq!(
            [ra.is_ok(), rb.is_ok()].iter().filter(|ok| **ok).count(),
            1,
            "round {i}"
        );
    }
}

#[tokio::test]
async fn replay_store_runtime_role_has_no_delete_or_update() {
    let s = require_pg!(setup().await);
    let table = format!("t_{}.vibe_challenge_replay", s.tenant);
    let rows = s
        .admin
        .query(
            "SELECT has_table_privilege($1, $2::text::regclass, 'SELECT'), \
             has_table_privilege($1, $2::text::regclass, 'INSERT'), \
             has_table_privilege($1, $2::text::regclass, 'DELETE'), \
             has_table_privilege($1, $2::text::regclass, 'UPDATE'), \
             has_table_privilege($1, $2::text::regclass, 'TRUNCATE')",
            &[&s.app, &table],
        )
        .await
        .unwrap();
    let got: Vec<bool> = (0..5).map(|i| rows[0].get(i)).collect();
    assert_eq!(got, vec![true, true, false, false, false]);
    // A verified row cannot be removed or shortened by the runtime role.
    let (a, b) = replay_stores(&s).await;
    let t = Some(s.tenant.as_str());
    let k = token_key("kept");
    let slot = a.reserve(&k, None, t).await.unwrap();
    a.settle(&slot, Outcome::Verified).await.unwrap();
    let app = connect(&url_as(&s.admin_url, &s.app)).await;
    let set = format!("SET app.tenant_id = '{}'", s.tenant);
    app.batch_execute(&set).await.unwrap();
    assert!(app
        .batch_execute(&format!("DELETE FROM {table}"))
        .await
        .is_err());
    assert!(app
        .batch_execute(&format!("UPDATE {table} SET expires_at = now()"))
        .await
        .is_err());
    assert!(app
        .batch_execute(&format!("TRUNCATE {table}"))
        .await
        .is_err());
    assert!(is_rejected(b.reserve(&k, None, t).await));
    // No shared object outside tenant schemas.
    let shared: i64 = s
        .admin
        .query_one(
            "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relname = 'vibe_challenge_replay' AND n.nspname NOT LIKE 't\\_%'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(shared, 0);
}

#[tokio::test]
async fn replay_store_isolates_tenants() {
    let s = require_pg!(setup().await);
    let other = second_tenant(&s).await;
    let (a, b) = replay_stores(&s).await;
    let a = a.with_capacity(2);
    let b = b.with_capacity(2);
    let (ta, tb) = (Some(s.tenant.as_str()), Some(other.as_str()));
    let k = token_key("same-token");
    a.reserve(&k, Some("192.0.2.7"), ta).await.unwrap();
    // The same token is independent per tenant.
    b.reserve(&k, Some("192.0.2.7"), tb).await.unwrap();
    // Per-tenant capacity: filling one tenant leaves the other usable.
    a.reserve(&token_key("fill"), None, ta).await.unwrap();
    assert!(is_unavailable(
        a.reserve(&token_key("over"), None, ta).await
    ));
    b.reserve(&token_key("other-ok"), None, tb).await.unwrap();
    // Cross-tenant reads return nothing.
    let app = connect(&url_as(&s.admin_url, &s.app)).await;
    app.batch_execute(&format!("SET app.tenant_id = '{other}'"))
        .await
        .unwrap();
    let n: i64 = app
        .query_one(
            &format!("SELECT count(*) FROM t_{}.vibe_challenge_replay", s.tenant),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 0);
    let own: i64 = app
        .query_one(
            &format!("SELECT count(*) FROM t_{other}.vibe_challenge_replay"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(own > 0);
    // Rows for another tenant cannot be written through this tenant's context.
    assert!(app
        .batch_execute(&format!(
            "INSERT INTO t_{t}.vibe_challenge_replay (tenant_id, key, nonce, state, expires_at) \
             VALUES ('{t}', '\\x00', '\\x00', 'verified', now() + interval '1 hour')",
            t = s.tenant
        ))
        .await
        .is_err());
    // IP digests differ per tenant.
    let digests = s
        .admin
        .query(
            &format!(
                "SELECT ip_hash FROM t_{}.vibe_challenge_replay WHERE ip_hash IS NOT NULL \
                 UNION ALL SELECT ip_hash FROM t_{other}.vibe_challenge_replay WHERE ip_hash IS NOT NULL",
                s.tenant
            ),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(digests.len(), 2);
    assert_ne!(
        digests[0].get::<_, Vec<u8>>(0),
        digests[1].get::<_, Vec<u8>>(0)
    );
}

#[tokio::test]
async fn replay_store_quotas_hold_across_instances() {
    let s = require_pg!(setup().await);
    let (a, b) = replay_stores(&s).await;
    let a = a.with_quotas(2, 3);
    let b = b.with_quotas(2, 3);
    let t = Some(s.tenant.as_str());
    let ip = Some("198.51.100.9");
    let q1 = a.reserve(&token_key("q1"), ip, t).await.unwrap();
    b.reserve(&token_key("q2"), ip, t).await.unwrap();
    // Third unverified token from the same IP, on either instance.
    assert!(is_rejected(a.reserve(&token_key("q3"), ip, t).await));
    // Tenant quota counts across IPs and instances.
    b.reserve(&token_key("q4"), Some("198.51.100.10"), t)
        .await
        .unwrap();
    assert!(is_rejected(
        a.reserve(&token_key("q5"), Some("198.51.100.11"), t).await
    ));
    // Verified tokens leave the quota.
    a.settle(&q1, Outcome::Verified).await.unwrap();
    assert!(b.reserve(&token_key("q6"), ip, t).await.is_ok());
}

#[tokio::test]
async fn replay_store_fails_closed_when_unusable() {
    let s = require_pg!(setup().await);
    let store = PgReplayStore::new(replay_pool(&s).await, IP_SECRET);
    // The startup check `vibe run` runs: a tenant without the table.
    let err = store
        .check(&["nomigrate0001".to_string()])
        .await
        .unwrap_err();
    assert!(
        err.contains("restart `vibe run --db postgres` with VIBE_MIGRATE_DATABASE_URL set"),
        "{err}"
    );
    assert!(is_unavailable(
        store
            .reserve(&token_key("x"), None, Some("nomigrate0001"))
            .await
    ));
    // No tenant: no reservation.
    assert!(is_unavailable(
        store.reserve(&token_key("x"), None, None).await
    ));
}

#[tokio::test]
async fn sslmode_require_fails_against_server_without_tls() {
    let Some(admin) = admin_url() else {
        if std::env::var_os("CI").is_some() {
            panic!("VIBE_TEST_PG_ADMIN_URL is not set in CI");
        }
        eprintln!("SKIPPED: VIBE_TEST_PG_ADMIN_URL not set");
        return;
    };
    // The CI service container has TLS turned off.
    let mut url = url::Url::parse(&admin).unwrap();
    url.query_pairs_mut().append_pair("sslmode", "require");
    if let Ok(exec) = PgExec::connect(url.as_str()).await {
        let ssl = exec
            .query_texts(
                "SELECT ssl::text || $1 FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
                "",
            )
            .await;
        panic!("sslmode=require must not connect to a server without TLS (ssl: {ssl:?})");
    }
}

#[tokio::test]
async fn automatic_migration_tracking_planning_and_upgrade() {
    use vibe::run::apply_migrations;
    use vibe::runtime::migrate::migration_digest;
    let s = require_pg!(setup().await);
    let owner = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    let serving = PgExec::connect(&url_as(&s.admin_url, &s.app))
        .await
        .unwrap();
    let tenants = vec![format!("pa{}", suffix()), format!("pb{}", suffix())];
    let mut m = manifest();
    s.admin
        .batch_execute("DROP TABLE vibe_platform.applied_migrations")
        .await
        .unwrap();
    let missing = apply_migrations(None, &serving, &m, &tenants, &s.app, false)
        .await
        .unwrap_err();
    assert!(missing.contains("VIBE_MIGRATE_DATABASE_URL"), "{missing}");
    let first = apply_migrations(Some(&owner), &serving, &m, &tenants, &s.app, false)
        .await
        .unwrap();
    assert_eq!(
        first.iter().filter(|s| s.starts_with("applied ")).count(),
        2
    );
    let rows = owner
        .query_texts(
            "SELECT sha256 FROM vibe_platform.applied_migrations WHERE tenant_id = $1",
            &tenants[0],
        )
        .await
        .unwrap();
    assert_eq!(rows, vec![migration_digest(&m.migrations[0].statements)]);
    serving.check_runtime_role().await.unwrap();
    let privileges = owner.query_texts("SELECT has_table_privilege($1, 'vibe_platform.applied_migrations', 'SELECT')::text || '|' || has_table_privilege($1, 'vibe_platform.applied_migrations', 'INSERT,UPDATE,DELETE,TRUNCATE')::text || '|' || has_schema_privilege($1, 'vibe_platform', 'CREATE')::text", &s.app).await.unwrap();
    assert_eq!(privileges, vec!["true|false|false"]);
    let again = apply_migrations(Some(&owner), &serving, &m, &tenants, &s.app, false)
        .await
        .unwrap();
    assert!(!again.iter().any(|s| s.starts_with("applied ")));
    assert!(again.iter().any(|s| s.contains("up to date")));
    assert!(
        apply_migrations(None, &serving, &m, &tenants, &s.app, false)
            .await
            .is_ok()
    );
    let mut next = m.migrations[0].clone();
    next.id = "0003-add".into();
    next.statements = vec!["ALTER TABLE audit_notes ADD COLUMN x text".into()];
    m.migrations.push(next);
    let err = apply_migrations(None, &serving, &m, &tenants, &s.app, false)
        .await
        .unwrap_err();
    assert!(err.contains("VIBE_MIGRATE_DATABASE_URL"), "{err}");
    let appended = apply_migrations(Some(&owner), &serving, &m, &tenants, &s.app, false)
        .await
        .unwrap();
    assert_eq!(
        appended
            .iter()
            .filter(|s| s.starts_with("applied "))
            .count(),
        2
    );
    assert!(appended
        .iter()
        .filter(|s| s.starts_with("applied "))
        .all(|s| s == "applied 0003-add"));
    let mut safe = m.migrations[0].clone();
    safe.id = "00035-safe".into();
    safe.statements = vec!["ALTER TABLE audit_notes ADD COLUMN pending text".into()];
    m.migrations.push(safe);
    let mut drop = m.migrations[0].clone();
    drop.id = "0004-drop".into();
    drop.statements = vec!["ALTER TABLE audit_notes DROP COLUMN note".into()];
    m.migrations.push(drop);
    let err = apply_migrations(Some(&owner), &serving, &m, &tenants, &s.app, false)
        .await
        .unwrap_err();
    assert!(
        err.contains("0004-drop") && err.contains("--allow-destructive-migrations"),
        "{err}"
    );
    for tenant in &tenants {
        assert_eq!(owner.query_texts("SELECT count(*)::text FROM information_schema.columns WHERE table_schema = $1 AND table_name = 'audit_notes' AND column_name = 'pending'", &format!("t_{tenant}")).await.unwrap(), vec!["0"]);
        let count = owner.query_texts("SELECT count(*)::text FROM information_schema.columns WHERE table_schema = $1 AND table_name = 'audit_notes' AND column_name = 'note'", &format!("t_{tenant}")).await.unwrap();
        assert_eq!(count, vec!["1"]);
        assert_eq!(owner.query_texts("SELECT count(*)::text FROM vibe_platform.applied_migrations WHERE tenant_id = $1 AND migration_id = '0004-drop'", tenant).await.unwrap(), vec!["0"]);
    }
    apply_migrations(Some(&owner), &serving, &m, &tenants, &s.app, true)
        .await
        .unwrap();
    for tenant in &tenants {
        assert_eq!(owner.query_texts("SELECT destructive::text FROM vibe_platform.applied_migrations WHERE tenant_id = $1 AND migration_id = '0004-drop'", tenant).await.unwrap(), vec!["true"]);
    }
    m.migrations[0]
        .statements
        .push("COMMENT ON TABLE audit_notes IS 'changed'".into());
    for connection in [Some(&owner), None] {
        let err = apply_migrations(connection, &serving, &m, &tenants, &s.app, false)
            .await
            .unwrap_err();
        assert!(
            err.contains("different SQL") && err.contains("append-only"),
            "{err}"
        );
    }
    let mut removed = manifest();
    removed.migrations.clear();
    let warning = apply_migrations(Some(&owner), &serving, &removed, &tenants, &s.app, false)
        .await
        .unwrap();
    assert!(warning
        .iter()
        .any(|s| s.contains("no longer in the manifest")));
    // Simulate an old deployment that applied non-idempotent SQL but recorded nothing.
    let mut legacy = manifest();
    legacy.migrations[0]
        .statements
        .push("ALTER TABLE audit_notes ADD COLUMN x text".into());
    let tenant = format!("pc{}", suffix());
    migrate_tenant(&owner, &legacy, &tenant, &s.app)
        .await
        .unwrap();
    owner
        .batch(&[format!(
            "DELETE FROM vibe_platform.applied_migrations WHERE tenant_id = '{tenant}'"
        )])
        .await
        .unwrap();
    let err = apply_migrations(
        Some(&owner),
        &serving,
        &legacy,
        std::slice::from_ref(&tenant),
        &s.app,
        false,
    )
    .await
    .unwrap_err();
    let sha = migration_digest(&legacy.migrations[0].statements);
    let insert = format!("INSERT INTO vibe_platform.applied_migrations (tenant_id, migration_id, sha256, destructive) VALUES ('{tenant}', '{}', '{sha}', false);", legacy.migrations[0].id);
    assert!(err.contains(&insert), "{err}");
    owner.batch(&[insert]).await.unwrap();
    apply_migrations(Some(&owner), &serving, &legacy, &[tenant], &s.app, false)
        .await
        .unwrap();
}

/// Exercise the deployed binary using a minimal development artifact signed
/// with the same canonical serialization as the project build helper.
#[tokio::test]
async fn runtime_binary_automatically_migrates_and_refuses_destructive_sql() {
    use std::{
        fs,
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };
    struct Running(Child);
    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let s = require_pg!(setup().await);
    let tenant = format!("pd{}", suffix());
    let root = std::env::temp_dir().join(format!("vibe-pg-artifact-{}", suffix()));
    fs::create_dir_all(&root).unwrap();
    let key = vibe::sign::create_dev_key(&root).unwrap();
    fs::write(
        root.join(".vibe/slugs.json"),
        serde_json::to_vec(&json!({"test": {"tenant_id": tenant}})).unwrap(),
    )
    .unwrap();
    let mut m = manifest();
    m.build_mode = Some(vibe::manifest::BuildMode::Dev);
    let write = |manifest: &mut Manifest| {
        manifest.signature.clear();
        manifest.signature = vibe::sign::sign_bytes(&serde_json::to_vec(manifest).unwrap(), &key);
        fs::write(
            root.join("manifest.json"),
            serde_json::to_vec(manifest).unwrap(),
        )
        .unwrap();
    };
    write(&mut m);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let spawn = || {
        Running(
            Command::new(env!("CARGO_BIN_EXE_vibe-runtime"))
                .env_clear()
                .current_dir(&root)
                .env("VIBE_ENV", "development")
                .env("VIBE_DATABASE_URL", url_as(&s.admin_url, &s.app))
                .env("VIBE_MIGRATE_DATABASE_URL", url_as(&s.admin_url, &s.owner))
                .args([
                    "run",
                    "--artifact",
                    ".",
                    "--db",
                    "postgres",
                    "--addr",
                    &addr.to_string(),
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        )
    };
    let mut child = spawn();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if child.0.try_wait().unwrap().is_some() {
            use std::io::Read;
            let mut error = String::new();
            child
                .0
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut error)
                .unwrap();
            panic!("runtime exited before listening: {error}");
        }
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "runtime startup exceeded 60s");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let rows: i64 = s
        .admin
        .query_one(
            "SELECT count(*) FROM vibe_platform.applied_migrations WHERE tenant_id = $1",
            &[&tenant],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(rows, 1);
    drop(child);
    let mut destructive = m.migrations[0].clone();
    destructive.id = "0003-destructive".into();
    destructive.statements = vec!["ALTER TABLE audit_notes DROP COLUMN note".into()];
    m.migrations.push(destructive);
    write(&mut m);
    let mut child = spawn();
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "runtime refusal exceeded 60s");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    use std::io::Read;
    let mut error = String::new();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut error)
        .unwrap();
    assert!(!status.success());
    assert!(
        error.contains("refusing to apply destructive migration 0003-destructive")
            && error.contains("--allow-destructive-migrations"),
        "{error}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn concurrent_startups_apply_a_tenant_migration_once() {
    use vibe::run::apply_migrations;
    let s = require_pg!(setup().await);
    let owner_url = url_as(&s.admin_url, &s.owner);
    let first = PgExec::connect(&owner_url).await.unwrap();
    let second = PgExec::connect(&owner_url).await.unwrap();
    let serving = PgExec::connect(&url_as(&s.admin_url, &s.app))
        .await
        .unwrap();
    // Exercise tracking creation as well as two fresh-tenant transactions.
    s.admin
        .batch_execute("DROP SCHEMA vibe_platform CASCADE")
        .await
        .unwrap();
    let tenants = vec![format!("con{}", suffix())];
    let mut m = manifest();
    // No IF NOT EXISTS: applying this SQL twice must fail.
    m.migrations[0].statements = vec![
        "CREATE TABLE concurrent_notes (tenant_id text, note text)".into(),
        // Keep the transaction open so the other startup waits for this
        // tenant, then must read the newly committed tracking record.
        format!(
            "INSERT INTO concurrent_notes SELECT '{}', pg_sleep(0.1)::text",
            tenants[0]
        ),
    ];
    let (a, b) = tokio::join!(
        apply_migrations(Some(&first), &serving, &m, &tenants, &s.app, false),
        apply_migrations(Some(&second), &serving, &m, &tenants, &s.app, false),
    );
    let logs = [a.unwrap(), b.unwrap()];
    let expected = format!("applied {}", m.migrations[0].id);
    assert_eq!(
        logs.iter()
            .flatten()
            .filter(|line| **line == expected)
            .count(),
        1
    );
    assert_eq!(
        first
            .query_texts(
                "SELECT count(*)::text FROM vibe_platform.applied_migrations WHERE tenant_id = $1",
                &tenants[0],
            )
            .await
            .unwrap(),
        vec!["1"]
    );
}

#[tokio::test]
async fn memory_allowlist_forms_match_postgres_raw_tenant_roles() {
    let s = require_pg!(setup().await);
    let other = format!("tn{}", suffix());
    // Match tests/fixtures/auth-app/schema.sql, preserving pre-existing roles.
    let mut created_roles = Vec::new();
    for role in ["public_reader", "public_writer"] {
        if !s
            .admin
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)",
                &[&role],
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
        {
            created_roles.push(role);
        }
    }
    let admin_url = s.admin_url.clone();
    let owner = s.owner.clone();
    let app = s.app.clone();
    let tenant = s.tenant.clone();
    let other_tenant = other.clone();
    // A task captures assertion panics, letting cleanup finish while the
    // harness still holds the exclusive catalog lock.
    let result = tokio::spawn(async move {
        let admin = connect(&admin_url).await;
        for role in ["public_reader", "public_writer"] {
            admin.batch_execute(&format!(
                "DO $$ BEGIN CREATE ROLE {role} NOLOGIN; EXCEPTION WHEN duplicate_object THEN NULL; END $$; GRANT {role} TO {app}"
            )).await.unwrap();
        }
        let mut m = manifest();
        let mut counter = m.models[0].clone();
        counter.id = "counter".into();
        counter.table = "counters".into();
        counter.fields.retain(|f| f.name != "body");
        let mut name = field("name", "string");
        name["unique"] = json!(true);
        counter.fields.push(serde_json::from_value(name).unwrap());
        counter.fields.push(serde_json::from_value(field("value", "integer")).unwrap());
        m.models.push(counter);
        let mut migration = m.migrations[0].clone();
        migration.id = "migrations/main/0003-counters".into();
        migration.seq = "0003".into();
        // Model bootstrap supplies the platform RLS policy. Migrations supply
        // defaults and the global name constraint used by ON CONFLICT (name).
        migration.statements = vec![
            "ALTER TABLE counters ALTER COLUMN id SET DEFAULT gen_random_uuid(), ALTER COLUMN tenant_id SET DEFAULT current_setting('app.tenant_id', false), ALTER COLUMN created_at SET DEFAULT now(), ALTER COLUMN updated_at SET DEFAULT now(), ALTER COLUMN value SET DEFAULT 0, ADD UNIQUE (name)".into(),
        ];
        m.migrations.push(migration);
        let owner_exec = PgExec::connect(&url_as(&admin_url, &owner)).await.unwrap();
        for tenant in [&tenant, &other_tenant] {
            migrate_tenant(&owner_exec, &m, tenant, &app).await.unwrap();
            admin.batch_execute(&format!(
                "GRANT USAGE ON SCHEMA \"t_{tenant}\" TO public_reader, public_writer; \
                 GRANT SELECT ON \"t_{tenant}\".counters TO public_reader; \
                 GRANT SELECT, INSERT, UPDATE ON \"t_{tenant}\".counters TO public_writer"
            )).await.unwrap();
        }
        let pg = PgExec::connect(&url_as(&admin_url, &app))
            .await
            .unwrap();
        let read = "SELECT COALESCE(MAX(value), 0) AS value FROM counters WHERE name = $1";
        let rows = pg
            .raw(
                &tenant,
                "public_reader",
                false,
                read,
                vec![Some("counter".into())],
            )
            .await
            .unwrap();
        assert_eq!(rows[0]["value"], 0);
        for (sql,params,expected) in [
            ("INSERT INTO counters (name,value) VALUES ($1,1) ON CONFLICT (name) DO UPDATE SET value = counters.value + EXCLUDED.value RETURNING value",vec![Some("counter".into())],1),
            ("INSERT INTO counters (name,value) VALUES ($1,1) ON CONFLICT (name) DO UPDATE SET value = counters.value + EXCLUDED.value RETURNING value",vec![Some("counter".into())],2),
            ("UPDATE counters SET value = value + $2 WHERE name = $1 RETURNING value",vec![Some("counter".into()),Some("3".into())],5),
            ("UPDATE counters SET value = value + 2 WHERE name = $1 RETURNING *",vec![Some("counter".into())],7),
            ("INSERT INTO counters (name,value) VALUES ($1,1) ON CONFLICT (name) DO UPDATE SET value = counters.value + 4 RETURNING value",vec![Some("counter".into())],11),
            (read,vec![Some("counter".into())],11),
        ] {
            let writes=!sql.starts_with("SELECT");
            let rows=pg.raw(&tenant,if writes {"public_writer"} else {"public_reader"},writes,sql,params).await.unwrap();
            assert_eq!(rows[0]["value"],expected);
        }
        let rows=pg.raw(&tenant,"public_writer",true,"INSERT INTO counters (name,value) VALUES ($1,99) ON CONFLICT (name) DO NOTHING RETURNING value",vec![Some("counter".into())]).await.unwrap();
        assert!(rows.is_empty());
        assert!(pg
            .raw(
                &tenant,
                "public_reader",
                true,
                "UPDATE counters SET value = value + 1 WHERE name = $1 RETURNING value",
                vec![Some("counter".into())]
            )
            .await
            .is_err());
        assert_eq!(
            pg.raw(
                &other_tenant,
                "public_reader",
                false,
                read,
                vec![Some("counter".into())]
            )
            .await
            .unwrap()[0]["value"],
            0
        );
    }).await;
    // Drop both migrated tenants and setup's platform schema before removing
    // their owners. DROP OWNED also revokes database grants; DROP ROLE
    // removes memberships without deleting pre-existing shared query roles.
    s.admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS \"t_{}\" CASCADE; DROP SCHEMA IF EXISTS \"t_{other}\" CASCADE; \
         DROP SCHEMA IF EXISTS vibe_platform CASCADE; \
         DROP OWNED BY {}, {}; DROP ROLE {}, {}",
            s.tenant, s.app, s.owner, s.app, s.owner
        ))
        .await
        .unwrap();
    for role in created_roles {
        s.admin
            .batch_execute(&format!("DROP ROLE {role}"))
            .await
            .unwrap();
    }
    if let Err(error) = result {
        if error.is_panic() {
            std::panic::resume_unwind(error.into_panic());
        }
        panic!("counter task failed: {error}");
    }
}
