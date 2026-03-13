use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An XChainAddClaimAttestation transaction adds an attestation to a
    /// cross-chain claim.
    XChainAddClaimAttestation => TransactionType::XChainAddClaimAttestation,
    {
        "XChainBridge" => xchain_bridge: serde_json::Value,
        "XChainClaimID" => xchain_claim_id: String,
        "OtherChainSource" => other_chain_source: String,
        "Amount" => amount: serde_json::Value,
        "AttestationRewardAccount" => attestation_reward_account: String,
        "AttestationSignerAccount" => attestation_signer_account: String,
        "PublicKey" => public_key: String,
        "Signature" => signature: String,
        "WasLockingChainSend" => was_locking_chain_send: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Destination" => destination: Option<String>
    }
}
