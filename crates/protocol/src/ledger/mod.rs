pub mod account_root;
pub mod amendments;
pub mod amm;
pub mod bridge;
pub mod check;
pub mod common;
pub mod credential;
pub mod delegate;
pub mod deposit_preauth;
pub mod did;
pub mod directory_node;
pub mod escrow;
pub mod fee_settings;
pub mod hook;
pub mod hook_state;
pub mod ledger_hashes;
pub mod mptoken;
pub mod mptoken_issuance;
pub mod negative_unl;
pub mod nftoken_offer;
pub mod nftoken_page;
pub mod offer;
pub mod oracle;
pub mod pay_channel;
pub mod permissioned_domain;
pub mod ripple_state;
pub mod signer_list;
pub mod ticket;
pub mod vault;
pub mod xchain_owned_claim_id;
pub mod xchain_owned_create_account_claim_id;

pub use account_root::AccountRoot;
pub use amendments::Amendments;
pub use amm::Amm;
pub use bridge::Bridge;
pub use check::Check;
pub use common::{CommonLedgerFields, LedgerObject};
pub use credential::Credential;
pub use delegate::Delegate;
pub use deposit_preauth::DepositPreauth;
pub use did::Did;
pub use directory_node::DirectoryNode;
pub use escrow::Escrow;
pub use fee_settings::FeeSettings;
pub use hook::HookDefinition;
pub use hook_state::HookState;
pub use ledger_hashes::LedgerHashes;
pub use mptoken::MpToken;
pub use mptoken_issuance::MpTokenIssuance;
pub use negative_unl::NegativeUnl;
pub use nftoken_offer::NFTokenOffer;
pub use nftoken_page::NFTokenPage;
pub use offer::Offer;
pub use oracle::Oracle;
pub use pay_channel::PayChannel;
pub use permissioned_domain::PermissionedDomain;
pub use ripple_state::RippleState;
pub use signer_list::SignerList;
pub use ticket::Ticket;
pub use vault::Vault;
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
    Vault(Vault),
    MPTokenIssuance(MpTokenIssuance),
    MPToken(MpToken),
    Credential(Credential),
    PermissionedDomain(PermissionedDomain),
    Delegate(Delegate),
    NegativeUNL(NegativeUnl),
    Bridge(Bridge),
    XChainOwnedClaimId(XChainOwnedClaimId),
    XChainOwnedCreateAccountClaimId(XChainOwnedCreateAccountClaimId),
    HookDefinition(HookDefinition),
    HookState(HookState),
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
