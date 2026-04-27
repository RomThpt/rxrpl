use std::collections::HashMap;

use rxrpl_primitives::Hash256;

use crate::adapter::ConsensusAdapter;
use crate::close_resolution::{next_resolution, AdaptiveCloseTime};
use crate::error::ConsensusError;
use crate::negative_unl::{NegativeUnlChange, NegativeUnlTracker};
use crate::params::ConsensusParams;
use crate::phase::ConsensusPhase;
use crate::types::{DisputedTx, NodeId, Proposal, TxSet, Validation};
use crate::unl::TrustedValidatorList;

/// Threshold percentage of trusted validators referencing a different
/// `prev_ledger` before we consider switching chains.
const WRONG_PREV_LEDGER_THRESHOLD: u32 = 60;

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
    wrong_prev_ledger_votes: HashMap<NodeId, Hash256>,
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
        Self {
            adapter,
            params,
            phase: ConsensusPhase::Open,
            our_position: None,
            our_set: None,
            peer_positions: HashMap::new(),
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
            future_proposals: HashMap::new(),
            adaptive_close_time,
            previous_close_agreed: true,
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
    /// Returns `Some(WrongPrevLedgerDetected)` when more than 60% of the
    /// trusted peers that sent proposals disagree with our `prev_ledger`.
    /// The caller should abort the current round and switch to the
    /// preferred ledger.
    ///
    /// In solo mode (empty UNL) this always returns `None`.
    pub fn check_wrong_prev_ledger(&self) -> Option<WrongPrevLedgerDetected> {
        if self.unl.is_empty() {
            return None;
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

    /// Get the disputes map.
    pub fn disputes(&self) -> &HashMap<Hash256, DisputedTx> {
        &self.disputes
    }

    /// Start a new consensus round for the next ledger.
    pub fn start_round(&mut self, prev_ledger: Hash256, ledger_seq: u32) {
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
        self.disputes.clear();
        self.round = 0;
        self.prev_ledger = prev_ledger;
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

        // Replay any proposals buffered while we were in Open phase
        let pending = std::mem::take(&mut self.pending_proposals);
        for p in pending {
            self.peer_proposal(p);
        }

        Ok(())
    }

    /// Receive a proposal from a peer.
    pub fn peer_proposal(&mut self, proposal: Proposal) {
        if self.phase != ConsensusPhase::Establish {
            // Buffer proposals received outside Establish phase
            self.pending_proposals.push(proposal);
            return;
        }

        // UNL filtering: if UNL is non-empty, only accept trusted nodes
        if !self.unl.is_empty() && !self.unl.is_trusted(&proposal.node_id) {
            tracing::debug!("rejected proposal from untrusted {:?}", proposal.node_id);
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

        if self.peer_positions.is_empty() {
            return round_close_time(our_time, resolution);
        }

        // Tally votes per rounded close-time bucket.
        let mut votes: HashMap<u32, u32> = HashMap::new();
        let our_bucket = round_close_time(our_time, resolution);
        *votes.entry(our_bucket).or_insert(0) += 1;
        for peer in self.peer_positions.values() {
            let peer_bucket = round_close_time(peer.close_time, resolution);
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
        let our_time = self.our_position.as_ref().map(|p| p.close_time).unwrap_or(0);
        let our_bucket = round_close_time(our_time, resolution);

        let mut votes: HashMap<u32, u32> = HashMap::new();
        *votes.entry(our_bucket).or_insert(0) += 1;
        for peer in self.peer_positions.values() {
            let b = round_close_time(peer.close_time, resolution);
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

        // Resolve disputes and update our set if needed
        let mut set_changed = false;
        if let Some(ref mut our_set) = self.our_set {
            let mut new_txs = our_set.txs.clone();

            for dispute in self.disputes.values() {
                let should_include = dispute.should_include(threshold);
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

/// Round a close time to the nearest resolution boundary.
pub fn round_close_time(t: u32, resolution: u32) -> u32 {
    if resolution == 0 {
        return t;
    }
    (t + resolution / 2) / resolution * resolution
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
        engine.peer_proposal(Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set_bc.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });
        engine.peer_proposal(Proposal {
            node_id: node_c,
            public_key: vec![0x02; 33],
            tx_set_hash: set_bc.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });

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

        engine.peer_proposal(Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set_b.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });
        engine.peer_proposal(Proposal {
            node_id: node_c,
            public_key: vec![0x02; 33],
            tx_set_hash: set_b.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });

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

        engine.peer_proposal(Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });

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

        engine.peer_proposal(Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: Hash256::new([0xFF; 32]), // unknown
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });

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

        engine.peer_proposal(Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set.hash,
            close_time: 200,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });
        engine.peer_proposal(Proposal {
            node_id: node_c,
            public_key: vec![0x02; 33],
            tx_set_hash: set.hash,
            close_time: 150,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });

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
    fn close_time_spread_sets_flag() {
        let adapter = SimpleAdapter;
        let node_a = NodeId(Hash256::new([0xA0; 32]));
        let node_b = NodeId(Hash256::new([0xB0; 32]));

        let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());
        engine.start_round(Hash256::ZERO, 1);
        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        // Peer proposes time far away (spread > 30s resolution)
        engine.peer_proposal(Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set.hash,
            close_time: 200,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });

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

    fn node(id: u8) -> NodeId {
        NodeId(Hash256::new([id; 32]))
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
        engine.peer_proposal(proposal_for(node(99), set.hash, Hash256::ZERO, 1));
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
        engine.peer_proposal(proposal_for(node(2), set.hash, Hash256::ZERO, 1));
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
        engine.peer_proposal(proposal_for(node(2), set.hash, bad_prev, 1));
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
        engine.peer_proposal(proposal_for(node(2), set.hash, Hash256::ZERO, 1));
        engine.peer_proposal(proposal_for(node(3), set.hash, Hash256::ZERO, 1));

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

        engine.peer_proposal(proposal_for(node(2), set.hash, Hash256::ZERO, 1));
        engine.peer_proposal(proposal_for(node(3), set.hash, Hash256::ZERO, 1));
        engine.peer_proposal(proposal_for(node(4), set.hash, Hash256::ZERO, 1));

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
        engine.peer_proposal(proposal_for(node(2), set.hash, future_prev, 2));

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
        engine.peer_proposal(proposal_for(node(2), set.hash, future_prev, 2));
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
        engine.peer_proposal(proposal_for(node(2), set.hash, stale_prev, 2));
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
        engine.peer_proposal(proposal_for(node(2), set.hash, prev, 2));
        engine.peer_proposal(proposal_for(node(2), set.hash, prev, 2));
        engine.peer_proposal(proposal_for(node(2), set.hash, prev, 2));
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
            engine.peer_proposal(p);
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
        engine.peer_proposal(proposal_for(node(2), set.hash, bad_prev, 1));

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
        engine.peer_proposal(proposal_for(node(2), set.hash, peer_prev, 1));
        engine.peer_proposal(proposal_for(node(3), set.hash, peer_prev, 1));
        engine.peer_proposal(proposal_for(node(4), set.hash, peer_prev, 1));
        engine.peer_proposal(proposal_for(node(5), set.hash, peer_prev, 1));

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
        engine.peer_proposal(proposal_for(node(2), set.hash, peer_prev, 1));
        // 3 peers agree
        engine.peer_proposal(proposal_for(node(3), set.hash, Hash256::ZERO, 1));
        engine.peer_proposal(proposal_for(node(4), set.hash, Hash256::ZERO, 1));
        engine.peer_proposal(proposal_for(node(5), set.hash, Hash256::ZERO, 1));

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
        engine.peer_proposal(proposal_for(node(2), set.hash, peer_prev, 1));
        engine.peer_proposal(proposal_for(node(3), set.hash, peer_prev, 1));
        engine.peer_proposal(proposal_for(node(4), set.hash, peer_prev, 1));
        // 2 peers agree
        engine.peer_proposal(proposal_for(node(5), set.hash, Hash256::ZERO, 1));

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
        engine.peer_proposal(proposal_for(node(50), set.hash, peer_prev, 1));
        engine.peer_proposal(proposal_for(node(51), set.hash, peer_prev, 1));

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
        engine.peer_proposal(proposal_for(node(2), set.hash, peer_prev, 1));
        engine.peer_proposal(proposal_for(node(3), set.hash, peer_prev, 1));

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
        engine.peer_proposal(proposal_for(node(2), set.hash, prev_a, 1));
        engine.peer_proposal(proposal_for(node(3), set.hash, prev_a, 1));
        engine.peer_proposal(proposal_for(node(4), set.hash, prev_b, 1));
        engine.peer_proposal(proposal_for(node(5), set.hash, prev_b, 1));

        // 2/4 = 50% < 60% -> no detection
        assert_eq!(engine.check_wrong_prev_ledger(), None);
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
            engine.peer_proposal(Proposal {
                node_id: node_b,
                public_key: vec![0x02; 33],
                tx_set_hash: set.hash,
                close_time: 100,
                prop_seq: 0,
                ledger_seq: seq,
                prev_ledger: Hash256::ZERO,
                signature: None,
            });

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
        engine.peer_proposal(Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set.hash,
            close_time: 200, // spread of 100 > 30
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });
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
        engine.peer_proposal(Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set.hash,
            close_time: 105,
            prop_seq: 0,
            ledger_seq: 9,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });
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
}
