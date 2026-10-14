//! Bounded in-memory rate-limit and quota state.
//!
//! Buckets are sharded by `(route, key type)`. Every shard has a fixed
//! capacity. When a shard is full, its least recently used bucket is evicted
//! in O(1) amortized time, so the limiter never refuses requests just
//! because it is full. Flooding one shard with junk keys (e.g. rotating
//! `input.email` values) only evicts buckets in that shard, and only by
//! recency. It never blocks other clients and never needs a full scan while
//! the lock is held.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Buckets per `(route, key type)` shard.
pub const SHARD_CAPACITY: usize = 10_000;
pub const WINDOW: Duration = Duration::from_secs(60);

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
        self.generation += 1;
        let generation = self.generation;
        if !self.entries.contains_key(key) {
            while self.entries.len() >= self.capacity {
                self.evict_one();
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
        &mut entry.1
    }

    fn evict_one(&mut self) {
        while let Some((key, generation)) = self.order.pop_front() {
            if self
                .entries
                .get(&key)
                .is_some_and(|(g, _)| *g == generation)
            {
                self.entries.remove(&key);
                return;
            }
        }
    }
}

/// Sliding one-minute windows, sharded by `(route, key type)`.
#[derive(Default)]
pub struct RateBuckets {
    shards: HashMap<String, BoundedMap<Vec<Instant>>>,
}

/// Outcome of a rate check.
pub enum RateDecision {
    Allow,
    /// Seconds until the client may retry.
    Limited(u64),
}

impl RateBuckets {
    /// `checks` = `(shard, bucket key, per-minute limit)`. The request is
    /// counted only when every bucket admits it.
    pub fn check(&mut self, checks: &[(String, String, u32)], now: Instant) -> RateDecision {
        let mut worst: Option<u64> = None;
        for (shard, key, limit) in checks {
            let Some(map) = self.shards.get_mut(shard) else {
                continue;
            };
            if map.get(key).is_none() {
                continue;
            }
            let hits = map.touch(key, Vec::new);
            hits.retain(|at| now.duration_since(*at) < WINDOW);
            if hits.len() as u32 >= *limit {
                let wait = hits.first().map(|t| retry_after(*t, now)).unwrap_or(1);
                worst = Some(worst.map_or(wait, |w| w.max(wait)));
            }
        }
        if let Some(wait) = worst {
            return RateDecision::Limited(wait);
        }
        for (shard, key, _) in checks {
            self.shards
                .entry(shard.clone())
                .or_insert_with(|| BoundedMap::new(SHARD_CAPACITY))
                .touch(key, Vec::new)
                .push(now);
        }
        RateDecision::Allow
    }
}

/// Seconds until the oldest hit leaves the window (at least 1).
pub fn retry_after(oldest: Instant, now: Instant) -> u64 {
    let remaining = WINDOW.saturating_sub(now.duration_since(oldest));
    (remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0)).max(1)
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
            let decision = rates.check(&[(shard.clone(), format!("junk{i}"), 5)], now);
            assert!(matches!(decision, RateDecision::Allow));
        }
        let normal = [
            (shard.clone(), "real-user".to_string(), 5),
            ("login:ip".to_string(), "203.0.113.7".to_string(), 10),
        ];
        assert!(matches!(rates.check(&normal, now), RateDecision::Allow));
        // Limits still apply to that client.
        for _ in 0..4 {
            assert!(matches!(rates.check(&normal, now), RateDecision::Allow));
        }
        assert!(matches!(
            rates.check(&normal, now),
            RateDecision::Limited(_)
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
}
