//! Per-IP token-bucket rate limit for the JSON-RPC + WebSocket endpoints.
//!
//! Mitigates audit finding **H4** (an unauthenticated client could flood
//! the RPC port with thousands of cheap requests per second). Implementation
//! choices:
//!
//! - **In-process state.** No external dependency (Redis, tower-governor) is
//!   warranted for a single-node validator; a `DashMap<IpAddr, Bucket>`
//!   suffices and avoids cross-task locks.
//! - **Token bucket.** A 100-token bucket refills at 10 tokens / second,
//!   so steady throughput is 10 req/s with a 100-burst. Browsers running
//!   `wscompat`-style suites stay well under that.
//! - **Privileged loopback bypass.** 127.0.0.0/8 and ::1 always pass —
//!   admin-only RPC tooling and operator scripts must not be throttled by
//!   their own host.
//!
//! Memory: each `IpAddr` entry is ~32 B; a passive eviction sweep every
//! `EVICT_INTERVAL` removes idle buckets so the map cannot grow unbounded
//! from a churn of one-shot client IPs.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::extract::ConnectInfo;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use once_cell::sync::Lazy;

// Token bucket: 1000-token burst, 100 tokens/s refill. Steady throughput is
// 100 req/s per IP with a 1000-burst absorber. Tuned for batch RPC clients
// (xrpl-hive txcompat) and operator tools that legitimately fire dozens of
// requests in the first second of a test. Still mitigates trivial floods —
// an attacker hammering a single IP at >100 req/s sustained will get HTTP 429.
const TOKENS_BURST: u64 = 1000;
const TOKENS_PER_SEC: u64 = 100;
const EVICT_INTERVAL: Duration = Duration::from_secs(300);

struct Bucket {
    tokens_milli: u64,
    last_refill_unix_ms: u64,
}

static BUCKETS: Lazy<DashMap<IpAddr, Bucket>> = Lazy::new(DashMap::new);
static LAST_EVICT_MS: AtomicU64 = AtomicU64::new(0);

/// Axum middleware that throttles per-IP request rate.
pub async fn rate_limit_by_ip(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let ip = addr.ip();
    if is_loopback(&ip) {
        return next.run(request).await;
    }

    if !try_consume(ip, 1000) {
        return (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded\n").into_response();
    }

    maybe_evict_idle();
    next.run(request).await
}

fn is_loopback(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Try to deduct `cost_milli` thousandths of a token. Returns true if it succeeded.
fn try_consume(ip: IpAddr, cost_milli: u64) -> bool {
    let now_ms = now_unix_ms();
    let mut entry = BUCKETS.entry(ip).or_insert_with(|| Bucket {
        tokens_milli: TOKENS_BURST * 1000,
        last_refill_unix_ms: now_ms,
    });

    // Refill since last visit.
    let elapsed_ms = now_ms.saturating_sub(entry.last_refill_unix_ms);
    let refill_milli = elapsed_ms.saturating_mul(TOKENS_PER_SEC); // tokens_per_sec * (ms/1000) * 1000
    entry.tokens_milli = entry
        .tokens_milli
        .saturating_add(refill_milli)
        .min(TOKENS_BURST * 1000);
    entry.last_refill_unix_ms = now_ms;

    if entry.tokens_milli >= cost_milli {
        entry.tokens_milli -= cost_milli;
        true
    } else {
        false
    }
}

fn maybe_evict_idle() {
    let now_ms = now_unix_ms();
    let last = LAST_EVICT_MS.load(Ordering::Relaxed);
    if now_ms.saturating_sub(last) < EVICT_INTERVAL.as_millis() as u64 {
        return;
    }
    if LAST_EVICT_MS
        .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    let cutoff_ms = now_ms.saturating_sub(EVICT_INTERVAL.as_millis() as u64);
    BUCKETS.retain(|_, b| b.last_refill_unix_ms >= cutoff_ms);
}

fn now_unix_ms() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// Silences unused warning in non-test builds where `Arc` is only required
// by a future expansion that will share buckets across multiple endpoints.
#[allow(dead_code)]
fn _link_arc() -> Option<Arc<()>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    // Tests share a process-global BUCKETS map. To stay independent under
    // `cargo test`'s parallel scheduler each test owns a unique TEST-NET
    // IP that no other test (or production code path) ever touches.

    #[test]
    fn loopback_bypass() {
        assert!(is_loopback(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
    }

    #[test]
    fn burst_then_throttle() {
        let ip: IpAddr = "198.51.100.1".parse().unwrap();
        BUCKETS.remove(&ip);
        for _ in 0..TOKENS_BURST {
            assert!(try_consume(ip, 1000));
        }
        assert!(!try_consume(ip, 1000));
        BUCKETS.remove(&ip);
    }

    #[test]
    fn refill_over_time() {
        let ip: IpAddr = "198.51.100.2".parse().unwrap();
        BUCKETS.remove(&ip);
        for _ in 0..TOKENS_BURST {
            assert!(try_consume(ip, 1000));
        }
        // Wind back the clock 1 second so the bucket refills exactly
        // TOKENS_PER_SEC tokens.
        if let Some(mut entry) = BUCKETS.get_mut(&ip) {
            entry.last_refill_unix_ms = entry.last_refill_unix_ms.saturating_sub(1_000);
        }
        let attempts = (TOKENS_PER_SEC as usize) + 30;
        let mut admitted = 0;
        for _ in 0..attempts {
            if try_consume(ip, 1000) {
                admitted += 1;
            }
        }
        // Refill = TOKENS_PER_SEC tokens; allow ±20% jitter from the
        // shared clock skew between BUCKETS init and the rewound time.
        let lower = (TOKENS_PER_SEC * 8 / 10) as usize;
        let upper = (TOKENS_PER_SEC * 12 / 10) as usize;
        assert!(
            (lower..=upper).contains(&admitted),
            "expected ~{} admitted (±20%), got {admitted}",
            TOKENS_PER_SEC
        );
        BUCKETS.remove(&ip);
    }
}
