use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use rxrpl_primitives::Hash256;

use crate::adapter::ConsensusAdapter;
use crate::close_resolution::{AdaptiveCloseTime, next_resolution};
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
    /// Most recent close_time observed in any peer proposal/validation,
    /// regardless of round/prev_ledger. Used as a hint to align our own
    /// close_time when our_position isn't yet set (chicken-and-egg with
    /// rounded_close_time). Without this, two cross-impl validators race
    /// each other on the close-timer wall-clock and produce divergent
    /// close_time → divergent ledger hashes for the same content.
    latest_peer_close_time: Option<u32>,
    /// `ledger_seq` of the most recent peer proposal, regardless of round.
    /// Lets the node layer tell "the peer proposed our current round" from
    /// "the peer is already on a later ledger" — the latter means rxrpl is
    /// behind and must catch up, not close `#N` solo with a future-round
    /// `latest_peer_close_time`.
    latest_peer_ledger_seq: Option<u32>,
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
    /// Instant we last re-broadcast our position. Periodic re-broadcast
    /// (every `params.propose_interval_ms`) keeps our proposal fresh in
    /// peers' buffers so peers with a longer idle interval (e.g. rippled's
    /// 20s) see our position within their own Establish window, not the
    /// stale proposal we emitted ~18s before they opened the round (issue
    /// #76 — `prev_proposers` jumps from 1 to 3 when a fresh proposal
    /// reaches rippled mid-Establish). Time-based so it stays independent
    /// of how often `converge()` is polled.
    last_share_at: Option<Instant>,
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
    /// Counter: incoming peer proposals buffered into the holding pen
    /// (`future_proposals`) because their `prev_ledger` does not match our
    /// current `prev_ledger` and we have not yet caught up. Mirrors the
    /// rippled `Counter[ConsensusProposals.heldFutureLedger]` JLOG metric.
    /// Exposed via [`Self::proposals_held_pending_prev_ledger_total`].
    proposals_held_pending_prev_ledger_total: AtomicU64,
    /// Instant the current Establish phase began (set in [`Self::close_ledger`]).
    /// Used to enforce `params.min_consensus_time_ms`: even once quorum
    /// agreement is observed, [`Self::converge`] will not accept before this
    /// much wall-clock time has elapsed. `None` outside the Establish phase.
    establish_started_at: Option<Instant>,
    /// Instant the consensus `round` counter last advanced. `converge()` is
    /// polled far more often than `propose_interval_ms`, but the round
    /// counter (threshold escalation + `max_consensus_rounds` window) must
    /// tick at the proposal cadence so the Establish window stays sized in
    /// real time, independent of poll frequency. `None` until the first
    /// advance of the current round.
    last_round_advance: Option<Instant>,
    /// Whether this node is itself a trusted UNL member. Set authoritatively
    /// by the node layer (which knows every form of its own identity —
    /// node-peer key, validator master key, validator ephemeral signing
    /// key) via [`Self::set_self_trusted`]. The engine cannot reliably infer
    /// this by key matching alone: it signs proposals with the ephemeral
    /// key, but the UNL may list the master key, and the two never match.
    /// A node never receives its own proposal as a peer, so without
    /// counting itself a validator is permanently one short of quorum.
    self_trusted: bool,
    /// Canonical transaction blobs seen this round, keyed by tx id. Absorbed
    /// from `our_set` at close and from every peer set acquired during
    /// dispute resolution, so a set rebuilt after dispute resolution still
    /// carries the blobs needed to serve it to an acquiring peer.
    tx_blobs: HashMap<Hash256, Vec<u8>>,
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
        Self::new_with_unl(
            adapter,
            node_id,
            Vec::new(),
            params,
            TrustedValidatorList::empty(),
        )
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
            latest_peer_close_time: None,
            latest_peer_ledger_seq: None,
            accepted_close_flags: 0,
            accepted_validation: None,
            unl,
            negative_unl_tracker: NegativeUnlTracker::new(),
            pending_proposals: Vec::new(),
            last_share_at: None,
            wrong_prev_ledger_votes: HashMap::new(),
            validations_trie,
            prev_ledger_seq: 0,
            future_proposals: HashMap::new(),
            adaptive_close_time,
            previous_close_agreed: true,
            prior_close_time: 0,
            proposal_dropped_stale_total: AtomicU64::new(0),
            proposals_held_pending_prev_ledger_total: AtomicU64::new(0),
            establish_started_at: None,
            last_round_advance: None,
            self_trusted: false,
            tx_blobs: HashMap::new(),
        }
    }

    /// Declare whether this node is itself a trusted UNL member. Called by
    /// the node layer, which alone knows every form of the local identity
    /// (node-peer key, validator master key, validator ephemeral signing
    /// key) and can match it against the configured UNL. Drives the quorum
    /// self-count in [`Self::converge`].
    pub fn set_self_trusted(&mut self, trusted: bool) {
        self.self_trusted = trusted;
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

    /// Register the master public keys for a trusted validator set with the
    /// negative-UNL tracker. The tracker needs the hex-encoded master key of
    /// each validator so that emitted [`crate::negative_unl::NegativeUnlChange`]
    /// entries carry the value used in `UNLModify` pseudo-transactions.
    ///
    /// Validators in `trusted_set` without an entry in `key_map` are skipped
    /// silently — `evaluate_negative_unl` will simply not emit a change for
    /// them (mirrors rippled, which does not demote unknown validators).
    pub fn register_validators(
        &mut self,
        trusted_set: &std::collections::HashSet<NodeId>,
        key_map: &HashMap<NodeId, String>,
    ) {
        for node_id in trusted_set {
            if let Some(key) = key_map.get(node_id) {
                self.negative_unl_tracker
                    .register_validator(*node_id, key.clone());
            }
        }
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
        let current_nunl: Vec<NodeId> = self.unl.negative_unl_set().iter().copied().collect();
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

    /// Number of peer positions observed for the current round.
    /// Used by the node loop to decide whether to extend the OPEN phase
    /// (wait for at least one peer to propose first) — breaks the cross-impl
    /// chase loop where two nodes close at slightly different wall-clock times.
    pub fn peer_position_count(&self) -> usize {
        self.peer_positions.len()
    }

    /// Most recent peer-broadcast close_time observed (across any round/prev_ledger).
    /// The node layer uses this to align its own pending close_time with
    /// the peer's bucket BEFORE its first proposal is built (chicken-and-egg
    /// with `rounded_close_time` which requires our_position to exist).
    pub fn latest_peer_close_time(&self) -> Option<u32> {
        self.latest_peer_close_time
    }

    /// `ledger_seq` of the most recent peer proposal seen this engine's
    /// lifetime. The node layer compares it against its own open seq to
    /// detect that it is behind the network.
    pub fn latest_peer_ledger_seq(&self) -> Option<u32> {
        self.latest_peer_ledger_seq
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
        let (preferred, &count) = ledger_counts.iter().max_by_key(|&(_, &c)| c).unwrap();

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
        let derived_node_id = NodeId(rxrpl_crypto::sha512_half::sha512_half(&[validation
            .public_key
            .as_slice()]));
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
    // NIGHT-SHIFT-REVIEW: the trusted set on the trie is independent of
    // the `TrustedValidatorList` UNL because the latter exposes no
    // `add_trusted(NodeId)` setter. Callers enrolling a node by NodeId
    // outside the UNL constructor / manifest pipeline must call this
    // method explicitly — until `TrustedValidatorList` grows a setter
    // and the two sets can share state.
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

    /// Replace the trusted validator set from a verified validator list,
    /// keeping the validations-trie trusted set in sync and letting the
    /// UNL's derived quorum follow the new size. A no-op on an empty list,
    /// so a malformed or empty VL can never wipe the UNL into solo mode.
    pub fn set_trusted_master_keys(&mut self, master_keys: &[rxrpl_primitives::PublicKey]) {
        if master_keys.is_empty() {
            return;
        }
        let new_ids: Vec<NodeId> = master_keys
            .iter()
            .map(|pk| NodeId::from_public_key(pk.as_bytes()))
            .collect();
        let stale: Vec<NodeId> = self
            .unl
            .trusted_set()
            .iter()
            .filter(|id| !new_ids.contains(id))
            .copied()
            .collect();
        for id in &stale {
            self.validations_trie.remove_trusted(id);
        }
        for id in &new_ids {
            self.validations_trie.add_trusted(*id);
        }
        self.unl.update_from_validator_keys(master_keys);
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
        self.last_share_at = None;
        self.last_round_advance = None;
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
        // Absorb our own transaction blobs so a set rebuilt after dispute
        // resolution can still be served to an acquiring peer.
        self.tx_blobs.clear();
        for (id, blob) in &our_set.blobs {
            self.tx_blobs.insert(*id, blob.clone());
        }
        // Publish the candidate set so a peer that receives our ProposeSet
        // can acquire it (the cache is what `handle_get_tx_set` serves from).
        self.adapter.publish_tx_set(&our_set);
        self.our_set = Some(our_set);
        self.phase = ConsensusPhase::Establish;
        let now = Instant::now();
        self.establish_started_at = Some(now);
        // The proposal just emitted counts as our first share this round;
        // the next periodic re-broadcast is `propose_interval_ms` later.
        self.last_share_at = Some(now);
        // Anchor the round-advance clock to Establish start so the round
        // counter ticks once per `propose_interval_ms` regardless of how
        // often converge() is polled.
        self.last_round_advance = Some(now);

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

    /// Total number of incoming peer proposals buffered into the holding
    /// pen (`future_proposals`) because their `prev_ledger` was unknown to
    /// us at the time the proposal arrived. Mirrors rippled's
    /// `Counter[ConsensusProposals.heldFutureLedger]` JLOG metric (held
    /// pending prev-ledger catch-up).
    pub fn proposals_held_pending_prev_ledger_total(&self) -> u64 {
        self.proposals_held_pending_prev_ledger_total
            .load(Ordering::Relaxed)
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
        // Track most recent peer close_time regardless of round: lets the node
        // layer adopt the peer's close_time when picking its own pending value
        // (breaks cross-impl close-timer race).
        if proposal.close_time > 0 {
            self.latest_peer_close_time = Some(proposal.close_time);
        }
        if proposal.ledger_seq > 0 {
            self.latest_peer_ledger_seq = Some(
                self.latest_peer_ledger_seq
                    .map_or(proposal.ledger_seq, |s| s.max(proposal.ledger_seq)),
            );
        }
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
                self.prev_ledger,
                proposal.prev_ledger
            );
            self.wrong_prev_ledger_votes
                .insert(proposal.node_id, proposal.prev_ledger);
            self.proposals_held_pending_prev_ledger_total
                .fetch_add(1, Ordering::Relaxed);
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
                    our.ledger_seq,
                    proposal.ledger_seq
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
                proposal.node_id,
                proposal.ledger_seq,
                proposal.prop_seq
            );
            return;
        }

        tracing::debug!(
            "accepted proposal from {:?} seq={}",
            proposal.node_id,
            proposal.ledger_seq
        );
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

            // Absorb the peer set's blobs so a dispute that pulls one of
            // its transactions into our set can still be served onward.
            for (id, blob) in &peer_set.blobs {
                if !blob.is_empty() {
                    self.tx_blobs.entry(*id).or_insert_with(|| blob.clone());
                }
            }

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
    /// Returns `(close_time, had_close_time_consensus)`. When there is no
    /// consensus the close_time is the rippled "no opinion" value
    /// (`parentCloseTime + 1`) and the bool is `false`.
    fn effective_close_time(&self) -> (u32, bool) {
        let our_time = match &self.our_position {
            Some(p) => p.close_time,
            None => return (0, true),
        };
        let resolution = self.adaptive_close_time.resolution();
        let prior = self.prior_close_time;

        if self.peer_positions.is_empty() {
            return (eff_close_time(our_time, resolution, prior), true);
        }

        // Tally votes per rounded close-time bucket.
        //
        // CRITICAL (H11): peer votes are FILTERED, not clamped. Calling
        // `eff_close_time(peer_time, ...)` here would silently rewrite
        // any peer vote with `close_time == 0` (rippled "no opinion"
        // sentinel) or `close_time < prior + 1` to `prior + 1`,
        // collapsing distinct adversarial votes into one shared bucket
        // and manufacturing apparent agreement. We discard those votes
        // entirely instead, then apply `eff_close_time` only to the
        // final winning bucket below.
        let min_allowed = prior.saturating_add(1);
        let mut votes: HashMap<u32, u32> = HashMap::new();
        // Our own time is voted only if it is a real opinion (non-zero)
        // and would not be clamped to the floor bucket. The same filter
        // we apply to peers applies to us, so self can never manufacture
        // a vote in the floor bucket either.
        let our_bucket_raw = round_close_time(our_time, resolution);
        if our_time != 0 && our_bucket_raw >= min_allowed {
            *votes.entry(our_bucket_raw).or_insert(0) += 1;
        }
        for peer in self.peer_positions.values() {
            // Filter: skip the "no opinion" sentinel and any vote that
            // would round below the monotonicity floor (those would be
            // clamped into a single bucket otherwise).
            if peer.close_time == 0 {
                continue;
            }
            let rounded = round_close_time(peer.close_time, resolution);
            if rounded < min_allowed {
                continue;
            }
            *votes.entry(rounded).or_insert(0) += 1;
        }

        // No surviving votes (everyone was filtered out): fall back to
        // the solo path so we still emit a monotonic close_time.
        if votes.is_empty() {
            return (eff_close_time(our_time, resolution, prior), true);
        }

        // Pick the bucket with the most votes. On ties, pick the LATER
        // bucket: both validators run the same tiebreak deterministically,
        // and biasing towards "later" matches the natural drift of each
        // validator's clock forward over time, so the chosen bucket is
        // less likely to fall behind prior_close_time + 1s monotonicity.
        let mut best_bucket = 0u32;
        let mut best_count = 0u32;
        for (bucket, count) in &votes {
            if *count > best_count || (*count == best_count && *bucket > best_bucket) {
                best_bucket = *bucket;
                best_count = *count;
            }
        }
        // No strict majority for any bucket (e.g. a 1-1 split between rxrpl
        // and rippled one resolution apart): there is no close-time
        // consensus. rippled's `effCloseTime` then closes at
        // `parentCloseTime + 1` (a 0/"no opinion" close time). Match that
        // exactly — both implementations compute `prior + 1` deterministically
        // — instead of biasing to the "later" bucket, which rippled would not
        // pick and which therefore forks the ledger hash every round.
        //
        // The denominator is the number of *surviving* votes, not every
        // peer: a filtered-out garbage vote (H11) must not dilute an
        // otherwise-unanimous honest cohort into a false "no consensus".
        let surviving_votes: u32 = votes.values().sum();
        if best_count * 2 <= surviving_votes {
            return (eff_close_time(0, resolution, prior), false);
        }
        // Apply eff_close_time only to the FINAL winning bucket so the
        // monotonicity guarantee (close_time > prior) still holds for
        // the accepted ledger.
        (eff_close_time(best_bucket, resolution, prior), true)
    }

    /// Update `our_position.close_time` to match the consensus winner
    /// only when a STRICT majority of voters (us + peers) share the
    /// same bucket. This drives cross-validator convergence without
    /// suppressing the disagreement signal used for adaptive resolution
    /// widening: when nodes are split across buckets (e.g. 1-1 in a
    /// 2-validator setup) no realignment fires and the spread/flag
    /// detection in `accept()` still sees the disagreement.
    /// Returns `true` if our position's close_time was changed to match the
    /// peer-popular bucket. The caller can use this signal to re-broadcast
    /// the updated position so peers see our new bucket within their own
    /// Establish window.
    fn align_close_time_with_peers(&mut self) -> bool {
        if self.our_position.is_none() || self.peer_positions.is_empty() {
            return false;
        }
        let resolution = self.adaptive_close_time.resolution();
        let prior = self.prior_close_time;
        let our_time = self
            .our_position
            .as_ref()
            .map(|p| p.close_time)
            .unwrap_or(0);
        let min_allowed = prior.saturating_add(1);

        // Same filter-don't-clamp policy as `effective_close_time` (H11):
        // peers with the "no opinion" sentinel (close_time == 0) or with
        // a rounded vote below the monotonicity floor are dropped, not
        // rewritten to `prior + 1`. Otherwise two adversarial peers
        // sending close_time=1 and close_time=2 would both vote into the
        // same floor bucket, manufacture a strict majority, and force
        // realignment of our position to a bucket nobody actually voted
        // for.
        let mut votes: HashMap<u32, u32> = HashMap::new();
        let our_bucket = round_close_time(our_time, resolution);
        if our_time != 0 && our_bucket >= min_allowed {
            *votes.entry(our_bucket).or_insert(0) += 1;
        }
        for peer in self.peer_positions.values() {
            if peer.close_time == 0 {
                continue;
            }
            let rounded = round_close_time(peer.close_time, resolution);
            if rounded < min_allowed {
                continue;
            }
            *votes.entry(rounded).or_insert(0) += 1;
        }

        if votes.is_empty() {
            return false;
        }

        // Denominator includes filtered voters too: a peer that sent a
        // garbage close_time still counts as "a voter that didn't agree
        // with us", so a single surviving honest peer can't flip our
        // position when the cohort is mostly garbage.
        let total: u32 = (self.peer_positions.len() as u32).saturating_add(1);

        // Find best bucket; tiebreak by latest.
        let mut best_bucket = 0u32;
        let mut best_count = 0u32;
        for (bucket, count) in &votes {
            if *count > best_count || (*count == best_count && *bucket > best_bucket) {
                best_bucket = *bucket;
                best_count = *count;
            }
        }
        // Strict majority (>50%) before realigning. Below that, leave
        // our_position alone so the disagreement signal survives.
        // Exception: explicit 2-voter UNL (us + 1 peer) where 1-1 ties
        // would block realignment forever and force max_consensus_rounds
        // force-accept every round (~12s/ledger). In that case adopt the
        // latest bucket on ties so both positions converge in one round.
        // Gated on a non-empty UNL so unit tests with implicit voters
        // (empty UNL) keep rippled-compat disagreement-preserve semantics.
        let allow_tie_realign = total == 2 && !self.unl.is_empty();
        if best_count * 2 < total || (!allow_tie_realign && best_count * 2 == total) {
            return false;
        }
        if let Some(ref mut pos) = self.our_position {
            if pos.close_time != best_bucket {
                tracing::debug!(
                    "consensus: realigning our close_time {} -> {} (majority bucket)",
                    pos.close_time,
                    best_bucket
                );
                pos.close_time = best_bucket;
                return true;
            }
        }
        false
    }

    /// Whether the Establish phase has run at least
    /// `params.min_consensus_time_ms`. Returns `true` when the floor is
    /// disabled (`0`), when there is no recorded Establish start, or once
    /// enough wall-clock time has elapsed. Mirrors rippled's
    /// `ledgerMIN_CONSENSUS` gate on declaring consensus.
    fn min_consensus_time_elapsed(&self) -> bool {
        let floor = self.params.min_consensus_time_ms;
        if floor == 0 {
            return true;
        }
        match self.establish_started_at {
            Some(started) => started.elapsed().as_millis() as u64 >= floor,
            None => true,
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
        let close_realigned = self.align_close_time_with_peers();

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
                // Rebuild from blobs so the new set can still be served.
                let items: Vec<(Hash256, Vec<u8>)> = new_txs
                    .iter()
                    .map(|id| {
                        let blob = self.tx_blobs.get(id).cloned().unwrap_or_default();
                        (*id, blob)
                    })
                    .collect();
                *our_set = TxSet::from_items(items);
                if let Some(ref mut pos) = self.our_position {
                    pos.tx_set_hash = our_set.hash;
                    pos.prop_seq += 1;
                    // Re-publish the rebuilt set so peers can acquire it.
                    self.adapter.publish_tx_set(our_set);
                    self.adapter.share_position(pos);
                    self.last_share_at = Some(Instant::now());
                }
                // Update dispute our_vote to match new reality
                for dispute in self.disputes.values_mut() {
                    let in_set = our_set.txs.contains(&dispute.tx_hash);
                    dispute.our_vote = in_set;
                }
            }
        }

        // Periodic re-broadcast: if our close_time was realigned this tick OR
        // at least `propose_interval_ms` has passed since our last share,
        // push our current position so peers in a slower idle phase
        // (rippled's 20s vs our 2s) see a fresh proposal within their
        // Establish window. Without this, peers count us only when our
        // single initial proposal happens to land mid-Establish (issue #76
        // — `prev_proposers` oscillating 1↔3). Time-based so the cadence is
        // unaffected by how often `converge()` is polled.
        let refresh_due = self
            .last_share_at
            .is_none_or(|t| t.elapsed().as_millis() as u64 >= self.params.propose_interval_ms);
        let needs_refresh = close_realigned || refresh_due;
        if needs_refresh && !set_changed {
            if let Some(ref mut pos) = self.our_position {
                pos.prop_seq = pos.prop_seq.saturating_add(1);
                self.adapter.share_position(pos);
                self.last_share_at = Some(Instant::now());
            }
        }

        // Count agreement on our position
        let our_hash = match &self.our_position {
            Some(p) => p.tx_set_hash,
            None => return false,
        };

        // A peer agrees with us only when it shares BOTH our transaction set
        // AND our close-time bucket. Checking the tx-set alone lets two empty
        // ledgers (tx_set_hash == ZERO) reach quorum instantly, so rxrpl would
        // accept and close with its own un-converged close_time — diverging
        // from a peer that rounds to a different bucket. Requiring the bucket
        // match holds the round open until `align_close_time_with_peers` (run
        // at the top of this method) has pulled our close_time onto the
        // peer-popular bucket. Only enforced in UNL mode; solo/unit-test paths
        // (empty UNL) keep the tx-set-only predicate.
        let resolution = self.adaptive_close_time.resolution();
        let our_ct_bucket = self
            .our_position
            .as_ref()
            .map(|p| round_close_time(p.close_time, resolution))
            .unwrap_or(0);
        let agrees = |p: &Proposal| -> bool {
            p.tx_set_hash == our_hash && round_close_time(p.close_time, resolution) == our_ct_bucket
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
                if !self.min_consensus_time_elapsed() {
                    // Quorum agrees, but hold the round open until the
                    // min-consensus floor passes so slower peers can weigh
                    // in. The node loop calls converge() again next tick.
                    return false;
                }
                self.accept();
                return true;
            }
        } else {
            // UNL mode: count only trusted members who agree on our set
            // AND our close-time bucket.
            let agreeing_unl = self
                .peer_positions
                .values()
                .filter(|p| self.unl.is_trusted(&p.node_id) && agrees(p))
                .count();
            // Self-trust check. A node never receives its own proposal as a
            // peer, so it must count itself toward quorum or it is forever
            // one short and force-accepts at `max_consensus_rounds`.
            //
            // The authoritative signal is `self.self_trusted`, set by the
            // node layer which knows every form of the local identity
            // (node-peer key, validator master key, validator ephemeral
            // signing key) and matches it against the UNL. Key-form
            // inference inside the engine is unreliable: the engine signs
            // with the ephemeral key but the UNL may list the master key.
            // The `is_trusted(...)` fallbacks below still serve solo /
            // unit-test paths that build the engine without calling
            // `set_self_trusted`.
            let self_in_unl = self.self_trusted
                || self.unl.is_trusted(&self.node_id)
                || (!self.public_key.is_empty()
                    && self
                        .unl
                        .is_trusted(&NodeId::from_public_key(&self.public_key)));
            let self_counts = if self_in_unl { 1 } else { 0 };
            if agreeing_unl + self_counts >= self.unl.quorum_threshold() {
                if !self.min_consensus_time_elapsed() {
                    // Quorum met, but hold until the min-consensus floor
                    // elapses (rippled `ledgerMIN_CONSENSUS`) so peer
                    // ProposeSets in flight are not finalized past.
                    return false;
                }
                self.accept();
                return true;
            }
        }

        // Advance the round counter at most once per `propose_interval_ms`.
        // converge() is polled every `converge_poll_interval_ms` (finer) for
        // fast quorum / min-consensus-time detection, but the round counter
        // drives threshold escalation and the `max_consensus_rounds`
        // Establish window — both must tick at the proposal cadence so the
        // window stays `max_consensus_rounds * propose_interval_ms` in real
        // time (must exceed rippled's ~20s idle interval). Without this gate
        // a fine poll would burn through all rounds in seconds.
        let round_advance_due = self
            .last_round_advance
            .is_none_or(|t| t.elapsed().as_millis() as u64 >= self.params.propose_interval_ms);
        if !round_advance_due {
            return false;
        }
        self.round += 1;
        self.last_round_advance = Some(Instant::now());

        // If we've exceeded max rounds, accept anyway — but ONLY if we have
        // at least one peer position. Otherwise we'd validate a ledger alone
        // with our own close_time, fork from peers, and force a recovery cycle
        // every round (~14s/ledger). With no peers known yet (e.g. first round
        // before peer's first proposal arrives), keep waiting; share_position
        // to nudge peers, and let the timer's stall_timeout abort if needed.
        if self.round >= self.params.max_consensus_rounds {
            if self.peer_positions.is_empty() && !self.unl.is_empty() {
                self.round = self.params.max_consensus_rounds;
                return false;
            }
            self.accept();
            return true;
        }

        false
    }

    /// Accept the consensus result: compute close time, create validation,
    /// notify adapter.
    fn accept(&mut self) {
        self.phase = ConsensusPhase::Accepted;

        // Compute effective close time. `agreed` is whether a close-time
        // consensus bucket was reached: it drives both the no-consensus
        // close flag and `previous_close_agreed` (→ next round's resolution
        // cadence). Deriving it from the same tally that picks `close_time`
        // keeps the flag and the time consistent — a flag that disagreed
        // with the time would itself fork the ledger hash cross-impl.
        let (close_time, agreed) = self.effective_close_time();
        self.accepted_close_time = Some(close_time);
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
mod tests;
