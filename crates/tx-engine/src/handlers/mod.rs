pub mod account_set;
pub mod offer_cancel;
pub mod offer_create;
pub mod payment;
pub mod set_regular_key;
pub mod signer_list_set;
pub mod ticket_create;
pub mod trust_set;

use rxrpl_protocol::TransactionType;

use crate::registry::TransactorRegistry;

/// Register all implemented Phase A transaction handlers.
pub fn register_phase_a(registry: &mut TransactorRegistry) {
    registry.register(TransactionType::Payment, payment::PaymentTransactor);
    registry.register(TransactionType::AccountSet, account_set::AccountSetTransactor);
    registry.register(
        TransactionType::SetRegularKey,
        set_regular_key::SetRegularKeyTransactor,
    );
    registry.register(TransactionType::TrustSet, trust_set::TrustSetTransactor);
    registry.register(
        TransactionType::OfferCreate,
        offer_create::OfferCreateTransactor,
    );
    registry.register(
        TransactionType::OfferCancel,
        offer_cancel::OfferCancelTransactor,
    );
    registry.register(
        TransactionType::TicketCreate,
        ticket_create::TicketCreateTransactor,
    );
    registry.register(
        TransactionType::SignerListSet,
        signer_list_set::SignerListSetTransactor,
    );
}
