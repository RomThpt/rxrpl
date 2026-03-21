use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use openssl::ssl::{SslAcceptor, SslConnector};
use rxrpl_consensus::types::{Proposal, Validation};
use rxrpl_p2p_proto::MessageType;
use rxrpl_p2p_proto::codec::{PeerCodec, PeerMessage};
use rxrpl_primitives::Hash256;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{RwLock, mpsc};
use tokio_util::codec::Framed;

use crate::command::OverlayCommand;
use crate::discovery::PeerDiscovery;
use crate::error::OverlayError;
use crate::event::PeerEvent;
use crate::handshake;
use crate::identity::NodeIdentity;
use crate::ledger_provider::LedgerProvider;
use crate::ledger_sync::LedgerSyncer;
use crate::peer_handle::PeerHandle;
use crate::peer_loop;
use crate::peer_set::{PeerInfo, PeerSet};
use crate::proto_convert;
use crate::relay::RelayFilter;
use crate::reputation::PeerReputation;
use crate::tls::{self, PeerStream};

/// Messages forwarded from the overlay to the consensus layer.
pub enum ConsensusMessage {
    Proposal(Proposal),
    Validation(Validation),
    Transaction {
        hash: Hash256,
        data: Vec<u8>,
    },
    StatusChange {
        from: Hash256,
        ledger_seq: u32,
        ledger_hash: Hash256,
    },
    LedgerData {
        hash: Hash256,
        seq: u32,
        nodes: Vec<(Vec<u8>, Vec<u8>)>,
    },
    ValidatorListReceived {
        validator_count: usize,
    },
}

/// Configuration for the peer manager.
pub struct PeerManagerConfig {
    pub listen_port: u16,
    pub max_peers: usize,
    pub seeds: Vec<String>,
    pub fixed_peers: Vec<String>,
    pub network_id: u32,
    pub tls_server: Arc<SslAcceptor>,
    pub tls_client: Arc<SslConnector>,
}

/// Central P2P network manager.
///
/// Accepts inbound connections, manages outbound connections,
/// and dispatches messages between peers and the consensus layer.
pub struct PeerManager {
    identity: Arc<NodeIdentity>,
    config: PeerManagerConfig,
    seeds: Vec<String>,
    peer_set: Arc<PeerSet>,
    peer_handles: HashMap<Hash256, PeerHandle>,
    relay_filter: RelayFilter,
    ledger_seq: Arc<AtomicU32>,
    ledger_hash: Arc<RwLock<Hash256>>,
    cmd_rx: mpsc::UnboundedReceiver<OverlayCommand>,
    cmd_tx_internal: mpsc::UnboundedSender<OverlayCommand>,
    event_rx: mpsc::UnboundedReceiver<PeerEvent>,
    event_tx: mpsc::UnboundedSender<PeerEvent>,
    consensus_tx: mpsc::UnboundedSender<ConsensusMessage>,
    ledger_provider: Option<Arc<dyn LedgerProvider>>,
    ledger_syncer: LedgerSyncer,
    discovery: Option<Arc<PeerDiscovery>>,
    server_event_tx: Option<tokio::sync::broadcast::Sender<serde_json::Value>>,
}

impl PeerManager {
    pub fn new(
        identity: Arc<NodeIdentity>,
        config: PeerManagerConfig,
        ledger_seq: Arc<AtomicU32>,
        ledger_hash: Arc<RwLock<Hash256>>,
    ) -> (
        Self,
        mpsc::UnboundedSender<OverlayCommand>,
        mpsc::UnboundedReceiver<ConsensusMessage>,
    ) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cmd_tx_internal = cmd_tx.clone();
        let (consensus_tx, consensus_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let peer_set = Arc::new(PeerSet::new(config.max_peers));

        let seeds = config.seeds.clone();
        let mgr = Self {
            identity,
            seeds,
            config,
            peer_set,
            peer_handles: HashMap::new(),
            relay_filter: RelayFilter::new(65536),
            ledger_seq,
            ledger_hash,
            cmd_rx,
            cmd_tx_internal,
            event_rx,
            event_tx,
            consensus_tx,
            ledger_provider: None,
            ledger_syncer: LedgerSyncer::new(),
            discovery: None,
            server_event_tx: None,
        };

        (mgr, cmd_tx, consensus_rx)
    }

    /// Set a ledger provider for serving GetLedger requests.
    pub fn set_ledger_provider(&mut self, provider: Arc<dyn LedgerProvider>) {
        self.ledger_provider = Some(provider);
    }

    /// Set the event sender for emitting overlay events as JSON values.
    ///
    /// Used to bridge overlay events (peer connect/disconnect, validations)
    /// to the RPC server's subscription system without a direct dependency.
    pub fn set_event_sender(&mut self, tx: tokio::sync::broadcast::Sender<serde_json::Value>) {
        self.server_event_tx = Some(tx);
    }

    /// Run the peer manager event loop.
    pub async fn run(mut self) -> Result<(), OverlayError> {
        let bind_addr = format!("0.0.0.0:{}", self.config.listen_port);
        let listener = TcpListener::bind(&bind_addr).await?;
        tracing::info!("P2P listening on {}", bind_addr);

        // Spawn fixed peer connectors with retry
        for addr in &self.config.fixed_peers {
            self.spawn_fixed_peer_connector(addr.clone());
        }

        // Create and launch peer discovery using seeds + fixed_peers
        if self.discovery.is_none() {
            let mut all_seeds: Vec<String> = Vec::new();
            // Seeds from config (includes defaults like r.ripple.com)
            for seed in &self.seeds {
                if !all_seeds.contains(seed) {
                    all_seeds.push(seed.clone());
                }
            }
            // Fixed peers also act as seeds
            for fp in &self.config.fixed_peers {
                if !all_seeds.contains(fp) {
                    all_seeds.push(fp.clone());
                }
            }
            if !all_seeds.is_empty() {
                self.discovery = Some(Arc::new(PeerDiscovery::new(
                    all_seeds,
                    Arc::clone(&self.peer_set),
                    self.cmd_tx_internal.clone(),
                    self.config.max_peers,
                )));
            }
        }
        if let Some(ref discovery) = self.discovery {
            let disc = Arc::clone(discovery);
            tokio::spawn(async move {
                disc.bootstrap().await;
                disc.run_loop().await;
            });
        }

        let mut sync_interval = tokio::time::interval(Duration::from_secs(5));
        sync_interval.tick().await; // skip first immediate tick

        let mut reputation_interval = tokio::time::interval(Duration::from_secs(30));
        reputation_interval.tick().await; // skip first immediate tick

        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, addr)) => {
                            tracing::debug!("inbound connection from {}", addr);
                            self.spawn_inbound_handler(stream, addr.to_string());
                        }
                        Err(e) => {
                            tracing::error!("accept error: {}", e);
                        }
                    }
                }

                Some(cmd) = self.cmd_rx.recv() => {
                    self.handle_command(cmd);
                }

                Some(event) = self.event_rx.recv() => {
                    self.handle_event(event);
                }

                _ = sync_interval.tick() => {
                    self.check_sync();
                }

                _ = reputation_interval.tick() => {
                    self.check_peer_reputations();
                }
            }
        }
    }

    fn spawn_fixed_peer_connector(&self, addr: String) {
        let identity = Arc::clone(&self.identity);
        let network_id = self.config.network_id;
        let ledger_seq = Arc::clone(&self.ledger_seq);
        let ledger_hash = Arc::clone(&self.ledger_hash);
        let event_tx = self.event_tx.clone();
        let peer_set = Arc::clone(&self.peer_set);
        let tls_client = Arc::clone(&self.config.tls_client);

        tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            let max_backoff = Duration::from_secs(30);

            loop {
                match try_connect_outbound(
                    &addr,
                    &identity,
                    network_id,
                    &ledger_seq,
                    &ledger_hash,
                    &event_tx,
                    &peer_set,
                    &tls_client,
                )
                .await
                {
                    Ok(node_id) => {
                        tracing::info!("connected to fixed peer {} ({})", addr, node_id);
                        backoff = Duration::from_secs(1);
                        // Wait for disconnect before retrying.
                        // The peer_loop will send Disconnected, and we reconnect on next iteration.
                        loop {
                            // Check periodically if we're still connected
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            if peer_set.get(&node_id).is_none() {
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to connect to {}: {}", addr, e);
                    }
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(max_backoff);
            }
        });
    }

    fn spawn_inbound_handler(&self, stream: TcpStream, addr: String) {
        let identity = Arc::clone(&self.identity);
        let network_id = self.config.network_id;
        let ledger_seq = Arc::clone(&self.ledger_seq);
        let ledger_hash = Arc::clone(&self.ledger_hash);
        let event_tx = self.event_tx.clone();
        let peer_set = Arc::clone(&self.peer_set);
        let tls_server = self.config.tls_server.clone();

        tokio::spawn(async move {
            if let Err(e) = try_accept_inbound(
                stream,
                &addr,
                &identity,
                network_id,
                &ledger_seq,
                &ledger_hash,
                &event_tx,
                &peer_set,
                &tls_server,
            )
            .await
            {
                tracing::debug!("inbound handshake failed from {}: {}", addr, e);
            }
        });
    }

    fn handle_command(&self, cmd: OverlayCommand) {
        match cmd {
            OverlayCommand::Broadcast { msg_type, payload } => {
                for handle in self.peer_handles.values() {
                    let _ = handle.tx.try_send(PeerMessage {
                        msg_type,
                        payload: payload.clone(),
                    });
                }
            }
            OverlayCommand::SendTo {
                node_id,
                msg_type,
                payload,
            } => {
                if let Some(handle) = self.peer_handles.get(&node_id) {
                    let _ = handle.tx.try_send(PeerMessage { msg_type, payload });
                }
            }
            OverlayCommand::RequestLedger { seq, hash } => {
                self.send_get_ledger(seq, hash);
            }
            OverlayCommand::ConnectTo { addr } => {
                let identity = Arc::clone(&self.identity);
                let network_id = self.config.network_id;
                let ledger_seq = Arc::clone(&self.ledger_seq);
                let ledger_hash = Arc::clone(&self.ledger_hash);
                let event_tx = self.event_tx.clone();
                let peer_set = Arc::clone(&self.peer_set);
                let tls_client = Arc::clone(&self.config.tls_client);

                tokio::spawn(async move {
                    if let Err(e) = try_connect_outbound(
                        &addr,
                        &identity,
                        network_id,
                        &ledger_seq,
                        &ledger_hash,
                        &event_tx,
                        &peer_set,
                        &tls_client,
                    )
                    .await
                    {
                        tracing::warn!("connect to {} failed: {}", addr, e);
                    }
                });
            }
        }
    }

    fn handle_event(&mut self, event: PeerEvent) {
        match event {
            PeerEvent::Connected {
                node_id,
                info,
                write_tx,
            } => {
                tracing::info!("peer {} registered ({})", node_id, info.address);
                if let Some(ref tx) = self.server_event_tx {
                    let _ = tx.send(serde_json::json!({
                        "type": "peerStatusChange",
                        "peer_id": node_id.to_string(),
                        "event": "connected",
                    }));
                }
                self.peer_handles.insert(
                    node_id,
                    PeerHandle {
                        node_id,
                        info,
                        tx: write_tx,
                    },
                );
            }
            PeerEvent::Message {
                from,
                msg_type,
                payload,
            } => {
                self.dispatch_message(from, msg_type, &payload);
            }
            PeerEvent::Disconnected { node_id } => {
                tracing::info!("peer {} disconnected", node_id);
                if let Some(ref tx) = self.server_event_tx {
                    let _ = tx.send(serde_json::json!({
                        "type": "peerStatusChange",
                        "peer_id": node_id.to_string(),
                        "event": "disconnected",
                    }));
                }
                self.peer_handles.remove(&node_id);
                self.peer_set.remove(&node_id);
            }
        }
    }

    fn dispatch_message(&mut self, from: Hash256, msg_type: MessageType, payload: &[u8]) {
        let peer_info = self.peer_set.get(&from);
        let payload_len = payload.len() as u64;

        match msg_type {
            MessageType::Hello => {
                // Hello is already handled during handshake; ignore late arrivals.
                tracing::debug!("ignoring late Hello from {}", from);
            }
            MessageType::Ping => {
                match proto_convert::decode_ping(payload) {
                    Ok(ping) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        if ping.r#type.unwrap_or(0) == 0 {
                            let pong = proto_convert::encode_ping(ping.seq.unwrap_or(0), true);
                            if let Some(handle) = self.peer_handles.get(&from) {
                                let _ = handle.tx.try_send(PeerMessage {
                                    msg_type: MessageType::Ping,
                                    payload: pong,
                                });
                            }
                        }
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::Transaction => {
                match proto_convert::decode_transaction(payload) {
                    Ok((tx_hash, tx_data)) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        if self.relay_filter.should_relay(&tx_hash) {
                            let _ = self.consensus_tx.send(ConsensusMessage::Transaction {
                                hash: tx_hash,
                                data: tx_data,
                            });
                            // Re-broadcast to other peers
                            for (id, handle) in &self.peer_handles {
                                if *id != from {
                                    let _ = handle.tx.try_send(PeerMessage {
                                        msg_type: MessageType::Transaction,
                                        payload: payload.to_vec(),
                                    });
                                }
                            }
                        }
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::ProposeSet => {
                match proto_convert::decode_propose_set(payload) {
                    Ok(proposal) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        let _ = self.consensus_tx.send(ConsensusMessage::Proposal(proposal));
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::Validation => {
                match proto_convert::decode_validation(payload) {
                    Ok(validation) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        if let Some(ref tx) = self.server_event_tx {
                            let _ = tx.send(serde_json::json!({
                                "type": "validationReceived",
                                "validator": validation.node_id.0.to_string(),
                                "ledger_hash": validation.ledger_hash.to_string(),
                                "ledger_seq": validation.ledger_seq,
                                "full": validation.full,
                            }));
                        }
                        let _ = self
                            .consensus_tx
                            .send(ConsensusMessage::Validation(validation));
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::StatusChange => {
                match proto_convert::decode_status_change(payload) {
                    Ok((ledger_hash, ledger_seq)) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                            info.ledger_seq.store(ledger_seq, Ordering::Relaxed);
                        }

                        // Trigger sync if peer is ahead
                        let our_seq = self.ledger_seq.load(Ordering::Relaxed);
                        if self.ledger_syncer.needs_sync(our_seq, ledger_seq) {
                            let requests = self.ledger_syncer.request_missing(our_seq, ledger_seq);
                            for (seq, hash) in requests {
                                self.send_get_ledger(seq, hash);
                            }
                        }

                        let _ = self.consensus_tx.send(ConsensusMessage::StatusChange {
                            from,
                            ledger_seq,
                            ledger_hash,
                        });
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::Cluster => {
                // Cluster messages are only exchanged between nodes in the same
                // cluster. Log and ignore for non-cluster peers.
                if let Some(ref info) = peer_info {
                    info.reputation.record_valid_message(payload_len);
                }
                tracing::debug!("Cluster message from {}", from);
            }
            MessageType::GetLedger => {
                if let Some(ref info) = peer_info {
                    info.reputation.record_valid_message(payload_len);
                }
                self.handle_get_ledger(from, payload);
            }
            MessageType::LedgerData => {
                match proto_convert::decode_ledger_data(payload) {
                    Ok(msg) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                            // Peer provided requested ledger data -- useful contribution
                            info.reputation.record_useful_contribution();
                        }
                        let ledger_hash_bytes = msg.ledger_hash.unwrap_or_default();
                        let hash =
                            Hash256::new(ledger_hash_bytes[..32].try_into().unwrap_or([0u8; 32]));
                        let nodes: Vec<(Vec<u8>, Vec<u8>)> = msg
                            .nodes
                            .into_iter()
                            .map(|n| (n.nodeid.unwrap_or_default(), n.nodedata.unwrap_or_default()))
                            .collect();

                        let ledger_seq = msg.ledger_seq.unwrap_or(0);

                        // Feed nodes into incremental sync if active.
                        let incremental_complete =
                            self.ledger_syncer.feed_nodes(ledger_seq, &nodes);
                        if incremental_complete {
                            tracing::info!(
                                "incremental sync complete for ledger #{} hash={}",
                                ledger_seq, hash,
                            );
                        }

                        // Notify ledger syncer about the response (full sync path).
                        if let Some(synced) =
                            self.ledger_syncer
                                .handle_response(ledger_seq, hash, nodes.clone())
                        {
                            tracing::info!(
                                "synced ledger #{} hash={} ({} nodes)",
                                synced.seq,
                                synced.hash,
                                synced.nodes.len()
                            );
                        }

                        let _ = self.consensus_tx.send(ConsensusMessage::LedgerData {
                            hash,
                            seq: ledger_seq,
                            nodes,
                        });
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::Endpoints => {
                // Try rippled TMEndpoints format first, fall back to legacy TMPeers
                let peers_result = proto_convert::decode_endpoints(payload)
                    .map(|eps| {
                        eps.into_iter()
                            .filter_map(|(endpoint, _hops)| {
                                // endpoint is "ip:port" string
                                let (ip, port_str) = endpoint.rsplit_once(':')?;
                                let port = port_str.parse::<u16>().ok()?;
                                Some((ip.to_string(), port))
                            })
                            .collect::<Vec<_>>()
                    })
                    .or_else(|_| proto_convert::decode_peers(payload));

                match peers_result {
                    Ok(peers) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        tracing::debug!(
                            "received {} peer addresses from {}",
                            peers.len(),
                            from
                        );
                        if let Some(ref discovery) = self.discovery {
                            let disc = Arc::clone(discovery);
                            let peers = peers.clone();
                            tokio::spawn(async move {
                                disc.handle_peers_response(peers).await;
                            });
                        }
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::Manifests => {
                match proto_convert::decode_manifests(payload) {
                    Ok(manifest_list) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        tracing::debug!(
                            "received {} manifests from {}",
                            manifest_list.len(),
                            from
                        );
                        // Each entry is a raw serialized manifest (rippled binary format).
                        // For now we store the count; full parsing requires SField codec.
                        if let Some(ref tx) = self.server_event_tx {
                            let _ = tx.send(serde_json::json!({
                                "type": "manifestsReceived",
                                "count": manifest_list.len(),
                                "from": from.to_string(),
                            }));
                        }
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::HaveSet => {
                match proto_convert::decode_have_set(payload) {
                    Ok(have_set) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        tracing::debug!(
                            "HaveTransactionSet from {} hash={} status={}",
                            from, have_set.hash, have_set.status
                        );
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::GetObjects => {
                if let Some(ref info) = peer_info {
                    info.reputation.record_valid_message(payload_len);
                }
                tracing::debug!("GetObjectByHash from {} ({} bytes)", from, payload_len);
                // TODO: implement object serving
            }
            MessageType::ValidatorList => {
                match proto_convert::decode_validator_list(payload) {
                    Ok(vl) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        tracing::debug!(
                            "ValidatorList v{} from {} ({} bytes manifest, {} bytes blob)",
                            vl.version.unwrap_or(0), from,
                            vl.manifest.as_ref().map(|v| v.len()).unwrap_or(0),
                            vl.blob.as_ref().map(|v| v.len()).unwrap_or(0)
                        );
                        // Try to extract validator count from blob JSON
                        if let Some(blob_bytes) = vl.blob.as_ref() {
                            if let Ok(decoded) = base64_decode_validator_blob(blob_bytes) {
                                let _ = self.consensus_tx.send(
                                    ConsensusMessage::ValidatorListReceived {
                                        validator_count: decoded,
                                    },
                                );
                            }
                        }
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::ValidatorListCollection => {
                match proto_convert::decode_validator_list_collection(payload) {
                    Ok(vlc) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        tracing::debug!(
                            "ValidatorListCollection v{} from {} ({} blobs)",
                            vlc.version.unwrap_or(0), from, vlc.blobs.len()
                        );
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::Squelch => {
                if let Some(ref info) = peer_info {
                    info.reputation.record_valid_message(payload_len);
                }
                tracing::trace!("Squelch from {}", from);
            }
            MessageType::HaveTransactions => {
                if let Some(ref info) = peer_info {
                    info.reputation.record_valid_message(payload_len);
                }
                tracing::trace!("HaveTransactions from {}", from);
            }
            MessageType::Transactions => {
                if let Some(ref info) = peer_info {
                    info.reputation.record_valid_message(payload_len);
                }
                tracing::trace!("Transactions batch from {}", from);
            }
        }
    }

    /// Check for ledger gaps and request missing ledgers from peers.
    fn check_sync(&mut self) {
        let our_seq = self.ledger_seq.load(Ordering::Relaxed);

        // Find the highest peer sequence
        let max_peer_seq = self
            .peer_handles
            .keys()
            .filter_map(|id| self.peer_set.get(id))
            .map(|info| info.ledger_seq.load(Ordering::Relaxed))
            .max()
            .unwrap_or(0);

        if self.ledger_syncer.needs_sync(our_seq, max_peer_seq) {
            let requests = self.ledger_syncer.request_missing(our_seq, max_peer_seq);
            for (seq, hash) in requests {
                self.send_get_ledger(seq, hash);
            }
        }

        // Check and retry timed-out requests
        let timed_out = self.ledger_syncer.check_timeouts(std::time::Instant::now());
        for seq in timed_out {
            tracing::debug!("ledger sync request for #{} timed out, retrying", seq);
            self.send_get_ledger(seq, None);
        }
    }

    /// Disconnect peers whose reputation has dropped below the threshold.
    fn check_peer_reputations(&mut self) {
        let bad_peers: Vec<Hash256> = self
            .peer_set
            .all_peers()
            .iter()
            .filter(|info| info.reputation.should_disconnect())
            .map(|info| info.node_id)
            .collect();

        for node_id in bad_peers {
            tracing::warn!(
                "disconnecting peer {} due to low reputation score ({})",
                node_id,
                self.peer_set
                    .get(&node_id)
                    .map(|i| i.reputation.score())
                    .unwrap_or(0),
            );
            if let Some(handle) = self.peer_handles.remove(&node_id) {
                drop(handle);
            }
            self.peer_set.remove(&node_id);
        }
    }

    /// Send a GetLedger request to the best 3 peers by reputation score.
    ///
    /// Uses weighted peer selection: peers are ranked by reputation score with
    /// a bonus for peers whose known ledger sequence is at or ahead of the target.
    ///
    /// When the ledger syncer has an active incremental sync for the target
    /// sequence, the request includes specific node hashes (delta sync).
    /// Otherwise, falls back to requesting all leaf nodes.
    fn send_get_ledger(&self, seq: u32, hash: Option<Hash256>) {
        // LedgerType enum values: LtCurrent=0, LtClosed=1, LtValidated=2, LtHash=3
        const LT_HASH: i32 = 3;

        // Check if we have missing node hashes from an active incremental sync.
        let node_ids: Vec<Vec<u8>> = self
            .ledger_syncer
            .get_missing_node_ids(seq)
            .into_iter()
            .map(|h| h.as_bytes().to_vec())
            .collect();

        let is_delta = !node_ids.is_empty();
        let payload = proto_convert::encode_get_ledger_with_nodes(
            LT_HASH,
            hash.as_ref(),
            seq,
            0,
            node_ids,
        );

        let best = self.peer_set.best_peers_for_ledger(seq, 3);
        let mut sent = 0;
        for node_id in &best {
            if let Some(handle) = self.peer_handles.get(node_id) {
                let _ = handle.tx.try_send(PeerMessage {
                    msg_type: MessageType::GetLedger,
                    payload: payload.clone(),
                });
                sent += 1;
            }
        }
        if sent == 0 {
            tracing::debug!("no peers available for GetLedger seq={}", seq);
        } else if is_delta {
            tracing::debug!(
                "sent GetLedger seq={} to {} peers (delta sync, reputation-selected)",
                seq, sent
            );
        } else {
            tracing::debug!("sent GetLedger seq={} to {} peers (reputation-selected)", seq, sent);
        }
    }

    fn handle_get_ledger(&self, from: Hash256, payload: &[u8]) {
        let req = match proto_convert::decode_get_ledger(payload) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("bad GetLedger from {}: {}", from, e);
                return;
            }
        };

        let provider = match &self.ledger_provider {
            Some(p) => p,
            None => {
                tracing::debug!("GetLedger from {} but no ledger provider", from);
                return;
            }
        };

        // LedgerType enum values: LtCurrent=0, LtClosed=1, LtValidated=2, LtHash=3
        const LT_CLOSED: i32 = 1;
        const LT_VALIDATED: i32 = 2;
        const LT_HASH: i32 = 3;

        let req_ledger_type = req.itype.unwrap_or(0);
        let req_ledger_hash = req.ledger_hash.unwrap_or_default();
        let req_ledger_seq = req.ledger_seq.unwrap_or(0);

        let ledger = match req_ledger_type {
            x if x == LT_CLOSED || x == LT_VALIDATED => {
                provider.latest_closed()
            }
            x if x == LT_HASH => {
                if req_ledger_hash.len() >= 32 {
                    let hash = Hash256::new(req_ledger_hash[..32].try_into().unwrap_or([0u8; 32]));
                    provider.get_by_hash(&hash)
                } else if req_ledger_seq > 0 {
                    provider.get_by_seq(req_ledger_seq)
                } else {
                    None
                }
            }
            _ => provider.latest_closed(),
        };

        let ledger = match ledger {
            Some(l) => l,
            None => {
                tracing::debug!("GetLedger from {}: ledger not found", from);
                let empty_response = proto_convert::encode_ledger_data(
                    &Hash256::ZERO,
                    req_ledger_seq,
                    req_ledger_type,
                    vec![],
                    0,
                );
                if let Some(handle) = self.peer_handles.get(&from) {
                    let _ = handle.tx.try_send(PeerMessage {
                        msg_type: MessageType::LedgerData,
                        payload: empty_response,
                    });
                }
                return;
            }
        };

        // Serialize state nodes (limit to 256KB)
        let mut nodes = Vec::new();
        let mut total_size = 0usize;
        let mut truncated = false;
        const MAX_RESPONSE_SIZE: usize = 256 * 1024;

        // Parse requested node_ids from the request for delta sync.
        let request_node_ids: Vec<Hash256> = req
            .node_ids
            .iter()
            .filter_map(|id_bytes| {
                if id_bytes.len() >= 32 {
                    let arr: [u8; 32] = id_bytes[..32].try_into().ok()?;
                    Some(Hash256::new(arr))
                } else {
                    None
                }
            })
            .collect();

        if !request_node_ids.is_empty() {
            // Delta sync: serve specific nodes by hash from the backing store.
            for node_hash in &request_node_ids {
                if let Some(store) = ledger.state_map.store() {
                    if let Ok(Some(data)) = store.fetch(node_hash) {
                        let entry_size = node_hash.as_bytes().len() + data.len();
                        if total_size + entry_size <= MAX_RESPONSE_SIZE {
                            nodes.push((node_hash.as_bytes().to_vec(), data));
                            total_size += entry_size;
                        } else {
                            truncated = true;
                            break;
                        }
                    }
                }
            }

            if truncated {
                tracing::warn!(
                    "GetLedger delta response truncated at 256KB: sent {} of {} requested nodes for seq={}",
                    nodes.len(), request_node_ids.len(), ledger.header.sequence
                );
            }
        } else {
            // Full sync fallback: serve all leaf nodes.
            ledger.state_map.for_each(&mut |key, data| {
                let entry_size = key.as_bytes().len() + data.len();
                if total_size + entry_size <= MAX_RESPONSE_SIZE {
                    nodes.push((key.as_bytes().to_vec(), data.to_vec()));
                    total_size += entry_size;
                } else {
                    truncated = true;
                }
            });

            if truncated {
                tracing::warn!(
                    "GetLedger response truncated at 256KB: sent {} state nodes for seq={}",
                    nodes.len(), ledger.header.sequence
                );
            }
        }

        let response = proto_convert::encode_ledger_data(
            &ledger.header.hash,
            ledger.header.sequence,
            req_ledger_type,
            nodes,
            0,
        );

        if let Some(handle) = self.peer_handles.get(&from) {
            let _ = handle.tx.try_send(PeerMessage {
                msg_type: MessageType::LedgerData,
                payload: response,
            });
        }
    }
}

/// Connect to a peer (outbound), perform handshake, and spawn read/write loops.
#[allow(clippy::too_many_arguments)]
async fn try_connect_outbound(
    addr: &str,
    identity: &NodeIdentity,
    network_id: u32,
    ledger_seq: &AtomicU32,
    ledger_hash: &RwLock<Hash256>,
    event_tx: &mpsc::UnboundedSender<PeerEvent>,
    peer_set: &PeerSet,
    tls_client: &Arc<SslConnector>,
) -> Result<Hash256, OverlayError> {
    let tcp = TcpStream::connect(addr)
        .await
        .map_err(|e| OverlayError::Connection(format!("{addr}: {e}")))?;

    let stream = tls::connect_tls(tcp, tls_client)
        .await
        .map_err(|e| OverlayError::Connection(format!("TLS connect {addr}: {e}")))?;

    let seq = ledger_seq.load(Ordering::Relaxed);
    let hash = *ledger_hash.read().await;

    let (peer_node_id, framed) =
        handshake::handshake_outbound_http(stream, identity, network_id, seq, &hash).await?;

    if peer_set.get(&peer_node_id).is_some() {
        return Err(OverlayError::Handshake("already connected".into()));
    }

    let info = Arc::new(PeerInfo {
        node_id: peer_node_id,
        address: addr.to_string(),
        inbound: false,
        ledger_seq: AtomicU32::new(0),
        reputation: PeerReputation::new(),
    });

    if !peer_set.add(Arc::clone(&info)) {
        return Err(OverlayError::PeerLimitReached);
    }

    let write_tx = spawn_peer_loops(peer_node_id, framed, event_tx.clone());
    let _ = event_tx.send(PeerEvent::Connected {
        node_id: peer_node_id,
        info,
        write_tx,
    });

    Ok(peer_node_id)
}

/// Accept an inbound peer, perform handshake, and spawn read/write loops.
#[allow(clippy::too_many_arguments)]
async fn try_accept_inbound(
    tcp: TcpStream,
    addr: &str,
    identity: &NodeIdentity,
    network_id: u32,
    ledger_seq: &AtomicU32,
    ledger_hash: &RwLock<Hash256>,
    event_tx: &mpsc::UnboundedSender<PeerEvent>,
    peer_set: &PeerSet,
    tls_server: &Arc<SslAcceptor>,
) -> Result<Hash256, OverlayError> {
    let stream = tls::accept_tls(tcp, tls_server)
        .await
        .map_err(|e| OverlayError::Connection(format!("TLS accept {addr}: {e}")))?;

    let seq = ledger_seq.load(Ordering::Relaxed);
    let hash = *ledger_hash.read().await;

    let (peer_node_id, framed) =
        handshake::handshake_inbound_http(stream, identity, network_id, seq, &hash).await?;

    if peer_set.get(&peer_node_id).is_some() {
        return Err(OverlayError::Handshake("already connected".into()));
    }

    let info = Arc::new(PeerInfo {
        node_id: peer_node_id,
        address: addr.to_string(),
        inbound: true,
        ledger_seq: AtomicU32::new(0),
        reputation: PeerReputation::new(),
    });

    if !peer_set.add(Arc::clone(&info)) {
        return Err(OverlayError::PeerLimitReached);
    }

    let write_tx = spawn_peer_loops(peer_node_id, framed, event_tx.clone());
    let _ = event_tx.send(PeerEvent::Connected {
        node_id: peer_node_id,
        info,
        write_tx,
    });

    Ok(peer_node_id)
}

/// Split a framed connection and spawn read/write loops.
/// Returns the write channel sender for the PeerHandle.
fn spawn_peer_loops(
    node_id: Hash256,
    framed: Framed<PeerStream, PeerCodec>,
    event_tx: mpsc::UnboundedSender<PeerEvent>,
) -> mpsc::Sender<PeerMessage> {
    let (write, read) = framed.split();
    let (tx, rx) = mpsc::channel(256);

    tokio::spawn(peer_loop::run_peer_read_loop(node_id, read, event_tx));
    tokio::spawn(peer_loop::run_peer_write_loop(write, rx));

    tx
}

/// Decode a validator list blob (base64-encoded JSON) and return the validator count.
///
/// The blob format is: `{"validators": [{"validation_public_key": "...", "manifest": "..."}, ...], ...}`
fn base64_decode_validator_blob(blob_bytes: &[u8]) -> Result<usize, ()> {
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(blob_bytes)
        .map_err(|_| ())?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).map_err(|_| ())?;
    let validators = json.get("validators").and_then(|v| v.as_array()).ok_or(())?;
    Ok(validators.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_validator_blob_extracts_count() {
        use base64::Engine;
        let json = serde_json::json!({
            "sequence": 1,
            "expiration": 999999999,
            "validators": [
                {"validation_public_key": "ED0001", "manifest": "AA=="},
                {"validation_public_key": "ED0002", "manifest": "BB=="},
                {"validation_public_key": "ED0003", "manifest": "CC=="},
            ]
        });
        let blob = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&json).unwrap());
        assert_eq!(base64_decode_validator_blob(blob.as_bytes()), Ok(3));
    }

    #[test]
    fn decode_validator_blob_invalid_base64() {
        assert_eq!(base64_decode_validator_blob(b"!!!invalid!!!"), Err(()));
    }

    #[test]
    fn decode_validator_blob_no_validators_key() {
        use base64::Engine;
        let json = serde_json::json!({"sequence": 1});
        let blob = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&json).unwrap());
        assert_eq!(base64_decode_validator_blob(blob.as_bytes()), Err(()));
    }

    #[test]
    fn quorum_auto_compute_from_validator_count() {
        // Simulate the quorum calculation from node.rs:
        // new_quorum = ceil(count * 0.8)
        let count = 35usize;
        let quorum = (count as f64 * 0.8).ceil() as usize;
        assert_eq!(quorum, 28);

        let count = 10usize;
        let quorum = (count as f64 * 0.8).ceil() as usize;
        assert_eq!(quorum, 8);

        let count = 1usize;
        let quorum = (count as f64 * 0.8).ceil() as usize;
        assert_eq!(quorum, 1);
    }
}
