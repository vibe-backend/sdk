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

/// Config may lower any limit freely, but may raise it to at most this
/// multiple of the default. A value above the cap, zero, or a value that
/// does not parse fails the run instead of being clamped or ignored.
pub const MAX_RAISE_FACTOR: u64 = 2;

impl Limits {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Read the limits through `get` (the environment in production).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let num = |name: &str, default: u64| -> Result<u64, String> {
            let Some(raw) = get(name) else {
                return Ok(default);
            };
            let cap = default * MAX_RAISE_FACTOR;
            let n = raw.trim().parse::<u64>().map_err(|_| {
                format!("{name} must be a whole number between 1 and {cap}, got {raw:?}")
            })?;
            if n == 0 || n > cap {
                return Err(format!(
                    "{name} must be between 1 and {cap} (default {default}, hard cap {MAX_RAISE_FACTOR}x), got {n}"
                ));
            }
            Ok(n)
        };
        let d = Self::default();
        Ok(Self {
            source_calls: num("LLM_MAX_CALLS_PER_SOURCE", d.source_calls as u64)? as u32,
            run_calls: num("LLM_MAX_CALLS_PER_RUN", d.run_calls as u64)? as u32,
            source_time: Duration::from_secs(num("LLM_SOURCE_DEADLINE", d.source_time.as_secs())?),
            run_time: Duration::from_secs(num("LLM_RUN_DEADLINE", d.run_time.as_secs())?),
        })
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

    pub fn from_env() -> Result<Self, String> {
        Ok(Self::new(Limits::from_env()?))
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

    fn limits(pairs: &[(&str, &str)]) -> Result<Limits, String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Limits::from_lookup(|name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        })
    }

    #[test]
    fn defaults_when_unset() {
        let l = limits(&[]).unwrap();
        assert_eq!(l.source_calls, 8);
        assert_eq!(l.run_calls, 200);
        assert_eq!(l.source_time, Duration::from_secs(300));
        assert_eq!(l.run_time, Duration::from_secs(1800));
    }

    #[test]
    fn config_may_lower_freely() {
        let l = limits(&[
            ("LLM_MAX_CALLS_PER_SOURCE", "1"),
            ("LLM_MAX_CALLS_PER_RUN", "5"),
            ("LLM_SOURCE_DEADLINE", "10"),
            ("LLM_RUN_DEADLINE", "60"),
        ])
        .unwrap();
        assert_eq!(l.source_calls, 1);
        assert_eq!(l.run_calls, 5);
        assert_eq!(l.source_time, Duration::from_secs(10));
        assert_eq!(l.run_time, Duration::from_secs(60));
    }

    #[test]
    fn raising_up_to_twice_the_default_is_allowed() {
        let l = limits(&[
            ("LLM_MAX_CALLS_PER_SOURCE", "16"),
            ("LLM_MAX_CALLS_PER_RUN", "400"),
            ("LLM_SOURCE_DEADLINE", "600"),
            ("LLM_RUN_DEADLINE", "3600"),
        ])
        .unwrap();
        assert_eq!(l.source_calls, 16);
        assert_eq!(l.run_calls, 400);
        assert_eq!(l.source_time, Duration::from_secs(600));
        assert_eq!(l.run_time, Duration::from_secs(3600));
    }

    #[test]
    fn above_the_hard_cap_fails() {
        for (name, v) in [
            ("LLM_MAX_CALLS_PER_SOURCE", "17"),
            ("LLM_MAX_CALLS_PER_RUN", "401"),
            ("LLM_SOURCE_DEADLINE", "601"),
            ("LLM_RUN_DEADLINE", "3601"),
        ] {
            let err = limits(&[(name, v)]).unwrap_err();
            assert!(err.contains(name) && err.contains("hard cap"), "{err}");
        }
    }

    #[test]
    fn invalid_values_fail() {
        for v in ["", "abc", "-1", "1.5", "0", "99999999999999999999999"] {
            let err = limits(&[("LLM_MAX_CALLS_PER_SOURCE", v)]).unwrap_err();
            assert!(err.contains("LLM_MAX_CALLS_PER_SOURCE"), "{v}: {err}");
        }
    }

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
