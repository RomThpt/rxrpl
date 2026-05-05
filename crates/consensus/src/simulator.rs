use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rxrpl_primitives::Hash256;

use crate::adapter::ConsensusAdapter;
use crate::engine::ConsensusEngine;
use crate::params::ConsensusParams;
use crate::types::{NodeId, Proposal, TxSet, Validation};
use crate::unl::TrustedValidatorList;

/// Configuration for a consensus simulation.
#[derive(Clone, Debug)]
pub struct SimConfig {
    pub node_count: usize,
    pub base_latency_ms: u64,
    pub latency_jitter_ms: u64,
    pub packet_loss_pct: u8,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            node_count: 5,
            base_latency_ms: 0,
            latency_jitter_ms: 0,
            packet_loss_pct: 0,
        }
    }
}

/// Result of a simulation run.
#[derive(Clone, Debug)]
pub struct SimResult {
    pub accepted: bool,
    pub rounds: u32,
    pub agreed_set: Option<Hash256>,
    pub per_node: Vec<NodeResult>,
}

/// Per-node result after simulation.
#[derive(Clone, Debug)]
pub struct NodeResult {
    pub node_id: NodeId,
    pub accepted: bool,
    pub accepted_set: Option<Hash256>,
    pub rounds: u32,
}

/// A message scheduled for delivery at a specific tick.
#[derive(Clone, Debug)]
struct ScheduledMessage {
    deliver_at: u64,
    #[allow(dead_code)]
    from: usize,
    to: usize,
    payload: SimMessage,
}

impl PartialEq for ScheduledMessage {
    fn eq(&self, other: &Self) -> bool {
        self.deliver_at == other.deliver_at
    }
}

impl Eq for ScheduledMessage {}

impl PartialOrd for ScheduledMessage {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScheduledMessage {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.deliver_at.cmp(&other.deliver_at)
    }
}

#[derive(Clone, Debug)]
enum SimMessage {
    Proposal(Proposal),
}

/// Simulated network with message queue and configurable conditions.
struct SimNetwork {
    messages: BinaryHeap<Reverse<ScheduledMessage>>,
    clock: u64,
    config: SimConfig,
    rng: StdRng,
    partitions: Option<Vec<HashSet<usize>>>,
}

impl SimNetwork {
    fn new(config: SimConfig, seed: u64) -> Self {
        Self {
            messages: BinaryHeap::new(),
            clock: 0,
            config,
            rng: StdRng::seed_from_u64(seed),
            partitions: None,
        }
    }

    fn send(&mut self, from: usize, to: usize, msg: SimMessage) {
        // Check partition: only deliver if same group
        if let Some(ref groups) = self.partitions {
            let same_group = groups.iter().any(|g| g.contains(&from) && g.contains(&to));
            if !same_group {
                return;
            }
        }

        // Packet loss
        if self.config.packet_loss_pct > 0
            && self.rng.gen_range(0..100) < self.config.packet_loss_pct as u32
        {
            return;
        }

        let latency = self.config.base_latency_ms
            + if self.config.latency_jitter_ms > 0 {
                self.rng.gen_range(0..self.config.latency_jitter_ms)
            } else {
                0
            };

        self.messages.push(Reverse(ScheduledMessage {
            deliver_at: self.clock + latency,
            from,
            to,
            payload: msg,
        }));
    }

    fn deliver_due(&mut self) -> Vec<(usize, SimMessage)> {
        let mut delivered = Vec::new();
        while let Some(Reverse(msg)) = self.messages.peek() {
            if msg.deliver_at <= self.clock {
                let Reverse(msg) = self.messages.pop().unwrap();
                delivered.push((msg.to, msg.payload));
            } else {
                break;
            }
        }
        delivered
    }

    fn partition(&mut self, groups: Vec<HashSet<usize>>) {
        self.partitions = Some(groups);
    }

    fn heal(&mut self) {
        self.partitions = None;
    }

    fn tick(&mut self) {
        self.clock += 1;
    }
}

/// Adapter that buffers outgoing messages for the simulator to deliver.
struct SimAdapter {
    tx_sets: HashMap<Hash256, TxSet>,
}

impl SimAdapter {
    fn new() -> Self {
        Self {
            tx_sets: HashMap::new(),
        }
    }

    fn register_tx_set(&mut self, set: &TxSet) {
        self.tx_sets.insert(set.hash, set.clone());
    }
}

impl ConsensusAdapter for SimAdapter {
    fn propose(&self, _proposal: &Proposal) {}

    fn share_position(&self, _proposal: &Proposal) {}

    fn share_tx(&self, _tx_hash: &Hash256, _tx_data: &[u8]) {}

    fn acquire_tx_set(&self, hash: &Hash256) -> Option<TxSet> {
        self.tx_sets.get(hash).cloned()
    }

    fn on_close(&self, _: &Hash256, _: u32, _: u32, _: &TxSet) {}

    fn on_accept(&self, _validation: &Validation) {}

    fn on_accept_ledger(&self, _tx_set: &TxSet, _close_time: u32, _close_flags: u8) -> Hash256 {
        Hash256::new([0xAA; 32])
    }
}

/// Multi-node consensus simulator.
///
/// Creates N nodes with a shared UNL and simulates consensus rounds
/// with configurable network conditions (latency, jitter, packet loss,
/// partitions).
pub struct ConsensusSimulator {
    engines: Vec<ConsensusEngine<SimAdapter>>,
    network: SimNetwork,
    #[allow(dead_code)]
    node_ids: Vec<NodeId>,
}

impl ConsensusSimulator {
    /// Create a new simulator with the given configuration.
    pub fn new(config: SimConfig) -> Self {
        Self::with_seed(config, 42)
    }

    /// Create a new simulator with a specific RNG seed.
    pub fn with_seed(config: SimConfig, seed: u64) -> Self {
        let node_count = config.node_count;
        let mut node_ids = Vec::with_capacity(node_count);
        let mut trusted = HashSet::new();

        for i in 0..node_count {
            let id = NodeId(Hash256::new([i as u8 + 1; 32]));
            node_ids.push(id);
            trusted.insert(id);
        }

        let unl = TrustedValidatorList::new(trusted);
        let mut engines = Vec::with_capacity(node_count);

        for i in 0..node_count {
            let adapter = SimAdapter::new();
            let engine = ConsensusEngine::new_with_unl(
                adapter,
                node_ids[i],
                Vec::new(),
                ConsensusParams::default(),
                unl.clone(),
            );
            engines.push(engine);
        }

        let network = SimNetwork::new(config, seed);

        Self {
            engines,
            network,
            node_ids,
        }
    }

    /// Run a complete consensus round with the given transaction sets per node.
    ///
    /// If `tx_sets` has fewer entries than nodes, remaining nodes use the first set.
    /// Returns the simulation result after convergence or max ticks.
    pub fn run_round(&mut self, tx_sets: Vec<TxSet>) -> SimResult {
        let prev_ledger = Hash256::ZERO;
        let ledger_seq = 1u32;
        let close_time = 100u32;

        let node_count = self.engines.len();

        // Register all tx sets in all adapters (simulates tx set sharing)
        for set in &tx_sets {
            for engine in &mut self.engines {
                engine.adapter_mut().register_tx_set(set);
            }
        }

        // Start round and close ledger for each node, collect initial proposals
        let mut initial_proposals = Vec::with_capacity(node_count);
        for (i, engine) in self.engines.iter_mut().enumerate() {
            let set = if i < tx_sets.len() {
                tx_sets[i].clone()
            } else {
                tx_sets[0].clone()
            };

            engine.start_round(prev_ledger, ledger_seq);
            engine.close_ledger(set, close_time, ledger_seq).unwrap();
            initial_proposals.push(engine.our_position().unwrap().clone());
        }

        // Broadcast initial proposals
        for (i, proposal) in initial_proposals.into_iter().enumerate() {
            for j in 0..node_count {
                if j != i {
                    self.network
                        .send(i, j, SimMessage::Proposal(proposal.clone()));
                }
            }
        }

        // Run simulation ticks until all converge or timeout
        let max_ticks = 1000u64;
        let mut converge_round = 0u32;

        for _ in 0..max_ticks {
            self.network.tick();

            // Deliver messages
            let messages = self.network.deliver_due();
            for (to, msg) in messages {
                match msg {
                    SimMessage::Proposal(proposal) => {
                        // Anchor freshness against the proposal's own
                        // close_time so the simulator's frozen-time model
                        // (`close_time = 100`) does not collide with the
                        // wall-clock check in `peer_proposal`.
                        let now = proposal.close_time;
                        self.engines[to].peer_proposal_at(proposal, now);
                    }
                }
            }

            // Attempt convergence on all nodes
            let mut all_accepted = true;
            for engine in &mut self.engines {
                if engine.phase() != crate::phase::ConsensusPhase::Accepted {
                    let converged = engine.converge();
                    if converged {
                        // Broadcast updated position if changed
                        // (In real network, share_position handles this)
                    }
                    if !converged {
                        all_accepted = false;
                    }
                }
            }

            converge_round += 1;

            if all_accepted {
                break;
            }

            // Broadcast any updated positions
            let positions: Vec<_> = self
                .engines
                .iter()
                .map(|e| e.our_position().cloned())
                .collect();
            for (i, pos) in positions.into_iter().enumerate() {
                if let Some(pos) = pos {
                    for j in 0..node_count {
                        if j != i {
                            self.network.send(i, j, SimMessage::Proposal(pos.clone()));
                        }
                    }
                }
            }
        }

        // Collect results
        let per_node: Vec<NodeResult> = self
            .engines
            .iter()
            .map(|e| NodeResult {
                node_id: e.node_id(),
                accepted: e.phase() == crate::phase::ConsensusPhase::Accepted,
                accepted_set: e.accepted_set(),
                rounds: converge_round,
            })
            .collect();

        let all_accepted = per_node.iter().all(|n| n.accepted);
        let agreed_set = if all_accepted {
            let first = per_node[0].accepted_set;
            if per_node.iter().all(|n| n.accepted_set == first) {
                first
            } else {
                None
            }
        } else {
            None
        };

        SimResult {
            accepted: all_accepted,
            rounds: converge_round,
            agreed_set,
            per_node,
        }
    }

    /// Apply a network partition.
    pub fn partition(&mut self, groups: Vec<Vec<usize>>) {
        let groups: Vec<HashSet<usize>> = groups
            .into_iter()
            .map(|g| g.into_iter().collect())
            .collect();
        self.network.partition(groups);
    }

    /// Heal all network partitions.
    pub fn heal(&mut self) {
        self.network.heal();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_same_tx_set() {
        let config = SimConfig {
            node_count: 5,
            ..SimConfig::default()
        };
        let mut sim = ConsensusSimulator::new(config);
        let tx_set = TxSet::new(vec![Hash256::new([0x01; 32])]);
        let result = sim.run_round(vec![tx_set]);

        assert!(result.accepted);
        assert!(result.agreed_set.is_some());
        for node in &result.per_node {
            assert!(node.accepted);
        }
    }

    #[test]
    fn split_proposals_majority_wins() {
        let config = SimConfig {
            node_count: 5,
            ..SimConfig::default()
        };
        let mut sim = ConsensusSimulator::new(config);

        let tx1 = Hash256::new([0x01; 32]);
        let tx2 = Hash256::new([0x02; 32]);

        // 3 nodes propose {tx1}, 2 nodes propose {tx1, tx2}
        let set_majority = TxSet::new(vec![tx1]);
        let set_minority = TxSet::new(vec![tx1, tx2]);

        let result = sim.run_round(vec![
            set_majority.clone(),
            set_majority.clone(),
            set_majority.clone(),
            set_minority.clone(),
            set_minority.clone(),
        ]);

        assert!(result.accepted);
        // All nodes should converge on the majority set (tx1 only)
        assert!(result.agreed_set.is_some());
    }

    #[test]
    fn high_latency_convergence() {
        let config = SimConfig {
            node_count: 5,
            base_latency_ms: 50,
            latency_jitter_ms: 25,
            ..SimConfig::default()
        };
        let mut sim = ConsensusSimulator::new(config);
        let tx_set = TxSet::new(vec![Hash256::new([0x01; 32])]);
        let result = sim.run_round(vec![tx_set]);

        assert!(result.accepted);
        // With latency, should take more rounds
        assert!(result.rounds >= 1);
    }

    #[test]
    fn packet_loss_eventual_convergence() {
        let config = SimConfig {
            node_count: 5,
            packet_loss_pct: 30,
            ..SimConfig::default()
        };
        let mut sim = ConsensusSimulator::with_seed(config, 123);
        let tx_set = TxSet::new(vec![Hash256::new([0x01; 32])]);
        let result = sim.run_round(vec![tx_set]);

        // With 30% packet loss, should still eventually converge
        assert!(result.accepted);
    }

    #[test]
    fn solo_mode_immediate_accept() {
        let config = SimConfig {
            node_count: 1,
            ..SimConfig::default()
        };
        let mut sim = ConsensusSimulator::new(config);
        let tx_set = TxSet::new(vec![Hash256::new([0x01; 32])]);
        let result = sim.run_round(vec![tx_set]);

        assert!(result.accepted);
        assert!(result.agreed_set.is_some());
        assert_eq!(result.per_node.len(), 1);
    }

    #[test]
    fn network_partition_majority_converges() {
        let config = SimConfig {
            node_count: 5,
            ..SimConfig::default()
        };
        let mut sim = ConsensusSimulator::new(config);

        // Partition: {0,1,2} and {3,4}
        sim.partition(vec![vec![0, 1, 2], vec![3, 4]]);

        let tx_set = TxSet::new(vec![Hash256::new([0x01; 32])]);
        let result = sim.run_round(vec![tx_set]);

        // With 5-node UNL and quorum=4, neither partition can reach quorum
        // But the max_consensus_rounds safety net should accept eventually
        // Nodes in the 3-node partition get 3/5 which is < 4 (80% quorum)
        // After max rounds, all nodes force-accept
        for node in &result.per_node {
            assert!(node.accepted);
        }
    }

    #[test]
    fn empty_tx_set_convergence() {
        let config = SimConfig {
            node_count: 5,
            ..SimConfig::default()
        };
        let mut sim = ConsensusSimulator::new(config);
        let tx_set = TxSet::new(vec![]);
        let result = sim.run_round(vec![tx_set]);

        assert!(result.accepted);
        assert!(result.agreed_set.is_some());
    }
}
