use std::collections::HashSet;

use rxrpl_primitives::AccountId;
use serde_json::Value;

use crate::error::RpcServerError;
use crate::events::ServerEvent;

/// Types of subscription streams.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum StreamType {
    Ledger,
    Transactions,
    TransactionsProposed,
    Validations,
    Manifests,
    PeerStatus,
    Server,
    Consensus,
    BookChanges,
    PathFind,
}

impl StreamType {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "ledger" => Some(Self::Ledger),
            "transactions" => Some(Self::Transactions),
            "transactions_proposed" => Some(Self::TransactionsProposed),
            "validations" => Some(Self::Validations),
            "manifests" => Some(Self::Manifests),
            "peer_status" => Some(Self::PeerStatus),
            "server" => Some(Self::Server),
            "consensus" => Some(Self::Consensus),
            "book_changes" => Some(Self::BookChanges),
            "path_find" => Some(Self::PathFind),
            _ => None,
        }
    }
}

/// Per-connection subscription state.
#[derive(Default, Debug)]
pub struct ConnectionSubscriptions {
    streams: HashSet<StreamType>,
    accounts: HashSet<AccountId>,
    accounts_proposed: HashSet<AccountId>,
}

impl ConnectionSubscriptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a `subscribe` command. Returns the response value.
    pub fn apply_subscribe(&mut self, params: &Value) -> Result<Value, RpcServerError> {
        if let Some(streams) = params.get("streams").and_then(|v| v.as_array()) {
            for s in streams {
                if let Some(name) = s.as_str() {
                    if let Some(st) = StreamType::from_str(name) {
                        self.streams.insert(st);
                    } else {
                        return Err(RpcServerError::InvalidParams(format!(
                            "unknown stream: {name}"
                        )));
                    }
                }
            }
        }

        if let Some(accounts) = params.get("accounts").and_then(|v| v.as_array()) {
            for a in accounts {
                if let Some(addr) = a.as_str() {
                    let id = parse_account(addr)?;
                    self.accounts.insert(id);
                }
            }
        }

        if let Some(accounts) = params.get("accounts_proposed").and_then(|v| v.as_array()) {
            for a in accounts {
                if let Some(addr) = a.as_str() {
                    let id = parse_account(addr)?;
                    self.accounts_proposed.insert(id);
                }
            }
        }

        Ok(serde_json::json!({}))
    }

    /// Apply an `unsubscribe` command. Returns the response value.
    pub fn apply_unsubscribe(&mut self, params: &Value) -> Result<Value, RpcServerError> {
        if let Some(streams) = params.get("streams").and_then(|v| v.as_array()) {
            for s in streams {
                if let Some(name) = s.as_str() {
                    if let Some(st) = StreamType::from_str(name) {
                        self.streams.remove(&st);
                    }
                }
            }
        }

        if let Some(accounts) = params.get("accounts").and_then(|v| v.as_array()) {
            for a in accounts {
                if let Some(addr) = a.as_str() {
                    if let Ok(id) = parse_account(addr) {
                        self.accounts.remove(&id);
                    }
                }
            }
        }

        if let Some(accounts) = params.get("accounts_proposed").and_then(|v| v.as_array()) {
            for a in accounts {
                if let Some(addr) = a.as_str() {
                    if let Ok(id) = parse_account(addr) {
                        self.accounts_proposed.remove(&id);
                    }
                }
            }
        }

        Ok(serde_json::json!({}))
    }

    /// Check if this connection is interested in the given event.
    pub fn matches(&self, event: &ServerEvent) -> bool {
        match event {
            ServerEvent::LedgerClosed { .. } => self.streams.contains(&StreamType::Ledger),
            ServerEvent::TransactionValidated { transaction, .. } => {
                if self.streams.contains(&StreamType::Transactions) {
                    return true;
                }
                if !self.accounts.is_empty() {
                    return tx_matches_accounts(transaction, &self.accounts);
                }
                false
            }
            ServerEvent::TransactionProposed { transaction, .. } => {
                if self.streams.contains(&StreamType::TransactionsProposed) {
                    return true;
                }
                if !self.accounts_proposed.is_empty() {
                    return tx_matches_accounts(transaction, &self.accounts_proposed);
                }
                false
            }
            ServerEvent::ValidationReceived { .. } => {
                self.streams.contains(&StreamType::Validations)
            }
            ServerEvent::ManifestReceived { .. } => self.streams.contains(&StreamType::Manifests),
            ServerEvent::PeerStatusChange { .. } => self.streams.contains(&StreamType::PeerStatus),
            ServerEvent::ServerStateChange { .. } => self.streams.contains(&StreamType::Server),
            ServerEvent::ConsensusPhaseChange { .. } => {
                self.streams.contains(&StreamType::Consensus)
            }
            ServerEvent::BookChange { .. } => self.streams.contains(&StreamType::BookChanges),
            ServerEvent::PathFindUpdate { .. } => self.streams.contains(&StreamType::PathFind),
        }
    }
}

/// Check if a transaction's Account or Destination matches any of the given accounts.
fn tx_matches_accounts(tx: &Value, accounts: &HashSet<AccountId>) -> bool {
    for field in &["Account", "Destination"] {
        if let Some(addr) = tx.get(field).and_then(|v| v.as_str()) {
            if let Ok(id) = parse_account(addr) {
                if accounts.contains(&id) {
                    return true;
                }
            }
        }
    }
    false
}

fn parse_account(addr: &str) -> Result<AccountId, RpcServerError> {
    rxrpl_codec::address::classic::decode_account_id(addr)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid account: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_ledger_stream() {
        let mut subs = ConnectionSubscriptions::new();
        let params = serde_json::json!({"streams": ["ledger"]});
        subs.apply_subscribe(&params).unwrap();

        let event = ServerEvent::LedgerClosed {
            ledger_index: 1,
            ledger_hash: rxrpl_primitives::Hash256::default(),
            ledger_time: 0,
            txn_count: 0,
        };
        assert!(subs.matches(&event));
    }

    #[test]
    fn subscribe_transactions_stream() {
        let mut subs = ConnectionSubscriptions::new();
        let params = serde_json::json!({"streams": ["transactions"]});
        subs.apply_subscribe(&params).unwrap();

        let event = ServerEvent::TransactionValidated {
            transaction: serde_json::json!({"Account": "rTest"}),
            meta: serde_json::json!({}),
            ledger_index: 1,
        };
        assert!(subs.matches(&event));
    }

    #[test]
    fn unsubscribe_removes_stream() {
        let mut subs = ConnectionSubscriptions::new();
        subs.apply_subscribe(&serde_json::json!({"streams": ["ledger"]}))
            .unwrap();
        subs.apply_unsubscribe(&serde_json::json!({"streams": ["ledger"]}))
            .unwrap();

        let event = ServerEvent::LedgerClosed {
            ledger_index: 1,
            ledger_hash: rxrpl_primitives::Hash256::default(),
            ledger_time: 0,
            txn_count: 0,
        };
        assert!(!subs.matches(&event));
    }

    #[test]
    fn unknown_stream_rejected() {
        let mut subs = ConnectionSubscriptions::new();
        let result = subs.apply_subscribe(&serde_json::json!({"streams": ["bogus"]}));
        assert!(result.is_err());
    }

    #[test]
    fn no_match_without_subscription() {
        let subs = ConnectionSubscriptions::new();
        let event = ServerEvent::LedgerClosed {
            ledger_index: 1,
            ledger_hash: rxrpl_primitives::Hash256::default(),
            ledger_time: 0,
            txn_count: 0,
        };
        assert!(!subs.matches(&event));
    }

    #[test]
    fn proposed_stream_matches_proposed_events() {
        let mut subs = ConnectionSubscriptions::new();
        subs.apply_subscribe(&serde_json::json!({"streams": ["transactions_proposed"]}))
            .unwrap();

        let event = ServerEvent::TransactionProposed {
            transaction: serde_json::json!({"Account": "rTest"}),
            engine_result: "tesSUCCESS".into(),
            engine_result_code: 0,
        };
        assert!(subs.matches(&event));

        // Should NOT match validated events
        let validated = ServerEvent::TransactionValidated {
            transaction: serde_json::json!({"Account": "rTest"}),
            meta: serde_json::json!({}),
            ledger_index: 1,
        };
        assert!(!subs.matches(&validated));
    }
}
