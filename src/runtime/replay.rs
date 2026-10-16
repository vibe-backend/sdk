//! Single-use bookkeeping for challenge tokens behind a trait (#157).
//!
//! [`ReplayCache`](super::challenge::ReplayCache) keeps the state in process
//! memory and suits a single instance. [`PgReplayStore`] keeps it in a
//! PostgreSQL table shared by every instance, so a token, the per-IP and
//! per-tenant quotas and the capacity limit hold across instances. Every
//! store error is reported as [`ChallengeError::Unavailable`]: the request
//! fails closed.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use super::challenge::{
    ChallengeError, DEFAULT_PENDING_PER_IP, DEFAULT_PENDING_PER_TENANT, DEFAULT_REJECTED_HOLD,
    TOKEN_LIFETIME,
};

/// Schema holding the shared table (created by `vibe migrate`).
pub const REPLAY_SCHEMA: &str = "vibe_platform";
/// Default shared table, in [`REPLAY_SCHEMA`].
pub const DEFAULT_REPLAY_TABLE: &str = "challenge_replay";
const DEFAULT_CAPACITY: i64 = 100_000;
const PENDING_HOLD: Duration = Duration::from_secs(30);
/// Expired rows removed per reservation.
const CLEANUP_BATCH: i64 = 100;

/// Replay-cache key for a token (SHA-256; the token itself is never stored).
pub fn token_key(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// How a reservation ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The provider accepted the token: keep it for the full TTL.
    Verified,
    /// The provider rejected the token: keep it only briefly.
    Rejected,
    /// No answer (provider unavailable, request dropped): release the slot.
    Released,
}

#[async_trait]
pub trait ReplayStore: Send + Sync {
    /// `memory` or `postgres`.
    fn name(&self) -> &'static str;
    /// Reserves `key` before the provider is asked. Rejected when the key is
    /// reserved or used, or the IP or tenant is over its quota of unverified
    /// slots; unavailable when the store is full or cannot be reached.
    async fn reserve(
        &self,
        key: &[u8; 32],
        ip: Option<&str>,
        tenant: Option<&str>,
    ) -> Result<(), ChallengeError>;
    /// Ends a reservation.
    async fn settle(&self, key: &[u8; 32], outcome: Outcome) -> Result<(), ChallengeError>;
}

/// Releases a pending reservation when the request is dropped mid-check.
pub(crate) struct PendingGuard {
    store: Arc<dyn ReplayStore>,
    key: [u8; 32],
    armed: bool,
}

impl PendingGuard {
    pub(crate) fn new(store: Arc<dyn ReplayStore>, key: [u8; 32]) -> Self {
        Self {
            store,
            key,
            armed: true,
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let store = self.store.clone();
        let key = self.key;
        // Without a runtime the pending entry still expires on its own.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = store.settle(&key, Outcome::Released).await;
            });
        }
    }
}

/// Which replay store `vibe serve` uses (`VIBE_CHALLENGE_REPLAY_STORE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreKind {
    Memory,
    Postgres,
}

/// `VIBE_CHALLENGE_REPLAY_STORE`: `memory`, `postgres`, or unset (`None`).
/// There is no default here; [`resolve_replay_store`] decides.
pub fn replay_kind_from_vars(
    get: impl Fn(&str) -> Option<String>,
) -> Result<Option<StoreKind>, String> {
    match get("VIBE_CHALLENGE_REPLAY_STORE")
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("") => Ok(None),
        Some("memory") => Ok(Some(StoreKind::Memory)),
        Some("postgres") => Ok(Some(StoreKind::Postgres)),
        Some(other) => Err(format!(
            "unknown VIBE_CHALLENGE_REPLAY_STORE {other:?}; use postgres (or memory outside production)"
        )),
    }
}

/// The replay store to use, or `None` when no verifier is configured.
///
/// - Production: always PostgreSQL. Unset means `postgres`; `memory` is refused
///   (even without a verifier).
/// - Development and CI: an explicit `memory` or `postgres` is required when a
///   verifier is configured; unset is an error.
/// - `postgres` needs a database (not `--memory`).
pub fn resolve_replay_store(
    env: super::http::RunEnv,
    verifier_configured: bool,
    kind: Option<StoreKind>,
    has_database: bool,
) -> Result<Option<StoreKind>, String> {
    let production = env == super::http::RunEnv::Production;
    if production && kind == Some(StoreKind::Memory) {
        return Err(
            "VIBE_CHALLENGE_REPLAY_STORE=memory is refused in production; challenge verification uses the shared PostgreSQL store (unset the variable or set it to postgres)".into(),
        );
    }
    if !verifier_configured {
        return Ok(None);
    }
    let kind = match (kind, production) {
        (Some(kind), _) => kind,
        (None, true) => StoreKind::Postgres,
        (None, false) => {
            return Err(
                "challenge verification needs an explicit replay store outside production: set VIBE_CHALLENGE_REPLAY_STORE=memory (single process) or postgres".into(),
            )
        }
    };
    if kind == StoreKind::Postgres && !has_database {
        return Err(
            "the challenge replay store needs PostgreSQL (VIBE_DATABASE_URL); with --memory set VIBE_CHALLENGE_REPLAY_STORE=memory".into(),
        );
    }
    Ok(Some(kind))
}

/// Replay state in a PostgreSQL table shared by all instances.
pub struct PgReplayStore {
    pool: deadpool_postgres::Pool,
    table: String,
    ttl: Duration,
    capacity: i64,
    per_ip: i64,
    per_tenant: i64,
    rejected_hold: Duration,
}

fn valid_table(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && name.len() <= 48
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn unavailable(err: impl std::fmt::Display) -> ChallengeError {
    ChallengeError::Unavailable(format!("replay store: {err}"))
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

impl PgReplayStore {
    pub fn new(pool: deadpool_postgres::Pool, table: &str) -> Result<Self, String> {
        if !valid_table(table) {
            return Err(format!("invalid replay table name {table:?}"));
        }
        Ok(Self {
            pool,
            table: format!("{REPLAY_SCHEMA}.{table}"),
            ttl: TOKEN_LIFETIME + Duration::from_secs(60),
            capacity: DEFAULT_CAPACITY,
            per_ip: DEFAULT_PENDING_PER_IP as i64,
            per_tenant: DEFAULT_PENDING_PER_TENANT as i64,
            rejected_hold: DEFAULT_REJECTED_HOLD,
        })
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity as i64;
        self
    }

    pub fn with_quotas(mut self, per_ip: usize, per_tenant: usize) -> Self {
        self.per_ip = per_ip as i64;
        self.per_tenant = per_tenant as i64;
        self
    }

    pub fn with_rejected_hold(mut self, hold: Duration) -> Self {
        self.rejected_hold = hold;
        self
    }

    /// DDL for the shared table, run by `vibe migrate` as the owner role.
    /// The runtime role gets SELECT, INSERT, UPDATE and DELETE on it.
    pub fn migration_sql(table: &str, runtime_role: &str) -> Result<Vec<String>, String> {
        if !valid_table(table) {
            return Err(format!("invalid replay table name {table:?}"));
        }
        let role = quote_ident(runtime_role);
        let name = table;
        let table = format!("{REPLAY_SCHEMA}.{name}");
        Ok(vec![
            format!("CREATE SCHEMA IF NOT EXISTS {REPLAY_SCHEMA}"),
            format!("REVOKE ALL ON SCHEMA {REPLAY_SCHEMA} FROM PUBLIC"),
            format!("GRANT USAGE ON SCHEMA {REPLAY_SCHEMA} TO {role}"),
            format!(
                "CREATE TABLE IF NOT EXISTS {table} (key bytea PRIMARY KEY, expires_at timestamptz NOT NULL, counted boolean NOT NULL, ip_hash bytea, tenant text)"
            ),
            format!("CREATE INDEX IF NOT EXISTS {name}_expires ON {table} (expires_at)"),
            format!("CREATE INDEX IF NOT EXISTS {name}_ip ON {table} (ip_hash) WHERE counted"),
            format!("CREATE INDEX IF NOT EXISTS {name}_tenant ON {table} (tenant) WHERE counted"),
            format!("REVOKE ALL ON {table} FROM PUBLIC"),
            format!("GRANT SELECT, INSERT, UPDATE, DELETE ON {table} TO {role}"),
        ])
    }

    /// Fails unless the runtime role can use the table (startup check).
    pub async fn check(&self) -> Result<(), String> {
        let client = self.pool.get().await.map_err(|err| err.to_string())?;
        client
            .execute(
                &format!(
                    "SELECT 1 FROM {t} WHERE false FOR UPDATE",
                    t = self.table
                ),
                &[],
            )
            .await
            .map_err(|err| {
                format!(
                    "challenge replay table {} is not usable ({err}); run `vibe migrate` to create it",
                    self.table
                )
            })?;
        Ok(())
    }

    async fn try_reserve(
        &self,
        key: &[u8; 32],
        ip: Option<&str>,
        tenant: Option<&str>,
    ) -> Result<(), ChallengeError> {
        let t = &self.table;
        let mut client = self.pool.get().await.map_err(unavailable)?;
        let tx = client.transaction().await.map_err(unavailable)?;
        tx.batch_execute("SET LOCAL statement_timeout = '2s'")
            .await
            .map_err(unavailable)?;
        tx.execute(
            &format!(
                "DELETE FROM {t} WHERE ctid IN (SELECT ctid FROM {t} WHERE expires_at <= now() LIMIT {CLEANUP_BATCH})"
            ),
            &[],
        )
        .await
        .map_err(unavailable)?;
        let ip_hash: Option<Vec<u8>> = ip.map(|ip| Sha256::digest(ip.as_bytes()).to_vec());
        // Serialize quota checks per IP and per tenant across instances.
        if let Some(h) = &ip_hash {
            tx.execute(
                "SELECT pg_advisory_xact_lock(hashtextextended('vibe-replay-ip:' || encode($1, 'hex'), 0))",
                &[h],
            )
            .await
            .map_err(unavailable)?;
            let n: i64 = tx
                .query_one(
                    &format!("SELECT count(*) FROM {t} WHERE counted AND ip_hash = $1 AND expires_at > now()"),
                    &[h],
                )
                .await
                .map_err(unavailable)?
                .get(0);
            if n >= self.per_ip {
                return Err(ChallengeError::Rejected(
                    "too many unverified challenge tokens from this client".into(),
                ));
            }
        }
        if let Some(tenant) = tenant {
            tx.execute(
                "SELECT pg_advisory_xact_lock(hashtextextended('vibe-replay-tenant:' || $1, 0))",
                &[&tenant],
            )
            .await
            .map_err(unavailable)?;
            let n: i64 = tx
                .query_one(
                    &format!("SELECT count(*) FROM {t} WHERE counted AND tenant = $1 AND expires_at > now()"),
                    &[&tenant],
                )
                .await
                .map_err(unavailable)?
                .get(0);
            if n >= self.per_tenant {
                return Err(ChallengeError::Rejected(
                    "too many unverified challenge tokens for this tenant".into(),
                ));
            }
        }
        let live: i64 = tx
            .query_one(
                &format!(
                    "SELECT count(*) FROM (SELECT 1 FROM {t} WHERE expires_at > now() LIMIT $1) s"
                ),
                &[&self.capacity],
            )
            .await
            .map_err(unavailable)?
            .get(0);
        if live >= self.capacity {
            return Err(ChallengeError::Unavailable(
                "challenge replay cache is full".into(),
            ));
        }
        // A live row means the token is reserved or used; an expired row is
        // taken over atomically.
        let inserted = tx
            .query_opt(
                &format!(
                    "INSERT INTO {t} AS r (key, expires_at, counted, ip_hash, tenant) \
                     VALUES ($1, now() + make_interval(secs => $2), true, $3, $4) \
                     ON CONFLICT (key) DO UPDATE SET expires_at = EXCLUDED.expires_at, counted = true, \
                     ip_hash = EXCLUDED.ip_hash, tenant = EXCLUDED.tenant WHERE r.expires_at <= now() \
                     RETURNING 1"
                ),
                &[
                    &key.as_slice(),
                    &PENDING_HOLD.as_secs_f64(),
                    &ip_hash,
                    &tenant,
                ],
            )
            .await
            .map_err(unavailable)?;
        if inserted.is_none() {
            return Err(ChallengeError::Rejected(
                "challenge token already used".into(),
            ));
        }
        tx.commit().await.map_err(unavailable)?;
        Ok(())
    }
}

#[async_trait]
impl ReplayStore for PgReplayStore {
    fn name(&self) -> &'static str {
        "postgres"
    }

    async fn reserve(
        &self,
        key: &[u8; 32],
        ip: Option<&str>,
        tenant: Option<&str>,
    ) -> Result<(), ChallengeError> {
        self.try_reserve(key, ip, tenant).await
    }

    async fn settle(&self, key: &[u8; 32], outcome: Outcome) -> Result<(), ChallengeError> {
        let t = &self.table;
        let client = self.pool.get().await.map_err(unavailable)?;
        let k = key.as_slice();
        match outcome {
            Outcome::Verified => {
                let n = client
                    .execute(
                        &format!("UPDATE {t} SET expires_at = now() + make_interval(secs => $2), counted = false WHERE key = $1 AND counted"),
                        &[&k, &self.ttl.as_secs_f64()],
                    )
                    .await
                    .map_err(unavailable)?;
                if n != 1 {
                    // The reservation lapsed; do not accept the token.
                    return Err(ChallengeError::Unavailable(
                        "challenge reservation expired before it was recorded".into(),
                    ));
                }
            }
            Outcome::Rejected if !self.rejected_hold.is_zero() => {
                client
                    .execute(
                        &format!("UPDATE {t} SET expires_at = now() + make_interval(secs => $2) WHERE key = $1 AND counted"),
                        &[&k, &self.rejected_hold.as_secs_f64()],
                    )
                    .await
                    .map_err(unavailable)?;
            }
            Outcome::Rejected | Outcome::Released => {
                client
                    .execute(
                        &format!("DELETE FROM {t} WHERE key = $1 AND counted"),
                        &[&k],
                    )
                    .await
                    .map_err(unavailable)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn replay_store_variable_has_no_default() {
        assert_eq!(replay_kind_from_vars(vars(&[])).unwrap(), None);
        assert_eq!(
            replay_kind_from_vars(vars(&[("VIBE_CHALLENGE_REPLAY_STORE", "")])).unwrap(),
            None
        );
        assert_eq!(
            replay_kind_from_vars(vars(&[("VIBE_CHALLENGE_REPLAY_STORE", "Postgres")])).unwrap(),
            Some(StoreKind::Postgres)
        );
        assert_eq!(
            replay_kind_from_vars(vars(&[("VIBE_CHALLENGE_REPLAY_STORE", "memory")])).unwrap(),
            Some(StoreKind::Memory)
        );
        assert!(replay_kind_from_vars(vars(&[("VIBE_CHALLENGE_REPLAY_STORE", "redis")])).is_err());
    }

    #[test]
    fn production_always_uses_the_shared_store() {
        use super::super::http::RunEnv::Production;
        // Unset means postgres.
        assert_eq!(
            resolve_replay_store(Production, true, None, true).unwrap(),
            Some(StoreKind::Postgres)
        );
        assert_eq!(
            resolve_replay_store(Production, true, Some(StoreKind::Postgres), true).unwrap(),
            Some(StoreKind::Postgres)
        );
        // The memory switch is refused, with or without a verifier.
        let err =
            resolve_replay_store(Production, true, Some(StoreKind::Memory), true).unwrap_err();
        assert!(err.contains("refused in production"), "{err}");
        assert!(resolve_replay_store(Production, false, Some(StoreKind::Memory), true).is_err());
        // No database: refused.
        assert!(resolve_replay_store(Production, true, None, false).is_err());
        // No verifier: no store.
        assert_eq!(
            resolve_replay_store(Production, false, None, true).unwrap(),
            None
        );
    }

    #[test]
    fn development_needs_an_explicit_choice() {
        use super::super::http::RunEnv::{Ci, Dev};
        for env in [Dev, Ci] {
            let err = resolve_replay_store(env, true, None, true).unwrap_err();
            assert!(err.contains("VIBE_CHALLENGE_REPLAY_STORE=memory"), "{err}");
            assert_eq!(
                resolve_replay_store(env, true, Some(StoreKind::Memory), false).unwrap(),
                Some(StoreKind::Memory)
            );
            assert_eq!(
                resolve_replay_store(env, true, Some(StoreKind::Postgres), true).unwrap(),
                Some(StoreKind::Postgres)
            );
            assert!(resolve_replay_store(env, true, Some(StoreKind::Postgres), false).is_err());
            assert_eq!(resolve_replay_store(env, false, None, false).unwrap(), None);
        }
    }

    #[test]
    fn table_names_are_checked() {
        assert!(valid_table("vibe_challenge_replay"));
        assert!(!valid_table("x; drop table y"));
        assert!(!valid_table("Upper"));
        assert!(PgReplayStore::migration_sql("bad name", "app").is_err());
        let sql = PgReplayStore::migration_sql("t1", "app\"x").unwrap();
        assert!(sql.last().unwrap().ends_with("TO \"app\"\"x\""));
    }
}
