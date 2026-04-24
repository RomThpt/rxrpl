use std::collections::HashSet;

use rxrpl_primitives::AccountId;
use serde_json::Value;

use crate::error::RpcServerError;
use crate::events::ServerEvent;

/// Parameters for a persistent path_find subscription.
///
/// rippled allows at most one active path_find per WebSocket connection.
/// On each new validated ledger the pathfinding algorithm is re-run and,
/// if the result differs from the previous one, an update is pushed.
#[derive(Clone, Debug)]
pub struct PathFindSubscription {
    pub source: AccountId,
    pub destination: AccountId,
    pub destination_amount: Value,
    pub source_currencies: Option<Vec<rxrpl_pathfind::Issue>>,
    /// Serialized JSON of the last alternatives sent to the client.
    /// Used to suppress duplicate updates.
    pub last_result: Option<String>,
    /// Original string representations for response fields.
    pub source_account_str: String,
    pub destination_account_str: String,
}

/// Key identifying an order book by its trading pair.
#[derive(Clone, Debug)]
struct OrderBookKey {
    taker_pays: Value,
    taker_gets: Value,
    canonical: String,
}

impl OrderBookKey {
    fn new(taker_pays: Value, taker_gets: Value) -> Self {
        let canonical = format!(
            "{}|{}",
            serde_json::to_string(&taker_pays).unwrap_or_default(),
            serde_json::to_string(&taker_gets).unwrap_or_default(),
        );
        Self {
            taker_pays,
            taker_gets,
            canonical,
        }
    }

    fn matches_event(&self, event_pays: &Value, event_gets: &Value) -> bool {
        self.taker_pays == *event_pays && self.taker_gets == *event_gets
    }
}

impl PartialEq for OrderBookKey {
    fn eq(&self, other: &Self) -> bool {
        self.canonical == other.canonical
    }
}

impl Eq for OrderBookKey {}

impl std::hash::Hash for OrderBookKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.canonical.hash(state);
    }
}

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
    OrderBook,
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
            "order_book" => Some(Self::OrderBook),
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
    order_books: HashSet<OrderBookKey>,
    /// At most one active path_find subscription per connection (matching rippled).
    path_find: Option<PathFindSubscription>,
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

        if let Some(books) = params.get("books").and_then(|v| v.as_array()) {
            for book in books {
                let taker_pays = book.get("taker_pays").cloned().unwrap_or(Value::Null);
                let taker_gets = book.get("taker_gets").cloned().unwrap_or(Value::Null);
                if taker_pays.is_null() || taker_gets.is_null() {
                    return Err(RpcServerError::InvalidParams(
                        "books entry requires taker_pays and taker_gets".into(),
                    ));
                }
                self.order_books.insert(OrderBookKey::new(taker_pays, taker_gets));
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

        if let Some(books) = params.get("books").and_then(|v| v.as_array()) {
            for book in books {
                let taker_pays = book.get("taker_pays").cloned().unwrap_or(Value::Null);
                let taker_gets = book.get("taker_gets").cloned().unwrap_or(Value::Null);
                self.order_books.remove(&OrderBookKey::new(taker_pays, taker_gets));
            }
        }

        Ok(serde_json::json!({}))
    }

    /// Register a persistent path_find subscription.
    ///
    /// Returns an error if a subscription is already active (rippled limit: 1).
    pub fn create_path_find(
        &mut self,
        sub: PathFindSubscription,
    ) -> Result<(), RpcServerError> {
        if self.path_find.is_some() {
            return Err(RpcServerError::InvalidParams(
                "only one path_find subscription allowed per connection".into(),
            ));
        }
        self.path_find = Some(sub);
        Ok(())
    }

    /// Remove the active path_find subscription.
    pub fn close_path_find(&mut self) -> bool {
        self.path_find.take().is_some()
    }

    /// Return a reference to the active path_find subscription, if any.
    pub fn path_find_subscription(&self) -> Option<&PathFindSubscription> {
        self.path_find.as_ref()
    }

    /// Return a mutable reference to the active path_find subscription.
    pub fn path_find_subscription_mut(&mut self) -> Option<&mut PathFindSubscription> {
        self.path_find.as_mut()
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
            ServerEvent::OrderBookUpdate {
                taker_pays,
                taker_gets,
                ..
            } => {
                self.streams.contains(&StreamType::OrderBook)
                    || self
                        .order_books
                        .iter()
                        .any(|key| key.matches_event(taker_pays, taker_gets))
            }
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
        .map_err(|_| RpcServerError::AccountMalformed)
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
    fn subscribe_order_book_matches_event() {
        let mut subs = ConnectionSubscriptions::new();
        let params = serde_json::json!({
            "books": [{"taker_pays": {"currency": "XRP"}, "taker_gets": {"currency": "USD", "issuer": "rIssuer"}}]
        });
        subs.apply_subscribe(&params).unwrap();

        let event = ServerEvent::OrderBookUpdate {
            taker_pays: serde_json::json!({"currency": "XRP"}),
            taker_gets: serde_json::json!({"currency": "USD", "issuer": "rIssuer"}),
            offers: vec![],
        };
        assert!(subs.matches(&event));
    }

    #[test]
    fn order_book_no_match_different_pair() {
        let mut subs = ConnectionSubscriptions::new();
        let params = serde_json::json!({
            "books": [{"taker_pays": {"currency": "XRP"}, "taker_gets": {"currency": "USD", "issuer": "rIssuer"}}]
        });
        subs.apply_subscribe(&params).unwrap();

        let event = ServerEvent::OrderBookUpdate {
            taker_pays: serde_json::json!({"currency": "XRP"}),
            taker_gets: serde_json::json!({"currency": "EUR", "issuer": "rOther"}),
            offers: vec![],
        };
        assert!(!subs.matches(&event));
    }

    #[test]
    fn unsubscribe_order_book_removes_pair() {
        let mut subs = ConnectionSubscriptions::new();
        let book = serde_json::json!({"taker_pays": {"currency": "XRP"}, "taker_gets": {"currency": "USD", "issuer": "rIssuer"}});
        subs.apply_subscribe(&serde_json::json!({"books": [book.clone()]}))
            .unwrap();
        subs.apply_unsubscribe(&serde_json::json!({"books": [book]}))
            .unwrap();

        let event = ServerEvent::OrderBookUpdate {
            taker_pays: serde_json::json!({"currency": "XRP"}),
            taker_gets: serde_json::json!({"currency": "USD", "issuer": "rIssuer"}),
            offers: vec![],
        };
        assert!(!subs.matches(&event));
    }

    #[test]
    fn order_book_missing_fields_rejected() {
        let mut subs = ConnectionSubscriptions::new();
        let result = subs.apply_subscribe(&serde_json::json!({"books": [{"taker_pays": {"currency": "XRP"}}]}));
        assert!(result.is_err());
    }

    #[test]
    fn subscribe_order_book_stream_matches_all_updates() {
        let mut subs = ConnectionSubscriptions::new();
        subs.apply_subscribe(&serde_json::json!({"streams": ["order_book"]}))
            .unwrap();

        // Should match ANY OrderBookUpdate regardless of pair
        let event = ServerEvent::OrderBookUpdate {
            taker_pays: serde_json::json!({"currency": "XRP"}),
            taker_gets: serde_json::json!({"currency": "EUR", "issuer": "rAny"}),
            offers: vec![],
        };
        assert!(subs.matches(&event));

        // Also a different pair
        let event2 = ServerEvent::OrderBookUpdate {
            taker_pays: serde_json::json!({"currency": "BTC", "issuer": "rBTC"}),
            taker_gets: serde_json::json!({"currency": "USD", "issuer": "rUSD"}),
            offers: vec![],
        };
        assert!(subs.matches(&event2));
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

    fn make_test_path_find_sub() -> PathFindSubscription {
        PathFindSubscription {
            source: AccountId([1u8; 20]),
            destination: AccountId([2u8; 20]),
            destination_amount: serde_json::json!("1000000"),
            source_currencies: None,
            last_result: None,
            source_account_str: "rSource".into(),
            destination_account_str: "rDest".into(),
        }
    }

    #[test]
    fn create_path_find_subscription() {
        let mut subs = ConnectionSubscriptions::new();
        assert!(subs.path_find_subscription().is_none());

        subs.create_path_find(make_test_path_find_sub()).unwrap();
        assert!(subs.path_find_subscription().is_some());
    }

    #[test]
    fn only_one_path_find_allowed() {
        let mut subs = ConnectionSubscriptions::new();
        subs.create_path_find(make_test_path_find_sub()).unwrap();

        let result = subs.create_path_find(make_test_path_find_sub());
        assert!(result.is_err());
    }

    #[test]
    fn close_path_find_subscription() {
        let mut subs = ConnectionSubscriptions::new();
        subs.create_path_find(make_test_path_find_sub()).unwrap();

        assert!(subs.close_path_find());
        assert!(subs.path_find_subscription().is_none());
    }

    #[test]
    fn close_path_find_when_none_active() {
        let mut subs = ConnectionSubscriptions::new();
        assert!(!subs.close_path_find());
    }

    #[test]
    fn path_find_subscription_mutable_access() {
        let mut subs = ConnectionSubscriptions::new();
        subs.create_path_find(make_test_path_find_sub()).unwrap();

        let sub = subs.path_find_subscription_mut().unwrap();
        sub.last_result = Some("test".into());

        assert_eq!(
            subs.path_find_subscription().unwrap().last_result.as_deref(),
            Some("test")
        );
    }

    #[test]
    fn create_after_close_succeeds() {
        let mut subs = ConnectionSubscriptions::new();
        subs.create_path_find(make_test_path_find_sub()).unwrap();
        subs.close_path_find();
        // Should be able to create a new one after closing
        subs.create_path_find(make_test_path_find_sub()).unwrap();
        assert!(subs.path_find_subscription().is_some());
    }
}
