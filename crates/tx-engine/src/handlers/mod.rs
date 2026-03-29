pub mod account_delete;
pub mod account_set;
pub mod amm_bid;
pub mod amm_clawback;
pub mod amm_create;
pub mod amm_delete;
pub mod amm_deposit;
pub mod amm_vote;
pub mod amm_withdraw;
pub mod batch_submit;
pub mod check_cancel;
pub mod check_cash;
pub mod check_create;
pub mod clawback;
pub mod credential_accept;
pub mod credential_create;
pub mod credential_delete;
pub mod delegate_set;
pub mod deposit_preauth;
pub mod did_delete;
pub mod did_set;
pub mod enable_amendment;
pub mod escrow_cancel;
pub mod escrow_create;
pub mod escrow_finish;
pub mod ledger_state_fix;
pub mod mptoken_authorize;
pub mod mptoken_issuance_create;
pub mod mptoken_issuance_destroy;
pub mod mptoken_issuance_set;
pub mod nftoken_accept_offer;
pub mod nftoken_burn;
pub mod nftoken_cancel_offer;
pub mod nftoken_create_offer;
pub mod nftoken_mint;
pub mod nftoken_modify;
pub mod nickname_set;
pub mod offer_cancel;
pub mod offer_create;
pub mod oracle_delete;
pub mod oracle_set;
pub mod payment;
pub mod payment_channel_claim;
pub mod payment_channel_create;
pub mod payment_channel_fund;
pub mod permissioned_domain_delete;
pub mod permissioned_domain_set;
pub mod set_fee;
pub mod set_hook;
pub mod set_regular_key;
pub mod signer_list_set;
pub mod ticket_create;
pub mod trust_set;
pub mod unl_modify;
pub mod vault_clawback;
pub mod vault_create;
pub mod vault_delete;
pub mod vault_deposit;
pub mod vault_set;
pub mod vault_withdraw;
pub mod xchain_account_create_commit;
pub mod xchain_add_account_create_attestation;
pub mod xchain_add_claim_attestation;
pub mod xchain_claim;
pub mod xchain_commit;
pub mod xchain_create_bridge;
pub mod xchain_create_claim_id;
pub mod xchain_modify_bridge;

use rxrpl_protocol::TransactionType;

use crate::registry::TransactorRegistry;

/// Register all implemented Phase A transaction handlers.
pub fn register_phase_a(registry: &mut TransactorRegistry) {
    registry.register(TransactionType::Payment, payment::PaymentTransactor);
    registry.register(
        TransactionType::AccountSet,
        account_set::AccountSetTransactor,
    );
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
    registry.register(
        TransactionType::AccountDelete,
        account_delete::AccountDeleteTransactor,
    );
}

/// Register all implemented Phase B transaction handlers.
pub fn register_phase_b(registry: &mut TransactorRegistry) {
    registry.register(
        TransactionType::EscrowCreate,
        escrow_create::EscrowCreateTransactor,
    );
    registry.register(
        TransactionType::EscrowFinish,
        escrow_finish::EscrowFinishTransactor,
    );
    registry.register(
        TransactionType::EscrowCancel,
        escrow_cancel::EscrowCancelTransactor,
    );
    registry.register(
        TransactionType::CheckCreate,
        check_create::CheckCreateTransactor,
    );
    registry.register(TransactionType::CheckCash, check_cash::CheckCashTransactor);
    registry.register(
        TransactionType::CheckCancel,
        check_cancel::CheckCancelTransactor,
    );
    registry.register(
        TransactionType::PaymentChannelCreate,
        payment_channel_create::PaymentChannelCreateTransactor,
    );
    registry.register(
        TransactionType::PaymentChannelFund,
        payment_channel_fund::PaymentChannelFundTransactor,
    );
    registry.register(
        TransactionType::PaymentChannelClaim,
        payment_channel_claim::PaymentChannelClaimTransactor,
    );
    registry.register(
        TransactionType::DepositPreauth,
        deposit_preauth::DepositPreauthTransactor,
    );
}

/// Register all implemented Phase C1 transaction handlers.
pub fn register_phase_c1(registry: &mut TransactorRegistry) {
    registry.register(
        TransactionType::NFTokenMint,
        nftoken_mint::NFTokenMintTransactor,
    );
    registry.register(
        TransactionType::NFTokenBurn,
        nftoken_burn::NFTokenBurnTransactor,
    );
    registry.register(
        TransactionType::NFTokenCreateOffer,
        nftoken_create_offer::NFTokenCreateOfferTransactor,
    );
    registry.register(
        TransactionType::NFTokenCancelOffer,
        nftoken_cancel_offer::NFTokenCancelOfferTransactor,
    );
    registry.register(
        TransactionType::NFTokenAcceptOffer,
        nftoken_accept_offer::NFTokenAcceptOfferTransactor,
    );
    registry.register(
        TransactionType::NFTokenModify,
        nftoken_modify::NFTokenModifyTransactor,
    );
    registry.register(TransactionType::Clawback, clawback::ClawbackTransactor);
}

/// Register all implemented Phase C2 transaction handlers (simple set/delete).
pub fn register_phase_c2(registry: &mut TransactorRegistry) {
    registry.register(TransactionType::DIDSet, did_set::DIDSetTransactor);
    registry.register(TransactionType::DIDDelete, did_delete::DIDDeleteTransactor);
    registry.register(TransactionType::OracleSet, oracle_set::OracleSetTransactor);
    registry.register(
        TransactionType::OracleDelete,
        oracle_delete::OracleDeleteTransactor,
    );
    registry.register(
        TransactionType::PermissionedDomainSet,
        permissioned_domain_set::PermissionedDomainSetTransactor,
    );
    registry.register(
        TransactionType::PermissionedDomainDelete,
        permissioned_domain_delete::PermissionedDomainDeleteTransactor,
    );
    registry.register(
        TransactionType::DelegateSet,
        delegate_set::DelegateSetTransactor,
    );
    registry.register(
        TransactionType::CredentialCreate,
        credential_create::CredentialCreateTransactor,
    );
    registry.register(
        TransactionType::CredentialAccept,
        credential_accept::CredentialAcceptTransactor,
    );
    registry.register(
        TransactionType::CredentialDelete,
        credential_delete::CredentialDeleteTransactor,
    );
    registry.register(
        TransactionType::LedgerStateFix,
        ledger_state_fix::LedgerStateFixTransactor,
    );
}

/// Register all implemented Phase C3 transaction handlers (MPToken lifecycle).
pub fn register_phase_c3(registry: &mut TransactorRegistry) {
    registry.register(
        TransactionType::MPTokenIssuanceCreate,
        mptoken_issuance_create::MPTokenIssuanceCreateTransactor,
    );
    registry.register(
        TransactionType::MPTokenIssuanceDestroy,
        mptoken_issuance_destroy::MPTokenIssuanceDestroyTransactor,
    );
    registry.register(
        TransactionType::MPTokenIssuanceSet,
        mptoken_issuance_set::MPTokenIssuanceSetTransactor,
    );
    registry.register(
        TransactionType::MPTokenAuthorize,
        mptoken_authorize::MPTokenAuthorizeTransactor,
    );
}

/// Register all implemented Phase D1 transaction handlers (Vault lifecycle).
pub fn register_phase_d1(registry: &mut TransactorRegistry) {
    registry.register(
        TransactionType::VaultCreate,
        vault_create::VaultCreateTransactor,
    );
    registry.register(TransactionType::VaultSet, vault_set::VaultSetTransactor);
    registry.register(
        TransactionType::VaultDelete,
        vault_delete::VaultDeleteTransactor,
    );
    registry.register(
        TransactionType::VaultDeposit,
        vault_deposit::VaultDepositTransactor,
    );
    registry.register(
        TransactionType::VaultWithdraw,
        vault_withdraw::VaultWithdrawTransactor,
    );
    registry.register(
        TransactionType::VaultClawback,
        vault_clawback::VaultClawbackTransactor,
    );
}

/// Register all implemented Phase D2 transaction handlers (AMM).
pub fn register_phase_d2(registry: &mut TransactorRegistry) {
    registry.register(TransactionType::AMMCreate, amm_create::AMMCreateTransactor);
    registry.register(
        TransactionType::AMMDeposit,
        amm_deposit::AMMDepositTransactor,
    );
    registry.register(
        TransactionType::AMMWithdraw,
        amm_withdraw::AMMWithdrawTransactor,
    );
    registry.register(TransactionType::AMMVote, amm_vote::AMMVoteTransactor);
    registry.register(TransactionType::AMMBid, amm_bid::AMMBidTransactor);
    registry.register(TransactionType::AMMDelete, amm_delete::AMMDeleteTransactor);
    registry.register(
        TransactionType::AMMClawback,
        amm_clawback::AMMClawbackTransactor,
    );
}

/// Register all implemented Phase E transaction handlers (XChain Bridge).
pub fn register_phase_e(registry: &mut TransactorRegistry) {
    registry.register(
        TransactionType::XChainCreateBridge,
        xchain_create_bridge::XChainCreateBridgeTransactor,
    );
    registry.register(
        TransactionType::XChainModifyBridge,
        xchain_modify_bridge::XChainModifyBridgeTransactor,
    );
    registry.register(
        TransactionType::XChainCreateClaimId,
        xchain_create_claim_id::XChainCreateClaimIdTransactor,
    );
    registry.register(
        TransactionType::XChainCommit,
        xchain_commit::XChainCommitTransactor,
    );
    registry.register(
        TransactionType::XChainClaim,
        xchain_claim::XChainClaimTransactor,
    );
    registry.register(
        TransactionType::XChainAccountCreateCommit,
        xchain_account_create_commit::XChainAccountCreateCommitTransactor,
    );
    registry.register(
        TransactionType::XChainAddClaimAttestation,
        xchain_add_claim_attestation::XChainAddClaimAttestationTransactor,
    );
    registry.register(
        TransactionType::XChainAddAccountCreateAttestation,
        xchain_add_account_create_attestation::XChainAddAccountCreateAttestationTransactor,
    );
}

/// Register the BatchSubmit handler (atomic batch execution).
pub fn register_batch(registry: &mut TransactorRegistry) {
    registry.register(
        TransactionType::BatchSubmit,
        batch_submit::BatchSubmitTransactor,
    );
}

/// Register the SetHook handler (WASM hooks).
pub fn register_hooks(registry: &mut TransactorRegistry) {
    registry.register(TransactionType::SetHook, set_hook::SetHookTransactor);
}

/// Register stub handlers for unimplemented transaction types.
///
/// These return `TemDisabled` at preflight: NickNameSet (deprecated 2014).
pub fn register_stubs(registry: &mut TransactorRegistry) {
    registry.register(
        TransactionType::NickNameSet,
        nickname_set::NickNameSetTransactor,
    );
}

/// Register pseudo-transaction handlers.
///
/// Pseudo-transactions (EnableAmendment, SetFee, UNLModify) are emitted by
/// consensus and bypass signature verification and fee deduction in the engine.
pub fn register_pseudo(registry: &mut TransactorRegistry) {
    registry.register(
        TransactionType::EnableAmendment,
        enable_amendment::EnableAmendmentTransactor,
    );
    registry.register(TransactionType::SetFee, set_fee::SetFeeTransactor);
    registry.register(TransactionType::UNLModify, unl_modify::UNLModifyTransactor);
}
