pub mod challenge;
mod crud;
pub mod eckey;
mod exec;
mod http;
pub mod jwks;
mod limiter;
pub mod memsql;
pub mod migrate;
mod ops;
pub mod refresh;
pub mod replay;
pub mod rsakey;
pub(crate) mod slugs;
mod sql;

pub use challenge::{ChallengeContext, ChallengeError, ChallengeVerifier, TurnstileVerifier};
pub use crud::{
    check_expr, check_when, expression_roots, parse_ownership, run_method, when_problem, CallCtx,
    CallResult, Caller, MethodFail, Ownership,
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
pub use slugs::{apply_dev_default, load_project_slugs, load_slug_file, DevSlugs, SlugLoad};
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

/// Platform error codes advertised to the compiler prompt.
/// Backend-specific failures are handled separately to keep runtime changes prompt-neutral.
pub const RUNTIME_ERROR_CODES: &[&str] = &[
    "BAD_REQUEST",
    "CHALLENGE_FAILED",
    "CHALLENGE_REQUIRED",
    "CHALLENGE_UNAVAILABLE",
    "ENDPOINT_DISABLED",
    "FORBIDDEN",
    "INTERNAL",
    "INVALID_REFRESH_TOKEN",
    "METHOD_NOT_ALLOWED",
    "NOT_FOUND",
    "PAYLOAD_TOO_LARGE",
    "PRECONDITION_FAILED",
    "PRECONDITION_REQUIRED",
    "RATE_LIMITED",
    "SERVICE_UNAVAILABLE",
    "TX_CONFLICT",
    "TX_TIMEOUT",
    "UNAUTHORIZED",
    "UNIQUE_VIOLATION",
    "UNSUPPORTED_MEDIA_TYPE",
    "VALIDATION_FAILED",
    "WEAK_PASSWORD",
];

#[cfg(feature = "sdk")]
pub(crate) use crud::expression_paths;
