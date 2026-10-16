use async_trait::async_trait;
use serde_json::{Map, Value};

use super::sql::{self, Col, Stmt};

pub type Row = Map<String, Value>;

#[derive(Debug)]
pub enum ExecError {
    NotFound,
    Conflict,
    Precondition,
    /// The database refused the statement for the role: a missing table or
    /// column privilege, or a row-level security violation (SQLSTATE 42501).
    /// The message is for logs only.
    Denied(String),
    Message(String),
}

/// Rejects a caller-supplied `tenant_id` before any executor runs the
/// statement (spec §2C.4): the executor sets it from the request tenant, so
/// even the caller's own tenant id is refused. The errors match what
/// PostgreSQL reports for the same statement under RLS + FORCE.
pub fn reject_tenant_input(stmt: &Stmt) -> Result<(), ExecError> {
    match stmt {
        Stmt::Insert { table, values, .. } if values.iter().any(|(c, _)| c.name == "tenant_id") => {
            Err(ExecError::Denied(format!(
                "new row violates row-level security policy for table \"{table}\""
            )))
        }
        Stmt::Update { table, sets, .. } if sets.iter().any(|(c, _)| c.name == "tenant_id") => Err(
            ExecError::Denied(format!("permission denied for table {table}")),
        ),
        _ => Ok(()),
    }
}

#[async_trait]
pub trait QueryExec: Send + Sync {
    async fn execute(&self, tenant_id: &str, role: &str, stmt: Stmt)
        -> Result<Vec<Row>, ExecError>;

    /// Runs a lock `query` step: validated, parameterised SQL in the tenant
    /// schema under row level security. Params are sent as text and parsed
    /// by PostgreSQL for whatever type the statement needs. Only PostgreSQL
    /// implements this.
    ///
    /// `role` is `public_reader` or `public_writer` (spec §12.8): the statement
    /// runs under `SET LOCAL ROLE`, and in a READ ONLY transaction unless
    /// `writes`.
    async fn raw(
        &self,
        _tenant_id: &str,
        _role: &str,
        _writes: bool,
        _sql: &str,
        _params: Vec<Option<String>>,
    ) -> Result<Vec<Row>, ExecError> {
        Err(ExecError::Message(
            "query steps need PostgreSQL; the --memory executor does not run them".into(),
        ))
    }

    /// Platform lookup for `sign-jwt`: `users.roles` of the user whose `id`
    /// is `user_id`; `None` when there is no such user.
    async fn user_roles(
        &self,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<Option<Vec<String>>, ExecError>;
}

/// Database roles a `query` step may run as.
pub const QUERY_ROLES: [&str; 2] = ["public_reader", "public_writer"];

pub(super) fn roles_from(value: Option<&Value>) -> Result<Vec<String>, ExecError> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| ExecError::Message("users.roles must be text[]".into()))
            })
            .collect(),
        Some(_) => Err(ExecError::Message("users.roles must be text[]".into())),
    }
}

/// A query parameter sent in text format so PostgreSQL infers its type.
#[derive(Debug)]
struct TextParam(Option<String>);

impl tokio_postgres::types::ToSql for TextParam {
    fn to_sql(
        &self,
        _ty: &tokio_postgres::types::Type,
        out: &mut bytes::BytesMut,
    ) -> Result<tokio_postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match &self.0 {
            Some(text) => {
                out.extend_from_slice(text.as_bytes());
                Ok(tokio_postgres::types::IsNull::No)
            }
            None => Ok(tokio_postgres::types::IsNull::Yes),
        }
    }

    fn accepts(_ty: &tokio_postgres::types::Type) -> bool {
        true
    }

    fn encode_format(&self, _ty: &tokio_postgres::types::Type) -> tokio_postgres::types::Format {
        tokio_postgres::types::Format::Text
    }

    tokio_postgres::types::to_sql_checked!();
}

/// Privileges of the runtime connection role, as read from the catalog.
/// Everything is evaluated with `pg_has_role(.., 'MEMBER')`, so a role that
/// could `SET ROLE` to a privileged or owning role (even through NOINHERIT
/// membership) counts as having that privilege.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RoleStatus {
    pub name: String,
    /// Superuser roles the connection role is (or can become).
    pub superuser_via: Vec<String>,
    /// BYPASSRLS roles the connection role is (or can become).
    pub bypass_rls_via: Vec<String>,
    /// Tenant-schema relations (tables, views, sequences, ...) it can own.
    pub owned_relations: i64,
    /// Tenant-schema functions/procedures it can own.
    pub owned_functions: i64,
    /// Tenant-schema types it can own.
    pub owned_types: i64,
    /// Tenant schemas it can own.
    pub owned_schemas: i64,
    /// Tenant relations the role can read or write that are not tables with
    /// ENABLE + FORCE row level security (views and RLS-less tables would
    /// expose every tenant).
    pub unprotected_relations: Vec<String>,
    /// Policies on tenant tables other than the canonical `tenant_isolation`
    /// (permissive policies are OR-combined, so any extra one widens access),
    /// plus a `tenant_isolation` whose definition was altered.
    pub foreign_policies: Vec<String>,
}

impl RoleStatus {
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.superuser_via.is_empty() {
            out.push(format!(
                "is or can become a superuser ({})",
                self.superuser_via.join(", ")
            ));
        }
        if !self.bypass_rls_via.is_empty() {
            out.push(format!(
                "is or can become a BYPASSRLS role ({})",
                self.bypass_rls_via.join(", ")
            ));
        }
        for (count, what) in [
            (self.owned_relations, "tenant relation(s)"),
            (self.owned_functions, "tenant function(s)"),
            (self.owned_types, "tenant type(s)"),
            (self.owned_schemas, "tenant schema(s)"),
        ] {
            if count > 0 {
                out.push(format!("can own {count} {what}"));
            }
        }
        if !self.unprotected_relations.is_empty() {
            out.push(format!(
                "can access tenant relation(s) without ENABLE + FORCE ROW LEVEL SECURITY ({}); re-run `vibe migrate` or enable RLS on them",
                self.unprotected_relations.join(", ")
            ));
        }
        if !self.foreign_policies.is_empty() {
            out.push(format!(
                "tenant table(s) carry policies other than the canonical tenant_isolation ({}); permissive policies are OR-combined and would let rows leak across tenants; drop them and re-run `vibe migrate`",
                self.foreign_policies.join(", ")
            ));
        }
        out
    }
}

/// Runtime roles (`app_rw`, `public_reader`, `public_writer`) that do not
/// exist or that the connection role cannot `SET ROLE` to.
pub const MISSING_ROLES_SQL: &str = "\
SELECT r FROM unnest($1::text[]) AS r
WHERE CASE WHEN EXISTS (SELECT 1 FROM pg_roles WHERE rolname = r)
           THEN NOT pg_has_role(current_user, r, 'MEMBER')
           ELSE true END
ORDER BY r";

/// Catalog query behind [`PgExec::check_runtime_role`].
pub const ROLE_STATUS_SQL: &str = "\
SELECT current_user::text, \
 COALESCE((SELECT array_agg(m.rolname::text ORDER BY m.rolname) FROM pg_roles m \
   WHERE m.rolsuper AND pg_has_role(current_user, m.oid, 'MEMBER')), '{}'), \
 COALESCE((SELECT array_agg(m.rolname::text ORDER BY m.rolname) FROM pg_roles m \
   WHERE m.rolbypassrls AND pg_has_role(current_user, m.oid, 'MEMBER')), '{}'), \
 (SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
   WHERE n.nspname LIKE 't\\_%' AND pg_has_role(current_user, c.relowner, 'MEMBER')), \
 (SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
   WHERE n.nspname LIKE 't\\_%' AND pg_has_role(current_user, p.proowner, 'MEMBER')), \
 (SELECT count(*) FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
   WHERE n.nspname LIKE 't\\_%' AND t.typrelid = 0 \
   AND pg_has_role(current_user, t.typowner, 'MEMBER')), \
 (SELECT count(*) FROM pg_namespace n WHERE n.nspname LIKE 't\\_%' \
   AND pg_has_role(current_user, n.nspowner, 'MEMBER')), \
 COALESCE((SELECT array_agg(format('%I.%I', n.nspname, c.relname) ORDER BY 1) \
   FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
   WHERE n.nspname LIKE 't\\_%' AND c.relkind IN ('r','p','v','m','f') \
   AND EXISTS (SELECT 1 FROM pg_roles m WHERE pg_has_role(current_user, m.oid, 'MEMBER') \
     AND (has_table_privilege(m.oid, c.oid, 'SELECT,INSERT,UPDATE,DELETE') \
       OR has_any_column_privilege(m.oid, c.oid, 'SELECT,INSERT,UPDATE'))) \
   AND NOT (c.relkind IN ('r','p') AND c.relrowsecurity AND c.relforcerowsecurity)), '{}'), \
 COALESCE((SELECT array_agg(format('%I.%I:%I', n.nspname, c.relname, p.polname) ORDER BY 1) \
   FROM pg_policy p JOIN pg_class c ON c.oid = p.polrelid \
   JOIN pg_namespace n ON n.oid = c.relnamespace \
   WHERE n.nspname LIKE 't\\_%' \
   AND EXISTS (SELECT 1 FROM pg_roles m WHERE pg_has_role(current_user, m.oid, 'MEMBER') \
     AND (has_table_privilege(m.oid, c.oid, 'SELECT,INSERT,UPDATE,DELETE') \
       OR has_any_column_privilege(m.oid, c.oid, 'SELECT,INSERT,UPDATE'))) AND NOT ( \
     p.polname = 'tenant_isolation' AND p.polpermissive AND p.polcmd = '*' \
     AND p.polroles = '{0}'::oid[] \
     AND pg_get_expr(p.polqual, p.polrelid) = '(tenant_id = current_setting(''app.tenant_id''::text, false))' \
     AND pg_get_expr(p.polwithcheck, p.polrelid) = '(tenant_id = current_setting(''app.tenant_id''::text, false))')), '{}')";

/// A statement kind the role may never run is a permission error (403); an
/// unknown role is a bug.
pub fn role_error(message: String) -> ExecError {
    if message.starts_with("unknown executor role") {
        ExecError::Message(message)
    } else {
        ExecError::Denied(message)
    }
}

/// Logical caller classes passed as `role` to [`QueryExec::execute`], and the
/// statements each may run. Enforced by every executor (spec §7.1/§7.2:
/// public readers only read; nobody deletes).
pub fn role_allows(role: &str, stmt: &Stmt) -> Result<(), String> {
    let allowed = match role {
        "public_reader" => matches!(stmt, Stmt::Select { .. }),
        "public_writer" | "app_rw" => matches!(
            stmt,
            Stmt::Select { .. }
                | Stmt::Insert { .. }
                | Stmt::Update { .. }
                | Stmt::SoftDelete { .. }
        ),
        other => return Err(format!("unknown executor role {other}")),
    };
    if allowed {
        Ok(())
    } else {
        Err(format!("role {role} may not run this statement"))
    }
}

/// Connection settings for [`PgExec`].
#[derive(Clone, Debug)]
pub struct PgOptions {
    /// Maximum pooled connections (`VIBE_DB_POOL_SIZE`, default 16).
    pub pool_size: usize,
    /// Per-request `statement_timeout` (`VIBE_DB_STATEMENT_TIMEOUT_MS`,
    /// default 5000). Applied with `SET LOCAL` inside each request transaction.
    pub statement_timeout_ms: u64,
    /// How long a request waits for a free connection
    /// (`VIBE_DB_POOL_TIMEOUT_MS`, default 5000).
    pub pool_timeout_ms: u64,
}

impl Default for PgOptions {
    fn default() -> Self {
        Self {
            pool_size: 16,
            statement_timeout_ms: 5_000,
            pool_timeout_ms: 5_000,
        }
    }
}

impl PgOptions {
    pub fn from_env() -> Result<Self, String> {
        let mut out = Self::default();
        let num = |name: &str| -> Result<Option<u64>, String> {
            match std::env::var(name) {
                Ok(v) if !v.trim().is_empty() => v
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .map(Some)
                    .ok_or_else(|| format!("{name} must be a positive integer")),
                _ => Ok(None),
            }
        };
        if let Some(n) = num("VIBE_DB_POOL_SIZE")? {
            out.pool_size = n as usize;
        }
        if let Some(n) = num("VIBE_DB_STATEMENT_TIMEOUT_MS")? {
            out.statement_timeout_ms = n;
        }
        if let Some(n) = num("VIBE_DB_POOL_TIMEOUT_MS")? {
            out.pool_timeout_ms = n;
        }
        Ok(out)
    }
}

/// TLS decision for a connection URL. `sslmode=disable` is plain text;
/// `require`/`verify-ca`/`verify-full` use TLS with certificate and host
/// verification (webpki roots). Without `sslmode` (or `prefer`/`allow`),
/// loopback and Unix-socket hosts are plain text and any other host requires
/// TLS, so a remote database is never reached unencrypted by accident.
#[derive(Debug, PartialEq, Eq)]
pub enum TlsMode {
    Disable,
    Require,
}

/// Parses a URL into a tokio-postgres config, ignoring `sslmode` and
/// `sslrootcert` (which this module handles; tokio-postgres rejects
/// `verify-ca`/`verify-full` and does not know `sslrootcert`).
fn pg_config(url: &str) -> Result<tokio_postgres::Config, String> {
    let cleaned = match url::Url::parse(url) {
        Ok(mut u) if u.query().is_some() => {
            let pairs: Vec<(String, String)> = u
                .query_pairs()
                .filter(|(k, _)| k != "sslmode" && k != "sslrootcert")
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            if pairs.is_empty() {
                u.set_query(None);
            } else {
                u.query_pairs_mut().clear().extend_pairs(pairs);
            }
            u.to_string()
        }
        _ => url.to_string(),
    };
    let mut config: tokio_postgres::Config = cleaned
        .parse()
        .map_err(|err| format!("invalid database URL: {err}"))?;
    config.ssl_mode(tokio_postgres::config::SslMode::Disable);
    Ok(config)
}

/// `sslrootcert=<path>`: a PEM file of CA certificates to trust instead of
/// the webpki roots (for a private CA). Hostname verification still applies.
fn ssl_root_cert(url: &str) -> Option<String> {
    url::Url::parse(url).ok().and_then(|u| {
        u.query_pairs()
            .find(|(k, _)| k == "sslrootcert")
            .map(|(_, v)| v.into_owned())
    })
}

fn root_store(url: &str) -> Result<rustls::RootCertStore, String> {
    use rustls::pki_types::{pem::PemObject, CertificateDer};
    let Some(path) = ssl_root_cert(url) else {
        return Ok(rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        });
    };
    let mut roots = rustls::RootCertStore::empty();
    for cert in
        CertificateDer::pem_file_iter(&path).map_err(|err| format!("sslrootcert {path}: {err}"))?
    {
        let cert = cert.map_err(|err| format!("sslrootcert {path}: {err}"))?;
        roots
            .add(cert)
            .map_err(|err| format!("sslrootcert {path}: {err}"))?;
    }
    if roots.is_empty() {
        return Err(format!("sslrootcert {path}: no certificates found"));
    }
    Ok(roots)
}

pub fn tls_mode(url: &str) -> Result<TlsMode, String> {
    let config = pg_config(url)?;
    let sslmode = url::Url::parse(url).ok().and_then(|u| {
        u.query_pairs()
            .find(|(k, _)| k == "sslmode")
            .map(|(_, v)| v.to_ascii_lowercase())
    });
    match sslmode.as_deref() {
        Some("disable") => return Ok(TlsMode::Disable),
        Some("require" | "verify-ca" | "verify-full") => return Ok(TlsMode::Require),
        Some("prefer" | "allow") | None => {}
        Some(other) => return Err(format!("unsupported sslmode {other}")),
    }
    let local = config.get_hosts().iter().all(|host| match host {
        tokio_postgres::config::Host::Tcp(name) => {
            name == "localhost"
                || name
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        }
        #[cfg(unix)]
        tokio_postgres::config::Host::Unix(_) => true,
    });
    Ok(if local {
        TlsMode::Disable
    } else {
        TlsMode::Require
    })
}

/// A pool of PostgreSQL connections. Requests no longer share one connection,
/// so a slow query only holds its own connection, and each request
/// transaction has a `statement_timeout`.
pub struct PgExec {
    pool: deadpool_postgres::Pool,
    options: PgOptions,
}

impl PgExec {
    pub async fn connect(url: &str) -> Result<Self, String> {
        Self::connect_with(url, PgOptions::default()).await
    }

    pub async fn connect_with(url: &str, options: PgOptions) -> Result<Self, String> {
        let config = pg_config(url)?;
        let manager_config = deadpool_postgres::ManagerConfig {
            // A connection can come back mid-transaction (a dropped session, a
            // cancelled request). Roll back and reset session state before
            // reuse; a connection where this fails is discarded.
            recycling_method: deadpool_postgres::RecyclingMethod::Custom(RECYCLE_SQL.into()),
        };
        let manager = match tls_mode(url)? {
            TlsMode::Disable => deadpool_postgres::Manager::from_config(
                config,
                tokio_postgres::NoTls,
                manager_config,
            ),
            TlsMode::Require => {
                // pg_config leaves ssl_mode at Disable, under which
                // tokio-postgres never starts TLS even with a TLS connector.
                let mut config = config;
                config.ssl_mode(tokio_postgres::config::SslMode::Require);
                let roots = root_store(url)?;
                let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .map_err(|err| err.to_string())?
                .with_root_certificates(roots)
                .with_no_client_auth();
                deadpool_postgres::Manager::from_config(
                    config,
                    tokio_postgres_rustls::MakeRustlsConnect::new(tls),
                    manager_config,
                )
            }
        };
        let pool = deadpool_postgres::Pool::builder(manager)
            .max_size(options.pool_size)
            .wait_timeout(Some(std::time::Duration::from_millis(
                options.pool_timeout_ms,
            )))
            .runtime(deadpool_postgres::Runtime::Tokio1)
            .build()
            .map_err(|err| err.to_string())?;
        // Fail at startup, not on the first request.
        drop(pool.get().await.map_err(pool_error)?);
        Ok(Self { pool, options })
    }

    /// The connection pool (shared with the challenge replay store).
    pub fn pool(&self) -> deadpool_postgres::Pool {
        self.pool.clone()
    }

    async fn client(&self) -> Result<deadpool_postgres::Object, String> {
        self.pool.get().await.map_err(pool_error)
    }

    /// One pooled connection held for several calls (transactions, session
    /// settings). Returned to the pool on drop.
    pub async fn session(&self) -> Result<PgSession, String> {
        Ok(PgSession {
            client: Some(self.client().await?),
            in_transaction: false,
        })
    }

    /// Refuses a connection role that can bypass row-level security: a
    /// superuser, a role with BYPASSRLS, or a role that owns (directly or via
    /// membership) any tenant schema or table. Spec §7.1: the connection role
    /// must not own the tables and must not have BYPASSRLS.
    pub async fn check_runtime_role(&self) -> Result<(), String> {
        let client = self.client().await?;
        let row = client
            .query_one(ROLE_STATUS_SQL, &[])
            .await
            .map_err(pg_error)?;
        let roles: Vec<String> = super::grants::RUNTIME_ROLES
            .iter()
            .map(|r| r.to_string())
            .collect();
        let missing: Vec<String> = client
            .query(MISSING_ROLES_SQL, &[&roles])
            .await
            .map_err(pg_error)?
            .iter()
            .map(|row| row.get(0))
            .collect();
        drop(client);
        let status = RoleStatus {
            name: row.get(0),
            superuser_via: row.get(1),
            bypass_rls_via: row.get(2),
            owned_relations: row.get(3),
            owned_functions: row.get(4),
            owned_types: row.get(5),
            owned_schemas: row.get(6),
            unprotected_relations: row.get(7),
            foreign_policies: row.get(8),
        };
        let mut problems = status.problems();
        if !missing.is_empty() {
            problems.push(format!(
                "cannot SET ROLE to {} (create them NOLOGIN and GRANT them to the runtime login)",
                missing.join(", ")
            ));
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "refusing to serve: database role `{}` {}; connect with a non-owner role without BYPASSRLS that can SET ROLE to app_rw, public_reader and public_writer (use VIBE_MIGRATE_DATABASE_URL for `vibe migrate`)",
                status.name,
                problems.join(", ")
            ))
        }
    }

    /// Runs exactly one SQL command over the extended protocol; the server
    /// rejects text that contains more than one.
    pub async fn execute_one(&self, statement: &str) -> Result<(), String> {
        let client = self.client().await?;
        client.execute(statement, &[]).await.map_err(pg_error)?;
        Ok(())
    }

    /// Runs a query returning one text column.
    pub async fn query_texts(&self, sql: &str, param: &str) -> Result<Vec<String>, String> {
        let client = self.client().await?;
        let rows = client.query(sql, &[&param]).await.map_err(pg_error)?;
        Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }

    /// Runs statements in order on one connection (session settings such as
    /// `SET search_path` carry across the slice, not across calls).
    pub async fn batch(&self, statements: &[String]) -> Result<(), String> {
        let client = self.client().await?;
        for statement in statements {
            client.batch_execute(statement).await.map_err(pg_error)?;
        }
        Ok(())
    }
}

/// A single pinned connection from [`PgExec::session`].
///
/// While [`in_transaction`](Self::in_transaction) is set (between `BEGIN`
/// and COMMIT/ROLLBACK), dropping the session — an error path that skipped
/// ROLLBACK, a panic, or a cancelled future — detaches the connection from
/// the pool and closes it, so the server rolls back and no half-done
/// transaction is ever handed out again. Otherwise it returns to the pool,
/// which also rolls back and resets state on recycle.
pub struct PgSession {
    client: Option<deadpool_postgres::Object>,
    in_transaction: bool,
}

impl Drop for PgSession {
    fn drop(&mut self) {
        if self.in_transaction {
            if let Some(object) = self.client.take() {
                drop(deadpool_postgres::Object::take(object));
            }
        }
    }
}

impl std::ops::Deref for PgSession {
    type Target = deadpool_postgres::Object;
    fn deref(&self) -> &Self::Target {
        self.client.as_ref().expect("session client")
    }
}

impl PgSession {
    /// Marks whether a transaction is open on this session (see the type
    /// docs for what happens on drop).
    pub fn in_transaction(&mut self, open: bool) {
        self.in_transaction = open;
    }

    /// Runs exactly one SQL command over the extended protocol; the server
    /// rejects text that contains more than one.
    pub async fn execute_one(&self, statement: &str) -> Result<(), String> {
        (**self).execute(statement, &[]).await.map_err(pg_error)?;
        Ok(())
    }

    /// Runs a query returning one text column.
    pub async fn query_texts(&self, sql: &str, param: &str) -> Result<Vec<String>, String> {
        let rows = (**self).query(sql, &[&param]).await.map_err(pg_error)?;
        Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }

    pub async fn batch(&self, statements: &[String]) -> Result<(), String> {
        for statement in statements {
            (**self).batch_execute(statement).await.map_err(pg_error)?;
        }
        Ok(())
    }
}

#[async_trait]
impl QueryExec for PgExec {
    async fn execute(
        &self,
        tenant_id: &str,
        role: &str,
        stmt: Stmt,
    ) -> Result<Vec<Row>, ExecError> {
        if !super::slugs::valid_tenant(tenant_id) {
            return Err(ExecError::Message("invalid tenant id".into()));
        }
        // `role` is the logical caller class. It limits which statements may
        // run; the database additionally enforces the non-owner connection
        // role (checked by `check_runtime_role`) and its migrate-time grants.
        role_allows(role, &stmt).map_err(role_error)?;
        reject_tenant_input(&stmt)?;
        let mut stmt = stmt;
        if let Stmt::Insert { values, .. } = &mut stmt {
            values.push((
                Col {
                    name: "tenant_id".into(),
                    ty: "string".into(),
                },
                Value::String(tenant_id.into()),
            ));
        }
        let (sql, params) = sql::render(&stmt).map_err(ExecError::Message)?;
        let set_role = format!(
            "SET LOCAL ROLE {}",
            sql::quote(role).map_err(ExecError::Message)?
        );
        let returning = returning_cols(&stmt);
        let mut client = self
            .client()
            .await
            .map_err(|err| ExecError::Message(format!("database unavailable: {err}")))?;
        let tx = client
            .transaction()
            .await
            .map_err(|err| ExecError::Message(pg_error(err)))?;
        let schema = format!("\"t_{tenant_id}\"");
        tx.query(
            "SELECT set_config('app.tenant_id', $1, true), set_config('search_path', $2, true), \
             set_config('statement_timeout', $3, true)",
            &[
                &tenant_id,
                &schema,
                &self.options.statement_timeout_ms.to_string(),
            ],
        )
        .await
        .map_err(|err| ExecError::Message(pg_error(err)))?;
        // Statements run as the logical role (a NOLOGIN role the runtime
        // login is a member of), so its table and column grants apply.
        tx.batch_execute(&set_role).await.map_err(map_pg)?;
        let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p as _).collect();
        let rows = tx.query(&sql, &refs).await.map_err(map_pg)?;
        tx.commit()
            .await
            .map_err(|err| ExecError::Message(pg_error(err)))?;
        if rows.is_empty() {
            return match stmt {
                Stmt::SoftDelete { .. } => Err(ExecError::NotFound),
                Stmt::Update { .. } => Err(ExecError::Precondition),
                _ => Ok(Vec::new()),
            };
        }
        let mut out = Vec::new();
        for row in rows {
            let mut mapped = Map::new();
            for (idx, col) in returning.iter().enumerate() {
                let text: Option<String> = row
                    .try_get(idx)
                    .map_err(|err| ExecError::Message(pg_error(err)))?;
                mapped.insert(col.name.clone(), parse_cell(text, &col.ty));
            }
            out.push(mapped);
        }
        Ok(out)
    }

    async fn raw(
        &self,
        tenant_id: &str,
        role: &str,
        writes: bool,
        sql: &str,
        params: Vec<Option<String>>,
    ) -> Result<Vec<Row>, ExecError> {
        if !QUERY_ROLES.contains(&role) {
            return Err(ExecError::Message(format!("query cannot run as {role}")));
        }
        self.run_tx(tenant_id, Some(role), writes, sql, params)
            .await
    }

    async fn user_roles(
        &self,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<Option<Vec<String>>, ExecError> {
        let rows = self
            .run_tx(
                tenant_id,
                None,
                false,
                "SELECT roles FROM users WHERE id::text = $1",
                vec![Some(user_id.to_string())],
            )
            .await?;
        match rows.first() {
            None => Ok(None),
            Some(row) => roles_from(row.get("roles")).map(Some),
        }
    }
}

impl PgExec {
    /// One transaction in the tenant schema; `role` switches to a query role
    /// with `SET LOCAL ROLE`; without `writes` it is READ ONLY.
    async fn run_tx(
        &self,
        tenant_id: &str,
        role: Option<&str>,
        writes: bool,
        sql: &str,
        params: Vec<Option<String>>,
    ) -> Result<Vec<Row>, ExecError> {
        if !super::slugs::valid_tenant(tenant_id) {
            return Err(ExecError::Message("invalid tenant id".into()));
        }
        let mut client = self.client().await.map_err(ExecError::Message)?;
        let tx = client
            .transaction()
            .await
            .map_err(|err| ExecError::Message(err.to_string()))?;
        if !writes {
            tx.batch_execute("SET TRANSACTION READ ONLY")
                .await
                .map_err(|err| ExecError::Message(err.to_string()))?;
        }
        let schema = format!("\"t_{tenant_id}\"");
        tx.query(
            "SELECT set_config('app.tenant_id', $1, true), set_config('search_path', $2, true)",
            &[
                &tenant_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &schema as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|err| ExecError::Message(err.to_string()))?;
        if let Some(role) = role {
            // `role` is one of QUERY_ROLES (checked by the caller).
            tx.batch_execute(&format!("SET LOCAL ROLE {role}"))
                .await
                .map_err(|err| ExecError::Message(format!("SET ROLE {role}: {err}")))?;
        }
        let params: Vec<TextParam> = params.into_iter().map(TextParam).collect();
        let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p as _).collect();
        let body = sql.trim().trim_end_matches(';');
        let rows = if crate::sqlguard::returns_rows(body) {
            // Every column comes back as JSON, whatever its type.
            let wrapped = crate::sqlguard::wrap_for_rows(body);
            if role.is_some() {
                // Query steps: check the text that is actually executed.
                if let Some(problem) = crate::sqlguard::wrapped_problem(&wrapped) {
                    return Err(ExecError::Message(problem));
                }
            }
            tx.query(&wrapped, &refs).await.map_err(map_pg)?
        } else {
            if role.is_some() {
                if let Some(problem) = crate::sqlguard::query_problem(body) {
                    return Err(ExecError::Message(problem));
                }
            }
            tx.execute(body, &refs).await.map_err(map_pg)?;
            Vec::new()
        };
        tx.commit()
            .await
            .map_err(|err| ExecError::Message(err.to_string()))?;
        rows.iter()
            .map(|row| {
                let text: String = row.get(0);
                match serde_json::from_str(&text) {
                    Ok(Value::Object(map)) => Ok(map),
                    _ => Err(ExecError::Message("query row is not an object".into())),
                }
            })
            .collect()
    }
}

fn returning_cols(stmt: &Stmt) -> Vec<Col> {
    match stmt {
        Stmt::Insert { returning, .. } | Stmt::Update { returning, .. } => returning.clone(),
        Stmt::Select { columns, .. } => columns.clone(),
        Stmt::SoftDelete { .. } => vec![Col {
            name: "id".into(),
            ty: "uuid".into(),
        }],
    }
}

fn parse_cell(text: Option<String>, ty: &str) -> Value {
    let Some(text) = text else {
        return Value::Null;
    };
    match ty {
        "integer" => text
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or(Value::String(text)),
        "number" => text
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::String(text)),
        "boolean" => Value::Bool(text == "true" || text == "t"),
        _ => Value::String(text),
    }
}

fn map_pg(err: tokio_postgres::Error) -> ExecError {
    let code = err.code().map(|code| code.code());
    if code == Some("23505") {
        ExecError::Conflict
    } else if code == Some("42501") {
        ExecError::Denied(pg_error(err))
    } else {
        ExecError::Message(pg_error(err))
    }
}

/// Run on every connection before it is handed out again.
const RECYCLE_SQL: &str = "ROLLBACK; CLOSE ALL; SET SESSION AUTHORIZATION DEFAULT; RESET ALL; \
     UNLISTEN *; SELECT pg_advisory_unlock_all(); DISCARD TEMP; DISCARD SEQUENCES";

/// A pool error; connection failures keep the server's detail.
fn pool_error(err: deadpool_postgres::PoolError) -> String {
    match err {
        deadpool_postgres::PoolError::Backend(err) => pg_error(err),
        other => other.to_string(),
    }
}

/// A Postgres error with the server's message, detail, hint and SQLSTATE
/// (tokio-postgres's `Display` only says "db error"). For logs and the CLI;
/// HTTP responses never include it.
pub fn pg_error(err: tokio_postgres::Error) -> String {
    let Some(db) = err.as_db_error() else {
        return err.to_string();
    };
    let mut out = format!("{}: {} ({})", db.severity(), db.message(), db.code().code());
    if let Some(detail) = db.detail() {
        out.push_str(&format!("; detail: {detail}"));
    }
    if let Some(hint) = db.hint() {
        out.push_str(&format!("; hint: {hint}"));
    }
    if let Some(schema) = db.schema() {
        out.push_str(&format!("; schema: {schema}"));
    }
    if let Some(table) = db.table() {
        out.push_str(&format!("; table: {table}"));
    }
    out
}

pub fn etag(id: &str, updated_at: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(id.as_bytes());
    hasher.update(b"|");
    hasher.update(updated_at.as_bytes());
    format!("\"{}\"", hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod role_tests {
    use super::*;

    #[test]
    fn safe_role_has_no_problems() {
        assert!(RoleStatus::default().problems().is_empty());
    }

    #[test]
    fn unsafe_roles_are_reported() {
        let s = RoleStatus {
            name: "app".into(),
            superuser_via: vec!["postgres".into()],
            bypass_rls_via: vec!["etl".into()],
            owned_relations: 2,
            owned_functions: 1,
            owned_types: 1,
            owned_schemas: 1,
            unprotected_relations: vec!["t_x.later".into()],
            foreign_policies: vec!["t_x.notes:open".into()],
        };
        let p = s.problems();
        assert_eq!(p.len(), 8, "{p:?}");
        assert!(p[6].contains("t_x.later"));
        assert!(p[7].contains("t_x.notes:open") && p[7].contains("OR-combined"));
        assert!(p[0].contains("postgres"));
        assert!(p[1].contains("etl"));
    }

    fn select() -> Stmt {
        Stmt::Select {
            table: "t".into(),
            columns: vec![],
            id: None,
            filters: vec![],
            sort: None,
            limit: 1,
            offset: 0,
        }
    }

    #[test]
    fn executor_role_is_enforced() {
        assert!(role_allows("public_reader", &select()).is_ok());
        assert!(role_allows("app_rw", &select()).is_ok());
        assert!(role_allows("bogus", &select()).is_err());
        let insert = Stmt::Insert {
            table: "t".into(),
            values: vec![],
            returning: vec![],
        };
        assert!(role_allows("public_reader", &insert).is_err());
        assert!(role_allows("public_writer", &insert).is_ok());
    }
}

#[cfg(test)]
mod tls_tests {
    use super::{tls_mode, PgOptions, TlsMode};

    #[test]
    fn remote_hosts_require_tls_unless_disabled() {
        let m = |u: &str| tls_mode(u).unwrap();
        assert_eq!(m("postgres://u@localhost/db"), TlsMode::Disable);
        assert_eq!(m("postgres://u@127.0.0.1/db"), TlsMode::Disable);
        assert_eq!(m("postgres://u@db.example.com/db"), TlsMode::Require);
        assert_eq!(
            m("postgres://u@db.example.com/db?sslmode=prefer"),
            TlsMode::Require
        );
        assert_eq!(
            m("postgres://u@db.example.com/db?sslmode=disable"),
            TlsMode::Disable
        );
        assert_eq!(
            m("postgres://u@localhost/db?sslmode=verify-full"),
            TlsMode::Require
        );
        assert!(tls_mode("postgres://u@localhost/db?sslmode=bogus").is_err());
    }

    #[test]
    fn options_have_safe_defaults() {
        let o = PgOptions::default();
        assert_eq!(o.pool_size, 16);
        assert_eq!(o.statement_timeout_ms, 5_000);
        assert_eq!(o.pool_timeout_ms, 5_000);
    }
}
