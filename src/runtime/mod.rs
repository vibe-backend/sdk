pub mod allowlist;
pub mod challenge;
mod crud;
mod exec;
pub mod grants;
mod http;
pub mod jwks;
mod limiter;
mod memory;
pub mod migrate;
mod pools;
mod slugs;
mod sql;

pub use challenge::{ChallengeContext, ChallengeError, ChallengeVerifier, TurnstileVerifier};
pub use crud::{check_expr, check_when, parse_ownership, run_method, Ownership};
pub use exec::{etag, role_allows, ExecError, PgExec, PgOptions, PgSession, QueryExec, RoleStatus};
pub use http::{
    detect_jwt_alg, hash_api_secret, load_api_keys, resolve_pepper, router, ApiKeyFile, AppState,
    AuthConfig, RunEnv, Slug, StoredKey, REQUEST_TYPES,
};
pub use limiter::{saturation_refusals, ProxyNet, QuotaBuckets, RateBuckets};
pub use memory::{MemoryExec, MEMORY_UNSUPPORTED_SQL};
pub use migrate::{
    check_owner_logins, migrate_tenant, migrate_with_owner_window, owner_login_alerts_total,
};
pub use pools::{
    parse_pool_max_total, parse_tenant_role, pool_exhausted_count, pool_exhausted_metrics,
    pool_exhausted_total, secret_source, secret_source_from_env, EnvSecrets, FileSecrets,
    PoolScope, TenantPools, TenantSecretSource, DEFAULT_MAX_CONNECTIONS, DEV_SECRETS_FILE,
    MAX_TENANT_CONNECTIONS, MAX_TENANT_POOL_SIZE, POOL_MAX_TOTAL_ENV,
};
pub use slugs::{add_slug, load_project_slugs, valid_tenant, SlugLoad, TENANT_ID_RULE};
pub use sql::{
    bootstrap_statements, policy_parts, quote as sql_quote, render_policy, rls_statements,
    tenant_schema_statements, Col, PolicyParts, Sort, Stmt, POLICY_TEMPLATE,
};
