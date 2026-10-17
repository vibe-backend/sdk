//! Single-use bookkeeping for challenge tokens behind a trait (#157).
//!
//! [`ReplayCache`](super::challenge::ReplayCache) keeps the state in process
//! memory and suits a single instance. [`PgReplayStore`] keeps it in a table
//! inside each tenant's own schema (`t_<tenant>.vibe_challenge_replay`,
//! created by that tenant's automatic tenant migration, `tenant_id` under FORCE RLS), so a
//! token, the per-IP and per-tenant quotas and the per-tenant capacity hold
//! across instances. No object is shared between tenants.
//!
//! The runtime role only has SELECT and INSERT on the table: the table is an
//! append-only log. A reservation is a `pending` row; settling it appends a
//! `verified`, `rejected` or `released` row. Reads ignore expired rows, and
//! only the owner role deletes them (during automatic tenant migration), so the runtime
//! cannot remove or shorten a live verified row. Every store error is
//! reported as [`ChallengeError::Unavailable`]: the request fails closed.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

use super::challenge::{
    ChallengeError, DEFAULT_PENDING_PER_IP, DEFAULT_PENDING_PER_TENANT, DEFAULT_REJECTED_HOLD,
    TOKEN_LIFETIME,
};

/// Table in each tenant schema (created by automatic tenant migration).
pub const REPLAY_TABLE: &str = "vibe_challenge_replay";
/// Live rows allowed per tenant.
const DEFAULT_CAPACITY: i64 = 10_000;
const PENDING_HOLD: Duration = Duration::from_secs(30);

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

/// A reservation handed out by [`ReplayStore::reserve`]. The nonce ties the
/// settlement to this reservation only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplaySlot {
    pub key: [u8; 32],
    pub tenant: Option<String>,
    pub nonce: [u8; 16],
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
    ) -> Result<ReplaySlot, ChallengeError>;
    /// Ends a reservation.
    async fn settle(&self, slot: &ReplaySlot, outcome: Outcome) -> Result<(), ChallengeError>;
}

/// Releases a pending reservation when the request is dropped mid-check.
pub(crate) struct PendingGuard {
    store: Arc<dyn ReplayStore>,
    slot: Option<ReplaySlot>,
}

impl PendingGuard {
    pub(crate) fn new(store: Arc<dyn ReplayStore>, slot: ReplaySlot) -> Self {
        Self {
            store,
            slot: Some(slot),
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.slot = None;
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        let Some(slot) = self.slot.take() else {
            return;
        };
        let store = self.store.clone();
        // Without a runtime the pending entry still expires on its own.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = store.settle(&slot, Outcome::Released).await;
            });
        }
    }
}

/// Which replay store `vibe run` uses (`VIBE_CHALLENGE_REPLAY_STORE`).
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
            "VIBE_CHALLENGE_REPLAY_STORE=memory is refused in production; challenge verification uses the PostgreSQL store (unset the variable or set it to postgres)".into(),
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

/// Replay state in each tenant's schema, shared by all instances.
pub struct PgReplayStore {
    pool: deadpool_postgres::Pool,
    ip_secret: Vec<u8>,
    ttl: Duration,
    capacity: i64,
    per_ip: i64,
    per_tenant: i64,
    rejected_hold: Duration,
}

fn unavailable(err: impl std::fmt::Display) -> ChallengeError {
    ChallengeError::Unavailable(format!("replay store: {err}"))
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn table_for(tenant: &str) -> Result<String, ChallengeError> {
    if !super::slugs::valid_tenant(tenant) {
        return Err(unavailable("invalid tenant id"));
    }
    Ok(format!("\"t_{tenant}\".{REPLAY_TABLE}"))
}

/// A live hold: an unreleased pending row, or a rejected or verified row.
const LIVE: &str = "expires_at > now() AND (state IN ('verified', 'rejected') OR NOT EXISTS (\
     SELECT 1 FROM {t} x WHERE x.key = r.key AND x.nonce = r.nonce AND x.state = 'released'))";

fn live(t: &str) -> String {
    LIVE.replace("{t}", t)
}

/// HMAC-SHA256 of the client IP under a per-tenant key derived from `secret`
/// (the API-key pepper), so the stored value is not comparable across tenants
/// and cannot be reversed by hashing the IPv4 space.
pub fn ip_digest(secret: &[u8], tenant: &str, ip: &str) -> Vec<u8> {
    let mut kdf = <Hmac<Sha256> as KeyInit>::new_from_slice(secret).expect("hmac key");
    kdf.update(b"vibe-challenge-replay-ip\0");
    kdf.update(tenant.as_bytes());
    let tenant_key = kdf.finalize().into_bytes();
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&tenant_key).expect("hmac key");
    mac.update(ip.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

impl PgReplayStore {
    /// `ip_secret` keys the per-tenant IP digest (the API-key pepper).
    pub fn new(pool: deadpool_postgres::Pool, ip_secret: &[u8]) -> Self {
        Self {
            pool,
            ip_secret: ip_secret.to_vec(),
            ttl: TOKEN_LIFETIME + Duration::from_secs(60),
            capacity: DEFAULT_CAPACITY,
            per_ip: DEFAULT_PENDING_PER_IP as i64,
            per_tenant: DEFAULT_PENDING_PER_TENANT as i64,
            rejected_hold: DEFAULT_REJECTED_HOLD,
        }
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Live rows allowed per tenant.
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

    /// DDL for one tenant's table, run inside that tenant's automatic tenant migration
    /// as the owner role (before the schema's RLS pass, which adds ENABLE +
    /// FORCE RLS and the tenant policy). The runtime role gets SELECT and
    /// INSERT only. Also removes expired rows (as the owner).
    pub fn migration_sql(tenant: &str, runtime_role: &str) -> Result<Vec<String>, String> {
        if !super::slugs::valid_tenant(tenant) {
            return Err("invalid tenant id".into());
        }
        let role = quote_ident(runtime_role);
        let t = format!("\"t_{tenant}\".{REPLAY_TABLE}");
        Ok(vec![
            format!("CREATE SCHEMA IF NOT EXISTS \"t_{tenant}\""),
            format!("GRANT USAGE ON SCHEMA \"t_{tenant}\" TO {role}"),
            format!(
                "CREATE TABLE IF NOT EXISTS {t} (tenant_id text NOT NULL, key bytea NOT NULL, nonce bytea NOT NULL, \
                 state text NOT NULL CHECK (state IN ('pending', 'verified', 'rejected', 'released')), \
                 expires_at timestamptz NOT NULL, ip_hash bytea, PRIMARY KEY (tenant_id, key, nonce, state))"
            ),
            format!("CREATE INDEX IF NOT EXISTS {REPLAY_TABLE}_expires ON {t} (tenant_id, expires_at)"),
            format!("REVOKE ALL ON {t} FROM PUBLIC"),
            format!("REVOKE ALL ON {t} FROM {role}"),
            format!("GRANT SELECT, INSERT ON {t} TO {role}"),
            format!("ALTER TABLE {t} ENABLE ROW LEVEL SECURITY"),
            format!("ALTER TABLE {t} FORCE ROW LEVEL SECURITY"),
            format!("SELECT set_config('app.tenant_id', '{tenant}', true)"),
            format!("DELETE FROM {t} WHERE expires_at <= now()"),
        ])
    }

    /// Startup check run by `vibe run`: every listed tenant has the table,
    /// usable by this role, and the role cannot DELETE, UPDATE or TRUNCATE it.
    pub async fn check(&self, tenants: &[String]) -> Result<(), String> {
        let client = self
            .pool
            .get()
            .await
            .map_err(|err| format!("challenge replay store: {err}"))?;
        for tenant in tenants {
            let t = table_for(tenant).map_err(|err| format!("{err:?}"))?;
            let row = client
                .query_one(
                    "SELECT has_table_privilege($1, 'SELECT') AND has_table_privilege($1, 'INSERT'), \
                     has_table_privilege($1, 'DELETE') OR has_table_privilege($1, 'UPDATE') OR has_table_privilege($1, 'TRUNCATE')",
                    &[&t],
                )
                .await
                .map_err(|err| {
                    format!("challenge replay table {t} is not usable ({err}); restart `vibe run --db postgres` with VIBE_MIGRATE_DATABASE_URL set for this tenant")
                })?;
            let (usable, too_much): (bool, bool) = (row.get(0), row.get(1));
            if !usable {
                return Err(format!(
                    "challenge replay table {t} is not readable and writable by this role; restart `vibe run --db postgres` with VIBE_MIGRATE_DATABASE_URL set for this tenant"
                ));
            }
            if too_much {
                return Err(format!(
                    "challenge replay table {t} grants DELETE, UPDATE or TRUNCATE to this role; restart `vibe run --db postgres` with VIBE_MIGRATE_DATABASE_URL set to reset its grants"
                ));
            }
        }
        Ok(())
    }

    async fn begin<'a>(
        client: &'a mut deadpool_postgres::Object,
        tenant: &str,
    ) -> Result<deadpool_postgres::Transaction<'a>, ChallengeError> {
        let tx = client.transaction().await.map_err(unavailable)?;
        tx.query(
            "SELECT set_config('app.tenant_id', $1, true), set_config('statement_timeout', '2s', true), \
             pg_advisory_xact_lock(hashtextextended('vibe-replay:' || $1, 0))",
            &[&tenant],
        )
        .await
        .map_err(unavailable)?;
        Ok(tx)
    }

    async fn count(
        tx: &deadpool_postgres::Transaction<'_>,
        sql: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<i64, ChallengeError> {
        Ok(tx.query_one(sql, params).await.map_err(unavailable)?.get(0))
    }

    async fn try_reserve(
        &self,
        key: &[u8; 32],
        ip: Option<&str>,
        tenant: &str,
    ) -> Result<ReplaySlot, ChallengeError> {
        let t = table_for(tenant)?;
        let live = live(&t);
        let k = key.as_slice();
        let mut client = self.pool.get().await.map_err(unavailable)?;
        // One lock per tenant serializes the checks below across instances.
        let tx = Self::begin(&mut client, tenant).await?;
        let used = Self::count(
            &tx,
            &format!(
                "SELECT count(*) FROM (SELECT 1 FROM {t} r WHERE r.key = $1 AND {live} LIMIT 1) s"
            ),
            &[&k],
        )
        .await?;
        if used > 0 {
            return Err(ChallengeError::Rejected(
                "challenge token already used".into(),
            ));
        }
        let ip_hash = ip.map(|ip| ip_digest(&self.ip_secret, tenant, ip));
        let unverified = format!("r.state IN ('pending', 'rejected') AND {live}");
        if let Some(h) = &ip_hash {
            let n = Self::count(
                &tx,
                &format!("SELECT count(*) FROM (SELECT 1 FROM {t} r WHERE r.ip_hash = $1 AND {unverified} LIMIT $2) s"),
                &[h, &self.per_ip],
            )
            .await?;
            if n >= self.per_ip {
                return Err(ChallengeError::Rejected(
                    "too many unverified challenge tokens from this client".into(),
                ));
            }
        }
        let n = Self::count(
            &tx,
            &format!("SELECT count(*) FROM (SELECT 1 FROM {t} r WHERE {unverified} LIMIT $1) s"),
            &[&self.per_tenant],
        )
        .await?;
        if n >= self.per_tenant {
            return Err(ChallengeError::Rejected(
                "too many unverified challenge tokens for this tenant".into(),
            ));
        }
        let n = Self::count(
            &tx,
            &format!("SELECT count(*) FROM (SELECT 1 FROM {t} r WHERE {live} LIMIT $1) s"),
            &[&self.capacity],
        )
        .await?;
        if n >= self.capacity {
            return Err(ChallengeError::Unavailable(
                "challenge replay cache is full".into(),
            ));
        }
        let nonce = *uuid::Uuid::new_v4().as_bytes();
        tx.execute(
            &format!(
                "INSERT INTO {t} (tenant_id, key, nonce, state, expires_at, ip_hash) \
                 VALUES ($1, $2, $3, 'pending', now() + make_interval(secs => $4), $5)"
            ),
            &[
                &tenant,
                &k,
                &nonce.as_slice(),
                &PENDING_HOLD.as_secs_f64(),
                &ip_hash,
            ],
        )
        .await
        .map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;
        Ok(ReplaySlot {
            key: *key,
            tenant: Some(tenant.to_string()),
            nonce,
        })
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
    ) -> Result<ReplaySlot, ChallengeError> {
        // Rows live in the tenant's schema: no tenant, no reservation.
        let tenant = tenant.ok_or_else(|| unavailable("no tenant for this request"))?;
        self.try_reserve(key, ip, tenant).await
    }

    async fn settle(&self, slot: &ReplaySlot, outcome: Outcome) -> Result<(), ChallengeError> {
        let tenant = slot
            .tenant
            .as_deref()
            .ok_or_else(|| unavailable("no tenant for this request"))?;
        let t = table_for(tenant)?;
        let live = live(&t);
        let (k, nonce) = (slot.key.as_slice(), slot.nonce.as_slice());
        let mut client = self.pool.get().await.map_err(unavailable)?;
        let tx = Self::begin(&mut client, tenant).await?;
        // Only this reservation, while it is still live and pending.
        let pending = Self::count(
            &tx,
            &format!("SELECT count(*) FROM {t} r WHERE r.key = $1 AND r.nonce = $2 AND r.state = 'pending' AND {live}"),
            &[&k, &nonce],
        )
        .await?;
        if pending == 0 {
            return match outcome {
                // The reservation lapsed; do not accept the token.
                Outcome::Verified => Err(ChallengeError::Unavailable(
                    "challenge reservation expired before it was recorded".into(),
                )),
                _ => Ok(()),
            };
        }
        let append = |state: &'static str, secs: f64| {
            format!(
                "INSERT INTO {t} (tenant_id, key, nonce, state, expires_at, ip_hash) \
                 SELECT tenant_id, key, nonce, '{state}', now() + make_interval(secs => {secs}), ip_hash \
                 FROM {t} WHERE key = $1 AND nonce = $2 AND state = 'pending'"
            )
        };
        let mut steps = Vec::new();
        match outcome {
            Outcome::Verified => steps.push(append("verified", self.ttl.as_secs_f64())),
            Outcome::Rejected if !self.rejected_hold.is_zero() => {
                steps.push(append("rejected", self.rejected_hold.as_secs_f64()))
            }
            Outcome::Rejected | Outcome::Released => {}
        }
        // The pending row stops counting once released; it expires with it.
        steps.push(append("released", PENDING_HOLD.as_secs_f64()));
        for sql in steps {
            tx.execute(&sql, &[&k, &nonce]).await.map_err(unavailable)?;
        }
        tx.commit().await.map_err(unavailable)?;
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
    fn migration_grants_only_select_and_insert() {
        assert!(PgReplayStore::migration_sql("Bad name", "app").is_err());
        let sql = PgReplayStore::migration_sql("tenant0001", "app\"x").unwrap();
        assert!(sql.iter().all(|s| !s.contains("vibe_platform")));
        assert!(sql.iter().any(|s| s
            == "GRANT SELECT, INSERT ON \"t_tenant0001\".vibe_challenge_replay TO \"app\"\"x\""));
        assert!(sql
            .iter()
            .filter(|s| s.starts_with("GRANT"))
            .all(|s| !s.contains("DELETE") && !s.contains("UPDATE")));
    }

    #[test]
    fn ip_digest_is_keyed_per_tenant() {
        let a = ip_digest(b"secret", "tenant0001", "203.0.113.1");
        assert_eq!(a, ip_digest(b"secret", "tenant0001", "203.0.113.1"));
        assert_ne!(a, ip_digest(b"secret", "tenant0002", "203.0.113.1"));
        assert_ne!(a, ip_digest(b"other", "tenant0001", "203.0.113.1"));
        assert_ne!(a, Sha256::digest(b"203.0.113.1").to_vec());
    }
}
