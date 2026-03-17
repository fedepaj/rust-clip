use anyhow::Result;
use std::sync::{Arc, Mutex};

use crate::core::identity::RingIdentity;
use crate::core::packet::{WirePacket, PacketType};
use crate::mesh::topology::Topology;
use crate::mesh::gossip::{VectorClock, PacketCache};
use crate::mesh::router::Router;
use crate::protocol::clipboard_sync::ClipboardSync;
use crate::protocol::handshake::{HandshakeManager, HandshakeResult};
use crate::transport::{TransportEvent, TransportType, TransportAddr, Transport};

/// The Swarm is the central event loop that:
/// - Receives TransportEvents from all transports
/// - Deserializes bytes into WirePackets
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
}

impl Swarm {
    pub fn new(identity: RingIdentity, topology: Topology) -> Self {
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
        }
    }

    /// Get the event sender for transports to push events into
    pub fn event_sender(&self) -> flume::Sender<TransportEvent> {
        self.event_tx.clone()
    }

    /// Get the topology (shared reference)
    pub fn topology(&self) -> &Topology {
        &self.topology
    }

    /// Run the swarm event loop. This blocks the current thread.
    pub fn run(
        &mut self,
        transports: Vec<Box<dyn Transport>>,
        ble_send_tx: Option<flume::Sender<Vec<u8>>>,
        udp_transport: Option<crate::transport::lan::udp::UdpTransport>,
    ) -> Result<()> {
        let my_id = self.identity.stable_peer_id().to_string();
        println!("  [Swarm] Running. My PeerId: {}", my_id);

        // Start all transports
        for transport in &transports {
            if let Err(e) = transport.start(self.event_tx.clone()) {
                println!("  [Swarm] Transport {:?} failed to start: {}", transport.transport_type(), e);
            }
        }

        // Start clipboard monitor
        {
            let identity = self.identity.clone();
            let topology = self.topology.clone();
            let clip_sync = self.clipboard_sync.clone();
            let monitor_ble_tx = ble_send_tx.clone();
            let monitor_udp = udp_transport.clone();
            std::thread::spawn(move || {
                clipboard_monitor(identity, topology, clip_sync, monitor_ble_tx, monitor_udp);
            });
        }

        // Main event loop
        loop {
            match self.event_rx.recv() {
                Ok(event) => self.handle_event(event, &ble_send_tx, &udp_transport),
                Err(_) => {
                    println!("  [Swarm] All event senders dropped, shutting down.");
                    break;
                }
            }
        }

        Ok(())
    }

    fn handle_event(
        &mut self,
        event: TransportEvent,
        ble_send_tx: &Option<flume::Sender<Vec<u8>>>,
        udp_transport: &Option<crate::transport::lan::udp::UdpTransport>,
    ) {
        match event {
            TransportEvent::PeerDiscovered { peer_id, transport, addr } => {
                println!("  [Swarm] Peer discovered: {} via {:?}", peer_id, transport);
                self.topology.add_or_update(
                    peer_id,
                    vec![],
                    None,
                    Some((transport, addr)),
                );
            }

            TransportEvent::LinkEstablished { peer_id: _, transport } => {
                println!("  [Swarm] Link established via {:?}. Sending Hello broadcast...", transport);
                // We don't know the remote peer_id yet — send Hello as broadcast.
                // The remote will learn our identity from the Hello payload and respond with Welcome.
                match self.handshake_mgr.initiate(&self.identity, "broadcast") {
                    Ok(hello_packet) => {
                        self.send_packet(hello_packet, Some(transport), ble_send_tx, udp_transport);
                    }
                    Err(e) => println!("  [Swarm] Failed to initiate handshake: {}", e),
                }
            }

            TransportEvent::PacketReceived { data, from_transport, from_addr } => {
                // Deserialize
                let packet: WirePacket = match bincode::deserialize(&data) {
                    Ok(p) => p,
                    Err(e) => {
                        println!("  [Swarm] Failed to deserialize packet: {}", e);
                        return;
                    }
                };

                // Deduplication
                let sig_bytes = packet.signature.to_bytes();
                if self.packet_cache.seen(&sig_bytes) {
                    return;
                }

                self.process_packet(packet, from_transport, from_addr, ble_send_tx, udp_transport);
            }

            TransportEvent::PeerLost { peer_id, transport } => {
                println!("  [Swarm] Peer lost: {} via {:?}", peer_id, transport);
                // TODO: Mark transport as inactive in topology
            }

            TransportEvent::Error { transport, error } => {
                println!("  [Swarm] Transport {:?} error: {}", transport, error);
            }
        }
    }

    fn process_packet(
        &mut self,
        mut packet: WirePacket,
        from_transport: TransportType,
        from_addr: Option<std::net::SocketAddr>,
        ble_send_tx: &Option<flume::Sender<Vec<u8>>>,
        udp_transport: &Option<crate::transport::lan::udp::UdpTransport>,
    ) {
        let my_id = self.identity.stable_peer_id();

        // Check if this packet is for us or needs relaying
        if packet.header.receiver_id != "broadcast" && packet.header.receiver_id != my_id {
            // Relay: decrement TTL and forward
            if packet.decrement_ttl() {
                println!("  [Swarm] Relaying packet for {} (TTL={})", packet.header.receiver_id, packet.ttl());
                let target_id = packet.header.receiver_id.clone();
                if let Some((transport, addr)) = Router::resolve_route(&self.topology, &target_id) {
                    self.send_packet_to(packet, transport, addr, ble_send_tx, udp_transport);
                }
            }
            return;
        }

        // Process packet for us
        match packet.header.packet_type {
            PacketType::Hello => {
                let result = self.handshake_mgr.process_hello(&self.identity, &packet);
                self.handle_handshake_result(result, from_transport, from_addr, ble_send_tx, udp_transport);

                // Also need to send the Welcome reply — it's built inside process_hello
                // but returned as SessionEstablished. The Welcome was already logged.
                // We need to rebuild and send it. Let's handle this in handle_handshake_result.
            }

            PacketType::Welcome => {
                let result = self.handshake_mgr.process_welcome(&self.identity, &packet);
                self.handle_handshake_result(result, from_transport, from_addr, ble_send_tx, udp_transport);
            }

            PacketType::ClipboardText => {
                let sender_id = packet.header.sender_id.clone();

                // Get session key for sender
                let session_key = match self.topology.get_session_key(&sender_id) {
                    Some(k) => k,
                    None => {
                        println!("  [Swarm] No session key for {}, dropping clipboard", sender_id);
                        return;
                    }
                };

                // Decrypt
                let plaintext = match packet.decrypt_payload(&session_key) {
                    Ok(p) => p,
                    Err(e) => {
                        println!("  [Swarm] Clipboard decrypt failed from {}: {}", sender_id, e);
                        return;
                    }
                };

                // Convert to text
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
                    sync.apply_remote(text.as_bytes(), &sender_id, my_id)
                };

                if !should_apply {
                    return;
                }

                let short = if text.len() > 40 { format!("{}...", &text[..40]) } else { text.clone() };
                println!("  [Clipboard] Received from {}: \"{}\"", &sender_id[..8], short);

                // Apply to local clipboard in a separate thread (arboard may block)
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

            PacketType::RevocationNotice => {
                println!("  [Swarm] Revocation notice received");
                // TODO: Process revocation
            }

            PacketType::LinkUp => {
                // Internal event from transport — initiate handshake
                // The sender_id in LinkUp is our own rotating_id, so we use a different approach.
                // LinkUp is legacy — we now use TransportEvent::LinkEstablished instead.
                println!("  [Swarm] Legacy LinkUp received, ignoring (use TransportEvent::LinkEstablished)");
            }

            _ => {
                println!("  [Swarm] Received {:?} packet ({} bytes)", packet.header.packet_type, packet.payload.len());
            }
        }
    }

    fn handle_handshake_result(
        &mut self,
        result: HandshakeResult,
        from_transport: TransportType,
        from_addr: Option<std::net::SocketAddr>,
        ble_send_tx: &Option<flume::Sender<Vec<u8>>>,
        udp_transport: &Option<crate::transport::lan::udp::UdpTransport>,
    ) {
        match result {
            HandshakeResult::SessionEstablished { peer_id, peer_pubkey, peer_rotating_id, session_key, reply_packet, .. } => {
                println!("  [Swarm] Session established with {} (rotating: {})", peer_id, &peer_rotating_id);

                // Store in topology with the transport we received this handshake from
                self.topology.add_or_update(
                    peer_id.clone(),
                    peer_pubkey,
                    Some(session_key),
                    Some((from_transport.clone(), from_addr)),
                );

                // Correlate with mDNS: if peer's rotating_id is in topology with LAN info, merge it
                if let Some(mdns_entry) = self.topology.peers.get(&peer_rotating_id) {
                    if let Some(lan_status) = mdns_entry.transports.get(&TransportType::Mdns) {
                        if lan_status.is_active {
                            let lan_addr = lan_status.addr;
                            // Add LAN transport to the StablePeerId entry
                            self.topology.add_or_update(
                                peer_id.clone(),
                                vec![],
                                None, // Don't overwrite session key
                                Some((TransportType::Mdns, lan_addr)),
                            );
                            println!("  [Swarm] Merged LAN transport for {} (addr: {:?})", &peer_id[..8], lan_addr);
                        }
                    }
                }

                // Merge vector clocks
                self.vector_clock.increment(self.identity.stable_peer_id().to_string());

                // If we were the responder, send the Welcome reply
                if let Some(reply) = reply_packet {
                    self.send_packet(reply, Some(from_transport), ble_send_tx, udp_transport);
                }
            }
            HandshakeResult::SendPacket(packet) => {
                self.send_packet(packet, Some(from_transport), ble_send_tx, udp_transport);
            }
            HandshakeResult::Failed(reason) => {
                println!("  [Swarm] Handshake failed: {}", reason);
            }
            HandshakeResult::Ignored => {}
        }
    }

    fn send_packet(
        &self,
        packet: WirePacket,
        preferred_transport: Option<TransportType>,
        ble_send_tx: &Option<flume::Sender<Vec<u8>>>,
        udp_transport: &Option<crate::transport::lan::udp::UdpTransport>,
    ) {
        let dest = &packet.header.receiver_id;

        // Try to resolve route from topology
        if let Some((transport, addr)) = Router::resolve_route(&self.topology, dest) {
            self.send_packet_to(packet, transport, addr, ble_send_tx, udp_transport);
            return;
        }

        // Fallback: use preferred transport (or BLE)
        if let Ok(data) = bincode::serialize(&packet) {
            let transport = preferred_transport.unwrap_or(TransportType::Ble);
            match transport {
                TransportType::Ble => {
                    if let Some(tx) = ble_send_tx {
                        let _ = tx.send(data);
                    }
                }
                TransportType::Mdns | TransportType::TcpDirect => {
                    // Can't send without an address, drop
                    println!("  [Swarm] No address for LAN send to {}, dropping", dest);
                }
                _ => {}
            }
        }
    }

    fn send_packet_to(
        &self,
        packet: WirePacket,
        transport: TransportType,
        addr: TransportAddr,
        ble_send_tx: &Option<flume::Sender<Vec<u8>>>,
        udp_transport: &Option<crate::transport::lan::udp::UdpTransport>,
    ) {
        match (transport, addr) {
            (TransportType::Mdns | TransportType::TcpDirect, TransportAddr::Socket(socket_addr)) => {
                if let Some(udp) = udp_transport {
                    if let Err(e) = udp.send(socket_addr, &packet) {
                        println!("  [Swarm] UDP send failed: {}", e);
                    }
                }
            }
            (TransportType::Ble, _) => {
                if let Ok(data) = bincode::serialize(&packet) {
                    if let Some(tx) = ble_send_tx {
                        let _ = tx.send(data);
                    }
                }
            }
            _ => {
                println!("  [Swarm] Unsupported transport/addr combination");
            }
        }
    }
}

/// Background thread that monitors local clipboard and sends encrypted updates to all peers.
fn clipboard_monitor(
    identity: RingIdentity,
    topology: Topology,
    clipboard_sync: Arc<Mutex<ClipboardSync>>,
    ble_send_tx: Option<flume::Sender<Vec<u8>>>,
    udp_transport: Option<crate::transport::lan::udp::UdpTransport>,
) {
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            println!("  [Clipboard] Failed to open: {}", e);
            return;
        }
    };

    // Pre-fill with current content to avoid sending on startup
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

            // Route via best transport for this peer
            if let Some((transport_type, addr)) = peer.get_best_transport() {
                let used_transport = transport_type.clone();
                match (transport_type, addr) {
                    (TransportType::Mdns | TransportType::TcpDirect, Some(socket_addr)) => {
                        if let Some(ref udp) = udp_transport {
                            if udp.send(socket_addr, &packet).is_ok() {
                                sent_via.push((peer_id, used_transport));
                            }
                        }
                    }
                    (TransportType::Ble, _) => {
                        if let Ok(data) = bincode::serialize(&packet) {
                            if let Some(ref tx) = ble_send_tx {
                                if tx.send(data).is_ok() {
                                    sent_via.push((peer_id, used_transport));
                                }
                            }
                        }
                    }
                    _ => {}
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
