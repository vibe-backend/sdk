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

    #[cfg(test)]
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

/// Sliding one-minute windows, sharded by `(route, key type)`.
#[derive(Default)]
pub struct RateBuckets {
    shards: HashMap<String, RateShard>,
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
}

impl RateShard {
    fn new() -> Self {
        Self {
            map: BoundedMap::new(SHARD_CAPACITY),
            owned: HashMap::new(),
            owner_keys: HashMap::new(),
            saturated_logged: None,
            refused_since_log: 0,
        }
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
    /// `checks` = `(shard, bucket key, per-minute limit)`; `owner` is the
    /// client network ([`owner_of`]) that new buckets are charged to. The
    /// request is counted only when every bucket admits it.
    pub fn check(
        &mut self,
        checks: &[(String, String, u32)],
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
        for (shard, key, limit) in checks {
            let rs = self.shards.get_mut(shard).expect("created above");
            if rs.map.get(key).is_some() {
                continue;
            }
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
#[derive(Default)]
pub struct QuotaBuckets {
    shards: HashMap<&'static str, BoundedMap<(String, u32)>>,
}

impl QuotaBuckets {
    /// `checks` = `(key type, key, daily limit)`. Counts only when all admit.
    pub fn check(&mut self, day: &str, checks: &[(&'static str, String, u32)]) -> bool {
        for (kind, key, limit) in checks {
            if let Some((d, count)) = self.shards.get(kind).and_then(|m| m.get(key)) {
                if d == day && count >= limit {
                    return false;
                }
            }
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
        let mut rates = RateBuckets::default();
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
        let mut rates = RateBuckets::default();
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
        let mut rates = RateBuckets::default();
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
        let mut rates = RateBuckets::default();
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
        let mut rates = RateBuckets::default();
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
        let mut rates = RateBuckets::default();
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
        let mut rates = RateBuckets::default();
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
        let mut rates = RateBuckets::default();
        for n in 0..100 {
            let owner = format!("2001:db8:{n:x}::/48");
            pinned_fill(&mut rates, "login:ip", &owner, OWNER_CAP, &owner, now);
        }
        assert_eq!(rates.shards["login:ip"].map.entries.len(), NETWORK_CAP);
        // 100 /48s in 100 different /32s: the reserve keeps room.
        let mut rates = RateBuckets::default();
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
        let mut rates = RateBuckets::default();
        for n in 0..100 {
            let owner = format!("198.51.100.{n}");
            pinned_fill(&mut rates, shard, &owner, OWNER_CAP, &owner, now);
        }
        assert_eq!(rates.shards[shard].map.entries.len(), NETWORK_CAP);
        // 100 different /24s.
        let mut rates = RateBuckets::default();
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
        let mut rates = RateBuckets::default();
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
            let mut rates = RateBuckets::default();
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
            let mut rates = RateBuckets::default();
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
            let mut rates = RateBuckets::default();
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
            let mut rates = RateBuckets::default();
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
            let mut rates = RateBuckets::default();
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
        let mut rates = RateBuckets::default();
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
        let mut rates = RateBuckets::default();
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
        let mut rates = RateBuckets::default();
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
        let mut rates = RateBuckets::default();
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
        let mut rates = RateBuckets::default();
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
}
