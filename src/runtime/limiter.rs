//! Bounded in-memory rate-limit and quota state.
//!
//! Buckets are sharded by `(route, key type)`. Every shard has a fixed
//! capacity. When a shard is full, its least recently used bucket is evicted
//! in O(1) amortized time, so the limiter never refuses requests just
//! because it is full. Flooding one shard with junk keys (e.g. rotating
//! `input.email` values) only evicts buckets in that shard, and only by
//! recency. It never blocks other clients and never needs a full scan while
//! the lock is held.
//!
//! Buckets at their limit are pinned until their window ends (evicting them
//! would reset the counter). To stop one client network from filling a shard
//! with pinned buckets, every rate bucket is charged to the network that
//! created it (an IPv4 address or an IPv6 /48, see [`owner_of`]), and one
//! network may hold at most [`OWNER_CAP`] active buckets per shard, each
//! wider network (IPv4 /24, IPv6 /32) at most [`NETWORK_CAP`], and the last
//! [`RESERVE`] slots only go to clients that hold few buckets.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// New keys refused because a shard was saturated, since process start.
/// Read with [`saturation_refusals`]; see docs/release-hardening.md.
static SATURATION_REFUSALS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Total new keys refused because a rate-limit shard was saturated.
pub fn saturation_refusals() -> u64 {
    SATURATION_REFUSALS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Buckets per `(route, key type)` shard.
pub const SHARD_CAPACITY: usize = 10_000;
pub const WINDOW: Duration = Duration::from_secs(60);
/// Pinned entries examined per eviction before refusing a new key.
const EVICT_SCAN: usize = 64;
/// Active buckets one client network (IPv4 address or IPv6 /48) may own in a
/// single shard.
pub const OWNER_CAP: usize = SHARD_CAPACITY / 100;
/// Active buckets one wider network (IPv4 /24 or IPv6 /32) may own in a
/// single shard, so many addresses from one block cannot fill it either.
pub const NETWORK_CAP: usize = SHARD_CAPACITY / 10;
/// The last `RESERVE` slots of a shard are kept for clients that hold few
/// buckets: once a shard is that full, a new bucket is only created for a
/// network below `RESERVE_OWNER_CAP` (and its wider network below
/// `RESERVE_NETWORK_CAP`). New clients keep getting buckets even after heavy
/// clients have used up everything else.
pub const RESERVE: usize = SHARD_CAPACITY / 10;
/// Reserve caps for IPv4: an address below `RESERVE_OWNER_CAP`, its /24 below
/// `RESERVE_NETWORK_CAP`.
pub const RESERVE_OWNER_CAP: usize = 5;
pub const RESERVE_NETWORK_CAP: usize = 10;
/// Reserve caps for IPv6: a /48 below `RESERVE_V6_48_CAP`, its /32 below
/// `RESERVE_V6_32_CAP` in total across all of its /48s.
pub const RESERVE_V6_48_CAP: usize = 10;
pub const RESERVE_V6_32_CAP: usize = 50;

/// `(network cap, wider network cap)` inside the reserve for an
/// [`owner_of`] string.
fn reserve_caps(owner: &str) -> (usize, usize) {
    if owner.ends_with("/48") {
        (RESERVE_V6_48_CAP, RESERVE_V6_32_CAP)
    } else {
        (RESERVE_OWNER_CAP, RESERVE_NETWORK_CAP)
    }
}

/// A map with a fixed capacity and least-recently-used eviction.
pub struct BoundedMap<V> {
    capacity: usize,
    entries: HashMap<String, (u64, V)>,
    order: VecDeque<(String, u64)>,
    generation: u64,
}

impl<V> BoundedMap<V> {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashMap::new(),
            order: VecDeque::new(),
            generation: 0,
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.entries.get(key).map(|(_, v)| v)
    }

    /// Returns the entry for `key`, inserting `make()` (and evicting the least
    /// recently used entry if full). Marks the entry as most recently used.
    pub fn touch(&mut self, key: &str, make: impl FnOnce() -> V) -> &mut V {
        self.touch_pinned(key, make, |_| true)
            .expect("every entry is evictable")
    }

    /// Like [`touch`](Self::touch), but entries for which `evictable` is false
    /// are never evicted. When the map is full and no evictable entry is found
    /// within a bounded scan, a *new* key is refused (`None`); existing keys
    /// always succeed.
    pub fn touch_pinned(
        &mut self,
        key: &str,
        make: impl FnOnce() -> V,
        evictable: impl Fn(&V) -> bool,
    ) -> Option<&mut V> {
        self.touch_pinned_with(key, make, evictable, |_| {})
    }

    /// Like [`touch_pinned`](Self::touch_pinned); `on_evict` receives the
    /// entry evicted to make room, if any.
    pub fn touch_pinned_with(
        &mut self,
        key: &str,
        make: impl FnOnce() -> V,
        evictable: impl Fn(&V) -> bool,
        on_evict: impl FnOnce(V),
    ) -> Option<&mut V> {
        self.generation += 1;
        let generation = self.generation;
        if !self.entries.contains_key(key) {
            if self.entries.len() >= self.capacity {
                match self.evict_one(&evictable) {
                    Some(old) => on_evict(old),
                    None => return None,
                }
            }
            self.entries.insert(key.to_string(), (generation, make()));
        }
        self.order.push_back((key.to_string(), generation));
        // Stale order records (older generations) are skipped on eviction. When
        // they pile up, compact once; that is O(n) but amortized O(1) per touch.
        if self.order.len() > self.capacity * 4 {
            let entries = &self.entries;
            self.order
                .retain(|(k, g)| entries.get(k).is_some_and(|(cur, _)| cur == g));
        }
        let entry = self.entries.get_mut(key).expect("present");
        entry.0 = generation;
        Some(&mut entry.1)
    }

    /// Evicts the least recently used evictable entry. Pinned entries met on
    /// the way are moved to the back; the scan is bounded (`EVICT_SCAN`) so a
    /// full, pinned shard costs O(1) per request, not O(n).
    fn evict_one(&mut self, evictable: &impl Fn(&V) -> bool) -> Option<V> {
        let mut pinned_seen = 0;
        while let Some((key, generation)) = self.order.pop_front() {
            let Some((g, value)) = self.entries.get(&key) else {
                continue;
            };
            if *g != generation {
                continue;
            }
            if evictable(value) {
                return self.entries.remove(&key).map(|(_, v)| v);
            }
            self.order.push_back((key, generation));
            pinned_seen += 1;
            if pinned_seen >= EVICT_SCAN {
                return None;
            }
        }
        None
    }

    /// Removes `key`. Its order records go stale and are skipped later.
    pub fn remove(&mut self, key: &str) -> Option<V> {
        self.entries.remove(key).map(|(_, v)| v)
    }
}

/// Sliding one-minute windows, sharded by `(route, key type)`, plus the D130
/// tenant and platform network-range layers.
pub struct RateBuckets {
    shards: HashMap<String, RateShard>,
    net: NetLayers,
    memory: std::sync::Arc<LimiterMemory>,
}

/// Default hard cap on the estimated memory of all limiter tables combined
/// (`VIBE_RATE_TABLE_MEMORY_MAX_BYTES`, 256 MiB).
pub const TABLE_MEMORY_MAX_BYTES: usize = 256 * 1024 * 1024;

/// Estimated heap bytes per D130 counter slot: about 480 measured
/// (tests/limiter_memory.rs) with one record per window queue, doubled to
/// cover queues at their worst-case length (4 records per slot) and hash
/// table growth. See docs/release-hardening.md.
pub const COUNTER_SLOT_BYTES: usize = 1_024;
/// Estimated bytes per rate bucket, excluding its hit timestamps, including
/// per-owner bookkeeping (about 990 measured with 5 hits; rounded up).
pub const BUCKET_BYTES: usize = 1_024;
/// Bytes per allowed hit of a bucket (`Instant`), doubled for `Vec` growth.
pub const HIT_BYTES: usize = 2 * std::mem::size_of::<Instant>();
/// Estimated bytes per daily-quota entry (about 200 measured; rounded up).
pub const QUOTA_ENTRY_BYTES: usize = 256;

/// Shared hard memory budget for every limiter table (rate shards, D130
/// tables, daily quotas). Estimates are conservative upper bounds per entry.
#[derive(Debug)]
pub struct LimiterMemory {
    max: usize,
    quota: std::sync::atomic::AtomicUsize,
}

impl LimiterMemory {
    pub fn new(max: usize) -> Self {
        Self {
            max,
            quota: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn max_bytes(&self) -> usize {
        self.max
    }

    fn quota_bytes(&self) -> usize {
        self.quota.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Default for RateBuckets {
    fn default() -> Self {
        Self::with_limits(NetLimits::default())
    }
}

/// One shard plus the per-network accounting of who created its buckets.
struct RateShard {
    map: BoundedMap<RateWindow>,
    /// Live buckets per owner network.
    owned: HashMap<String, usize>,
    /// Keys per owner in creation order (may hold stale keys; pruned lazily).
    owner_keys: HashMap<String, VecDeque<String>>,
    /// Last time saturation was logged for this shard.
    saturated_logged: Option<Instant>,
    /// Refusals since the last saturation log line.
    refused_since_log: u64,
    /// Largest per-minute limit of any bucket created (memory estimate).
    max_limit: u32,
}

impl RateShard {
    fn new() -> Self {
        Self {
            map: BoundedMap::new(SHARD_CAPACITY),
            owned: HashMap::new(),
            owner_keys: HashMap::new(),
            saturated_logged: None,
            refused_since_log: 0,
            max_limit: 0,
        }
    }

    fn bucket_bytes(limit: u32) -> usize {
        BUCKET_BYTES + HIT_BYTES * limit as usize
    }

    fn bytes(&self) -> usize {
        self.map.len() * Self::bucket_bytes(self.max_limit)
    }

    fn release(owned: &mut HashMap<String, usize>, owners: &[String; 2]) {
        for owner in owners {
            if let Some(n) = owned.get_mut(owner) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    owned.remove(owner);
                }
            }
        }
    }

    fn count(&self, owner: &str) -> usize {
        self.owned.get(owner).copied().unwrap_or(0)
    }

    /// Frees `owner`'s buckets that have no hit in the current window
    /// (removing them loses no state) until it owns fewer than `cap`, with a
    /// bounded scan. Returns whether it is below `cap`.
    fn prune_below(&mut self, owner: &str, cap: usize, now: Instant) -> bool {
        if self.count(owner) < cap {
            return true;
        }
        let Some(mut keys) = self.owner_keys.remove(owner) else {
            return false;
        };
        let mut scanned = 0;
        while scanned < EVICT_SCAN && self.count(owner) >= cap {
            let Some(key) = keys.pop_front() else { break };
            scanned += 1;
            let Some(window) = self.map.get(&key) else {
                continue; // evicted or removed already
            };
            if !window.owners.iter().any(|o| o == owner) {
                continue;
            }
            if window.is_idle(now) {
                if let Some(old) = self.map.remove(&key) {
                    Self::release(&mut self.owned, &old.owners);
                }
            } else {
                keys.push_back(key);
            }
        }
        self.owner_keys.insert(owner.to_string(), keys);
        self.count(owner) < cap
    }

    /// Whether a new bucket may be charged to `owners`; `Err` names the
    /// reason for the saturation log.
    fn admit(&mut self, owners: &[String; 2], now: Instant) -> Result<(), &'static str> {
        if !self.prune_below(&owners[0], OWNER_CAP, now) {
            return Err("owner_cap");
        }
        if !self.prune_below(&owners[1], NETWORK_CAP, now) {
            return Err("network_cap");
        }
        let (own_cap, net_cap) = reserve_caps(&owners[0]);
        if self.map.entries.len() >= SHARD_CAPACITY - RESERVE
            && (!self.prune_below(&owners[0], own_cap, now)
                || !self.prune_below(&owners[1], net_cap, now))
        {
            return Err("reserve");
        }
        Ok(())
    }

    fn insert(&mut self, key: &str, owners: &[String; 2], limit: u32, now: Instant) -> bool {
        let owned = &mut self.owned;
        let made = self.map.touch_pinned_with(
            key,
            || RateWindow {
                limit,
                hits: Vec::new(),
                owners: owners.clone(),
            },
            |w| w.evictable(now),
            |old| Self::release(owned, &old.owners),
        );
        if made.is_none() {
            return false;
        }
        for owner in owners {
            *self.owned.entry(owner.clone()).or_insert(0) += 1;
            let live = self.count(owner);
            let keys = self.owner_keys.entry(owner.clone()).or_default();
            keys.push_back(key.to_string());
            if keys.len() > live * 4 + 16 {
                let map = &self.map;
                keys.retain(|k| map.get(k).is_some_and(|w| w.owners.contains(owner)));
            }
        }
        if self.owner_keys.len() > self.owned.len() * 2 + 64 {
            let owned = &self.owned;
            self.owner_keys.retain(|o, _| owned.contains_key(o));
        }
        true
    }

    fn log_saturation(&mut self, shard: &str, reason: &'static str, now: Instant) {
        SATURATION_REFUSALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.refused_since_log += 1;
        if self
            .saturated_logged
            .is_some_and(|at| now.duration_since(at) < WINDOW)
        {
            return;
        }
        self.saturated_logged = Some(now);
        // Stable target and fields for alerting (docs/release-hardening.md).
        tracing::warn!(
            target: "vibe::rate_limit_saturation",
            shard,
            reason,
            buckets = self.map.entries.len(),
            owners = self.owned.len(),
            refused = self.refused_since_log,
            refused_total = saturation_refusals(),
            "rate-limit shard saturated; refusing new keys"
        );
        self.refused_since_log = 0;
    }
}

/// Hits in the current window, the limit they are counted against, and the
/// client network that created the bucket.
pub struct RateWindow {
    limit: u32,
    hits: Vec<Instant>,
    /// `[network, wider network]` the bucket is charged to.
    owners: [String; 2],
}

impl RateWindow {
    /// No hit in the current window: the bucket carries no state.
    fn is_idle(&self, now: Instant) -> bool {
        self.hits.iter().all(|at| now.duration_since(*at) >= WINDOW)
    }

    /// A bucket at its limit whose window is still open must not be evicted:
    /// evicting it would reset the counter and let a client rotate junk keys
    /// to bypass its own limit.
    fn evictable(&self, now: Instant) -> bool {
        let live = self
            .hits
            .iter()
            .filter(|at| now.duration_since(**at) < WINDOW)
            .count();
        (live as u32) < self.limit
    }
}

/// Outcome of a rate check.
pub enum RateDecision {
    Allow,
    /// Seconds until the client may retry.
    Limited(u64),
}

impl RateBuckets {
    pub fn with_limits(limits: NetLimits) -> Self {
        let memory = std::sync::Arc::new(LimiterMemory::new(limits.table_memory_max_bytes));
        Self {
            shards: HashMap::new(),
            net: NetLayers::new(limits),
            memory,
        }
    }

    /// The memory budget shared with [`QuotaBuckets::with_memory`].
    pub fn memory(&self) -> std::sync::Arc<LimiterMemory> {
        self.memory.clone()
    }

    /// Estimated bytes of every limiter table sharing this budget.
    pub fn estimated_bytes(&self) -> usize {
        self.shard_bytes() + self.net.bytes() + self.memory.quota_bytes()
    }

    fn shard_bytes(&self) -> usize {
        self.shards.values().map(RateShard::bytes).sum()
    }

    /// Live D130 counter slots `(tenant, platform)` (measurement hook).
    #[doc(hidden)]
    pub fn counter_slots(&self) -> (usize, usize) {
        (
            self.net.tenants.entries.len(),
            self.net.platform.entries.len(),
        )
    }

    /// Shard logic only (D130 layers off), for the #73 tests.
    #[cfg(test)]
    fn shard_only() -> Self {
        Self::with_limits(NetLimits {
            tenant_net_cap: 0,
            ..NetLimits::default()
        })
    }

    /// [`check_tenant`](Self::check_tenant) for one fixed tenant.
    #[cfg(test)]
    fn check(
        &mut self,
        checks: &[(String, String, u32)],
        owner: &str,
        now: Instant,
    ) -> RateDecision {
        self.check_tenant(checks, "t0", owner, now)
    }

    /// `checks` = `(shard, bucket key, per-minute limit)`; `owner` is the
    /// client network ([`owner_of`]) that new buckets are charged to, within
    /// `tenant` (D130). The request is counted only when every bucket and
    /// both D130 layers admit it.
    pub fn check_tenant(
        &mut self,
        checks: &[(String, String, u32)],
        tenant: &str,
        owner: &str,
        now: Instant,
    ) -> RateDecision {
        let mut worst: Option<u64> = None;
        for (shard, key, limit) in checks {
            let Some(rs) = self.shards.get_mut(shard) else {
                continue;
            };
            let map = &mut rs.map;
            if map.get(key).is_none() {
                continue;
            }
            let window = map
                .touch_pinned(key, || unreachable!("existing key"), |w| w.evictable(now))
                .expect("existing key");
            window.limit = *limit;
            let hits = &mut window.hits;
            hits.retain(|at| now.duration_since(*at) < WINDOW);
            if hits.len() as u32 >= *limit {
                let wait = hits.first().map(|t| retry_after(*t, now)).unwrap_or(1);
                worst = Some(worst.map_or(wait, |w| w.max(wait)));
            }
        }
        if let Some(wait) = worst {
            return RateDecision::Limited(wait);
        }
        // New keys: refuse when the creating network or its wider network is
        // over its cap in that shard, when the shard is into its reserve and
        // the client already holds several buckets, or when the shard is full
        // of pinned buckets. Nothing is counted or created on refusal.
        let owners = owner_levels(owner);
        for (shard, key, _) in checks {
            let rs = self
                .shards
                .entry(shard.clone())
                .or_insert_with(RateShard::new);
            if rs.map.get(key).is_some() {
                continue;
            }
            if let Err(reason) = rs.admit(&owners, now) {
                rs.log_saturation(shard, reason, now);
                return RateDecision::Limited(WINDOW.as_secs());
            }
        }
        // D130: tenant and platform network-range slot counters. Same
        // response for either layer.
        let new = checks
            .iter()
            .filter(|(shard, key, _)| self.shards[shard].map.get(key).is_none())
            .map(|(shard, key, _)| (shard.as_str(), key.as_str()))
            .collect::<std::collections::HashSet<_>>()
            .len() as u32;
        let new_bucket_bytes: usize = checks
            .iter()
            .filter(|(shard, key, _)| self.shards[shard].map.get(key).is_none())
            .map(|(shard, _, limit)| {
                RateShard::bucket_bytes((*limit).max(self.shards[shard].max_limit))
            })
            .sum();
        self.net.base_bytes = self.shard_bytes() + self.memory.quota_bytes() + new_bucket_bytes;
        self.net.max_bytes = self.memory.max;
        match self.net.charge(tenant, owner, new, now) {
            Ok(()) => {}
            Err(NetRefusal::Cap(layer, wait)) => {
                record_slot_refused(layer, "network_cap");
                return RateDecision::Limited(wait);
            }
            Err(NetRefusal::SlotsFull(layer, reason, wait)) => {
                self.net.log_full(layer, reason, now);
                return RateDecision::Limited(wait);
            }
        }
        for (shard, key, limit) in checks {
            let rs = self.shards.get_mut(shard).expect("created above");
            if rs.map.get(key).is_some() {
                continue;
            }
            rs.max_limit = rs.max_limit.max(*limit);
            if !rs.insert(key, &owners, *limit, now) {
                rs.log_saturation(shard, "pinned", now);
                return RateDecision::Limited(WINDOW.as_secs());
            }
        }
        for (shard, key, _) in checks {
            if let Some(w) = self
                .shards
                .get_mut(shard)
                .and_then(|rs| rs.map.entries.get_mut(key))
            {
                w.1.hits.push(now);
            }
        }
        RateDecision::Allow
    }
}

// ---------------------------------------------------------------------------
// D130 (#132): tenant-scoped and platform network-range slot counters.
//
// The per-shard caps above are shared by every tenant on a route. D130 adds
// two layers that count NEW rate buckets (slot reservations, the same meaning
// as the #73 reserve caps) per network range in fixed windows of [`WINDOW`]:
//
// * tenant layer: `(tenant, IPv4 /24)` cap 10; `(tenant, IPv6 /48)` cap 10
//   plus `(tenant, IPv6 /32)` cap 50;
// * platform layer: `IPv4 /24` and `IPv6 /48` across all tenants, cap
//   `platform_factor` (default 5) times the tenant cap.
//
// A request that creates new buckets must pass both layers; requests that
// only hit existing buckets are not counted here. Each tenant has a
// guaranteed floor in both layers (see below). A refusal is a plain 429 with `Retry-After`; the caller
// cannot tell which layer refused.
//
// Fixed windows apply only to these counters; every other limit stays
// sliding-window. A client can get up to 2x a cap across a window edge
// (accepted risk, docs/release-hardening.md).
//
// Counter slots are bounded and partitioned: tenant counters by tenant
// (`slots_per_tenant` each), platform counters by network aggregate (IPv4
// /24, IPv6 /32; `platform_slots_per_aggregate` each). A full partition only
// refuses requests charged to that partition. The global tenant budget is
// sized for `expected_tenants` full partitions, and a tenant below
// `tenant_slot_floor` slots is admitted even past it. The platform total is a
// memory bound shared by all tenants; every platform counter is also charged
// to the tenant that created it, and a tenant holding fewer than
// `platform_slot_floor` platform counters is admitted even when the shared
// platform total is used up (only above-floor growth is refused there). The
// platform total is sized as `tenant budget + expected_tenants x
// platform_slot_floor` ([`NetLimits::sized_for`]). The memory cap keeps
// `floor_reserve_bytes` for floor growth: requests that stay within the
// tenant's floors are checked against the full cap, everything else against
// the cap minus the reserve. Expired counters are cleared first (oldest
// first); a counter inside its window is NEVER evicted. When no slot can be freed the request
// fails closed (429) and `vibe_rate_limit_slot_refused_total{layer,
// reason="table_full"}` is incremented ([`slot_refused_total`]; the older
// [`counter_slot_refusals`] is kept as an alias). Network-cap refusals count
// under `reason="network_cap"`.
//
// State is per process: multi-instance deploys need shared storage (#157).
// ---------------------------------------------------------------------------

/// Requests refused because D130 counter slots were full of live counters.
static COUNTER_SLOT_REFUSALS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Total requests refused (fail closed) because D130 counter slots or the
/// limiter memory budget were exhausted: the sum of
/// `vibe_rate_limit_slot_refused_total` over every reason but `network_cap`.
pub fn counter_slot_refusals() -> u64 {
    COUNTER_SLOT_REFUSALS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Name of the D130 refusal metric.
pub const SLOT_REFUSED_METRIC: &str = "vibe_rate_limit_slot_refused_total";
const SLOT_LAYERS: [&str; 2] = ["tenant", "platform"];
const SLOT_REASONS: [&str; 4] = [
    "network_cap",
    "table_full",
    "tenant_budget_exhausted",
    "prefix_cap_exceeded",
];
#[allow(clippy::declare_interior_mutable_const)]
const ZERO: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SLOT_REFUSED: [std::sync::atomic::AtomicU64; 8] = [ZERO; 8];

fn slot_index(layer: &str, reason: &str) -> Option<usize> {
    let l = SLOT_LAYERS.iter().position(|x| *x == layer)?;
    let r = SLOT_REASONS.iter().position(|x| *x == reason)?;
    Some(l * SLOT_REASONS.len() + r)
}

fn record_slot_refused(layer: &str, reason: &str) {
    if let Some(i) = slot_index(layer, reason) {
        SLOT_REFUSED[i].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// `vibe_rate_limit_slot_refused_total{layer, reason}`: `layer` is `tenant`
/// or `platform`; `reason` is `network_cap` (a network over its new-slot
/// cap), `table_full` (a tenant's own partition, the platform total or the
/// memory budget is full), `tenant_budget_exhausted` (the global tenant
/// budget is used up) or `prefix_cap_exceeded` (a platform IPv4 /24 or IPv6
/// /32 partition is full). Internal only;
/// responses never name the layer.
pub fn slot_refused_total(layer: &str, reason: &str) -> u64 {
    slot_index(layer, reason).map_or(0, |i| {
        SLOT_REFUSED[i].load(std::sync::atomic::Ordering::Relaxed)
    })
}

/// The refusal metric in Prometheus text format.
pub fn slot_refused_metrics() -> String {
    let mut out = format!("# TYPE {SLOT_REFUSED_METRIC} counter\n");
    for layer in SLOT_LAYERS {
        for reason in SLOT_REASONS {
            out.push_str(&format!(
                "{SLOT_REFUSED_METRIC}{{layer=\"{layer}\",reason=\"{reason}\"}} {}\n",
                slot_refused_total(layer, reason)
            ));
        }
    }
    out
}

/// New-bucket caps per window for one tenant: IPv4 /24 and IPv6 /48.
pub const TENANT_NET_CAP: u32 = 10;
/// New-bucket cap per window for one tenant's IPv6 /32.
pub const TENANT_V6_32_CAP: u32 = 50;
/// Default platform cap multiplier (platform cap = factor x tenant cap).
pub const PLATFORM_FACTOR: u32 = 5;
/// Largest accepted platform cap multiplier.
pub const MAX_PLATFORM_FACTOR: u32 = 100;
/// Counter slots one tenant may hold.
pub const SLOTS_PER_TENANT: usize = 4_096;
/// Default number of tenants the global tenant slot budget is sized for.
pub const EXPECTED_TENANTS: usize = 256;
/// Largest accepted `expected_tenants`.
pub const MAX_EXPECTED_TENANTS: usize = 4_096;
/// Slots a tenant always gets, even when the global budget is used up.
pub const TENANT_SLOT_FLOOR: usize = 64;
/// Platform counter slots one network aggregate (IPv4 /24, IPv6 /32) may hold.
pub const PLATFORM_SLOTS_PER_AGGREGATE: usize = 4_096;
/// Platform counter slots across all aggregates (memory bound) for the
/// default sizing: the default tenant budget plus every expected tenant's
/// platform floor.
pub const PLATFORM_SLOTS_TOTAL: usize =
    EXPECTED_TENANTS * SLOTS_PER_TENANT + EXPECTED_TENANTS * PLATFORM_SLOT_FLOOR;
/// Platform counters a tenant always gets, even when the shared platform
/// total is used up.
pub const PLATFORM_SLOT_FLOOR: usize = 64;
/// Estimated bytes reserved per floor slot in the memory cap: one tenant
/// counter, one platform counter and one rate bucket.
pub const FLOOR_SLOT_BYTES: usize = 2 * COUNTER_SLOT_BYTES + BUCKET_BYTES;
/// Default memory reserved for floor growth: `EXPECTED_TENANTS x
/// PLATFORM_SLOT_FLOOR x FLOOR_SLOT_BYTES` (48 MiB at 256 tenants).
pub const FLOOR_RESERVE_BYTES: usize = EXPECTED_TENANTS * PLATFORM_SLOT_FLOOR * FLOOR_SLOT_BYTES;

/// D130 limits; see the section comment above.
#[derive(Clone, Debug)]
pub struct NetLimits {
    /// `0` disables the D130 layers (tests of the shard logic only).
    pub tenant_net_cap: u32,
    pub tenant_v6_32_cap: u32,
    pub platform_factor: u32,
    /// Counter slots one tenant may hold (its partition).
    pub slots_per_tenant: usize,
    /// Global tenant slot budget; at least `expected_tenants x
    /// slots_per_tenant` (see [`NetLimits::sized_for`]).
    pub tenant_slots_total: usize,
    /// Slots every tenant may use even when the global budget is used up, so
    /// a tenant with few counters is never refused because of other tenants.
    pub tenant_slot_floor: usize,
    /// Platform counter slots one network aggregate may hold (its partition).
    pub platform_slots_per_aggregate: usize,
    /// Platform counter slots across all aggregates.
    pub platform_slots: usize,
    /// Platform counters every tenant may create even when the shared
    /// platform total is used up.
    pub platform_slot_floor: usize,
    /// Part of `table_memory_max_bytes` kept for requests within a tenant's
    /// floors (at most half of the cap is reserved).
    pub floor_reserve_bytes: usize,
    /// Hard cap on the estimated bytes of all limiter tables combined.
    pub table_memory_max_bytes: usize,
}

impl Default for NetLimits {
    fn default() -> Self {
        Self {
            tenant_net_cap: TENANT_NET_CAP,
            tenant_v6_32_cap: TENANT_V6_32_CAP,
            platform_factor: PLATFORM_FACTOR,
            slots_per_tenant: SLOTS_PER_TENANT,
            tenant_slots_total: EXPECTED_TENANTS * SLOTS_PER_TENANT,
            tenant_slot_floor: TENANT_SLOT_FLOOR,
            platform_slots_per_aggregate: PLATFORM_SLOTS_PER_AGGREGATE,
            platform_slots: PLATFORM_SLOTS_TOTAL,
            platform_slot_floor: PLATFORM_SLOT_FLOOR,
            floor_reserve_bytes: FLOOR_RESERVE_BYTES,
            table_memory_max_bytes: TABLE_MEMORY_MAX_BYTES,
        }
    }
}

impl NetLimits {
    /// Sizes the global tenant budget for `tenants` tenants at full use, the
    /// platform total for that budget plus every tenant's platform floor,
    /// and the memory reserve for every tenant's floor.
    pub fn sized_for(mut self, tenants: usize) -> Self {
        let tenants = tenants.max(1);
        self.tenant_slots_total = tenants.saturating_mul(self.slots_per_tenant);
        let floors = tenants.saturating_mul(self.platform_slot_floor);
        self.platform_slots = self.tenant_slots_total.saturating_add(floors);
        self.floor_reserve_bytes = floors.saturating_mul(FLOOR_SLOT_BYTES);
        self
    }

    /// Memory kept for floor growth: `floor_reserve_bytes`, at most half of
    /// the cap.
    fn floor_reserve(&self) -> usize {
        self.floor_reserve_bytes
            .min(self.table_memory_max_bytes / 2)
    }

    fn platform_cap(&self) -> u32 {
        self.tenant_net_cap.saturating_mul(self.platform_factor)
    }
}

/// One fixed-window counter.
struct FixedCounter {
    start: Instant,
    count: u32,
    /// Tenant charged for this slot (its floor).
    owner: String,
}

impl FixedCounter {
    fn live(&self, now: Instant) -> bool {
        now.duration_since(self.start) < WINDOW
    }
}

/// Fixed-window counters grouped into partitions (a tenant, or a network
/// aggregate), with a per-group slot cap and a total slot cap. Every slot is
/// also charged to the tenant that created it; a tenant below `floor` slots
/// is admitted even when the total is used up, so a full table never refuses
/// a tenant that holds few counters. Queues hold `(group, key, window start)` in start order; records whose
/// counter was removed or restarted are stale and skipped.
struct CounterTable {
    per_group: usize,
    total: usize,
    floor: usize,
    entries: HashMap<(String, String), FixedCounter>,
    group_len: HashMap<String, usize>,
    owner_len: HashMap<String, usize>,
    group_queue: HashMap<String, VecDeque<(String, Instant)>>,
    queue: VecDeque<(String, String, Instant)>,
}

impl CounterTable {
    fn new(per_group: usize, total: usize, floor: usize) -> Self {
        Self {
            per_group: per_group.max(1),
            total: total.max(1),
            floor: floor.min(per_group),
            entries: HashMap::new(),
            group_len: HashMap::new(),
            owner_len: HashMap::new(),
            group_queue: HashMap::new(),
            queue: VecDeque::new(),
        }
    }

    fn is_current(&self, group: &str, key: &str, start: Instant) -> bool {
        self.entries
            .get(&(group.to_string(), key.to_string()))
            .is_some_and(|c| c.start == start)
    }

    /// Slots charged to `owner`.
    fn owned(&self, owner: &str) -> usize {
        self.owner_len.get(owner).copied().unwrap_or(0)
    }

    fn remove(&mut self, group: &str, key: &str) {
        if let Some(c) = self.entries.remove(&(group.to_string(), key.to_string())) {
            if let Some(n) = self.owner_len.get_mut(&c.owner) {
                *n -= 1;
                if *n == 0 {
                    self.owner_len.remove(&c.owner);
                }
            }
            if let Some(n) = self.group_len.get_mut(group) {
                *n -= 1;
                if *n == 0 {
                    self.group_len.remove(group);
                    self.group_queue.remove(group);
                }
            }
        }
    }

    /// Clears expired counters, oldest first: `group`'s, then any group's.
    /// Stops at the first live counter (later ones started later), so live
    /// counters are never removed. Amortized O(1).
    fn sweep(&mut self, group: &str, now: Instant) {
        while let Some((key, start)) = self.group_queue.get(group).and_then(|q| q.front()).cloned()
        {
            if self.is_current(group, &key, start) {
                if now.duration_since(start) < WINDOW {
                    break;
                }
                self.remove(group, &key);
            }
            if let Some(q) = self.group_queue.get_mut(group) {
                q.pop_front();
            }
        }
        while let Some((g, key, start)) = self.queue.front().cloned() {
            if self.is_current(&g, &key, start) {
                if now.duration_since(start) < WINDOW {
                    break;
                }
                self.remove(&g, &key);
            }
            self.queue.pop_front();
        }
    }

    /// Live count for `key` (0 when absent or expired).
    fn count(&self, group: &str, key: &str, now: Instant) -> u32 {
        self.entries
            .get(&(group.to_string(), key.to_string()))
            .filter(|c| c.live(now))
            .map_or(0, |c| c.count)
    }

    /// Seconds until `key`'s window ends.
    fn wait(&self, group: &str, key: &str, now: Instant) -> u64 {
        self.entries
            .get(&(group.to_string(), key.to_string()))
            .map_or(1, |c| retry_after(c.start, now))
    }

    /// Slots that must be free to add the absent `keys`, versus free slots
    /// (after [`sweep`](Self::sweep)). `Err` carries seconds until the oldest
    /// live counter in the way expires.
    fn room(
        &self,
        group: &str,
        owner: &str,
        keys: &[&str],
        now: Instant,
    ) -> Result<(), (Bound, u64)> {
        let missing = keys
            .iter()
            .filter(|k| {
                !self
                    .entries
                    .contains_key(&(group.to_string(), k.to_string()))
            })
            .count();
        let used = self.group_len.get(group).copied().unwrap_or(0);
        let within_total =
            self.entries.len() + missing <= self.total || self.owned(owner) + missing <= self.floor;
        if used + missing <= self.per_group && within_total {
            return Ok(());
        }
        let bound = if used + missing > self.per_group {
            Bound::Partition
        } else {
            Bound::Total
        };
        let oldest = if bound == Bound::Partition {
            self.group_queue
                .get(group)
                .and_then(|q| q.iter().find(|(k, s)| self.is_current(group, k, *s)))
                .map(|(_, s)| *s)
        } else {
            self.queue
                .iter()
                .find(|(g, k, s)| self.is_current(g, k, *s))
                .map(|(_, _, s)| *s)
        };
        Err((
            bound,
            oldest.map_or(WINDOW.as_secs(), |s| retry_after(s, now)),
        ))
    }

    /// Absent `keys` (slots a successful add would create).
    fn missing(&self, group: &str, keys: &[&str]) -> usize {
        keys.iter()
            .filter(|k| {
                !self
                    .entries
                    .contains_key(&(group.to_string(), k.to_string()))
            })
            .count()
    }

    /// Adds `n` to `key`, restarting its window if expired. Call only after
    /// [`room`](Self::room) succeeded.
    fn add(&mut self, group: &str, owner: &str, key: &str, n: u32, now: Instant) {
        let id = (group.to_string(), key.to_string());
        match self.entries.get_mut(&id) {
            Some(c) if c.live(now) => {
                c.count += n;
                return;
            }
            Some(c) => {
                c.start = now;
                c.count = n;
            }
            None => {
                self.entries.insert(
                    id,
                    FixedCounter {
                        start: now,
                        count: n,
                        owner: owner.to_string(),
                    },
                );
                *self.group_len.entry(group.to_string()).or_insert(0) += 1;
                *self.owner_len.entry(owner.to_string()).or_insert(0) += 1;
            }
        }
        self.group_queue
            .entry(group.to_string())
            .or_default()
            .push_back((key.to_string(), now));
        self.queue
            .push_back((group.to_string(), key.to_string(), now));
        if self.queue.len() > self.entries.len() * 4 + 64 {
            let entries = &self.entries;
            self.queue.retain(|(g, k, s)| {
                entries
                    .get(&(g.clone(), k.clone()))
                    .is_some_and(|c| c.start == *s)
            });
            for (g, q) in self.group_queue.iter_mut() {
                q.retain(|(k, s)| {
                    entries
                        .get(&(g.clone(), k.clone()))
                        .is_some_and(|c| c.start == *s)
                });
            }
        }
    }
}

/// Which slot bound refused: a group's partition or the table total.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Bound {
    Partition,
    Total,
}

/// The D130 layers.
struct NetLayers {
    limits: NetLimits,
    tenants: CounterTable,
    platform: CounterTable,
    /// Last warn log per `(layer, reason)`.
    full_logged: HashMap<(&'static str, &'static str), Instant>,
    /// Estimated bytes of the other tables plus the buckets this request
    /// would create (set by `check_tenant`).
    base_bytes: usize,
    max_bytes: usize,
}

/// Which table a refusal came from (logs only, never the response).
#[derive(Clone, Copy, Debug, PartialEq)]
enum NetRefusal {
    Cap(&'static str, u64),
    /// `(layer, reason, wait)`; reason is `table_full`,
    /// `tenant_budget_exhausted` or `prefix_cap_exceeded`.
    SlotsFull(&'static str, &'static str, u64),
}

impl NetLayers {
    fn new(limits: NetLimits) -> Self {
        Self {
            tenants: CounterTable::new(
                limits.slots_per_tenant,
                limits.tenant_slots_total,
                limits.tenant_slot_floor,
            ),
            platform: CounterTable::new(
                limits.platform_slots_per_aggregate,
                limits.platform_slots,
                limits.platform_slot_floor,
            ),
            max_bytes: limits.table_memory_max_bytes,
            limits,
            full_logged: HashMap::new(),
            base_bytes: 0,
        }
    }

    /// `(tenant keys with caps, platform keys, platform partition)` for an
    /// [`owner_of`] string. The platform partition is the IPv4 /24 or IPv6
    /// /32 aggregate.
    fn keys(&self, owner: &str) -> (Vec<(String, u32)>, Vec<String>, String) {
        let [net, wider] = owner_levels(owner);
        let cap = self.limits.tenant_net_cap;
        if net.ends_with("/48") {
            (
                vec![
                    (net.clone(), cap),
                    (wider.clone(), self.limits.tenant_v6_32_cap),
                ],
                vec![net],
                wider,
            )
        } else {
            (vec![(wider.clone(), cap)], vec![wider.clone()], wider)
        }
    }

    /// Seconds until every live counter this request touches has reset, in
    /// both layers, so the value does not depend on which layer refused.
    fn wait_all(
        &self,
        tenant: &str,
        tkeys: &[(String, u32)],
        group: &str,
        pkeys: &[String],
        now: Instant,
    ) -> u64 {
        let t = tkeys
            .iter()
            .filter(|(k, _)| self.tenants.count(tenant, k, now) > 0)
            .map(|(k, _)| self.tenants.wait(tenant, k, now));
        let p = pkeys
            .iter()
            .filter(|k| self.platform.count(group, k, now) > 0)
            .map(|k| self.platform.wait(group, k, now));
        t.chain(p).max().unwrap_or(1)
    }

    /// Charges `n` new buckets to `tenant` and the client's network ranges, or
    /// refuses without counting anything.
    fn charge(
        &mut self,
        tenant: &str,
        owner: &str,
        n: u32,
        now: Instant,
    ) -> Result<(), NetRefusal> {
        if self.limits.tenant_net_cap == 0 || n == 0 {
            return Ok(());
        }
        let (tkeys, pkeys, group) = self.keys(owner);
        let pcap = self.limits.platform_cap();
        self.tenants.sweep(tenant, now);
        self.platform.sweep(&group, now);
        let over = if tkeys
            .iter()
            .any(|(k, cap)| self.tenants.count(tenant, k, now) + n > *cap)
        {
            Some("tenant")
        } else if pkeys
            .iter()
            .any(|k| self.platform.count(&group, k, now) + n > pcap)
        {
            Some("platform")
        } else {
            None
        };
        if let Some(layer) = over {
            return Err(NetRefusal::Cap(
                layer,
                self.wait_all(tenant, &tkeys, &group, &pkeys, now),
            ));
        }
        let tk: Vec<&str> = tkeys.iter().map(|(k, _)| k.as_str()).collect();
        let pk: Vec<&str> = pkeys.iter().map(String::as_str).collect();
        let new_t = self.tenants.missing(tenant, &tk);
        let new_p = self.platform.missing(&group, &pk);
        let projected = self
            .base_bytes
            .saturating_add(self.bytes())
            .saturating_add((new_t + new_p) * COUNTER_SLOT_BYTES);
        // Growth within both of the tenant's floors may use the reserved part
        // of the memory cap; any other growth may not.
        let within_floor = self.tenants.owned(tenant) + new_t <= self.tenants.floor
            && self.platform.owned(tenant) + new_p <= self.platform.floor;
        let mem_cap = if within_floor {
            self.max_bytes
        } else {
            self.max_bytes.saturating_sub(self.limits.floor_reserve())
        };
        let full = match (
            self.tenants.room(tenant, tenant, &tk, now),
            self.platform.room(&group, tenant, &pk, now),
        ) {
            (Err((Bound::Partition, w)), _) => Some(("tenant", "table_full", w)),
            (Err((Bound::Total, w)), _) => Some(("tenant", "tenant_budget_exhausted", w)),
            (_, Err((Bound::Partition, w))) => Some(("platform", "prefix_cap_exceeded", w)),
            (_, Err((Bound::Total, w))) => Some(("platform", "table_full", w)),
            _ if projected > mem_cap => {
                let layer = if new_t > 0 { "tenant" } else { "platform" };
                Some((layer, "table_full", WINDOW.as_secs()))
            }
            _ => None,
        };
        if let Some((layer, reason, w)) = full {
            let w = w.max(self.wait_all(tenant, &tkeys, &group, &pkeys, now));
            return Err(NetRefusal::SlotsFull(layer, reason, w));
        }
        for k in tk {
            self.tenants.add(tenant, tenant, k, n, now);
        }
        for k in pk {
            self.platform.add(&group, tenant, k, n, now);
        }
        Ok(())
    }

    /// Estimated bytes of both D130 tables.
    fn bytes(&self) -> usize {
        (self.tenants.entries.len() + self.platform.entries.len()) * COUNTER_SLOT_BYTES
    }

    /// Counts a fail-closed refusal and logs at most once per minute per
    /// `(layer, reason)` (alert hook: target `vibe::rate_limit_counter_slots`).
    fn log_full(&mut self, layer: &'static str, reason: &'static str, now: Instant) {
        COUNTER_SLOT_REFUSALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        record_slot_refused(layer, reason);
        if self
            .full_logged
            .get(&(layer, reason))
            .is_some_and(|at| now.duration_since(*at) < WINDOW)
        {
            return;
        }
        self.full_logged.insert((layer, reason), now);
        tracing::warn!(
            target: "vibe::rate_limit_counter_slots",
            layer,
            reason,
            refused = slot_refused_total(layer, reason),
            refused_total = counter_slot_refusals(),
            "rate-limit counter slots exhausted; failing closed"
        );
    }
}

/// Seconds until the oldest hit leaves the window (at least 1).
pub fn retry_after(oldest: Instant, now: Instant) -> u64 {
    let remaining = WINDOW.saturating_sub(now.duration_since(oldest));
    (remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0)).max(1)
}

/// The network a rate bucket is charged to, from an [`ip_bucket`] string:
/// IPv4 addresses as is, IPv6 grouped by /48 (a typical end-site allocation,
/// so one site cannot spread over many /64s to escape the per-owner cap).
pub fn owner_of(ip_bucket: &str) -> String {
    let addr = ip_bucket.split('/').next().unwrap_or(ip_bucket);
    match addr.parse::<IpAddr>().map(|ip| ip.to_canonical()) {
        Ok(IpAddr::V6(v6)) => {
            let net = mask(u128::from(v6), 48, 128);
            format!("{}/48", std::net::Ipv6Addr::from(net))
        }
        Ok(IpAddr::V4(v4)) => v4.to_string(),
        Err(_) => ip_bucket.to_string(),
    }
}

/// `[network, wider network]` for an [`owner_of`] string: an IPv4 address
/// and its /24, or an IPv6 /48 and its /32.
pub fn owner_levels(owner: &str) -> [String; 2] {
    let addr = owner.split('/').next().unwrap_or(owner);
    let wider = match addr.parse::<IpAddr>().map(|ip| ip.to_canonical()) {
        Ok(IpAddr::V6(v6)) => {
            let net = mask(u128::from(v6), 32, 128);
            format!("{}/32", std::net::Ipv6Addr::from(net))
        }
        Ok(IpAddr::V4(v4)) => {
            let net = mask(u32::from(v4) as u128, 24, 32) as u32;
            format!("{}/24", std::net::Ipv4Addr::from(net))
        }
        Err(_) => format!("{owner}/*"),
    };
    [owner.to_string(), wider]
}

/// Daily public-write counters, one bounded shard per key type.
pub struct QuotaBuckets {
    shards: HashMap<&'static str, BoundedMap<(String, u32)>>,
    memory: std::sync::Arc<LimiterMemory>,
}

impl Default for QuotaBuckets {
    fn default() -> Self {
        Self::with_memory(std::sync::Arc::new(LimiterMemory::new(
            TABLE_MEMORY_MAX_BYTES,
        )))
    }
}

impl QuotaBuckets {
    /// Quotas sharing the rate limiter's memory budget
    /// ([`RateBuckets::memory`]).
    pub fn with_memory(memory: std::sync::Arc<LimiterMemory>) -> Self {
        Self {
            shards: HashMap::new(),
            memory,
        }
    }

    /// `checks` = `(key type, key, daily limit)`. Counts only when all admit.
    pub fn check(&mut self, day: &str, checks: &[(&'static str, String, u32)]) -> bool {
        for (kind, key, limit) in checks {
            if let Some((d, count)) = self.shards.get(kind).and_then(|m| m.get(key)) {
                if d == day && count >= limit {
                    return false;
                }
            }
        }
        let new = checks
            .iter()
            .filter(|(kind, key, _)| self.shards.get(kind).is_none_or(|m| m.get(key).is_none()))
            .count();
        let used = self.memory.quota_bytes();
        if new > 0 && used + new * QUOTA_ENTRY_BYTES > self.memory.max {
            // Rate buckets and D130 tables are counted by the rate limiter;
            // here only quota growth is refused (fail closed).
            COUNTER_SLOT_REFUSALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return false;
        }
        for (kind, key, _) in checks {
            let entry = self
                .shards
                .entry(kind)
                .or_insert_with(|| BoundedMap::new(SHARD_CAPACITY))
                .touch(key, || (day.to_string(), 0));
            if entry.0 != day {
                *entry = (day.to_string(), 0);
            }
            entry.1 += 1;
        }
        let entries: usize = self.shards.values().map(|m| m.len()).sum();
        self.memory.quota.store(
            entries * QUOTA_ENTRY_BYTES,
            std::sync::atomic::Ordering::Relaxed,
        );
        true
    }
}

/// A trusted proxy entry: a single address or a CIDR block.
#[derive(Clone, Debug, PartialEq)]
pub struct ProxyNet {
    net: IpAddr,
    prefix: u8,
}

impl ProxyNet {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        let (addr, prefix) = match raw.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (raw, None),
        };
        let net: IpAddr = addr
            .parse::<IpAddr>()
            .map_err(|_| format!("invalid proxy address {raw}"))?
            .to_canonical();
        let max = if net.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(p) => p
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= max)
                .ok_or_else(|| format!("invalid prefix in {raw}"))?,
            None => max,
        };
        Ok(Self { net, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.net, ip.to_canonical()) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                mask(u32::from(n) as u128, self.prefix, 32)
                    == mask(u32::from(a) as u128, self.prefix, 32)
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                mask(u128::from(n), self.prefix, 128) == mask(u128::from(a), self.prefix, 128)
            }
            _ => false,
        }
    }
}

fn mask(value: u128, prefix: u8, bits: u8) -> u128 {
    if prefix == 0 {
        return 0;
    }
    let shift = (bits - prefix) as u32;
    (value >> shift) << shift
}

/// Parses an `X-Forwarded-For` hop: a bare address, `a.b.c.d:port`, or
/// `[v6]:port`. IPv4-mapped IPv6 addresses become IPv4.
pub fn parse_hop(hop: &str) -> Option<IpAddr> {
    let hop = hop.trim();
    if let Ok(ip) = hop.parse::<IpAddr>() {
        return Some(ip.to_canonical());
    }
    hop.parse::<std::net::SocketAddr>()
        .ok()
        .map(|sa| sa.ip().to_canonical())
}

/// The rate-limit identity of an address: IPv4 as is, IPv6 grouped by /64
/// (one subscriber usually owns a whole /64).
pub fn ip_bucket(ip: IpAddr) -> String {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => {
            let net = mask(u128::from(v6), 64, 128);
            format!("{}/64", std::net::Ipv6Addr::from(net))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_map_evicts_least_recently_used() {
        let mut m: BoundedMap<u32> = BoundedMap::new(2);
        *m.touch("a", || 0) += 1;
        *m.touch("b", || 0) += 1;
        *m.touch("a", || 0) += 1; // a is now most recent
        m.touch("c", || 0);
        assert!(m.get("b").is_none(), "b was least recently used");
        assert_eq!(m.get("a"), Some(&2));
        assert_eq!(m.len(), 2);
        for i in 0..100 {
            m.touch(&format!("k{i}"), || 0);
        }
        assert_eq!(m.len(), 2);
        assert!(m.order.len() <= 2 * 4 + 1);
    }

    #[test]
    fn flooding_one_shard_does_not_block_a_normal_client() {
        let mut rates = RateBuckets::shard_only();
        let now = Instant::now();
        let shard = "login:input.email".to_string();
        for i in 0..(SHARD_CAPACITY * 2) {
            let decision = rates.check(
                &[(shard.clone(), format!("junk{i}"), 5)],
                &format!("10.{}.{}.1", i / 256 % 256, i % 256),
                now,
            );
            assert!(matches!(decision, RateDecision::Allow));
        }
        let normal = [
            (shard.clone(), "real-user".to_string(), 5),
            ("login:ip".to_string(), "203.0.113.7".to_string(), 10),
        ];
        assert!(matches!(
            rates.check(&normal, "203.0.113.7", now),
            RateDecision::Allow
        ));
        // Limits still apply to that client.
        for _ in 0..4 {
            assert!(matches!(
                rates.check(&normal, "203.0.113.7", now),
                RateDecision::Allow
            ));
        }
        assert!(matches!(
            rates.check(&normal, "203.0.113.7", now),
            RateDecision::Limited(_)
        ));
    }

    #[test]
    fn rotating_junk_keys_cannot_reset_a_limited_bucket() {
        let mut rates = RateBuckets::shard_only();
        let now = Instant::now();
        let shard = "login:input.email".to_string();
        let victim = [(shard.clone(), "victim@x".to_string(), 3)];
        for _ in 0..3 {
            assert!(matches!(
                rates.check(&victim, "198.51.100.1", now),
                RateDecision::Allow
            ));
        }
        assert!(matches!(
            rates.check(&victim, "198.51.100.1", now),
            RateDecision::Limited(_)
        ));
        for i in 0..(SHARD_CAPACITY * 2) {
            rates.check(
                &[(shard.clone(), format!("junk{i}"), 3)],
                &format!("11.{}.{}.1", i / 256 % 256, i % 256),
                now,
            );
        }
        assert!(
            matches!(
                rates.check(&victim, "198.51.100.1", now),
                RateDecision::Limited(_)
            ),
            "the at-limit bucket must survive eviction"
        );
        // After the window it is free again.
        assert!(matches!(
            rates.check(
                &victim,
                "198.51.100.1",
                now + WINDOW + Duration::from_secs(1)
            ),
            RateDecision::Allow
        ));
    }

    #[test]
    fn full_pinned_shard_refuses_only_new_keys_in_that_shard() {
        let mut rates = RateBuckets::shard_only();
        let now = Instant::now();
        let shard = "login:input.email".to_string();
        for i in 0..SHARD_CAPACITY {
            rates.check(
                &[(shard.clone(), format!("k{i}"), 1)],
                &format!("12.{}.{}.1", i / 256 % 256, i % 256),
                now,
            );
        }
        assert!(matches!(
            rates.check(&[(shard.clone(), "new".into(), 1)], "192.0.2.1", now),
            RateDecision::Limited(_)
        ));
        assert!(matches!(
            rates.check(&[("login:ip".into(), "1.2.3.4".into(), 10)], "1.2.3.4", now),
            RateDecision::Allow
        ));
    }

    #[test]
    fn quota_is_bounded_and_resets_by_day() {
        let mut q = QuotaBuckets::default();
        for i in 0..(SHARD_CAPACITY + 10) {
            assert!(q.check("2026-10-14", &[("ip", format!("ip{i}"), 50)]));
        }
        let real = [
            ("tenant", "t1".to_string(), 2),
            ("ip", "1.2.3.4".to_string(), 50),
        ];
        assert!(q.check("2026-10-14", &real));
        assert!(q.check("2026-10-14", &real));
        assert!(!q.check("2026-10-14", &real));
        assert!(q.check("2026-10-15", &real), "a new day resets the counter");
    }

    #[test]
    fn retry_after_counts_down() {
        let now = Instant::now();
        assert_eq!(retry_after(now - Duration::from_secs(45), now), 15);
        assert_eq!(retry_after(now, now), 60);
        assert_eq!(retry_after(now - Duration::from_secs(70), now), 1);
    }

    #[test]
    fn proxy_nets_and_hops() {
        let net = ProxyNet::parse("10.0.0.0/8").unwrap();
        assert!(net.contains("10.1.2.3".parse().unwrap()));
        assert!(net.contains("::ffff:10.1.2.3".parse().unwrap()));
        assert!(!net.contains("11.0.0.1".parse().unwrap()));
        let v6 = ProxyNet::parse("2001:db8::/32").unwrap();
        assert!(v6.contains("2001:db8:1::1".parse().unwrap()));
        assert!(ProxyNet::parse("10.0.0.0/33").is_err());
        assert!(ProxyNet::parse("nope").is_err());
        assert_eq!(parse_hop("1.2.3.4:5678"), Some("1.2.3.4".parse().unwrap()));
        assert_eq!(
            parse_hop("[2001:db8::1]:443"),
            Some("2001:db8::1".parse().unwrap())
        );
        assert_eq!(
            parse_hop("::ffff:1.2.3.4"),
            Some("1.2.3.4".parse().unwrap())
        );
        assert_eq!(parse_hop("garbage"), None);
    }

    #[test]
    fn saturation_refusals_are_counted() {
        let mut rates = RateBuckets::shard_only();
        let now = Instant::now();
        let shard = "signup:input.email".to_string();
        let before = saturation_refusals();
        let mut refused = 0u64;
        for i in 0..(OWNER_CAP + 10) {
            match rates.check(&[(shard.clone(), format!("k{i}"), 1)], "1.2.3.4", now) {
                RateDecision::Allow => {}
                RateDecision::Limited(_) => refused += 1,
            }
        }
        assert_eq!(refused, 10);
        // Other tests may refuse concurrently; the counter only grows.
        assert!(saturation_refusals() >= before + refused);
    }

    #[test]
    fn ipv6_is_grouped_by_64() {
        let a = ip_bucket("2001:db8:1:2:aaaa::1".parse().unwrap());
        let b = ip_bucket("2001:db8:1:2:bbbb::9".parse().unwrap());
        assert_eq!(a, b);
        assert_ne!(a, ip_bucket("2001:db8:1:3::1".parse().unwrap()));
        assert_eq!(ip_bucket("::ffff:1.2.3.4".parse().unwrap()), "1.2.3.4");
    }

    #[test]
    fn many_ipv6_64s_from_one_48_cannot_fill_a_shard() {
        // Issue #71: every /64 gets its own IP bucket, so a client with many
        // /64s could pin a whole shard with at-limit buckets.
        let mut rates = RateBuckets::shard_only();
        let now = Instant::now();
        let shard = "login:ip".to_string();
        let mut refused = 0;
        for i in 0..SHARD_CAPACITY {
            let ip: IpAddr = format!("2001:db8:aaaa:{:x}::1", i).parse().unwrap();
            let bucket = ip_bucket(ip);
            let owner = owner_of(&bucket);
            match rates.check(&[(shard.clone(), bucket, 1)], &owner, now) {
                RateDecision::Allow => {}
                RateDecision::Limited(_) => refused += 1,
            }
        }
        assert_eq!(refused, SHARD_CAPACITY - OWNER_CAP);
        // A new client from another network still gets a bucket.
        let other = ip_bucket("2001:db8:bbbb:1::1".parse().unwrap());
        assert!(matches!(
            rates.check(&[(shard.clone(), other.clone(), 1)], &owner_of(&other), now),
            RateDecision::Allow
        ));
        // A new IPv4 client too.
        assert!(matches!(
            rates.check(
                &[(shard.clone(), "203.0.113.9".into(), 1)],
                "203.0.113.9",
                now
            ),
            RateDecision::Allow
        ));
    }

    #[test]
    fn owner_cap_frees_up_after_the_window() {
        let mut rates = RateBuckets::shard_only();
        let now = Instant::now();
        let shard = "signup:input.email".to_string();
        let owner = "2001:db8:1::/48";
        for i in 0..OWNER_CAP {
            assert!(matches!(
                rates.check(&[(shard.clone(), format!("e{i}"), 5)], owner, now),
                RateDecision::Allow
            ));
        }
        assert!(matches!(
            rates.check(&[(shard.clone(), "one-more".into(), 5)], owner, now),
            RateDecision::Limited(_)
        ));
        // Existing buckets of that owner keep working.
        assert!(matches!(
            rates.check(&[(shard.clone(), "e0".into(), 5)], owner, now),
            RateDecision::Allow
        ));
        // Once its buckets go idle, the owner may create new ones again.
        let later = now + WINDOW + Duration::from_secs(1);
        assert!(matches!(
            rates.check(&[(shard.clone(), "one-more".into(), 5)], owner, later),
            RateDecision::Allow
        ));
    }

    #[test]
    fn owner_cap_is_per_shard_and_eviction_releases_ownership() {
        let mut rates = RateBuckets::shard_only();
        let now = Instant::now();
        let owner = "198.51.100.7";
        for i in 0..OWNER_CAP {
            rates.check(&[("a:input.email".into(), format!("e{i}"), 5)], owner, now);
        }
        assert!(matches!(
            rates.check(&[("b:input.email".into(), "x".into(), 5)], owner, now),
            RateDecision::Allow
        ));
        // Other networks flood shard a: LRU evicts the owner's (not at-limit)
        // buckets and its count drops, so it may create buckets again.
        for i in 0..SHARD_CAPACITY {
            let o = format!("13.{}.{}.1", i / 256 % 256, i % 256);
            rates.check(&[("a:input.email".into(), format!("j{i}"), 5)], &o, now);
        }
        assert!(matches!(
            rates.check(&[("a:input.email".into(), "fresh".into(), 5)], owner, now),
            RateDecision::Allow
        ));
    }

    #[test]
    fn owner_of_groups_ipv6_by_48() {
        assert_eq!(owner_of("2001:db8:1:2::/64"), "2001:db8:1::/48");
        assert_eq!(owner_of("2001:db8:1:ffff::/64"), "2001:db8:1::/48");
        assert_ne!(owner_of("2001:db8:2::/64"), owner_of("2001:db8:1::/64"));
        assert_eq!(owner_of("1.2.3.4"), "1.2.3.4");
    }

    fn pinned_fill(
        rates: &mut RateBuckets,
        shard: &str,
        owner: &str,
        keys: usize,
        tag: &str,
        now: Instant,
    ) {
        for k in 0..keys {
            rates.check(&[(shard.to_string(), format!("{tag}-{k}"), 1)], owner, now);
        }
    }

    #[test]
    fn a_hundred_ipv6_48s_cannot_lock_out_new_clients() {
        let now = Instant::now();
        // Same /32: the wider-network cap stops them at NETWORK_CAP.
        let mut rates = RateBuckets::shard_only();
        for n in 0..100 {
            let owner = format!("2001:db8:{n:x}::/48");
            pinned_fill(&mut rates, "login:ip", &owner, OWNER_CAP, &owner, now);
        }
        assert_eq!(rates.shards["login:ip"].map.entries.len(), NETWORK_CAP);
        // 100 /48s in 100 different /32s: the reserve keeps room.
        let mut rates = RateBuckets::shard_only();
        for n in 0..100 {
            let owner = format!("2001:{:x}:1::/48", 0x100 + n);
            pinned_fill(&mut rates, "login:ip", &owner, OWNER_CAP, &owner, now);
        }
        assert!(rates.shards["login:ip"].map.entries.len() < SHARD_CAPACITY);
        let fresh = ip_bucket("2a00:1450:4001::1".parse().unwrap());
        assert!(matches!(
            rates.check(
                &[("login:ip".into(), fresh.clone(), 1)],
                &owner_of(&fresh),
                now
            ),
            RateDecision::Allow
        ));
    }

    #[test]
    fn a_hundred_ipv4_addresses_cannot_lock_out_new_emails() {
        let now = Instant::now();
        let shard = "login:input.email";
        // One /24.
        let mut rates = RateBuckets::shard_only();
        for n in 0..100 {
            let owner = format!("198.51.100.{n}");
            pinned_fill(&mut rates, shard, &owner, OWNER_CAP, &owner, now);
        }
        assert_eq!(rates.shards[shard].map.entries.len(), NETWORK_CAP);
        // 100 different /24s.
        let mut rates = RateBuckets::shard_only();
        for n in 0..100 {
            let owner = format!("100.{n}.0.1");
            pinned_fill(&mut rates, shard, &owner, OWNER_CAP, &owner, now);
        }
        for user in 0..20 {
            let owner = format!("203.0.{user}.7");
            assert!(
                matches!(
                    rates.check(
                        &[(shard.into(), format!("new-user-{user}"), 5)],
                        &owner,
                        now
                    ),
                    RateDecision::Allow
                ),
                "new user {user}"
            );
        }
    }

    #[test]
    fn shared_ip_without_trusted_proxies_is_capped_but_others_are_not() {
        // Many users behind one CGNAT address (or one proxy when
        // VIBE_TRUSTED_PROXIES is unset) share that address's allowance.
        let now = Instant::now();
        let shard = "signup:input.email";
        let mut rates = RateBuckets::shard_only();
        let cgnat = "100.64.0.1";
        for user in 0..OWNER_CAP {
            assert!(matches!(
                rates.check(&[(shard.into(), format!("u{user}"), 5)], cgnat, now),
                RateDecision::Allow
            ));
        }
        assert!(matches!(
            rates.check(&[(shard.into(), "one-more".into(), 5)], cgnat, now),
            RateDecision::Limited(_)
        ));
        // Neighbours in the same /24 and other networks are unaffected.
        assert!(matches!(
            rates.check(&[(shard.into(), "neighbour".into(), 5)], "100.64.0.2", now),
            RateDecision::Allow
        ));
        assert!(matches!(
            rates.check(&[(shard.into(), "elsewhere".into(), 5)], "192.0.2.10", now),
            RateDecision::Allow
        ));
        // Existing users behind the shared address keep working, and new ones
        // get in again once the window has passed.
        assert!(matches!(
            rates.check(&[(shard.into(), "u0".into(), 5)], cgnat, now),
            RateDecision::Allow
        ));
        let later = now + WINDOW + Duration::from_secs(1);
        assert!(matches!(
            rates.check(&[(shard.into(), "one-more".into(), 5)], cgnat, later),
            RateDecision::Allow
        ));
    }

    #[test]
    fn owner_levels_nest() {
        assert_eq!(owner_levels("2001:db8:1::/48")[1], "2001:db8::/32");
        assert_eq!(owner_levels("198.51.100.7")[1], "198.51.100.0/24");
    }

    /// Saturation events: `(target, reason, refused)` per log line.
    type Events = Vec<(String, String, u64)>;

    thread_local! {
        /// Per-test capture buffer; `Some` only inside [`capture`].
        static CAPTURED: std::cell::RefCell<Option<Events>> = const { std::cell::RefCell::new(None) };
    }

    /// Records events into the current thread's [`CAPTURED`] buffer.
    ///
    /// Installed once as the global default. A per-test scoped subscriber
    /// (`with_default`) is racy: tracing caches callsite interest globally,
    /// so another test thread hitting the same callsite with no subscriber
    /// can cache `Interest::never` and the scoped capture sees no events.
    struct Captured;

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Captured {
        fn register_callsite(
            &self,
            _: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::always()
        }

        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct V(String, u64);
            impl tracing::field::Visit for V {
                fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
                    if f.name() == "reason" {
                        self.0 = v.to_string();
                    }
                }
                fn record_u64(&mut self, f: &tracing::field::Field, v: u64) {
                    if f.name() == "refused" {
                        self.1 = v;
                    }
                }
                fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
            }
            CAPTURED.with(|c| {
                if let Some(buf) = c.borrow_mut().as_mut() {
                    let mut v = V(String::new(), 0);
                    event.record(&mut v);
                    buf.push((event.metadata().target().to_string(), v.0, v.1));
                }
            });
        }
    }

    fn capture<T>(f: impl FnOnce() -> T) -> (T, Events) {
        use tracing_subscriber::layer::SubscriberExt;
        static INSTALL: std::sync::Once = std::sync::Once::new();
        INSTALL.call_once(|| {
            let subscriber = tracing_subscriber::registry().with(Captured);
            // Fails only if a global default is already set elsewhere.
            tracing::subscriber::set_global_default(subscriber)
                .expect("test capture subscriber must be the global default");
        });
        CAPTURED.with(|c| *c.borrow_mut() = Some(Vec::new()));
        let out = f();
        let events = CAPTURED.with(|c| c.borrow_mut().take()).unwrap_or_default();
        (out, events)
    }

    fn allowed(d: RateDecision) -> bool {
        matches!(d, RateDecision::Allow)
    }

    /// 10 addresses in each of `nets` /24s (`100.{n}.0.0/24`), each pinning
    /// OWNER_CAP buckets. Returns the number of refused new keys.
    fn fill_29_24s(rates: &mut RateBuckets, shard: &str, nets: usize, now: Instant) -> usize {
        let mut refused = 0;
        for n in 0..nets {
            for host in 0..10 {
                let owner = format!("100.{n}.0.{host}");
                for k in 0..OWNER_CAP {
                    if !allowed(rates.check(
                        &[(shard.to_string(), format!("{owner}-{k}"), 1)],
                        &owner,
                        now,
                    )) {
                        refused += 1;
                    }
                }
            }
        }
        refused
    }

    #[test]
    fn saturation_log_has_stable_target_each_reason_and_is_rate_limited() {
        let now = Instant::now();
        let target = "vibe::rate_limit_saturation";
        // owner_cap: one address over OWNER_CAP, refused twice in one minute
        // (one line), then again after the window (a second line counting
        // the refusals since the first).
        let (_, events) = capture(|| {
            let mut rates = RateBuckets::shard_only();
            let shard = "s".to_string();
            for i in 0..OWNER_CAP + 2 {
                rates.check(&[(shard.clone(), format!("k{i}"), 1)], "192.0.2.1", now);
            }
            let later = now + WINDOW;
            rates.check(&[(shard.clone(), "x1".into(), 1)], "192.0.2.1", later);
            rates.check(&[(shard.clone(), "x2".into(), 1)], "192.0.2.1", later);
        });
        let ours: Vec<_> = events.iter().filter(|e| e.0 == target).collect();
        assert_eq!(ours.len(), 1, "{events:?}");
        assert_eq!(ours[0].1, "owner_cap");
        assert_eq!(ours[0].2, 1);
        // A second line only after a minute, counting every refusal since
        // the first. Address A is refused at `now` (line 1); B fills its
        // buckets 30s later and is refused twice (no line), then once more a
        // minute after line 1 while its buckets are still live (line 2).
        let (_, events) = capture(|| {
            let mut rates = RateBuckets::shard_only();
            let shard = "s";
            let a = "192.0.2.1";
            let b = "198.51.100.1";
            pinned_fill(&mut rates, shard, a, OWNER_CAP + 1, a, now);
            let t = now + std::time::Duration::from_secs(30);
            pinned_fill(&mut rates, shard, b, OWNER_CAP + 2, b, t);
            rates.check(&[(shard.into(), "x".into(), 1)], b, now + WINDOW);
        });
        let ours: Vec<_> = events.iter().filter(|e| e.0 == target).collect();
        assert_eq!(ours.len(), 2, "{events:?}");
        assert!(ours.iter().all(|e| e.1 == "owner_cap"));
        assert_eq!(ours[1].2, 3, "refusals since the previous line");

        // network_cap: 11 addresses of one /24.
        let (_, events) = capture(|| {
            let mut rates = RateBuckets::shard_only();
            for host in 0..11 {
                let owner = format!("198.51.100.{host}");
                pinned_fill(&mut rates, "s", &owner, OWNER_CAP, &owner, now);
            }
        });
        let reasons: Vec<_> = events.iter().filter(|e| e.0 == target).collect();
        assert_eq!(reasons.len(), 1, "{events:?}");
        assert_eq!(reasons[0].1, "network_cap");

        // reserve, then pinned: 9 /24s fill everything but the reserve, the
        // next heavy client is refused (reserve); small clients fill the
        // reserve with pinned buckets and the next new key is refused
        // (pinned).
        let (_, events) = capture(|| {
            let mut rates = RateBuckets::shard_only();
            fill_29_24s(&mut rates, "a", 9, now);
            assert_eq!(rates.shards["a"].map.len(), SHARD_CAPACITY - RESERVE);
            pinned_fill(
                &mut rates,
                "a",
                "100.200.0.1",
                RESERVE_OWNER_CAP + 1,
                "r",
                now,
            );
            let mut rates = RateBuckets::shard_only();
            fill_29_24s(&mut rates, "b", 9, now);
            for n in 0..RESERVE / RESERVE_OWNER_CAP {
                let owner = format!("172.{}.{}.1", 16 + n / 256, n % 256);
                pinned_fill(&mut rates, "b", &owner, RESERVE_OWNER_CAP, &owner, now);
            }
            assert_eq!(rates.shards["b"].map.len(), SHARD_CAPACITY);
            assert!(!allowed(rates.check(
                &[("b".into(), "late".into(), 1)],
                "8.8.8.8",
                now
            )));
        });
        let got: Vec<_> = events
            .iter()
            .filter(|e| e.0 == target)
            .map(|e| e.1.as_str())
            .collect();
        assert_eq!(got, vec!["reserve", "pinned"], "{events:?}");
    }

    #[test]
    fn two_hundred_ninety_ips_across_29_24s_cannot_saturate_a_shard() {
        // Review scenario: 10 addresses in each of 29 /24s, every address
        // pinning OWNER_CAP buckets. NETWORK_CAP stops each /24 and the
        // reserve keeps room for new clients.
        let now = Instant::now();
        let shard = "login:input.email";
        let mut rates = RateBuckets::shard_only();
        let refused = fill_29_24s(&mut rates, shard, 29, now);
        let len = rates.shards[shard].map.len();
        assert!(len < SHARD_CAPACITY, "shard is full: {len}");
        assert!(refused >= 29 * 10 * OWNER_CAP - SHARD_CAPACITY);
        for user in 0..20 {
            let owner = format!("203.0.{user}.7");
            assert!(
                allowed(rates.check(&[(shard.into(), format!("new-{user}"), 5)], &owner, now)),
                "new user {user}"
            );
        }
        let v6 = ip_bucket("2a00:1450:4001::1".parse().unwrap());
        assert!(allowed(rates.check(
            &[(shard.into(), "new-v6".into(), 5)],
            &owner_of(&v6),
            now
        )));
    }

    /// Fills everything but the reserve with 9 IPv4 /24s at NETWORK_CAP.
    fn fill_outside_reserve(rates: &mut RateBuckets, shard: &str, now: Instant) {
        fill_29_24s(rates, shard, 9, now);
        assert_eq!(rates.shards[shard].map.len(), SHARD_CAPACITY - RESERVE);
    }

    fn new_keys(
        rates: &mut RateBuckets,
        shard: &str,
        owner: &str,
        n: usize,
        now: Instant,
    ) -> usize {
        (0..n)
            .filter(|k| {
                allowed(rates.check(&[(shard.into(), format!("{owner}-{k}"), 1)], owner, now))
            })
            .count()
    }

    #[test]
    fn reserve_caps_each_ipv6_48_at_10() {
        let now = Instant::now();
        let mut rates = RateBuckets::shard_only();
        fill_outside_reserve(&mut rates, "s", now);
        assert_eq!(
            new_keys(&mut rates, "s", "2001:db8:1::/48", 30, now),
            RESERVE_V6_48_CAP
        );
        // Another /48 in the same /32 gets its own 10.
        assert_eq!(
            new_keys(&mut rates, "s", "2001:db8:2::/48", 30, now),
            RESERVE_V6_48_CAP
        );
    }

    #[test]
    fn reserve_caps_each_ipv6_32_at_50_across_many_48s() {
        let now = Instant::now();
        let mut rates = RateBuckets::shard_only();
        fill_outside_reserve(&mut rates, "s", now);
        let got: usize = (0..40)
            .map(|n| new_keys(&mut rates, "s", &format!("2001:db8:{n:x}::/48"), 30, now))
            .sum();
        assert_eq!(got, RESERVE_V6_32_CAP);
        // A different /32 is not affected.
        assert_eq!(
            new_keys(&mut rates, "s", "2001:db9:1::/48", 30, now),
            RESERVE_V6_48_CAP
        );
    }

    #[test]
    fn reserve_caps_for_ipv4_are_unchanged() {
        let now = Instant::now();
        let mut rates = RateBuckets::shard_only();
        fill_outside_reserve(&mut rates, "s", now);
        assert_eq!(
            new_keys(&mut rates, "s", "198.51.100.1", 30, now),
            RESERVE_OWNER_CAP
        );
        assert_eq!(
            new_keys(&mut rates, "s", "198.51.100.2", 30, now),
            RESERVE_OWNER_CAP
        );
        // The /24 is now at its reserve cap of 10.
        assert_eq!(new_keys(&mut rates, "s", "198.51.100.3", 30, now), 0);
        assert_eq!(RESERVE_NETWORK_CAP, 10);
    }

    #[test]
    fn reserve_still_admits_new_clients_after_heavy_ipv6_32s() {
        let now = Instant::now();
        let mut rates = RateBuckets::shard_only();
        fill_outside_reserve(&mut rates, "s", now);
        // 10 /32s, each spreading over 20 /48s, use their reserve allowance.
        for a in 0..10 {
            for n in 0..20 {
                new_keys(
                    &mut rates,
                    "s",
                    &format!("2001:{:x}:{n:x}::/48", 0x100 + a),
                    30,
                    now,
                );
            }
        }
        assert!(rates.shards["s"].map.len() < SHARD_CAPACITY);
        assert_eq!(new_keys(&mut rates, "s", "203.0.113.9", 1, now), 1);
        let v6 = ip_bucket("2a00:1450:4001::1".parse().unwrap());
        assert_eq!(new_keys(&mut rates, "s", &owner_of(&v6), 1, now), 1);
    }

    // ---- D130 (#132) ----

    /// `None` when allowed, else the Retry-After seconds.
    fn d130(
        rates: &mut RateBuckets,
        tenant: &str,
        owner: &str,
        key: &str,
        now: Instant,
    ) -> Option<u64> {
        match rates.check_tenant(&[("s".into(), key.into(), 5)], tenant, owner, now) {
            RateDecision::Allow => None,
            RateDecision::Limited(w) => Some(w),
        }
    }

    /// New keys admitted out of `n` for `tenant` from `owner`.
    fn d130_fill(
        rates: &mut RateBuckets,
        tenant: &str,
        owner: &str,
        n: usize,
        now: Instant,
    ) -> usize {
        (0..n)
            .filter(|i| d130(rates, tenant, owner, &format!("{tenant}-{owner}-{i}"), now).is_none())
            .count()
    }

    fn small_slots(per_tenant: usize, total: usize, platform: usize) -> RateBuckets {
        RateBuckets::with_limits(NetLimits {
            slots_per_tenant: per_tenant,
            tenant_slots_total: total,
            tenant_slot_floor: 0,
            platform_slots_per_aggregate: platform,
            platform_slots: platform,
            platform_slot_floor: 0,
            ..NetLimits::default()
        })
    }

    #[test]
    fn d130_tenant_ipv4_24_caps_new_slots_at_10_per_window() {
        let now = Instant::now();
        let mut rates = RateBuckets::default();
        assert_eq!(d130_fill(&mut rates, "ta", "198.51.100.1", 6, now), 6);
        // The cap is per /24, not per address.
        assert_eq!(d130_fill(&mut rates, "ta", "198.51.100.2", 10, now), 4);
        assert_eq!(d130(&mut rates, "ta", "198.51.100.3", "x", now), Some(60));
        let t = now + Duration::from_secs(20);
        assert_eq!(d130(&mut rates, "ta", "198.51.100.3", "x", t), Some(40));
        // Another /24 is unaffected.
        assert_eq!(d130_fill(&mut rates, "ta", "198.51.101.1", 10, now), 10);
        // Existing buckets are not new slots and keep working.
        assert_eq!(
            d130(&mut rates, "ta", "198.51.100.1", "ta-198.51.100.1-0", now),
            None
        );
        // Fixed window: a fresh allowance once it ends.
        assert_eq!(
            d130_fill(&mut rates, "ta", "198.51.100.9", 11, now + WINDOW),
            10
        );
    }

    #[test]
    fn d130_tenant_ipv6_caps_48_at_10_and_32_at_50() {
        let now = Instant::now();
        let mut rates = RateBuckets::default();
        assert_eq!(d130_fill(&mut rates, "ta", "2001:db8:1::/48", 15, now), 10);
        let got: usize = (2..20)
            .map(|n| d130_fill(&mut rates, "ta", &format!("2001:db8:{n:x}::/48"), 15, now))
            .sum();
        assert_eq!(got, 40, "the /32 allows 50 in total");
        assert_eq!(d130_fill(&mut rates, "ta", "2001:db9:1::/48", 15, now), 10);
    }

    #[test]
    fn d130_tenants_are_isolated() {
        let now = Instant::now();
        let mut rates = RateBuckets::default();
        assert_eq!(d130_fill(&mut rates, "ta", "198.51.100.1", 30, now), 10);
        assert_eq!(d130_fill(&mut rates, "tb", "198.51.100.1", 30, now), 10);
        assert_eq!(d130_fill(&mut rates, "ta", "2001:db8:1::/48", 30, now), 10);
        assert_eq!(d130_fill(&mut rates, "tb", "2001:db8:1::/48", 30, now), 10);
    }

    #[test]
    fn d130_platform_caps_a_network_across_tenants() {
        let now = Instant::now();
        let mut rates = RateBuckets::default();
        for t in 0..5 {
            assert_eq!(
                d130_fill(&mut rates, &format!("t{t}"), "198.51.100.1", 10, now),
                10
            );
            assert_eq!(
                d130_fill(&mut rates, &format!("t{t}"), "2001:db8:1::/48", 10, now),
                10
            );
        }
        // 50 = 5 x 10 used: a fresh tenant is refused on that /24 and /48.
        assert_eq!(d130_fill(&mut rates, "t5", "198.51.100.7", 10, now), 0);
        assert_eq!(d130_fill(&mut rates, "t5", "2001:db8:1::/48", 10, now), 0);
        // Platform counters are per /48 (another /48 of the /32 is fine).
        assert_eq!(d130_fill(&mut rates, "t5", "2001:db8:2::/48", 10, now), 10);
        assert_eq!(d130_fill(&mut rates, "t5", "198.51.101.1", 10, now), 10);
    }

    #[test]
    fn d130_platform_factor_is_configurable() {
        let now = Instant::now();
        let mut rates = RateBuckets::with_limits(NetLimits {
            platform_factor: 2,
            ..NetLimits::default()
        });
        assert_eq!(d130_fill(&mut rates, "ta", "198.51.100.1", 10, now), 10);
        assert_eq!(d130_fill(&mut rates, "tb", "198.51.100.1", 10, now), 10);
        assert_eq!(d130_fill(&mut rates, "tc", "198.51.100.1", 10, now), 0);
    }

    #[test]
    fn d130_refusal_does_not_reveal_the_layer() {
        let now = Instant::now();
        let mut rates = RateBuckets::default();
        // Tenant layer refusal.
        d130_fill(&mut rates, "ta", "198.51.100.1", 10, now);
        let tenant = d130(&mut rates, "ta", "198.51.100.1", "x", now);
        // Platform layer refusal on another /24.
        for t in 0..5 {
            d130_fill(&mut rates, &format!("p{t}"), "203.0.113.1", 10, now);
        }
        let platform = d130(&mut rates, "fresh", "203.0.113.1", "x", now);
        assert_eq!(tenant, Some(60));
        assert_eq!(tenant, platform);
    }

    #[test]
    fn d130_refused_request_counts_nothing() {
        let now = Instant::now();
        let mut rates = RateBuckets::default();
        d130_fill(&mut rates, "ta", "198.51.100.1", 10, now);
        let checks = [
            ("s".to_string(), "new".to_string(), 5),
            ("ip".to_string(), "198.51.100.1".to_string(), 5),
        ];
        assert!(!allowed(rates.check_tenant(
            &checks,
            "ta",
            "198.51.100.1",
            now
        )));
        assert!(rates.shards["s"].map.get("new").is_none());
        assert!(rates
            .shards
            .get("ip")
            .is_none_or(|s| s.map.get("198.51.100.1").is_none()));
    }

    #[test]
    fn d130_full_slots_with_nothing_expired_fail_closed_with_metric() {
        let now = Instant::now();
        let mut rates = small_slots(2, 100, 100);
        let ((), events) = capture(|| {
            assert_eq!(d130_fill(&mut rates, "ta", "198.51.100.1", 1, now), 1);
            assert_eq!(d130_fill(&mut rates, "ta", "198.51.101.1", 1, now), 1);
            let before = counter_slot_refusals();
            let t = now + Duration::from_secs(15);
            assert_eq!(d130(&mut rates, "ta", "198.51.102.1", "x", t), Some(45));
            assert_eq!(d130(&mut rates, "ta", "198.51.102.1", "y", t), Some(45));
            assert!(counter_slot_refusals() >= before + 2);
        });
        let ours: Vec<_> = events
            .iter()
            .filter(|e| e.0 == "vibe::rate_limit_counter_slots")
            .collect();
        assert_eq!(ours.len(), 1, "rate-limited log line: {events:?}");
        // Per-tenant slot caps: another tenant is unaffected.
        assert_eq!(d130_fill(&mut rates, "tb", "198.51.102.1", 1, now), 1);
        // Expired counters are cleared first, then the slot is reused.
        assert_eq!(
            d130(&mut rates, "ta", "198.51.102.1", "x", now + WINDOW),
            None
        );
    }

    #[test]
    fn d130_total_and_platform_slot_caps_fail_closed() {
        let now = Instant::now();
        let mut rates = small_slots(10, 2, 100);
        assert_eq!(d130_fill(&mut rates, "ta", "198.51.100.1", 1, now), 1);
        assert_eq!(d130_fill(&mut rates, "tb", "198.51.100.1", 1, now), 1);
        assert_eq!(d130_fill(&mut rates, "tc", "198.51.100.1", 1, now), 0);
        let mut rates = small_slots(10, 100, 1);
        assert_eq!(d130_fill(&mut rates, "ta", "198.51.100.1", 1, now), 1);
        assert_eq!(d130_fill(&mut rates, "ta", "198.51.101.1", 1, now), 0);
        assert_eq!(
            d130_fill(&mut rates, "ta", "198.51.101.1", 1, now + WINDOW),
            1
        );
    }

    #[test]
    fn d130_never_evicts_a_counter_inside_its_window() {
        let now = Instant::now();
        let mut rates = small_slots(3, 3, 3);
        assert_eq!(d130_fill(&mut rates, "ta", "198.51.100.1", 10, now), 10);
        let t = now + Duration::from_secs(10);
        assert_eq!(d130_fill(&mut rates, "ta", "198.51.101.1", 1, t), 1);
        assert_eq!(d130_fill(&mut rates, "ta", "198.51.102.1", 1, t), 1);
        // Full of live counters: many attempts from new /24s must not evict.
        let late = now + Duration::from_secs(59);
        for n in 0..50 {
            assert!(d130(&mut rates, "ta", &format!("10.0.{n}.1"), "k", late).is_some());
        }
        assert_eq!(rates.net.tenants.count("ta", "198.51.100.0/24", late), 10);
        assert_eq!(
            rates
                .net
                .platform
                .count("198.51.100.0/24", "198.51.100.0/24", late),
            10
        );
        assert_eq!(d130(&mut rates, "ta", "198.51.100.2", "k", late), Some(1));
        // Once the oldest window ends, only that counter is cleared.
        let end = now + WINDOW;
        assert_eq!(d130(&mut rates, "ta", "10.0.0.1", "k", end), None);
        assert_eq!(rates.net.tenants.count("ta", "198.51.101.0/24", end), 1);
        assert_eq!(rates.net.tenants.count("ta", "198.51.102.0/24", end), 1);
    }

    #[test]
    fn d130_fixed_window_edge_allows_up_to_2x() {
        // Accepted risk (docs/release-hardening.md).
        let now = Instant::now();
        let mut rates = RateBuckets::default();
        let edge = now + Duration::from_secs(59);
        assert_eq!(d130_fill(&mut rates, "ta", "198.51.100.1", 1, now), 1);
        assert_eq!(d130_fill(&mut rates, "ta", "198.51.100.2", 9, edge), 9);
        assert_eq!(
            d130_fill(&mut rates, "ta", "198.51.100.3", 10, now + WINDOW),
            10
        );
    }

    #[test]
    fn d130_layers_bound_counter_queues() {
        let now = Instant::now();
        let mut rates = small_slots(8, 8, 8);
        for i in 0..2_000u64 {
            let t = now + WINDOW * (i as u32);
            d130(
                &mut rates,
                "ta",
                &format!("10.{}.0.1", i % 8),
                &format!("k{i}"),
                t,
            );
        }
        assert!(rates.net.tenants.queue.len() <= 8 * 4 + 64 + 1);
        assert!(rates.net.tenants.entries.len() <= 8);
    }

    fn v4(i: usize) -> String {
        format!("10.{}.{}.1", i / 256, i % 256)
    }

    fn v6(slash32: usize, slash48: usize) -> String {
        format!("2001:{:x}:{:x}::/48", 0x1000 + slash32, slash48)
    }

    #[test]
    fn tenant_at_its_slot_limit_does_not_affect_another_tenant() {
        let now = Instant::now();
        let mut rates = RateBuckets::with_limits(NetLimits {
            slots_per_tenant: 4,
            ..NetLimits::default()
        });
        for i in 0..4 {
            assert!(rates.net.charge("ta", &v4(i), 1, now).is_ok());
        }
        assert!(matches!(
            rates.net.charge("ta", &v4(9), 1, now),
            Err(NetRefusal::SlotsFull("tenant", "table_full", _))
        ));
        for i in 0..4 {
            assert!(rates.net.charge("tb", &v4(100 + i), 1, now).is_ok());
        }
        assert!(rates.net.charge("ta", &v4(9), 1, now + WINDOW).is_ok());
    }

    #[test]
    fn many_prefixes_across_many_tenants_do_not_block_a_fresh_tenant() {
        let now = Instant::now();
        let mut rates = RateBuckets::default();
        for net in 0..4_096 {
            for t in 0..16 {
                assert!(rates.net.charge(&format!("t{t}"), &v4(net), 1, now).is_ok());
            }
        }
        assert!(rates.net.tenants.entries.len() >= 65_536);
        assert!(rates.net.charge("fresh", &v4(5_000), 1, now).is_ok());
        assert!(rates.net.charge("fresh", "2001:db8:1::/48", 1, now).is_ok());
    }

    #[test]
    fn tenant_below_its_floor_is_admitted_when_the_global_budget_is_used() {
        let now = Instant::now();
        let mut rates = RateBuckets::with_limits(NetLimits {
            slots_per_tenant: 8,
            tenant_slots_total: 16,
            tenant_slot_floor: 2,
            ..NetLimits::default()
        });
        for t in 0..2 {
            for i in 0..8 {
                assert!(rates
                    .net
                    .charge(&format!("t{t}"), &v4(t * 8 + i), 1, now)
                    .is_ok());
            }
        }
        assert!(rates.net.charge("fresh", &v4(100), 1, now).is_ok());
        assert!(rates.net.charge("fresh", &v4(101), 1, now).is_ok());
        assert!(rates.net.charge("fresh", &v4(102), 1, now).is_err());
        assert!(rates.net.charge("other", &v4(103), 1, now).is_ok());
    }

    #[test]
    fn full_platform_aggregate_does_not_block_unrelated_networks() {
        let now = Instant::now();
        let mut rates = RateBuckets::with_limits(NetLimits {
            platform_slots_per_aggregate: 16,
            ..NetLimits::default()
        });
        for i in 0..16 {
            assert!(rates
                .net
                .charge(&format!("t{}", i % 32), &v6(0, i), 1, now)
                .is_ok());
        }
        assert!(matches!(
            rates.net.charge("fresh", &v6(0, 99), 1, now),
            Err(NetRefusal::SlotsFull("platform", "prefix_cap_exceeded", _))
        ));
        assert!(rates.net.charge("fresh", &v6(1, 0), 1, now).is_ok());
        assert!(rates.net.charge("fresh", &v4(7), 1, now).is_ok());
    }

    #[test]
    fn many_ipv6_aggregates_across_many_tenants_do_not_block_a_fresh_network() {
        let now = Instant::now();
        let mut rates = RateBuckets::default();
        let mut n = 0;
        for a in 0..4_096 {
            for b in 0..16 {
                assert!(rates
                    .net
                    .charge(&format!("t{}", n % 64), &v6(a, b), 1, now)
                    .is_ok());
                n += 1;
            }
        }
        assert!(rates.net.platform.entries.len() >= 65_536);
        assert!(rates.net.charge("fresh", &v6(5_000, 0), 1, now).is_ok());
        assert!(rates.net.charge("t0", &v6(5_001, 0), 1, now).is_ok());
        assert!(rates.net.charge("fresh", &v4(1), 1, now).is_ok());
    }

    #[test]
    fn retry_after_is_the_same_whichever_layer_refuses() {
        let now = Instant::now();
        let later = now + Duration::from_secs(20);
        let at = now + Duration::from_secs(30);
        // Platform counter starts first, tenant counter later: tenant refuses.
        let mut a = RateBuckets::default();
        for t in 0..4 {
            d130_fill(&mut a, &format!("p{t}"), "198.51.100.1", 10, now);
        }
        assert_eq!(d130_fill(&mut a, "ta", "198.51.100.1", 10, later), 10);
        let tenant = d130(&mut a, "ta", "198.51.100.2", "x", at);
        // Same timeline, but the platform layer refuses a fresh tenant.
        let mut b = RateBuckets::default();
        for t in 0..4 {
            d130_fill(&mut b, &format!("p{t}"), "198.51.100.1", 10, now);
        }
        assert_eq!(d130_fill(&mut b, "ta", "198.51.100.1", 1, later), 1);
        assert_eq!(d130_fill(&mut b, "q", "198.51.100.1", 9, later), 9);
        let platform = d130(&mut b, "ta", "198.51.100.2", "x", at);
        assert_eq!(tenant, Some(50));
        assert_eq!(platform, tenant);
    }

    #[test]
    fn refusals_are_counted_by_layer_and_reason() {
        let now = Instant::now();
        let mut rates = small_slots(1, 100, 100);
        let cap = slot_refused_total("tenant", "network_cap");
        let full = slot_refused_total("tenant", "table_full");
        let pcap = slot_refused_total("platform", "network_cap");
        d130_fill(&mut rates, "ta", "198.51.100.1", 11, now);
        assert!(slot_refused_total("tenant", "network_cap") > cap);
        d130_fill(&mut rates, "ta", "198.51.101.1", 1, now);
        assert!(slot_refused_total("tenant", "table_full") > full);
        for t in 0..6 {
            d130_fill(&mut rates, &format!("p{t}"), "203.0.113.1", 10, now);
        }
        assert!(slot_refused_total("platform", "network_cap") > pcap);
        let text = slot_refused_metrics();
        assert!(text.contains(
            "vibe_rate_limit_slot_refused_total{layer=\"platform\",reason=\"table_full\"}"
        ));
    }

    #[test]
    fn ipv4_mapped_ipv6_counts_as_its_ipv4_24() {
        let now = Instant::now();
        let mut rates = RateBuckets::default();
        let mapped = owner_of(&ip_bucket("::ffff:198.51.100.7".parse().unwrap()));
        assert_eq!(mapped, "198.51.100.7");
        assert_eq!(d130_fill(&mut rates, "ta", "198.51.100.1", 6, now), 6);
        assert_eq!(d130_fill(&mut rates, "ta", &mapped, 10, now), 4);
        assert_eq!(owner_of("::ffff:198.51.100.7"), "198.51.100.7");
        assert_eq!(rates.net.tenants.count("ta", "198.51.100.0/24", now), 10);
    }

    fn refusal(rates: &mut RateBuckets, tenant: &str, owner: &str, now: Instant) -> NetRefusal {
        rates
            .net
            .charge(tenant, owner, 1, now)
            .expect_err("refused")
    }

    #[test]
    fn each_refusal_path_has_its_reason() {
        let now = Instant::now();
        // network_cap (tenant and platform layer).
        let mut rates = RateBuckets::default();
        d130_fill(&mut rates, "ta", "198.51.100.1", 10, now);
        assert!(matches!(
            refusal(&mut rates, "ta", "198.51.100.2", now),
            NetRefusal::Cap("tenant", _)
        ));
        for t in 0..5 {
            d130_fill(&mut rates, &format!("p{t}"), "203.0.113.1", 10, now);
        }
        assert!(matches!(
            refusal(&mut rates, "fresh", "203.0.113.1", now),
            NetRefusal::Cap("platform", _)
        ));
        // table_full: a tenant's own partition.
        let mut rates = RateBuckets::with_limits(NetLimits {
            slots_per_tenant: 2,
            ..NetLimits::default()
        });
        for i in 0..2 {
            assert!(rates.net.charge("ta", &v4(i), 1, now).is_ok());
        }
        assert!(matches!(
            refusal(&mut rates, "ta", &v4(5), now),
            NetRefusal::SlotsFull("tenant", "table_full", _)
        ));
        // tenant_budget_exhausted: the global tenant budget.
        let mut rates = RateBuckets::with_limits(NetLimits {
            slots_per_tenant: 4,
            tenant_slots_total: 4,
            tenant_slot_floor: 1,
            ..NetLimits::default()
        });
        for i in 0..4 {
            assert!(rates.net.charge("ta", &v4(i), 1, now).is_ok());
        }
        assert!(rates.net.charge("tb", &v4(10), 1, now).is_ok());
        assert!(matches!(
            refusal(&mut rates, "tb", &v4(11), now),
            NetRefusal::SlotsFull("tenant", "tenant_budget_exhausted", _)
        ));
        // prefix_cap_exceeded: a platform IPv4 /24 partition is full.
        let mut rates = RateBuckets::with_limits(NetLimits {
            platform_slots_per_aggregate: 1,
            ..NetLimits::default()
        });
        assert!(rates.net.charge("ta", "2001:db8:1::/48", 1, now).is_ok());
        assert!(matches!(
            refusal(&mut rates, "tb", "2001:db8:2::/48", now),
            NetRefusal::SlotsFull("platform", "prefix_cap_exceeded", _)
        ));
        // table_full: the platform total.
        let mut rates = RateBuckets::with_limits(NetLimits {
            platform_slots: 1,
            platform_slot_floor: 0,
            ..NetLimits::default()
        });
        assert!(rates.net.charge("ta", &v4(0), 1, now).is_ok());
        assert!(matches!(
            refusal(&mut rates, "tb", &v4(1), now),
            NetRefusal::SlotsFull("platform", "table_full", _)
        ));
    }

    #[test]
    fn budget_and_prefix_refusals_are_counted_and_logged() {
        let now = Instant::now();
        let budget = slot_refused_total("tenant", "tenant_budget_exhausted");
        let prefix = slot_refused_total("platform", "prefix_cap_exceeded");
        let mut rates = RateBuckets::with_limits(NetLimits {
            slots_per_tenant: 4,
            tenant_slots_total: 4,
            tenant_slot_floor: 2,
            platform_slots_per_aggregate: 1,
            ..NetLimits::default()
        });
        let ((), events) = capture(|| {
            for i in 0..4 {
                d130(&mut rates, "ta", &v4(i), &format!("a{i}"), now);
            }
            assert!(d130(&mut rates, "tb", &v4(10), "b0", now).is_none());
            assert!(d130(&mut rates, "tb", &v4(11), "b1", now).is_none());
            assert!(d130(&mut rates, "tb", &v4(12), "b2", now).is_some());
            assert!(d130(&mut rates, "tb", &v4(13), "b3", now).is_some());
            assert!(d130(&mut rates, "tc", "2001:db8:1::/48", "c0", now).is_none());
            assert!(d130(&mut rates, "td", "2001:db8:2::/48", "c1", now).is_some());
        });
        assert!(slot_refused_total("tenant", "tenant_budget_exhausted") >= budget + 2);
        assert!(slot_refused_total("platform", "prefix_cap_exceeded") > prefix);
        let ours = events
            .iter()
            .filter(|e| e.0 == "vibe::rate_limit_counter_slots")
            .count();
        assert_eq!(ours, 2, "one line per reason per minute: {events:?}");
    }

    #[test]
    fn memory_cap_fails_closed_with_table_full() {
        let now = Instant::now();
        let max = 20 * COUNTER_SLOT_BYTES + 5 * RateShard::bucket_bytes(5);
        let mut rates = RateBuckets::with_limits(NetLimits {
            table_memory_max_bytes: max,
            ..NetLimits::default()
        });
        let full = slot_refused_total("tenant", "table_full");
        let admitted = (0..20)
            .filter(|i| {
                d130(&mut rates, &format!("t{i}"), &v4(*i), &format!("k{i}"), now).is_none()
            })
            .count();
        assert!(admitted > 0 && admitted < 20, "admitted {admitted}");
        assert!(rates.estimated_bytes() <= max);
        // Same response shape as any other refusal.
        assert_eq!(d130(&mut rates, "fresh", &v4(99), "k", now), Some(60));
        assert!(slot_refused_total("tenant", "table_full") > full);
        // Existing buckets keep working; expiry frees memory.
        assert_eq!(d130(&mut rates, "t0", &v4(0), "k0", now), None);
        assert_eq!(d130(&mut rates, "fresh", &v4(99), "k2", now + WINDOW), None);
    }

    #[test]
    fn quota_growth_shares_the_memory_cap() {
        let rates = RateBuckets::with_limits(NetLimits {
            table_memory_max_bytes: 3 * QUOTA_ENTRY_BYTES,
            ..NetLimits::default()
        });
        let mut quotas = QuotaBuckets::with_memory(rates.memory());
        let day = "2026-10-16";
        assert!(quotas.check(day, &[("ip", "a".into(), 50)]));
        assert!(quotas.check(day, &[("ip", "b".into(), 50)]));
        assert!(quotas.check(day, &[("ip", "c".into(), 50)]));
        assert!(!quotas.check(day, &[("ip", "d".into(), 50)]));
        assert!(quotas.check(day, &[("ip", "a".into(), 50)]));
        assert_eq!(rates.estimated_bytes(), 3 * QUOTA_ENTRY_BYTES);
    }

    #[test]
    fn tenant_gets_its_platform_floor_when_the_shared_platform_pool_is_full() {
        let now = Instant::now();
        let mut rates = RateBuckets::with_limits(NetLimits {
            platform_slots: 8,
            platform_slot_floor: 2,
            ..NetLimits::default()
        });
        for i in 0..4 {
            assert!(rates.net.charge("ta", &v4(i), 1, now).is_ok());
            assert!(rates.net.charge("tb", &v4(10 + i), 1, now).is_ok());
        }
        assert_eq!(rates.net.platform.entries.len(), 8);
        // Above-floor growth is refused by the shared total.
        assert!(matches!(
            rates.net.charge("tb", &v4(20), 1, now),
            Err(NetRefusal::SlotsFull("platform", "table_full", _))
        ));
        // A tenant with no slots still gets its floor, IPv4 and IPv6.
        assert!(rates.net.charge("fresh", &v4(30), 1, now).is_ok());
        assert!(rates.net.charge("fresh", "2001:db8:1::/48", 1, now).is_ok());
        assert!(matches!(
            rates.net.charge("fresh", &v4(31), 1, now),
            Err(NetRefusal::SlotsFull("platform", "table_full", _))
        ));
        assert!(rates.net.charge("other", &v4(32), 1, now).is_ok());
        // Expired counters release the floor again.
        assert!(rates.net.charge("fresh", &v4(31), 1, now + WINDOW).is_ok());
    }

    #[test]
    fn tenant_gets_its_floor_when_the_memory_budget_is_used_by_other_tenants() {
        let now = Instant::now();
        let max = 200 * 1024;
        let reserve = 40 * 1024;
        let mut rates = RateBuckets::with_limits(NetLimits {
            tenant_slot_floor: 2,
            platform_slot_floor: 2,
            floor_reserve_bytes: reserve,
            table_memory_max_bytes: max,
            ..NetLimits::default()
        });
        let full = slot_refused_total("tenant", "table_full");
        let mut refused = 0;
        for t in 0..4 {
            for i in 0..100 {
                let tenant = format!("h{t}");
                let key = format!("{tenant}-{i}");
                if d130(&mut rates, &tenant, &v4(t * 100 + i), &key, now).is_some() {
                    refused += 1;
                }
            }
        }
        assert!(refused > 0, "the above-floor budget must be used up");
        // Only the heavy tenants' own floor slots may use the reserve.
        assert!(rates.estimated_bytes() <= max - reserve + 4 * 2 * FLOOR_SLOT_BYTES * 2);
        assert!(slot_refused_total("tenant", "table_full") > full);
        // A tenant with no slots gets its floor from the reserve.
        assert_eq!(d130(&mut rates, "fresh", &v4(1_000), "f0", now), None);
        assert_eq!(d130(&mut rates, "fresh", &v4(1_001), "f1", now), None);
        // Above its floor it is refused like everyone else.
        assert_eq!(d130(&mut rates, "fresh", &v4(1_002), "f2", now), Some(60));
        assert!(rates.estimated_bytes() <= max);
    }

    #[test]
    fn sizing_reserves_every_tenant_floor() {
        let limits = NetLimits::default().sized_for(EXPECTED_TENANTS);
        assert_eq!(limits.platform_slots, PLATFORM_SLOTS_TOTAL);
        assert_eq!(limits.floor_reserve_bytes, FLOOR_RESERVE_BYTES);
        assert_eq!(
            limits.platform_slots,
            limits.tenant_slots_total + EXPECTED_TENANTS * limits.platform_slot_floor
        );
        let big = NetLimits::default().sized_for(1_024);
        assert_eq!(
            big.platform_slots,
            1_024 * (SLOTS_PER_TENANT + PLATFORM_SLOT_FLOOR)
        );
        assert_eq!(
            big.floor_reserve_bytes,
            1_024 * PLATFORM_SLOT_FLOOR * FLOOR_SLOT_BYTES
        );
        // At most half of the memory cap is reserved.
        assert_eq!(big.floor_reserve(), TABLE_MEMORY_MAX_BYTES / 2);
        assert_eq!(FLOOR_RESERVE_BYTES, 48 * 1024 * 1024);
    }
}
