use dashmap::DashMap;
use std::time::{Duration, Instant};

struct Bucket {
    window_start: Instant,
    count: u32,
}

pub struct MintLimiter {
    buckets: DashMap<String, Bucket>,
    limit: u32,
    window: Duration,
    burst: u32,
}

impl MintLimiter {
    pub fn new(limit: u32, window_secs: u64, burst: u32) -> Self {
        Self {
            buckets: DashMap::new(),
            limit: limit.max(1),
            window: Duration::from_secs(window_secs.max(1)),
            burst,
        }
    }

    pub fn allow(&self, ip: &str) -> bool {
        let now = Instant::now();
        let mut entry = self
            .buckets
            .entry(ip.to_string())
            .or_insert_with(|| Bucket {
                window_start: now,
                count: 0,
            });

        if now.duration_since(entry.window_start) >= self.window {
            entry.window_start = now;
            entry.count = 0;
        }

        if entry.count < self.burst {
            entry.count += 1;
            return true;
        }

        if entry.count < self.limit {
            entry.count += 1;
            return true;
        }

        false
    }

    pub fn prune(&self) {
        let now = Instant::now();
        self.buckets
            .retain(|_, b| now.duration_since(b.window_start) < self.window);
    }

    pub fn len_probe(&self) -> usize {
        self.buckets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_allows_immediate_mints() {
        let limiter = MintLimiter::new(60, 600, 15);
        for _ in 0..15 {
            assert!(limiter.allow("1.2.3.4"));
        }
        assert!(limiter.allow("1.2.3.4"));
    }

    #[test]
    fn rejects_after_limit() {
        let limiter = MintLimiter::new(3, 600, 1);
        assert!(limiter.allow("ip-a"));
        assert!(limiter.allow("ip-a"));
        assert!(limiter.allow("ip-a"));
        assert!(!limiter.allow("ip-a"));
        assert!(limiter.allow("ip-b"));
    }

    #[test]
    fn rejects_without_burst_when_limit_reached() {
        let limiter = MintLimiter::new(2, 600, 2);
        assert!(limiter.allow("x"));
        assert!(limiter.allow("x"));
        assert!(!limiter.allow("x"));
    }

    #[test]
    fn prune_removes_stale_buckets() {
        let limiter = MintLimiter::new(3, 600, 1);
        limiter.allow("a");
        limiter.allow("b");
        assert_eq!(limiter.len_probe(), 2);

        for mut b in limiter.buckets.iter_mut() {
            b.window_start = Instant::now() - Duration::from_secs(601);
        }
        limiter.prune();
        assert_eq!(limiter.len_probe(), 0);
    }
}
