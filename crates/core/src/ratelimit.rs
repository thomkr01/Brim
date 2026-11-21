//! Token buckets keyed by caller (a session id or a client address), kept in process memory.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

pub struct Bucket {
    tokens: f64,
    at: Instant,
}

/// Takes one request from `key`'s bucket, which holds `burst` and refills `rate` per second. Full
/// buckets are dropped once the map holds more than 10,000, so random keys cannot grow it forever.
/// Callers that share one map give each kind of key its own prefix (`ip:`, `sid:`).
pub fn take(map: &Mutex<HashMap<String, Bucket>>, key: &str, burst: f64, rate: f64) -> bool {
    take_at(map, key, burst, rate, Instant::now())
}

fn take_at(
    map: &Mutex<HashMap<String, Bucket>>,
    key: &str,
    burst: f64,
    rate: f64,
    now: Instant,
) -> bool {
    let mut m = map.lock().unwrap();
    if m.len() > 10_000 {
        m.retain(|_, b| now.duration_since(b.at).as_secs_f64() < burst / rate);
        // Still big: a flood of new keys. Start over, so the scan above runs once per 10,000 new
        // keys and not on every request (everyone gets a full bucket once).
        if m.len() > 5_000 {
            m.clear();
        }
    }
    let b = m.entry(key.to_string()).or_insert(Bucket {
        tokens: burst,
        at: now,
    });
    b.tokens = (b.tokens + now.duration_since(b.at).as_secs_f64() * rate).min(burst);
    b.at = now;
    let ok = b.tokens >= 1.0;
    if ok {
        b.tokens -= 1.0;
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn bucket_allows_burst_then_refills_per_key() {
        let m = Mutex::new(HashMap::new());
        let now = Instant::now();
        assert!((0..40).all(|_| take_at(&m, "a", 40.0, 20.0, now)));
        assert!(!take_at(&m, "a", 40.0, 20.0, now));
        assert!(
            take_at(&m, "b", 40.0, 20.0, now),
            "another key has its own bucket"
        );
        assert!(take_at(
            &m,
            "a",
            40.0,
            20.0,
            now + Duration::from_millis(100)
        ));
    }
}
