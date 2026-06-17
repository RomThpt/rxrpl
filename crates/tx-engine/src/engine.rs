use rxrpl_amendment::Rules;
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, TransactionType, keylet};
use serde_json::Value;

use crate::error::TxEngineError;
use crate::fees::FeeSettings;
use crate::helpers;
use crate::invariants;
use crate::registry::TransactorRegistry;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext};
use crate::view::apply_view::ApplyView;
use crate::view::ledger_view::LedgerView;
use crate::view::read_view::ReadView;
use crate::view::sandbox::Sandbox;

/// Check that the transaction's Sequence matches the AccountRoot's Sequence.
///
/// Mirrors rippled's `Transactor::checkSeqProxy`. Returns:
/// - `tefPAST_SEQ` if the tx Sequence is below the account's (already used)
/// - `terPRE_SEQ` if the tx Sequence is above the account's (gap, retry later)
/// - `terNO_ACCOUNT` if the source account does not exist
fn check_seq_proxy(tx: &Value, view: &dyn ReadView) -> Result<(), TransactionResult> {
    let account_str = helpers::get_account(tx)?;
    let account_id =
        decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let account_key = keylet::account(&account_id);
    let account_bytes = view
        .read(&account_key)
        .ok_or(TransactionResult::TerNoAccount)?;
    let account_obj: Value =
        serde_json::from_slice(&account_bytes).map_err(|_| TransactionResult::TefInternal)?;

    let a_seq = helpers::get_sequence(&account_obj);

    // Ticket path (rippled Transactor::checkSeqProxy): when a TicketSequence
    // is present the tx consumes a Ticket SLE instead of the account Sequence.
    // The ticket must already exist; otherwise the tx is invalid. A ticket
    // below the account sequence was never created or is already spent
    // (tefNO_TICKET); one at/above the account sequence has not been created
    // yet, so the tx may apply later (terPRE_TICKET, retryable).
    if let Some(ticket_seq) = tx.get("TicketSequence").and_then(|v| v.as_u64()) {
        let ticket_seq = ticket_seq as u32;
        let ticket_key = keylet::ticket(&account_id, ticket_seq);
        if view.read(&ticket_key).is_some() {
            return Ok(());
        }
        return if ticket_seq < a_seq {
            Err(TransactionResult::TefNoTicket)
        } else {
            Err(TransactionResult::TerPreTicket)
        };
    }

    let t_seq = tx.get("Sequence").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

    if t_seq == a_seq {
        Ok(())
    } else if t_seq > a_seq {
        Err(TransactionResult::TerPreSeq)
    } else {
        Err(TransactionResult::TefPastSeq)
    }
}

/// The transaction execution engine.
///
/// Orchestrates the full transaction processing pipeline:
/// 1. Look up transactor from registry
/// 2. Run preflight (stateless validation)
/// 3. Verify signature
/// 4. Calculate base fee
/// 5. Run preclaim (read-only ledger validation)
/// 6. Create sandbox, deduct fee from sender account
/// 7. Run apply in nested (child) sandbox
/// 8. Run invariant checks
/// 9. Commit sandbox to ledger + record tx in tx_map
pub struct TxEngine {
    registry: TransactorRegistry,
    invariants: Vec<Box<dyn invariants::InvariantCheck>>,
    skip_signature_verification: bool,
}

impl TxEngine {
    pub fn new(registry: TransactorRegistry) -> Self {
        Self {
            registry,
            invariants: invariants::default_invariant_checks(),
            skip_signature_verification: false,
        }
    }

    /// Create an engine that skips signature verification.
    ///
    /// Useful for unit tests that use unsigned transactions.
    pub fn new_without_sig_check(registry: TransactorRegistry) -> Self {
        Self {
            registry,
            invariants: invariants::default_invariant_checks(),
            skip_signature_verification: true,
        }
    }

    /// Check if a transaction type is a pseudo-transaction.
    ///
    /// Pseudo-transactions bypass signature verification and fee deduction.
    fn is_pseudo_transaction(tx_type: &TransactionType) -> bool {
        matches!(
            tx_type,
            TransactionType::EnableAmendment | TransactionType::SetFee | TransactionType::UNLModify
        )
    }

    /// Apply a transaction to a ledger.
    ///
    /// Returns the transaction result code.
    pub fn apply(
        &self,
        tx: &Value,
        ledger: &mut rxrpl_ledger::Ledger,
        rules: &Rules,
        fees: &FeeSettings,
    ) -> Result<TransactionResult, TxEngineError> {
        // 1. Determine transaction type
        let tx_type_str = tx
            .get("TransactionType")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TxEngineError::UnknownTransactionType("missing".into()))?;

        let tx_type = TransactionType::from_name(tx_type_str)
            .map_err(|_| TxEngineError::UnknownTransactionType(tx_type_str.into()))?;

        let transactor = self
            .registry
            .get(&tx_type)
            .ok_or_else(|| TxEngineError::UnknownTransactionType(tx_type_str.into()))?;

        let is_pseudo = Self::is_pseudo_transaction(&tx_type);

        // 2. Preflight (stateless)
        let preflight_ctx = PreflightContext { tx, rules, fees };
        if let Err(result) = transactor.preflight(&preflight_ctx) {
            return Ok(result);
        }

        // 3. Verify signature (skip for pseudo-transactions)
        if !is_pseudo && !self.skip_signature_verification {
            if let Err(_e) = rxrpl_protocol::tx::verify_signature(tx) {
                return Ok(TransactionResult::TefBadSignature);
            }
        }

        // 4. Calculate base fee
        let _base_fee = transactor.calculate_base_fee(&preflight_ctx);

        // 5. Preclaim (read-only ledger validation)
        let view = LedgerView::with_fees(ledger, fees.clone());

        // Master/regular key authorization (single-sig only): when a tx is
        // single-signed (no Signers array), verify the SigningPubKey is
        // authorized for the Account. Three cases match rippled:
        //  1. Pubkey derives to Account itself → master key sign. Reject if
        //     account has lsfDisableMaster (0x00100000) set → tefMASTER_DISABLED.
        //  2. Pubkey derives to AccountRoot.RegularKey → regular key sign. OK.
        //  3. Otherwise → tefBAD_AUTH.
        // This is stateful authorization (independent of cryptographic
        // signature verification) and must run even when crypto verify is
        // skipped — otherwise the production node (which skips verify because
        // signatures are checked at ingress / consensus) would never enforce
        // lsfDisableMaster or RegularKey rotation.
        if !is_pseudo {
            let has_signers = tx
                .get("Signers")
                .and_then(|v| v.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            if !has_signers {
                if let Ok(account_str) = helpers::get_account(tx) {
                    if let Ok(account_id) = decode_account_id(account_str) {
                        if let Some(pubkey_hex) = tx.get("SigningPubKey").and_then(|v| v.as_str()) {
                            if !pubkey_hex.is_empty() {
                                if let Ok(pubkey_bytes) = hex::decode(pubkey_hex) {
                                    let signer_id =
                                        rxrpl_codec::address::classic::account_id_from_public_key(
                                            &pubkey_bytes,
                                        );
                                    let acct_key = rxrpl_protocol::keylet::account(&account_id);
                                    if let Some(acct_bytes) = view.read(&acct_key) {
                                        if let Ok(acct_obj) =
                                            serde_json::from_slice::<serde_json::Value>(&acct_bytes)
                                        {
                                            if signer_id == account_id {
                                                // Master key sign — reject if disabled.
                                                let flags = acct_obj
                                                    .get("Flags")
                                                    .and_then(|v| v.as_u64())
                                                    .unwrap_or(0)
                                                    as u32;
                                                const LSF_DISABLE_MASTER: u32 = 0x00100000;
                                                if flags & LSF_DISABLE_MASTER != 0 {
                                                    return Ok(
                                                        TransactionResult::TefMasterDisabled,
                                                    );
                                                }
                                            } else {
                                                // Pubkey doesn't derive to Account → must match
                                                // the configured RegularKey. If no RegularKey is
                                                // configured, this signer is unauthorized.
                                                let reg_id = acct_obj
                                                    .get("RegularKey")
                                                    .and_then(|v| v.as_str())
                                                    .and_then(|s| decode_account_id(s).ok());
                                                if Some(signer_id) != reg_id {
                                                    return Ok(TransactionResult::TefBadAuth);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Multi-sign stateful check: if the tx has a non-empty Signers array,
        // the Account must have a SignerList SLE AND the sum of weights of
        // included signers must meet the SignerList's quorum. Without this
        // gate, rxrpl was silently accepting multi-signed txs from accounts
        // that never registered a signer list (rippled returns
        // tefNOT_MULTI_SIGNING / tefBAD_QUORUM).
        // Runs regardless of skip_signature_verification — the SignerList /
        // quorum check is stateful authorization, not cryptographic verify.
        if !is_pseudo {
            if let Some(signers_arr) = tx.get("Signers").and_then(|v| v.as_array()) {
                if !signers_arr.is_empty() {
                    if let Ok(account_str) = helpers::get_account(tx) {
                        if let Ok(account_id) = decode_account_id(account_str) {
                            let signer_list_key = rxrpl_protocol::keylet::signer_list(&account_id);
                            match view.read(&signer_list_key) {
                                None => return Ok(TransactionResult::TefNotMultiSigning),
                                Some(bytes) => {
                                    if let Ok(sl) =
                                        serde_json::from_slice::<serde_json::Value>(&bytes)
                                    {
                                        let quorum = sl
                                            .get("SignerQuorum")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(0);
                                        let entries = sl
                                            .get("SignerEntries")
                                            .and_then(|v| v.as_array())
                                            .cloned()
                                            .unwrap_or_default();
                                        let mut total_weight: u64 = 0;
                                        for s in signers_arr {
                                            let signer_obj = s
                                                .get("Signer")
                                                .or(Some(s))
                                                .cloned()
                                                .unwrap_or(serde_json::Value::Null);
                                            let signer_acct = signer_obj
                                                .get("Account")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            if let Some(weight) = entries.iter().find_map(|e| {
                                                let entry = e.get("SignerEntry").or(Some(e))?;
                                                if entry.get("Account").and_then(|v| v.as_str())
                                                    == Some(signer_acct)
                                                {
                                                    entry
                                                        .get("SignerWeight")
                                                        .and_then(|v| v.as_u64())
                                                } else {
                                                    None
                                                }
                                            }) {
                                                total_weight = total_weight.saturating_add(weight);
                                            }
                                        }
                                        if total_weight < quorum {
                                            return Ok(TransactionResult::TefBadQuorum);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Sequence check: rippled Transactor::checkSeqProxy. With a plain
        // Sequence the tx must match the AccountRoot Sequence (else tefPAST_SEQ
        // / terPRE_SEQ); with a TicketSequence the referenced Ticket SLE must
        // exist (else tefNO_TICKET / terPRE_TICKET). Skipped only for
        // pseudo-transactions, which carry neither.
        if !is_pseudo {
            if let Err(result) = check_seq_proxy(tx, &view) {
                return Ok(result);
            }
        }

        let preclaim_ctx = PreclaimContext {
            tx,
            view: &view,
            rules,
        };
        if let Err(result) = transactor.preclaim(&preclaim_ctx) {
            return Ok(result);
        }

        // 6. Create sandbox and deduct fee from sender account
        //    (skip fee deduction for pseudo-transactions)
        let drops_before = ledger.header.drops;
        let view = LedgerView::with_fees(ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);

        if !is_pseudo {
            let fee_drops = helpers::get_fee(tx);
            if fee_drops > 0 {
                let account_str =
                    helpers::get_account(tx).map_err(TxEngineError::TransactionFailed)?;
                let account_id = decode_account_id(account_str).map_err(|_| {
                    TxEngineError::TransactionFailed(TransactionResult::TemInvalidAccountId)
                })?;
                let account_key = keylet::account(&account_id);

                let account_bytes =
                    sandbox
                        .read(&account_key)
                        .ok_or(TxEngineError::TransactionFailed(
                            TransactionResult::TerNoAccount,
                        ))?;
                let mut account_obj: Value =
                    serde_json::from_slice(&account_bytes).map_err(|_| {
                        TxEngineError::TransactionFailed(TransactionResult::TefInternal)
                    })?;

                let balance = helpers::get_balance(&account_obj);
                if balance < fee_drops {
                    return Ok(TransactionResult::TerInsufFee);
                }
                helpers::set_balance(&mut account_obj, balance - fee_drops);

                let updated = serde_json::to_vec(&account_obj).map_err(|_| {
                    TxEngineError::TransactionFailed(TransactionResult::TefInternal)
                })?;
                sandbox.update(account_key, updated).map_err(|_| {
                    TxEngineError::TransactionFailed(TransactionResult::TefInternal)
                })?;

                sandbox.destroy_drops(fee_drops);
            }
        }

        // 7. Apply in child sandbox
        let mut child = sandbox.child();

        let handler_result = if tx_type == TransactionType::BatchSubmit {
            self.apply_batch(tx, &mut child, rules, fees)
        } else {
            let mut apply_ctx = ApplyContext {
                tx,
                view: &mut child,
                rules,
                fees,
            };
            transactor.apply(&mut apply_ctx)
        };
        // Consume child to release borrow on sandbox
        let child_changes = child.into_changes();

        let (result, should_commit) = match handler_result {
            Ok(result) => {
                // tes: merge child mutations into parent
                sandbox.merge_child_changes(child_changes);

                // Execute hooks on the destination account (if any)
                if result == TransactionResult::TesSuccess {
                    let tx_hash = rxrpl_protocol::tx::compute_tx_hash(tx).unwrap_or_default();
                    if let Some(hook_result) =
                        crate::hooks::execute_hooks_for_tx(tx, &tx_hash, &sandbox)
                    {
                        if hook_result.rollback {
                            // A hook called rollback -- revert the transaction
                            return Ok(TransactionResult::TecHookRejected);
                        }
                    }
                }

                (result, true)
            }
            Err(result) if result.is_claimed() => {
                // tec: discard child mutations, keep fee deduction
                (result, true)
            }
            Err(result) => {
                // tem/tef/ter: discard everything
                return Ok(result);
            }
        };

        // 8. Invariant checks
        let mut changes = sandbox.into_changes();
        let drops_after = drops_before.saturating_sub(changes.destroyed_drops);

        let tx_for_invariants = if is_pseudo { None } else { Some(tx) };
        if let Err(msg) = invariants::run_invariant_checks(
            &self.invariants,
            &changes,
            drops_before,
            drops_after,
            tx_for_invariants,
        ) {
            return Err(TxEngineError::InvariantViolated(msg));
        }

        // 9. Commit to ledger
        if should_commit {
            // Record the modifying transaction on every touched entry that
            // carries PreviousTxnID, mirroring rippled, before metadata/commit.
            let tx_hash = rxrpl_protocol::tx::compute_tx_hash(tx)
                .map_err(|e| TxEngineError::Codec(e.to_string()))?;
            changes.stamp_previous_txn(
                &hex::encode_upper(tx_hash.as_bytes()),
                ledger.header.sequence,
            );

            // Build metadata before consuming changes, then store the
            // transaction SHAMap leaf in rippled's canonical form,
            // `VL(tx) || VL(meta)`, so the transaction tree root (tx_hash)
            // matches the validated chain.
            let meta = changes.build_metadata(0, result.code());
            let meta_json = meta.to_canonical_json();

            changes.apply_to_ledger(ledger)?;

            // Build the canonical `VL(tx) || VL(meta)` leaf. If either fails to
            // encode (e.g. a pseudo-transaction whose SLE is not yet stored in
            // canonical form), fall back to a tx-only leaf rather than failing
            // the apply — the state is already correct, only this ledger's
            // tx_hash would differ. Tracked as separate metadata-fidelity work.
            let leaf = match (
                rxrpl_codec::binary::encode(tx),
                rxrpl_codec::binary::encode(&meta_json),
            ) {
                (Ok(tx_blob), Ok(meta_blob)) => {
                    let mut leaf = rxrpl_codec::binary::encode_vl(&tx_blob);
                    leaf.extend_from_slice(&rxrpl_codec::binary::encode_vl(&meta_blob));
                    leaf
                }
                (tx_enc, meta_enc) => {
                    tracing::warn!(
                        "non-canonical tx leaf for {} (tx_ok={}, meta_err={:?})",
                        tx_hash,
                        tx_enc.is_ok(),
                        meta_enc.err(),
                    );
                    let mut leaf = rxrpl_codec::binary::encode_vl(&tx_enc.unwrap_or_default());
                    leaf.extend_from_slice(&rxrpl_codec::binary::encode_vl(&[]));
                    leaf
                }
            };
            ledger.add_transaction(tx_hash, leaf)?;
        }

        Ok(result)
    }

    /// Execute inner transactions for a BatchSubmit atomically.
    ///
    /// Each inner transaction goes through preflight, preclaim, fee deduction,
    /// and apply within the same sandbox. If any inner tx fails, the entire
    /// batch is rolled back.
    fn apply_batch(
        &self,
        batch_tx: &Value,
        sandbox: &mut crate::view::sandbox::Sandbox<'_>,
        rules: &Rules,
        fees: &FeeSettings,
    ) -> Result<TransactionResult, TransactionResult> {
        use crate::handlers::batch_submit::extract_inner_txs;
        use crate::transactor::PreclaimContext;

        let inner_txs = extract_inner_txs(batch_tx)?;

        for inner_tx in inner_txs {
            let inner_type_str = inner_tx
                .get("TransactionType")
                .and_then(|v| v.as_str())
                .ok_or(TransactionResult::TemMalformed)?;

            let inner_type = TransactionType::from_name(inner_type_str)
                .map_err(|_| TransactionResult::TemMalformed)?;

            let transactor = self
                .registry
                .get(&inner_type)
                .ok_or(TransactionResult::TemMalformed)?;

            // Preflight
            let preflight_ctx = crate::transactor::PreflightContext {
                tx: inner_tx,
                rules,
                fees,
            };
            transactor.preflight(&preflight_ctx)?;

            // Preclaim against current sandbox state (sees prior inner tx mutations)
            let preclaim_ctx = PreclaimContext {
                tx: inner_tx,
                view: sandbox as &dyn ReadView,
                rules,
            };
            transactor.preclaim(&preclaim_ctx)?;

            // Fee deduction for inner transaction
            let fee_drops = helpers::get_fee(inner_tx);
            if fee_drops > 0 {
                let account_str =
                    helpers::get_account(inner_tx).map_err(|_| TransactionResult::TemMalformed)?;
                let account_id = decode_account_id(account_str)
                    .map_err(|_| TransactionResult::TemInvalidAccountId)?;
                let account_key = keylet::account(&account_id);

                let account_bytes = sandbox
                    .read(&account_key)
                    .ok_or(TransactionResult::TerNoAccount)?;
                let mut account_obj: Value = serde_json::from_slice(&account_bytes)
                    .map_err(|_| TransactionResult::TefInternal)?;

                let balance = helpers::get_balance(&account_obj);
                if balance < fee_drops {
                    return Err(TransactionResult::TerInsufFee);
                }
                helpers::set_balance(&mut account_obj, balance - fee_drops);

                let updated =
                    serde_json::to_vec(&account_obj).map_err(|_| TransactionResult::TefInternal)?;
                sandbox
                    .update(account_key, updated)
                    .map_err(|_| TransactionResult::TefInternal)?;

                sandbox.destroy_drops(fee_drops);
            }

            // Apply inner transaction
            let mut apply_ctx = ApplyContext {
                tx: inner_tx,
                view: sandbox,
                rules,
                fees,
            };
            let inner_result = transactor.apply(&mut apply_ctx)?;

            // Only TesSuccess is acceptable for atomic batch
            if inner_result != TransactionResult::TesSuccess {
                return Err(inner_result);
            }
        }

        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transactor::Transactor;
    use rxrpl_ledger::Ledger;

    /// A test transactor that always succeeds.
    struct NoopTransactor;

    impl Transactor for NoopTransactor {
        fn preflight(&self, _ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
            Ok(())
        }
        fn preclaim(&self, _ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
            Ok(())
        }
        fn apply(
            &self,
            _ctx: &mut ApplyContext<'_>,
        ) -> Result<TransactionResult, TransactionResult> {
            Ok(TransactionResult::TesSuccess)
        }
    }

    /// A test transactor that fails in preflight.
    struct FailPreflightTransactor;

    impl Transactor for FailPreflightTransactor {
        fn preflight(&self, _ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
            Err(TransactionResult::TemMalformed)
        }
        fn preclaim(&self, _ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
            Ok(())
        }
        fn apply(
            &self,
            _ctx: &mut ApplyContext<'_>,
        ) -> Result<TransactionResult, TransactionResult> {
            unreachable!("should not reach apply");
        }
    }

    /// A test transactor that fails with tec (claimed cost).
    struct TecTransactor;

    impl Transactor for TecTransactor {
        fn preflight(&self, _ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
            Ok(())
        }
        fn preclaim(&self, _ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
            Ok(())
        }
        fn apply(
            &self,
            _ctx: &mut ApplyContext<'_>,
        ) -> Result<TransactionResult, TransactionResult> {
            Err(TransactionResult::TecClaimCost)
        }
    }

    fn test_engine_with(
        tx_type: TransactionType,
        transactor: impl Transactor + 'static,
    ) -> TxEngine {
        let mut registry = TransactorRegistry::new();
        registry.register(tx_type, transactor);
        TxEngine::new_without_sig_check(registry)
    }

    fn make_tx(tx_type: &str) -> Value {
        serde_json::json!({
            "TransactionType": tx_type,
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Fee": "10",
            "Sequence": 1,
        })
    }

    fn setup_ledger_with_account(address: &str, balance: u64) -> Ledger {
        let mut ledger = Ledger::genesis();
        let account_id = decode_account_id(address).unwrap();
        let key = keylet::account(&account_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": address,
            "Balance": balance.to_string(),
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        let json_bytes = serde_json::to_vec(&account).unwrap();
        let data = rxrpl_ledger::sle_codec::encode_sle(&json_bytes).unwrap();
        ledger.put_state(key, data).unwrap();
        ledger
    }

    fn ledger_with_seq_and_ticket(
        address: &str,
        account_seq: u32,
        ticket_seq: Option<u32>,
    ) -> Ledger {
        let mut ledger = Ledger::genesis();
        let account_id = decode_account_id(address).unwrap();
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": address,
            "Balance": "1000000",
            "Sequence": account_seq,
            "OwnerCount": 0,
            "Flags": 0,
        });
        let bytes = serde_json::to_vec(&account).unwrap();
        ledger
            .put_state(
                keylet::account(&account_id),
                rxrpl_ledger::sle_codec::encode_sle(&bytes).unwrap(),
            )
            .unwrap();
        if let Some(tseq) = ticket_seq {
            let ticket = serde_json::json!({
                "LedgerEntryType": "Ticket",
                "Account": address,
                "TicketSequence": tseq,
                "Flags": 0,
            });
            let tbytes = serde_json::to_vec(&ticket).unwrap();
            ledger
                .put_state(
                    keylet::ticket(&account_id, tseq),
                    rxrpl_ledger::sle_codec::encode_sle(&tbytes).unwrap(),
                )
                .unwrap();
        }
        ledger
    }

    fn ticket_tx(address: &str, ticket_seq: u32) -> Value {
        serde_json::json!({
            "TransactionType": "AccountSet",
            "Account": address,
            "Fee": "10",
            "Sequence": 0,
            "TicketSequence": ticket_seq,
        })
    }

    #[test]
    fn check_seq_proxy_accepts_existing_ticket() {
        let addr = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
        let ledger = ledger_with_seq_and_ticket(addr, 5, Some(3));
        let view = LedgerView::with_fees(&ledger, FeeSettings::default());
        assert_eq!(check_seq_proxy(&ticket_tx(addr, 3), &view), Ok(()));
    }

    #[test]
    fn check_seq_proxy_rejects_spent_ticket_with_no_ticket() {
        let addr = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
        // ticket_seq 3 < account seq 5, and no ticket SLE -> already spent.
        let ledger = ledger_with_seq_and_ticket(addr, 5, None);
        let view = LedgerView::with_fees(&ledger, FeeSettings::default());
        assert_eq!(
            check_seq_proxy(&ticket_tx(addr, 3), &view),
            Err(TransactionResult::TefNoTicket)
        );
    }

    #[test]
    fn check_seq_proxy_defers_future_ticket_with_pre_ticket() {
        let addr = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
        // ticket_seq 10 >= account seq 5, not yet created -> retryable.
        let ledger = ledger_with_seq_and_ticket(addr, 5, None);
        let view = LedgerView::with_fees(&ledger, FeeSettings::default());
        assert_eq!(
            check_seq_proxy(&ticket_tx(addr, 10), &view),
            Err(TransactionResult::TerPreTicket)
        );
    }

    #[test]
    fn engine_noop_success() {
        let engine = test_engine_with(TransactionType::AccountSet, NoopTransactor);
        let mut ledger = setup_ledger_with_account("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", 1_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let tx = make_tx("AccountSet");

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);
    }

    #[test]
    fn engine_preflight_failure() {
        let engine = test_engine_with(TransactionType::AccountSet, FailPreflightTransactor);
        let mut ledger = Ledger::genesis();
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let tx = make_tx("AccountSet");

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TemMalformed);
    }

    #[test]
    fn engine_unknown_tx_type() {
        let engine = TxEngine::new(TransactorRegistry::new());
        let mut ledger = Ledger::genesis();
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let tx = make_tx("AccountSet");

        assert!(engine.apply(&tx, &mut ledger, &rules, &fees).is_err());
    }

    #[test]
    fn fee_deducted_from_sender() {
        let engine = test_engine_with(TransactionType::AccountSet, NoopTransactor);
        let mut ledger = setup_ledger_with_account("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", 1_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let tx = make_tx("AccountSet");

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify fee was deducted
        let account_id = decode_account_id("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh").unwrap();
        let key = keylet::account(&account_id);
        let data = ledger.get_state(&key).unwrap();
        let account: Value = rxrpl_ledger::sle_codec::decode_state(data).unwrap();
        assert_eq!(account["Balance"].as_str().unwrap(), "999990");
    }

    #[test]
    fn tec_keeps_fee_discards_mutations() {
        let engine = test_engine_with(TransactionType::AccountSet, TecTransactor);
        let mut ledger = setup_ledger_with_account("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", 1_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let tx = make_tx("AccountSet");

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TecClaimCost);

        // Fee should still be deducted
        let account_id = decode_account_id("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh").unwrap();
        let key = keylet::account(&account_id);
        let data = ledger.get_state(&key).unwrap();
        let account: Value = rxrpl_ledger::sle_codec::decode_state(data).unwrap();
        assert_eq!(account["Balance"].as_str().unwrap(), "999990");
    }

    #[test]
    fn insufficient_fee_balance() {
        let engine = test_engine_with(TransactionType::AccountSet, NoopTransactor);
        let mut ledger = setup_ledger_with_account("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", 5);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let tx = make_tx("AccountSet");

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TerInsufFee);
    }

    #[test]
    fn tx_recorded_in_tx_map() {
        let engine = test_engine_with(TransactionType::AccountSet, NoopTransactor);
        let mut ledger = setup_ledger_with_account("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", 1_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let tx = make_tx("AccountSet");

        engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();

        // Verify tx was recorded
        let tx_hash = rxrpl_protocol::tx::compute_tx_hash(&tx).unwrap();
        assert!(ledger.tx_map.has(&tx_hash));
    }

    #[test]
    fn signature_verification_rejects_invalid() {
        let mut registry = TransactorRegistry::new();
        registry.register(TransactionType::AccountSet, NoopTransactor);
        // Engine WITH signature verification enabled
        let engine = TxEngine::new(registry);

        let mut ledger = setup_ledger_with_account("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", 1_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        // Unsigned tx -- should fail sig check
        let tx = make_tx("AccountSet");

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TefBadSignature);
    }

    #[test]
    fn signature_verification_accepts_valid() {
        use rxrpl_codec::address::classic::encode_classic_address_from_pubkey;
        use rxrpl_crypto::{KeyPair, KeyType, Seed};

        let seed = Seed::from_passphrase("test_engine_sig");
        let kp = KeyPair::from_seed(&seed, KeyType::Ed25519);
        let sender = encode_classic_address_from_pubkey(kp.public_key.as_bytes());

        let mut registry = TransactorRegistry::new();
        registry.register(TransactionType::AccountSet, NoopTransactor);
        let engine = TxEngine::new(registry);

        let mut ledger = setup_ledger_with_account(&sender, 1_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();

        let tx = serde_json::json!({
            "TransactionType": "AccountSet",
            "Account": sender,
            "Fee": "10",
            "Sequence": 1,
        });
        let signed_tx = rxrpl_protocol::tx::sign(&tx, &kp).unwrap();

        let result = engine
            .apply(&signed_tx, &mut ledger, &rules, &fees)
            .unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);
    }

    // ---- BatchSubmit engine tests ----

    fn batch_engine() -> TxEngine {
        let mut registry = TransactorRegistry::new();
        crate::handlers::register_phase_a(&mut registry);
        crate::handlers::register_batch(&mut registry);
        TxEngine::new_without_sig_check(registry)
    }

    fn setup_two_accounts(addr1: &str, bal1: u64, addr2: &str, bal2: u64) -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, bal) in [(addr1, bal1), (addr2, bal2)] {
            let id = decode_account_id(addr).unwrap();
            let key = keylet::account(&id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": bal.to_string(),
                "Sequence": 1,
                "OwnerCount": 0,
                "Flags": 0,
            });
            let json_bytes = serde_json::to_vec(&account).unwrap();
            let data = rxrpl_ledger::sle_codec::encode_sle(&json_bytes).unwrap();
            ledger.put_state(key, data).unwrap();
        }
        ledger
    }

    const GENESIS: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const DEST: &str = "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe";

    #[test]
    fn batch_submit_single_payment() {
        let engine = batch_engine();
        let mut ledger = setup_two_accounts(GENESIS, 1_000_000_000, DEST, 10_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();

        let tx = serde_json::json!({
            "TransactionType": "BatchSubmit",
            "Account": GENESIS,
            "Fee": "10",
            "Sequence": 1,
            "RawTransactions": [{
                "RawTransaction": {
                    "InnerTx": {
                        "TransactionType": "Payment",
                        "Account": GENESIS,
                        "Destination": DEST,
                        "Amount": "5000000",
                        "Fee": "12",
                        "Sequence": 1,
                    }
                }
            }],
        });

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Check balances: genesis lost outer fee (10) + inner fee (12) + payment (5M)
        let gid = decode_account_id(GENESIS).unwrap();
        let gdata = ledger.get_state(&keylet::account(&gid)).unwrap();
        let gobj: Value = rxrpl_ledger::sle_codec::decode_state(gdata).unwrap();
        let genesis_balance: u64 = gobj["Balance"].as_str().unwrap().parse().unwrap();
        assert_eq!(genesis_balance, 1_000_000_000 - 10 - 12 - 5_000_000);

        // Dest gained 5M
        let did = decode_account_id(DEST).unwrap();
        let ddata = ledger.get_state(&keylet::account(&did)).unwrap();
        let dobj: Value = rxrpl_ledger::sle_codec::decode_state(ddata).unwrap();
        let dest_balance: u64 = dobj["Balance"].as_str().unwrap().parse().unwrap();
        assert_eq!(dest_balance, 10_000_000 + 5_000_000);
    }

    #[test]
    fn batch_submit_multiple_payments() {
        let engine = batch_engine();
        let mut ledger = setup_two_accounts(GENESIS, 1_000_000_000, DEST, 10_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();

        let tx = serde_json::json!({
            "TransactionType": "BatchSubmit",
            "Account": GENESIS,
            "Fee": "20",
            "Sequence": 1,
            "RawTransactions": [
                {
                    "RawTransaction": {
                        "InnerTx": {
                            "TransactionType": "Payment",
                            "Account": GENESIS,
                            "Destination": DEST,
                            "Amount": "1000000",
                            "Fee": "12",
                            "Sequence": 1,
                        }
                    }
                },
                {
                    "RawTransaction": {
                        "InnerTx": {
                            "TransactionType": "Payment",
                            "Account": GENESIS,
                            "Destination": DEST,
                            "Amount": "2000000",
                            "Fee": "12",
                            "Sequence": 2,
                        }
                    }
                },
            ],
        });

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Genesis: -20 (outer fee) -12 -1M -12 -2M
        let gid = decode_account_id(GENESIS).unwrap();
        let gdata = ledger.get_state(&keylet::account(&gid)).unwrap();
        let gobj: Value = rxrpl_ledger::sle_codec::decode_state(gdata).unwrap();
        let genesis_balance: u64 = gobj["Balance"].as_str().unwrap().parse().unwrap();
        assert_eq!(
            genesis_balance,
            1_000_000_000 - 20 - 12 - 1_000_000 - 12 - 2_000_000
        );

        // Dest: +1M +2M
        let did = decode_account_id(DEST).unwrap();
        let ddata = ledger.get_state(&keylet::account(&did)).unwrap();
        let dobj: Value = rxrpl_ledger::sle_codec::decode_state(ddata).unwrap();
        let dest_balance: u64 = dobj["Balance"].as_str().unwrap().parse().unwrap();
        assert_eq!(dest_balance, 10_000_000 + 1_000_000 + 2_000_000);
    }

    #[test]
    fn batch_submit_inner_failure_rolls_back() {
        let engine = batch_engine();
        // Give dest only 1 XRP so it exists but second payment from dest will fail
        let mut ledger = setup_two_accounts(GENESIS, 1_000_000_000, DEST, 1_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();

        let tx = serde_json::json!({
            "TransactionType": "BatchSubmit",
            "Account": GENESIS,
            "Fee": "20",
            "Sequence": 1,
            "RawTransactions": [
                {
                    "RawTransaction": {
                        "InnerTx": {
                            "TransactionType": "Payment",
                            "Account": GENESIS,
                            "Destination": DEST,
                            "Amount": "5000000",
                            "Fee": "12",
                            "Sequence": 1,
                        }
                    }
                },
                {
                    "RawTransaction": {
                        "InnerTx": {
                            "TransactionType": "Payment",
                            "Account": DEST,
                            "Destination": GENESIS,
                            "Amount": "999999999999",
                            "Fee": "12",
                            "Sequence": 1,
                        }
                    }
                },
            ],
        });

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        // Inner tx failure => batch fails, but outer fee is still claimed (tec)
        assert!(result != TransactionResult::TesSuccess);

        // Genesis balance: only outer fee deducted (child sandbox rolled back)
        let gid = decode_account_id(GENESIS).unwrap();
        let gdata = ledger.get_state(&keylet::account(&gid)).unwrap();
        let gobj: Value = rxrpl_ledger::sle_codec::decode_state(gdata).unwrap();
        let genesis_balance: u64 = gobj["Balance"].as_str().unwrap().parse().unwrap();
        assert_eq!(genesis_balance, 1_000_000_000 - 20);
    }

    #[test]
    fn batch_submit_empty_rejected() {
        let engine = batch_engine();
        let mut ledger = setup_ledger_with_account(GENESIS, 1_000_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();

        let tx = serde_json::json!({
            "TransactionType": "BatchSubmit",
            "Account": GENESIS,
            "Fee": "10",
            "RawTransactions": [],
        });

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TemMalformed);
    }

    // ---- Sequence check tests (rippled checkSeqProxy parity) ----

    fn payment_engine() -> TxEngine {
        let mut registry = TransactorRegistry::new();
        crate::handlers::register_phase_a(&mut registry);
        TxEngine::new_without_sig_check(registry)
    }

    fn make_payment(account: &str, dest: &str, amount: &str, sequence: u32) -> Value {
        serde_json::json!({
            "TransactionType": "Payment",
            "Account": account,
            "Destination": dest,
            "Amount": amount,
            "Fee": "10",
            "Sequence": sequence,
        })
    }

    #[test]
    fn payment_with_past_sequence_returns_tef_past_seq() {
        let engine = payment_engine();
        let mut ledger = setup_two_accounts(GENESIS, 1_000_000_000, DEST, 10_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();

        let tx = make_payment(GENESIS, DEST, "5000000", 0);

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TefPastSeq);
    }

    #[test]
    fn payment_with_future_sequence_returns_ter_pre_seq() {
        let engine = payment_engine();
        let mut ledger = setup_two_accounts(GENESIS, 1_000_000_000, DEST, 10_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();

        let tx = make_payment(GENESIS, DEST, "5000000", 5);

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TerPreSeq);
    }

    #[test]
    fn payment_with_correct_sequence_increments_account_seq() {
        let engine = payment_engine();
        let mut ledger = setup_two_accounts(GENESIS, 1_000_000_000, DEST, 10_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();

        let tx = make_payment(GENESIS, DEST, "5000000", 1);

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let gid = decode_account_id(GENESIS).unwrap();
        let gdata = ledger.get_state(&keylet::account(&gid)).unwrap();
        let gobj: Value = rxrpl_ledger::sle_codec::decode_state(gdata).unwrap();
        assert_eq!(gobj["Sequence"].as_u64().unwrap(), 2);
    }

    #[test]
    fn payment_replay_after_apply_is_rejected_as_past_seq() {
        let engine = payment_engine();
        let mut ledger = setup_two_accounts(GENESIS, 1_000_000_000, DEST, 10_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();

        let tx = make_payment(GENESIS, DEST, "50_000_000".replace('_', "").as_str(), 1);

        let first = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(first, TransactionResult::TesSuccess);

        let second = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(second, TransactionResult::TefPastSeq);

        // Destination balance must reflect a single application only.
        let did = decode_account_id(DEST).unwrap();
        let ddata = ledger.get_state(&keylet::account(&did)).unwrap();
        let dobj: Value = rxrpl_ledger::sle_codec::decode_state(ddata).unwrap();
        let dest_balance: u64 = dobj["Balance"].as_str().unwrap().parse().unwrap();
        assert_eq!(dest_balance, 10_000_000 + 50_000_000);
    }

    #[test]
    fn account_set_sequence_check_applies_to_non_payment_tx() {
        let engine = test_engine_with(TransactionType::AccountSet, NoopTransactor);
        let mut ledger = setup_ledger_with_account("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", 1_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();

        let mut tx = make_tx("AccountSet");
        tx["Sequence"] = Value::from(99u64);

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TerPreSeq);
    }
}
