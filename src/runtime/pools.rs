//! One connection pool per (tenant, role class), each logged in directly as
//! that tenant's `LOGIN` role (decision 97). There is no shared runtime
//! login and no `SET ROLE`: a tenant connection holds no identity it could
//! switch back to, so it can only ever reach its own tenant schema.
//!
//! Credentials come from a [`TenantSecretSource`]. Production uses a secret
//! manager reference (`VIBE_TENANT_DB_SECRET_REF`); the JSON password file
//! (`VIBE_TENANT_DB_SECRETS`) is for development and tests only and is
//! refused in production. Secrets are never logged or put in errors.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use super::exec::{ExecError, PgExec, PgOptions, QueryExec, Row};
use super::grants::{tenant_role, RUNTIME_ROLES};
use super::sql::Stmt;

/// Spec §7.1 / D129: at most 5 connections per (tenant, role) pool.
pub const MAX_TENANT_POOL_SIZE: usize = 5;

/// D129: at most 15 connections in use at once per tenant, across its roles.
pub const MAX_TENANT_CONNECTIONS: usize = 15;

/// Default process-wide cap on database connections in use at once, across
/// every tenant pool (`VIBE_DB_POOL_MAX_TOTAL`).
pub const DEFAULT_MAX_CONNECTIONS: usize = 200;

/// Environment variable for the process-wide connection cap.
pub const POOL_MAX_TOTAL_ENV: &str = "VIBE_DB_POOL_MAX_TOTAL";

/// Default dev/test password file looked for under the project; its mere
/// presence refuses a production start.
pub const DEV_SECRETS_FILE: &str = ".vibe/tenant-db-secrets.json";

/// Which connection limit refused a request (`scope` label of
/// `vibe_db_pool_exhausted_total`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolScope {
    /// 5 per (tenant, role).
    Role,
    /// 15 per tenant.
    Tenant,
    /// `VIBE_DB_POOL_MAX_TOTAL` per process.
    Process,
}

impl PoolScope {
    pub const ALL: [PoolScope; 3] = [PoolScope::Role, PoolScope::Tenant, PoolScope::Process];

    pub fn as_str(self) -> &'static str {
        match self {
            PoolScope::Role => "role",
            PoolScope::Tenant => "tenant",
            PoolScope::Process => "process",
        }
    }

    fn counter(self) -> &'static AtomicU64 {
        match self {
            PoolScope::Role => &EXHAUSTED_ROLE,
            PoolScope::Tenant => &EXHAUSTED_TENANT,
            PoolScope::Process => &EXHAUSTED_PROCESS,
        }
    }
}

/// Requests refused with `DB_POOL_EXHAUSTED` since process start, per scope.
static EXHAUSTED_ROLE: AtomicU64 = AtomicU64::new(0);
static EXHAUSTED_TENANT: AtomicU64 = AtomicU64::new(0);
static EXHAUSTED_PROCESS: AtomicU64 = AtomicU64::new(0);

/// Metric `vibe_db_pool_exhausted_total{scope}` for one scope.
pub fn pool_exhausted_count(scope: PoolScope) -> u64 {
    scope.counter().load(Ordering::Relaxed)
}

/// Sum of `vibe_db_pool_exhausted_total` over every scope.
pub fn pool_exhausted_total() -> u64 {
    PoolScope::ALL
        .iter()
        .map(|s| pool_exhausted_count(*s))
        .sum()
}

/// `vibe_db_pool_exhausted_total` in the Prometheus text format.
pub fn pool_exhausted_metrics() -> String {
    let mut out = String::from(
        "# HELP vibe_db_pool_exhausted_total Requests refused with 503 DB_POOL_EXHAUSTED.\n\
         # TYPE vibe_db_pool_exhausted_total counter\n",
    );
    for scope in PoolScope::ALL {
        out.push_str(&format!(
            "vibe_db_pool_exhausted_total{{scope=\"{}\"}} {}\n",
            scope.as_str(),
            pool_exhausted_count(scope)
        ));
    }
    out
}

/// Parses `VIBE_DB_POOL_MAX_TOTAL` (unset or blank: the default).
pub fn parse_pool_max_total(value: Option<&str>) -> Result<usize, String> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(DEFAULT_MAX_CONNECTIONS),
        Some(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("{POOL_MAX_TOTAL_ENV} must be a positive integer")),
    }
}

/// `(tenant, class)` for a `tenant_<id>_<suffix>` runtime role name.
pub fn parse_tenant_role(name: &str) -> Option<(String, &'static str)> {
    let rest = name.strip_prefix("tenant_")?;
    for class in RUNTIME_ROLES {
        let suffix = match *class {
            "app_rw" => "_rw",
            "public_reader" => "_public_reader",
            _ => "_public_writer",
        };
        if let Some(tenant) = rest.strip_suffix(suffix) {
            if tenant_role(tenant, class).ok().as_deref() == Some(name) {
                return Some((tenant.to_string(), class));
            }
        }
    }
    None
}

/// Where tenant role passwords come from. Implement this for a secret
/// manager; [`EnvSecrets`] is the minimal built-in production source.
pub trait TenantSecretSource: Send + Sync {
    /// Every tenant runtime role this source has credentials for.
    fn roles(&self) -> Result<Vec<String>, String>;
    /// The password for `role` (`Some("")` means no password, e.g. trust).
    /// `None` if the source has no entry. Errors must not contain secrets.
    fn password(&self, role: &str) -> Result<Option<String>, String>;
    /// Short description for logs (never secrets).
    fn describe(&self) -> String;
}

/// Development/test only: role -> password, from a JSON file or tests.
pub struct FileSecrets {
    entries: HashMap<String, String>,
    origin: String,
}

impl FileSecrets {
    pub fn from_map(entries: HashMap<String, String>) -> Self {
        Self {
            entries,
            origin: "in-memory map".into(),
        }
    }

    /// Reads a JSON object `{"tenant_<id>_rw": "password", ...}`. Refuses a
    /// file whose permissions are looser than 0600 (any group/other bit,
    /// or execute for the owner).
    pub fn load(path: &Path) -> Result<Self, String> {
        let shown = path.display();
        let meta = std::fs::metadata(path).map_err(|err| format!("{shown}: {err}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o177 != 0 {
                return Err(format!(
                    "{shown}: permissions are {mode:04o}; the tenant DB password file must be 0600 (chmod 600)"
                ));
            }
        }
        #[cfg(not(unix))]
        let _ = meta;
        let text = std::fs::read_to_string(path).map_err(|err| format!("{shown}: {err}"))?;
        // serde_json errors carry a position, not the content.
        let entries: HashMap<String, String> = serde_json::from_str(&text)
            .map_err(|err| format!("{shown}: not a JSON object of role -> password ({err})"))?;
        Ok(Self {
            entries,
            origin: format!("file {shown} (dev/test only)"),
        })
    }
}

impl TenantSecretSource for FileSecrets {
    fn roles(&self) -> Result<Vec<String>, String> {
        let mut roles: Vec<String> = self.entries.keys().cloned().collect();
        roles.sort();
        Ok(roles)
    }
    fn password(&self, role: &str) -> Result<Option<String>, String> {
        Ok(self.entries.get(role).cloned())
    }
    fn describe(&self) -> String {
        self.origin.clone()
    }
}

/// Production source for `VIBE_TENANT_DB_SECRET_REF=env:<PREFIX>`: the
/// password of role `tenant_x_rw` is in the environment variable
/// `<PREFIX>TENANT_X_RW` (role name upper-cased), as injected by a secret
/// manager (Kubernetes secrets, ECS/Cloud Run secret env, Vault agent, ...).
pub struct EnvSecrets {
    prefix: String,
    vars: HashMap<String, String>,
}

impl EnvSecrets {
    pub fn new(prefix: &str) -> Result<Self, String> {
        Self::from_vars(prefix, std::env::vars())
    }

    pub fn from_vars(
        prefix: &str,
        vars: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, String> {
        if prefix.is_empty()
            || !prefix
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            return Err("VIBE_TENANT_DB_SECRET_REF=env:<PREFIX> needs an upper-case PREFIX of A-Z, 0-9 and _".into());
        }
        let vars = vars
            .into_iter()
            .filter_map(|(k, v)| {
                k.strip_prefix(prefix)
                    .map(|role| (role.to_ascii_lowercase(), v))
            })
            .collect();
        Ok(Self {
            prefix: prefix.to_string(),
            vars,
        })
    }
}

impl TenantSecretSource for EnvSecrets {
    fn roles(&self) -> Result<Vec<String>, String> {
        let mut roles: Vec<String> = self.vars.keys().cloned().collect();
        roles.sort();
        Ok(roles)
    }
    fn password(&self, role: &str) -> Result<Option<String>, String> {
        Ok(self.vars.get(role).cloned())
    }
    fn describe(&self) -> String {
        format!("environment variables {}*", self.prefix)
    }
}

/// Picks the secret source for `vibe serve` (spec decision 97, D129):
/// - `VIBE_TENANT_DB_SECRET_REF=env:<PREFIX>`: [`EnvSecrets`] (production);
/// - `VIBE_TENANT_DB_SECRETS=<path>`: [`FileSecrets`], development/CI only;
/// - in production, a configured password file, or one present at
///   `<project>/.vibe/tenant-db-secrets.json`, refuses the start.
pub fn secret_source_from_env(
    production: bool,
    project: &Path,
) -> Result<Box<dyn TenantSecretSource>, String> {
    let file = std::env::var("VIBE_TENANT_DB_SECRETS")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let reference = std::env::var("VIBE_TENANT_DB_SECRET_REF")
        .ok()
        .filter(|v| !v.trim().is_empty());
    secret_source(production, project, file.as_deref(), reference.as_deref())
}

pub fn secret_source(
    production: bool,
    project: &Path,
    file: Option<&str>,
    reference: Option<&str>,
) -> Result<Box<dyn TenantSecretSource>, String> {
    if production {
        if file.is_some() {
            return Err("refusing to start: VIBE_TENANT_DB_SECRETS (a password file) is for development and tests only; in production set VIBE_TENANT_DB_SECRET_REF to a secret-manager reference".into());
        }
        let default = project.join(DEV_SECRETS_FILE);
        if default.exists() {
            return Err(format!(
                "refusing to start: a tenant DB password file is present at {} in production; remove it and use VIBE_TENANT_DB_SECRET_REF",
                default.display()
            ));
        }
    }
    match (reference, file) {
        (Some(_), Some(_)) => {
            Err("set only one of VIBE_TENANT_DB_SECRET_REF and VIBE_TENANT_DB_SECRETS".into())
        }
        (Some(reference), None) => match reference.split_once(':') {
            Some(("env", prefix)) => Ok(Box::new(EnvSecrets::new(prefix)?)),
            _ => Err("VIBE_TENANT_DB_SECRET_REF: unsupported reference (supported: env:<PREFIX>); other secret managers plug in through the TenantSecretSource trait".into()),
        },
        (None, Some(path)) => Ok(Box::new(FileSecrets::load(Path::new(path))?)),
        (None, None) => Err("tenant DB credentials are not configured: set VIBE_TENANT_DB_SECRET_REF=env:<PREFIX> (production) or VIBE_TENANT_DB_SECRETS=<0600 JSON file> (development/tests); vibe serve logs in as each tenant role directly (decision 97)".into()),
    }
}

/// Lazily opened per-tenant pools under one process-wide connection cap.
pub struct TenantPools {
    /// Server and database; the user and password are replaced per role.
    base_url: String,
    secrets: Box<dyn TenantSecretSource>,
    options: PgOptions,
    /// Connections in use at once across all pools.
    in_use: Arc<tokio::sync::Semaphore>,
    max_connections: usize,
    /// Connections in use per (tenant, role) and per tenant.
    role_slots: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
    tenant_slots: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
    pools: tokio::sync::Mutex<HashMap<String, Arc<PgExec>>>,
}

impl TenantPools {
    /// Test/dev convenience over [`FileSecrets::from_map`].
    pub fn new(
        base_url: &str,
        secrets: HashMap<String, String>,
        options: PgOptions,
    ) -> Result<Self, String> {
        Self::with_source(
            base_url,
            Box::new(FileSecrets::from_map(secrets)),
            options,
            DEFAULT_MAX_CONNECTIONS,
        )
    }

    pub fn with_source(
        base_url: &str,
        secrets: Box<dyn TenantSecretSource>,
        options: PgOptions,
        max_connections: usize,
    ) -> Result<Self, String> {
        url::Url::parse(base_url).map_err(|err| format!("invalid database URL: {err}"))?;
        if let Some(bad) = secrets
            .roles()?
            .into_iter()
            .find(|k| parse_tenant_role(k).is_none())
        {
            return Err(format!(
                "tenant DB secrets ({}): `{bad}` is not a tenant runtime role name (tenant_<id>_rw, tenant_<id>_public_reader, tenant_<id>_public_writer)",
                secrets.describe()
            ));
        }
        if max_connections == 0 {
            return Err(format!("{POOL_MAX_TOTAL_ENV} must be a positive integer"));
        }
        let mut options = options;
        options.pool_size = options.pool_size.min(MAX_TENANT_POOL_SIZE);
        Ok(Self {
            base_url: base_url.to_string(),
            secrets,
            options,
            in_use: Arc::new(tokio::sync::Semaphore::new(max_connections)),
            max_connections,
            role_slots: std::sync::Mutex::default(),
            tenant_slots: std::sync::Mutex::default(),
            pools: tokio::sync::Mutex::default(),
        })
    }

    /// `VIBE_DATABASE_URL` (or `DATABASE_URL`; server and database only),
    /// [`secret_source_from_env`], and `VIBE_DB_POOL_MAX_TOTAL` (default 200).
    pub fn from_env(production: bool, project: &Path) -> Result<Self, String> {
        let url = std::env::var("VIBE_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .map_err(|_| {
                "set VIBE_DATABASE_URL or pass --memory for the dev executor".to_string()
            })?;
        let max = parse_pool_max_total(std::env::var(POOL_MAX_TOTAL_ENV).ok().as_deref())?;
        Self::with_source(
            &url,
            secret_source_from_env(production, project)?,
            PgOptions::from_env()?,
            max,
        )
    }

    fn slot(
        map: &std::sync::Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
        key: String,
        size: usize,
    ) -> Arc<tokio::sync::Semaphore> {
        let mut map = map.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(key)
            .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(size)))
            .clone()
    }

    /// One connection slot for `tenant`/`class` under all three caps, or the
    /// scope of the first cap that is full. Never waits.
    pub fn acquire(
        &self,
        tenant: &str,
        class: &str,
    ) -> Result<[tokio::sync::OwnedSemaphorePermit; 3], PoolScope> {
        // Tenant first: with three role classes of 5, the tenant cap (15)
        // is only reached when every role is full, so checking it first is
        // what lets a full tenant be reported as `scope="tenant"`.
        let tenant_permit = Self::slot(
            &self.tenant_slots,
            tenant.to_string(),
            MAX_TENANT_CONNECTIONS,
        )
        .try_acquire_owned()
        .map_err(|_| PoolScope::Tenant)?;
        let role = Self::slot(
            &self.role_slots,
            format!("{tenant}\u{0}{class}"),
            MAX_TENANT_POOL_SIZE,
        )
        .try_acquire_owned()
        .map_err(|_| PoolScope::Role)?;
        let process = self
            .in_use
            .clone()
            .try_acquire_owned()
            .map_err(|_| PoolScope::Process)?;
        Ok([tenant_permit, role, process])
    }

    pub fn max_connections(&self) -> usize {
        self.max_connections
    }

    pub fn describe_secrets(&self) -> String {
        self.secrets.describe()
    }

    fn url_for(&self, role: &str) -> Result<String, String> {
        let password = self
            .secrets
            .password(role)?
            .ok_or_else(|| format!("no credentials for tenant role {role}"))?;
        let mut url = url::Url::parse(&self.base_url)
            .map_err(|err| format!("invalid database URL: {err}"))?;
        url.set_username(role)
            .map_err(|_| "database URL cannot carry a user".to_string())?;
        url.set_password(if password.is_empty() {
            None
        } else {
            Some(&password)
        })
        .map_err(|_| "database URL cannot carry a password".to_string())?;
        Ok(url.to_string())
    }

    /// The pool for one tenant and role class, opened (and its role checked
    /// with [`PgExec::check_tenant_role`]) on first use.
    pub async fn pool(&self, tenant: &str, class: &str) -> Result<Arc<PgExec>, String> {
        let role = tenant_role(tenant, class)?;
        let mut pools = self.pools.lock().await;
        if let Some(pool) = pools.get(&role) {
            return Ok(pool.clone());
        }
        // Connection errors from PgExec never include the URL (password).
        let pool = PgExec::connect_with(&self.url_for(&role)?, self.options.clone()).await?;
        pool.check_tenant_role(tenant, class).await?;
        let pool = Arc::new(pool);
        pools.insert(role, pool.clone());
        Ok(pool)
    }

    /// Opens and checks every role the secret source knows at startup, and
    /// checks that each tenant's installed policies match the compiled lock
    /// policy of every model ([`PgExec::check_policies`]). Any problem stops
    /// `vibe serve` (fail closed).
    pub async fn check_all(&self, models: &[crate::manifest::ModelSpec]) -> Result<usize, String> {
        let roles = self.secrets.roles()?;
        let mut tenants = BTreeSet::new();
        for role in &roles {
            let (tenant, class) = parse_tenant_role(role).expect("validated in with_source");
            self.pool(&tenant, class).await?;
            tenants.insert(tenant);
        }
        for tenant in &tenants {
            let any = RUNTIME_ROLES
                .iter()
                .find(|class| roles.contains(&tenant_role(tenant, class).unwrap_or_default()))
                .expect("tenant came from a role");
            self.pool(tenant, any)
                .await?
                .check_policies(tenant, models)
                .await?;
        }
        Ok(roles.len())
    }
}

#[async_trait]
impl QueryExec for TenantPools {
    async fn execute(
        &self,
        tenant_id: &str,
        role: &str,
        stmt: Stmt,
    ) -> Result<Vec<Row>, ExecError> {
        super::exec::role_allows(role, &stmt).map_err(super::exec::role_error)?;
        // Role, tenant and process caps: refuse at once instead of queueing
        // until a timeout (D129).
        let _permits = match self.acquire(tenant_id, role) {
            Ok(permits) => permits,
            Err(scope) => {
                let total = scope.counter().fetch_add(1, Ordering::Relaxed) + 1;
                tracing::warn!(
                    scope = scope.as_str(),
                    max_connections = self.max_connections,
                    vibe_db_pool_exhausted_total = total,
                    "database connection limit reached; refusing request with DB_POOL_EXHAUSTED"
                );
                return Err(ExecError::PoolExhausted);
            }
        };
        let pool = self
            .pool(tenant_id, role)
            .await
            .map_err(|err| ExecError::Message(format!("database unavailable: {err}")))?;
        pool.execute(tenant_id, role, stmt).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_names_round_trip() {
        for class in RUNTIME_ROLES {
            for tenant in ["acme0001", "acme_0001", "acme_public"] {
                let name = tenant_role(tenant, class).unwrap();
                assert_eq!(parse_tenant_role(&name), Some((tenant.into(), *class)));
            }
        }
        assert_eq!(parse_tenant_role("app_rw"), None);
        assert_eq!(parse_tenant_role("tenant_x_rw"), None);
        assert_eq!(parse_tenant_role("tenant_acme0001_owner"), None);
    }

    #[test]
    fn unknown_secret_names_are_refused_and_pools_capped() {
        let mut secrets = HashMap::new();
        secrets.insert("app_rw".to_string(), String::new());
        assert!(TenantPools::new("postgres://h/db", secrets, PgOptions::default()).is_err());
        let mut secrets = HashMap::new();
        secrets.insert("tenant_acme0001_owner".to_string(), String::new());
        assert!(TenantPools::new("postgres://h/db", secrets, PgOptions::default()).is_err());
        let p = TenantPools::new("postgres://h/db", HashMap::new(), PgOptions::default()).unwrap();
        assert_eq!(p.options.pool_size, MAX_TENANT_POOL_SIZE);
        assert_eq!(p.max_connections(), DEFAULT_MAX_CONNECTIONS);
        assert_eq!(DEFAULT_MAX_CONNECTIONS, 200);
    }

    #[test]
    fn url_replaces_the_user_and_password() {
        let mut secrets = HashMap::new();
        secrets.insert("tenant_acme0001_rw".to_string(), "p@ss/word".to_string());
        let p = TenantPools::new(
            "postgres://shared:old@db.example.com:5432/app?sslmode=require",
            secrets,
            PgOptions::default(),
        )
        .unwrap();
        let url = p.url_for("tenant_acme0001_rw").unwrap();
        assert!(
            url.starts_with("postgres://tenant_acme0001_rw:p%40ss%2Fword@db.example.com:5432/app"),
            "{url}"
        );
        assert!(!url.contains("shared") && !url.contains("old"));
        let err = p.url_for("tenant_other001_rw").unwrap_err();
        assert!(!err.contains("p@ss"));
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vibe-secrets-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn write_mode(path: &Path, text: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn password_file_must_be_0600_and_never_echoes_secrets() {
        let dir = temp_dir("mode");
        let path = dir.join("s.json");
        let text = r#"{"tenant_acme0001_rw": "s3cret-value"}"#;
        for mode in [0o644, 0o640, 0o604, 0o700, 0o660] {
            write_mode(&path, text, mode);
            let err = FileSecrets::load(&path).err().expect("looser than 0600");
            assert!(
                err.contains("0600") && !err.contains("s3cret"),
                "{mode:o}: {err}"
            );
        }
        write_mode(&path, text, 0o600);
        let s = FileSecrets::load(&path).unwrap();
        assert_eq!(
            s.password("tenant_acme0001_rw").unwrap().as_deref(),
            Some("s3cret-value")
        );
        assert!(!s.describe().contains("s3cret"));
        write_mode(&path, r#"{"tenant_acme0001_rw": "s3cret-value""#, 0o600);
        let err = FileSecrets::load(&path).err().unwrap();
        assert!(!err.contains("s3cret"), "{err}");
        write_mode(&path, text, 0o400);
        assert!(FileSecrets::load(&path).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn production_refuses_password_files_configured_or_present() {
        let dir = temp_dir("prod");
        let path = dir.join("s.json");
        write_mode(&path, "{}", 0o600);
        let file = path.to_str();
        let err = secret_source(true, &dir, file, None).err().unwrap();
        assert!(err.contains("development and tests only"), "{err}");
        let err = secret_source(true, &dir, file, Some("env:VIBE_TDB_"))
            .err()
            .unwrap();
        assert!(err.contains("development and tests only"), "{err}");
        // Present at the default location, even if not configured.
        std::fs::create_dir_all(dir.join(".vibe")).unwrap();
        write_mode(&dir.join(DEV_SECRETS_FILE), "{}", 0o600);
        let err = secret_source(true, &dir, None, Some("env:VIBE_TDB_"))
            .err()
            .unwrap();
        assert!(err.contains("present"), "{err}");
        // Development may use the file; production uses the reference.
        assert!(secret_source(false, &dir, file, None).is_ok());
        let clean = temp_dir("prod-clean");
        assert!(secret_source(true, &clean, None, Some("env:VIBE_TDB_")).is_ok());
        assert!(secret_source(true, &clean, None, Some("vault:kv/x")).is_err());
        assert!(secret_source(true, &clean, None, None).is_err());
    }

    #[test]
    fn env_reference_maps_variables_to_roles() {
        let s = EnvSecrets::from_vars(
            "VIBE_TDB_",
            [
                ("VIBE_TDB_TENANT_ACME0001_RW".to_string(), "pw1".to_string()),
                ("OTHER".to_string(), "x".to_string()),
            ],
        )
        .unwrap();
        assert_eq!(s.roles().unwrap(), vec!["tenant_acme0001_rw".to_string()]);
        assert_eq!(
            s.password("tenant_acme0001_rw").unwrap().as_deref(),
            Some("pw1")
        );
        assert!(!s.describe().contains("pw1"));
        assert!(EnvSecrets::from_vars("lower", Vec::new()).is_err());
    }

    #[tokio::test]
    async fn connection_cap_refuses_immediately_with_pool_exhausted() {
        let mut secrets = HashMap::new();
        secrets.insert("tenant_acme0001_rw".to_string(), String::new());
        // Unroutable database: if the cap did not refuse first, this would
        // hang on connect instead of returning at once.
        let pools = TenantPools::with_source(
            "postgres://10.255.255.1:5432/db",
            Box::new(FileSecrets::from_map(secrets)),
            PgOptions::default(),
            1,
        )
        .unwrap();
        let held = pools.in_use.clone().try_acquire_owned().unwrap();
        let before = pool_exhausted_total();
        let started = std::time::Instant::now();
        let select = Stmt::Select {
            table: "notes".into(),
            columns: vec![],
            id: None,
            filters: vec![],
            sort: None,
            limit: 1,
            offset: 0,
        };
        let err = pools
            .execute("acme0001", "app_rw", select)
            .await
            .unwrap_err();
        assert!(matches!(err, ExecError::PoolExhausted), "{err:?}");
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
        assert!(pool_exhausted_total() > before);
        drop(held);
    }

    fn capped_pools(max: usize) -> TenantPools {
        let mut secrets = HashMap::new();
        for tenant in ["acme0001", "beta0001"] {
            for class in RUNTIME_ROLES {
                secrets.insert(tenant_role(tenant, class).unwrap(), String::new());
            }
        }
        TenantPools::with_source(
            "postgres://10.255.255.1:5432/db",
            Box::new(FileSecrets::from_map(secrets)),
            PgOptions::default(),
            max,
        )
        .unwrap()
    }

    #[test]
    fn five_per_role_then_refused_with_role_scope() {
        let pools = capped_pools(200);
        let held: Vec<_> = (0..5)
            .map(|_| pools.acquire("acme0001", "app_rw").unwrap())
            .collect();
        assert_eq!(
            pools.acquire("acme0001", "app_rw").err(),
            Some(PoolScope::Role)
        );
        // Other roles and tenants are unaffected.
        assert!(pools.acquire("acme0001", "public_reader").is_ok());
        assert!(pools.acquire("beta0001", "app_rw").is_ok());
        drop(held);
        assert!(pools.acquire("acme0001", "app_rw").is_ok());
    }

    #[test]
    fn fifteen_per_tenant_then_refused_with_tenant_scope() {
        let pools = capped_pools(200);
        let mut held = Vec::new();
        for class in RUNTIME_ROLES {
            for _ in 0..5 {
                held.push(pools.acquire("acme0001", class).unwrap());
            }
        }
        assert_eq!(held.len(), MAX_TENANT_CONNECTIONS);
        for class in RUNTIME_ROLES {
            assert_eq!(
                pools.acquire("acme0001", class).err(),
                Some(PoolScope::Tenant)
            );
        }
        assert!(pools.acquire("beta0001", "app_rw").is_ok());
        held.pop();
        assert!(pools.acquire("acme0001", "public_writer").is_ok());
    }

    #[test]
    fn process_cap_is_refused_with_process_scope() {
        let pools = capped_pools(3);
        let held: Vec<_> = (0..3)
            .map(|_| pools.acquire("acme0001", "app_rw").unwrap())
            .collect();
        assert_eq!(
            pools.acquire("beta0001", "app_rw").err(),
            Some(PoolScope::Process)
        );
        drop(held);
        assert!(pools.acquire("beta0001", "app_rw").is_ok());
    }

    #[tokio::test]
    async fn every_scope_refuses_immediately_and_counts_its_metric() {
        let select = || Stmt::Select {
            table: "notes".into(),
            columns: vec![],
            id: None,
            filters: vec![],
            sort: None,
            limit: 1,
            offset: 0,
        };
        for scope in PoolScope::ALL {
            let pools = capped_pools(if scope == PoolScope::Process { 1 } else { 200 });
            let mut held = Vec::new();
            match scope {
                PoolScope::Role => {
                    for _ in 0..5 {
                        held.push(pools.acquire("acme0001", "app_rw").unwrap());
                    }
                }
                PoolScope::Tenant => {
                    for class in RUNTIME_ROLES {
                        for _ in 0..5 {
                            held.push(pools.acquire("acme0001", class).unwrap());
                        }
                    }
                }
                PoolScope::Process => held.push(pools.acquire("beta0001", "app_rw").unwrap()),
            }
            let before = pool_exhausted_count(scope);
            let started = std::time::Instant::now();
            let err = pools
                .execute("acme0001", "app_rw", select())
                .await
                .unwrap_err();
            assert!(
                matches!(err, ExecError::PoolExhausted),
                "{scope:?}: {err:?}"
            );
            assert!(started.elapsed() < std::time::Duration::from_millis(200));
            assert!(pool_exhausted_count(scope) > before, "{scope:?}");
            drop(held);
        }
        let text = pool_exhausted_metrics();
        for scope in ["role", "tenant", "process"] {
            assert!(
                text.contains(&format!(
                    "vibe_db_pool_exhausted_total{{scope=\"{scope}\"}} "
                )),
                "{text}"
            );
        }
    }

    #[test]
    fn pool_max_total_env_is_parsed() {
        assert_eq!(POOL_MAX_TOTAL_ENV, "VIBE_DB_POOL_MAX_TOTAL");
        assert_eq!(parse_pool_max_total(None), Ok(200));
        assert_eq!(parse_pool_max_total(Some(" ")), Ok(200));
        assert_eq!(parse_pool_max_total(Some("50")), Ok(50));
        for bad in ["0", "-1", "x"] {
            assert!(parse_pool_max_total(Some(bad))
                .unwrap_err()
                .contains("VIBE_DB_POOL_MAX_TOTAL"));
        }
    }
}
