use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An XChainAddAccountCreateAttestation transaction adds an attestation
    /// to a cross-chain account creation.
    XChainAddAccountCreateAttestation => TransactionType::XChainAddAccountCreateAttestation,
    {
        "XChainBridge" => xchain_bridge: serde_json::Value,
        "OtherChainSource" => other_chain_source: String,
        "Destination" => destination: String,
        "Amount" => amount: serde_json::Value,
        "SignatureReward" => signature_reward: serde_json::Value,
        "AttestationRewardAccount" => attestation_reward_account: String,
        "AttestationSignerAccount" => attestation_signer_account: String,
        "PublicKey" => public_key: String,
        "Signature" => signature: String,
        "WasLockingChainSend" => was_locking_chain_send: u8,
        "XChainAccountCreateCount" => xchain_account_create_count: String
    }
}
