pub mod challenge;
mod crud;
mod exec;
mod http;
pub mod jwks;
mod limiter;
mod migrate;
mod ops;
pub mod replay;
mod slugs;
mod sql;

pub use challenge::{ChallengeContext, ChallengeError, ChallengeVerifier, TurnstileVerifier};
pub use crud::{
    check_expr, check_when, parse_ownership, run_method, when_problem, CallCtx, CallResult, Caller,
    MethodFail, Ownership,
};
pub use exec::{
    etag, role_allows, ExecError, MemoryExec, PgExec, PgOptions, PgSession, QueryExec, RoleStatus,
    Row,
};
pub use http::{
    check_test_ip_override, detect_jwt_alg, hash_api_secret, honor_test_ip_from_env, load_api_keys,
    resolve_pepper, router, ApiKeyFile, AppState, AuthConfig, RunEnv, SigningKey, Slug, StoredKey,
    REQUEST_TYPES, TEST_IP_OVERRIDE_AVAILABLE,
};
pub use limiter::{
    active_tenants_alerts, counter_slot_refusals, saturation_refusals, slot_refused_metrics,
    slot_refused_total, slot_refused_within_floor_total, LimiterMemory, NetLimits, ProxyNet,
    QuotaBuckets, RateBuckets, ACTIVE_TENANTS_ALERT, BUCKET_BYTES, COUNTER_SLOT_BYTES,
    FLOOR_RESERVE_BYTES, FLOOR_SLOT_BYTES, HIT_BYTES, MAX_EXPECTED_TENANTS, MAX_PLATFORM_FACTOR,
    PLATFORM_SLOT_FLOOR, QUOTA_ENTRY_BYTES, TABLE_MEMORY_MAX_BYTES,
};
pub use ops::{custom_claim_problem, load_signing_keys, sub_source_problem};
pub use sql::Stmt;

/// argon2id hash with the platform floor (for seeding test data).
pub fn hash_password_for_tests(plain: &str) -> String {
    crud::hash_password(plain).expect("hash")
}
pub use migrate::migrate_tenant;
pub use slugs::{add_slug, load_project_slugs, SlugLoad};
pub use sql::{
    bootstrap_statements, quote as quote_ident, quote as sql_quote, runtime_grant_statements,
    runtime_schema_statements, Col,
};
