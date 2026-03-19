use anyhow::Result;
use std::sync::{Arc, Mutex};

use crate::core::identity::RingIdentity;
use crate::core::packet::{WirePacket, PacketType};
use crate::mesh::topology::Topology;
use crate::mesh::gossip::{VectorClock, PacketCache};
use crate::mesh::router::Router;
use crate::protocol::clipboard_sync::ClipboardSync;
use crate::protocol::handshake::{HandshakeManager, HandshakeResult};
use crate::transport::{TransportEvent, TransportType};
use crate::transport::lan::udp::UdpTransport;

/// The Swarm is the central event loop that:
/// - Receives TransportEvents from all transports
/// - Dispatches to handshake manager or protocol handlers
/// - Routes outbound packets through the best transport
pub struct Swarm {
    identity: RingIdentity,
    topology: Topology,
    handshake_mgr: HandshakeManager,
    packet_cache: PacketCache,
    vector_clock: VectorClock,
    clipboard_sync: Arc<Mutex<ClipboardSync>>,
    event_rx: flume::Receiver<TransportEvent>,
    event_tx: flume::Sender<TransportEvent>,

    // Transport send handles
    ble_send_tx: Option<flume::Sender<Vec<u8>>>,
    udp_transport: Option<UdpTransport>,
}

impl Swarm {
    pub fn new(
        identity: RingIdentity,
        topology: Topology,
        ble_send_tx: Option<flume::Sender<Vec<u8>>>,
        udp_transport: Option<UdpTransport>,
    ) -> Self {
        let (event_tx, event_rx) = flume::unbounded();

        Self {
            identity,
            topology,
            handshake_mgr: HandshakeManager::new(),
            packet_cache: PacketCache::new(200),
            vector_clock: VectorClock::new(),
            clipboard_sync: Arc::new(Mutex::new(ClipboardSync::new())),
            event_rx,
            event_tx,
            ble_send_tx,
            udp_transport,
        }
    }

    /// Get the event sender for transports to push events into.
    pub fn event_sender(&self) -> flume::Sender<TransportEvent> {
        self.event_tx.clone()
    }

    /// Run the swarm event loop. This blocks the current thread.
    pub fn run(&mut self) -> Result<()> {
        let my_id = self.identity.stable_peer_id().to_string();
        println!("  [Swarm] Running. My PeerId: {}", &my_id[..8]);

        // Start clipboard monitor
        self.start_clipboard_monitor();

        // Main event loop
        loop {
            match self.event_rx.recv() {
                Ok(event) => self.handle_event(event),
                Err(_) => {
                    println!("  [Swarm] All event senders dropped, shutting down.");
                    break;
                }
            }
        }

        Ok(())
    }

    // ─── Event dispatch ──────────────────────────────────────────────

    fn handle_event(&mut self, event: TransportEvent) {
        match event {
            TransportEvent::PeerDiscovered { peer_id, transport, addr } => {
                self.on_peer_discovered(peer_id, transport, addr);
            }
            TransportEvent::LinkEstablished { peer_id: _, transport } => {
                self.on_link_established(transport);
            }
            TransportEvent::PacketReceived { data, from_transport, from_addr } => {
                self.on_packet_received(data, from_transport, from_addr);
            }
            TransportEvent::PeerLost { peer_id, transport } => {
                self.on_peer_lost(peer_id, transport);
            }
            TransportEvent::Error { transport, error } => {
                println!("  [Swarm] Transport {:?} error: {}", transport, error);
            }
        }
    }

    // ─── PeerDiscovered (mDNS) ───────────────────────────────────────

    fn on_peer_discovered(
        &mut self,
        peer_id: String,       // rotating_id from mDNS
        transport: TransportType,
        addr: Option<std::net::SocketAddr>,
    ) {
        // Store discovery info in topology (keyed by rotating_id)
        self.topology.add_or_update(
            peer_id.clone(), vec![], None,
            Some((transport.clone(), addr)),
        );

        // Skip if we already have a session with a StablePeerId that maps to this rotating_id
        if self.topology.has_session(&peer_id) {
            return;
        }
        if let Some(stable_id) = self.topology.find_by_rotating_id(&peer_id) {
            if self.topology.has_session(&stable_id) {
                return;
            }
        }

        // Skip if we already have a pending handshake for this peer
        if self.handshake_mgr.has_pending(&peer_id) {
            return;
        }

        println!("  [Swarm] Peer discovered: {} via {:?}. Initiating handshake...", &peer_id[..8.min(peer_id.len())], transport);

        // Initiate handshake (pending_key = rotating_id for mDNS correlation)
        let hello_packet = match self.handshake_mgr.initiate(&self.identity, &peer_id) {
            Ok(p) => p,
            Err(e) => {
                println!("  [Swarm] Failed to create Hello: {}", e);
                return;
            }
        };

        // Send Hello directly to the discovered address
        self.send_to_addr(&hello_packet, &transport, addr);
    }

    // ─── LinkEstablished (BLE) ───────────────────────────────────────

    fn on_link_established(&mut self, transport: TransportType) {
        // BLE link established — peer identity unknown until handshake completes
        if self.handshake_mgr.has_pending("broadcast") {
            println!("  [Swarm] BLE link up, but handshake already pending. Skipping.");
            return;
        }

        println!("  [Swarm] Link established via {:?}. Sending Hello...", transport);

        let hello_packet = match self.handshake_mgr.initiate(&self.identity, "broadcast") {
            Ok(p) => p,
            Err(e) => {
                println!("  [Swarm] Failed to create Hello: {}", e);
                return;
            }
        };

        self.send_to_addr(&hello_packet, &transport, None);
    }

    // ─── PacketReceived ──────────────────────────────────────────────

    fn on_packet_received(
        &mut self,
        data: Vec<u8>,
        from_transport: TransportType,
        from_addr: Option<std::net::SocketAddr>,
    ) {
        let packet: WirePacket = match bincode::deserialize(&data) {
            Ok(p) => p,
            Err(e) => {
                println!("  [Swarm] Deserialize failed: {}", e);
                return;
            }
        };

        // Deduplication by signature
        let sig_bytes = packet.signature.to_bytes();
        if self.packet_cache.seen(&sig_bytes) {
            return;
        }

        self.process_packet(packet, from_transport, from_addr);
    }

    // ─── PeerLost ────────────────────────────────────────────────────

    fn on_peer_lost(&mut self, peer_id: String, transport: TransportType) {
        println!("  [Swarm] Peer lost: {} via {:?}", &peer_id[..8.min(peer_id.len())], transport);
        self.topology.mark_transport_inactive(&peer_id, &transport);

        // Also check if this rotating_id maps to a StablePeerId
        if let Some(stable_id) = self.topology.find_by_rotating_id(&peer_id) {
            self.topology.mark_transport_inactive(&stable_id, &transport);
        }
    }

    // ─── Packet processing ───────────────────────────────────────────

    fn process_packet(
        &mut self,
        mut packet: WirePacket,
        from_transport: TransportType,
        from_addr: Option<std::net::SocketAddr>,
    ) {
        let my_id = self.identity.stable_peer_id();

        // Relay if not for us
        if packet.header.receiver_id != "broadcast" && packet.header.receiver_id != my_id {
            if packet.decrement_ttl() {
                let target_id = packet.header.receiver_id.clone();
                if let Some((transport, addr)) = Router::resolve_route(&self.topology, &target_id) {
                    self.send_packet_via_route(packet, transport, addr);
                }
            }
            return;
        }

        match packet.header.packet_type {
            PacketType::Hello => {
                self.handle_hello(packet, from_transport, from_addr);
            }
            PacketType::Welcome => {
                self.handle_welcome(packet, from_transport, from_addr);
            }
            PacketType::ClipboardText => {
                self.handle_clipboard(packet);
            }
            PacketType::RevocationNotice => {
                println!("  [Swarm] Revocation notice received (TODO)");
            }
            other => {
                println!("  [Swarm] Received {:?} ({} bytes)", other, packet.payload.len());
            }
        }
    }

    // ─── Handshake handlers ──────────────────────────────────────────

    fn handle_hello(
        &mut self,
        packet: WirePacket,
        from_transport: TransportType,
        from_addr: Option<std::net::SocketAddr>,
    ) {
        // Session guard: if we already have a session with this sender, skip
        let sender_id = &packet.header.sender_id;
        if self.topology.has_session(sender_id) {
            return;
        }

        let result = self.handshake_mgr.process_hello(&self.identity, &packet);
        self.apply_handshake_result(result, from_transport, from_addr);
    }

    fn handle_welcome(
        &mut self,
        packet: WirePacket,
        from_transport: TransportType,
        from_addr: Option<std::net::SocketAddr>,
    ) {
        let result = self.handshake_mgr.process_welcome(&self.identity, &packet);
        self.apply_handshake_result(result, from_transport, from_addr);
    }

    fn apply_handshake_result(
        &mut self,
        result: HandshakeResult,
        from_transport: TransportType,
        from_addr: Option<std::net::SocketAddr>,
    ) {
        match result {
            HandshakeResult::SessionEstablished {
                peer_id,
                peer_pubkey,
                peer_rotating_id,
                session_key,
                reply_packet,
            } => {
                println!("  [Swarm] Session established with {} via {:?}", &peer_id[..8], from_transport);

                // Store session under StablePeerId with the transport we received from
                self.topology.add_or_update(
                    peer_id.clone(),
                    peer_pubkey,
                    Some(session_key),
                    Some((from_transport.clone(), from_addr)),
                );

                // Correlate: if there's a topology entry under the peer's rotating_id
                // with LAN address info, merge that transport into the StablePeerId entry
                self.merge_rotating_id_transport(&peer_id, &peer_rotating_id);

                // Update vector clock
                self.vector_clock.increment(self.identity.stable_peer_id().to_string());

                // Send Welcome reply if we're the responder
                if let Some(reply) = reply_packet {
                    self.send_to_addr(&reply, &from_transport, from_addr);
                }
            }

            HandshakeResult::Failed(reason) => {
                println!("  [Swarm] Handshake failed: {}", reason);
            }

            HandshakeResult::Ignored => {}
        }
    }

    /// After handshake completes, the peer is stored under StablePeerId.
    /// If mDNS discovered the peer earlier (stored under rotating_id),
    /// copy the LAN transport info to the StablePeerId entry.
    fn merge_rotating_id_transport(&self, stable_peer_id: &str, rotating_id: &str) {
        if let Some(mdns_entry) = self.topology.peers.get(rotating_id) {
            if let Some(lan_status) = mdns_entry.transports.get(&TransportType::Mdns) {
                if lan_status.is_active {
                    self.topology.add_or_update(
                        stable_peer_id.to_string(),
                        vec![],
                        None,
                        Some((TransportType::Mdns, lan_status.addr)),
                    );
                    println!("  [Swarm] Merged LAN transport for {} (addr: {:?})", &stable_peer_id[..8], lan_status.addr);
                }
            }
        }
    }

    // ─── Clipboard ───────────────────────────────────────────────────

    fn handle_clipboard(&mut self, packet: WirePacket) {
        let sender_id = packet.header.sender_id.clone();
        let my_id = self.identity.stable_peer_id().to_string();

        let session_key = match self.topology.get_session_key(&sender_id) {
            Some(k) => k,
            None => {
                println!("  [Swarm] No session key for {}, dropping clipboard", &sender_id[..8]);
                return;
            }
        };

        let plaintext = match packet.decrypt_payload(&session_key) {
            Ok(p) => p,
            Err(e) => {
                println!("  [Swarm] Clipboard decrypt failed from {}: {}", &sender_id[..8], e);
                return;
            }
        };

        let text = match String::from_utf8(plaintext) {
            Ok(t) => t,
            Err(_) => {
                println!("  [Swarm] Invalid UTF-8 in clipboard data");
                return;
            }
        };

        // Dedup check
        let should_apply = {
            let mut sync = self.clipboard_sync.lock().unwrap();
            sync.apply_remote(text.as_bytes(), &sender_id, &my_id)
        };

        if !should_apply {
            return;
        }

        let short = if text.len() > 40 { format!("{}...", &text[..40]) } else { text.clone() };
        println!("  [Clipboard] Received from {}: \"{}\"", &sender_id[..8], short);

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

    // ─── Sending ─────────────────────────────────────────────────────

    /// Send a packet to a specific address (used for handshake initiation).
    fn send_to_addr(
        &self,
        packet: &WirePacket,
        transport: &TransportType,
        addr: Option<std::net::SocketAddr>,
    ) {
        match transport {
            TransportType::Mdns | TransportType::TcpDirect => {
                if let Some(socket_addr) = addr {
                    if let Some(ref udp) = self.udp_transport {
                        if let Err(e) = udp.send(socket_addr, packet) {
                            println!("  [Swarm] UDP send failed: {}", e);
                        }
                    }
                } else {
                    println!("  [Swarm] No address for LAN send, dropping packet");
                }
            }
            TransportType::Ble => {
                if let Ok(data) = bincode::serialize(packet) {
                    if let Some(ref tx) = self.ble_send_tx {
                        let _ = tx.send(data);
                    }
                }
            }
            _ => {
                println!("  [Swarm] Unsupported transport {:?}", transport);
            }
        }
    }

    /// Send a packet using the router to determine best path.
    fn send_packet_via_route(
        &self,
        packet: WirePacket,
        transport: TransportType,
        addr: crate::transport::TransportAddr,
    ) {
        match (&transport, &addr) {
            (TransportType::Mdns | TransportType::TcpDirect, crate::transport::TransportAddr::Socket(socket_addr)) => {
                if let Some(ref udp) = self.udp_transport {
                    if let Err(e) = udp.send(*socket_addr, &packet) {
                        println!("  [Swarm] UDP relay failed: {}", e);
                    }
                }
            }
            (TransportType::Ble, _) => {
                if let Ok(data) = bincode::serialize(&packet) {
                    if let Some(ref tx) = self.ble_send_tx {
                        let _ = tx.send(data);
                    }
                }
            }
            _ => {
                println!("  [Swarm] Unsupported route: {:?}/{:?}", transport, addr);
            }
        }
    }

    // ─── Clipboard monitor ───────────────────────────────────────────

    fn start_clipboard_monitor(&self) {
        let identity = self.identity.clone();
        let topology = self.topology.clone();
        let clip_sync = self.clipboard_sync.clone();
        let ble_send_tx = self.ble_send_tx.clone();
        let udp_transport = self.udp_transport.clone();

        std::thread::spawn(move || {
            clipboard_monitor(identity, topology, clip_sync, ble_send_tx, udp_transport);
        });
    }
}

// ─── Clipboard monitor (background thread) ──────────────────────────

fn clipboard_monitor(
    identity: RingIdentity,
    topology: Topology,
    clipboard_sync: Arc<Mutex<ClipboardSync>>,
    ble_send_tx: Option<flume::Sender<Vec<u8>>>,
    udp_transport: Option<UdpTransport>,
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
        std::thread::sleep(std::time::Duration::from_millis(500));

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

        // Send to all peers with session keys
        let mut sent_via: Vec<(String, TransportType)> = Vec::new();
        for entry in topology.peers.iter() {
            let peer_id = entry.key().clone();
            let peer = entry.value();

            let session_key = match peer.session_key {
                Some(ref k) => k,
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

            // Route via best transport
            if let Some((transport_type, addr)) = peer.get_best_transport() {
                let sent = match (&transport_type, addr) {
                    (TransportType::Mdns | TransportType::TcpDirect, Some(socket_addr)) => {
                        udp_transport.as_ref()
                            .map_or(false, |udp| udp.send(socket_addr, &packet).is_ok())
                    }
                    (TransportType::Ble, _) => {
                        bincode::serialize(&packet).ok()
                            .and_then(|data| ble_send_tx.as_ref().map(|tx| tx.send(data).is_ok()))
                            .unwrap_or(false)
                    }
                    _ => false,
                };
                if sent {
                    sent_via.push((peer_id, transport_type));
                }
            } else if let Some(ref tx) = ble_send_tx {
                // Fallback to BLE
                if let Ok(data) = bincode::serialize(&packet) {
                    if tx.send(data).is_ok() {
                        sent_via.push((peer_id, TransportType::Ble));
                    }
                }
            }
        }

        if !sent_via.is_empty() {
            let short = if text.len() > 40 { format!("{}...", &text[..40]) } else { text.clone() };
            for (pid, transport) in &sent_via {
                println!("  [Clipboard] Sent to {} via {:?}: \"{}\"", &pid[..8], transport, short);
            }
        }
    }
}
