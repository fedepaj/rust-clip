use anyhow::Result;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::VerifyingKey;

use crate::core::identity::RingIdentity;
use crate::core::packet::{PacketType, WirePacket};
use crate::core::revocation::{RevocationEntry, RevocationList};
use crate::mesh::gossip::{PacketCache, VectorClock};
use crate::mesh::router::{Route, RouteAnnouncement, Router};
use crate::mesh::topology::{RouteEntry, Topology};
use crate::protocol::clipboard_sync::ClipboardSync;
use crate::protocol::handshake::{HandshakeManager, HandshakeResult};
use crate::transport::{TransportEvent, TransportRegistry, TransportType};

const PERIODIC_INTERVAL_SECS: u64 = 30;
const ROUTE_EXPIRY_SECS: i64 = 90;
/// The Swarm is the central event loop that:
/// - Receives TransportEvents from all transports
/// - Manages topology and peer state
/// - Dispatches to handshake manager and protocol handlers
/// - Routes packets via the best available transport
pub struct Swarm {
    identity: RingIdentity,
    topology: Topology,
    handshake_mgr: HandshakeManager,
    packet_cache: PacketCache,
    vector_clock: VectorClock,
    clipboard_sync: Arc<Mutex<ClipboardSync>>,
    revocation_list: RevocationList,
    transports: Arc<TransportRegistry>,
    event_rx: flume::Receiver<TransportEvent>,
    event_tx: flume::Sender<TransportEvent>,

    /// BLE handshake queue: (peer_id, handle) pairs waiting for handshake.
    /// BLE handshakes are sequential (one at a time) because the pending_key
    /// is always "broadcast" (we don't know the peer's identity until handshake).
    ble_handshake_queue: VecDeque<(String, String)>,
    /// Whether a BLE handshake is currently in progress.
    ble_handshake_pending: bool,
    /// The client-side handle for the current BLE handshake in progress.
    /// Used to register the peer with the correct handle after Welcome arrives.
    ble_current_handle: Option<String>,
}

impl Swarm {
    pub fn new(
        identity: RingIdentity,
        topology: Topology,
        transports: TransportRegistry,
    ) -> Self {
        let (event_tx, event_rx) = flume::unbounded();

        Self {
            identity,
            topology,
            handshake_mgr: HandshakeManager::new(),
            packet_cache: PacketCache::new(200),
            vector_clock: VectorClock::new(),
            clipboard_sync: Arc::new(Mutex::new(ClipboardSync::new())),
            revocation_list: RevocationList::load(),
            transports: Arc::new(transports),
            event_rx,
            event_tx,
            ble_handshake_queue: VecDeque::new(),
            ble_handshake_pending: false,
            ble_current_handle: None,
        }
    }

    /// Get the event sender for transports to push events into.
    pub fn event_sender(&self) -> flume::Sender<TransportEvent> {
        self.event_tx.clone()
    }

    /// Run the swarm event loop. Blocks the current thread.
    pub fn run(&mut self) -> Result<()> {
        let my_id = self.identity.stable_peer_id().to_string();
        println!("  [Swarm] Running. PeerId: {}...", &my_id[..8]);

        self.start_clipboard_monitor();

        loop {
            match self.event_rx.recv_timeout(Duration::from_secs(PERIODIC_INTERVAL_SECS)) {
                Ok(event) => self.handle_event(event),
                Err(flume::RecvTimeoutError::Timeout) => self.periodic_tasks(),
                Err(flume::RecvTimeoutError::Disconnected) => {
                    println!("  [Swarm] All senders dropped, shutting down.");
                    break;
                }
            }
        }

        Ok(())
    }

    // ─── Event dispatch ──────────────────────────────────────────

    fn handle_event(&mut self, event: TransportEvent) {
        match event {
            TransportEvent::PeerDiscovered {
                peer_id,
                transport,
                handle,
            } => self.on_peer_discovered(peer_id, transport, handle),

            TransportEvent::PeerLost {
                peer_id,
                transport,
            } => self.on_peer_lost(peer_id, transport),

            TransportEvent::PacketReceived {
                data,
                from_transport,
                from_handle,
            } => self.on_packet_received(data, from_transport, from_handle),

            TransportEvent::Error { transport, error } => {
                println!("  [Swarm] {:?} error: {}", transport, error);
            }
        }
    }

    // ─── PeerDiscovered ──────────────────────────────────────────

    fn on_peer_discovered(&mut self, peer_id: String, transport: TransportType, handle: String) {
        // Store in discovered peers
        self.topology
            .add_discovered(peer_id.clone(), transport.clone(), handle.clone());

        // Skip if peer has an active session with reachable links.
        // If peer has a stale session (no active links), allow re-handshake.
        if self.topology.has_session(&peer_id) {
            if self.topology.peers.get(&peer_id).map_or(false, |p| p.is_reachable()) {
                return;
            }
        }
        if let Some(stable_id) = self.topology.resolve_rotating_id(&peer_id) {
            if self.topology.has_session(&stable_id) {
                if self.topology.peers.get(&stable_id).map_or(false, |p| p.is_reachable()) {
                    // Peer already identified and reachable — just add the new link
                    self.topology.add_link(&stable_id, transport, handle);
                    return;
                }
            }
        }

        match transport {
            TransportType::Ble => {
                // BLE handshakes are sequential — queue them.
                // All BLE handshakes use "broadcast" as pending_key because
                // we don't know peer identity until the handshake completes.
                if !self.ble_handshake_queue.iter().any(|(pid, _)| pid == &peer_id) {
                    self.ble_handshake_queue.push_back((peer_id.clone(), handle));
                }
                self.try_start_ble_handshake();
            }
            _ => {
                // LAN transports: use peer_id (rotating_id) as pending_key
                if self.handshake_mgr.has_pending(&peer_id) {
                    return;
                }

                let short = &peer_id[..8.min(peer_id.len())];
                println!(
                    "  [Swarm] Peer discovered: {} via {:?}. Initiating handshake...",
                    short, transport
                );

                let hello = match self.handshake_mgr.initiate(&self.identity, &peer_id) {
                    Ok(p) => p,
                    Err(e) => {
                        println!("  [Swarm] Failed to create Hello: {}", e);
                        return;
                    }
                };

                self.send_via(&hello, &transport, &handle);
            }
        }
    }

    /// Start the next BLE handshake from the queue, if none is in progress.
    fn try_start_ble_handshake(&mut self) {
        if self.ble_handshake_pending {
            return;
        }

        while let Some((peer_id, handle)) = self.ble_handshake_queue.pop_front() {
            // Skip if already has session
            if self.topology.has_session(&peer_id) {
                continue;
            }
            if let Some(stable_id) = self.topology.resolve_rotating_id(&peer_id) {
                if self.topology.has_session(&stable_id) {
                    continue;
                }
            }

            let short = &peer_id[..8.min(peer_id.len())];
            println!(
                "  [Swarm] BLE peer discovered: {}. Initiating handshake...",
                short
            );

            // BLE always uses "broadcast" pending_key
            let hello = match self.handshake_mgr.initiate(&self.identity, "broadcast") {
                Ok(p) => p,
                Err(e) => {
                    println!("  [Swarm] Failed to create BLE Hello: {}", e);
                    continue;
                }
            };

            self.ble_handshake_pending = true;
            self.ble_current_handle = Some(handle.clone());
            self.send_via(&hello, &TransportType::Ble, &handle);
            return;
        }
    }

    // ─── PeerLost ────────────────────────────────────────────────

    fn on_peer_lost(&mut self, peer_id: String, transport: TransportType) {
        let short = &peer_id[..8.min(peer_id.len())];

        if let Some(stable_id) = self.topology.handle_peer_lost(&peer_id, &transport) {
            println!(
                "  [Swarm] Peer lost: {} ({}) via {:?}",
                short,
                &stable_id[..8],
                transport
            );
        } else {
            println!("  [Swarm] Discovered peer lost: {} via {:?}", short, transport);
        }
    }

    // ─── PacketReceived ──────────────────────────────────────────

    fn on_packet_received(
        &mut self,
        data: Vec<u8>,
        from_transport: TransportType,
        from_handle: String,
    ) {
        let packet: WirePacket = match bincode::deserialize(&data) {
            Ok(p) => p,
            Err(e) => {
                println!("  [Swarm] Deserialize failed: {}", e);
                return;
            }
        };

        // Timestamp anti-replay check
        if let Err(e) = packet.validate_timestamp() {
            println!("  [Swarm] Packet rejected (timestamp): {}", e);
            return;
        }

        // Deduplication by signature
        let sig_bytes = packet.signature.to_bytes();
        if self.packet_cache.seen(&sig_bytes) {
            return;
        }

        self.process_packet(packet, from_transport, from_handle);
    }

    // ─── Packet processing ───────────────────────────────────────

    fn process_packet(
        &mut self,
        mut packet: WirePacket,
        from_transport: TransportType,
        from_handle: String,
    ) {
        let my_id = self.identity.stable_peer_id();

        // Verify outer signature for non-handshake packets from known peers.
        // Hello/Welcome signatures are verified inside the handshake manager.
        let is_handshake = matches!(
            packet.header.packet_type,
            PacketType::Hello | PacketType::Welcome
        );
        if !is_handshake {
            if let Some(peer) = self.topology.peers.get(&packet.header.sender_id) {
                if let Ok(vk) = VerifyingKey::from_bytes(
                    peer.pubkey.as_slice().try_into().unwrap_or(&[0u8; 32]),
                ) {
                    if packet.verify_signature(&vk).is_err() {
                        println!(
                            "  [Swarm] Signature verification failed from {}",
                            &packet.header.sender_id[..8]
                        );
                        return;
                    }
                }
            }
        }

        // Relay if not for us
        if packet.header.receiver_id != "broadcast" && packet.header.receiver_id != my_id {
            if packet.decrement_ttl() {
                let dest = packet.header.receiver_id.clone();
                println!(
                    "  [Swarm] Relaying packet for {} (TTL={})",
                    &dest[..8.min(dest.len())],
                    packet.ttl()
                );
                self.send_to_peer(&packet, &dest);
            }
            return;
        }

        match packet.header.packet_type {
            PacketType::Hello => self.handle_hello(packet, from_transport, from_handle),
            PacketType::Welcome => self.handle_welcome(packet, from_transport, from_handle),
            PacketType::ClipboardText => self.handle_clipboard(packet),
            PacketType::RouteAnnounce => self.handle_route_announce(packet),
            PacketType::RevocationNotice => self.handle_revocation(packet),
            other => {
                println!(
                    "  [Swarm] {:?} packet ({} bytes)",
                    other,
                    packet.payload.len()
                );
            }
        }
    }

    // ─── Handshake handlers ──────────────────────────────────────

    fn handle_hello(
        &mut self,
        packet: WirePacket,
        from_transport: TransportType,
        from_handle: String,
    ) {
        let sender_id = &packet.header.sender_id;

        // Session guard: skip if peer is already connected AND reachable.
        // If the peer has a session but is unreachable (all links inactive),
        // allow re-handshake to establish a fresh session key (PFS).
        if self.topology.has_session(sender_id) {
            if let Some(peer) = self.topology.peers.get(sender_id) {
                if peer.is_reachable() {
                    return;
                }
            }
            // Peer has stale session — allow re-handshake
            println!(
                "  [Swarm] Peer {} reconnecting (re-handshake for new session key)",
                &sender_id[..8.min(sender_id.len())]
            );
        }

        // Revocation check: reject handshakes from revoked peers
        if self.revocation_list.is_revoked(sender_id) {
            println!(
                "  [Swarm] Rejecting Hello from revoked peer {}",
                &sender_id[..8.min(sender_id.len())]
            );
            return;
        }

        let result = self.handshake_mgr.process_hello(&self.identity, &packet);
        self.apply_handshake_result(result, from_transport, from_handle);
    }

    fn handle_welcome(
        &mut self,
        packet: WirePacket,
        from_transport: TransportType,
        from_handle: String,
    ) {
        let is_ble = from_transport == TransportType::Ble;

        // For BLE initiator, use the tracked client-side handle
        let effective_handle = if is_ble {
            self.ble_current_handle.take().unwrap_or(from_handle)
        } else {
            from_handle
        };

        let result = self.handshake_mgr.process_welcome(&self.identity, &packet);
        self.apply_handshake_result(result, from_transport, effective_handle);

        // Always unblock BLE queue after Welcome processing (success or failure)
        if is_ble {
            self.ble_handshake_pending = false;
            self.try_start_ble_handshake();
        }
    }

    fn apply_handshake_result(
        &mut self,
        result: HandshakeResult,
        from_transport: TransportType,
        from_handle: String,
    ) {
        match result {
            HandshakeResult::SessionEstablished {
                peer_id,
                peer_pubkey,
                peer_rotating_id,
                session_key,
                reply_packet,
            } => {
                println!(
                    "  [Swarm] Session established with {} via {:?}",
                    &peer_id[..8],
                    from_transport
                );

                // Register peer in topology with the transport link
                self.topology.register_peer(
                    peer_id.clone(),
                    peer_pubkey,
                    peer_rotating_id.clone(),
                    session_key,
                    from_transport.clone(),
                    from_handle.clone(),
                );

                // Merge transport info from discovered entry (if any)
                self.merge_discovered_link(&peer_id, &peer_rotating_id);

                // Remove from discovered
                self.topology.remove_discovered(&peer_rotating_id);

                // Update vector clock
                self.vector_clock
                    .increment(self.identity.stable_peer_id().to_string());

                // Send Welcome reply if we're the responder
                if let Some(reply) = reply_packet {
                    self.send_via(&reply, &from_transport, &from_handle);
                }

                // Announce our routes to the new peer
                self.send_route_announce_to(&peer_id);
            }

            HandshakeResult::Failed(reason) => {
                println!("  [Swarm] Handshake failed: {}", reason);
            }

            HandshakeResult::Ignored => {}
        }
    }

    /// If the peer was discovered via a transport (e.g. mDNS) but the handshake
    /// arrived via a different transport, merge the discovered link info.
    fn merge_discovered_link(&self, stable_peer_id: &str, rotating_id: &str) {
        if let Some(discovered) = self.topology.get_discovered(rotating_id) {
            // Check if this link is different from what's already registered
            if let Some(peer) = self.topology.peers.get(stable_peer_id) {
                if !peer.links.contains_key(&discovered.transport) {
                    drop(peer); // Release DashMap ref before mutating
                    self.topology.add_link(
                        stable_peer_id,
                        discovered.transport,
                        discovered.handle,
                    );
                    println!(
                        "  [Swarm] Merged discovered link for {}",
                        &stable_peer_id[..8]
                    );
                }
            }
        }
    }

    // ─── Clipboard ───────────────────────────────────────────────

    fn handle_clipboard(&mut self, packet: WirePacket) {
        let sender_id = packet.header.sender_id.clone();
        let my_id = self.identity.stable_peer_id().to_string();

        let session_key = match self.topology.get_session_key(&sender_id) {
            Some(k) => k,
            None => {
                println!(
                    "  [Swarm] No session for {}, dropping clipboard",
                    &sender_id[..8]
                );
                return;
            }
        };

        let plaintext = match packet.decrypt_payload(&session_key) {
            Ok(p) => p,
            Err(e) => {
                println!(
                    "  [Swarm] Clipboard decrypt failed from {}: {}",
                    &sender_id[..8],
                    e
                );
                return;
            }
        };

        let text = match String::from_utf8(plaintext) {
            Ok(t) => t,
            Err(_) => {
                println!("  [Swarm] Invalid UTF-8 in clipboard");
                return;
            }
        };

        let should_apply = {
            let mut sync = self.clipboard_sync.lock().unwrap();
            sync.apply_remote(text.as_bytes(), &sender_id, &my_id)
        };

        if !should_apply {
            return;
        }

        let short = if text.len() > 40 {
            format!("{}...", &text[..40])
        } else {
            text.clone()
        };
        println!(
            "  [Clipboard] Received from {}: \"{}\"",
            &sender_id[..8],
            short
        );

        std::thread::spawn(move || {
            match arboard::Clipboard::new() {
                Ok(mut cb) => {
                    if let Err(e) = cb.set_text(&text) {
                        println!("  [Clipboard] Failed to set: {}", e);
                    }
                }
                Err(e) => println!("  [Clipboard] Failed to open: {}", e),
            }
        });
    }

    // ─── Route announcements ─────────────────────────────────────

    fn handle_route_announce(&mut self, packet: WirePacket) {
        let sender_id = packet.header.sender_id.clone();

        let session_key = match self.topology.get_session_key(&sender_id) {
            Some(k) => k,
            None => return,
        };

        let plaintext = match packet.decrypt_payload(&session_key) {
            Ok(p) => p,
            Err(_) => return,
        };

        let announcement: RouteAnnouncement = match bincode::deserialize(&plaintext) {
            Ok(a) => a,
            Err(_) => return,
        };

        let my_id = self.identity.stable_peer_id();
        for entry in &announcement.reachable {
            // Don't add routes to ourselves or to the sender (we already have a direct link)
            if entry.peer_id == my_id || entry.peer_id == sender_id {
                continue;
            }

            // Don't add route if we already have a direct link
            if self
                .topology
                .peers
                .get(&entry.peer_id)
                .map_or(false, |p| p.is_reachable())
            {
                continue;
            }

            let hop_count = entry.hop_count.saturating_add(1);
            if hop_count > 4 {
                continue;
            }

            // Get the transport we use to reach the sender (relay)
            let via_transport = self
                .topology
                .best_link_for(&sender_id)
                .map(|(t, _)| t)
                .unwrap_or(TransportType::Ble);

            self.topology.add_route(
                entry.peer_id.clone(),
                RouteEntry {
                    next_hop: sender_id.clone(),
                    via_transport,
                    hop_count,
                    last_updated: chrono::Utc::now(),
                },
            );

            println!(
                "  [Router] Learned route to {} via {} ({} hops)",
                &entry.peer_id[..8],
                &sender_id[..8],
                hop_count
            );
        }
    }

    // ─── Revocation ───────────────────────────────────────────────

    fn handle_revocation(&mut self, packet: WirePacket) {
        let sender_id = packet.header.sender_id.clone();

        let session_key = match self.topology.get_session_key(&sender_id) {
            Some(k) => k,
            None => return,
        };

        let plaintext = match packet.decrypt_payload(&session_key) {
            Ok(p) => p,
            Err(_) => return,
        };

        let entry: RevocationEntry = match bincode::deserialize(&plaintext) {
            Ok(e) => e,
            Err(_) => return,
        };

        // Verify the revocation signature using the revoker's pubkey
        if let Some(peer) = self.topology.peers.get(&entry.revoker_id) {
            if let Ok(vk) = VerifyingKey::from_bytes(
                peer.pubkey.as_slice().try_into().unwrap_or(&[0u8; 32]),
            ) {
                if entry.verify(&vk).is_err() {
                    println!("  [Swarm] Invalid revocation signature from {}", &sender_id[..8]);
                    return;
                }
            }
        }

        let revoked_id = entry.revoked_peer_id.clone();
        if self.revocation_list.add(entry) {
            println!(
                "  [Swarm] Peer {} revoked by {}",
                &revoked_id[..8.min(revoked_id.len())],
                &sender_id[..8]
            );

            // Persist
            if let Err(e) = self.revocation_list.save() {
                println!("  [Swarm] Failed to save revocation list: {}", e);
            }

            // Disconnect the revoked peer if connected
            if self.topology.has_session(&revoked_id) {
                self.topology.peers.remove(&revoked_id);
                println!("  [Swarm] Disconnected revoked peer {}", &revoked_id[..8]);
            }

            // Propagate to all other peers
            self.broadcast_revocation(&revoked_id);
        }
    }

    /// Broadcast a revocation notice to all peers with sessions.
    fn broadcast_revocation(&self, revoked_peer_id: &str) {
        let entry = match self.revocation_list.entries.iter().find(|e| e.revoked_peer_id == revoked_peer_id) {
            Some(e) => e,
            None => return,
        };

        let payload = match bincode::serialize(entry) {
            Ok(p) => p,
            Err(_) => return,
        };

        let my_id = self.identity.stable_peer_id();
        for peer_id in self.topology.active_peers() {
            if peer_id == revoked_peer_id {
                continue;
            }

            let session_key = match self.topology.get_session_key(&peer_id) {
                Some(k) => k,
                None => continue,
            };

            let packet = match WirePacket::new_encrypted(
                my_id.to_string(),
                peer_id.clone(),
                PacketType::RevocationNotice,
                &payload,
                &session_key,
                &self.identity.identity_key,
            ) {
                Ok(p) => p,
                Err(_) => continue,
            };

            self.send_to_peer(&packet, &peer_id);
        }
    }

    /// Send a route announcement to a specific peer.
    fn send_route_announce_to(&self, peer_id: &str) {
        let my_id = self.identity.stable_peer_id();
        let announcement = RouteAnnouncement::from_topology(&self.topology, my_id);

        if announcement.reachable.is_empty() {
            return;
        }

        let payload = match bincode::serialize(&announcement) {
            Ok(p) => p,
            Err(_) => return,
        };

        let session_key = match self.topology.get_session_key(peer_id) {
            Some(k) => k,
            None => return,
        };

        let packet = match WirePacket::new_encrypted(
            my_id.to_string(),
            peer_id.to_string(),
            PacketType::RouteAnnounce,
            &payload,
            &session_key,
            &self.identity.identity_key,
        ) {
            Ok(p) => p,
            Err(_) => return,
        };

        self.send_to_peer(&packet, peer_id);
    }

    // ─── Sending ─────────────────────────────────────────────────

    /// Send a packet directly via a specific transport and handle.
    fn send_via(&self, packet: &WirePacket, transport: &TransportType, handle: &str) {
        let data = match bincode::serialize(packet) {
            Ok(d) => d,
            Err(e) => {
                println!("  [Swarm] Serialize failed: {}", e);
                return;
            }
        };

        if let Err(e) = self.transports.send(transport, handle, &data) {
            println!("  [Swarm] Send via {:?} failed: {}", transport, e);
        }
    }

    /// Send a packet to a peer using the best available route.
    fn send_to_peer(&self, packet: &WirePacket, peer_id: &str) {
        match Router::resolve(&self.topology, peer_id) {
            Some(Route::Direct { transport, handle }) => {
                self.send_via(packet, &transport, &handle);
            }
            Some(Route::Relay {
                transport, handle, ..
            }) => {
                self.send_via(packet, &transport, &handle);
            }
            None => {
                // No route — broadcast on all transports
                if let Ok(data) = bincode::serialize(packet) {
                    self.transports.broadcast_all(&data);
                }
            }
        }
    }

    // ─── Periodic tasks ──────────────────────────────────────────

    fn periodic_tasks(&mut self) {
        // Clean up stale handshakes
        self.handshake_mgr.cleanup_stale();

        // Clean up stale routes
        self.topology.cleanup_stale_routes(ROUTE_EXPIRY_SECS);

        // Send route announcements to all peers with sessions
        let peer_ids = self.topology.active_peers();
        for peer_id in &peer_ids {
            self.send_route_announce_to(peer_id);
        }

        if !peer_ids.is_empty() {
            println!(
                "  [Swarm] Periodic: {} active peers, routes announced",
                peer_ids.len()
            );
        }
    }

    // ─── Clipboard monitor ───────────────────────────────────────

    fn start_clipboard_monitor(&self) {
        let identity = self.identity.clone();
        let topology = self.topology.clone();
        let clip_sync = self.clipboard_sync.clone();
        let transports = self.transports.clone();

        std::thread::spawn(move || {
            clipboard_monitor(identity, topology, clip_sync, transports);
        });
    }
}

// ─── Clipboard monitor (background thread) ──────────────────────

fn clipboard_monitor(
    identity: RingIdentity,
    topology: Topology,
    clipboard_sync: Arc<Mutex<ClipboardSync>>,
    transports: Arc<TransportRegistry>,
) {
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            println!("  [Clipboard] Failed to open: {}", e);
            return;
        }
    };

    let mut last_text = clipboard.get_text().unwrap_or_default();
    println!("  [Clipboard] Monitor started");

    loop {
        std::thread::sleep(Duration::from_millis(500));

        let text = match clipboard.get_text() {
            Ok(t) => t,
            Err(_) => continue,
        };

        if text.is_empty() || text == last_text {
            continue;
        }

        let my_id = identity.stable_peer_id();
        let should_send = {
            let mut sync = clipboard_sync.lock().unwrap();
            sync.should_propagate(text.as_bytes(), my_id)
        };

        if !should_send {
            last_text = text;
            continue;
        }

        last_text = text.clone();

        // Send to all peers with active sessions
        let mut sent_via: Vec<(String, TransportType)> = Vec::new();

        for entry in topology.peers.iter() {
            let peer_id = entry.key().clone();
            let peer = entry.value();

            let session_key = match &peer.session_key {
                Some(k) => k,
                None => continue,
            };

            let packet = match WirePacket::new_encrypted(
                my_id.to_string(),
                peer_id.clone(),
                PacketType::ClipboardText,
                text.as_bytes(),
                session_key,
                &identity.identity_key,
            ) {
                Ok(p) => p,
                Err(e) => {
                    println!("  [Clipboard] Encrypt failed for {}: {}", &peer_id[..8], e);
                    continue;
                }
            };

            if let Some((transport, handle)) = peer.best_link() {
                let data = match bincode::serialize(&packet) {
                    Ok(d) => d,
                    Err(_) => continue,
                };

                if transports.send(&transport, &handle, &data).is_ok() {
                    sent_via.push((peer_id, transport));
                }
            }
        }

        if !sent_via.is_empty() {
            let short = if text.len() > 40 {
                format!("{}...", &text[..40])
            } else {
                text.clone()
            };
            for (pid, transport) in &sent_via {
                println!(
                    "  [Clipboard] Sent to {} via {:?}: \"{}\"",
                    &pid[..8],
                    transport,
                    short
                );
            }
        }
    }
}
