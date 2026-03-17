use anyhow::Result;

use crate::core::identity::RingIdentity;
use crate::core::packet::{WirePacket, PacketType};
use crate::mesh::topology::Topology;
use crate::mesh::gossip::{VectorClock, PacketCache};
use crate::mesh::router::Router;
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

            TransportEvent::LinkEstablished { peer_id, transport } => {
                println!("  [Swarm] Link established with {} via {:?}. Initiating handshake...", peer_id, transport);
                // Initiate handshake
                match self.handshake_mgr.initiate(&self.identity, &peer_id) {
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
                println!("  [Swarm] Clipboard data received ({} bytes)", packet.payload.len());
                // TODO: Decrypt and apply clipboard
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
            HandshakeResult::SessionEstablished { peer_id, peer_pubkey, session_key, .. } => {
                println!("  [Swarm] Session established with {}", peer_id);

                // Store in topology
                self.topology.add_or_update(
                    peer_id.clone(),
                    peer_pubkey,
                    Some(session_key),
                    Some((from_transport.clone(), from_addr)),
                );

                // Merge vector clocks
                self.vector_clock.increment(self.identity.stable_peer_id().to_string());

                // If we were the responder (Hello received), send Welcome
                // The process_hello already built the Welcome internally,
                // but we need to actually create and send it.
                // This is handled by the fact that process_hello logs session,
                // and the caller (process_packet for Hello) should build the Welcome.
                // Let's build it here.
                self.send_welcome_reply(&peer_id, from_transport, ble_send_tx, udp_transport);
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

    fn send_welcome_reply(
        &self,
        peer_id: &str,
        transport: TransportType,
        ble_send_tx: &Option<flume::Sender<Vec<u8>>>,
        udp_transport: &Option<crate::transport::lan::udp::UdpTransport>,
    ) {
        // Build a Welcome packet
        let (_secret, public) = RingIdentity::generate_ephemeral_key();
        let timestamp = chrono::Utc::now().timestamp() as u64;

        let mut sign_data = Vec::new();
        sign_data.extend_from_slice(self.identity.stable_peer_id().as_bytes());
        sign_data.extend_from_slice(self.identity.public_key.as_bytes());
        sign_data.extend_from_slice(public.as_bytes());
        sign_data.extend_from_slice(&timestamp.to_le_bytes());
        let inner_sig = self.identity.sign(&sign_data);

        let welcome = crate::core::packet::HandshakePayload::Welcome {
            stable_peer_id: self.identity.stable_peer_id().to_string(),
            ed25519_pubkey: self.identity.public_key.as_bytes().to_vec(),
            ephemeral_pubkey: *public.as_bytes(),
            timestamp,
            signature: inner_sig.to_bytes().to_vec(),
        };

        if let Ok(payload_bytes) = bincode::serialize(&welcome) {
            if let Ok(packet) = WirePacket::new_plain(
                self.identity.stable_peer_id().to_string(),
                peer_id.to_string(),
                PacketType::Welcome,
                &payload_bytes,
                &self.identity.identity_key,
            ) {
                self.send_packet(packet, Some(transport), ble_send_tx, udp_transport);
            }
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
