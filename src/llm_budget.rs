//! One cap on LLM calls and wall-clock time, per source and per compile
//! run. Every HTTP attempt (first try, transport retry, format repair,
//! translation) reserves from the same budget, and each attempt's timeout
//! is clipped to the time left, so no call can overrun the cap.

use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// HTTP attempts one source may make (`LLM_MAX_CALLS_PER_SOURCE`, default 8).
    pub source_calls: u32,
    /// HTTP attempts one run may make (`LLM_MAX_CALLS_PER_RUN`, default 200).
    pub run_calls: u32,
    /// Wall clock one source may spend in LLM calls (`LLM_SOURCE_DEADLINE`, default 300s).
    pub source_time: Duration,
    /// Wall clock one run may spend in LLM calls (`LLM_RUN_DEADLINE`, default 1800s).
    pub run_time: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            source_calls: 8,
            run_calls: 200,
            source_time: Duration::from_secs(300),
            run_time: Duration::from_secs(1800),
        }
    }
}

impl Limits {
    pub fn from_env() -> Self {
        let num = |name: &str, default: u64, max: u64| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(|n| n.min(max))
                .unwrap_or(default)
        };
        let d = Self::default();
        Self {
            source_calls: num("LLM_MAX_CALLS_PER_SOURCE", d.source_calls as u64, 100) as u32,
            run_calls: num("LLM_MAX_CALLS_PER_RUN", d.run_calls as u64, 10_000) as u32,
            source_time: Duration::from_secs(num(
                "LLM_SOURCE_DEADLINE",
                d.source_time.as_secs(),
                3600,
            )),
            run_time: Duration::from_secs(num("LLM_RUN_DEADLINE", d.run_time.as_secs(), 86_400)),
        }
    }
}

#[derive(Debug)]
struct State {
    run_start: Instant,
    run_calls: u32,
    source_start: Instant,
    source_calls: u32,
}

#[derive(Debug)]
pub struct CallBudget {
    limits: Limits,
    state: Mutex<State>,
}

impl CallBudget {
    pub fn new(limits: Limits) -> Self {
        let now = Instant::now();
        Self {
            limits,
            state: Mutex::new(State {
                run_start: now,
                run_calls: 0,
                source_start: now,
                source_calls: 0,
            }),
        }
    }

    pub fn from_env() -> Self {
        Self::new(Limits::from_env())
    }

    /// Start the per-source counters for the next source.
    pub fn begin_source(&self) {
        let mut s = self.state.lock().expect("budget lock");
        s.source_start = Instant::now();
        s.source_calls = 0;
    }

    /// Time left before either deadline.
    pub fn remaining(&self) -> Duration {
        let s = self.state.lock().expect("budget lock");
        self.left(&s)
    }

    fn left(&self, s: &State) -> Duration {
        let run = self.limits.run_time.saturating_sub(s.run_start.elapsed());
        let source = self
            .limits
            .source_time
            .saturating_sub(s.source_start.elapsed());
        run.min(source)
    }

    /// Reserve one HTTP attempt. Returns the time it may take, or why the
    /// budget is spent.
    pub fn reserve(&self) -> Result<Duration, String> {
        let mut s = self.state.lock().expect("budget lock");
        if s.source_calls >= self.limits.source_calls {
            return Err(format!(
                "LLM call budget spent: {} calls for this source (LLM_MAX_CALLS_PER_SOURCE)",
                self.limits.source_calls
            ));
        }
        if s.run_calls >= self.limits.run_calls {
            return Err(format!(
                "LLM call budget spent: {} calls in this run (LLM_MAX_CALLS_PER_RUN)",
                self.limits.run_calls
            ));
        }
        let left = self.left(&s);
        if left.is_zero() {
            return Err(
                "LLM time budget spent (LLM_SOURCE_DEADLINE / LLM_RUN_DEADLINE)".to_string(),
            );
        }
        s.source_calls += 1;
        s.run_calls += 1;
        Ok(left)
    }

    pub fn calls(&self) -> (u32, u32) {
        let s = self.state.lock().expect("budget lock");
        (s.source_calls, s.run_calls)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_calls_per_source_and_run() {
        let b = CallBudget::new(Limits {
            source_calls: 2,
            run_calls: 3,
            ..Limits::default()
        });
        assert!(b.reserve().is_ok());
        assert!(b.reserve().is_ok());
        assert!(b.reserve().unwrap_err().contains("this source"));
        b.begin_source();
        assert!(b.reserve().is_ok());
        assert!(b.reserve().unwrap_err().contains("this run"));
    }

    #[test]
    fn caps_time() {
        let b = CallBudget::new(Limits {
            source_time: Duration::ZERO,
            ..Limits::default()
        });
        assert!(b.reserve().unwrap_err().contains("time budget"));
    }
}
