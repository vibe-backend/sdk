//! Real PostgreSQL tests. They run when `VIBE_TEST_PG_ADMIN_URL` points at a
//! superuser connection (CI sets it via a service container). Outside CI they
//! are skipped with a SKIPPED note; in CI (`CI` set) a missing URL fails.

use serde_json::{json, Value};
use tokio_postgres::{Client, NoTls};
use vibe::manifest::Manifest;
use vibe::runtime::challenge::ChallengeError;
use vibe::runtime::replay::{token_key, Outcome, PgReplayStore, ReplayStore};
use vibe::runtime::{migrate_tenant, Col, PgExec, QueryExec, Stmt, TenantPools};

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

/// Name of a tenant's role for a class (`tenant_<id>_rw`, ...).
fn trole(tenant: &str, class: &str) -> String {
    vibe::runtime::grants::tenant_role(tenant, class).unwrap()
}

/// The per-tenant migration role used by these tests.
fn owner_of(tenant: &str) -> String {
    vibe::runtime::grants::tenant_owner_role(tenant).unwrap()
}

/// Logs in as `tenant`'s role for `class` and runs the serve-time check.
async fn check_tenant(admin_url: &str, tenant: &str, class: &str) -> Result<(), String> {
    let exec = PgExec::connect(&url_as(admin_url, &trole(tenant, class)))
        .await
        .unwrap();
    exec.check_tenant_role(tenant, class).await
}

/// Grants of the test model: full access for `app_rw`, column-limited
/// public roles.
const GRANTS: &str =
    "app_rw [SELECT, INSERT, UPDATE], public_reader [SELECT(body)], public_writer [INSERT(body)]";

/// Role classes; each is a separate LOGIN role per tenant (decision 97).
const RUNTIME_ROLES: [&str; 3] = ["app_rw", "public_reader", "public_writer"];

/// What an operator pre-creates for a tenant: three LOGIN NOINHERIT tenant
/// roles, and a migration role that may create the tenant schema (and so
/// owns only that schema). The only membership is the owner in the
/// tenant's `_rw` role, granted `WITH ADMIN FALSE, INHERIT FALSE, SET TRUE`
/// (D129, #246; PostgreSQL 16+).
async fn ensure_tenant_roles(admin_url: &str, tenant: &str) {
    let _guard = SETUP.lock().await;
    let admin = connect(admin_url).await;
    let db = admin_url
        .rsplit('/')
        .next()
        .unwrap()
        .split('?')
        .next()
        .unwrap()
        .to_string();
    let owner = owner_of(tenant);
    let mut sql = format!(
        "DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{owner}') \
         THEN CREATE ROLE {owner} LOGIN PASSWORD 'pw'; END IF; END $$; \
         GRANT CREATE ON DATABASE {db} TO {owner};"
    );
    for class in RUNTIME_ROLES {
        let role = trole(tenant, class);
        sql.push_str(&format!(
            "DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{role}') \
             THEN CREATE ROLE {role} LOGIN NOINHERIT PASSWORD 'pw'; END IF; END $$;"
        ));
    }
    sql.push_str(&vibe::runtime::migrate::owner_membership_grant(tenant).unwrap());
    sql.push(';');
    admin.batch_execute(&sql).await.unwrap();
}

/// Creates the tenant's roles if needed and migrates as its own migration
/// role.
async fn migrate_as(s: &Setup, m: &Manifest, tenant: &str) -> Result<Vec<String>, String> {
    migrate_fresh(&s.admin_url, m, tenant).await
}

async fn migrate_fresh(admin_url: &str, m: &Manifest, tenant: &str) -> Result<Vec<String>, String> {
    ensure_tenant_roles(admin_url, tenant).await;
    let exec = PgExec::connect(&url_as(admin_url, &owner_of(tenant)))
        .await
        .unwrap();
    migrate_tenant(&exec, m, tenant).await
}

/// One pool per (tenant, class) for `tenants`, as `vibe serve` uses.
fn pools_for(admin_url: &str, tenants: &[&str]) -> TenantPools {
    let mut secrets = std::collections::HashMap::new();
    for t in tenants {
        for class in RUNTIME_ROLES {
            secrets.insert(trole(t, class), "pw".to_string());
        }
    }
    TenantPools::new(admin_url, secrets, vibe::runtime::PgOptions::default()).unwrap()
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
            "policy": vibe::runtime::POLICY_TEMPLATE,
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
    _owner_guard: Option<tokio::sync::RwLockReadGuard<'static, ()>>,
    admin_url: String,
    admin: Client,
    owner: String,
    app: String,
    tenant: String,
}

// Normal fixtures may use owner logins concurrently. The cluster-wide sweep
// needs exclusive access because it disables every unlocked owner, in all DBs.
static OWNER_LOGINS: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

/// Catalog-wide DDL (GRANT ON DATABASE, CREATE ROLE) races between parallel
/// tests ("tuple concurrently updated"), so role setup runs one at a time.
static SETUP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Each fixture gets a fresh database, so intentionally broken tenant schemas
/// cannot affect another test's database-wide startup checks. Databases and
/// uniquely named roles are left in the disposable test cluster for inspection.
async fn setup() -> Option<Setup> {
    setup_with_owner_guard(Some(OWNER_LOGINS.read().await)).await
}

async fn setup_with_owner_guard(
    owner_guard: Option<tokio::sync::RwLockReadGuard<'static, ()>>,
) -> Option<Setup> {
    let root_url = admin_url()?;
    let root = connect(&root_url).await;
    let database = format!("vibe_pg_{}", uuid::Uuid::new_v4().simple());
    root.batch_execute(&format!("CREATE DATABASE {database} TEMPLATE template0"))
        .await
        .unwrap();
    let mut url = url::Url::parse(&root_url).unwrap();
    url.set_path(&format!("/{database}"));
    let admin_url = url.to_string();
    let admin = connect(&admin_url).await;
    let tenant = format!("tn{}", suffix());
    migrate_fresh(&admin_url, &manifest(), &tenant)
        .await
        .unwrap();
    Some(Setup {
        _owner_guard: owner_guard,
        owner: owner_of(&tenant),
        app: trole(&tenant, "app_rw"),
        admin_url,
        admin,
        tenant,
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
async fn tenant_roles_are_accepted_and_limited_to_their_own_schema() {
    let s = require_pg!(setup().await);
    for class in RUNTIME_ROLES {
        check_tenant(&s.admin_url, &s.tenant, class).await.unwrap();
    }
    let schema = format!("t_{}", s.tenant);
    let notes = format!("{schema}.notes");
    let rw = trole(&s.tenant, "app_rw");
    let reader = trole(&s.tenant, "public_reader");
    let writer = trole(&s.tenant, "public_writer");
    // Each tenant role has exactly the model grants on its own tables.
    let row = s
        .admin
        .query_one(
            "SELECT has_table_privilege($1, $4, 'SELECT'), has_table_privilege($1, $4, 'INSERT'), \
             has_table_privilege($1, $4, 'UPDATE'), has_table_privilege($1, $4, 'DELETE'), \
             has_table_privilege($1, $4, 'TRUNCATE'), \
             has_column_privilege($2, $4, 'body', 'SELECT'), \
             has_column_privilege($2, $4, 'tenant_id', 'SELECT'), \
             has_column_privilege($3, $4, 'body', 'SELECT'), \
             has_schema_privilege($1, $5, 'CREATE'), \
             has_table_privilege($1, $6, 'SELECT')",
            &[
                &rw,
                &reader,
                &writer,
                &notes,
                &schema,
                &format!("{schema}.audit_notes"),
            ],
        )
        .await
        .unwrap();
    let flags: Vec<bool> = (0..10).map(|i| row.get(i)).collect();
    assert_eq!(
        flags,
        vec![true, true, false, false, false, true, false, false, false, false],
        "table-wide UPDATE is narrowed to columns (never id/tenant_id/created_at); no CREATE; \
         migration-only tables get no grants"
    );
    // No shared runtime role, no membership in either direction except
    // the owner in the tenant's own `_rw` role (D129, #246).
    let shared: i64 = s
        .admin
        .query_one(
            "SELECT count(*) FROM pg_auth_members m JOIN pg_roles r ON r.oid IN (m.member, m.roleid) \
             WHERE r.rolname = ANY($1) AND NOT (m.roleid = $2::text::regrole AND m.member = $3::text::regrole \
             AND NOT m.admin_option AND NOT m.inherit_option AND m.set_option)",
            &[&vec![rw.clone(), reader.clone(), writer.clone()], &rw, &owner_of(&s.tenant)],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(shared, 0);
}

/// Tenant B's own login, in a separate tenant from `s.tenant` (tenant A).
async fn second_tenant(s: &Setup) -> String {
    let b = format!("tb{}", suffix());
    migrate_as(s, &manifest(), &b).await.unwrap();
    let schema = format!("t_{b}");
    s.admin
        .batch_execute(&format!(
            "INSERT INTO {schema}.notes (id, tenant_id, body, created_at, updated_at) \
             VALUES (gen_random_uuid(), '{b}', 'b-secret', now(), now()); \
             INSERT INTO {schema}.audit_notes VALUES ('{b}', 'b-secret')"
        ))
        .await
        .unwrap();
    b
}

/// Issue #98: a tenant login cannot reach another tenant, whatever it tries
/// inside its own connection: direct access, `SET ROLE` to the other
/// tenant's roles or to the old shared roles, `SET SESSION AUTHORIZATION`,
/// or changing `app.tenant_id`.
#[tokio::test]
async fn cross_tenant_access_fails_even_through_set_role() {
    let s = require_pg!(setup().await);
    let b = second_tenant(&s).await;
    let b_schema = format!("t_{b}");
    // Old #99-style shared roles exist on this cluster too; they must not
    // be reachable.
    s.admin
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'app_rw') \
             THEN CREATE ROLE app_rw NOLOGIN; END IF; END $$",
        )
        .await
        .unwrap();
    for class in RUNTIME_ROLES {
        let a = connect(&url_as(&s.admin_url, &trole(&s.tenant, class))).await;
        let attempts = [
            format!("SELECT count(*) FROM {b_schema}.notes"),
            format!("SELECT count(*) FROM {b_schema}.audit_notes"),
            format!("UPDATE {b_schema}.notes SET body = 'pwned'"),
            format!(
                "INSERT INTO {b_schema}.notes (id, tenant_id, body, created_at, updated_at) \
                 VALUES (gen_random_uuid(), '{b}', 'x', now(), now())"
            ),
            format!("SET ROLE {}", trole(&b, "app_rw")),
            format!("SET ROLE {}", trole(&b, class)),
            "SET ROLE app_rw".to_string(),
            format!("SET ROLE {}", owner_of(&b)),
            format!("SET ROLE {}", s.owner),
            "SET ROLE postgres".to_string(),
            format!("SET SESSION AUTHORIZATION {}", trole(&b, "app_rw")),
            format!(
                "BEGIN; SET LOCAL ROLE {}; SELECT count(*) FROM {b_schema}.notes",
                trole(&b, "app_rw")
            ),
            format!(
                "BEGIN; SELECT set_config('app.tenant_id', '{b}', true); \
                 SELECT set_config('search_path', '{b_schema}', true); SELECT count(*) FROM notes"
            ),
        ];
        for attempt in &attempts {
            let got = a.batch_execute(attempt).await;
            let _ = a.batch_execute("ROLLBACK").await;
            let err = got.expect_err(&format!("{class}: `{attempt}` must fail"));
            let code = err.code().map(|c| c.code().to_string()).unwrap_or_default();
            assert!(
                code == "42501" || code == "22023" || code == "42P01",
                "{class}: `{attempt}`: unexpected error {err:?}"
            );
        }
        // The session is still the tenant role; nothing changed identity.
        let who: String = a
            .query_one(
                "SELECT session_user::text || '|' || current_user::text",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        let me = trole(&s.tenant, class);
        assert_eq!(who, format!("{me}|{me}"));
    }
    // B's data is untouched.
    let body: String = s
        .admin
        .query_one(
            &format!("SELECT string_agg(body, ',') FROM {b_schema}.notes"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(body, "b-secret");
    // The executor refuses to run tenant B's statements on tenant A's pool,
    // and the per-tenant pools keep each tenant in its own schema.
    let a_pool = PgExec::connect(&url_as(&s.admin_url, &s.app))
        .await
        .unwrap();
    let err = a_pool
        .execute(&b, "app_rw", p_select(&["body"], None, None))
        .await
        .unwrap_err();
    assert!(format!("{err:?}").contains("not tenant role"), "{err:?}");
    let pools = pools_for(&s.admin_url, &[&s.tenant, &b]);
    assert_eq!(pools.check_all(&manifest().models).await.unwrap(), 6);
    let seen = pools
        .execute(&b, "app_rw", p_select(&["body"], None, None))
        .await
        .unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["body"], json!("b-secret"));
    let seen = pools
        .execute(&s.tenant, "app_rw", p_select(&["body"], None, None))
        .await
        .unwrap();
    assert!(
        seen.iter().all(|r| r["body"] != json!("b-secret")),
        "{seen:?}"
    );
}

/// The startup check refuses a tenant role that can reach anything outside
/// its own tenant schema.
#[tokio::test]
async fn tenant_role_with_cross_tenant_reach_is_refused() {
    let s = require_pg!(setup().await);
    let b = second_tenant(&s).await;
    let b_schema = format!("t_{b}");
    let rw = trole(&s.tenant, "app_rw");
    for (grant, revoke) in [
        (
            format!("GRANT USAGE ON SCHEMA {b_schema} TO {rw}"),
            format!("REVOKE USAGE ON SCHEMA {b_schema} FROM {rw}"),
        ),
        (
            format!("GRANT SELECT (body) ON {b_schema}.notes TO {rw}"),
            format!("REVOKE SELECT (body) ON {b_schema}.notes FROM {rw}"),
        ),
        (
            format!("GRANT {} TO {rw}", trole(&b, "app_rw")),
            format!("REVOKE {} FROM {rw}", trole(&b, "app_rw")),
        ),
        (
            format!("GRANT {rw} TO {}", trole(&b, "public_reader")),
            format!("REVOKE {rw} FROM {}", trole(&b, "public_reader")),
        ),
        (
            format!("CREATE TABLE public.shared_{b} (x int); GRANT SELECT ON public.shared_{b} TO PUBLIC"),
            format!("DROP TABLE public.shared_{b}"),
        ),
        (format!("ALTER ROLE {rw} INHERIT"), format!("ALTER ROLE {rw} NOINHERIT")),
        (format!("ALTER ROLE {rw} BYPASSRLS"), format!("ALTER ROLE {rw} NOBYPASSRLS")),
    ] {
        s.admin.batch_execute(&grant).await.unwrap();
        let got = check_tenant(&s.admin_url, &s.tenant, "app_rw").await;
        s.admin.batch_execute(&revoke).await.unwrap();
        let err = got.expect_err(&format!("`{grant}` must be refused"));
        assert!(err.contains("refusing to serve"), "{grant}: {err}");
    }
    check_tenant(&s.admin_url, &s.tenant, "app_rw")
        .await
        .unwrap();
    // A pool for one class may not be logged in as another class's role.
    let exec = PgExec::connect(&url_as(&s.admin_url, &trole(&s.tenant, "public_reader")))
        .await
        .unwrap();
    let err = exec
        .check_tenant_role(&s.tenant, "app_rw")
        .await
        .unwrap_err();
    assert!(err.contains("not the tenant role"), "{err}");
}

/// Issue #246: the only role membership allowed for a tenant is its owner
/// as a direct member of its own `_rw` role, granted `WITH ADMIN FALSE,
/// INHERIT FALSE, SET TRUE`. Any other membership fails the startup check.
#[tokio::test]
async fn startup_verification_requires_membership() {
    let s = require_pg!(setup().await);
    let b = second_tenant(&s).await;
    let rw = trole(&s.tenant, "app_rw");
    let owner = owner_of(&s.tenant);
    let rw_b = trole(&b, "app_rw");
    let mid = format!("mid_{}", suffix());
    // Allowed case: provisioning issued exactly the owner grant.
    let row = s
        .admin
        .query_one(
            "SELECT m.admin_option, m.inherit_option, m.set_option FROM pg_auth_members m \
             WHERE m.roleid = $1::text::regrole AND m.member = $2::text::regrole",
            &[&rw, &owner],
        )
        .await
        .unwrap();
    let opts: (bool, bool, bool) = (row.get(0), row.get(1), row.get(2));
    assert_eq!(opts, (false, false, true));
    for class in RUNTIME_ROLES {
        check_tenant(&s.admin_url, &s.tenant, class).await.unwrap();
    }
    for (name, change, repair) in [
        (
            "missing membership",
            format!("REVOKE {rw} FROM {owner}"),
            vibe::runtime::migrate::owner_membership_grant(&s.tenant).unwrap(),
        ),
        (
            "owner in another tenant's rw",
            format!("GRANT {rw_b} TO {owner} WITH ADMIN FALSE, INHERIT FALSE, SET TRUE"),
            format!("REVOKE {rw_b} FROM {owner}"),
        ),
        (
            "INHERIT TRUE",
            format!("GRANT {rw} TO {owner} WITH INHERIT TRUE"),
            format!("REVOKE INHERIT OPTION FOR {rw} FROM {owner}"),
        ),
        (
            "ADMIN OPTION",
            format!("GRANT {rw} TO {owner} WITH ADMIN TRUE"),
            format!("REVOKE ADMIN OPTION FOR {rw} FROM {owner}"),
        ),
        (
            "SET FALSE",
            format!("REVOKE SET OPTION FOR {rw} FROM {owner}"),
            format!("GRANT {rw} TO {owner} WITH SET TRUE"),
        ),
        (
            "reverse membership",
            // PostgreSQL refuses a circular grant, so drop the allowed one first.
            format!("REVOKE {rw} FROM {owner}; GRANT {owner} TO {rw} WITH INHERIT FALSE"),
            format!(
                "REVOKE {owner} FROM {rw}; \
                 GRANT {rw} TO {owner} WITH ADMIN FALSE, INHERIT FALSE, SET TRUE"
            ),
        ),
        (
            "indirect membership via an intermediate role",
            format!(
                "CREATE ROLE {mid} NOLOGIN; GRANT {mid} TO {owner} WITH INHERIT FALSE; \
                 GRANT {rw} TO {mid} WITH INHERIT FALSE"
            ),
            format!("DROP ROLE {mid}"),
        ),
    ] {
        s.admin.batch_execute(&change).await.unwrap();
        let got = check_tenant(&s.admin_url, &s.tenant, "app_rw").await;
        // Straight to migrate: re-provisioning would reset the grant options.
        let owner_exec = PgExec::connect(&url_as(&s.admin_url, &owner))
            .await
            .unwrap();
        let migrated = migrate_tenant(&owner_exec, &manifest(), &s.tenant).await;
        s.admin.batch_execute(&repair).await.unwrap();
        let err = got.expect_err(&format!("{name}: startup must fail"));
        assert!(err.contains("refusing to serve"), "{name}: {err}");
        assert!(migrated.is_err(), "{name}: migrate must refuse");
    }
    // Owner of tenant A in tenant B's rw also fails tenant B's startup.
    s.admin
        .batch_execute(&format!(
            "GRANT {rw_b} TO {owner} WITH ADMIN FALSE, INHERIT FALSE, SET TRUE"
        ))
        .await
        .unwrap();
    let got = check_tenant(&s.admin_url, &b, "app_rw").await;
    s.admin
        .batch_execute(&format!("REVOKE {rw_b} FROM {owner}"))
        .await
        .unwrap();
    assert!(got.unwrap_err().contains("refusing to serve"));
    for class in RUNTIME_ROLES {
        check_tenant(&s.admin_url, &s.tenant, class).await.unwrap();
    }
    migrate_as(&s, &manifest(), &s.tenant).await.unwrap();
}

/// sign-jwt reads `users.roles` through the tenant's `_rw` login pool. That
/// login is the tenant role itself (decision 97), so no `SET ROLE` is needed
/// and the default model grants let it read the column.
#[tokio::test]
async fn user_roles_lookup_runs_as_the_tenant_login() {
    let s = require_pg!(setup().await);
    let mut m: Value = serde_json::to_value(manifest()).unwrap();
    let mut users = m["models"][0].clone();
    users["id"] = json!("user");
    users["table"] = json!("users");
    users["grants"] = json!("app_rw [SELECT, INSERT, UPDATE], public_reader [], public_writer []");
    users["fields"] = json!([
        field("id", "uuid"),
        field("tenant_id", "string"),
        field("email", "string"),
        field("roles", "string[]"),
        field("created_at", "timestamp"),
        field("updated_at", "timestamp"),
        field("deleted_at", "timestamp")
    ]);
    m["models"].as_array_mut().unwrap().push(users);
    let m: Manifest = serde_json::from_value(m).unwrap();
    migrate_as(&s, &m, &s.tenant).await.unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    s.admin
        .batch_execute(&format!(
            "INSERT INTO t_{t}.users (id, tenant_id, email, roles, created_at, updated_at) \
             VALUES ('{id}', '{t}', 'a@example.com', ARRAY['admin','editor'], now(), now())",
            t = s.tenant
        ))
        .await
        .unwrap();
    let pools = pools_for(&s.admin_url, &[&s.tenant]);
    let got = pools.user_roles(&s.tenant, &id).await.unwrap();
    assert_eq!(got, Some(vec!["admin".to_string(), "editor".to_string()]));
    let missing = uuid::Uuid::new_v4().to_string();
    assert_eq!(pools.user_roles(&s.tenant, &missing).await.unwrap(), None);
    // The lookup ran as the tenant login, not as any other role.
    let exec = pools.pool(&s.tenant, "app_rw").await.unwrap();
    exec.check_tenant_role(&s.tenant, "app_rw").await.unwrap();
}

/// Concurrent first requests for one role open and check its pool once and
/// all get the same pool.
#[tokio::test]
async fn concurrent_first_use_opens_one_pool() {
    let s = require_pg!(setup().await);
    let pools = std::sync::Arc::new(pools_for(&s.admin_url, &[&s.tenant]));
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let pools = pools.clone();
        let tenant = s.tenant.clone();
        tasks.push(tokio::spawn(async move {
            pools.pool(&tenant, "app_rw").await.unwrap()
        }));
    }
    let mut got = Vec::new();
    for t in tasks {
        got.push(t.await.unwrap());
    }
    assert!(got.iter().all(|p| std::sync::Arc::ptr_eq(p, &got[0])));
    assert_eq!(pools.opens_started(), 1);
    assert_eq!(pools.open_pools().await, 1);
}

/// Issue #250: `tenant_<id>_rw` holds exactly SELECT and INSERT on the
/// challenge replay table; any other privilege fails the startup check.
#[tokio::test]
async fn replay_table_privileges_are_exactly_select_insert_delete() {
    let s = require_pg!(setup().await);
    let rw = trole(&s.tenant, "app_rw");
    let reader = trole(&s.tenant, "public_reader");
    let t = format!("t_{}.vibe_challenge_replay", s.tenant);
    check_tenant(&s.admin_url, &s.tenant, "app_rw")
        .await
        .unwrap();
    check_tenant(&s.admin_url, &s.tenant, "public_reader")
        .await
        .unwrap();
    let cases = [
        (
            "app_rw",
            format!("GRANT UPDATE ON {t} TO {rw}"),
            format!("REVOKE UPDATE ON {t} FROM {rw}"),
        ),
        (
            "app_rw",
            format!("GRANT TRUNCATE ON {t} TO {rw}"),
            format!("REVOKE TRUNCATE ON {t} FROM {rw}"),
        ),
        (
            "app_rw",
            format!("GRANT REFERENCES ON {t} TO {rw}"),
            format!("REVOKE REFERENCES ON {t} FROM {rw}"),
        ),
        (
            "app_rw",
            format!("GRANT TRIGGER ON {t} TO {rw}"),
            format!("REVOKE TRIGGER ON {t} FROM {rw}"),
        ),
        (
            "app_rw",
            format!("GRANT REFERENCES ON {t} TO {rw}"),
            format!("REVOKE REFERENCES ON {t} FROM {rw}"),
        ),
        (
            "app_rw",
            format!("GRANT UPDATE (state) ON {t} TO {rw}"),
            format!("REVOKE UPDATE (state) ON {t} FROM {rw}"),
        ),
        (
            "app_rw",
            format!("GRANT SELECT ON {t} TO {rw} WITH GRANT OPTION"),
            format!("REVOKE GRANT OPTION FOR SELECT ON {t} FROM {rw}"),
        ),
        (
            "app_rw",
            format!("GRANT SELECT ON {t} TO PUBLIC"),
            format!("REVOKE SELECT ON {t} FROM PUBLIC"),
        ),
        (
            "app_rw",
            format!("REVOKE INSERT ON {t} FROM {rw}"),
            format!("GRANT INSERT ON {t} TO {rw}"),
        ),
        (
            "public_reader",
            format!("GRANT SELECT ON {t} TO {reader}"),
            format!("REVOKE SELECT ON {t} FROM {reader}"),
        ),
    ];
    for (class, change, repair) in cases {
        s.admin.batch_execute(&change).await.unwrap();
        let got = check_tenant(&s.admin_url, &s.tenant, class).await;
        s.admin.batch_execute(&repair).await.unwrap();
        let err = got.expect_err(&format!("`{change}` must fail startup"));
        assert!(err.contains("refusing to serve"), "{change}: {err}");
    }
    check_tenant(&s.admin_url, &s.tenant, "app_rw")
        .await
        .unwrap();
    check_tenant(&s.admin_url, &s.tenant, "public_reader")
        .await
        .unwrap();
}

/// `vibe migrate` refuses a shared migration login and unsafe tenant roles.
#[tokio::test]
async fn migrate_requires_a_per_tenant_owner_and_safe_tenant_roles() {
    let s = require_pg!(setup().await);
    // s.owner owns s.tenant's schema; it may not migrate another tenant.
    let shared = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    let other = format!("tw{}", suffix());
    ensure_tenant_roles(&s.admin_url, &other).await;
    let err = migrate_tenant(&shared, &manifest(), &other)
        .await
        .unwrap_err();
    assert!(
        err.contains(&format!("must log in as {}", owner_of(&other))),
        "{err}"
    );
    // A login with the right name but reach into another tenant is refused
    // too (here: the other tenant's owner made a member of this one).
    s.admin
        .batch_execute(&format!("GRANT {} TO {}", s.owner, owner_of(&other)))
        .await
        .unwrap();
    let as_other = PgExec::connect(&url_as(&s.admin_url, &owner_of(&other)))
        .await
        .unwrap();
    let err = migrate_tenant(&as_other, &manifest(), &other)
        .await
        .unwrap_err();
    s.admin
        .batch_execute(&format!("REVOKE {} FROM {}", s.owner, owner_of(&other)))
        .await
        .unwrap();
    assert!(
        err.contains("can own schema") && err.contains(&s.tenant),
        "{err}"
    );
    // Missing or unsafe tenant roles.
    let fresh = format!("tm{}", suffix());
    ensure_tenant_roles(&s.admin_url, &fresh).await;
    let owner = PgExec::connect(&url_as(&s.admin_url, &owner_of(&fresh)))
        .await
        .unwrap();
    let reader = trole(&fresh, "public_reader");
    for (break_it, fix_it, expect) in [
        (
            format!("DROP ROLE {reader}"),
            format!("CREATE ROLE {reader} LOGIN NOINHERIT PASSWORD 'pw'"),
            "does not exist",
        ),
        (
            format!("ALTER ROLE {reader} INHERIT"),
            format!("ALTER ROLE {reader} NOINHERIT"),
            "NOINHERIT",
        ),
        (
            format!("ALTER ROLE {reader} NOLOGIN"),
            format!("ALTER ROLE {reader} LOGIN"),
            "LOGIN",
        ),
        (
            format!("GRANT pg_read_all_data TO {reader}"),
            format!("REVOKE pg_read_all_data FROM {reader}"),
            "member",
        ),
        (
            format!(
                "CREATE TABLE public.owned_{fresh} (x int); \
                 ALTER TABLE public.owned_{fresh} OWNER TO {reader}"
            ),
            format!("DROP TABLE public.owned_{fresh}"),
            "must not be owners",
        ),
    ] {
        s.admin.batch_execute(&break_it).await.unwrap();
        let got = migrate_tenant(&owner, &manifest(), &fresh).await;
        s.admin.batch_execute(&fix_it).await.unwrap();
        let err = got.expect_err(&break_it);
        assert!(
            err.contains(expect) && err.contains("tenant roles"),
            "{break_it}: {err}"
        );
    }
    let log = migrate_tenant(&owner, &manifest(), &fresh).await.unwrap();
    assert!(log.iter().any(|l| l.starts_with("verified")), "{log:?}");
    // The schema belongs to tenant_<id>_owner, runtime roles own nothing,
    // and every table has ENABLE + FORCE row level security.
    let row = s
        .admin
        .query_one(
            "SELECT n.nspowner::regrole::text, \
             (SELECT count(*) FROM pg_class c WHERE c.relnamespace = n.oid AND c.relkind = 'r' \
               AND NOT (c.relrowsecurity AND c.relforcerowsecurity)), \
             (SELECT count(*) FROM pg_class c WHERE c.relnamespace = n.oid AND c.relkind = 'r'), \
             (SELECT count(*) FROM pg_class c JOIN pg_roles r ON r.oid = c.relowner \
               WHERE r.rolname = ANY($2)) \
             FROM pg_namespace n WHERE n.nspname = $1",
            &[
                &format!("t_{fresh}"),
                &RUNTIME_ROLES
                    .iter()
                    .map(|c| trole(&fresh, c))
                    .collect::<Vec<_>>(),
            ],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), owner_of(&fresh));
    assert_eq!(
        row.get::<_, i64>(1),
        0,
        "every table is FORCE ROW LEVEL SECURITY"
    );
    assert!(row.get::<_, i64>(2) >= 2);
    assert_eq!(row.get::<_, i64>(3), 0, "runtime roles own nothing");
}

/// D129: the policy text the compiler writes into the lock is exactly what
/// is installed (`pg_policies`), and `vibe serve` refuses to start when they
/// differ.
#[tokio::test]
async fn installed_policies_match_the_compiled_lock_or_serve_refuses() {
    let s = require_pg!(setup().await);
    let m = manifest();
    let lock_line = &m.models[0].policy;
    assert_eq!(lock_line, vibe::runtime::POLICY_TEMPLATE);
    assert!(!lock_line.contains("current_setting"));
    // Lock line rendered for this tenant == the installed policy.
    let want =
        vibe::runtime::policy_parts(&vibe::runtime::render_policy(&s.tenant).unwrap()).unwrap();
    let row = s
        .admin
        .query_one(
            "SELECT policyname::text, permissive, cmd, roles::text[], qual, with_check FROM pg_policies \
             WHERE schemaname = $1 AND tablename = 'notes'",
            &[&format!("t_{}", s.tenant)],
        )
        .await
        .unwrap();
    let mut roles: Vec<String> = row.get(3);
    roles.sort();
    assert_eq!(row.get::<_, String>(0), want.name);
    assert_eq!(row.get::<_, String>(1), "PERMISSIVE");
    assert_eq!(row.get::<_, String>(2), "ALL");
    assert_eq!(roles, want.roles);
    assert_eq!(row.get::<_, String>(4), format!("({}::text)", want.using));
    assert_eq!(
        row.get::<_, String>(5),
        format!("({}::text)", want.with_check)
    );
    // Startup check passes.
    pools_for(&s.admin_url, &[&s.tenant])
        .check_all(&m.models)
        .await
        .unwrap();
    // A lock whose policy differs from what is installed: refused.
    let mut stale = m.clone();
    stale.models[0].policy = "tenant_id = current_setting('app.tenant_id', false)".into();
    let err = pools_for(&s.admin_url, &[&s.tenant])
        .check_all(&stale.models)
        .await
        .unwrap_err();
    assert!(err.contains("differ") && err.contains("recompile"), "{err}");
    // An installed policy that differs from the lock: refused. Only the
    // public reader is dropped from the roles, so the generic catalog check
    // is the only other thing that could notice.
    let schema = format!("t_{}", s.tenant);
    let rw = trole(&s.tenant, "app_rw");
    for altered in [
        format!(
            "TO {rw}, {w} USING (tenant_id = '{t}') WITH CHECK (tenant_id = '{t}')",
            w = trole(&s.tenant, "public_writer"),
            t = s.tenant
        ),
        format!(
            "AS PERMISSIVE FOR SELECT TO {rw}, {r}, {w} USING (tenant_id = '{t}')",
            r = trole(&s.tenant, "public_reader"),
            w = trole(&s.tenant, "public_writer"),
            t = s.tenant
        ),
    ] {
        s.admin
            .batch_execute(&format!(
                "DROP POLICY tenant_isolation ON {schema}.notes; \
                 CREATE POLICY tenant_isolation ON {schema}.notes {altered}"
            ))
            .await
            .unwrap();
        let err = s_check(&s, &m).await.unwrap_err();
        assert!(err.contains("refusing to serve"), "{altered}: {err}");
    }
    // Missing policy: refused.
    s.admin
        .batch_execute(&format!("DROP POLICY tenant_isolation ON {schema}.notes"))
        .await
        .unwrap();
    let err = PgExec::connect(&url_as(&s.admin_url, &s.app))
        .await
        .unwrap()
        .check_policies(&s.tenant, &m.models)
        .await
        .unwrap_err();
    assert!(err.contains("not installed"), "{err}");
    // Re-running migrate restores it.
    migrate_as(&s, &m, &s.tenant).await.unwrap();
    s_check(&s, &m).await.unwrap();
}

async fn s_check(s: &Setup, m: &Manifest) -> Result<usize, String> {
    pools_for(&s.admin_url, &[&s.tenant])
        .check_all(&m.models)
        .await
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
    // The policy compares with this tenant's id as a constant: a row with
    // someone else's tenant_id fails WITH CHECK whatever app.tenant_id says.
    let admin = &s.admin;
    let schema = format!("t_{}", s.tenant);
    let app = connect(&url_as(&s.admin_url, &s.app)).await;
    let forged = app
        .batch_execute(&format!(
            "BEGIN; SELECT set_config('app.tenant_id', 'othertenant1', true); \
             INSERT INTO {schema}.notes (id, tenant_id, created_at, updated_at) \
             VALUES (gen_random_uuid(), 'othertenant1', now(), now()); COMMIT;"
        ))
        .await;
    let _ = app.batch_execute("ROLLBACK").await;
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
    let group = format!("grp_{}", suffix());
    s.admin
        .batch_execute(&format!("CREATE ROLE {group} NOLOGIN BYPASSRLS"))
        .await
        .unwrap();
    let cases = [
        ("su", "SUPERUSER".to_string(), "superuser"),
        ("bypass", "BYPASSRLS".to_string(), "BYPASSRLS"),
        (
            "member-of-superuser",
            "IN ROLE postgres".to_string(),
            "member of postgres",
        ),
        (
            "noinherit-member-of-owner",
            format!("IN ROLE {}", s.owner),
            "member of",
        ),
        (
            "member-of-bypassrls-group",
            format!("IN ROLE {group}"),
            &group,
        ),
    ];
    for (label, extra, expect) in &cases {
        // A tenant role for a fresh tenant, created with the bad attribute.
        let tenant = format!("tr{}", suffix());
        create_login(
            &s.admin,
            &trole(&tenant, "app_rw"),
            &format!("NOINHERIT {extra}"),
        )
        .await;
        let err = check_tenant(&s.admin_url, &tenant, "app_rw")
            .await
            .expect_err(&format!("{label} must be rejected"));
        assert!(
            err.contains("refusing to serve") && err.contains(*expect),
            "{label}: {err}"
        );
    }
    // A role that owns objects.
    let tenant = format!("tr{}", suffix());
    let rw = trole(&tenant, "app_rw");
    create_login(&s.admin, &rw, "NOINHERIT").await;
    s.admin
        .batch_execute(&format!("CREATE TABLE public.owned_{tenant} (x int); ALTER TABLE public.owned_{tenant} OWNER TO {rw}"))
        .await
        .unwrap();
    let err = check_tenant(&s.admin_url, &tenant, "app_rw")
        .await
        .unwrap_err();
    s.admin
        .batch_execute(&format!("DROP TABLE public.owned_{tenant}"))
        .await
        .unwrap();
    assert!(err.contains("owns"), "{err}");
    // Any other login (for example a shared runtime login) is refused.
    let err = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap()
        .check_tenant_role(&s.tenant, "app_rw")
        .await
        .unwrap_err();
    assert!(err.contains("not the tenant role"), "{err}");
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
    check_tenant(&s.admin_url, &s.tenant, "app_rw")
        .await
        .unwrap();
}

#[tokio::test]
async fn migration_without_tenant_id_or_with_a_view_fails() {
    let s = require_pg!(setup().await);
    for (stmt, expect) in [
        (
            "CREATE TABLE IF NOT EXISTS loose (x text)",
            "without a tenant_id",
        ),
        ("CREATE VIEW v_notes AS SELECT * FROM notes", "CREATE VIEW"),
        (
            "CREATE FUNCTION steal() RETURNS bigint LANGUAGE sql SECURITY DEFINER \
             AS 'SELECT count(*) FROM notes'",
            "not allowed in migrations",
        ),
        (
            "CREATE FUNCTION steal_trg() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER \
             AS 'BEGIN PERFORM set_config(''app.tenant_id'', ''other'', true); RETURN NEW; END'",
            "not allowed in migrations",
        ),
        (
            "CREATE PROCEDURE steal_proc() LANGUAGE sql SECURITY DEFINER              AS 'SELECT count(*) FROM notes'",
            "not allowed in migrations",
        ),
        (
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'SELECT 1'",
            "CREATE FUNCTION",
        ),
        ("CREATE TABLE t2 (tenant_id text); COMMIT", "more than one command"),
    ] {
        let mut m = manifest();
        m.migrations[0].statements = vec![stmt.to_string()];
        let tenant = format!("tv{}", suffix());
        let err = migrate_as(&s, &m, &tenant).await.unwrap_err();
        assert!(err.contains(expect), "{stmt}: {err}");
        if !stmt.contains("loose") {
            assert!(err.contains("not allowed in migrations"), "{stmt}: {err}");
        }
    }
}

#[tokio::test]
async fn serve_refuses_tables_without_forced_rls() {
    let s = require_pg!(setup().await);
    let schema = format!("t_{}", s.tenant);
    // A table added behind migrate's back gets no grants by default; one
    // that a tenant role was granted (even a single column) is refused.
    s.admin
        .batch_execute(&format!(
            "SET ROLE {owner}; CREATE TABLE {schema}.later (tenant_id text); RESET ROLE; \
             GRANT SELECT (tenant_id) ON {schema}.later TO {reader}",
            owner = s.owner,
            reader = trole(&s.tenant, "public_reader")
        ))
        .await
        .unwrap();
    let err = check_tenant(&s.admin_url, &s.tenant, "public_reader")
        .await
        .unwrap_err();
    assert!(err.contains(&format!("{schema}.later")), "{err}");
    s.admin
        .batch_execute(&format!(
            "ALTER TABLE {schema}.later ENABLE ROW LEVEL SECURITY"
        ))
        .await
        .unwrap();
    let err = check_tenant(&s.admin_url, &s.tenant, "public_reader")
        .await
        .unwrap_err();
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
    // FORCE alone is still insufficient: every tenant table needs a policy.
    let err = check_tenant(&s.admin_url, &s.tenant, "public_reader")
        .await
        .unwrap_err();
    assert!(
        err.contains(&format!(
            "{schema}.later: tenant_isolation policy is missing"
        )),
        "{err}"
    );
    s.admin
        .batch_execute(&format!("DROP TABLE {schema}.later"))
        .await
        .unwrap();
    check_tenant(&s.admin_url, &s.tenant, "public_reader")
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
    for stmt in [
        "CREATE POLICY open ON audit_notes USING (true)",
        "CREATE POLICY narrow ON audit_notes AS RESTRICTIVE USING (true)",
    ] {
        let mut m = manifest();
        m.migrations[0].statements.push(stmt.to_string());
        let tenant = format!("tp{}", suffix());
        let err = migrate_as(&s, &m, &tenant).await.unwrap_err();
        assert!(
            err.contains("CREATE POLICY") && err.contains("not allowed in migrations"),
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
    migrate_as(&s, &manifest(), &format!("tq{}", suffix()))
        .await
        .unwrap();
}

#[tokio::test]
async fn serve_refuses_extra_or_altered_policies() {
    let s = require_pg!(setup().await);
    let schema = format!("t_{}", s.tenant);
    check_tenant(&s.admin_url, &s.tenant, "app_rw")
        .await
        .unwrap();
    // Reproduce the leak first: an extra permissive policy lets the tenant
    // role read a row that is not its tenant's.
    s.admin
        .batch_execute(&format!(
            "GRANT SELECT ON {schema}.audit_notes TO {app}; \
             INSERT INTO {schema}.audit_notes VALUES ('othertenant1', 'leak'); \
             CREATE POLICY open ON {schema}.audit_notes USING (true)",
            app = s.app
        ))
        .await
        .unwrap();
    let app = connect(&url_as(&s.admin_url, &s.app)).await;
    let leaked: i64 = app
        .query_one(
            &format!("SELECT count(*) FROM {schema}.audit_notes WHERE tenant_id = 'othertenant1'"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(leaked, 1, "extra permissive policy widens access");
    let err = check_tenant(&s.admin_url, &s.tenant, "app_rw")
        .await
        .unwrap_err();
    assert!(
        err.contains(&format!("{schema}.audit_notes: unexpected policy open")),
        "{err}"
    );
    s.admin
        .batch_execute(&format!(
            "DROP POLICY open ON {schema}.audit_notes; DELETE FROM {schema}.audit_notes; \
             REVOKE SELECT ON {schema}.audit_notes FROM {app}",
            app = s.app
        ))
        .await
        .unwrap();
    check_tenant(&s.admin_url, &s.tenant, "app_rw")
        .await
        .unwrap();
    // A tenant_isolation whose expression was altered is refused as well.
    s.admin
        .batch_execute(&format!(
            "ALTER POLICY tenant_isolation ON {schema}.notes USING (true)"
        ))
        .await
        .unwrap();
    let err = check_tenant(&s.admin_url, &s.tenant, "app_rw")
        .await
        .unwrap_err();
    assert!(
        err.contains(&format!(
            "{schema}.notes: tenant_isolation policy was altered"
        )),
        "{err}"
    );
    // The old GUC-based policy, or one for PUBLIC / another tenant: refused.
    for policy in [
        "USING (\"tenant_id\" = current_setting('app.tenant_id', false)) \
         WITH CHECK (\"tenant_id\" = current_setting('app.tenant_id', false))"
            .to_string(),
        format!(
            "TO {app} USING (\"tenant_id\" = 'othertenant1') WITH CHECK (\"tenant_id\" = 'othertenant1')",
            app = s.app
        ),
    ] {
        s.admin
            .batch_execute(&format!(
                "DROP POLICY tenant_isolation ON {schema}.notes; \
                 CREATE POLICY tenant_isolation ON {schema}.notes {policy}"
            ))
            .await
            .unwrap();
        assert!(
            check_tenant(&s.admin_url, &s.tenant, "app_rw").await.is_err(),
            "{policy}"
        );
    }
    // Re-running migrate restores the canonical policy.
    migrate_as(&s, &manifest(), &s.tenant).await.unwrap();
    check_tenant(&s.admin_url, &s.tenant, "app_rw")
        .await
        .unwrap();
}

/// A migration touching another tenant failed: the per-tenant migration role
/// has no privileges there (42501 / must be owner), or vibe's own
/// cross-tenant checks caught it.
fn blocked(err: &str) -> bool {
    [
        "row level security check failed",
        "another tenant",
        "not allowed in migrations",
        "42501",
        "must be owner",
    ]
    .iter()
    .any(|m| err.contains(m))
}

/// Count of rows in `table` (a tenant's `audit_notes`) visible to the
/// tenant role of `tenant`; `None` if it may not read the table at all.
async fn visible_rows(s: &Setup, table: &str, tenant: &str) -> Option<i64> {
    let app = connect(&url_as(&s.admin_url, &trole(tenant, "app_rw"))).await;
    app.query_one(&format!("SELECT count(*) FROM {table}"), &[])
        .await
        .ok()
        .map(|row| row.get(0))
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
        let err = migrate_as(&s, &m, &tenant).await.unwrap_err();
        assert!(blocked(&err), "{attack}: {err}");
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
            visible_rows(&s, &format!("{victim}.audit_notes"), &tenant).await,
            None,
            "{attack}: other tenants cannot read the victim's table"
        );
        check_tenant(&s.admin_url, &s.tenant, "app_rw")
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn migration_cannot_end_the_transaction() {
    let s = require_pg!(setup().await);
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
        let err = migrate_as(&s, &m, &tenant).await.unwrap_err();
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
    migrate_as(&s, &m, &tenant).await.unwrap_err();
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
    migrate_as(&s, &manifest(), &format!("tz{}", suffix()))
        .await
        .unwrap();
}

#[tokio::test]
async fn postgres_errors_carry_detail() {
    let s = require_pg!(setup().await);
    let mut m = manifest();
    m.migrations[0]
        .statements
        .push("ALTER TABLE no_such_table ADD COLUMN x text".into());
    let err = migrate_as(&s, &m, &format!("te{}", suffix()))
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
        let err = migrate_as(&s, &m, &tenant).await.unwrap_err();
        assert!(
            err.contains("42601")
                || err.contains("multiple commands")
                || err.contains("more than one command"),
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
        let err = migrate_as(&s, &m, &format!("tf{}", suffix()))
            .await
            .unwrap_err();
        assert!(blocked(&err), "{attack}: {err}");
    }
}

#[tokio::test]
async fn migration_repairs_only_its_tenant() {
    let s = require_pg!(setup().await);
    let victim = format!("t_{}", s.tenant);
    // Left behind outside vibe (e.g. by an older, unprotected migration).
    s.admin
        .batch_execute(&format!(
            "ALTER TABLE {victim}.audit_notes NO FORCE ROW LEVEL SECURITY"
        ))
        .await
        .unwrap();
    assert!(
        check_tenant(&s.admin_url, &s.tenant, "app_rw")
            .await
            .is_err(),
        "serve refuses"
    );
    let other = format!("tg{}", suffix());
    migrate_as(&s, &manifest(), &other).await.unwrap();
    // A healthy tenant cannot start while any other schema is broken.
    let err = check_tenant(&s.admin_url, &other, "app_rw")
        .await
        .unwrap_err();
    assert!(err.contains(&format!("{victim}.audit_notes")), "{err}");
    // Migrating the affected tenant repairs it.
    migrate_as(&s, &manifest(), &s.tenant).await.unwrap();
    check_tenant(&s.admin_url, &s.tenant, "app_rw")
        .await
        .unwrap();
}

const PENDING_ROLE_SPEC: &str =
    "decision 97: each tenant migrates with its own migration role that owns only its schema";

/// Migrates a fresh attacker tenant with `attack` appended; returns its schema.
async fn attack_data(s: &Setup, attack: &[String]) -> String {
    let mut m = manifest();
    m.migrations[0].statements.extend(attack.iter().cloned());
    let tenant = format!("th{}", suffix());
    let _ = migrate_as(s, &m, &tenant).await;
    format!("t_{tenant}")
}

#[tokio::test]
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
    let _ = migrate_as(&s, &m, &format!("ti{}", suffix())).await;
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
        Err(ExecError::PoolExhausted) => "exhausted".into(),
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
/// (through per-tenant pools logged in as each tenant role, as `vibe serve`
/// does) and requires the same outcome class for each.
#[tokio::test]
async fn memory_and_postgres_agree() {
    let s = require_pg!(setup().await);
    let other = format!("to{}", suffix());
    migrate_as(&s, &manifest(), &other).await.unwrap();
    let pg = pools_for(&s.admin_url, &[&s.tenant, &other]);
    assert_eq!(pg.check_all(&manifest().models).await.unwrap(), 6);
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

/// `app.tenant_id` is not an identity (decision 97): a tenant login sees its
/// own rows with the setting unset or set to another tenant, and never sees
/// another tenant's rows, on fresh and reused sessions alike.
#[tokio::test]
async fn tenant_setting_is_not_the_tenant_boundary() {
    let s = require_pg!(setup().await);
    let b = second_tenant(&s).await;
    let schema = format!("t_{}", s.tenant);
    s.admin
        .batch_execute(&format!(
            "INSERT INTO {schema}.notes (id, tenant_id, body, created_at, updated_at) \
             VALUES (gen_random_uuid(), '{t}', 'a', now(), now())",
            t = s.tenant
        ))
        .await
        .unwrap();
    let app = connect(&url_as(&s.admin_url, &s.app)).await;
    let count = |sql: String| {
        let app = &app;
        async move { app.query_one(&sql, &[]).await.map(|r| r.get::<_, i64>(0)) }
    };
    // Fresh session, setting never defined: own rows are visible.
    assert_eq!(
        count(format!("SELECT count(*) FROM {schema}.notes"))
            .await
            .unwrap(),
        1
    );
    // Setting pointed at tenant B: still only own rows, B's schema denied.
    app.batch_execute(&format!("SELECT set_config('app.tenant_id', '{b}', false)"))
        .await
        .unwrap();
    assert_eq!(
        count(format!("SELECT count(*) FROM {schema}.notes"))
            .await
            .unwrap(),
        1
    );
    assert!(count(format!("SELECT count(*) FROM t_{b}.notes"))
        .await
        .is_err());
}

#[tokio::test]
async fn cancelled_migration_does_not_return_its_connection_mid_transaction() {
    let s = require_pg!(setup().await);
    let tenant = format!("tc{}", suffix());
    ensure_tenant_roles(&s.admin_url, &tenant).await;
    let owner = owner_of(&tenant);
    let exec = PgExec::connect_with(
        &url_as(&s.admin_url, &owner),
        vibe::runtime::PgOptions {
            pool_size: 1,
            statement_timeout_ms: 30_000,
            pool_timeout_ms: 5_000,
        },
    )
    .await
    .unwrap();
    migrate_tenant(&exec, &manifest(), &tenant).await.unwrap();
    // Another session holds a lock on the table, so the next migration
    // waits inside its transaction.
    let locker = connect(&s.admin_url).await;
    locker
        .batch_execute(&format!(
            "BEGIN; LOCK TABLE t_{tenant}.audit_notes IN ACCESS EXCLUSIVE MODE"
        ))
        .await
        .unwrap();
    let mut m = manifest();
    m.migrations[0]
        .statements
        .push("ALTER TABLE audit_notes ADD COLUMN waited text".into());
    // Cancel the migration while it waits inside its transaction, after
    // reading the pid of the backend running it from pg_stat_activity.
    let find_pid = async {
        loop {
            let rows = s
                .admin
                .query(
                    "SELECT pid FROM pg_stat_activity WHERE usename = $1 \
                     AND state = 'active' AND wait_event_type = 'Lock'",
                    &[&owner],
                )
                .await
                .unwrap();
            if let Some(row) = rows.first() {
                return row.get::<_, i32>(0);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    };
    let migration = migrate_tenant(&exec, &m, &tenant);
    let sleeping_pid = tokio::select! {
        pid = tokio::time::timeout(std::time::Duration::from_secs(4), find_pid) => {
            pid.expect("the waiting migration never showed up in pg_stat_activity")
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
    // a fresh session sees no open transaction and the column was not added.
    let got = exec
        .query_texts(
            "SELECT coalesce(txid_current_if_assigned()::text, 'none') || '|' || \
             (SELECT count(*) FROM information_schema.columns WHERE table_schema = $1 \
              AND table_name = 'audit_notes' AND column_name = 'waited')::text",
            &format!("t_{tenant}"),
        )
        .await
        .unwrap();
    assert_eq!(got, vec!["none|0".to_string()]);
    locker.batch_execute("ROLLBACK").await.unwrap();
}

// ---- D129 owner login window, allowlist and index placement ----

/// Pre-created roles with the owner NOLOGIN and no password, as operators
/// leave it outside migrate.
async fn ensure_nologin_owner(admin_url: &str, tenant: &str) {
    ensure_tenant_roles(admin_url, tenant).await;
    connect(admin_url)
        .await
        .batch_execute(&format!(
            "ALTER ROLE {} NOLOGIN PASSWORD NULL",
            owner_of(tenant)
        ))
        .await
        .unwrap();
}

/// (rolcanlogin, has a password) for `role`.
async fn login_state(admin: &Client, role: &str) -> (bool, bool) {
    let row = admin
        .query_one(
            "SELECT rolcanlogin, rolpassword IS NOT NULL FROM pg_authid WHERE rolname = $1",
            &[&role],
        )
        .await
        .unwrap();
    (row.get(0), row.get(1))
}

async fn audit_events(admin: &Client, tenant: &str) -> Vec<String> {
    admin
        .query(
            "SELECT event FROM public.vibe_migrate_audit WHERE tenant = $1 ORDER BY at, event",
            &[&tenant],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect()
}

#[tokio::test]
async fn migrate_opens_and_always_closes_the_owner_login_window() {
    let s = require_pg!(setup().await);
    let tenant = format!("tw{}", suffix());
    let owner = owner_of(&tenant);
    ensure_nologin_owner(&s.admin_url, &tenant).await;
    assert_eq!(login_state(&s.admin, &owner).await, (false, false));
    let log = vibe::runtime::migrate_with_owner_window(&s.admin_url, &manifest(), &tenant)
        .await
        .unwrap();
    assert!(log.iter().any(|l| l.starts_with("verified")), "{log:?}");
    // Success: NOLOGIN again, password removed, audited.
    assert_eq!(login_state(&s.admin, &owner).await, (false, false));
    let events = audit_events(&s.admin, &tenant).await;
    assert!(
        events.iter().any(|e| e == "migrate committed"),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| e.contains("owner login window closed")),
        "{events:?}"
    );
    check_tenant(&s.admin_url, &tenant, "app_rw").await.unwrap();

    // Failure: the transaction rolls back and the window still closes.
    let failing = format!("tw{}", suffix());
    let failing_owner = owner_of(&failing);
    ensure_nologin_owner(&s.admin_url, &failing).await;
    let mut m = manifest();
    m.migrations[0]
        .statements
        .push("CREATE TABLE IF NOT EXISTS loose (x text)".into());
    let err = vibe::runtime::migrate_with_owner_window(&s.admin_url, &m, &failing)
        .await
        .unwrap_err();
    assert!(err.contains("tenant_id"), "{err}");
    assert_eq!(login_state(&s.admin, &failing_owner).await, (false, false));
    let left: i64 = s
        .admin
        .query_one(
            "SELECT count(*) FROM pg_namespace WHERE nspname = $1",
            &[&format!("t_{failing}")],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(left, 0, "a failed migration commits nothing");
    let events = audit_events(&s.admin, &failing).await;
    assert!(
        events.iter().any(|e| e.starts_with("migrate failed")),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| e.contains("owner login window closed")),
        "{events:?}"
    );
    // A rejected statement (allowlist) also closes the window.
    let mut m = manifest();
    m.migrations[0]
        .statements
        .push("DELETE FROM audit_notes".into());
    let err = vibe::runtime::migrate_with_owner_window(&s.admin_url, &m, &failing)
        .await
        .unwrap_err();
    assert!(err.contains("not allowed in migrations"), "{err}");
    assert_eq!(login_state(&s.admin, &failing_owner).await, (false, false));
}

#[tokio::test]
async fn migrate_refuses_an_owner_that_can_already_log_in() {
    let s = require_pg!(setup().await);
    let tenant = format!("tl{}", suffix());
    // ensure_tenant_roles leaves the owner LOGIN with a password.
    ensure_tenant_roles(&s.admin_url, &tenant).await;
    let err = vibe::runtime::migrate_with_owner_window(&s.admin_url, &manifest(), &tenant)
        .await
        .unwrap_err();
    assert!(err.contains("can already log in"), "{err}");
}

#[tokio::test]
async fn periodic_check_disables_and_reports_owners_that_can_log_in() {
    let _exclusive = OWNER_LOGINS.write().await;
    let s = require_pg!(setup_with_owner_guard(None).await);
    let tenant = format!("tc{}", suffix());
    let owner = owner_of(&tenant);
    ensure_tenant_roles(&s.admin_url, &tenant).await;
    let admin = PgExec::connect(&s.admin_url).await.unwrap();

    // While a migrate window lock is held for the owner, it is not reported.
    let holder = connect(&s.admin_url).await;
    holder
        .execute(
            "SELECT pg_advisory_lock($1, oid::bigint::bit(32)::int) FROM pg_roles WHERE rolname = $2",
            &[&vibe::runtime::migrate::MIGRATE_LOCK_CLASS, &owner],
        )
        .await
        .unwrap();
    let open = admin
        .query_texts(vibe::runtime::migrate::OWNER_LOGIN_SQL, "")
        .await
        .unwrap();
    assert!(!open.contains(&owner), "{open:?}");
    drop(holder);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let before = vibe::runtime::owner_login_alerts_total();
    let found = vibe::runtime::check_owner_logins(&admin).await.unwrap();
    assert!(found.contains(&owner), "{found:?}");
    assert!(vibe::runtime::owner_login_alerts_total() > before);
    assert_eq!(login_state(&s.admin, &owner).await, (false, false));
    let again = vibe::runtime::check_owner_logins(&admin).await.unwrap();
    assert!(!again.contains(&owner), "{again:?}");
}

#[tokio::test]
async fn serve_refuses_a_pool_logged_in_as_an_owner() {
    let s = require_pg!(setup().await);
    let exec = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    for class in RUNTIME_ROLES {
        let err = exec.check_tenant_role(&s.tenant, class).await.unwrap_err();
        assert!(err.contains("owner role"), "{class}: {err}");
    }
}

/// (index name, is unique, is valid) in `schema`.
async fn indexes(admin: &Client, schema: &str) -> Vec<(String, bool, bool)> {
    admin
        .query(
            "SELECT c.relname::text, i.indisunique, i.indisvalid FROM pg_index i \
             JOIN pg_class c ON c.oid = i.indexrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 ORDER BY 1",
            &[&schema],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect()
}

#[tokio::test]
async fn unique_indexes_are_transactional_and_others_concurrent_after_commit() {
    let s = require_pg!(setup().await);
    let mut m = manifest();
    m.migrations[0].statements.extend([
        "CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS audit_note_u ON audit_notes (tenant_id, note)"
            .to_string(),
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS audit_note_p ON audit_notes (note)".to_string(),
    ]);
    // A later failure rolls back the UNIQUE index with everything else, and
    // the performance index is never built.
    let failing = format!("ti{}", suffix());
    let mut bad = m.clone();
    bad.migrations[0]
        .statements
        .push("CREATE TABLE IF NOT EXISTS loose (x text)".into());
    migrate_as(&s, &bad, &failing).await.unwrap_err();
    assert!(indexes(&s.admin, &format!("t_{failing}")).await.is_empty());

    let tenant = format!("ti{}", suffix());
    let log = migrate_as(&s, &m, &tenant).await.unwrap();
    assert!(
        log.iter()
            .any(|l| l.starts_with("audit: built index concurrently after commit")),
        "{log:?}"
    );
    let got = indexes(&s.admin, &format!("t_{tenant}")).await;
    assert!(
        got.contains(&("audit_note_u".into(), true, true)),
        "{got:?}"
    );
    assert!(
        got.contains(&("audit_note_p".into(), false, true)),
        "{got:?}"
    );
}

/// Indexes in `schema` that the platform recorded as built by its
/// concurrent index step.
async fn recorded_indexes(admin: &Client, tenant: &str) -> Vec<String> {
    admin
        .query(
            "SELECT index_name FROM public.vibe_concurrent_indexes WHERE tenant = $1 ORDER BY 1",
            &[&tenant],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect()
}

async fn mark_invalid(admin: &Client, schema: &str, names: &[&str]) {
    for name in names {
        admin
            .batch_execute(&format!(
                "UPDATE pg_index SET indisvalid = false WHERE indexrelid = '{schema}.{name}'::regclass"
            ))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn platform_recorded_invalid_index_is_dropped_and_rebuilt_with_an_audit_entry() {
    let s = require_pg!(setup().await);
    let tenant = format!("tr{}", suffix());
    let schema = format!("t_{tenant}");
    let mut m = manifest();
    m.migrations[0].statements.extend([
        "CREATE UNIQUE INDEX IF NOT EXISTS audit_note_u ON audit_notes (tenant_id, note)"
            .to_string(),
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS audit_note_p ON audit_notes (note)".to_string(),
    ]);
    ensure_nologin_owner(&s.admin_url, &tenant).await;
    vibe::runtime::migrate_with_owner_window(&s.admin_url, &m, &tenant)
        .await
        .unwrap();
    // Only the concurrent index step's index is recorded.
    assert_eq!(
        recorded_indexes(&s.admin, &tenant).await,
        vec!["audit_note_p"]
    );
    // Simulate a failed concurrent build.
    mark_invalid(&s.admin, &schema, &["audit_note_p"]).await;
    let log = vibe::runtime::migrate_with_owner_window(&s.admin_url, &m, &tenant)
        .await
        .unwrap();
    let line = format!(
        "audit: dropped invalid index {schema}.audit_note_p; rebuilding concurrently after commit"
    );
    assert!(log.contains(&line), "{line} not in {log:?}");
    let got = indexes(&s.admin, &schema).await;
    assert!(
        got.contains(&("audit_note_u".into(), true, true)),
        "{got:?}"
    );
    assert!(
        got.contains(&("audit_note_p".into(), false, true)),
        "{got:?}"
    );
    let events = audit_events(&s.admin, &tenant).await;
    assert!(
        events
            .iter()
            .any(|e| e.starts_with("dropped invalid index") && e.contains("audit_note_p")),
        "{events:?}"
    );
}

#[tokio::test]
async fn invalid_index_not_recorded_by_the_platform_is_left_untouched() {
    let s = require_pg!(setup().await);
    let tenant = format!("tn{}", suffix());
    let schema = format!("t_{tenant}");
    let mut m = manifest();
    m.migrations[0].statements.push(
        "CREATE UNIQUE INDEX IF NOT EXISTS audit_note_u ON audit_notes (tenant_id, note)"
            .to_string(),
    );
    ensure_nologin_owner(&s.admin_url, &tenant).await;
    vibe::runtime::migrate_with_owner_window(&s.admin_url, &m, &tenant)
        .await
        .unwrap();
    // An index made outside the concurrent index step, plus the migration's
    // UNIQUE index (built in the transaction, never recorded): both INVALID.
    s.admin
        .batch_execute(&format!(
            "CREATE INDEX outside_idx ON {schema}.audit_notes (note)"
        ))
        .await
        .unwrap();
    mark_invalid(&s.admin, &schema, &["outside_idx", "audit_note_u"]).await;
    assert!(recorded_indexes(&s.admin, &tenant).await.is_empty());
    let oids_before = s
        .admin
        .query(
            &format!(
                "SELECT '{schema}.outside_idx'::regclass::oid, '{schema}.audit_note_u'::regclass::oid"
            ),
            &[],
        )
        .await
        .unwrap();
    let log = vibe::runtime::migrate_with_owner_window(&s.admin_url, &m, &tenant)
        .await
        .unwrap();
    assert!(
        !log.iter().any(|l| l.contains("dropped invalid index")),
        "{log:?}"
    );
    assert!(
        log.iter()
            .any(|l| l.contains("outside_idx") && l.contains("left in place")),
        "{log:?}"
    );
    let got = indexes(&s.admin, &schema).await;
    assert!(
        got.contains(&("outside_idx".into(), false, false)),
        "{got:?}"
    );
    assert!(
        got.contains(&("audit_note_u".into(), true, false)),
        "{got:?}"
    );
    let oids_after = s
        .admin
        .query(
            &format!(
                "SELECT '{schema}.outside_idx'::regclass::oid, '{schema}.audit_note_u'::regclass::oid"
            ),
            &[],
        )
        .await
        .unwrap();
    let pair = |rows: &[tokio_postgres::Row]| -> (u32, u32) { (rows[0].get(0), rows[0].get(1)) };
    assert_eq!(pair(&oids_before), pair(&oids_after));
    // A record for another tenant does not count for this one.
    s.admin
        .execute(
            "INSERT INTO public.vibe_concurrent_indexes (tenant, index_name) VALUES ($1, 'outside_idx') \
             ON CONFLICT DO NOTHING",
            &[&format!("{tenant}x")],
        )
        .await
        .unwrap();
    vibe::runtime::migrate_with_owner_window(&s.admin_url, &m, &tenant)
        .await
        .unwrap();
    assert!(indexes(&s.admin, &schema)
        .await
        .contains(&("outside_idx".into(), false, false)));
}

#[tokio::test]
async fn tenant_pools_refuse_at_the_role_cap_without_waiting() {
    let s = require_pg!(setup().await);
    let pools = pools_for(&s.admin_url, &[&s.tenant]);
    let held: Vec<_> = (0..5)
        .map(|_| pools.acquire(&s.tenant, "app_rw").unwrap())
        .collect();
    let before = vibe::runtime::pool_exhausted_count(vibe::runtime::PoolScope::Role);
    let started = std::time::Instant::now();
    let err = pools
        .execute(
            &s.tenant,
            "app_rw",
            Stmt::Select {
                table: "notes".into(),
                columns: vec![],
                id: None,
                filters: vec![],
                sort: None,
                limit: 1,
                offset: 0,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, vibe::runtime::ExecError::PoolExhausted),
        "{err:?}"
    );
    assert!(started.elapsed() < std::time::Duration::from_millis(200));
    assert!(vibe::runtime::pool_exhausted_count(vibe::runtime::PoolScope::Role) > before);
    drop(held);
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
async fn replay_store_runtime_role_cannot_delete_live_rows_or_update() {
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
    assert_eq!(got, vec![true, true, true, false, false]);
    // A verified row cannot be removed or shortened by the runtime role.
    let (a, b) = replay_stores(&s).await;
    let t = Some(s.tenant.as_str());
    let k = token_key("kept");
    let slot = a.reserve(&k, None, t).await.unwrap();
    a.settle(&slot, Outcome::Verified).await.unwrap();
    let app = connect(&url_as(&s.admin_url, &s.app)).await;
    let set = format!("SET app.tenant_id = '{}'", s.tenant);
    app.batch_execute(&set).await.unwrap();
    assert_eq!(
        app.execute(&format!("DELETE FROM {table}"), &[])
            .await
            .unwrap(),
        0
    );
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
    // D129: each tenant's rows go through that tenant's own app_rw pool.
    let pools = std::sync::Arc::new(pools_for(&s.admin_url, &[&s.tenant, &other]));
    let a = PgReplayStore::for_tenant_pools(pools.clone(), IP_SECRET).with_capacity(2);
    let b = PgReplayStore::for_tenant_pools(pools, IP_SECRET).with_capacity(2);
    a.check(&[s.tenant.clone(), other.clone()]).await.unwrap();
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
    // One tenant's role cannot reach the other tenant's table at all.
    let app = connect(&url_as(&s.admin_url, &trole(&other, "app_rw"))).await;
    assert!(app
        .query_one(
            &format!("SELECT count(*) FROM t_{}.vibe_challenge_replay", s.tenant),
            &[],
        )
        .await
        .is_err());
    let own: i64 = app
        .query_one(
            &format!("SELECT count(*) FROM t_{other}.vibe_challenge_replay"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(own > 0);
    // Rows for another tenant cannot be written through this tenant's role.
    assert!(app
        .batch_execute(&format!(
            "INSERT INTO t_{other}.vibe_challenge_replay (tenant_id, key, nonce, state, expires_at) \
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
    // The startup check `vibe serve` runs: a tenant without the table.
    let err = store
        .check(&["nomigrate0001".to_string()])
        .await
        .unwrap_err();
    assert!(err.contains("run `vibe migrate`"), "{err}");
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

// D129 (4): idle tenant pools close and reopen on the next request.

fn select_notes() -> Stmt {
    Stmt::Select {
        table: "notes".into(),
        columns: vec![],
        id: None,
        filters: vec![],
        sort: None,
        limit: 1,
        offset: 0,
    }
}

#[tokio::test]
async fn idle_tenant_pools_close_and_reopen_on_the_next_request() {
    let s = require_pg!(setup().await);
    assert_eq!(
        vibe::runtime::POOL_IDLE_TIMEOUT,
        std::time::Duration::from_secs(300)
    );
    let pools = pools_for(&s.admin_url, &[&s.tenant])
        .with_idle_timeout(std::time::Duration::from_millis(200));
    pools
        .execute(&s.tenant, "app_rw", select_notes())
        .await
        .unwrap();
    assert_eq!(pools.open_pools().await, 1);
    // Used recently: stays open.
    assert_eq!(pools.close_idle().await, 0);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // Unused but a request holds its slot: stays open.
    let held = pools.acquire(&s.tenant, "app_rw").unwrap();
    let conn = pools.pool(&s.tenant, "app_rw").await.unwrap().pool();
    let checked_out = conn.get().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(pools.close_idle().await, 0);
    drop(checked_out);
    drop(held);
    let before = vibe::runtime::pool_idle_closed_total();
    assert_eq!(pools.close_idle().await, 1);
    assert_eq!(pools.open_pools().await, 0);
    assert!(vibe::runtime::pool_idle_closed_total() > before);
    assert!(conn.is_closed());
    // The next request opens it again.
    pools
        .execute(&s.tenant, "app_rw", select_notes())
        .await
        .unwrap();
    assert_eq!(pools.open_pools().await, 1);
}

#[tokio::test]
async fn idle_closer_task_closes_unused_pools() {
    let s = require_pg!(setup().await);
    let pools = std::sync::Arc::new(
        pools_for(&s.admin_url, &[&s.tenant])
            .with_idle_timeout(std::time::Duration::from_millis(100)),
    );
    let task = vibe::runtime::TenantPools::spawn_idle_closer(&pools);
    pools
        .execute(&s.tenant, "app_rw", select_notes())
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while pools.open_pools().await > 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "idle pool was never closed"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    drop(pools);
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("the closer stops once the pools are dropped")
        .unwrap();
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
async fn legacy_tenants_upgrade_in_sequence() {
    let s = require_pg!(setup().await);
    let other = second_tenant(&s).await;
    for tenant in [&s.tenant, &other] {
        s.admin
            .batch_execute(&format!(
                "DROP POLICY tenant_isolation ON t_{tenant}.audit_notes; \
             CREATE POLICY tenant_isolation ON t_{tenant}.audit_notes \
             USING (tenant_id = current_setting('app.tenant_id', false)) \
             WITH CHECK (tenant_id = current_setting('app.tenant_id', false))"
            ))
            .await
            .unwrap();
    }
    for tenant in [&s.tenant, &other] {
        let owner = PgExec::connect(&url_as(&s.admin_url, &owner_of(tenant)))
            .await
            .unwrap();
        migrate_tenant(&owner, &manifest(), tenant).await.unwrap();
        if tenant == &s.tenant {
            // Startup checks the entire DB, including the tenant still on legacy policy.
            let err = check_tenant(&s.admin_url, tenant, "app_rw")
                .await
                .unwrap_err();
            assert!(
                err.contains(&format!(
                    "t_{other}.audit_notes: tenant_isolation policy was altered"
                )),
                "{err}"
            );
        }
    }
    for tenant in [&s.tenant, &other] {
        check_tenant(&s.admin_url, tenant, "app_rw").await.unwrap();
    }
}

#[tokio::test]
async fn retention_removes_expired_rows_under_forced_rls() {
    let s = require_pg!(setup().await);
    let table = format!("t_{}.vibe_challenge_replay", s.tenant);
    let owner_client = connect(&url_as(&s.admin_url, &s.owner)).await;
    let flags = owner_client
        .query_one(
            "SELECT r.rolsuper, r.rolbypassrls, c.relrowsecurity, c.relforcerowsecurity \
         FROM pg_roles r, pg_class c WHERE r.rolname = current_user AND c.oid = $1::text::regclass",
            &[&table],
        )
        .await
        .unwrap();
    assert_eq!(
        (
            flags.get::<_, bool>(0),
            flags.get::<_, bool>(1),
            flags.get::<_, bool>(2),
            flags.get::<_, bool>(3)
        ),
        (false, false, true, true)
    );
    let rw = connect(&url_as(&s.admin_url, &s.app)).await;
    rw.batch_execute(&format!(
        "INSERT INTO {table} (tenant_id, key, nonce, state, expires_at) VALUES \
         ('{}', '\\x01', '\\x01', 'verified', now() - interval '1 hour'), \
         ('{}', '\\x02', '\\x02', 'verified', now() + interval '1 hour')",
        s.tenant, s.tenant
    ))
    .await
    .unwrap();
    rw.batch_execute(&format!("INSERT INTO {table} (tenant_id, key, nonce, state, expires_at) SELECT '{}', decode(lpad(to_hex(i), 8, '0'), 'hex'), decode('03', 'hex'), 'verified', now() - interval '1 hour' FROM generate_series(10, 2510) i", s.tenant)).await.unwrap();
    let owner = PgExec::connect(&url_as(&s.admin_url, &s.owner))
        .await
        .unwrap();
    // Hold a cleanup batch open while an independent runtime connection reads/inserts.
    owner_client.batch_execute(&format!("BEGIN; SET LOCAL ROLE {}; DELETE FROM {table} WHERE ctid IN (SELECT ctid FROM {table} WHERE expires_at < now() LIMIT 1000)", s.app)).await.unwrap();
    rw.batch_execute("SET lock_timeout = '100ms'")
        .await
        .unwrap();
    let pool = replay_pool(&s).await;
    pool.get()
        .await
        .unwrap()
        .batch_execute("SET lock_timeout = '100ms'")
        .await
        .unwrap();
    let store = PgReplayStore::new(pool, IP_SECRET);
    let slot = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        store.reserve(&token_key("during-cleanup"), None, Some(&s.tenant)),
    )
    .await
    .unwrap()
    .unwrap();
    store.settle(&slot, Outcome::Verified).await.unwrap();
    rw.query_one(&format!("SELECT count(*) FROM {table}"), &[])
        .await
        .unwrap();
    owner_client.batch_execute("ROLLBACK").await.unwrap();
    let other = second_tenant(&s).await;
    let other_table = format!("t_{other}.vibe_challenge_replay");
    s.admin.batch_execute(&format!("INSERT INTO {other_table} (tenant_id, key, nonce, state, expires_at) VALUES ('{other}', decode('01', 'hex'), decode('01', 'hex'), 'verified', now() - interval '1 hour')")).await.unwrap();
    let mut session = owner.session().await.unwrap();
    vibe::runtime::migrate::cleanup_replay(&mut session, &s.tenant)
        .await
        .unwrap();
    let remaining: i64 = s
        .admin
        .query_one(&format!("SELECT count(*) FROM {other_table}"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(remaining, 1);
    let row = s
        .admin
        .query_one(
            &format!("SELECT count(*) FILTER (WHERE expires_at <= now()), count(*) FROM {table}"),
            &[],
        )
        .await
        .unwrap();
    assert_eq!((row.get::<_, i64>(0), row.get::<_, i64>(1)), (0, 4));
    assert_eq!(
        rw.execute(&format!("DELETE FROM {table}"), &[])
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn pool_limits_apply_uniformly() {
    let s = require_pg!(setup().await);
    let pools = std::sync::Arc::new(pools_for(&s.admin_url, &[&s.tenant]));
    let store = PgReplayStore::for_tenant_pools(pools.clone(), b"test-secret");
    let permits: Vec<_> = (0..5)
        .map(|_| pools.acquire(&s.tenant, "app_rw").unwrap())
        .collect();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        store.reserve(&token_key("limited"), None, Some(&s.tenant)),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(ChallengeError::PoolExhausted)));
    assert_eq!(pools.opens_started(), 0);
    drop(permits);
    store
        .reserve(&token_key("available"), None, Some(&s.tenant))
        .await
        .unwrap();
}

#[tokio::test]
async fn replay_policy_drift_fails_closed() {
    let s = require_pg!(setup().await);
    let table = format!("t_{}.vibe_challenge_replay", s.tenant);
    for ddl in [
        format!("DROP POLICY replay_expired_delete ON {table}"),
        format!("CREATE POLICY replay_expired_delete ON {table} AS RESTRICTIVE FOR DELETE TO {} USING (tenant_id = '{}')", s.app, s.tenant),
    ] {
        s.admin.batch_execute(&ddl).await.unwrap();
        let err = check_tenant(&s.admin_url, &s.tenant, "app_rw").await.unwrap_err();
        assert!(err.contains(&format!("{table}: replay DELETE policy is missing or altered")), "{err}");
    }
    migrate_as(&s, &manifest(), &s.tenant).await.unwrap();
    check_tenant(&s.admin_url, &s.tenant, "app_rw")
        .await
        .unwrap();
}

#[tokio::test]
async fn startup_checks_unconfigured_tables_for_canonical_policy() {
    let s = require_pg!(setup().await);
    let other = format!("tb{}", suffix());
    s.admin
        .batch_execute(&format!(
            "CREATE SCHEMA t_{other}; CREATE TABLE t_{other}.entries (tenant_id text); \
         ALTER TABLE t_{other}.entries ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE t_{other}.entries FORCE ROW LEVEL SECURITY"
        ))
        .await
        .unwrap();
    assert!(check_tenant(&s.admin_url, &s.tenant, "app_rw")
        .await
        .is_err());
    s.admin
        .batch_execute(&format!("DROP SCHEMA t_{other} CASCADE"))
        .await
        .unwrap();
    s.admin
        .batch_execute(&format!(
            "DROP POLICY tenant_isolation ON t_{}.vibe_challenge_replay",
            s.tenant
        ))
        .await
        .unwrap();
    assert!(check_tenant(&s.admin_url, &s.tenant, "app_rw")
        .await
        .is_err());
}

#[tokio::test]
async fn replay_policy_is_limited_to_the_replay_table() {
    let s = require_pg!(setup().await);
    let pools = std::sync::Arc::new(pools_for(&s.admin_url, &[&s.tenant]));
    let store = PgReplayStore::for_tenant_pools(pools, IP_SECRET);
    // The installed policy passes migration, tenant-pool startup and replay startup.
    migrate_as(&s, &manifest(), &s.tenant).await.unwrap();
    check_tenant(&s.admin_url, &s.tenant, "app_rw")
        .await
        .unwrap();
    store.check(std::slice::from_ref(&s.tenant)).await.unwrap();

    // Even an otherwise canonical policy is forbidden on an ordinary table.
    // Include expires_at so rejection is about the policy, rather than DDL.
    s.admin
        .batch_execute(&format!(
            "ALTER TABLE t_{}.notes ADD COLUMN expires_at timestamptz; \
         CREATE POLICY replay_expired_delete ON t_{}.notes AS RESTRICTIVE \
         FOR DELETE TO {} USING (tenant_id = '{}' AND expires_at < now())",
            s.tenant, s.tenant, s.app, s.tenant
        ))
        .await
        .unwrap();
    let err = check_tenant(&s.admin_url, &s.tenant, "app_rw")
        .await
        .unwrap_err();
    assert!(err.contains("replay_expired_delete"), "{err}");
    let fresh_store = PgReplayStore::for_tenant_pools(
        std::sync::Arc::new(pools_for(&s.admin_url, &[&s.tenant])),
        IP_SECRET,
    );
    assert!(fresh_store
        .check(std::slice::from_ref(&s.tenant))
        .await
        .is_err());
    let err = migrate_as(&s, &manifest(), &s.tenant).await.unwrap_err();
    assert!(err.contains("replay_expired_delete"), "{err}");
}
