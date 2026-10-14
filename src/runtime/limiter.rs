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
//! network may hold at most [`OWNER_CAP`] active buckets per shard.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Buckets per `(route, key type)` shard.
pub const SHARD_CAPACITY: usize = 10_000;
pub const WINDOW: Duration = Duration::from_secs(60);
/// Pinned entries examined per eviction before refusing a new key.
const EVICT_SCAN: usize = 64;
/// Active buckets one client network (IPv4 address or IPv6 /48) may own in a
/// single shard. A client needs `SHARD_CAPACITY / OWNER_CAP` distinct
/// networks to fill a shard.
pub const OWNER_CAP: usize = SHARD_CAPACITY / 100;

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
}

impl RateShard {
    fn new() -> Self {
        Self {
            map: BoundedMap::new(SHARD_CAPACITY),
            owned: HashMap::new(),
            owner_keys: HashMap::new(),
            saturated_logged: None,
        }
    }

    fn release(owned: &mut HashMap<String, usize>, owner: &str) {
        if let Some(n) = owned.get_mut(owner) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                owned.remove(owner);
            }
        }
    }

    /// Makes room for one more bucket owned by `owner`: drops that owner's
    /// buckets that have no hit in the current window (removing them loses no
    /// state). Returns false when the owner still holds `OWNER_CAP` active
    /// buckets.
    fn owner_has_room(&mut self, owner: &str, now: Instant) -> bool {
        if self.owned.get(owner).copied().unwrap_or(0) < OWNER_CAP {
            return true;
        }
        let Some(keys) = self.owner_keys.get_mut(owner) else {
            return true;
        };
        let mut scanned = 0;
        while scanned < EVICT_SCAN {
            let Some(key) = keys.pop_front() else { break };
            scanned += 1;
            let Some(window) = self.map.get(&key) else {
                continue; // evicted or removed already
            };
            if window.owner != owner {
                continue;
            }
            if window.is_idle(now) {
                self.map.remove(&key);
                Self::release(&mut self.owned, owner);
                if self.owned.get(owner).copied().unwrap_or(0) < OWNER_CAP {
                    return true;
                }
            } else {
                keys.push_back(key);
            }
        }
        self.owned.get(owner).copied().unwrap_or(0) < OWNER_CAP
    }

    fn insert(&mut self, key: &str, owner: &str, limit: u32, now: Instant) -> bool {
        let owned = &mut self.owned;
        let made = self.map.touch_pinned_with(
            key,
            || RateWindow {
                limit,
                hits: Vec::new(),
                owner: owner.to_string(),
            },
            |w| w.evictable(now),
            |old| Self::release(owned, &old.owner),
        );
        if made.is_none() {
            return false;
        }
        *self.owned.entry(owner.to_string()).or_insert(0) += 1;
        let keys = self.owner_keys.entry(owner.to_string()).or_default();
        keys.push_back(key.to_string());
        let live = self.owned.get(owner).copied().unwrap_or(0);
        if keys.len() > live * 4 + 16 {
            let map = &self.map;
            keys.retain(|k| map.get(k).is_some_and(|w| w.owner == owner));
        }
        if self.owner_keys.len() > self.owned.len() * 2 + 64 {
            let owned = &self.owned;
            self.owner_keys.retain(|o, _| owned.contains_key(o));
        }
        true
    }

    fn log_saturation(&mut self, shard: &str, reason: &'static str, now: Instant) {
        if self
            .saturated_logged
            .is_some_and(|at| now.duration_since(at) < WINDOW)
        {
            return;
        }
        self.saturated_logged = Some(now);
        tracing::warn!(
            shard,
            reason,
            buckets = self.map.entries.len(),
            owners = self.owned.len(),
            "rate-limit shard saturated; refusing new keys"
        );
    }
}

/// Hits in the current window, the limit they are counted against, and the
/// client network that created the bucket.
pub struct RateWindow {
    limit: u32,
    hits: Vec<Instant>,
    owner: String,
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
        // New keys: refuse when the creating network already owns
        // `OWNER_CAP` active buckets in that shard, or when the shard is full
        // of pinned buckets. Nothing is counted or created on refusal.
        for (shard, key, _) in checks {
            let rs = self
                .shards
                .entry(shard.clone())
                .or_insert_with(RateShard::new);
            if rs.map.get(key).is_some() {
                continue;
            }
            if !rs.owner_has_room(owner, now) {
                rs.log_saturation(shard, "owner_cap", now);
                return RateDecision::Limited(WINDOW.as_secs());
            }
        }
        for (shard, key, limit) in checks {
            let rs = self.shards.get_mut(shard).expect("created above");
            if rs.map.get(key).is_some() {
                continue;
            }
            if !rs.insert(key, owner, *limit, now) {
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
                &format!("10.0.{}.{}", i / 256 % 256, i % 256),
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
                &format!("10.1.{}.{}", i / 256 % 256, i % 256),
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
                &format!("10.2.{}.{}", i / 256 % 256, i % 256),
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
            let o = format!("10.3.{}.{}", i / 256 % 256, i % 256);
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
}
