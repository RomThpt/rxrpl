pub mod common;
pub mod macros;
pub mod signer;

#[cfg(feature = "client")]
pub mod autofill;

pub mod payment;
pub mod account_set;
pub mod trust_set;
pub mod offer_create;
pub mod offer_cancel;
pub mod set_regular_key;
pub mod signer_list_set;
pub mod account_delete;
pub mod ticket_create;
pub mod deposit_preauth;
pub mod escrow_create;
pub mod escrow_finish;
pub mod escrow_cancel;
pub mod check_create;
pub mod check_cash;
pub mod check_cancel;
pub mod payment_channel_create;
pub mod payment_channel_fund;
pub mod payment_channel_claim;
pub mod nftoken_mint;
pub mod nftoken_burn;
pub mod nftoken_create_offer;
pub mod nftoken_cancel_offer;
pub mod nftoken_accept_offer;
pub mod clawback;

pub use common::{CommonFields, Memo, Signer, Transaction};
pub use signer::{
    combine_multisig, compute_tx_hash, serialize_signed, sign, sign_for, verify_multisig,
    verify_signature,
};

pub use payment::Payment;
pub use account_set::AccountSet;
pub use trust_set::TrustSet;
pub use offer_create::OfferCreate;
pub use offer_cancel::OfferCancel;
pub use set_regular_key::SetRegularKey;
pub use signer_list_set::SignerListSet;
pub use account_delete::AccountDelete;
pub use ticket_create::TicketCreate;
pub use deposit_preauth::DepositPreauth;
pub use escrow_create::EscrowCreate;
pub use escrow_finish::EscrowFinish;
pub use escrow_cancel::EscrowCancel;
pub use check_create::CheckCreate;
pub use check_cash::CheckCash;
pub use check_cancel::CheckCancel;
pub use payment_channel_create::PaymentChannelCreate;
pub use payment_channel_fund::PaymentChannelFund;
pub use payment_channel_claim::PaymentChannelClaim;
pub use nftoken_mint::NFTokenMint;
pub use nftoken_burn::NFTokenBurn;
pub use nftoken_create_offer::NFTokenCreateOffer;
pub use nftoken_cancel_offer::NFTokenCancelOffer;
pub use nftoken_accept_offer::NFTokenAcceptOffer;
pub use clawback::Clawback;

use serde_json::Value;

/// Polymorphic transaction enum for deserializing any transaction type.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "TransactionType")]
pub enum TransactionKind {
    Payment(Payment),
    AccountSet(AccountSet),
    TrustSet(TrustSet),
    OfferCreate(OfferCreate),
    OfferCancel(OfferCancel),
    SetRegularKey(SetRegularKey),
    SignerListSet(SignerListSet),
    AccountDelete(AccountDelete),
    TicketCreate(TicketCreate),
    DepositPreauth(DepositPreauth),
    EscrowCreate(EscrowCreate),
    EscrowFinish(EscrowFinish),
    EscrowCancel(EscrowCancel),
    CheckCreate(CheckCreate),
    CheckCash(CheckCash),
    CheckCancel(CheckCancel),
    PaymentChannelCreate(PaymentChannelCreate),
    PaymentChannelFund(PaymentChannelFund),
    PaymentChannelClaim(PaymentChannelClaim),
    NFTokenMint(NFTokenMint),
    NFTokenBurn(NFTokenBurn),
    NFTokenCreateOffer(NFTokenCreateOffer),
    NFTokenCancelOffer(NFTokenCancelOffer),
    NFTokenAcceptOffer(NFTokenAcceptOffer),
    Clawback(Clawback),
    /// Forward-compatible fallback for unknown transaction types.
    #[serde(other, deserialize_with = "deserialize_unknown")]
    Unknown,
}

fn deserialize_unknown<'de, D>(_deserializer: D) -> Result<(), D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(())
}

impl TransactionKind {
    /// Deserialize from a JSON value, falling back to Unknown for unrecognized types.
    pub fn from_json(value: &Value) -> Result<Self, crate::ProtocolError> {
        serde_json::from_value(value.clone()).map_err(crate::ProtocolError::Json)
    }
}
