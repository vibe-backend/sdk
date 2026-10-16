//! Real PostgreSQL tests. They run when `VIBE_TEST_PG_ADMIN_URL` points at a
//! superuser connection (CI sets it via a service container). Outside CI they
//! are skipped with a SKIPPED note; in CI (`CI` set) a missing URL fails.

use serde_json::{json, Value};
use tokio_postgres::{Client, NoTls};
use vibe::manifest::Manifest;
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
/// roles with no memberships, and a migration role that may create the
/// tenant schema (and so owns only that schema).
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
    let admin = connect(&admin_url).await;
    let tenant = format!("tn{}", suffix());
    migrate_fresh(&admin_url, &manifest(), &tenant)
        .await
        .unwrap();
    Some(Setup {
        owner: owner_of(&tenant),
        app: trole(&tenant, "app_rw"),
        admin_url,
        admin,
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
    // No shared runtime role, no membership in either direction.
    let shared: i64 = s
        .admin
        .query_one(
            "SELECT count(*) FROM pg_auth_members m JOIN pg_roles r ON r.oid IN (m.member, m.roleid) \
             WHERE r.rolname = ANY($1)",
            &[&vec![rw.clone(), reader.clone(), writer.clone()]],
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
    let s = require_pg!(setup_exclusive().await);
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
    let err = pools_for(&s.admin_url, &[&s.tenant])
        .pool(&s.tenant, "app_rw")
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
        let err = migrate_as(&s, &m, &tenant).await.unwrap_err();
        assert!(err.contains(expect), "{stmt}: {err}");
    }
}

#[tokio::test]
async fn serve_refuses_tables_without_forced_rls() {
    let s = require_pg!(setup_exclusive().await);
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
    check_tenant(&s.admin_url, &s.tenant, "public_reader")
        .await
        .unwrap();
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
    for stmt in [
        "CREATE POLICY open ON audit_notes USING (true)",
        "CREATE POLICY narrow ON audit_notes AS RESTRICTIVE USING (true)",
    ] {
        let mut m = manifest();
        m.migrations[0].statements.push(stmt.to_string());
        let tenant = format!("tp{}", suffix());
        let err = migrate_as(&s, &m, &tenant).await.unwrap_err();
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
    migrate_as(&s, &manifest(), &format!("tq{}", suffix()))
        .await
        .unwrap();
}

#[tokio::test]
async fn serve_refuses_extra_or_altered_policies() {
    let s = require_pg!(setup_exclusive().await);
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
    assert!(err.contains(&format!("{schema}.audit_notes:open")), "{err}");
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
    assert!(err.contains("notes:tenant_isolation"), "{err}");
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
        check_tenant(&s.admin_url, &s.tenant, "app_rw")
            .await
            .is_err(),
        "serve refuses"
    );
    let err = migrate_as(&s, &manifest(), &format!("tg{}", suffix()))
        .await
        .unwrap_err();
    assert!(err.contains("already present before this run"), "{err}");
    assert!(err.contains(&victim), "{err}");
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
    let mut m = manifest();
    m.migrations[0].statements.push("SELECT pg_sleep(5)".into());
    // Cancel the migration while it sleeps inside its transaction, after
    // reading the pid of the backend running it from pg_stat_activity.
    let find_pid = async {
        loop {
            let rows = s
                .admin
                .query(
                    "SELECT pid FROM pg_stat_activity WHERE usename = $1 \
                     AND state = 'active' AND query LIKE 'SELECT pg_sleep(5)%'",
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
