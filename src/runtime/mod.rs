pub mod challenge;
mod crud;
pub mod eckey;
mod exec;
mod http;
pub mod jwks;
mod limiter;
mod migrate;
mod ops;
pub mod refresh;
pub mod replay;
pub mod rsakey;
mod slugs;
mod sql;

pub use challenge::{ChallengeContext, ChallengeError, ChallengeVerifier, TurnstileVerifier};
pub use crud::{
    check_expr, check_when, parse_ownership, run_method, when_problem, CallCtx, CallResult, Caller,
    MethodFail, Ownership,
};
pub use exec::{
    etag, role_allows, ExecError, MemoryExec, PgExec, PgOptions, PgSession, QueryExec,
    RefreshIssuer, RoleStatus, Row,
};
pub use http::{
    check_static_jwt_key, detect_jwt_alg, hash_api_secret, load_api_keys, refresh_rejections,
    resolve_pepper, router, static_jwt_alg, ApiKeyFile, AppState, AuthConfig, RunEnv, SigningKey,
    Slug, StoredKey, REQUEST_TYPES,
};
pub use ops::{
    custom_claim_problem, ec_signing_key_from_der, ec_signing_key_from_pem, load_signing_keys,
    sub_source_problem,
};
pub use sql::Stmt;

/// argon2id hash with the platform floor (for seeding test data).
pub fn hash_password_for_tests(plain: &str) -> String {
    crud::hash_password(plain).expect("hash")
}
pub use limiter::{saturation_refusals, ProxyNet, QuotaBuckets, RateBuckets};
pub use migrate::migrate_tenant;
pub use slugs::{add_slug, load_project_slugs, valid_tenant, SlugLoad};
pub use sql::{
    bootstrap_statements, quote as quote_ident, quote as sql_quote, runtime_grant_statements,
    runtime_schema_statements, Col,
};

/// Render process counters for an internal metrics collector.
pub fn internal_metrics() -> String {
    let mut output = format!(
        "vibe_rate_limit_saturation_refusals_total {}\n",
        saturation_refusals()
    );
    let mut rejections: Vec<_> = refresh_rejections().into_iter().collect();
    rejections.sort_unstable_by_key(|(reason, _)| *reason);
    for (reason, count) in rejections {
        output.push_str(&format!(
            "vibe_refresh_rejections_total{{reason=\"{reason}\"}} {count}\n"
        ));
    }
    output
}
