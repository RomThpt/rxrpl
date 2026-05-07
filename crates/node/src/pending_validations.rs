use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use rxrpl_consensus::types::Validation;

const MAX_PER_KEY: usize = 64;
const MAX_TOTAL: usize = 1024;
const TTL: Duration = Duration::from_secs(60);

/// Buffer for validations whose signing (ephemeral) public key is not yet
/// in the trusted set, typically because the matching manifest has not been
/// applied. Drained per-key when the manifest arrives.
pub struct PendingValidations {
    by_key: HashMap<Vec<u8>, VecDeque<(Validation, Instant)>>,
    total: usize,
    buffered_total: u64,
    replayed_total: u64,
}

impl PendingValidations {
    pub fn new() -> Self {
        Self {
            by_key: HashMap::new(),
            total: 0,
            buffered_total: 0,
            replayed_total: 0,
        }
    }

    pub fn buffer(&mut self, validation: Validation, now: Instant) {
        self.purge_expired(now);
        if self.total >= MAX_TOTAL {
            self.evict_oldest();
        }
        let key = validation.public_key.clone();
        let queue = self.by_key.entry(key).or_default();
        if queue.len() >= MAX_PER_KEY {
            queue.pop_front();
            self.total -= 1;
        }
        queue.push_back((validation, now));
        self.total += 1;
        self.buffered_total += 1;
    }

    /// Drain validations buffered for `public_key`, dropping any that
    /// have already expired.
    pub fn drain(&mut self, public_key: &[u8], now: Instant) -> Vec<Validation> {
        let Some(queue) = self.by_key.remove(public_key) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(queue.len());
        for (v, ts) in queue {
            self.total -= 1;
            if now.duration_since(ts) <= TTL {
                out.push(v);
            }
        }
        self.replayed_total += out.len() as u64;
        out
    }

    pub fn purge_expired(&mut self, now: Instant) {
        self.by_key.retain(|_, queue| {
            while let Some((_, ts)) = queue.front() {
                if now.duration_since(*ts) > TTL {
                    queue.pop_front();
                    self.total -= 1;
                } else {
                    break;
                }
            }
            !queue.is_empty()
        });
    }

    fn evict_oldest(&mut self) {
        let mut oldest_key: Option<Vec<u8>> = None;
        let mut oldest_ts: Option<Instant> = None;
        for (k, q) in &self.by_key {
            if let Some((_, ts)) = q.front() {
                if oldest_ts.is_none_or(|o| *ts < o) {
                    oldest_ts = Some(*ts);
                    oldest_key = Some(k.clone());
                }
            }
        }
        if let Some(k) = oldest_key {
            if let Some(q) = self.by_key.get_mut(&k) {
                q.pop_front();
                self.total -= 1;
                if q.is_empty() {
                    self.by_key.remove(&k);
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        self.total
    }

    pub fn buffered_total(&self) -> u64 {
        self.buffered_total
    }

    pub fn replayed_total(&self) -> u64 {
        self.replayed_total
    }
}

impl Default for PendingValidations {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_consensus::types::Validation;

    fn mk_val(pk: &[u8], seq: u32) -> Validation {
        Validation {
            public_key: pk.to_vec(),
            ledger_seq: seq,
            full: true,
            ..Default::default()
        }
    }

    #[test]
    fn buffer_and_drain_roundtrip() {
        let mut buf = PendingValidations::new();
        let now = Instant::now();
        buf.buffer(mk_val(b"keyA", 1), now);
        buf.buffer(mk_val(b"keyA", 2), now);
        buf.buffer(mk_val(b"keyB", 9), now);
        assert_eq!(buf.len(), 3);

        let drained = buf.drain(b"keyA", now);
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].ledger_seq, 1);
        assert_eq!(drained[1].ledger_seq, 2);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf.replayed_total(), 2);
    }

    #[test]
    fn per_key_cap_drops_oldest() {
        let mut buf = PendingValidations::new();
        let now = Instant::now();
        for i in 0..(MAX_PER_KEY as u32 + 5) {
            buf.buffer(mk_val(b"key", i), now);
        }
        assert_eq!(buf.len(), MAX_PER_KEY);
        let drained = buf.drain(b"key", now);
        assert_eq!(drained.len(), MAX_PER_KEY);
        assert_eq!(drained[0].ledger_seq, 5);
    }

    #[test]
    fn ttl_expires_entries() {
        let mut buf = PendingValidations::new();
        let t0 = Instant::now();
        buf.buffer(mk_val(b"key", 1), t0);
        let later = t0 + TTL + Duration::from_secs(1);
        let drained = buf.drain(b"key", later);
        assert!(drained.is_empty());
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn global_cap_evicts_oldest_across_keys() {
        let mut buf = PendingValidations::new();
        let t0 = Instant::now();
        for i in 0..MAX_TOTAL {
            let key = format!("k{}", i % 32);
            buf.buffer(mk_val(key.as_bytes(), i as u32), t0 + Duration::from_millis(i as u64));
        }
        assert_eq!(buf.len(), MAX_TOTAL);
        let later = t0 + Duration::from_secs(1);
        buf.buffer(mk_val(b"new", 0), later);
        assert_eq!(buf.len(), MAX_TOTAL);
    }

    #[test]
    fn drain_unknown_key_is_empty() {
        let mut buf = PendingValidations::new();
        assert!(buf.drain(b"nope", Instant::now()).is_empty());
    }
}
