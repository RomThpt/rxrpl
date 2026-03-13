pub mod common;
pub mod account_root;
pub mod directory_node;
pub mod ripple_state;
pub mod offer;
pub mod signer_list;
pub mod ticket;
pub mod fee_settings;
pub mod amendments;
pub mod ledger_hashes;
pub mod escrow;
pub mod pay_channel;
pub mod check;
pub mod deposit_preauth;
pub mod nftoken_page;
pub mod nftoken_offer;
pub mod amm;
pub mod did;
pub mod oracle;
pub mod mptoken_issuance;
pub mod mptoken;
pub mod credential;
pub mod bridge;
pub mod xchain_owned_claim_id;
pub mod xchain_owned_create_account_claim_id;

pub use common::{CommonLedgerFields, LedgerObject};
pub use account_root::AccountRoot;
pub use directory_node::DirectoryNode;
pub use ripple_state::RippleState;
pub use offer::Offer;
pub use signer_list::SignerList;
pub use ticket::Ticket;
pub use fee_settings::FeeSettings;
pub use amendments::Amendments;
pub use ledger_hashes::LedgerHashes;
pub use escrow::Escrow;
pub use pay_channel::PayChannel;
pub use check::Check;
pub use deposit_preauth::DepositPreauth;
pub use nftoken_page::NFTokenPage;
pub use nftoken_offer::NFTokenOffer;
pub use amm::Amm;
pub use did::Did;
pub use oracle::Oracle;
pub use mptoken_issuance::MpTokenIssuance;
pub use mptoken::MpToken;
pub use credential::Credential;
pub use bridge::Bridge;
pub use xchain_owned_claim_id::XChainOwnedClaimId;
pub use xchain_owned_create_account_claim_id::XChainOwnedCreateAccountClaimId;

use serde_json::Value;

/// Polymorphic ledger object enum for deserializing any ledger entry type.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "LedgerEntryType")]
pub enum LedgerObjectKind {
    AccountRoot(AccountRoot),
    DirectoryNode(DirectoryNode),
    RippleState(RippleState),
    Offer(Offer),
    SignerList(SignerList),
    Ticket(Ticket),
    FeeSettings(FeeSettings),
    Amendments(Amendments),
    LedgerHashes(LedgerHashes),
    Escrow(Escrow),
    PayChannel(PayChannel),
    Check(Check),
    DepositPreauth(DepositPreauth),
    NFTokenPage(NFTokenPage),
    NFTokenOffer(NFTokenOffer),
    AMM(Amm),
    DID(Did),
    Oracle(Oracle),
    MPTokenIssuance(MpTokenIssuance),
    MPToken(MpToken),
    Credential(Credential),
    Bridge(Bridge),
    XChainOwnedClaimId(XChainOwnedClaimId),
    XChainOwnedCreateAccountClaimId(XChainOwnedCreateAccountClaimId),
    #[serde(other, deserialize_with = "deserialize_unknown")]
    Unknown,
}

fn deserialize_unknown<'de, D>(_deserializer: D) -> Result<(), D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(())
}

impl LedgerObjectKind {
    pub fn from_json(value: &Value) -> Result<Self, crate::ProtocolError> {
        serde_json::from_value(value.clone()).map_err(crate::ProtocolError::Json)
    }
}
