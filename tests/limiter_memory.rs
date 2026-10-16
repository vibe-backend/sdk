//! Measures heap bytes per limiter table entry with a counting allocator and
//! checks the per-entry estimates used by the memory cap are upper bounds.
//! Run with `--nocapture` to print the figures.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Instant;

use vibe::runtime::{NetLimits, QuotaBuckets, RateBuckets};

struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        LIVE.fetch_add(new as isize - layout.size() as isize, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

const N: usize = 8_000;
const LIMIT: u32 = 5;

fn fill(rates: &mut RateBuckets, now: Instant) {
    for i in 0..N {
        let owner = format!("10.{}.{}.1", i / 256, i % 256);
        let checks = [("s".to_string(), format!("key-{i}"), LIMIT)];
        let tenant = format!("tenant{}", i % 8);
        let _ = rates.check_tenant(&checks, &tenant, &owner, now);
    }
}

#[test]
fn limiter_bytes_per_entry_are_within_the_estimates() {
    let now = Instant::now();

    let before = LIVE.load(Ordering::Relaxed);
    let mut shard_only = RateBuckets::with_limits(NetLimits {
        tenant_net_cap: 0,
        ..NetLimits::default()
    });
    fill(&mut shard_only, now);
    let bucket = (LIVE.load(Ordering::Relaxed) - before) as f64 / N as f64;
    drop(shard_only);

    let before = LIVE.load(Ordering::Relaxed);
    let mut both = RateBuckets::default();
    fill(&mut both, now);
    let total = (LIVE.load(Ordering::Relaxed) - before) as f64;
    let (t, p) = both.counter_slots();
    assert_eq!((t, p), (N, N));
    let slot = (total - bucket * N as f64) / (t + p) as f64;
    let estimated = both.estimated_bytes();
    drop(both);

    let before = LIVE.load(Ordering::Relaxed);
    let mut quotas = QuotaBuckets::default();
    for i in 0..N {
        quotas.check(
            "2026-10-16",
            &[("ip", format!("10.{}.{}.1", i / 256, i % 256), 50)],
        );
    }
    let quota = (LIVE.load(Ordering::Relaxed) - before) as f64 / N as f64;
    drop(quotas);

    println!(
        "measured over {N} entries: rate bucket {bucket:.0} B (limit {LIMIT}), \
         D130 counter slot {slot:.0} B, quota entry {quota:.0} B; \
         estimate {estimated} B vs measured {total:.0} B"
    );
    let bucket_estimate = vibe::runtime::BUCKET_BYTES + vibe::runtime::HIT_BYTES * LIMIT as usize;
    assert!(bucket <= bucket_estimate as f64, "bucket {bucket}");
    assert!(
        slot <= vibe::runtime::COUNTER_SLOT_BYTES as f64,
        "slot {slot}"
    );
    assert!(
        quota <= vibe::runtime::QUOTA_ENTRY_BYTES as f64,
        "quota {quota}"
    );
    assert!(estimated as f64 >= total, "estimate below measured");
}
