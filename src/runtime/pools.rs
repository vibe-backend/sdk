//! One connection pool per (tenant, role class), each logged in directly as
//! that tenant's `LOGIN` role (decision 97). There is no shared runtime
//! login and no `SET ROLE`: a tenant connection holds no identity it could
//! switch back to, so it can only ever reach its own tenant schema.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::exec::{ExecError, PgExec, PgOptions, QueryExec, Row};
use super::grants::{tenant_role, RUNTIME_ROLES};
use super::sql::Stmt;

/// Spec §7.1: at most 5 connections per tenant pool.
pub const MAX_TENANT_POOL_SIZE: usize = 5;

/// Lazily opened per-tenant pools.
pub struct TenantPools {
    /// Server and database; the user and password are replaced per role.
    base_url: String,
    /// Tenant role name -> password (empty: no password, e.g. trust/peer).
    secrets: HashMap<String, String>,
    options: PgOptions,
    pools: tokio::sync::Mutex<HashMap<String, Arc<PgExec>>>,
}

/// `(tenant, class)` for a `tenant_<id>_<suffix>` role name.
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

impl TenantPools {
    /// `base_url` names the server and database; `secrets` maps each tenant
    /// role (`tenant_<id>_rw`, ...) to its password. Unknown role names are
    /// refused.
    pub fn new(
        base_url: &str,
        secrets: HashMap<String, String>,
        options: PgOptions,
    ) -> Result<Self, String> {
        url::Url::parse(base_url).map_err(|err| format!("invalid database URL: {err}"))?;
        if let Some(bad) = secrets.keys().find(|k| parse_tenant_role(k).is_none()) {
            return Err(format!(
                "tenant DB secrets: `{bad}` is not a tenant role name (tenant_<id>_rw, tenant_<id>_public_reader, tenant_<id>_public_writer)"
            ));
        }
        let mut options = options;
        options.pool_size = options.pool_size.min(MAX_TENANT_POOL_SIZE);
        Ok(Self {
            base_url: base_url.to_string(),
            secrets,
            options,
            pools: tokio::sync::Mutex::default(),
        })
    }

    /// From `VIBE_DATABASE_URL` (or `DATABASE_URL`; server and database
    /// only) and `VIBE_TENANT_DB_SECRETS`, the path of a JSON object mapping
    /// each tenant role to its password (the runtime secret store export).
    pub fn from_env() -> Result<Self, String> {
        let url = std::env::var("VIBE_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .map_err(|_| {
                "set VIBE_DATABASE_URL or pass --memory for the dev executor".to_string()
            })?;
        let path = std::env::var("VIBE_TENANT_DB_SECRETS").map_err(|_| {
            "VIBE_TENANT_DB_SECRETS is required: a JSON file mapping each tenant role (tenant_<id>_rw, tenant_<id>_public_reader, tenant_<id>_public_writer) to its password; vibe serve logs in as those roles directly (decision 97)".to_string()
        })?;
        let text = std::fs::read_to_string(&path)
            .map_err(|err| format!("VIBE_TENANT_DB_SECRETS {path}: {err}"))?;
        let secrets: HashMap<String, String> = serde_json::from_str(&text)
            .map_err(|err| format!("VIBE_TENANT_DB_SECRETS {path}: {err}"))?;
        Self::new(&url, secrets, PgOptions::from_env()?)
    }

    fn url_for(&self, role: &str) -> Result<String, String> {
        let password = self
            .secrets
            .get(role)
            .ok_or_else(|| format!("no credentials for tenant role {role}"))?;
        let mut url = url::Url::parse(&self.base_url)
            .map_err(|err| format!("invalid database URL: {err}"))?;
        url.set_username(role)
            .map_err(|_| "database URL cannot carry a user".to_string())?;
        url.set_password(if password.is_empty() {
            None
        } else {
            Some(password)
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
        let pool = PgExec::connect_with(&self.url_for(&role)?, self.options.clone()).await?;
        pool.check_tenant_role(tenant, class).await?;
        let pool = Arc::new(pool);
        pools.insert(role, pool.clone());
        Ok(pool)
    }

    /// Opens and checks every role in the secrets at startup, so an unsafe
    /// tenant role stops `vibe serve` instead of failing later.
    pub async fn check_all(&self) -> Result<usize, String> {
        let mut roles: Vec<&String> = self.secrets.keys().collect();
        roles.sort();
        for role in &roles {
            let (tenant, class) = parse_tenant_role(role).expect("validated in new");
            self.pool(&tenant, class).await?;
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
            let name = tenant_role("acme0001", class).unwrap();
            assert_eq!(parse_tenant_role(&name), Some(("acme0001".into(), *class)));
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
        let p = TenantPools::new("postgres://h/db", HashMap::new(), PgOptions::default()).unwrap();
        assert_eq!(p.options.pool_size, MAX_TENANT_POOL_SIZE);
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
        assert!(p.url_for("tenant_other001_rw").is_err());
    }
}
