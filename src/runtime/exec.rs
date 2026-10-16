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
}

/// What a tenant login role can do, as read from the catalog while logged
/// in as it (decision 97). A safe role has no problems: it is exactly the
/// expected `tenant_<id>_<class>` login, NOINHERIT, has no role memberships
/// in either direction (so `SET ROLE` can reach nothing), owns nothing, and
/// can reach no relation or schema outside its own tenant schema.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RoleStatus {
    /// `current_user` of the connection.
    pub name: String,
    /// `session_user` of the connection.
    pub session_user: String,
    /// The role name the connection must be logged in as.
    pub expected: String,
    pub superuser: bool,
    pub bypass_rls: bool,
    /// INHERIT instead of NOINHERIT.
    pub inherit: bool,
    /// `member of X` / `has member Y`.
    pub memberships: Vec<String>,
    /// Schemas, relations, functions and types it owns (anywhere).
    pub owned_objects: i64,
    /// Relations and schemas outside its own tenant schema it can use
    /// (other tenants, platform/shared tables in `public`), and any schema
    /// it can CREATE in.
    pub foreign_access: Vec<String>,
    /// Relations in its own schema that are not tables with ENABLE + FORCE
    /// row level security (whether or not it can access them today), or
    /// where it holds DELETE, TRUNCATE, REFERENCES or TRIGGER.
    pub unprotected_relations: Vec<String>,
    /// Policies in its own schema other than the canonical
    /// `tenant_isolation` for this tenant.
    pub foreign_policies: Vec<String>,
}

impl RoleStatus {
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.name != self.expected || self.session_user != self.expected {
            out.push(format!(
                "is logged in as `{}` (current user `{}`), not the tenant role `{}`",
                self.session_user, self.name, self.expected
            ));
        }
        if self.superuser {
            out.push("is a superuser".into());
        }
        if self.bypass_rls {
            out.push("has BYPASSRLS".into());
        }
        if self.inherit {
            out.push("is INHERIT (create it NOINHERIT)".into());
        }
        if !self.memberships.is_empty() {
            out.push(format!(
                "has role memberships ({}); a tenant role must not be able to SET ROLE to anything",
                self.memberships.join(", ")
            ));
        }
        if self.owned_objects > 0 {
            out.push(format!("owns {} database object(s)", self.owned_objects));
        }
        if !self.foreign_access.is_empty() {
            out.push(format!(
                "can reach objects outside its own tenant schema ({})",
                self.foreign_access.join(", ")
            ));
        }
        if !self.unprotected_relations.is_empty() {
            out.push(format!(
                "can access relation(s) without ENABLE + FORCE ROW LEVEL SECURITY or with a forbidden privilege ({}); re-run `vibe migrate`",
                self.unprotected_relations.join(", ")
            ));
        }
        if !self.foreign_policies.is_empty() {
            out.push(format!(
                "tenant table(s) carry policies other than the canonical tenant_isolation ({}); drop them and re-run `vibe migrate`",
                self.foreign_policies.join(", ")
            ));
        }
        out
    }
}

/// Catalog query behind [`PgExec::check_tenant_role`], run while logged in
/// as the tenant role. `$1` is the tenant id, `$2` the expected role name.
pub fn tenant_role_status_sql() -> String {
    format!(
        r#"SELECT current_user::text, session_user::text, $2::text, r.rolsuper, r.rolbypassrls, r.rolinherit,
 COALESCE((SELECT array_agg(x ORDER BY x) FROM (
   SELECT 'member of ' || g.rolname::text AS x FROM pg_auth_members m
     JOIN pg_roles g ON g.oid = m.roleid WHERE m.member = r.oid
   UNION ALL
   SELECT 'has member ' || u.rolname::text FROM pg_auth_members m
     JOIN pg_roles u ON u.oid = m.member WHERE m.roleid = r.oid) y), '{{}}'),
 (SELECT count(*) FROM pg_class WHERE relowner = r.oid)
   + (SELECT count(*) FROM pg_proc WHERE proowner = r.oid)
   + (SELECT count(*) FROM pg_type WHERE typowner = r.oid)
   + (SELECT count(*) FROM pg_namespace WHERE nspowner = r.oid),
 COALESCE((SELECT array_agg(x ORDER BY x) FROM (
   SELECT format('schema %I', n.nspname) AS x FROM pg_namespace n
     WHERE n.nspname LIKE 't\_%' AND n.nspname <> 't_' || $1::text
     AND has_schema_privilege(r.oid, n.oid, 'USAGE')
   UNION ALL
   SELECT format('CREATE on schema %I', n.nspname) FROM pg_namespace n
     WHERE n.nspname NOT LIKE 'pg\_%' AND n.nspname <> 'information_schema'
     AND has_schema_privilege(r.oid, n.oid, 'CREATE')
   UNION ALL
   SELECT format('%I.%I', n.nspname, c.relname) FROM pg_class c
     JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname NOT LIKE 'pg\_%' AND n.nspname <> 'information_schema'
     AND n.nspname <> 't_' || $1::text AND c.relkind IN ('r','p','v','m','f','S')
     AND (has_table_privilege(r.oid, c.oid, 'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER')
       OR (c.relkind <> 'S' AND has_any_column_privilege(r.oid, c.oid, 'SELECT,INSERT,UPDATE,REFERENCES')))
   ) y), '{{}}'),
 COALESCE((SELECT array_agg(format('%I.%I', n.nspname, c.relname) ORDER BY 1)
   FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE n.nspname = 't_' || $1::text AND c.relkind IN ('r','p','v','m','f')
   AND (NOT (c.relkind IN ('r','p') AND c.relrowsecurity AND c.relforcerowsecurity)
     OR has_table_privilege(r.oid, c.oid, 'DELETE,TRUNCATE,REFERENCES,TRIGGER'))), '{{}}'),
 COALESCE((SELECT array_agg(format('%I.%I:%I', n.nspname, c.relname, p.polname) ORDER BY 1)
   FROM pg_policy p JOIN pg_class c ON c.oid = p.polrelid
   JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE n.nspname = 't_' || $1::text
   AND NOT (p.polname = 'tenant_isolation' AND {canonical})), '{{}}')
FROM pg_roles r WHERE r.rolname = current_user"#,
        canonical = super::migrate::canonical_policy_sql("p", "$1::text")
    )
}

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

/// Parses a URL into a tokio-postgres config, ignoring `sslmode` (which this
/// module handles; tokio-postgres rejects `verify-ca`/`verify-full`).
fn pg_config(url: &str) -> Result<tokio_postgres::Config, String> {
    let cleaned = match url::Url::parse(url) {
        Ok(mut u) if u.query().is_some() => {
            let pairs: Vec<(String, String)> = u
                .query_pairs()
                .filter(|(k, _)| k != "sslmode")
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
                let roots = rustls::RootCertStore {
                    roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
                };
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

    /// Refuses to serve `tenant`/`class` over this pool unless the login is
    /// exactly that tenant's role and can reach only its own tenant schema
    /// (decision 97, spec §7.1): not a superuser, no BYPASSRLS, NOINHERIT,
    /// no role memberships either way (so `SET ROLE` reaches nothing), owns
    /// nothing, no privileges outside `t_<tenant>`, and only RLS-forced
    /// tables with the canonical policy inside it.
    pub async fn check_tenant_role(&self, tenant: &str, class: &str) -> Result<(), String> {
        let expected = super::grants::tenant_role(tenant, class)?;
        let client = self.client().await?;
        let rows = client
            .query(&tenant_role_status_sql(), &[&tenant, &expected])
            .await
            .map_err(pg_error)?;
        drop(client);
        let Some(row) = rows.first() else {
            return Err(format!("refusing to serve: cannot read role `{expected}`"));
        };
        let status = RoleStatus {
            name: row.get(0),
            session_user: row.get(1),
            expected: row.get(2),
            superuser: row.get(3),
            bypass_rls: row.get(4),
            inherit: row.get(5),
            memberships: row.get(6),
            owned_objects: row.get(7),
            foreign_access: row.get(8),
            unprotected_relations: row.get(9),
            foreign_policies: row.get(10),
        };
        let problems = status.problems();
        if problems.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "refusing to serve tenant {tenant}: database role `{expected}` {}",
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
        // run; the database additionally enforces the tenant login role's
        // migrate-time grants and the tenant policy.
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
        let login = super::grants::tenant_role(tenant_id, role).map_err(ExecError::Message)?;
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
        // `app.tenant_id` is only for logs and diagnostics (decision 97); the
        // tenant boundary is the login role and its own schema.
        let who = tx
            .query_one(
                "SELECT session_user::text, current_user::text, \
                 set_config('app.tenant_id', $1, true), set_config('search_path', $2, true), \
                 set_config('statement_timeout', $3, true)",
                &[
                    &tenant_id,
                    &schema,
                    &self.options.statement_timeout_ms.to_string(),
                ],
            )
            .await
            .map_err(|err| ExecError::Message(pg_error(err)))?;
        // There is no SET ROLE: the pool must be logged in as exactly this
        // tenant's role for this class, or nothing runs.
        let (session, current): (String, String) = (who.get(0), who.get(1));
        if session != login || current != login {
            return Err(ExecError::Message(format!(
                "connection is logged in as `{session}`, not tenant role `{login}`; refusing to run"
            )));
        }
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
        let s = RoleStatus {
            name: "tenant_acme0001_rw".into(),
            session_user: "tenant_acme0001_rw".into(),
            expected: "tenant_acme0001_rw".into(),
            ..RoleStatus::default()
        };
        assert!(s.problems().is_empty());
    }

    #[test]
    fn unsafe_roles_are_reported() {
        let s = RoleStatus {
            name: "app".into(),
            session_user: "app".into(),
            expected: "tenant_acme0001_rw".into(),
            superuser: true,
            bypass_rls: true,
            inherit: true,
            memberships: vec!["member of app_rw".into()],
            owned_objects: 2,
            foreign_access: vec!["schema t_other001".into()],
            unprotected_relations: vec!["t_acme0001.later".into()],
            foreign_policies: vec!["t_acme0001.notes:open".into()],
        };
        let p = s.problems();
        assert_eq!(p.len(), 9, "{p:?}");
        assert!(p[0].contains("tenant_acme0001_rw"));
        assert!(p[4].contains("member of app_rw") && p[4].contains("SET ROLE"));
        assert!(p[6].contains("t_other001"));
        assert!(p[7].contains("t_acme0001.later"));
        assert!(p[8].contains("t_acme0001.notes:open"));
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
