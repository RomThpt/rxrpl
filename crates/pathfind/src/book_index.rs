use std::collections::{HashMap, HashSet};

use rxrpl_ledger::Ledger;

use crate::types::Issue;

/// Index mapping each Issue to the set of Issues it can be exchanged for
/// via the order book.
pub struct BookIndex {
    books: HashMap<Issue, HashSet<Issue>>,
}

impl BookIndex {
    /// Build a book index by scanning the ledger for Offer entries.
    pub fn build(ledger: &Ledger) -> Self {
        let mut books: HashMap<Issue, HashSet<Issue>> = HashMap::new();

        // Iterate all ledger entries looking for Offers
        for (_key, data) in ledger.state_map.iter() {
            let entry: serde_json::Value = match serde_json::from_slice(&data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if entry.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
                continue;
            }

            let taker_pays = match entry.get("TakerPays") {
                Some(v) => v,
                None => continue,
            };
            let taker_gets = match entry.get("TakerGets") {
                Some(v) => v,
                None => continue,
            };

            let pays_issue = match parse_issue(taker_pays) {
                Some(i) => i,
                None => continue,
            };
            let gets_issue = match parse_issue(taker_gets) {
                Some(i) => i,
                None => continue,
            };

            books
                .entry(pays_issue.clone())
                .or_default()
                .insert(gets_issue.clone());
            books.entry(gets_issue).or_default().insert(pays_issue);
        }

        Self { books }
    }

    /// Get all issues that can be obtained by selling `issue`.
    pub fn get_books_for(&self, issue: &Issue) -> Vec<Issue> {
        self.books
            .get(issue)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// Parse a JSON amount into an Issue.
fn parse_issue(amount: &serde_json::Value) -> Option<Issue> {
    if let Some(_drops) = amount.as_str() {
        return Some(Issue::xrp());
    }

    let currency_str = amount.get("currency").and_then(|v| v.as_str())?;
    let issuer_str = amount.get("issuer").and_then(|v| v.as_str())?;

    let mut currency = [0u8; 20];
    if currency_str.len() == 3 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_xrp_issue() {
        let amount = serde_json::json!("1000000");
        let issue = parse_issue(&amount).unwrap();
        assert!(issue.is_xrp());
    }

    #[test]
    fn parse_iou_issue() {
        let amount = serde_json::json!({
            "currency": "USD",
            "issuer": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "value": "100",
        });
        let issue = parse_issue(&amount).unwrap();
        assert!(!issue.is_xrp());
        assert_eq!(&issue.currency[12..15], b"USD");
    }
}
