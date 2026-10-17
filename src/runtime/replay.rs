//! Single-use bookkeeping for challenge tokens behind a trait (#157).
//!
//! [`ReplayCache`](super::challenge::ReplayCache) keeps the state in process
//! memory and suits a single instance. [`PgReplayStore`] keeps it in a table
//! inside each tenant's own schema (`t_<tenant>.vibe_challenge_replay`,
//! created by that tenant's `vibe migrate`, `tenant_id` under FORCE RLS), so a
//! token, the per-IP and per-tenant quotas and the per-tenant capacity hold
//! across instances. No object is shared between tenants.
//!
//! The runtime role only has SELECT and INSERT on the table: the table is an
//! append-only log. A reservation is a `pending` row; settling it appends a
//! `verified`, `rejected` or `released` row. Reads ignore expired rows, and
//! only the owner role deletes them (during `vibe migrate`, and periodically
//! with `vibe replay-cleanup`, see [`cleanup_expired`]), so the runtime
//! cannot remove or shorten a live verified row. Each deletion run appends a
//! row to the owner-only audit table [`REPLAY_CLEANUP_AUDIT_TABLE`]. Every store error is
//! reported as [`ChallengeError::Unavailable`]: the request fails closed.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

use super::challenge::{
    ChallengeError, DEFAULT_PENDING_PER_IP, DEFAULT_PENDING_PER_TENANT, DEFAULT_REJECTED_HOLD,
    MAX_CLOCK_SKEW, TOKEN_LIFETIME,
};

/// Table in each tenant schema (created by `vibe migrate`).
pub const REPLAY_TABLE: &str = "vibe_challenge_replay";
/// Owner-only audit log of expired-row deletions (one row per run).
pub const REPLAY_CLEANUP_AUDIT_TABLE: &str = "vibe_challenge_replay_cleanup";
/// Extra time a verified row is kept beyond [`TOKEN_LIFETIME`] +
/// [`MAX_CLOCK_SKEW`]. It absorbs clock drift between the application hosts
/// (which run the token age check) and the database (whose `now()` sets and
/// checks `expires_at`): at least 60s and at least twice the allowed skew.
pub const RETENTION_MARGIN: Duration = {
    let twice_skew = MAX_CLOCK_SKEW.as_secs() * 2;
    Duration::from_secs(if twice_skew > 60 { twice_skew } else { 60 })
};
/// How long a verified row is kept. It must outlive the token: once a
/// verified row is deleted, reuse of that token is only refused by the
/// token age check, so deletion must never happen while the token itself is
/// still acceptable. A token is accepted for up to [`TOKEN_LIFETIME`] +
/// [`MAX_CLOCK_SKEW`] after verification (its `challenge_ts` may be up to
/// [`MAX_CLOCK_SKEW`] in the future), plus [`RETENTION_MARGIN`] for clock
/// drift between the application and the database.
pub const VERIFIED_RETENTION: Duration = Duration::from_secs(
    TOKEN_LIFETIME.as_secs() + MAX_CLOCK_SKEW.as_secs() + RETENTION_MARGIN.as_secs(),
);
const _: () =
    assert!(VERIFIED_RETENTION.as_secs() > TOKEN_LIFETIME.as_secs() + MAX_CLOCK_SKEW.as_secs());
const _: () = assert!(
    VERIFIED_RETENTION.as_secs() - TOKEN_LIFETIME.as_secs() - MAX_CLOCK_SKEW.as_secs() >= 60
);
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

/// One statement: delete this tenant's expired rows and append one audit row
/// with the count. `tenant` must already be validated; `tenant_value` is the
/// SQL for the tenant id (a literal, or `$1::text` when bound).
fn cleanup_sql(tenant: &str, tenant_value: &str, trigger: &str) -> String {
    let t = format!("\"t_{tenant}\".{REPLAY_TABLE}");
    let a = format!("\"t_{tenant}\".{REPLAY_CLEANUP_AUDIT_TABLE}");
    format!(
        "WITH d AS (DELETE FROM {t} WHERE tenant_id = {tenant_value} AND expires_at <= now() RETURNING 1) \
         INSERT INTO {a} (tenant_id, trigger, deleted) SELECT {tenant_value}, '{trigger}', count(*) FROM d \
         RETURNING deleted::text"
    )
}

/// Deletes one tenant's expired replay rows and records the run in
/// [`REPLAY_CLEANUP_AUDIT_TABLE`], in one transaction. Returns the number of
/// rows deleted.
///
/// `exec` must be the tenant owner connection (the one `vibe migrate` uses);
/// the runtime role has no DELETE (decision 27) and is refused here even if
/// it were granted one. Only rows whose `expires_at` has passed are removed,
/// and reads already ignore them, so verification never depends on this
/// running: an expired token is refused by its age check either way.
pub async fn cleanup_expired(
    exec: &super::PgExec,
    tenant: &str,
    runtime_role: &str,
) -> Result<u64, String> {
    if !super::slugs::valid_tenant(tenant) {
        return Err("invalid tenant id".into());
    }
    let mut session = exec.session().await?;
    let who = session
        .query_texts(
            "SELECT current_user::text || '|' || pg_has_role(current_user, $1::text, 'MEMBER')::text \
             FROM pg_roles WHERE rolname = $1::text",
            runtime_role,
        )
        .await?;
    // A runtime role that does not exist is a configuration error, not a
    // pass: the membership check would have nothing to compare against.
    let Some(who) = who.first() else {
        return Err(format!(
            "VIBE_RUNTIME_DB_ROLE {runtime_role:?} does not exist in this database"
        ));
    };
    let (user, member) = who.split_once('|').unwrap_or((who.as_str(), "true"));
    if user == runtime_role || member != "false" {
        return Err(format!(
            "replay cleanup must run as the tenant owner role, not the runtime role {runtime_role} (connected as {user})"
        ));
    }
    session.in_transaction(true);
    session.batch(&["BEGIN".to_string()]).await?;
    let result = async {
        session
            .query_texts("SELECT set_config('app.tenant_id', $1, true)", tenant)
            .await?;
        let deleted = session
            .query_texts(&cleanup_sql(tenant, "$1::text", "scheduled"), tenant)
            .await?;
        let n: u64 = deleted
            .first()
            .and_then(|v| v.parse().ok())
            .ok_or("replay cleanup returned no count")?;
        Ok::<u64, String>(n)
    }
    .await;
    match result {
        Ok(n) => {
            session.batch(&["COMMIT".to_string()]).await?;
            session.in_transaction(false);
            Ok(n)
        }
        Err(err) => {
            if session.batch(&["ROLLBACK".to_string()]).await.is_ok() {
                session.in_transaction(false);
            }
            Err(err)
        }
    }
}

impl PgReplayStore {
    /// `ip_secret` keys the per-tenant IP digest (the API-key pepper).
    pub fn new(pool: deadpool_postgres::Pool, ip_secret: &[u8]) -> Self {
        Self {
            pool,
            ip_secret: ip_secret.to_vec(),
            ttl: VERIFIED_RETENTION,
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

    /// DDL for one tenant's table, run inside that tenant's `vibe migrate`
    /// as the owner role (before the schema's RLS pass, which adds ENABLE +
    /// FORCE RLS and the tenant policy). The runtime role gets SELECT and
    /// INSERT only. Also removes expired rows (as the owner).
    pub fn migration_sql(tenant: &str, runtime_role: &str) -> Result<Vec<String>, String> {
        if !super::slugs::valid_tenant(tenant) {
            return Err("invalid tenant id".into());
        }
        let role = quote_ident(runtime_role);
        let t = format!("\"t_{tenant}\".{REPLAY_TABLE}");
        let a = format!("\"t_{tenant}\".{REPLAY_CLEANUP_AUDIT_TABLE}");
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
            format!(
                "CREATE TABLE IF NOT EXISTS {a} (tenant_id text NOT NULL, ran_at timestamptz NOT NULL DEFAULT now(), \
                 run_by text NOT NULL DEFAULT current_user, trigger text NOT NULL, deleted bigint NOT NULL)"
            ),
            format!("REVOKE ALL ON {a} FROM PUBLIC"),
            format!("REVOKE ALL ON {a} FROM {role}"),
            format!("ALTER TABLE {a} ENABLE ROW LEVEL SECURITY"),
            format!("ALTER TABLE {a} FORCE ROW LEVEL SECURITY"),
        ])
    }

    /// Run by `vibe migrate` after the tenant policies are installed: delete
    /// expired rows and audit the run (trigger `migrate`).
    pub fn migrate_cleanup_sql(tenant: &str) -> Result<Vec<String>, String> {
        if !super::slugs::valid_tenant(tenant) {
            return Err("invalid tenant id".into());
        }
        Ok(vec![
            format!("SELECT set_config('app.tenant_id', '{tenant}', true)"),
            cleanup_sql(tenant, &format!("'{tenant}'"), "migrate"),
        ])
    }

    /// Startup check run by `vibe serve`: every listed tenant has the table,
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
                    format!("challenge replay table {t} is not usable ({err}); run `vibe migrate` for this tenant")
                })?;
            let (usable, too_much): (bool, bool) = (row.get(0), row.get(1));
            if !usable {
                return Err(format!(
                    "challenge replay table {t} is not readable and writable by this role; run `vibe migrate` for this tenant"
                ));
            }
            if too_much {
                return Err(format!(
                    "challenge replay table {t} grants DELETE, UPDATE or TRUNCATE to this role; run `vibe migrate` to reset its grants"
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
    fn verified_rows_outlive_the_token() {
        // Cleanup may delete a verified row only once the token it records
        // is already refused by the age check.
        assert!(VERIFIED_RETENTION > TOKEN_LIFETIME + MAX_CLOCK_SKEW);
        assert!(RETENTION_MARGIN >= Duration::from_secs(60));
        assert!(RETENTION_MARGIN >= MAX_CLOCK_SKEW * 2);
        assert_eq!(
            VERIFIED_RETENTION,
            TOKEN_LIFETIME + MAX_CLOCK_SKEW + RETENTION_MARGIN
        );
    }

    /// Steps app time second by second from verification until well past
    /// the last moment the token is accepted, with the database clock
    /// `db_ahead` ahead of the app, and checks the verified row (whose
    /// `expires_at` is the database's `now() + VERIFIED_RETENTION` at
    /// verification) is live whenever the age check accepts the token.
    fn assert_row_live_while_token_accepted(
        db_ahead: chrono::Duration,
        ts_offset: chrono::Duration,
    ) {
        use super::super::challenge::check_token_age;
        let app_verify = chrono::DateTime::parse_from_rfc3339("2026-10-16T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let issued = (app_verify + ts_offset).fixed_offset();
        assert!(check_token_age(app_verify, issued).is_ok());
        let expires_at =
            app_verify + db_ahead + chrono::Duration::from_std(VERIFIED_RETENTION).unwrap();
        let horizon = (TOKEN_LIFETIME + MAX_CLOCK_SKEW + VERIFIED_RETENTION).as_secs() as i64;
        let mut last_accepted = None;
        for s in 0..=horizon {
            let app_now = app_verify + chrono::Duration::seconds(s);
            if check_token_age(app_now, issued).is_ok() {
                let db_now = app_now + db_ahead;
                assert!(
                    expires_at > db_now,
                    "row expired at app +{s}s while the token is still accepted"
                );
                last_accepted = Some(s);
            }
        }
        // The token is accepted until challenge_ts + TOKEN_LIFETIME.
        let expected =
            (ts_offset + chrono::Duration::from_std(TOKEN_LIFETIME).unwrap()).num_seconds();
        assert_eq!(last_accepted, Some(expected));
    }

    #[test]
    fn verified_row_covers_future_skew_with_db_clock_ahead() {
        let skew = chrono::Duration::from_std(MAX_CLOCK_SKEW).unwrap();
        let margin = chrono::Duration::from_std(RETENTION_MARGIN).unwrap();
        // challenge_ts at the future skew limit, database clock ahead of the
        // app by up to the full margin (just under it, since the comparison
        // with expires_at is strict).
        for db_ahead in [
            chrono::Duration::zero(),
            skew,
            margin - chrono::Duration::seconds(1),
        ] {
            assert_row_live_while_token_accepted(db_ahead, skew);
            assert_row_live_while_token_accepted(db_ahead, chrono::Duration::zero());
            assert_row_live_while_token_accepted(db_ahead, -skew);
        }
    }

    #[test]
    fn cleanup_deletes_only_expired_rows_and_audits() {
        let sql = cleanup_sql("tenant0001", "$1::text", "scheduled");
        assert!(sql.contains("DELETE FROM \"t_tenant0001\".vibe_challenge_replay WHERE tenant_id = $1::text AND expires_at <= now()"));
        assert!(sql.contains("INSERT INTO \"t_tenant0001\".vibe_challenge_replay_cleanup"));
        let migration = PgReplayStore::migration_sql("tenant0001", "app").unwrap();
        // The audit table is owner-only: nothing is granted to the runtime role.
        assert!(migration
            .iter()
            .any(|s| s
                == "REVOKE ALL ON \"t_tenant0001\".vibe_challenge_replay_cleanup FROM \"app\""));
        assert!(!migration
            .iter()
            .any(|s| s.starts_with("GRANT") && s.contains("vibe_challenge_replay_cleanup")));
        assert!(PgReplayStore::migrate_cleanup_sql("tenant0001")
            .unwrap()
            .last()
            .unwrap()
            .contains("'tenant0001', 'migrate', count(*)"));
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
