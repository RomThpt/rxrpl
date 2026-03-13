use rxrpl_protocol::tx::common::Transaction;
use rxrpl_protocol::ledger::common::LedgerObject;

// --- XChain transaction roundtrips ---

#[test]
fn xchain_create_bridge_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "XChainCreateBridge",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "XChainBridge": {
            "LockingChainDoor": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "LockingChainIssue": {"currency": "XRP"},
            "IssuingChainDoor": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
            "IssuingChainIssue": {"currency": "XRP"}
        },
        "SignatureReward": "200"
    });
    let tx = rxrpl_protocol::tx::XChainCreateBridge::from_json(&json).unwrap();
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "XChainCreateBridge");
    assert_eq!(rt["SignatureReward"], "200");
}

#[test]
fn xchain_modify_bridge_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "XChainModifyBridge",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "XChainBridge": {
            "LockingChainDoor": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "LockingChainIssue": {"currency": "XRP"},
            "IssuingChainDoor": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
            "IssuingChainIssue": {"currency": "XRP"}
        },
        "SignatureReward": "300"
    });
    let tx = rxrpl_protocol::tx::XChainModifyBridge::from_json(&json).unwrap();
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "XChainModifyBridge");
}

#[test]
fn xchain_create_claim_id_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "XChainCreateClaimId",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "XChainBridge": {
            "LockingChainDoor": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "LockingChainIssue": {"currency": "XRP"},
            "IssuingChainDoor": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
            "IssuingChainIssue": {"currency": "XRP"}
        },
        "SignatureReward": "200",
        "OtherChainSource": "rPAnLYrY3M8PMoer2UHkNRPHPEtQVbVPma"
    });
    let tx = rxrpl_protocol::tx::XChainCreateClaimId::from_json(&json).unwrap();
    assert_eq!(tx.other_chain_source, "rPAnLYrY3M8PMoer2UHkNRPHPEtQVbVPma");
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "XChainCreateClaimId");
}

#[test]
fn xchain_commit_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "XChainCommit",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "XChainBridge": {
            "LockingChainDoor": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "LockingChainIssue": {"currency": "XRP"},
            "IssuingChainDoor": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
            "IssuingChainIssue": {"currency": "XRP"}
        },
        "XChainClaimID": "1",
        "Amount": "1000000"
    });
    let tx = rxrpl_protocol::tx::XChainCommit::from_json(&json).unwrap();
    assert_eq!(tx.xchain_claim_id, "1");
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "XChainCommit");
}

#[test]
fn xchain_claim_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "XChainClaim",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "XChainBridge": {
            "LockingChainDoor": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "LockingChainIssue": {"currency": "XRP"},
            "IssuingChainDoor": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
            "IssuingChainIssue": {"currency": "XRP"}
        },
        "XChainClaimID": "1",
        "Destination": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
        "Amount": "1000000"
    });
    let tx = rxrpl_protocol::tx::XChainClaim::from_json(&json).unwrap();
    assert_eq!(tx.destination, "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M");
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "XChainClaim");
}

#[test]
fn xchain_account_create_commit_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "XChainAccountCreateCommit",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "XChainBridge": {
            "LockingChainDoor": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "LockingChainIssue": {"currency": "XRP"},
            "IssuingChainDoor": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
            "IssuingChainIssue": {"currency": "XRP"}
        },
        "Destination": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
        "Amount": "10000000",
        "SignatureReward": "200"
    });
    let tx = rxrpl_protocol::tx::XChainAccountCreateCommit::from_json(&json).unwrap();
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "XChainAccountCreateCommit");
}

#[test]
fn xchain_add_claim_attestation_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "XChainAddClaimAttestation",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "XChainBridge": {
            "LockingChainDoor": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "LockingChainIssue": {"currency": "XRP"},
            "IssuingChainDoor": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
            "IssuingChainIssue": {"currency": "XRP"}
        },
        "XChainClaimID": "1",
        "OtherChainSource": "rPAnLYrY3M8PMoer2UHkNRPHPEtQVbVPma",
        "Amount": "1000000",
        "AttestationRewardAccount": "rPAnLYrY3M8PMoer2UHkNRPHPEtQVbVPma",
        "AttestationSignerAccount": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "PublicKey": "ED1234567890ABCDEF",
        "Signature": "ABCDEF1234567890",
        "WasLockingChainSend": 1
    });
    let tx = rxrpl_protocol::tx::XChainAddClaimAttestation::from_json(&json).unwrap();
    assert_eq!(tx.was_locking_chain_send, 1);
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "XChainAddClaimAttestation");
}

#[test]
fn xchain_add_account_create_attestation_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "XChainAddAccountCreateAttestation",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "XChainBridge": {
            "LockingChainDoor": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "LockingChainIssue": {"currency": "XRP"},
            "IssuingChainDoor": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
            "IssuingChainIssue": {"currency": "XRP"}
        },
        "OtherChainSource": "rPAnLYrY3M8PMoer2UHkNRPHPEtQVbVPma",
        "Destination": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
        "Amount": "10000000",
        "SignatureReward": "200",
        "AttestationRewardAccount": "rPAnLYrY3M8PMoer2UHkNRPHPEtQVbVPma",
        "AttestationSignerAccount": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "PublicKey": "ED1234567890ABCDEF",
        "Signature": "ABCDEF1234567890",
        "WasLockingChainSend": 0,
        "XChainAccountCreateCount": "1"
    });
    let tx = rxrpl_protocol::tx::XChainAddAccountCreateAttestation::from_json(&json).unwrap();
    assert_eq!(tx.was_locking_chain_send, 0);
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "XChainAddAccountCreateAttestation");
}

// --- Credential transaction roundtrips ---

#[test]
fn credential_create_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "CredentialCreate",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "Subject": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
        "CredentialType": "4B5943",
        "Expiration": 741484800,
        "URI": "68747470733A2F2F"
    });
    let tx = rxrpl_protocol::tx::CredentialCreate::from_json(&json).unwrap();
    assert_eq!(tx.credential_type, "4B5943");
    assert_eq!(tx.expiration, Some(741484800));
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "CredentialCreate");
}

#[test]
fn credential_accept_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "CredentialAccept",
        "Account": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
        "Fee": "12",
        "Issuer": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "CredentialType": "4B5943"
    });
    let tx = rxrpl_protocol::tx::CredentialAccept::from_json(&json).unwrap();
    assert_eq!(tx.issuer, "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk");
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "CredentialAccept");
}

#[test]
fn credential_delete_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "CredentialDelete",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "CredentialType": "4B5943",
        "Subject": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
        "Issuer": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk"
    });
    let tx = rxrpl_protocol::tx::CredentialDelete::from_json(&json).unwrap();
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "CredentialDelete");
}

// --- MPToken transaction roundtrips ---

#[test]
fn mptoken_issuance_create_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "MPTokenIssuanceCreate",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "AssetScale": 2,
        "TransferFee": 314,
        "MaximumAmount": "100000000"
    });
    let tx = rxrpl_protocol::tx::MPTokenIssuanceCreate::from_json(&json).unwrap();
    assert_eq!(tx.asset_scale, Some(2));
    assert_eq!(tx.transfer_fee, Some(314));
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "MPTokenIssuanceCreate");
}

#[test]
fn mptoken_issuance_destroy_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "MPTokenIssuanceDestroy",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "MPTokenIssuanceID": "00000001A407AF5856CEF3379FAB85D584A4A42163"
    });
    let tx = rxrpl_protocol::tx::MPTokenIssuanceDestroy::from_json(&json).unwrap();
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "MPTokenIssuanceDestroy");
    assert_eq!(rt["MPTokenIssuanceID"], "00000001A407AF5856CEF3379FAB85D584A4A42163");
}

#[test]
fn mptoken_issuance_set_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "MPTokenIssuanceSet",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "MPTokenIssuanceID": "00000001A407AF5856CEF3379FAB85D584A4A42163",
        "Holder": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M"
    });
    let tx = rxrpl_protocol::tx::MPTokenIssuanceSet::from_json(&json).unwrap();
    assert_eq!(tx.holder, Some("rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M".to_string()));
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "MPTokenIssuanceSet");
}

#[test]
fn mptoken_authorize_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "MPTokenAuthorize",
        "Account": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
        "Fee": "12",
        "MPTokenIssuanceID": "00000001A407AF5856CEF3379FAB85D584A4A42163"
    });
    let tx = rxrpl_protocol::tx::MPTokenAuthorize::from_json(&json).unwrap();
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "MPTokenAuthorize");
}

// --- Vault transaction roundtrips ---

#[test]
fn vault_create_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "VaultCreate",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "Asset": {"currency": "USD", "issuer": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M"},
        "AssetsMaximum": 1000000
    });
    let tx = rxrpl_protocol::tx::VaultCreate::from_json(&json).unwrap();
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "VaultCreate");
}

#[test]
fn vault_set_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "VaultSet",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "VaultID": "000000000000000000000000000000000000000000000000000000000000ABCD"
    });
    let tx = rxrpl_protocol::tx::VaultSet::from_json(&json).unwrap();
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "VaultSet");
}

#[test]
fn vault_delete_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "VaultDelete",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "VaultID": "000000000000000000000000000000000000000000000000000000000000ABCD"
    });
    let tx = rxrpl_protocol::tx::VaultDelete::from_json(&json).unwrap();
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "VaultDelete");
}

#[test]
fn vault_deposit_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "VaultDeposit",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "VaultID": "000000000000000000000000000000000000000000000000000000000000ABCD",
        "Amount": "1000000"
    });
    let tx = rxrpl_protocol::tx::VaultDeposit::from_json(&json).unwrap();
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "VaultDeposit");
}

#[test]
fn vault_withdraw_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "VaultWithdraw",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "VaultID": "000000000000000000000000000000000000000000000000000000000000ABCD",
        "Amount": "500000",
        "Destination": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M"
    });
    let tx = rxrpl_protocol::tx::VaultWithdraw::from_json(&json).unwrap();
    assert_eq!(tx.destination, Some("rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M".to_string()));
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "VaultWithdraw");
}

#[test]
fn vault_clawback_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "VaultClawback",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "VaultID": "000000000000000000000000000000000000000000000000000000000000ABCD",
        "Holder": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M"
    });
    let tx = rxrpl_protocol::tx::VaultClawback::from_json(&json).unwrap();
    assert_eq!(tx.holder, "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M");
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "VaultClawback");
}

// --- PermissionedDomain, Delegate, and other transaction roundtrips ---

#[test]
fn permissioned_domain_set_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "PermissionedDomainSet",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "AcceptedCredentials": [
            {"Credential": {"Issuer": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M", "CredentialType": "4B5943"}}
        ]
    });
    let tx = rxrpl_protocol::tx::PermissionedDomainSet::from_json(&json).unwrap();
    assert_eq!(tx.accepted_credentials.len(), 1);
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "PermissionedDomainSet");
}

#[test]
fn permissioned_domain_delete_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "PermissionedDomainDelete",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "DomainID": "000000000000000000000000000000000000000000000000000000000000ABCD"
    });
    let tx = rxrpl_protocol::tx::PermissionedDomainDelete::from_json(&json).unwrap();
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "PermissionedDomainDelete");
}

#[test]
fn delegate_set_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "DelegateSet",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "Authorize": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
        "Permissions": [{"Permission": {"PermissionValue": "Payment"}}]
    });
    let tx = rxrpl_protocol::tx::DelegateSet::from_json(&json).unwrap();
    assert_eq!(tx.authorize, Some("rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M".to_string()));
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "DelegateSet");
}

#[test]
fn amm_clawback_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "AMMClawback",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "Holder": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
        "Asset": {"currency": "USD", "issuer": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk"},
        "Asset2": {"currency": "XRP"}
    });
    let tx = rxrpl_protocol::tx::AMMClawback::from_json(&json).unwrap();
    assert_eq!(tx.holder, "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M");
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "AMMClawback");
}

#[test]
fn nftoken_modify_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "NFTokenModify",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "NFTokenID": "000800006203F49C21D5D6E022CB16DE3538F248662FC73C00000000000000000000000A"
    });
    let tx = rxrpl_protocol::tx::NFTokenModify::from_json(&json).unwrap();
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "NFTokenModify");
}

#[test]
fn batch_submit_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "BatchSubmit",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "RawTransactions": [
            {"RawTransaction": {"InnerTx": {"TransactionType": "Payment", "Destination": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M", "Amount": "1000000"}}}
        ]
    });
    let tx = rxrpl_protocol::tx::BatchSubmit::from_json(&json).unwrap();
    assert_eq!(tx.raw_transactions.len(), 1);
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "BatchSubmit");
}

#[test]
fn ledger_state_fix_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "LedgerStateFix",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "LedgerFixType": 1
    });
    let tx = rxrpl_protocol::tx::LedgerStateFix::from_json(&json).unwrap();
    assert_eq!(tx.ledger_fix_type, 1);
    let rt = tx.to_json().unwrap();
    assert_eq!(rt["TransactionType"], "LedgerStateFix");
}

// --- Ledger entry roundtrips ---

#[test]
fn bridge_ledger_entry_roundtrip() {
    let json = serde_json::json!({
        "LedgerEntryType": "Bridge",
        "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
        "XChainBridge": {
            "LockingChainDoor": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "LockingChainIssue": {"currency": "XRP"},
            "IssuingChainDoor": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
            "IssuingChainIssue": {"currency": "XRP"}
        },
        "SignatureReward": "200",
        "XChainClaimID": "0",
        "XChainAccountCreateCount": "0",
        "XChainAccountClaimCount": "0",
        "OwnerNode": "0"
    });
    let entry = rxrpl_protocol::ledger::Bridge::from_json(&json).unwrap();
    assert_eq!(entry.account, "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh");
    let rt = entry.to_json().unwrap();
    assert_eq!(rt["Account"], "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh");
}

#[test]
fn xchain_owned_claim_id_roundtrip() {
    let json = serde_json::json!({
        "LedgerEntryType": "XChainOwnedClaimId",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "XChainBridge": {
            "LockingChainDoor": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "LockingChainIssue": {"currency": "XRP"},
            "IssuingChainDoor": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
            "IssuingChainIssue": {"currency": "XRP"}
        },
        "XChainClaimID": "1",
        "OtherChainSource": "rPAnLYrY3M8PMoer2UHkNRPHPEtQVbVPma",
        "XChainClaimAttestations": [],
        "SignatureReward": "200",
        "OwnerNode": "0"
    });
    let entry = rxrpl_protocol::ledger::XChainOwnedClaimId::from_json(&json).unwrap();
    assert_eq!(entry.xchain_claim_id, "1");
    let rt = entry.to_json().unwrap();
    assert_eq!(rt["XChainClaimID"], "1");
}

#[test]
fn xchain_owned_create_account_claim_id_roundtrip() {
    let json = serde_json::json!({
        "LedgerEntryType": "XChainOwnedCreateAccountClaimId",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "XChainBridge": {
            "LockingChainDoor": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "LockingChainIssue": {"currency": "XRP"},
            "IssuingChainDoor": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
            "IssuingChainIssue": {"currency": "XRP"}
        },
        "XChainAccountCreateCount": "1",
        "XChainCreateAccountAttestations": [],
        "OwnerNode": "0"
    });
    let entry = rxrpl_protocol::ledger::XChainOwnedCreateAccountClaimId::from_json(&json).unwrap();
    assert_eq!(entry.xchain_account_create_count, "1");
    let rt = entry.to_json().unwrap();
    assert_eq!(rt["XChainAccountCreateCount"], "1");
}

#[test]
fn credential_ledger_entry_roundtrip() {
    let json = serde_json::json!({
        "LedgerEntryType": "Credential",
        "Subject": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
        "Issuer": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "CredentialType": "4B5943",
        "IssuerNode": "0",
        "Expiration": 741484800
    });
    let entry = rxrpl_protocol::ledger::Credential::from_json(&json).unwrap();
    assert_eq!(entry.credential_type, "4B5943");
    let rt = entry.to_json().unwrap();
    assert_eq!(rt["CredentialType"], "4B5943");
}

#[test]
fn mptoken_issuance_ledger_entry_roundtrip() {
    let json = serde_json::json!({
        "LedgerEntryType": "MPTokenIssuance",
        "Issuer": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Sequence": 1,
        "OwnerNode": "0",
        "OutstandingAmount": "0",
        "AssetScale": 2
    });
    let entry = rxrpl_protocol::ledger::MpTokenIssuance::from_json(&json).unwrap();
    assert_eq!(entry.asset_scale, Some(2));
    let rt = entry.to_json().unwrap();
    assert_eq!(rt["Sequence"], 1);
}

#[test]
fn mptoken_ledger_entry_roundtrip() {
    let json = serde_json::json!({
        "LedgerEntryType": "MPToken",
        "Account": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
        "MPTokenIssuanceID": "00000001A407AF5856CEF3379FAB85D584A4A42163",
        "OwnerNode": "0",
        "MPTAmount": "1000"
    });
    let entry = rxrpl_protocol::ledger::MpToken::from_json(&json).unwrap();
    assert_eq!(entry.mpt_amount, Some("1000".to_string()));
    let rt = entry.to_json().unwrap();
    assert_eq!(rt["MPTokenIssuanceID"], "00000001A407AF5856CEF3379FAB85D584A4A42163");
}

#[test]
fn vault_ledger_entry_roundtrip() {
    let json = serde_json::json!({
        "LedgerEntryType": "Vault",
        "Owner": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Account": "rPseudoAccountForVault",
        "Sequence": 1,
        "OwnerNode": "0",
        "Asset": {"currency": "USD", "issuer": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M"},
        "WithdrawalPolicy": 1,
        "ShareMPTID": "00000001A407AF5856CEF3379FAB85D584A4A42163"
    });
    let entry = rxrpl_protocol::ledger::Vault::from_json(&json).unwrap();
    assert_eq!(entry.owner, "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk");
    let rt = entry.to_json().unwrap();
    assert_eq!(rt["Owner"], "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk");
}

#[test]
fn permissioned_domain_ledger_entry_roundtrip() {
    let json = serde_json::json!({
        "LedgerEntryType": "PermissionedDomain",
        "Owner": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Sequence": 1,
        "AcceptedCredentials": [
            {"Credential": {"Issuer": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M", "CredentialType": "4B5943"}}
        ],
        "OwnerNode": "0"
    });
    let entry = rxrpl_protocol::ledger::PermissionedDomain::from_json(&json).unwrap();
    assert_eq!(entry.accepted_credentials.len(), 1);
    let rt = entry.to_json().unwrap();
    assert_eq!(rt["Sequence"], 1);
}

#[test]
fn delegate_ledger_entry_roundtrip() {
    let json = serde_json::json!({
        "LedgerEntryType": "Delegate",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Authorize": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
        "Permissions": [{"Permission": {"PermissionValue": "Payment"}}],
        "OwnerNode": "0"
    });
    let entry = rxrpl_protocol::ledger::Delegate::from_json(&json).unwrap();
    assert_eq!(entry.authorize, "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M");
    let rt = entry.to_json().unwrap();
    assert_eq!(rt["Authorize"], "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M");
}

#[test]
fn negative_unl_ledger_entry_roundtrip() {
    let json = serde_json::json!({
        "LedgerEntryType": "NegativeUNL",
        "DisabledValidators": []
    });
    let entry = rxrpl_protocol::ledger::NegativeUnl::from_json(&json).unwrap();
    assert!(entry.disabled_validators.is_some());
}

// --- TransactionKind polymorphic roundtrips ---

#[test]
fn transaction_kind_xchain_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "XChainCreateBridge",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "XChainBridge": {
            "LockingChainDoor": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "LockingChainIssue": {"currency": "XRP"},
            "IssuingChainDoor": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
            "IssuingChainIssue": {"currency": "XRP"}
        },
        "SignatureReward": "200"
    });
    let kind = rxrpl_protocol::tx::TransactionKind::from_json(&json).unwrap();
    assert!(matches!(kind, rxrpl_protocol::tx::TransactionKind::XChainCreateBridge(_)));
}

#[test]
fn transaction_kind_vault_roundtrip() {
    let json = serde_json::json!({
        "TransactionType": "VaultCreate",
        "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Fee": "12",
        "Asset": {"currency": "USD", "issuer": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M"}
    });
    let kind = rxrpl_protocol::tx::TransactionKind::from_json(&json).unwrap();
    assert!(matches!(kind, rxrpl_protocol::tx::TransactionKind::VaultCreate(_)));
}

// --- LedgerObjectKind polymorphic roundtrips ---

#[test]
fn ledger_object_kind_bridge_roundtrip() {
    let json = serde_json::json!({
        "LedgerEntryType": "Bridge",
        "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
        "XChainBridge": {
            "LockingChainDoor": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "LockingChainIssue": {"currency": "XRP"},
            "IssuingChainDoor": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M",
            "IssuingChainIssue": {"currency": "XRP"}
        },
        "SignatureReward": "200",
        "XChainClaimID": "0",
        "XChainAccountCreateCount": "0",
        "XChainAccountClaimCount": "0",
        "OwnerNode": "0"
    });
    let kind = rxrpl_protocol::ledger::LedgerObjectKind::from_json(&json).unwrap();
    assert!(matches!(kind, rxrpl_protocol::ledger::LedgerObjectKind::Bridge(_)));
}

#[test]
fn ledger_object_kind_vault_roundtrip() {
    let json = serde_json::json!({
        "LedgerEntryType": "Vault",
        "Owner": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
        "Account": "rPseudoAccountForVault",
        "Sequence": 1,
        "OwnerNode": "0",
        "Asset": {"currency": "USD", "issuer": "rGWrZyQqhTp9Xu7G5iFQmGeFSGnR3EUj3M"},
        "WithdrawalPolicy": 1,
        "ShareMPTID": "00000001A407AF5856CEF3379FAB85D584A4A42163"
    });
    let kind = rxrpl_protocol::ledger::LedgerObjectKind::from_json(&json).unwrap();
    assert!(matches!(kind, rxrpl_protocol::ledger::LedgerObjectKind::Vault(_)));
}

// --- TransactionType enum code roundtrips ---

#[test]
fn new_transaction_type_codes_roundtrip() {
    use rxrpl_protocol::types::TransactionType;

    let variants = [
        (TransactionType::AMMClawback, 31, "AMMClawback"),
        (TransactionType::XChainCreateClaimId, 41, "XChainCreateClaimId"),
        (TransactionType::XChainCommit, 42, "XChainCommit"),
        (TransactionType::XChainClaim, 43, "XChainClaim"),
        (TransactionType::XChainAccountCreateCommit, 44, "XChainAccountCreateCommit"),
        (TransactionType::XChainAddClaimAttestation, 45, "XChainAddClaimAttestation"),
        (TransactionType::XChainAddAccountCreateAttestation, 46, "XChainAddAccountCreateAttestation"),
        (TransactionType::XChainModifyBridge, 47, "XChainModifyBridge"),
        (TransactionType::XChainCreateBridge, 48, "XChainCreateBridge"),
        (TransactionType::LedgerStateFix, 53, "LedgerStateFix"),
        (TransactionType::MPTokenIssuanceCreate, 54, "MPTokenIssuanceCreate"),
        (TransactionType::MPTokenIssuanceDestroy, 55, "MPTokenIssuanceDestroy"),
        (TransactionType::MPTokenIssuanceSet, 56, "MPTokenIssuanceSet"),
        (TransactionType::MPTokenAuthorize, 57, "MPTokenAuthorize"),
        (TransactionType::CredentialCreate, 58, "CredentialCreate"),
        (TransactionType::CredentialAccept, 59, "CredentialAccept"),
        (TransactionType::CredentialDelete, 60, "CredentialDelete"),
        (TransactionType::NFTokenModify, 61, "NFTokenModify"),
        (TransactionType::PermissionedDomainSet, 62, "PermissionedDomainSet"),
        (TransactionType::PermissionedDomainDelete, 63, "PermissionedDomainDelete"),
        (TransactionType::DelegateSet, 64, "DelegateSet"),
        (TransactionType::VaultCreate, 65, "VaultCreate"),
        (TransactionType::VaultSet, 66, "VaultSet"),
        (TransactionType::VaultDelete, 67, "VaultDelete"),
        (TransactionType::VaultDeposit, 68, "VaultDeposit"),
        (TransactionType::VaultWithdraw, 69, "VaultWithdraw"),
        (TransactionType::VaultClawback, 70, "VaultClawback"),
        (TransactionType::BatchSubmit, 71, "BatchSubmit"),
    ];
    for (variant, code, name) in variants {
        assert_eq!(variant.code(), code, "code mismatch for {name}");
        assert_eq!(variant.as_str(), name, "name mismatch for code {code}");
        assert_eq!(TransactionType::from_code(code).unwrap(), variant, "from_code({code}) failed");
        assert_eq!(TransactionType::from_name(name).unwrap(), variant, "from_name({name}) failed");
    }
}

// --- LedgerEntryType enum code roundtrips ---

#[test]
fn new_ledger_entry_type_codes_roundtrip() {
    use rxrpl_protocol::types::LedgerEntryType;

    let variants = [
        (LedgerEntryType::Bridge, 0x0069, "Bridge"),
        (LedgerEntryType::XChainOwnedClaimId, 0x0071, "XChainOwnedClaimId"),
        (LedgerEntryType::XChainOwnedCreateAccountClaimId, 0x0074, "XChainOwnedCreateAccountClaimId"),
        (LedgerEntryType::MPTokenIssuance, 0x007E, "MPTokenIssuance"),
        (LedgerEntryType::MPToken, 0x007F, "MPToken"),
        (LedgerEntryType::Credential, 0x0081, "Credential"),
        (LedgerEntryType::PermissionedDomain, 0x0082, "PermissionedDomain"),
        (LedgerEntryType::Delegate, 0x0083, "Delegate"),
        (LedgerEntryType::Vault, 0x0084, "Vault"),
        (LedgerEntryType::NegativeUNL, 0x004E, "NegativeUNL"),
    ];
    for (variant, code, name) in variants {
        assert_eq!(variant.code(), code, "code mismatch for {name}");
        assert_eq!(variant.as_str(), name, "name mismatch for code {code:#06x}");
        assert_eq!(LedgerEntryType::from_code(code).unwrap(), variant, "from_code({code:#06x}) failed");
        assert_eq!(LedgerEntryType::from_name(name).unwrap(), variant, "from_name({name}) failed");
    }
}
