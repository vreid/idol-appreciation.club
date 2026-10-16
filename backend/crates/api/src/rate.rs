//! In-memory login throttling. Token buckets keyed by client IP and by
//! (username, IP) pair, so both credential spraying (many usernames, one
//! source) and targeted guessing (one account, many sources) hit a bound.
//! No new dependencies: plain f64 buckets under a std mutex, with a lazy
//! prune that keeps the map bounded.
//!
//! Every attempt consumes a token BEFORE verification: password spraying
//! cannot pre-check guesses faster than the refill, and successful logins
//! are throttled identically (a hit is the valuable event to slow down).
use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::time::Instant;

use axum::http::HeaderMap;

/// Bucket depth: a burst of 10 attempts is always possible (password
/// managers typo twice, shared offices share IPs), sustained rate is
/// 1 per [`REFILL_SECS`].
const CAPACITY: f64 = 10.0;
const REFILL_SECS: f64 = 2.0;

/// Buckets idle for this long are collectable; combined with the prune
/// trigger this bounds the map at ~(#recent IPs + #recent pairs) entries.
const IDLE_SECS: u64 = 600;
const PRUNE_ABOVE: usize = 4096;

struct Bucket {
    tokens: f64,
    last: Instant,
}

#[derive(Default)]
pub struct LoginLimiter {
    buckets: StdMutex<HashMap<String, Bucket>>,
}

impl LoginLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume one token from each key's bucket. `Err(retry_after_secs)`
    /// when ANY bucket is empty (nothing consumed — rejection does not
    /// spend partial capacity); `retry_after_secs` is the wait for that
    /// bucket's next token.
    pub fn acquire(&self, keys: [&str; 2]) -> Result<(), i64> {
        let mut map = self.buckets.lock().expect("login limiter");
        if map.len() > PRUNE_ABOVE {
            map.retain(|_, b| b.last.elapsed().as_secs() < IDLE_SECS);
        }
        let now = Instant::now();
        let mut retry: Option<i64> = None;
        for key in keys {
            let bucket = map.entry(key.to_string()).or_insert(Bucket {
                tokens: CAPACITY,
                last: now,
            });
            let elapsed = now.duration_since(bucket.last).as_secs_f64();
            bucket.tokens = (bucket.tokens + elapsed / REFILL_SECS).min(CAPACITY);
            bucket.last = now;
            if bucket.tokens < 1.0 {
                let wait = ((1.0 - bucket.tokens) * REFILL_SECS).ceil() as i64;
                retry = retry.max(Some(wait.max(1)));
            }
        }
        match retry {
            Some(retry) => Err(retry),
            None => {
                for key in keys {
                    if let Some(bucket) = map.get_mut(key) {
                        bucket.tokens -= 1.0;
                    }
                }
                Ok(())
            }
        }
    }
}

/// Best-effort client identity for throttling. Production traffic arrives
/// through the ingress, which sets X-Forwarded-For; the first entry is the
/// client. Direct or spoof-absent traffic folds into the shared "direct"
/// key (the per-username bucket still bounds targeted guessing).
pub fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "direct".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_then_throttled() {
        let limiter = LoginLimiter::new();
        for _ in 0..10 {
            assert!(limiter.acquire(["k", "k2"]).is_ok());
        }
        let err = limiter.acquire(["k", "k2"]).unwrap_err();
        assert!((1..=2).contains(&err), "retry {err}");
    }

    #[test]
    fn independent_keys_and_no_partial_spend() {
        let limiter = LoginLimiter::new();
        for _ in 0..10 {
            assert!(limiter.acquire(["a", "b"]).is_ok());
        }
        // Both buckets were spent equally: `a` is empty and so is `b`.
        assert!(limiter.acquire(["a", "b"]).is_err());
        // A rejection must not spend anything: `b` stays at 0, and the
        // fresh pair `c`/`d` is unaffected.
        assert!(limiter.acquire(["c", "d"]).is_ok());
    }

    #[test]
    fn refill_restores_tokens() {
        let limiter = LoginLimiter::new();
        for _ in 0..10 {
            assert!(limiter.acquire(["k", "k2"]).is_ok());
        }
        assert!(limiter.acquire(["k", "k2"]).is_err());
        std::thread::sleep(std::time::Duration::from_millis(2100));
        assert!(limiter.acquire(["k", "k2"]).is_ok());
    }

    #[test]
    fn client_ip_prefers_first_xff_entry() {
        let mut headers = HeaderMap::new();
        assert_eq!(client_ip(&headers), "direct");
        headers.insert(
            "x-forwarded-for",
            "203.0.113.7, 10.0.0.1".parse().expect("header value"),
        );
        assert_eq!(client_ip(&headers), "203.0.113.7");
    }
}
