use rxrpl_amount::IOUAmount;
use rxrpl_ledger::Ledger;
use rxrpl_primitives::AccountId;

use crate::book_index::BookIndex;
use crate::currencies::account_currencies;
use crate::finder::Pathfinder;
use crate::line_cache::RippleLineCache;
use crate::ranking::{compute_path_ranks, get_best_paths};
use crate::types::{
    Issue, PATH_STEP_ACCOUNT, PATH_STEP_CURRENCY, PATH_STEP_ISSUER, PathAlternative, PathStep,
};

/// Maximum alternatives returned.
const MAX_ALTERNATIVES: usize = 4;

/// A path-finding request.
pub struct PathRequest {
    pub source: AccountId,
    pub destination: AccountId,
    pub destination_amount: serde_json::Value,
    pub source_currencies: Option<Vec<Issue>>,
}

impl PathRequest {
    /// Execute the path-finding request against a ledger.
    pub fn find_paths(&self, ledger: &Ledger) -> Vec<PathAlternative> {
        let dst_issue = match parse_amount_issue(&self.destination_amount) {
            Some(i) => i,
            None => return Vec::new(),
        };

        let book_index = BookIndex::build(ledger);
        let mut line_cache = RippleLineCache::new();

        // Determine source currencies. When a caller supplies entries with no
        // issuer (a wildcard over issuers, signalled by an all-zero AccountId
        // on a non-XRP issue), expand each wildcard against the source's
        // actual trust lines for that currency.
        let src_issues = match &self.source_currencies {
            Some(currencies) => {
                let mut expanded = Vec::new();
                let mut account_lines: Option<Vec<Issue>> = None;
                for issue in currencies {
                    if issue.is_xrp() || issue.issuer != rxrpl_primitives::AccountId([0u8; 20]) {
                        expanded.push(issue.clone());
                        continue;
                    }
                    let lines = account_lines.get_or_insert_with(|| {
                        account_currencies(ledger, &self.source, &mut line_cache)
                    });
                    for line in lines.iter() {
                        if line.currency == issue.currency {
                            expanded.push(line.clone());
                        }
                    }
                }
                expanded
            }
            None => {
                let mut issues = account_currencies(ledger, &self.source, &mut line_cache);
                // Always include XRP
                if !issues.iter().any(|i| i.is_xrp()) {
                    issues.insert(0, Issue::xrp());
                }
                issues
            }
        };

        let mut alternatives = Vec::new();

        for src_issue in &src_issues {
            let mut finder = Pathfinder::new(
                ledger,
                self.source,
                self.destination,
                src_issue.clone(),
                dst_issue.clone(),
                &mut line_cache,
                &book_index,
            );

            let paths = finder.find_paths();
            if paths.is_empty() {
                continue;
            }

            let requested = parse_destination_amount(&self.destination_amount);
            let mut ranks = compute_path_ranks(
                &paths,
                ledger,
                &mut line_cache,
                src_issue,
                &dst_issue,
                &self.source,
                &self.destination,
                &requested,
            );
            let best = get_best_paths(&paths, &mut ranks, MAX_ALTERNATIVES);

            if !best.is_empty() {
                let source_amount = build_source_amount(src_issue, &self.destination_amount);
                alternatives.push(PathAlternative {
                    source_amount,
                    paths_computed: best,
                });
            }

            if alternatives.len() >= MAX_ALTERNATIVES {
                break;
            }
        }

        alternatives
    }
}

/// Parse a JSON destination amount into an IOUAmount for simulation.
fn parse_destination_amount(amount: &serde_json::Value) -> IOUAmount {
    if let Some(drops_str) = amount.as_str() {
        if let Ok(drops) = drops_str.parse::<i64>() {
            if drops != 0 {
                return IOUAmount::new(drops, 0).unwrap_or(IOUAmount::ZERO);
            }
        }
        return IOUAmount::ZERO;
    }

    let value_str = amount.get("value").and_then(|v| v.as_str()).unwrap_or("0");

    let value: f64 = value_str.parse().unwrap_or(0.0);
    if value == 0.0 || !value.is_finite() {
        return IOUAmount::ZERO;
    }

    let negative = value < 0.0;
    let abs_val = value.abs();
    let mut exponent = 0i32;
    let mut mantissa = abs_val;

    while mantissa < 1e15 && exponent > -96 {
        mantissa *= 10.0;
        exponent -= 1;
    }
    while mantissa >= 1e16 && exponent < 80 {
        mantissa /= 10.0;
        exponent += 1;
    }

    IOUAmount::from_parts(mantissa as u64, exponent, negative).unwrap_or(IOUAmount::ZERO)
}

/// Parse a JSON amount value into an Issue.
pub fn parse_amount_issue(amount: &serde_json::Value) -> Option<Issue> {
    if amount.is_string() {
        return Some(Issue::xrp());
    }

    let currency_str = amount.get("currency").and_then(|v| v.as_str())?;
    let issuer_str = amount.get("issuer").and_then(|v| v.as_str())?;

    let mut currency = [0u8; 20];
    if currency_str == "XRP" {
        return Some(Issue::xrp());
    } else if currency_str.len() == 3 {
        currency[12] = currency_str.as_bytes()[0];
        currency[13] = currency_str.as_bytes()[1];
        currency[14] = currency_str.as_bytes()[2];
    } else if currency_str.len() == 40 {
        let decoded = hex::decode(currency_str).ok()?;
        currency.copy_from_slice(&decoded);
    }

    let issuer = rxrpl_codec::address::classic::decode_account_id(issuer_str).ok()?;
    Some(Issue { currency, issuer })
}

/// Parse a `source_currencies` entry. Unlike `destination_amount`, the issuer
/// is optional; a currency-only entry is a wildcard over any issuer.
/// When the issuer is omitted the returned Issue has an empty AccountId
/// (`[0u8; 20]`), which the request layer treats as "match any issuer for this
/// currency".
pub fn parse_source_currency(amount: &serde_json::Value) -> Option<Issue> {
    if amount.is_string() {
        return Some(Issue::xrp());
    }

    let currency_str = amount.get("currency").and_then(|v| v.as_str())?;

    if currency_str == "XRP" {
        return Some(Issue::xrp());
    }

    let mut currency = [0u8; 20];
    if currency_str.len() == 3 {
        currency[12] = currency_str.as_bytes()[0];
        currency[13] = currency_str.as_bytes()[1];
        currency[14] = currency_str.as_bytes()[2];
    } else if currency_str.len() == 40 {
        let decoded = hex::decode(currency_str).ok()?;
        currency.copy_from_slice(&decoded);
    }

    let issuer = match amount.get("issuer").and_then(|v| v.as_str()) {
        Some(s) => rxrpl_codec::address::classic::decode_account_id(s).ok()?,
        None => rxrpl_primitives::AccountId([0u8; 20]),
    };

    Some(Issue { currency, issuer })
}

/// Build a source_amount JSON value from an issue.
fn build_source_amount(issue: &Issue, dst_amount: &serde_json::Value) -> serde_json::Value {
    if issue.is_xrp() {
        // For XRP, use the destination amount value as estimate
        if let Some(drops) = dst_amount.as_str() {
            return serde_json::json!(drops);
        }
        return serde_json::json!("-1");
    }

    let currency_str = format_currency(&issue.currency);
    let issuer_str = rxrpl_codec::address::classic::encode_account_id(&issue.issuer);

    let value = dst_amount
        .get("value")
        .and_then(|v| v.as_str())
        .unwrap_or("-1");

    serde_json::json!({
        "currency": currency_str,
        "issuer": issuer_str,
        "value": value,
    })
}

/// Format a 20-byte currency code as a string.
fn format_currency(currency: &[u8; 20]) -> String {
    if *currency == [0u8; 20] {
        return "XRP".to_string();
    }
    // Check for standard 3-char code at offset 12
    if currency[..12] == [0u8; 12] && currency[15..] == [0u8; 5] {
        let chars = &currency[12..15];
        if chars.iter().all(|c| c.is_ascii_graphic()) {
            return String::from_utf8_lossy(chars).to_string();
        }
    }
    hex::encode(currency)
}

/// Convert a path step to its JSON representation.
pub fn path_step_to_json(step: &PathStep) -> serde_json::Value {
    let mut obj = serde_json::Map::new();

    if let Some(ref account) = step.account {
        if (step.step_type & PATH_STEP_ACCOUNT) != 0 {
            obj.insert(
                "account".to_string(),
                serde_json::Value::String(rxrpl_codec::address::classic::encode_account_id(
                    account,
                )),
            );
        }
    }

    if let Some(ref currency) = step.currency {
        if (step.step_type & PATH_STEP_CURRENCY) != 0 {
            obj.insert(
                "currency".to_string(),
                serde_json::Value::String(format_currency(currency)),
            );
        }
    }

    if let Some(ref issuer) = step.issuer {
        if (step.step_type & PATH_STEP_ISSUER) != 0 {
            obj.insert(
                "issuer".to_string(),
                serde_json::Value::String(rxrpl_codec::address::classic::encode_account_id(issuer)),
            );
        }
    }

    obj.insert(
        "type".to_string(),
        serde_json::Value::Number(step.step_type.into()),
    );

    serde_json::Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_xrp_amount() {
        let amount = serde_json::json!("1000000");
        let issue = parse_amount_issue(&amount).unwrap();
        assert!(issue.is_xrp());
    }

    #[test]
    fn parse_iou_amount() {
        let amount = serde_json::json!({
            "currency": "USD",
            "issuer": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "value": "100",
        });
        let issue = parse_amount_issue(&amount).unwrap();
        assert!(!issue.is_xrp());
    }

    #[test]
    fn format_standard_currency() {
        let mut c = [0u8; 20];
        c[12..15].copy_from_slice(b"USD");
        assert_eq!(format_currency(&c), "USD");
    }

    #[test]
    fn format_xrp_currency() {
        assert_eq!(format_currency(&[0u8; 20]), "XRP");
    }
}
