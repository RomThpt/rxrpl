use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A TicketCreate transaction sets aside one or more sequence numbers as Tickets.
    TicketCreate => TransactionType::TicketCreate,
    {
        "TicketCount" => ticket_count: u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "TicketCreate",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "TicketCount": 5
        });
        let tx = TicketCreate::from_json(&json).unwrap();
        assert_eq!(tx.ticket_count, 5);
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TicketCount"], 5);
    }
}
