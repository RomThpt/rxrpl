use std::collections::HashMap;

use rxrpl_ledger::Ledger;
use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::keylet;

use crate::types::PathFindTrustLine;

/// Cache of trust lines per account for pathfinding.
pub struct RippleLineCache {
    cache: HashMap<AccountId, Vec<PathFindTrustLine>>,
}

impl RippleLineCache {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    /// Get trust lines for an account, loading from ledger if not cached.
    pub fn get_lines(&mut self, ledger: &Ledger, account: &AccountId) -> &[PathFindTrustLine] {
        if !self.cache.contains_key(account) {
            let lines = load_trust_lines(ledger, account);
            self.cache.insert(*account, lines);
        }
        &self.cache[account]
    }
}

/// Load trust lines from an account's owner directory.
fn load_trust_lines(ledger: &Ledger, account: &AccountId) -> Vec<PathFindTrustLine> {
    let mut lines = Vec::new();
    let root = keylet::owner_dir(account);

    let dir_data = match ledger.get_state(&root) {
        Some(d) => d,
        None => return lines,
    };

    let mut page = 0u64;
    loop {
        let page_data = if page == 0 {
            Some(dir_data)
        } else {
            let page_key = keylet::dir_node(&root, page);
            ledger.get_state(&page_key)
        };

        let page_json: serde_json::Value = match page_data {
            Some(data) => match serde_json::from_slice(data) {
                Ok(v) => v,
                Err(_) => break,
            },
            None => break,
        };

        if let Some(indexes) = page_json.get("Indexes").and_then(|v| v.as_array()) {
            for idx_val in indexes {
                let idx_str = idx_val.as_str().unwrap_or_default();
                let idx_hash: Hash256 = match idx_str.parse() {
                    Ok(h) => h,
                    Err(_) => continue,
                };

                if let Some(entry_data) = ledger.get_state(&idx_hash) {
                    if let Ok(entry) = serde_json::from_slice::<serde_json::Value>(entry_data) {
                        if entry.get("LedgerEntryType").and_then(|v| v.as_str())
                            == Some("RippleState")
                        {
                            if let Some(line) = parse_trust_line(&entry, account) {
                                lines.push(line);
                            }
                        }
                    }
                }

                if lines.len() >= 400 {
                    break;
                }
            }
        }

        match page_json.get("IndexNext").and_then(|v| v.as_u64()) {
            Some(next) if next != 0 => page = next,
            _ => break,
        }
    }

    lines
}

/// No-ripple flag constants.
const LSF_LOW_NO_RIPPLE: u64 = 0x0010_0000;
const LSF_HIGH_NO_RIPPLE: u64 = 0x0020_0000;

/// Parse a RippleState entry into a PathFindTrustLine relative to `account`.
fn parse_trust_line(entry: &serde_json::Value, account: &AccountId) -> Option<PathFindTrustLine> {
    let high_issuer = entry
        .get("HighLimit")
        .and_then(|v| v.get("issuer"))
        .and_then(|v| v.as_str())?;
    let low_issuer = entry
        .get("LowLimit")
        .and_then(|v| v.get("issuer"))
        .and_then(|v| v.as_str())?;

    let account_str = rxrpl_codec::address::classic::encode_account_id(account);
    let is_high = high_issuer == account_str;

    let peer_str = if is_high { low_issuer } else { high_issuer };
    let peer = rxrpl_codec::address::classic::decode_account_id(peer_str).ok()?;

    let currency = extract_currency(entry)?;

    let balance_val = entry
        .get("Balance")
        .and_then(|v| v.get("value"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);

    // Balance is from low's perspective; negate if we are high
    let balance = if is_high { -balance_val } else { balance_val };

    let limit = if is_high {
        entry
            .get("HighLimit")
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0)
    } else {
        entry
            .get("LowLimit")
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0)
    };

    let peer_limit = if is_high {
        entry
            .get("LowLimit")
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0)
    } else {
        entry
            .get("HighLimit")
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0)
    };

    let flags = entry.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0);

    let no_ripple = if is_high {
        (flags & LSF_HIGH_NO_RIPPLE) != 0
    } else {
        (flags & LSF_LOW_NO_RIPPLE) != 0
    };

    let peer_no_ripple = if is_high {
        (flags & LSF_LOW_NO_RIPPLE) != 0
    } else {
        (flags & LSF_HIGH_NO_RIPPLE) != 0
    };

    Some(PathFindTrustLine {
        peer,
        currency,
        balance,
        limit,
        peer_limit,
        no_ripple,
        peer_no_ripple,
    })
}

/// Extract the 20-byte currency code from a RippleState entry.
fn extract_currency(entry: &serde_json::Value) -> Option<[u8; 20]> {
    let currency_str = entry
        .get("Balance")
        .and_then(|v| v.get("currency"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            entry
                .get("HighLimit")
                .and_then(|v| v.get("currency"))
                .and_then(|v| v.as_str())
        })?;

    let mut bytes = [0u8; 20];
    if currency_str.len() == 3 {
        bytes[12] = currency_str.as_bytes()[0];
        bytes[13] = currency_str.as_bytes()[1];
        bytes[14] = currency_str.as_bytes()[2];
    } else if currency_str.len() == 40 {
        let decoded = hex::decode(currency_str).ok()?;
        bytes.copy_from_slice(&decoded);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_standard_currency() {
        let entry = serde_json::json!({
            "Balance": { "currency": "USD", "value": "100" },
        });
        let c = extract_currency(&entry).unwrap();
        assert_eq!(&c[12..15], b"USD");
    }
}
