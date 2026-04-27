use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use rxrpl_primitives::Hash256;

use crate::adapter::ConsensusAdapter;
use crate::close_resolution::{next_resolution, AdaptiveCloseTime};
use crate::error::ConsensusError;
use crate::negative_unl::{NegativeUnlChange, NegativeUnlTracker};
use crate::params::ConsensusParams;
use crate::phase::ConsensusPhase;
use crate::proposal_tracker::ProposalTracker;
use crate::types::{DisputedTx, NodeId, Proposal, TxSet, Validation};
use crate::unl::TrustedValidatorList;
use crate::validations_trie::ValidationsTrie;

/// Threshold percentage of trusted validators referencing a different
/// `prev_ledger` before we consider switching chains.
const WRONG_PREV_LEDGER_THRESHOLD: u32 = 60;

/// Maximum permitted skew (in seconds) between the local ripple time and
/// the `close_time` of an incoming peer proposal. Mirrors rippled's
/// `propRELAY_INTERVAL` (xrpld/consensus/Consensus.h): proposals older or
/// further in the future than this are dropped as stale and never reach
/// the position aggregation pipeline.
const PROPOSAL_FRESHNESS_SECS: u32 = 30;

/// Seconds between the Unix epoch (1970-01-01) and the Ripple epoch
/// (2000-01-01), used to convert wall-clock time to ripple time.
const RIPPLE_EPOCH_OFFSET_SECS: u64 = 946_684_800;

/// Hard cap on the number of proposals buffered in `pending_proposals`
/// while we are outside `Establish` phase. Without this cap a peer can
/// flood the Open phase and exhaust memory (audit pass 2 C2). 1024 is
/// generous: it covers a full UNL of distinct trusted peers each sending
/// a handful of proposals per round, while bounding the worst-case Vec
/// growth to ~O(1024 * sizeof(Proposal)).
const PENDING_PROPOSALS_MAX: usize = 1024;

/// Result of checking whether trusted peers disagree on prev_ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrongPrevLedgerDetected {
    /// The prev_ledger hash that the supermajority of trusted peers reference.
    pub preferred_ledger: Hash256,
    /// How many trusted peers reference that ledger.
    pub peer_count: usize,
    /// Total trusted peers that sent proposals (with any prev_ledger).
    pub total_trusted: usize,
}

/// The RPCA (Ripple Protocol Consensus Algorithm) engine.
///
/// Implements the consensus state machine:
/// - Open: collect transactions
/// - Establish: converge on a transaction set
/// - Accepted: ledger accepted, transition to next round
pub struct ConsensusEngine<A: ConsensusAdapter> {
    adapter: A,
    params: ConsensusParams,
    phase: ConsensusPhase,
    /// Our current proposal.
    our_position: Option<Proposal>,
    /// Our current transaction set.
    our_set: Option<TxSet>,
    /// Proposals from other validators.
    peer_positions: HashMap<NodeId, Proposal>,
    /// Authoritative dedup layer for incoming peer proposals, keyed by
    /// `(NodeId, prev_ledger)`. Mirrors goXRPL `ProposalTracker`: a peer's
    /// stored entry is only replaced when the new `prop_seq` is strictly
    /// greater. `peer_positions` is kept in lockstep so existing engine
    /// APIs (effective close-time, dispute aggregation, wrong-prev-ledger
    /// detection) continue to read from a single position per peer.
    proposal_tracker: ProposalTracker,
    /// Disputed transactions (tx_hash -> dispute).
    disputes: HashMap<Hash256, DisputedTx>,
    /// Current consensus round.
    round: u32,
    /// Previous ledger hash.
    prev_ledger: Hash256,
    /// Our node ID.
    node_id: NodeId,
    /// Our raw public key bytes.
    public_key: Vec<u8>,
    /// Accepted close time after negotiation.
    accepted_close_time: Option<u32>,
    /// Close flags (1 = peers disagree on close time).
    accepted_close_flags: u8,
    /// Accepted validation after consensus.
    accepted_validation: Option<Validation>,
    /// Trusted validator list (UNL). Empty = solo mode.
    unl: TrustedValidatorList,
    /// Negative UNL tracker for automatic demotion/re-enable.
    negative_unl_tracker: NegativeUnlTracker,
    /// Proposals received while not in Establish phase, replayed on close.
    pending_proposals: Vec<Proposal>,
    /// Tracks proposals from trusted peers that reference a different
    /// `prev_ledger` than ours. Keyed by (node_id -> their prev_ledger).
    /// Used to detect when we are on the wrong chain.
    ///
    /// Secondary signal: this captures pre-quorum disagreement visible in
    /// proposal traffic for the *current* round. The primary signal is
    /// [`Self::validations_trie`], which aggregates trusted validations
    /// across rounds and is consulted first by
    /// [`Self::check_wrong_prev_ledger`].
    wrong_prev_ledger_votes: HashMap<NodeId, Hash256>,
    /// Aggregator over trusted validators' latest validations. The
    /// preferred-branch tip is the primary input to wrong-prev-ledger
    /// detection: when [`ValidationsTrie::get_preferred`] returns a hash
    /// that differs from `self.prev_ledger` and trusted support meets the
    /// [`WRONG_PREV_LEDGER_THRESHOLD`], we abandon the current chain.
    validations_trie: ValidationsTrie,
    /// Sequence number of the current `prev_ledger`. Tracked so that the
    /// validations-trie preferred-branch query can be anchored to the
    /// caller-visible sequence (rippled `getPreferred(largestIssued)`).
    /// Set by [`Self::start_round_with_prior`] to `ledger_seq - 1`.
    prev_ledger_seq: u32,
    /// Holding pen for proposals whose `prev_ledger` we do not yet know.
    /// Keyed by their `prev_ledger` hash. When `start_round` is called with
    /// a matching prev_ledger, the held proposals are moved into
    /// `pending_proposals` for replay.
    ///
    /// Capped at FUTURE_PROPOSALS_MAX_KEYS distinct prev_ledger keys to
    /// prevent a malicious peer from exhausting memory by spamming
    /// proposals with random hashes. Also evicted in `start_round` for any
    /// entry whose `ledger_seq` is more than FUTURE_PROPOSALS_STALE_LEDGERS
    /// behind the current seq (rxrpl can't catch up that far in one
    /// round).
    future_proposals: HashMap<Hash256, Vec<Proposal>>,
    /// Adaptive close-time resolution tracker.
    adaptive_close_time: AdaptiveCloseTime,
    /// Did the previous round agree on close time (within the
    /// then-current resolution)?  Initialised to `true` so the very
    /// first round behaves like rippled's `previousAgree=true` boot
    /// state — no spurious widening on startup.  Updated in
    /// [`Self::accept`] and consumed in [`Self::start_round`] to
    /// drive the rippled `getNextLedgerTimeResolution` cadence.
    previous_close_agreed: bool,
    /// Parent ledger's close time, used to clamp the effective close
    /// time of the current round to be strictly greater (rippled
    /// `effCloseTime` monotonicity guarantee).  Set per round via
    /// [`Self::start_round_with_prior`]; legacy [`Self::start_round`]
    /// callers leave it at `0`, which keeps the clamp inactive for
    /// any rounded close time > 1 (backwards compatible).
    prior_close_time: u32,
    /// Counter: incoming proposals dropped because their `close_time`
    /// drifted more than [`PROPOSAL_FRESHNESS_SECS`] from the local
    /// ripple time. Exposed via [`Self::proposal_dropped_stale_total`].
    proposal_dropped_stale_total: AtomicU64,
}

/// Maximum number of distinct `prev_ledger` hashes held in
/// `future_proposals`. Beyond this, the oldest entry by insertion order is
/// dropped to bound memory under adversarial spam.
const FUTURE_PROPOSALS_MAX_KEYS: usize = 64;
/// Maximum number of proposals held per `prev_ledger` key. A trusted peer
/// would not send more than one position per round, so a small cap
/// suffices and keeps the worst case bounded.
const FUTURE_PROPOSALS_MAX_PER_KEY: usize = 16;
/// Drop a held proposal when its `ledger_seq` is more than this many seqs
/// behind the current round. rxrpl can't catch up that far in time so the
/// proposal is no longer actionable.
const FUTURE_PROPOSALS_STALE_LEDGERS: u32 = 5;

impl<A: ConsensusAdapter> ConsensusEngine<A> {
    pub fn new(adapter: A, node_id: NodeId, params: ConsensusParams) -> Self {
        Self::new_with_unl(adapter, node_id, Vec::new(), params, TrustedValidatorList::empty())
    }

    pub fn new_with_unl(
        adapter: A,
        node_id: NodeId,
        public_key: Vec<u8>,
        params: ConsensusParams,
        unl: TrustedValidatorList,
    ) -> Self {
        let adaptive_close_time = AdaptiveCloseTime::new(params.close_time_resolution);
        // Seed the validations-trie trusted set from the UNL so that
        // record_trusted_validation() works out of the box for any node
        // already in the supplied trusted set.
        let mut validations_trie = ValidationsTrie::new();
        for n in unl.trusted_set() {
            validations_trie.add_trusted(*n);
        }
        Self {
            adapter,
            params,
            phase: ConsensusPhase::Open,
            our_position: None,
            our_set: None,
            peer_positions: HashMap::new(),
            proposal_tracker: ProposalTracker::new(),
            disputes: HashMap::new(),
            round: 0,
            prev_ledger: Hash256::ZERO,
            node_id,
            public_key,
            accepted_close_time: None,
            accepted_close_flags: 0,
            accepted_validation: None,
            unl,
            negative_unl_tracker: NegativeUnlTracker::new(),
            pending_proposals: Vec::new(),
            wrong_prev_ledger_votes: HashMap::new(),
            validations_trie,
            prev_ledger_seq: 0,
            future_proposals: HashMap::new(),
            adaptive_close_time,
            previous_close_agreed: true,
            prior_close_time: 0,
            proposal_dropped_stale_total: AtomicU64::new(0),
        }
    }

    /// Get a mutable reference to the adapter (for simulation/testing).
    pub fn adapter_mut(&mut self) -> &mut A {
        &mut self.adapter
    }

    /// Get the UNL.
    pub fn unl(&self) -> &TrustedValidatorList {
        &self.unl
    }

    /// Get a mutable reference to the UNL.
    pub fn unl_mut(&mut self) -> &mut TrustedValidatorList {
        &mut self.unl
    }

    /// Get the negative UNL tracker.
    pub fn negative_unl_tracker(&self) -> &NegativeUnlTracker {
        &self.negative_unl_tracker
    }

    /// Get a mutable reference to the negative UNL tracker.
    pub fn negative_unl_tracker_mut(&mut self) -> &mut NegativeUnlTracker {
        &mut self.negative_unl_tracker
    }

    /// Record a validation from a trusted validator for the current ledger.
    /// Should be called when a validation message is received.
    pub fn record_validation(&mut self, node_id: NodeId) {
        self.negative_unl_tracker.record_validation(node_id);
    }

    /// Notify the tracker that a ledger has closed.
    /// Should be called once per ledger close.
    pub fn on_ledger_close_for_tracker(&mut self) {
        self.negative_unl_tracker.on_ledger_close();
    }

    /// Evaluate the negative UNL at a flag ledger boundary.
    ///
    /// Returns a list of UNLModify changes (disable/re-enable) that should
    /// be emitted as pseudo-transactions. Also synchronizes the local UNL's
    /// negative set to match the tracker's disabled set.
    pub fn evaluate_negative_unl(&mut self, ledger_seq: u32) -> Vec<NegativeUnlChange> {
        if !NegativeUnlTracker::is_flag_ledger(ledger_seq) {
            return Vec::new();
        }

        let trusted = self.unl.trusted_set().clone();
        let changes = self.negative_unl_tracker.evaluate(&trusted, ledger_seq);

        // Synchronize the UNL's negative set with the tracker's disabled set.
        // Add any newly disabled validators.
        for n in self.negative_unl_tracker.disabled_set() {
            if !self.unl.is_in_negative_unl(n) {
                self.unl.add_to_negative_unl(*n);
            }
        }
        // Remove any re-enabled validators.
        let current_nunl: Vec<NodeId> =
            self.unl.negative_unl_set().iter().copied().collect();
        for n in current_nunl {
            if !self.negative_unl_tracker.disabled_set().contains(&n) {
                self.unl.remove_from_negative_unl(&n);
            }
        }

        changes
    }

    /// Get the current consensus phase.
    pub fn phase(&self) -> ConsensusPhase {
        self.phase
    }

    /// Get a reference to our current position.
    pub fn our_position(&self) -> Option<&Proposal> {
        self.our_position.as_ref()
    }

    /// Get a reference to our current transaction set.
    pub fn our_set(&self) -> Option<&TxSet> {
        self.our_set.as_ref()
    }

    /// Get the accepted close time.
    pub fn accepted_close_time(&self) -> Option<u32> {
        self.accepted_close_time
    }

    /// Returns the rounded median of our and peers' close_time, or None if
    /// no peer positions are available. Used by the node layer to converge
    /// on a shared close_time bucket even before formal quorum, so two
    /// independently-clocked validators produce identical closed-ledger
    /// hashes.
    pub fn rounded_close_time(&self) -> Option<u32> {
        if self.peer_positions.is_empty() {
            return None;
        }
        let our_time = self.our_position.as_ref().map(|p| p.close_time)?;
        let mut times: Vec<u32> = vec![our_time];
        for peer in self.peer_positions.values() {
            times.push(peer.close_time);
        }
        times.sort();
        let median = times[times.len() / 2];
        Some(round_close_time(
            median,
            self.adaptive_close_time.resolution(),
        ))
    }

    /// Get the accepted close flags.
    pub fn accepted_close_flags(&self) -> u8 {
        self.accepted_close_flags
    }

    /// Get the accepted validation.
    pub fn accepted_validation(&self) -> Option<&Validation> {
        self.accepted_validation.as_ref()
    }

    /// Get the adaptive close-time resolution tracker.
    pub fn adaptive_close_time(&self) -> &AdaptiveCloseTime {
        &self.adaptive_close_time
    }

    /// Get the current close-time resolution (adaptive).
    pub fn close_time_resolution(&self) -> u32 {
        self.adaptive_close_time.resolution()
    }

    /// Get the current previous ledger hash.
    pub fn prev_ledger(&self) -> Hash256 {
        self.prev_ledger
    }

    /// Get our node ID.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Check whether a supermajority of trusted validators reference a
    /// different `prev_ledger` than ours.
    ///
    /// Two-stage detection:
    ///
    /// 1. **Primary** — consult [`Self::validations_trie`]. If its
    ///    preferred-branch tip differs from `self.prev_ledger` and at
    ///    least [`WRONG_PREV_LEDGER_THRESHOLD`]% of the trusted set has
    ///    validated that alternative, return immediately. This is the
    ///    rippled `getPreferred`-driven path: it sees disagreement that
    ///    has already been signed off in validations, before the next
    ///    round of proposals lands.
    /// 2. **Secondary** — fall back to the proposal-derived
    ///    [`Self::wrong_prev_ledger_votes`] tally. This catches
    ///    disagreement that surfaces in the *current* round's proposal
    ///    traffic before any new validation has been issued.
    ///
    /// Returns `Some(WrongPrevLedgerDetected)` when either path crosses
    /// the threshold. The caller should abort the current round and
    /// switch to the preferred ledger.
    ///
    /// In solo mode (empty UNL) this always returns `None`.
    pub fn check_wrong_prev_ledger(&self) -> Option<WrongPrevLedgerDetected> {
        if self.unl.is_empty() {
            return None;
        }

        // Stage 1: validations-trie preferred branch.
        if let Some(detected) = self.check_wrong_prev_ledger_from_validations() {
            return Some(detected);
        }

        if self.wrong_prev_ledger_votes.is_empty() {
            return None;
        }

        // Count only trusted peers among those who sent mismatched proposals.
        let mut ledger_counts: HashMap<Hash256, usize> = HashMap::new();
        for (node_id, their_prev) in &self.wrong_prev_ledger_votes {
            if self.unl.is_trusted(node_id) {
                *ledger_counts.entry(*their_prev).or_default() += 1;
            }
        }

        if ledger_counts.is_empty() {
            return None;
        }

        // Find the most popular alternative prev_ledger.
        let (preferred, &count) = ledger_counts
            .iter()
            .max_by_key(|&(_, &c)| c)
            .unwrap();

        // Total trusted proposals = those agreeing with us + those disagreeing.
        let agreeing_trusted = self
            .peer_positions
            .values()
            .filter(|p| self.unl.is_trusted(&p.node_id))
            .count();
        let total_trusted = agreeing_trusted + ledger_counts.values().sum::<usize>();

        if total_trusted == 0 {
            return None;
        }

        let pct = (count as u32 * 100) / total_trusted as u32;
        if pct >= WRONG_PREV_LEDGER_THRESHOLD {
            Some(WrongPrevLedgerDetected {
                preferred_ledger: *preferred,
                peer_count: count,
                total_trusted,
            })
        } else {
            None
        }
    }

    /// Validations-trie path of [`Self::check_wrong_prev_ledger`]. Returns
    /// `Some` only when the trie's preferred branch differs from
    /// `self.prev_ledger` AND its tip support meets the
    /// [`WRONG_PREV_LEDGER_THRESHOLD`]% of the trusted set.
    ///
    /// Threshold denominator is `validations_trie.trusted_count()` (the
    /// full UNL as known to the trie), matching rippled's
    /// "fraction of UNL whose latest validation backs the alternative".
    fn check_wrong_prev_ledger_from_validations(&self) -> Option<WrongPrevLedgerDetected> {
        let trusted_total = self.validations_trie.trusted_count();
        if trusted_total == 0 {
            return None;
        }
        let preferred = self.validations_trie.get_preferred(self.prev_ledger_seq)?;
        if preferred == self.prev_ledger {
            return None;
        }
        let support = self.validations_trie.count_for(&preferred);
        let pct = (support * 100) / trusted_total as u32;
        if pct >= WRONG_PREV_LEDGER_THRESHOLD {
            Some(WrongPrevLedgerDetected {
                preferred_ledger: preferred,
                peer_count: support as usize,
                total_trusted: trusted_total,
            })
        } else {
            None
        }
    }

    /// Record a trusted validator's latest validation in the
    /// validations-trie aggregator. Returns `true` when the call
    /// materially changed trie state (new vote, or a switched ledger).
    /// Untrusted validators are ignored — call
    /// [`Self::add_trusted_validator`] first to enrol them.
    ///
    /// # Audit pass 2 C3 — caller MUST pre-verify the signature
    ///
    /// `rxrpl-consensus` does not depend on `rxrpl-overlay` (where the
    /// canonical signature verifier lives), so this method cannot itself
    /// run `verify_validation_signature`. Callers MUST verify the
    /// validation's signature against `validation.public_key` BEFORE
    /// invoking this entry point. Feeding an unverified validation here
    /// lets an attacker drive the 60% wrong-prev-ledger detector with
    /// forged votes.
    ///
    /// To prevent key laundering (a caller submits a validation whose
    /// `node_id` does not derive from the supplied `public_key`), this
    /// method enforces `node_id == sha512_half(public_key)` and rejects
    /// the call otherwise. An empty `public_key` also fails this check,
    /// so locally-constructed validations missing a key cannot bypass
    /// the binding.
    #[doc(hidden)]
    pub fn record_trusted_validation(&mut self, validation: Validation) -> bool {
        // C3: bind node_id to public_key — prevents a caller from
        // submitting a validation whose node_id is unrelated to the key
        // that (allegedly) signed it.
        let derived_node_id = NodeId(rxrpl_crypto::sha512_half::sha512_half(&[
            validation.public_key.as_slice(),
        ]));
        if validation.node_id != derived_node_id {
            tracing::warn!(
                target: "consensus",
                "record_trusted_validation_node_id_mismatch"
            );
            return false;
        }
        self.validations_trie.add(validation)
    }

    /// Read-only view of the validations-trie aggregator. Exposed for
    /// metrics and integration tests.
    pub fn validations_trie(&self) -> &ValidationsTrie {
        &self.validations_trie
    }

    /// Mark `node_id` as a trusted validator for the validations-trie
    /// aggregator, so its future validations contribute to
    /// preferred-branch detection.
    ///
    // NIGHT-SHIFT-REVIEW: T17 — UNL ingestion stays via the existing
    // `unl_mut()` / constructor / manifest pipeline (TrustedValidatorList
    // exposes no `add_trusted(NodeId)` setter, and the T17 whitelist
    // covers only `engine.rs`). Callers that build the UNL by NodeId
    // outside that pipeline must enrol the same node into the trie via
    // this method. Unifying both sets when the UNL gains a setter is a
    // follow-up.
    pub fn add_trusted_validator(&mut self, node_id: NodeId) {
        self.validations_trie.add_trusted(node_id);
    }

    /// Remove `node_id` from the validations-trie trusted set. Any prior
    /// validation contribution is decremented out of the trie
    /// immediately. UNL membership is untouched (see paired
    /// [`Self::add_trusted_validator`] note).
    pub fn remove_trusted_validator(&mut self, node_id: &NodeId) {
        self.validations_trie.remove_trusted(node_id);
    }

    /// Get the disputes map.
    pub fn disputes(&self) -> &HashMap<Hash256, DisputedTx> {
        &self.disputes
    }

    /// Start a new consensus round for the next ledger.
    ///
    /// Backwards-compatible entry point: leaves `prior_close_time` at
    /// `0`, which keeps the [`eff_close_time`] monotonicity clamp
    /// inactive for any rounded close time > 1.  Callers that have the
    /// parent ledger's close time available should prefer
    /// [`Self::start_round_with_prior`].
    pub fn start_round(&mut self, prev_ledger: Hash256, ledger_seq: u32) {
        self.start_round_with_prior(prev_ledger, ledger_seq, 0);
    }

    /// Start a new consensus round, supplying the parent ledger's
    /// close time so the effective close time of this round can be
    /// clamped to strictly greater (rippled `effCloseTime` monotonicity
    /// guarantee, see `xrpld/consensus/LedgerTiming.h`).
    pub fn start_round_with_prior(
        &mut self,
        prev_ledger: Hash256,
        ledger_seq: u32,
        prior_close_time: u32,
    ) {
        // Recompute the close-time resolution for THIS round using the
        // rippled `getNextLedgerTimeResolution` cadence: keyed on the
        // new ledger sequence and the prior round's agreement flag, NOT
        // on a count of consecutive agreements.  Mirrors rippled
        // `RCLConsensus::Adaptor::onStartRound` which calls
        // `getNextLedgerTimeResolution(parent.closeTimeResolution,
        //  parent.closeAgree, ledgerSeq)` before each round.
        let parent_resolution = self.adaptive_close_time.resolution();
        let new_resolution =
            next_resolution(parent_resolution, self.previous_close_agreed, ledger_seq);
        self.adaptive_close_time.set_resolution(new_resolution);

        self.phase = ConsensusPhase::Open;
        self.our_position = None;
        self.our_set = None;
        self.peer_positions.clear();
        // Drop tracker entries anchored on the previous round's prev_ledger.
        // Entries for the new prev_ledger (e.g. proposals replayed from
        // future_proposals) survive so their dedup state carries over.
        self.proposal_tracker.clear_for(&self.prev_ledger);
        self.disputes.clear();
        self.round = 0;
        self.prev_ledger = prev_ledger;
        // The new round produces ledger `ledger_seq`; its parent
        // (prev_ledger) therefore lives at `ledger_seq - 1`. Anchor the
        // validations-trie preferred-branch query to that sequence.
        self.prev_ledger_seq = ledger_seq.saturating_sub(1);
        self.prior_close_time = prior_close_time;
        self.accepted_close_time = None;
        self.accepted_close_flags = 0;
        self.accepted_validation = None;
        self.wrong_prev_ledger_votes.clear();
        // Move any held proposals matching this prev_ledger into the pending
        // buffer so they get replayed after close_ledger().
        if let Some(matching) = self.future_proposals.remove(&prev_ledger) {
            tracing::debug!(
                "replaying {} held proposal(s) matching new prev_ledger {}",
                matching.len(),
                prev_ledger
            );
            self.pending_proposals.extend(matching);
        }
        // Drop held proposals that are too far behind to still be useful.
        self.future_proposals.retain(|_, props| {
            props.retain(|p| p.ledger_seq + FUTURE_PROPOSALS_STALE_LEDGERS >= ledger_seq);
            !props.is_empty()
        });
        // Note: pending_proposals is NOT cleared here -- they will be
        // replayed in close_ledger() which follows start_round().
    }

    /// Insert a proposal into the holding pen, applying per-key and global
    /// caps. Oldest entries are dropped when caps are exceeded.
    fn hold_future_proposal(&mut self, proposal: Proposal) {
        let key = proposal.prev_ledger;
        let entry = self.future_proposals.entry(key).or_default();
        // Per-key cap: drop the oldest if at capacity.
        if entry.len() >= FUTURE_PROPOSALS_MAX_PER_KEY {
            entry.remove(0);
        }
        // Replace any existing entry from the same node (a peer can only
        // hold one position per round).
        entry.retain(|p| p.node_id != proposal.node_id);
        entry.push(proposal);
        // Global cap: if too many distinct prev_ledger keys, drop the
        // smallest-by-min-seq group (oldest unfulfilled).
        if self.future_proposals.len() > FUTURE_PROPOSALS_MAX_KEYS {
            if let Some(victim) = self
                .future_proposals
                .iter()
                .min_by_key(|(_, ps)| ps.iter().map(|p| p.ledger_seq).min().unwrap_or(u32::MAX))
                .map(|(k, _)| *k)
            {
                self.future_proposals.remove(&victim);
            }
        }
    }

    /// Close the open phase and begin establishing consensus.
    ///
    /// `our_set` is our proposed transaction set.
    pub fn close_ledger(
        &mut self,
        our_set: TxSet,
        close_time: u32,
        ledger_seq: u32,
    ) -> Result<(), ConsensusError> {
        if self.phase != ConsensusPhase::Open {
            return Err(ConsensusError::WrongPhase {
                expected: "open".into(),
                actual: self.phase.to_string(),
            });
        }

        let proposal = Proposal {
            node_id: self.node_id,
            public_key: self.public_key.clone(),
            tx_set_hash: our_set.hash,
            close_time,
            prop_seq: 0,
            ledger_seq,
            prev_ledger: self.prev_ledger,
            signature: None,
        };

        self.adapter.propose(&proposal);
        self.our_position = Some(proposal);
        self.our_set = Some(our_set);
        self.phase = ConsensusPhase::Establish;

        // Replay any proposals buffered while we were in Open phase.
        // Use our own `close_time` as the freshness anchor so that a
        // burst of proposals queued before phase=Establish are evaluated
        // against the round's local time instead of wall-clock drift,
        // matching the close-time bucket aggregation that follows.
        let pending = std::mem::take(&mut self.pending_proposals);
        for p in pending {
            self.peer_proposal_at(p, close_time);
        }

        Ok(())
    }

    /// Total number of incoming proposals dropped because their
    /// `close_time` was more than [`PROPOSAL_FRESHNESS_SECS`] seconds away
    /// from the local ripple time when received. Mirrors rippled's
    /// `propRELAY_INTERVAL` filter, exposed for metrics scraping.
    pub fn proposal_dropped_stale_total(&self) -> u64 {
        self.proposal_dropped_stale_total.load(Ordering::Relaxed)
    }

    /// Compute the current time in ripple-epoch seconds (seconds since
    /// 2000-01-01 UTC). Returns `0` if the system clock is before the
    /// ripple epoch (which would only happen on a misconfigured host).
    fn current_ripple_time() -> u32 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.as_secs().saturating_sub(RIPPLE_EPOCH_OFFSET_SECS)) as u32)
            .unwrap_or(0)
    }

    /// Receive a proposal from a peer.
    ///
    /// Uses the local wall-clock for the freshness check against
    /// [`PROPOSAL_FRESHNESS_SECS`]. For deterministic tests prefer
    /// [`Self::peer_proposal_at`].
    pub fn peer_proposal(&mut self, proposal: Proposal) {
        let now = Self::current_ripple_time();
        self.peer_proposal_at(proposal, now);
    }

    /// **Test-only entry point** — production code MUST call
    /// [`Self::peer_proposal`].
    ///
    /// Exposes the freshness-anchor seam to tests by accepting an explicit
    /// `now` (ripple-epoch seconds) so the freshness check is deterministic.
    /// Downstream callers must NOT supply an attacker-controlled `now` value,
    /// since doing so would bypass the [`PROPOSAL_FRESHNESS_SECS`] gate that
    /// [`Self::peer_proposal`] anchors to the local wall-clock.
    ///
    /// Kept `pub` (rather than `pub(crate)`) only because the
    /// `crates/consensus/tests/multi_node.rs` integration test needs a
    /// deterministic clock; hidden from rustdoc to discourage external use.
    #[doc(hidden)]
    pub fn peer_proposal_at(&mut self, proposal: Proposal, now: u32) {
        if self.phase != ConsensusPhase::Establish {
            // Apply UNL + freshness gates BEFORE buffering to prevent
            // pre-Establish memory amplification (audit pass 2 C2). Without
            // these checks any peer (untrusted, stale, malicious) could
            // flood the Open phase and exhaust memory via unbounded
            // pending_proposals growth.
            if !self.unl.is_empty() && !self.unl.is_trusted(&proposal.node_id) {
                return;
            }
            let delta = now.abs_diff(proposal.close_time);
            if delta > PROPOSAL_FRESHNESS_SECS {
                self.proposal_dropped_stale_total
                    .fetch_add(1, Ordering::Relaxed);
                return;
            }
            if self.pending_proposals.len() >= PENDING_PROPOSALS_MAX {
                tracing::warn!(
                    target: "consensus",
                    cap = PENDING_PROPOSALS_MAX,
                    "pending_proposals cap reached, dropping"
                );
                return;
            }
            self.pending_proposals.push(proposal);
            return;
        }

        // UNL filtering: if UNL is non-empty, only accept trusted nodes
        if !self.unl.is_empty() && !self.unl.is_trusted(&proposal.node_id) {
            tracing::debug!("rejected proposal from untrusted {:?}", proposal.node_id);
            return;
        }

        // Freshness gate (T14): drop proposals whose `close_time` is too
        // far from our local ripple time. Mirrors rippled
        // `propRELAY_INTERVAL` (xrpld/consensus/Consensus.h). Sits before
        // the prev_ledger / future-hold logic so stale proposals never
        // pollute `wrong_prev_ledger_votes` or the holding pen.
        let delta = now.abs_diff(proposal.close_time);
        if delta > PROPOSAL_FRESHNESS_SECS {
            self.proposal_dropped_stale_total
                .fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                target: "consensus",
                public_key = ?proposal.public_key,
                close_time = proposal.close_time,
                now,
                delta,
                "proposal_dropped_stale"
            );
            return;
        }

        // Track proposals referencing a different previous ledger.
        // These are recorded for wrong-prev-ledger detection AND held in
        // `future_proposals` so they can be replayed once we catch up to
        // the matching prev_ledger in `start_round`. Without the hold, a
        // peer running 16-second consensus rounds (rippled) is always at
        // least one ledger ahead of our catchup loop and we never get to
        // participate.
        if proposal.prev_ledger != self.prev_ledger {
            tracing::debug!(
                "holding proposal for future prev_ledger (ours={}, theirs={})",
                self.prev_ledger, proposal.prev_ledger
            );
            self.wrong_prev_ledger_votes
                .insert(proposal.node_id, proposal.prev_ledger);
            self.hold_future_proposal(proposal);
            return;
        }

        // Reject proposals for a different ledger sequence.
        // Peers may transmit `ledger_seq = 0` when the wire encoding does
        // not carry the field (rippled's TMProposeSet only includes
        // `previousledger` and `propose_seq`; `ledger_seq` is inferred from
        // the prev_ledger context). Treat 0 as "unknown" and trust the
        // prev_ledger match we just verified above.
        if let Some(ref our) = self.our_position {
            if proposal.ledger_seq != 0 && proposal.ledger_seq != our.ledger_seq {
                tracing::debug!(
                    "rejected proposal: seq mismatch (ours={}, theirs={})",
                    our.ledger_seq, proposal.ledger_seq
                );
                return;
            }
        }

        // Authoritative dedup: only let proposals through that the
        // (NodeId, prev_ledger)-keyed ProposalTracker accepts as a strict
        // `prop_seq` advance. Duplicates and stale-seq replays are dropped
        // silently here so they never bump dispute counters or rotate the
        // peer's stored position.
        if !self.proposal_tracker.track(proposal.clone()) {
            tracing::debug!(
                "dropped duplicate/older proposal from {:?} seq={} prop_seq={}",
                proposal.node_id, proposal.ledger_seq, proposal.prop_seq
            );
            return;
        }

        tracing::debug!("accepted proposal from {:?} seq={}", proposal.node_id, proposal.ledger_seq);
        let node_id = proposal.node_id;
        self.peer_positions.insert(node_id, proposal);
        self.create_disputes();
    }

    /// Create or update disputes from peer proposals that differ from ours.
    fn create_disputes(&mut self) {
        let our_set = match &self.our_set {
            Some(s) => s,
            None => return,
        };

        for (node_id, peer_prop) in &self.peer_positions {
            if peer_prop.tx_set_hash == our_set.hash {
                // Same set, vote yay on all our txs for this peer
                for tx_hash in &our_set.txs {
                    if let Some(dispute) = self.disputes.get_mut(tx_hash) {
                        dispute.vote(*node_id, true);
                    }
                }
                continue;
            }

            // Try to acquire peer's tx set
            let peer_set = match self.adapter.acquire_tx_set(&peer_prop.tx_set_hash) {
                Some(s) => s,
                None => continue,
            };

            // Find txs in our set but not peer's
            for tx_hash in &our_set.txs {
                if !peer_set.txs.contains(tx_hash) {
                    let dispute = self
                        .disputes
                        .entry(*tx_hash)
                        .or_insert_with(|| DisputedTx::new(*tx_hash, true));
                    dispute.vote(*node_id, false);
                }
            }

            // Find txs in peer's set but not ours
            for tx_hash in &peer_set.txs {
                if !our_set.txs.contains(tx_hash) {
                    let dispute = self
                        .disputes
                        .entry(*tx_hash)
                        .or_insert_with(|| DisputedTx::new(*tx_hash, false));
                    dispute.vote(*node_id, true);
                }
            }

            // Txs in both sets: peer agrees
            for tx_hash in &our_set.txs {
                if peer_set.txs.contains(tx_hash) {
                    if let Some(dispute) = self.disputes.get_mut(tx_hash) {
                        dispute.vote(*node_id, true);
                    }
                }
            }
        }
    }

    /// Compute the effective close time from all proposals.
    ///
    /// Inspired by goXRPL/rippled `RCLConsensus::Adaptor::haveCloseTimeConsensus`.
    /// Each trusted proposal's close_time is rounded to the current
    /// adaptive resolution bucket; the bucket with the most votes wins.
    /// On ties, picks the earlier bucket (deterministic).
    ///
    /// This differs from a plain median: when peers and self have very
    /// different times, the most-popular *bucket* (not the median) is
    /// chosen, which is what rippled does for cross-validator agreement.
    fn effective_close_time(&self) -> u32 {
        let our_time = match &self.our_position {
            Some(p) => p.close_time,
            None => return 0,
        };
        let resolution = self.adaptive_close_time.resolution();
        let prior = self.prior_close_time;

        if self.peer_positions.is_empty() {
            return eff_close_time(our_time, resolution, prior);
        }

        // Tally votes per rounded close-time bucket.
        let mut votes: HashMap<u32, u32> = HashMap::new();
        let our_bucket = eff_close_time(our_time, resolution, prior);
        *votes.entry(our_bucket).or_insert(0) += 1;
        for peer in self.peer_positions.values() {
            let peer_bucket = eff_close_time(peer.close_time, resolution, prior);
            *votes.entry(peer_bucket).or_insert(0) += 1;
        }

        // Pick the bucket with the most votes. On ties, pick the LATER
        // bucket: both validators run the same tiebreak deterministically,
        // and biasing towards "later" matches the natural drift of each
        // validator's clock forward over time, so the chosen bucket is
        // less likely to fall behind prior_close_time + 1s monotonicity.
        let mut best_bucket = our_bucket;
        let mut best_count = 0u32;
        for (bucket, count) in &votes {
            if *count > best_count || (*count == best_count && *bucket > best_bucket) {
                best_bucket = *bucket;
                best_count = *count;
            }
        }
        best_bucket
    }

    /// Update `our_position.close_time` to match the consensus winner
    /// only when a STRICT majority of voters (us + peers) share the
    /// same bucket. This drives cross-validator convergence without
    /// suppressing the disagreement signal used for adaptive resolution
    /// widening: when nodes are split across buckets (e.g. 1-1 in a
    /// 2-validator setup) no realignment fires and the spread/flag
    /// detection in `accept()` still sees the disagreement.
    fn align_close_time_with_peers(&mut self) {
        if self.our_position.is_none() || self.peer_positions.is_empty() {
            return;
        }
        let resolution = self.adaptive_close_time.resolution();
        let prior = self.prior_close_time;
        let our_time = self.our_position.as_ref().map(|p| p.close_time).unwrap_or(0);
        let our_bucket = eff_close_time(our_time, resolution, prior);

        let mut votes: HashMap<u32, u32> = HashMap::new();
        *votes.entry(our_bucket).or_insert(0) += 1;
        for peer in self.peer_positions.values() {
            let b = eff_close_time(peer.close_time, resolution, prior);
            *votes.entry(b).or_insert(0) += 1;
        }
        let total: u32 = votes.values().sum();
        // Find best bucket; tiebreak by latest.
        let mut best_bucket = our_bucket;
        let mut best_count = 0u32;
        for (bucket, count) in &votes {
            if *count > best_count || (*count == best_count && *bucket > best_bucket) {
                best_bucket = *bucket;
                best_count = *count;
            }
        }
        // Strict majority (>50%) before realigning. Below that, leave
        // our_position alone so the disagreement signal survives.
        if best_count * 2 <= total {
            return;
        }
        if let Some(ref mut pos) = self.our_position {
            if pos.close_time != best_bucket {
                tracing::debug!(
                    "consensus: realigning our close_time {} -> {} (majority bucket)",
                    pos.close_time,
                    best_bucket
                );
                pos.close_time = best_bucket;
            }
        }
    }

    /// Run one round of convergence.
    ///
    /// Adjusts our position based on peer proposals and the current threshold.
    /// Returns `true` if consensus has been reached.
    pub fn converge(&mut self) -> bool {
        if self.phase != ConsensusPhase::Establish {
            return false;
        }

        // Realign our close_time with the peer-popular bucket each round.
        // This is what makes cross-validator close_time converge in
        // rippled's RCL — without it, two independently-clocked
        // validators each propose their own close_time forever and the
        // closed-ledger hash never matches the peer's.
        self.align_close_time_with_peers();

        let threshold = self.params.threshold_for_round(self.round);
        // Per-tx dispute resolution uses rippled's avalanche cascade
        // (50/65/70/95) which tightens faster than the linear
        // whole-position agreement threshold above. See
        // [`avalanche_dispute_threshold`].
        let dispute_threshold = avalanche_dispute_threshold(self.round);

        // Resolve disputes and update our set if needed
        let mut set_changed = false;
        if let Some(ref mut our_set) = self.our_set {
            let mut new_txs = our_set.txs.clone();

            for dispute in self.disputes.values() {
                let should_include = dispute.our_vote(dispute_threshold);
                if should_include != dispute.our_vote {
                    set_changed = true;
                    if should_include {
                        // Add tx we didn't have
                        if !new_txs.contains(&dispute.tx_hash) {
                            new_txs.push(dispute.tx_hash);
                        }
                    } else {
                        // Remove tx we had
                        new_txs.retain(|h| h != &dispute.tx_hash);
                    }
                }
            }

            if set_changed {
                *our_set = TxSet::new(new_txs);
                if let Some(ref mut pos) = self.our_position {
                    pos.tx_set_hash = our_set.hash;
                    pos.prop_seq += 1;
                    self.adapter.share_position(pos);
                }
                // Update dispute our_vote to match new reality
                for dispute in self.disputes.values_mut() {
                    let in_set = our_set.txs.contains(&dispute.tx_hash);
                    dispute.our_vote = in_set;
                }
            }
        }

        // Count agreement on our position
        let our_hash = match &self.our_position {
            Some(p) => p.tx_set_hash,
            None => return false,
        };

        if self.unl.is_empty() {
            // Solo mode: percentage-based agreement (original logic)
            let total = self.peer_positions.len() + 1; // +1 for us
            let agreeing = self
                .peer_positions
                .values()
                .filter(|p| p.tx_set_hash == our_hash)
                .count()
                + 1; // +1 for us

            let agreement_pct = if total > 0 {
                (agreeing as u32 * 100) / total as u32
            } else {
                100
            };

            if agreement_pct >= threshold {
                self.accept();
                return true;
            }
        } else {
            // UNL mode: count only trusted members who agree
            let agreeing_unl = self
                .peer_positions
                .values()
                .filter(|p| self.unl.is_trusted(&p.node_id) && p.tx_set_hash == our_hash)
                .count();
            let self_counts = if self.unl.is_trusted(&self.node_id) {
                1
            } else {
                0
            };
            if agreeing_unl + self_counts >= self.unl.quorum_threshold() {
                self.accept();
                return true;
            }
        }

        self.round += 1;

        // If we've exceeded max rounds, accept anyway
        if self.round >= self.params.max_consensus_rounds {
            self.accept();
            return true;
        }

        false
    }

    /// Accept the consensus result: compute close time, create validation,
    /// notify adapter.
    fn accept(&mut self) {
        self.phase = ConsensusPhase::Accepted;

        // Compute effective close time
        let close_time = self.effective_close_time();
        self.accepted_close_time = Some(close_time);

        // Check whether peers agreed on close time.  The result is
        // recorded on `previous_close_agreed` and consumed by the next
        // `start_round`, which feeds it into `next_resolution` to pick
        // the bin for the upcoming ledger.  This replaces the legacy
        // `AdaptiveCloseTime::on_agreement` / `on_disagreement`
        // pathway (deprecated in T03).
        let current_resolution = self.adaptive_close_time.resolution();
        let agreed = if self.peer_positions.is_empty() {
            // Solo mode: no peers to disagree with, treat as agreement
            // so the cadence keeps pushing toward finer bins.
            true
        } else {
            let our_time = self
                .our_position
                .as_ref()
                .map(|p| p.close_time)
                .unwrap_or(0);
            let mut times: Vec<u32> = vec![our_time];
            for peer in self.peer_positions.values() {
                times.push(peer.close_time);
            }
            let min = *times.iter().min().unwrap();
            let max = *times.iter().max().unwrap();
            max - min <= current_resolution
        };
        if !agreed {
            self.accepted_close_flags = 1;
        }
        self.previous_close_agreed = agreed;

        // Ask adapter to apply the tx set and get ledger hash
        let our_set = match &self.our_set {
            Some(s) => s.clone(),
            None => return,
        };

        let ledger_hash =
            self.adapter
                .on_accept_ledger(&our_set, close_time, self.accepted_close_flags);

        let ledger_seq = self
            .our_position
            .as_ref()
            .map(|p| p.ledger_seq)
            .unwrap_or(0);

        // Create validation
        let validation = Validation {
            node_id: self.node_id,
            public_key: self.public_key.clone(),
            ledger_hash,
            ledger_seq,
            full: true,
            close_time,
            sign_time: close_time,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        };

        self.adapter.on_accept(&validation);
        self.accepted_validation = Some(validation);
    }

    /// Get the accepted transaction set hash, if consensus was reached.
    pub fn accepted_set(&self) -> Option<Hash256> {
        if self.phase == ConsensusPhase::Accepted {
            self.our_position.as_ref().map(|p| p.tx_set_hash)
        } else {
            None
        }
    }
}

/// Avalanche dispute threshold (percent) for the given consensus round.
///
/// Mirrors rippled's tightening cascade in
/// `include/xrpl/consensus/Consensus.h` — a peer must clear an
/// increasingly demanding majority for a tx to remain in our position
/// as the round count climbs:
///
/// | round | threshold | rippled constant       |
/// |------:|----------:|------------------------|
/// |   0   |   50%     | `avINIT_CONSENSUS_PCT` |
/// |   1   |   65%     | `avMID_CONSENSUS_PCT`  |
/// |   2   |   70%     | `avLATE_CONSENSUS_PCT` |
/// |  3+   |   95%     | `avSTUCK_CONSENSUS_PCT`|
///
/// This is *separate* from `ConsensusParams::threshold_for_round` which
/// gates whole-position agreement; avalanche thresholds gate per-tx
/// dispute resolution.
pub fn avalanche_dispute_threshold(round: u32) -> u32 {
    match round {
        0 => 50,
        1 => 65,
        2 => 70,
        _ => 95,
    }
}

/// Round a close time to the nearest resolution boundary.
pub fn round_close_time(t: u32, resolution: u32) -> u32 {
    if resolution == 0 {
        return t;
    }
    t.saturating_add(resolution / 2) / resolution * resolution
}

/// Compute the effective close time for a ledger, clamping to strictly
/// greater than `prior_close_time` to enforce monotonicity.
///
/// Mirrors rippled's `effCloseTime` (xrpld/consensus/LedgerTiming.h):
/// - When `close_time == 0`, returns 0 (the "untrusted close time" sentinel
///   used by rippled when a node has no opinion on the close time).
/// - Otherwise, rounds `close_time` to the resolution bucket and clamps
///   the result to be at least `prior_close_time + 1`. The clamp ensures
///   each ledger's close time is strictly greater than its parent's, which
///   downstream code relies on for ordering.
pub fn eff_close_time(close_time: u32, resolution: u32, prior_close_time: u32) -> u32 {
    if close_time == 0 {
        return 0;
    }
    let rounded = round_close_time(close_time, resolution);
    let min_allowed = prior_close_time.saturating_add(1);
    if rounded > min_allowed {
        rounded
    } else {
        min_allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockAdapter {
        tx_sets: HashMap<Hash256, TxSet>,
        accepted_ledger_hash: Hash256,
        shared_positions: Mutex<Vec<Proposal>>,
    }

    impl MockAdapter {
        fn new() -> Self {
            Self {
                tx_sets: HashMap::new(),
                accepted_ledger_hash: Hash256::new([0xAA; 32]),
                shared_positions: Mutex::new(Vec::new()),
            }
        }

        fn with_tx_sets(sets: Vec<TxSet>) -> Self {
            let mut adapter = Self::new();
            for set in sets {
                adapter.tx_sets.insert(set.hash, set);
            }
            adapter
        }
    }

    impl ConsensusAdapter for MockAdapter {
        fn propose(&self, _: &Proposal) {}
        fn share_position(&self, p: &Proposal) {
            self.shared_positions.lock().unwrap().push(p.clone());
        }
        fn share_tx(&self, _: &Hash256, _: &[u8]) {}
        fn acquire_tx_set(&self, hash: &Hash256) -> Option<TxSet> {
            self.tx_sets.get(hash).cloned()
        }
        fn on_close(&self, _: &Hash256, _: u32, _: u32, _: &TxSet) {}
        fn on_accept(&self, _: &Validation) {}
        fn on_accept_ledger(&self, _: &TxSet, _: u32, _: u8) -> Hash256 {
            self.accepted_ledger_hash
        }
    }

    // Simple mock for backward compat
    struct SimpleAdapter;
    impl ConsensusAdapter for SimpleAdapter {
        fn propose(&self, _: &Proposal) {}
        fn share_position(&self, _: &Proposal) {}
        fn share_tx(&self, _: &Hash256, _: &[u8]) {}
        fn acquire_tx_set(&self, _: &Hash256) -> Option<TxSet> {
            None
        }
        fn on_close(&self, _: &Hash256, _: u32, _: u32, _: &TxSet) {}
        fn on_accept(&self, _: &Validation) {}
        fn on_accept_ledger(&self, _: &TxSet, _: u32, _: u8) -> Hash256 {
            Hash256::new([0xAA; 32])
        }
    }

    fn test_engine() -> ConsensusEngine<SimpleAdapter> {
        let node_id = NodeId(Hash256::new([0x01; 32]));
        ConsensusEngine::new(SimpleAdapter, node_id, ConsensusParams::default())
    }

    #[test]
    fn initial_phase_is_open() {
        let engine = test_engine();
        assert_eq!(engine.phase(), ConsensusPhase::Open);
    }

    #[test]
    fn close_ledger_transitions_to_establish() {
        let mut engine = test_engine();
        engine.start_round(Hash256::ZERO, 1);

        let tx_set = TxSet::new(vec![]);
        engine.close_ledger(tx_set, 100, 1).unwrap();
        assert_eq!(engine.phase(), ConsensusPhase::Establish);
    }

    #[test]
    fn solo_consensus() {
        let mut engine = test_engine();
        engine.start_round(Hash256::ZERO, 1);

        let tx_set = TxSet::new(vec![]);
        engine.close_ledger(tx_set, 100, 1).unwrap();

        // With no peers, we have 100% agreement
        assert!(engine.converge());
        assert_eq!(engine.phase(), ConsensusPhase::Accepted);
        assert!(engine.accepted_set().is_some());
    }

    #[test]
    fn close_wrong_phase_fails() {
        let mut engine = test_engine();
        engine.start_round(Hash256::ZERO, 1);

        let tx_set = TxSet::new(vec![]);
        engine.close_ledger(tx_set.clone(), 100, 1).unwrap();

        // Already in establish, can't close again
        assert!(engine.close_ledger(tx_set, 200, 1).is_err());
    }

    // --- H1: Dispute resolution tests ---

    #[test]
    fn dispute_majority_drops_tx() {
        // 3 nodes: A={tx1,tx2}, B={tx1}, C={tx1}
        // A should drop tx2 since only 1/3 include it
        let tx1 = Hash256::new([0x01; 32]);
        let tx2 = Hash256::new([0x02; 32]);

        let set_a = TxSet::new(vec![tx1, tx2]);
        let set_bc = TxSet::new(vec![tx1]);

        let adapter = MockAdapter::with_tx_sets(vec![set_a.clone(), set_bc.clone()]);
        let node_a = NodeId(Hash256::new([0xA0; 32]));
        let node_b = NodeId(Hash256::new([0xB0; 32]));
        let node_c = NodeId(Hash256::new([0xC0; 32]));

        let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());
        engine.start_round(Hash256::ZERO, 1);
        engine.close_ledger(set_a, 100, 1).unwrap();

        // B and C propose set with only tx1
        engine.peer_proposal_at(
            Proposal {
                node_id: node_b,
                public_key: vec![0x02; 33],
                tx_set_hash: set_bc.hash,
                close_time: 100,
                prop_seq: 0,
                ledger_seq: 1,
                prev_ledger: Hash256::ZERO,
                signature: None,
            },
            100,
        );
        engine.peer_proposal_at(
            Proposal {
                node_id: node_c,
                public_key: vec![0x02; 33],
                tx_set_hash: set_bc.hash,
                close_time: 100,
                prop_seq: 0,
                ledger_seq: 1,
                prev_ledger: Hash256::ZERO,
                signature: None,
            },
            100,
        );

        // Converge: threshold round 0 = 50%, tx2 has 1/3 = 33% -> drop
        assert!(engine.converge());

        // Our set should now match B/C's set (only tx1)
        let final_set = engine.our_set().unwrap();
        assert_eq!(final_set.txs.len(), 1);
        assert!(final_set.txs.contains(&tx1));
        assert!(!final_set.txs.contains(&tx2));
    }

    #[test]
    fn dispute_increments_prop_seq() {
        let tx1 = Hash256::new([0x01; 32]);
        let tx2 = Hash256::new([0x02; 32]);

        let set_a = TxSet::new(vec![tx1, tx2]);
        let set_b = TxSet::new(vec![tx1]);

        let adapter = MockAdapter::with_tx_sets(vec![set_a.clone(), set_b.clone()]);
        let node_a = NodeId(Hash256::new([0xA0; 32]));
        let node_b = NodeId(Hash256::new([0xB0; 32]));
        let node_c = NodeId(Hash256::new([0xC0; 32]));

        let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());
        engine.start_round(Hash256::ZERO, 1);
        engine.close_ledger(set_a, 100, 1).unwrap();
        assert_eq!(engine.our_position().unwrap().prop_seq, 0);

        engine.peer_proposal_at(
            Proposal {
                node_id: node_b,
                public_key: vec![0x02; 33],
                tx_set_hash: set_b.hash,
                close_time: 100,
                prop_seq: 0,
                ledger_seq: 1,
                prev_ledger: Hash256::ZERO,
                signature: None,
            },
            100,
        );
        engine.peer_proposal_at(
            Proposal {
                node_id: node_c,
                public_key: vec![0x02; 33],
                tx_set_hash: set_b.hash,
                close_time: 100,
                prop_seq: 0,
                ledger_seq: 1,
                prev_ledger: Hash256::ZERO,
                signature: None,
            },
            100,
        );

        engine.converge();
        // Position changed, so prop_seq should have incremented
        assert_eq!(engine.our_position().unwrap().prop_seq, 1);
    }

    #[test]
    fn no_disputes_when_all_agree() {
        let tx1 = Hash256::new([0x01; 32]);
        let set = TxSet::new(vec![tx1]);

        let adapter = MockAdapter::with_tx_sets(vec![set.clone()]);
        let node_a = NodeId(Hash256::new([0xA0; 32]));
        let node_b = NodeId(Hash256::new([0xB0; 32]));

        let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());
        engine.start_round(Hash256::ZERO, 1);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        engine.peer_proposal_at(
            Proposal {
                node_id: node_b,
                public_key: vec![0x02; 33],
                tx_set_hash: set.hash,
                close_time: 100,
                prop_seq: 0,
                ledger_seq: 1,
                prev_ledger: Hash256::ZERO,
                signature: None,
            },
            100,
        );

        assert!(engine.disputes().is_empty());
        assert!(engine.converge());
    }

    #[test]
    fn unknown_peer_set_no_crash() {
        // Peer proposes a set we can't acquire -> no crash, no disputes
        let tx1 = Hash256::new([0x01; 32]);
        let set = TxSet::new(vec![tx1]);

        let adapter = MockAdapter::new(); // no known sets
        let node_a = NodeId(Hash256::new([0xA0; 32]));
        let node_b = NodeId(Hash256::new([0xB0; 32]));

        let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());
        engine.start_round(Hash256::ZERO, 1);
        engine.close_ledger(set, 100, 1).unwrap();

        engine.peer_proposal_at(
            Proposal {
                node_id: node_b,
                public_key: vec![0x02; 33],
                tx_set_hash: Hash256::new([0xFF; 32]), // unknown
                close_time: 100,
                prop_seq: 0,
                ledger_seq: 1,
                prev_ledger: Hash256::ZERO,
                signature: None,
            },
            100,
        );

        assert!(engine.disputes().is_empty());
    }

    // --- H2: Close time negotiation tests ---

    #[test]
    fn close_time_median_rounding() {
        let adapter = SimpleAdapter;
        let node_a = NodeId(Hash256::new([0xA0; 32]));
        let node_b = NodeId(Hash256::new([0xB0; 32]));
        let node_c = NodeId(Hash256::new([0xC0; 32]));

        let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());
        engine.start_round(Hash256::ZERO, 1);
        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        // Use the proposal's own close_time as the freshness anchor so
        // each peer proposal lands with delta=0 against PROPOSAL_FRESHNESS_SECS.
        engine.peer_proposal_at(
            Proposal {
                node_id: node_b,
                public_key: vec![0x02; 33],
                tx_set_hash: set.hash,
                close_time: 200,
                prop_seq: 0,
                ledger_seq: 1,
                prev_ledger: Hash256::ZERO,
                signature: None,
            },
            200,
        );
        engine.peer_proposal_at(
            Proposal {
                node_id: node_c,
                public_key: vec![0x02; 33],
                tx_set_hash: set.hash,
                close_time: 150,
                prop_seq: 0,
                ledger_seq: 1,
                prev_ledger: Hash256::ZERO,
                signature: None,
            },
            150,
        );

        assert!(engine.converge());
        // Vote-counting: each of {100→90, 150→150, 200→210} has 1 vote.
        // Tiebreak picks the LATEST bucket (210) deterministically so two
        // validators making the same tally agree on the same winner.
        assert_eq!(engine.accepted_close_time(), Some(210));
    }

    #[test]
    fn round_close_time_function() {
        assert_eq!(round_close_time(145, 30), 150);
        assert_eq!(round_close_time(150, 30), 150);
        assert_eq!(round_close_time(130, 30), 120);
        assert_eq!(round_close_time(100, 0), 100);
    }

    #[test]
    fn round_close_time_saturates_near_u32_max() {
        // prior to fix: (u32::MAX-30 + 60) wrapped, returning a tiny number
        let r = round_close_time(u32::MAX - 30, 30);
        assert!(r >= u32::MAX - 60, "expected near u32::MAX, got {}", r);
    }

    #[test]
    fn eff_close_time_zero_passthrough() {
        // close_time == 0 is rippled's "untrusted close time" sentinel and
        // must propagate unchanged regardless of resolution or prior.
        assert_eq!(eff_close_time(0, 30, 0), 0);
        assert_eq!(eff_close_time(0, 30, 12345), 0);
        assert_eq!(eff_close_time(0, 0, 99), 0);
    }

    #[test]
    fn eff_close_time_clamp_active() {
        // round_close_time(100, 30) == 90, but prior+1 == 121, so clamp wins.
        assert_eq!(eff_close_time(100, 30, 120), 121);
        // Rounded equals prior exactly => clamp to prior+1 (strictly greater).
        // round_close_time(150, 30) == 150, prior 150 -> must return 151.
        assert_eq!(eff_close_time(150, 30, 150), 151);
        // Rounded sits below prior by a wide margin.
        assert_eq!(eff_close_time(50, 10, 200), 201);
    }

    #[test]
    fn eff_close_time_clamp_inactive() {
        // round_close_time(150, 30) == 150 > prior+1 == 101 -> rounded wins.
        assert_eq!(eff_close_time(150, 30, 100), 150);
        // round_close_time(145, 30) == 150 > prior+1 == 100 -> rounded wins.
        assert_eq!(eff_close_time(145, 30, 99), 150);
        // prior_close_time == 0 (genesis-like) and rounded > 1.
        assert_eq!(eff_close_time(60, 10, 0), 60);
    }

    #[test]
    fn eff_close_time_resolution_zero() {
        // resolution == 0 makes round_close_time the identity, so eff_close_time
        // reduces to max(close_time, prior+1) for non-zero inputs.
        assert_eq!(eff_close_time(100, 0, 50), 100);
        assert_eq!(eff_close_time(100, 0, 100), 101);
        assert_eq!(eff_close_time(100, 0, 200), 201);
    }

    #[test]
    fn close_time_spread_sets_flag() {
        let adapter = SimpleAdapter;
        let node_a = NodeId(Hash256::new([0xA0; 32]));
        let node_b = NodeId(Hash256::new([0xB0; 32]));

        let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());
        engine.start_round(Hash256::ZERO, 1);
        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        // Peer proposes time far away (spread > 30s resolution).
        // Anchor `now` to the peer's own close_time so the freshness
        // gate doesn't drop the message — this test exercises the
        // spread-flag path, not the freshness path.
        engine.peer_proposal_at(
            Proposal {
                node_id: node_b,
                public_key: vec![0x02; 33],
                tx_set_hash: set.hash,
                close_time: 200,
                prop_seq: 0,
                ledger_seq: 1,
                prev_ledger: Hash256::ZERO,
                signature: None,
            },
            200,
        );

        engine.converge();
        assert_eq!(engine.accepted_close_flags(), 1);
    }

    // --- H5: Accept tests ---

    #[test]
    fn accept_creates_validation() {
        let mut engine = test_engine();
        engine.start_round(Hash256::ZERO, 1);

        let tx_set = TxSet::new(vec![Hash256::new([0x01; 32])]);
        engine.close_ledger(tx_set, 100, 1).unwrap();

        assert!(engine.converge());
        let val = engine.accepted_validation().unwrap();
        assert_eq!(val.ledger_hash, Hash256::new([0xAA; 32]));
        assert_eq!(val.ledger_seq, 1);
        assert!(val.full);
    }

    #[test]
    fn accept_includes_negotiated_close_time() {
        let adapter = SimpleAdapter;
        let node_a = NodeId(Hash256::new([0xA0; 32]));

        let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());
        engine.start_round(Hash256::ZERO, 1);
        let set = TxSet::new(vec![]);
        engine.close_ledger(set, 150, 1).unwrap();

        assert!(engine.converge());
        // Solo: close_time = our time, rounded
        assert_eq!(engine.accepted_close_time(), Some(150));
        let val = engine.accepted_validation().unwrap();
        assert_eq!(val.close_time, 150);
    }

    // --- L1: UNL tests ---

    use crate::unl::TrustedValidatorList;
    use std::collections::HashSet;

    /// Synthetic per-id "public key" for tests. 33 bytes (secp256k1
    /// length), prefix `0x02` so the verifier path treats it as a
    /// secp256k1-shaped key, with the id byte filling the rest. Distinct
    /// per `id` so derived NodeIds don't collide.
    fn test_pk(id: u8) -> Vec<u8> {
        let mut pk = vec![0x02; 33];
        for byte in pk.iter_mut().skip(1) {
            *byte = id;
        }
        pk
    }

    fn node(id: u8) -> NodeId {
        // Derive from the synthetic test public key so that validations
        // built with `validation_for(node(n), ...)` carry a matching
        // (node_id, public_key) pair for the C3 binding check in
        // `record_trusted_validation`.
        NodeId::from_public_key(&test_pk(id))
    }

    fn make_unl(ids: &[u8]) -> TrustedValidatorList {
        let mut trusted = HashSet::new();
        for id in ids {
            trusted.insert(node(*id));
        }
        TrustedValidatorList::new(trusted)
    }

    fn proposal_for(
        node_id: NodeId,
        tx_set_hash: Hash256,
        prev_ledger: Hash256,
        seq: u32,
    ) -> Proposal {
        Proposal {
            node_id,
            public_key: vec![0x02; 33],
            tx_set_hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: seq,
            prev_ledger,
            signature: None,
        }
    }

    #[test]
    fn untrusted_proposal_ignored_with_unl() {
        let unl = make_unl(&[1, 2, 3]);
        let adapter = SimpleAdapter;
        let mut engine =
            ConsensusEngine::new_with_unl(adapter, node(1), Vec::new(), ConsensusParams::default(), unl);
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        // Node 99 is not trusted
        engine.peer_proposal_at(proposal_for(node(99), set.hash, Hash256::ZERO, 1), 100);
        assert!(engine.peer_positions.is_empty());
    }

    #[test]
    fn trusted_proposal_accepted_with_unl() {
        let unl = make_unl(&[1, 2, 3]);
        let adapter = SimpleAdapter;
        let mut engine =
            ConsensusEngine::new_with_unl(adapter, node(1), Vec::new(), ConsensusParams::default(), unl);
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        // Node 2 is trusted
        engine.peer_proposal_at(proposal_for(node(2), set.hash, Hash256::ZERO, 1), 100);
        assert_eq!(engine.peer_positions.len(), 1);
    }

    #[test]
    fn mismatched_prev_ledger_ignored() {
        let unl = make_unl(&[1, 2]);
        let adapter = SimpleAdapter;
        let mut engine =
            ConsensusEngine::new_with_unl(adapter, node(1), Vec::new(), ConsensusParams::default(), unl);
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        // Different prev_ledger
        let bad_prev = Hash256::new([0xFF; 32]);
        engine.peer_proposal_at(proposal_for(node(2), set.hash, bad_prev, 1), 100);
        assert!(engine.peer_positions.is_empty());
    }

    #[test]
    fn unl_quorum_not_met_does_not_accept() {
        // 5-node UNL, quorum = ceil(5*0.8) = 4
        let unl = make_unl(&[1, 2, 3, 4, 5]);
        let adapter = SimpleAdapter;
        let mut engine =
            ConsensusEngine::new_with_unl(adapter, node(1), Vec::new(), ConsensusParams::default(), unl);
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        // Only nodes 2, 3 agree (+ us = 3 total, need 4)
        engine.peer_proposal_at(proposal_for(node(2), set.hash, Hash256::ZERO, 1), 100);
        engine.peer_proposal_at(proposal_for(node(3), set.hash, Hash256::ZERO, 1), 100);

        assert!(!engine.converge());
        assert_eq!(engine.phase(), ConsensusPhase::Establish);
    }

    #[test]
    fn unl_quorum_met_accepts() {
        // 5-node UNL, quorum = 4. Us (node 1) + nodes 2, 3, 4 = 4.
        let unl = make_unl(&[1, 2, 3, 4, 5]);
        let adapter = SimpleAdapter;
        let mut engine =
            ConsensusEngine::new_with_unl(adapter, node(1), Vec::new(), ConsensusParams::default(), unl);
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        engine.peer_proposal_at(proposal_for(node(2), set.hash, Hash256::ZERO, 1), 100);
        engine.peer_proposal_at(proposal_for(node(3), set.hash, Hash256::ZERO, 1), 100);
        engine.peer_proposal_at(proposal_for(node(4), set.hash, Hash256::ZERO, 1), 100);

        assert!(engine.converge());
        assert_eq!(engine.phase(), ConsensusPhase::Accepted);
    }

    #[test]
    fn solo_mode_unchanged_with_empty_unl() {
        // Empty UNL = solo mode, should still converge immediately
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new(adapter, node(1), ConsensusParams::default());
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set, 100, 1).unwrap();

        assert!(engine.converge());
        assert_eq!(engine.phase(), ConsensusPhase::Accepted);
    }

    // --- Future-proposal holding pen tests ---

    #[test]
    fn future_proposal_held_when_prev_ledger_unknown() {
        let unl = make_unl(&[1, 2]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        let future_prev = Hash256::new([0xAA; 32]);
        engine.peer_proposal_at(proposal_for(node(2), set.hash, future_prev, 2), 100);

        // Held, not accepted.
        assert!(engine.peer_positions.is_empty());
        assert_eq!(engine.future_proposals.get(&future_prev).map(|v| v.len()), Some(1));
    }

    #[test]
    fn held_proposal_replayed_on_matching_start_round() {
        let unl = make_unl(&[1, 2]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        let future_prev = Hash256::new([0xBB; 32]);
        engine.peer_proposal_at(proposal_for(node(2), set.hash, future_prev, 2), 100);
        assert!(engine.future_proposals.contains_key(&future_prev));

        // Catch up to the future ledger.
        engine.start_round(future_prev, 2);
        // Held proposal is moved to pending, ready for replay in close_ledger.
        assert!(!engine.future_proposals.contains_key(&future_prev));
        assert_eq!(engine.pending_proposals.len(), 1);

        // Replay path: close_ledger drains pending_proposals.
        let new_set = TxSet::new(vec![]);
        engine.close_ledger(new_set.clone(), 100, 2).unwrap();
        assert_eq!(engine.peer_positions.len(), 1);
        assert!(engine.peer_positions.contains_key(&node(2)));
    }

    #[test]
    fn held_proposal_evicted_when_too_stale() {
        let unl = make_unl(&[1, 2]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        engine.start_round(Hash256::ZERO, 1);
        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        // Hold a stale proposal (seq=2)
        let stale_prev = Hash256::new([0xCC; 32]);
        engine.peer_proposal_at(proposal_for(node(2), set.hash, stale_prev, 2), 100);
        assert!(engine.future_proposals.contains_key(&stale_prev));

        // Jump forward many rounds. Stale proposals get evicted.
        engine.start_round(Hash256::new([0xDD; 32]), 2 + FUTURE_PROPOSALS_STALE_LEDGERS + 1);
        assert!(engine.future_proposals.is_empty());
    }

    #[test]
    fn hold_dedups_per_node() {
        let unl = make_unl(&[1, 2]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        engine.start_round(Hash256::ZERO, 1);
        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        let prev = Hash256::new([0xEE; 32]);
        engine.peer_proposal_at(proposal_for(node(2), set.hash, prev, 2), 100);
        engine.peer_proposal_at(proposal_for(node(2), set.hash, prev, 2), 100);
        engine.peer_proposal_at(proposal_for(node(2), set.hash, prev, 2), 100);
        // Same node, same key: only the latest is kept.
        assert_eq!(engine.future_proposals.get(&prev).map(|v| v.len()), Some(1));
    }

    #[test]
    fn hold_caps_global_keys() {
        let unl = make_unl(&[1, 2]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        engine.start_round(Hash256::ZERO, 1);
        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        // Push proposals with FUTURE_PROPOSALS_MAX_KEYS + 5 distinct prev_ledger
        // keys. The map should never grow past the cap.
        for i in 0u8..(FUTURE_PROPOSALS_MAX_KEYS as u8 + 5) {
            let mut h = [0u8; 32];
            h[0] = i.wrapping_add(1);
            let p = proposal_for(node(2), set.hash, Hash256::new(h), 100 + i as u32);
            // proposal_for produces close_time=100; anchor `now` accordingly.
            engine.peer_proposal_at(p, 100);
        }
        assert!(engine.future_proposals.len() <= FUTURE_PROPOSALS_MAX_KEYS);
    }

    // --- Wrong prev_ledger recovery tests ---

    #[test]
    fn wrong_prev_ledger_not_detected_in_solo_mode() {
        // Solo mode (empty UNL) should never trigger wrong prev_ledger.
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new(adapter, node(1), ConsensusParams::default());
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        let bad_prev = Hash256::new([0xFF; 32]);
        engine.peer_proposal_at(proposal_for(node(2), set.hash, bad_prev, 1), 100);

        assert_eq!(engine.check_wrong_prev_ledger(), None);
    }

    #[test]
    fn wrong_prev_ledger_detected_with_supermajority() {
        // 5-node UNL. 4 peers send proposals with a different prev_ledger.
        // 4/4 trusted disagree = 100% > 60% threshold.
        let unl = make_unl(&[1, 2, 3, 4, 5]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        let our_prev = Hash256::ZERO;
        engine.start_round(our_prev, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        let peer_prev = Hash256::new([0xBB; 32]);
        engine.peer_proposal_at(proposal_for(node(2), set.hash, peer_prev, 1), 100);
        engine.peer_proposal_at(proposal_for(node(3), set.hash, peer_prev, 1), 100);
        engine.peer_proposal_at(proposal_for(node(4), set.hash, peer_prev, 1), 100);
        engine.peer_proposal_at(proposal_for(node(5), set.hash, peer_prev, 1), 100);

        let result = engine.check_wrong_prev_ledger();
        assert!(result.is_some());
        let detected = result.unwrap();
        assert_eq!(detected.preferred_ledger, peer_prev);
        assert_eq!(detected.peer_count, 4);
        assert_eq!(detected.total_trusted, 4); // 0 agreeing + 4 disagreeing
    }

    #[test]
    fn wrong_prev_ledger_not_detected_below_threshold() {
        // 5-node UNL. 1 peer disagrees out of 4 who sent proposals.
        // 1/4 = 25% < 60%, should NOT trigger.
        let unl = make_unl(&[1, 2, 3, 4, 5]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        let peer_prev = Hash256::new([0xBB; 32]);
        // 1 peer disagrees
        engine.peer_proposal_at(proposal_for(node(2), set.hash, peer_prev, 1), 100);
        // 3 peers agree
        engine.peer_proposal_at(proposal_for(node(3), set.hash, Hash256::ZERO, 1), 100);
        engine.peer_proposal_at(proposal_for(node(4), set.hash, Hash256::ZERO, 1), 100);
        engine.peer_proposal_at(proposal_for(node(5), set.hash, Hash256::ZERO, 1), 100);

        assert_eq!(engine.check_wrong_prev_ledger(), None);
    }

    #[test]
    fn wrong_prev_ledger_at_exact_threshold() {
        // 5-node UNL. 3 peers disagree, 2 agree.
        // 3/5 = 60% >= 60%, should trigger.
        let unl = make_unl(&[1, 2, 3, 4, 5]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        let peer_prev = Hash256::new([0xBB; 32]);
        // 3 peers disagree
        engine.peer_proposal_at(proposal_for(node(2), set.hash, peer_prev, 1), 100);
        engine.peer_proposal_at(proposal_for(node(3), set.hash, peer_prev, 1), 100);
        engine.peer_proposal_at(proposal_for(node(4), set.hash, peer_prev, 1), 100);
        // 2 peers agree
        engine.peer_proposal_at(proposal_for(node(5), set.hash, Hash256::ZERO, 1), 100);

        // total_trusted = 1 agreeing + 3 disagreeing = 4 (node(5) is agreeing, nodes 2,3,4 disagree)
        // pct = 3/4 = 75% >= 60%
        let result = engine.check_wrong_prev_ledger();
        assert!(result.is_some());
        let detected = result.unwrap();
        assert_eq!(detected.preferred_ledger, peer_prev);
        assert_eq!(detected.peer_count, 3);
    }

    #[test]
    fn wrong_prev_ledger_untrusted_not_counted() {
        // 3-node UNL (1,2,3). Untrusted nodes (50,51) disagree.
        // Only trusted count, so 0 trusted disagree -> no detection.
        let unl = make_unl(&[1, 2, 3]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        let peer_prev = Hash256::new([0xBB; 32]);
        // Untrusted nodes disagree
        engine.peer_proposal_at(proposal_for(node(50), set.hash, peer_prev, 1), 100);
        engine.peer_proposal_at(proposal_for(node(51), set.hash, peer_prev, 1), 100);

        assert_eq!(engine.check_wrong_prev_ledger(), None);
    }

    #[test]
    fn wrong_prev_ledger_cleared_on_new_round() {
        // After start_round, wrong_prev_ledger tracking resets.
        let unl = make_unl(&[1, 2, 3]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        let peer_prev = Hash256::new([0xBB; 32]);
        engine.peer_proposal_at(proposal_for(node(2), set.hash, peer_prev, 1), 100);
        engine.peer_proposal_at(proposal_for(node(3), set.hash, peer_prev, 1), 100);

        // Should detect (2/2 = 100%)
        assert!(engine.check_wrong_prev_ledger().is_some());

        // Start new round, tracking should be cleared.
        engine.start_round(peer_prev, 2);
        assert_eq!(engine.check_wrong_prev_ledger(), None);
    }

    #[test]
    fn wrong_prev_ledger_picks_most_popular() {
        // 5-node UNL. 2 peers reference ledger A, 2 reference ledger B.
        // Each is 2/4 = 50% < 60%, so no detection.
        let unl = make_unl(&[1, 2, 3, 4, 5]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        let prev_a = Hash256::new([0xAA; 32]);
        let prev_b = Hash256::new([0xBB; 32]);
        engine.peer_proposal_at(proposal_for(node(2), set.hash, prev_a, 1), 100);
        engine.peer_proposal_at(proposal_for(node(3), set.hash, prev_a, 1), 100);
        engine.peer_proposal_at(proposal_for(node(4), set.hash, prev_b, 1), 100);
        engine.peer_proposal_at(proposal_for(node(5), set.hash, prev_b, 1), 100);

        // 2/4 = 50% < 60% -> no detection
        assert_eq!(engine.check_wrong_prev_ledger(), None);
    }

    // --- T17: ValidationsTrie wiring into wrong-prev-ledger detection ---

    fn validation_for(node_id: NodeId, ledger_hash: Hash256, ledger_seq: u32) -> Validation {
        // Recover the synthetic test public key matching `node_id`. We
        // tried each `id` byte 0..=255 because the test helper `node(id)`
        // derives via `from_public_key(test_pk(id))`. Falling back to an
        // empty key keeps unrelated callers compiling but will fail the
        // C3 binding check (intentional — test_node_id_mismatch_rejected
        // exercises that path).
        let public_key = (0u8..=255)
            .find(|id| node(*id) == node_id)
            .map(test_pk)
            .unwrap_or_default();
        Validation {
            node_id,
            public_key,
            ledger_hash,
            ledger_seq,
            full: true,
            close_time: 100,
            sign_time: 100,
            ..Default::default()
        }
    }

    #[test]
    fn check_wrong_prev_ledger_none_when_trie_and_proposals_empty() {
        // 5-node UNL, no proposals, no validations recorded -> None.
        let unl = make_unl(&[1, 2, 3, 4, 5]);
        let adapter = SimpleAdapter;
        let engine = ConsensusEngine::new_with_unl(
            adapter,
            node(1),
            Vec::new(),
            ConsensusParams::default(),
            unl,
        );

        assert_eq!(engine.check_wrong_prev_ledger(), None);
    }

    #[test]
    fn check_wrong_prev_ledger_via_validations_trie_supermajority() {
        // 5-node UNL. 4 trusted validators record validations for a hash
        // that is NOT our prev_ledger. 4/5 = 80% >= 60% threshold.
        // Detection must come from the validations trie even though no
        // peer proposals have been received this round.
        let unl = make_unl(&[1, 2, 3, 4, 5]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter,
            node(1),
            Vec::new(),
            ConsensusParams::default(),
            unl,
        );
        engine.start_round(Hash256::ZERO, 1);

        let preferred = Hash256::new([0xCC; 32]);
        for n in 2u8..=5 {
            assert!(engine.record_trusted_validation(validation_for(node(n), preferred, 0)));
        }

        let detected = engine.check_wrong_prev_ledger().expect("must trigger");
        assert_eq!(detected.preferred_ledger, preferred);
        assert_eq!(detected.peer_count, 4);
        assert_eq!(detected.total_trusted, 5);
    }

    #[test]
    fn check_wrong_prev_ledger_validations_trie_agrees_with_us_returns_none() {
        // 3-node UNL. All trusted validators validated OUR prev_ledger.
        // No peer proposals. The trie's preferred branch == prev_ledger,
        // so neither stage fires.
        let unl = make_unl(&[1, 2, 3]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter,
            node(1),
            Vec::new(),
            ConsensusParams::default(),
            unl,
        );
        let our_prev = Hash256::new([0xAA; 32]);
        engine.start_round(our_prev, 1);

        for n in 2u8..=3 {
            engine.record_trusted_validation(validation_for(node(n), our_prev, 0));
        }

        assert_eq!(engine.check_wrong_prev_ledger(), None);
    }

    #[test]
    fn check_wrong_prev_ledger_validations_trie_below_threshold_falls_through() {
        // 5-node UNL. Only 1 trusted validation for an alternative
        // (1/5 = 20% < 60%). With no proposal-derived disagreement
        // either, detection must return None — the trie path doesn't
        // promote a minority hash.
        let unl = make_unl(&[1, 2, 3, 4, 5]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter,
            node(1),
            Vec::new(),
            ConsensusParams::default(),
            unl,
        );
        engine.start_round(Hash256::ZERO, 1);

        let alt = Hash256::new([0xDD; 32]);
        engine.record_trusted_validation(validation_for(node(2), alt, 0));

        assert_eq!(engine.check_wrong_prev_ledger(), None);
    }

    #[test]
    fn record_trusted_validation_ignores_untrusted_node() {
        // Node 99 is not in the UNL. record_trusted_validation must
        // return false and the trie must remain empty.
        let unl = make_unl(&[1, 2, 3]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter,
            node(1),
            Vec::new(),
            ConsensusParams::default(),
            unl,
        );

        let alt = Hash256::new([0xEE; 32]);
        assert!(!engine.record_trusted_validation(validation_for(node(99), alt, 0)));
        assert_eq!(engine.validations_trie().count_for(&alt), 0);
    }

    #[test]
    fn record_trusted_validation_rejects_node_id_public_key_mismatch() {
        // Audit pass 2 C3: a validation whose node_id does NOT derive
        // from its public_key must be rejected, even when the node is
        // trusted — otherwise a forged validation can be attributed to
        // any UNL member by lying about the node_id field.
        let unl = make_unl(&[1, 2]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter,
            node(1),
            Vec::new(),
            ConsensusParams::default(),
            unl,
        );
        engine.add_trusted_validator(node(2));

        let alt = Hash256::new([0x55; 32]);
        // Build a validation that *claims* to be from node(2) but
        // carries a public_key for node(3). The binding check must
        // reject it before it touches the trie.
        let mut forged = validation_for(node(2), alt, 0);
        forged.public_key = test_pk(3);

        assert!(
            !engine.record_trusted_validation(forged),
            "node_id/public_key mismatch must be rejected"
        );
        assert_eq!(
            engine.validations_trie().count_for(&alt),
            0,
            "forged vote must not enter the trie"
        );

        // A validation with an empty public_key is also rejected
        // (the empty-key digest does not match any node(n)).
        let mut empty_key = validation_for(node(2), alt, 0);
        empty_key.public_key = vec![];
        assert!(
            !engine.record_trusted_validation(empty_key),
            "empty public_key must be rejected"
        );
        assert_eq!(engine.validations_trie().count_for(&alt), 0);
    }

    #[test]
    fn add_then_remove_trusted_validator_flows_through_trie() {
        // Solo-mode engine (empty UNL). Enrol node 7 via the new
        // add_trusted_validator helper, record a validation, then remove
        // the node — its contribution must be decremented out.
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new(adapter, node(1), ConsensusParams::default());

        let alt = Hash256::new([0x77; 32]);
        // Pre-enrolment: validation is dropped.
        assert!(!engine.record_trusted_validation(validation_for(node(7), alt, 0)));

        engine.add_trusted_validator(node(7));
        assert!(engine.record_trusted_validation(validation_for(node(7), alt, 0)));
        assert_eq!(engine.validations_trie().count_for(&alt), 1);

        engine.remove_trusted_validator(&node(7));
        assert_eq!(engine.validations_trie().count_for(&alt), 0);
    }

    // --- Adaptive close-time resolution tests ---

    #[test]
    fn adaptive_resolution_starts_at_params_value() {
        let engine = test_engine();
        assert_eq!(engine.close_time_resolution(), 30);
    }

    #[test]
    fn solo_rounds_count_as_agreement() {
        // Solo mode (no peers) treats every round as agreement, so the
        // rippled `getNextLedgerTimeResolution` cadence steps one bin
        // finer at every multiple of `INCREASE_LEDGER_TIME_RESOLUTION_EVERY`
        // (= 8).  Starting at the default 30s bin (index 2), the first
        // tightening fires when `start_round` is called with seq == 8.
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new(adapter, node(1), ConsensusParams::default());

        for seq in 1..=8 {
            engine.start_round(Hash256::ZERO, seq);
            let set = TxSet::new(vec![]);
            engine.close_ledger(set, 100, seq).unwrap();
            engine.converge();
        }

        // 8 agreed rounds: at seq == 8 the modulo gate fires and the
        // bin steps from 30s (index 2) to 20s (index 1) — one bin
        // finer, matching rippled.
        assert_eq!(engine.close_time_resolution(), 20);
    }

    #[test]
    fn close_time_agreement_tightens_resolution() {
        // Run 8 consecutive rounds where the peer agrees on the
        // close-time bucket (spread of 0 ≤ resolution).  The first 7
        // are no-ops at the bin level — only the call to `start_round`
        // for seq == 8 (multiple of the rippled
        // `INCREASE_LEDGER_TIME_RESOLUTION_EVERY = 8` cadence) triggers
        // a step finer.  Starting at 30s (index 2) the bin moves to
        // 20s (index 1).
        let adapter = SimpleAdapter;
        let node_a = node(0xA0);
        let node_b = node(0xB0);
        let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());

        for seq in 1..=8u32 {
            engine.start_round(Hash256::ZERO, seq);
            let set = TxSet::new(vec![]);
            engine.close_ledger(set.clone(), 100, seq).unwrap();

            // Peer proposes same close time
            engine.peer_proposal_at(
                Proposal {
                    node_id: node_b,
                    public_key: vec![0x02; 33],
                    tx_set_hash: set.hash,
                    close_time: 100,
                    prop_seq: 0,
                    ledger_seq: seq,
                    prev_ledger: Hash256::ZERO,
                    signature: None,
                },
                100,
            );

            engine.converge();
        }

        // 8 agreed rounds -> bin stepped one finer: 30s -> 20s.
        assert_eq!(engine.close_time_resolution(), 20);
    }

    #[test]
    fn close_time_disagreement_loosens_resolution() {
        // The rippled `getNextLedgerTimeResolution` cadence widens the
        // bin on disagreement at every multiple of
        // `DECREASE_LEDGER_TIME_RESOLUTION_EVERY = 1`, i.e. on the very
        // next `start_round` after the disagreement is observed.
        // Default starts at 30s; one disagreed round followed by a
        // fresh start_round steps the bin one COARSER to 60s.
        let adapter = SimpleAdapter;
        let node_a = node(0xA0);
        let node_b = node(0xB0);
        let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());

        // Round 1: peers disagree (spread of 100 > 30s resolution).
        // `accept` records `previous_close_agreed = false`.
        engine.start_round(Hash256::ZERO, 1);
        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();
        engine.peer_proposal_at(
            Proposal {
                node_id: node_b,
                public_key: vec![0x02; 33],
                tx_set_hash: set.hash,
                close_time: 200, // spread of 100 > 30
                prop_seq: 0,
                ledger_seq: 1,
                prev_ledger: Hash256::ZERO,
                signature: None,
            },
            200,
        );
        engine.converge();
        // The disagreement flag is set, but the bin step happens on
        // the NEXT `start_round` (rippled recomputes resolution at the
        // top of each round, not in `accept`).
        assert_eq!(engine.accepted_close_flags(), 1);
        assert_eq!(engine.close_time_resolution(), 30);

        // Round 2: `start_round` calls
        // `next_resolution(30, false, 2)` → 60 (one bin coarser).
        engine.start_round(Hash256::ZERO, 2);
        assert_eq!(engine.close_time_resolution(), 60);
    }

    #[test]
    fn adaptive_resolution_used_for_rounding() {
        // Verify that once the bin steps finer through the rippled
        // cadence, downstream close-time rounding uses the NEW bin
        // (not the params-default).  Run 8 agreed solo rounds to
        // step from 30s -> 20s, then submit a round whose proposals
        // straddle a 20s boundary and check the accepted close time.
        let adapter = SimpleAdapter;
        let node_a = node(0xA0);
        let node_b = node(0xB0);
        let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());

        for seq in 1..=8 {
            engine.start_round(Hash256::ZERO, seq);
            let set = TxSet::new(vec![]);
            engine.close_ledger(set, 100, seq).unwrap();
            engine.converge();
        }
        assert_eq!(engine.close_time_resolution(), 20);

        // Round 9: peer proposes a close_time within the 20s bin so
        // the round still counts as agreement.  Both 100 and 105 round
        // to bucket 100 at resolution 20 ((100+10)/20*20 = 100,
        // (105+10)/20*20 = 100), giving an unambiguous winner.
        engine.start_round(Hash256::ZERO, 9);
        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 9).unwrap();
        engine.peer_proposal_at(
            Proposal {
                node_id: node_b,
                public_key: vec![0x02; 33],
                tx_set_hash: set.hash,
                close_time: 105,
                prop_seq: 0,
                ledger_seq: 9,
                prev_ledger: Hash256::ZERO,
                signature: None,
            },
            105,
        );
        engine.converge();

        // Both proposals round to bucket 100 at resolution 20 → winner is 100.
        assert_eq!(engine.accepted_close_time(), Some(100));
    }

    #[test]
    fn adaptive_resolution_getter_reflects_state() {
        // The engine drives `AdaptiveCloseTime` exclusively through
        // `set_resolution` now (T03), so the legacy
        // `consecutive_agreements` counter stays at 0.  The visible
        // state to assert is `resolution()`: stable at the default 30s
        // bin until the modulo cadence fires at seq == 8.
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new(adapter, node(1), ConsensusParams::default());

        assert_eq!(engine.adaptive_close_time().resolution(), 30);

        // Seq 1: agreement (solo) but seq % 8 != 0 → resolution holds.
        engine.start_round(Hash256::ZERO, 1);
        let set = TxSet::new(vec![]);
        engine.close_ledger(set, 100, 1).unwrap();
        engine.converge();
        assert_eq!(engine.adaptive_close_time().resolution(), 30);

        // Walk to seq == 8 — the cadence fires and the bin tightens.
        for seq in 2..=8 {
            engine.start_round(Hash256::ZERO, seq);
            let set = TxSet::new(vec![]);
            engine.close_ledger(set, 100, seq).unwrap();
            engine.converge();
        }
        assert_eq!(engine.adaptive_close_time().resolution(), 20);
    }

    #[test]
    fn monotonic_close_time_across_rounds() {
        // Verifies that `eff_close_time` is wired into the establish-phase
        // aggregation: when a round closes with a `close_time` that would
        // round to <= `prior_close_time`, the engine's accepted close time
        // is clamped to `prior_close_time + 1` (rippled `effCloseTime`
        // monotonicity guarantee).
        let mut engine = test_engine();

        // Round 1: parent close_time = 100. Our raw close_time = 50 would
        // round to 60 at the default 30s resolution, but the clamp pushes
        // it to 101.
        engine.start_round_with_prior(Hash256::ZERO, 1, 100);
        engine
            .close_ledger(TxSet::new(vec![]), 50, 1)
            .unwrap();
        assert!(engine.converge());
        let ct1 = engine.accepted_close_time().expect("round 1 close time");
        assert!(
            ct1 >= 101,
            "round 1 close_time {} should be >= prior+1 (101)",
            ct1
        );

        // Round 2: parent close_time = ct1 (>= 101). Same raw close_time
        // 50, must clamp to >= ct1 + 1 (>= 102), demonstrating monotonicity
        // across rounds.
        engine.start_round_with_prior(Hash256::ZERO, 2, ct1);
        engine
            .close_ledger(TxSet::new(vec![]), 50, 2)
            .unwrap();
        assert!(engine.converge());
        let ct2 = engine.accepted_close_time().expect("round 2 close time");
        assert!(
            ct2 >= ct1 + 1,
            "round 2 close_time {} should be strictly greater than round 1 ({})",
            ct2,
            ct1
        );
    }

    // --- T14: proposal freshness (propRELAY_INTERVAL) tests ---

    fn freshness_engine() -> ConsensusEngine<SimpleAdapter> {
        // 2-node UNL with us=node(1), peer=node(2). Drives the engine into
        // Establish phase via close_ledger so peer_proposal_at exercises the
        // freshness gate (it short-circuits in non-Establish phases).
        let unl = make_unl(&[1, 2]);
        let mut engine = ConsensusEngine::new_with_unl(
            SimpleAdapter,
            node(1),
            Vec::new(),
            ConsensusParams::default(),
            unl,
        );
        engine.start_round(Hash256::ZERO, 1);
        let set = TxSet::new(vec![]);
        engine.close_ledger(set, 1_000_000, 1).unwrap();
        engine
    }

    #[test]
    fn fresh_proposal_accepted() {
        let mut engine = freshness_engine();
        let now = 1_000_000u32;
        let our_set_hash = engine.our_set().unwrap().hash;
        let p = Proposal {
            node_id: node(2),
            public_key: vec![0x02; 33],
            tx_set_hash: our_set_hash,
            close_time: now, // delta = 0 < PROPOSAL_FRESHNESS_SECS
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };
        engine.peer_proposal_at(p, now);
        assert_eq!(engine.proposal_dropped_stale_total(), 0);
        assert_eq!(engine.peer_positions.len(), 1);
    }

    #[test]
    fn stale_proposal_rejected_increments_counter() {
        let mut engine = freshness_engine();
        let now = 1_000_000u32;
        let our_set_hash = engine.our_set().unwrap().hash;
        let stale = Proposal {
            node_id: node(2),
            public_key: vec![0x02; 33],
            tx_set_hash: our_set_hash,
            close_time: now - 100, // delta = 100 > 30
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };
        engine.peer_proposal_at(stale, now);
        assert_eq!(engine.proposal_dropped_stale_total(), 1);
        assert!(engine.peer_positions.is_empty());
    }

    #[test]
    fn future_proposal_rejected_increments_counter() {
        let mut engine = freshness_engine();
        let now = 1_000_000u32;
        let our_set_hash = engine.our_set().unwrap().hash;

        // First feed a stale proposal so we can verify the counter
        // accumulates across calls (== 2 after this test, per spec).
        let stale = Proposal {
            node_id: node(2),
            public_key: vec![0x02; 33],
            tx_set_hash: our_set_hash,
            close_time: now - 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };
        engine.peer_proposal_at(stale, now);
        assert_eq!(engine.proposal_dropped_stale_total(), 1);

        let future = Proposal {
            node_id: node(2),
            public_key: vec![0x02; 33],
            tx_set_hash: our_set_hash,
            close_time: now + 100, // delta = 100 > 30
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };
        engine.peer_proposal_at(future, now);
        assert_eq!(engine.proposal_dropped_stale_total(), 2);
        assert!(engine.peer_positions.is_empty());
    }

    // --- Audit pass 2 C2: pending_proposals gating in Open phase ---

    /// Engine in Open phase (no `close_ledger` so phase != Establish), with
    /// a 2-node UNL trusting nodes 1 and 2. Used to exercise the
    /// pre-Establish UNL/freshness/cap gates on `pending_proposals`.
    fn open_phase_engine() -> ConsensusEngine<SimpleAdapter> {
        let unl = make_unl(&[1, 2]);
        let mut engine = ConsensusEngine::new_with_unl(
            SimpleAdapter,
            node(1),
            Vec::new(),
            ConsensusParams::default(),
            unl,
        );
        engine.start_round(Hash256::ZERO, 1);
        debug_assert_eq!(engine.phase(), ConsensusPhase::Open);
        engine
    }

    #[test]
    fn pending_proposals_drops_untrusted_in_open_phase() {
        // Audit pass 2 C2: an untrusted peer must not be able to push into
        // pending_proposals during Open phase. Without the gate this is the
        // primary memory-amplification vector.
        let mut engine = open_phase_engine();
        let now = 1_000_000u32;
        let p = Proposal {
            node_id: node(99), // not in UNL
            public_key: vec![0x02; 33],
            tx_set_hash: Hash256::ZERO,
            close_time: now,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };
        engine.peer_proposal_at(p, now);
        assert!(engine.pending_proposals.is_empty());
    }

    #[test]
    fn pending_proposals_drops_stale_in_open_phase_and_bumps_counter() {
        // Audit pass 2 C2: a trusted but stale-time proposal must be
        // counted and dropped, never buffered. Mirrors the Establish-phase
        // freshness gate.
        let mut engine = open_phase_engine();
        let now = 1_000_000u32;
        let stale = Proposal {
            node_id: node(2), // trusted
            public_key: vec![0x02; 33],
            tx_set_hash: Hash256::ZERO,
            close_time: now - 100, // delta = 100 > PROPOSAL_FRESHNESS_SECS
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };
        engine.peer_proposal_at(stale, now);
        assert_eq!(engine.proposal_dropped_stale_total(), 1);
        assert!(engine.pending_proposals.is_empty());
    }

    #[test]
    fn pending_proposals_capped_in_open_phase() {
        // Audit pass 2 C2: even when every proposal passes UNL + freshness,
        // pending_proposals must be capped at PENDING_PROPOSALS_MAX so a
        // trusted peer (or compromised key) cannot exhaust memory.
        let mut engine = open_phase_engine();
        let now = 1_000_000u32;
        for i in 0..PENDING_PROPOSALS_MAX {
            let p = Proposal {
                node_id: node(2),
                public_key: vec![0x02; 33],
                tx_set_hash: Hash256::ZERO,
                close_time: now,
                prop_seq: i as u32,
                ledger_seq: 1,
                prev_ledger: Hash256::ZERO,
                signature: None,
            };
            engine.peer_proposal_at(p, now);
        }
        assert_eq!(engine.pending_proposals.len(), PENDING_PROPOSALS_MAX);

        // The 1025th proposal must be dropped, not buffered.
        let overflow = Proposal {
            node_id: node(2),
            public_key: vec![0x02; 33],
            tx_set_hash: Hash256::ZERO,
            close_time: now,
            prop_seq: PENDING_PROPOSALS_MAX as u32,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };
        engine.peer_proposal_at(overflow, now);
        assert_eq!(engine.pending_proposals.len(), PENDING_PROPOSALS_MAX);
    }

    // --- T19: ProposalTracker dedup tests ---

    /// Build a 3-node engine driven into Establish phase, with us=node(1)
    /// and a single-tx local set so peer proposals against a *different*
    /// tx_set materialise dispute votes we can assert on.
    fn dedup_engine(
        local_set: TxSet,
        peer_set: TxSet,
    ) -> ConsensusEngine<MockAdapter> {
        let unl = make_unl(&[1, 2, 3]);
        let adapter = MockAdapter::with_tx_sets(vec![local_set.clone(), peer_set]);
        let mut engine = ConsensusEngine::new_with_unl(
            adapter,
            node(1),
            Vec::new(),
            ConsensusParams::default(),
            unl,
        );
        engine.start_round(Hash256::ZERO, 1);
        engine.close_ledger(local_set, 100, 1).unwrap();
        engine
    }

    #[test]
    fn duplicate_proposal_does_not_bump_dispute_counters() {
        // Local set has tx1; peer's set is empty so tx1 becomes a dispute
        // when peer's proposal lands. Re-delivering the identical proposal
        // must NOT bump the dispute's nay_count for that peer (the
        // ProposalTracker drops the second call before it reaches
        // create_disputes).
        let tx1 = Hash256::new([0x01; 32]);
        let local_set = TxSet::new(vec![tx1]);
        let peer_set = TxSet::new(vec![]);
        let mut engine = dedup_engine(local_set.clone(), peer_set.clone());

        let p = Proposal {
            node_id: node(2),
            public_key: vec![0x02; 33],
            tx_set_hash: peer_set.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };
        engine.peer_proposal_at(p.clone(), 100);
        assert_eq!(engine.peer_positions.len(), 1);
        let nay_after_first = engine
            .disputes()
            .get(&tx1)
            .expect("dispute should exist after first proposal")
            .nay_count();
        assert_eq!(nay_after_first, 1);

        // Replay identical proposal — same node_id, prev_ledger, prop_seq.
        engine.peer_proposal_at(p, 100);

        // No new dispute created, peer_positions unchanged, vote count
        // unchanged (insert into HashMap with same key/value would also
        // be a no-op, but we want to prove create_disputes was skipped).
        assert_eq!(engine.peer_positions.len(), 1);
        assert_eq!(engine.disputes().len(), 1);
        let nay_after_dup = engine.disputes().get(&tx1).unwrap().nay_count();
        assert_eq!(nay_after_dup, nay_after_first);
        // ProposalTracker still holds prop_seq=0.
        assert_eq!(
            engine
                .proposal_tracker
                .get(&node(2), &Hash256::ZERO)
                .unwrap()
                .prop_seq,
            0
        );
    }

    #[test]
    fn lower_prop_seq_proposal_is_rejected() {
        // First accept prop_seq=5 with peer_set_a (empty). Then re-deliver
        // the same node with prop_seq=3 but a *different* tx_set: the
        // ProposalTracker must reject the older seq, leaving peer_positions
        // pinned to the prop_seq=5 entry.
        let tx1 = Hash256::new([0x01; 32]);
        let tx2 = Hash256::new([0x02; 32]);
        let local_set = TxSet::new(vec![tx1]);
        let peer_set_a = TxSet::new(vec![]);
        let peer_set_b = TxSet::new(vec![tx2]);

        let unl = make_unl(&[1, 2, 3]);
        let adapter = MockAdapter::with_tx_sets(vec![
            local_set.clone(),
            peer_set_a.clone(),
            peer_set_b.clone(),
        ]);
        let mut engine = ConsensusEngine::new_with_unl(
            adapter,
            node(1),
            Vec::new(),
            ConsensusParams::default(),
            unl,
        );
        engine.start_round(Hash256::ZERO, 1);
        engine.close_ledger(local_set, 100, 1).unwrap();

        let high = Proposal {
            node_id: node(2),
            public_key: vec![0x02; 33],
            tx_set_hash: peer_set_a.hash,
            close_time: 100,
            prop_seq: 5,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };
        engine.peer_proposal_at(high, 100);
        assert_eq!(
            engine
                .peer_positions
                .get(&node(2))
                .unwrap()
                .tx_set_hash,
            peer_set_a.hash
        );

        // Older prop_seq carrying a different tx_set: rejected silently.
        let stale = Proposal {
            node_id: node(2),
            public_key: vec![0x02; 33],
            tx_set_hash: peer_set_b.hash,
            close_time: 100,
            prop_seq: 3,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };
        engine.peer_proposal_at(stale, 100);

        // Stored position must still be the prop_seq=5 / peer_set_a entry.
        let stored = engine.peer_positions.get(&node(2)).unwrap();
        assert_eq!(stored.prop_seq, 5);
        assert_eq!(stored.tx_set_hash, peer_set_a.hash);
        assert_eq!(
            engine
                .proposal_tracker
                .get(&node(2), &Hash256::ZERO)
                .unwrap()
                .prop_seq,
            5
        );
    }

    #[test]
    fn higher_prop_seq_proposal_replaces_existing() {
        // First accept prop_seq=0 with peer_set_a. Then deliver prop_seq=1
        // with peer_set_b — ProposalTracker must accept and the engine's
        // peer_positions entry must rotate to reflect the new tx_set.
        let tx1 = Hash256::new([0x01; 32]);
        let tx2 = Hash256::new([0x02; 32]);
        let local_set = TxSet::new(vec![tx1]);
        let peer_set_a = TxSet::new(vec![]);
        let peer_set_b = TxSet::new(vec![tx2]);

        let unl = make_unl(&[1, 2, 3]);
        let adapter = MockAdapter::with_tx_sets(vec![
            local_set.clone(),
            peer_set_a.clone(),
            peer_set_b.clone(),
        ]);
        let mut engine = ConsensusEngine::new_with_unl(
            adapter,
            node(1),
            Vec::new(),
            ConsensusParams::default(),
            unl,
        );
        engine.start_round(Hash256::ZERO, 1);
        engine.close_ledger(local_set, 100, 1).unwrap();

        let first = Proposal {
            node_id: node(2),
            public_key: vec![0x02; 33],
            tx_set_hash: peer_set_a.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };
        engine.peer_proposal_at(first, 100);
        assert_eq!(
            engine
                .peer_positions
                .get(&node(2))
                .unwrap()
                .prop_seq,
            0
        );

        let updated = Proposal {
            node_id: node(2),
            public_key: vec![0x02; 33],
            tx_set_hash: peer_set_b.hash,
            close_time: 100,
            prop_seq: 1,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };
        engine.peer_proposal_at(updated, 100);

        let stored = engine.peer_positions.get(&node(2)).unwrap();
        assert_eq!(stored.prop_seq, 1);
        assert_eq!(stored.tx_set_hash, peer_set_b.hash);
        assert_eq!(
            engine
                .proposal_tracker
                .get(&node(2), &Hash256::ZERO)
                .unwrap()
                .prop_seq,
            1
        );
    }
}
