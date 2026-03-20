use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

pub mod ble;
pub mod lan;

// ─── Transport types ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum TransportType {
    Ble,
    Mdns,      // LAN UDP (discovered via mDNS)
    TcpDirect, // Direct TCP connection
    Hotspot,   // WiFi hotspot (ephemeral)
}

impl TransportType {
    /// Base bandwidth score for transport selection.
    /// Higher = faster. Used to pick the best route.
    pub fn bandwidth_score(&self) -> u32 {
        match self {
            TransportType::TcpDirect => 1000,
            TransportType::Mdns => 800,
            TransportType::Hotspot => 600,
            TransportType::Ble => 10,
        }
    }
}

// ─── Transport events ────────────────────────────────────────────
//
// All transports produce these events into a unified channel.
// The Swarm consumes them and updates topology accordingly.

#[derive(Debug, Clone)]
pub enum TransportEvent {
    /// A peer was discovered or a link was established on this transport.
    /// `peer_id`: transport-level identifier (rotating_id for mDNS, "ble-peer" for BLE).
    /// `handle`: opaque address for sending back (e.g. "192.168.1.5:12345" for UDP, "ble" for BLE).
    PeerDiscovered {
        peer_id: String,
        transport: TransportType,
        handle: String,
    },

    /// A peer is no longer reachable on this transport.
    PeerLost {
        peer_id: String,
        transport: TransportType,
    },

    /// Raw bytes received from a peer.
    /// `from_handle`: opaque sender address (same format as PeerDiscovered.handle).
    PacketReceived {
        data: Vec<u8>,
        from_transport: TransportType,
        from_handle: String,
    },

    /// Transport-level error.
    Error {
        transport: TransportType,
        error: String,
    },
}

// ─── Transport sender trait ──────────────────────────────────────
//
// Each transport implements this to provide a sending mechanism.
//
// To add a new transport (e.g. infrared):
// 1. Implement TransportSender
// 2. Push TransportEvents into the Swarm's event channel
// 3. Register the sender with TransportRegistry

pub trait TransportSender: Send + Sync {
    fn transport_type(&self) -> TransportType;

    /// Send raw bytes to a peer identified by a transport-specific handle.
    fn send_to(&self, handle: &str, data: &[u8]) -> Result<()>;

    /// Broadcast raw bytes to all reachable peers on this transport (best effort).
    fn broadcast(&self, data: &[u8]) -> Result<()>;
}

// ─── Channel-based sender (tagged) ───────────────────────────────
//
// Sends (handle, data) pairs through a flume channel.
// The receiver (e.g. BLE RunLoop) routes by handle:
// - specific handle (e.g. "ble-0"): send to that peer only
// - "*" or "ble": broadcast to all peers on this transport

pub struct ChannelSender {
    transport: TransportType,
    tx: flume::Sender<(String, Vec<u8>)>,
}

impl ChannelSender {
    pub fn new(transport: TransportType, tx: flume::Sender<(String, Vec<u8>)>) -> Self {
        Self { transport, tx }
    }
}

impl TransportSender for ChannelSender {
    fn transport_type(&self) -> TransportType {
        self.transport.clone()
    }

    fn send_to(&self, handle: &str, data: &[u8]) -> Result<()> {
        self.tx
            .send((handle.to_string(), data.to_vec()))
            .map_err(|e| anyhow::anyhow!("Channel send failed: {}", e))
    }

    fn broadcast(&self, data: &[u8]) -> Result<()> {
        self.send_to("*", data)
    }
}

// ─── Transport registry ──────────────────────────────────────────
//
// Holds all registered transport senders.
// Shared (via Arc) between Swarm and background threads (e.g. clipboard monitor).

pub struct TransportRegistry {
    senders: HashMap<TransportType, Arc<dyn TransportSender>>,
}

impl TransportRegistry {
    pub fn new() -> Self {
        Self {
            senders: HashMap::new(),
        }
    }

    /// Register a transport sender.
    pub fn register(&mut self, sender: Arc<dyn TransportSender>) {
        let t = sender.transport_type();
        self.senders.insert(t, sender);
    }

    /// Send data via a specific transport to a specific handle.
    pub fn send(&self, transport: &TransportType, handle: &str, data: &[u8]) -> Result<()> {
        match self.senders.get(transport) {
            Some(sender) => sender.send_to(handle, data),
            None => Err(anyhow::anyhow!("No sender for {:?}", transport)),
        }
    }

    /// Broadcast data on all transports.
    pub fn broadcast_all(&self, data: &[u8]) {
        for sender in self.senders.values() {
            let _ = sender.broadcast(data);
        }
    }

    /// Get the list of registered transport types.
    pub fn available_transports(&self) -> Vec<TransportType> {
        self.senders.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_sender_sends_tagged() {
        let (tx, rx) = flume::unbounded::<(String, Vec<u8>)>();
        let sender = ChannelSender::new(TransportType::Ble, tx);

        sender.send_to("ble-0", b"hello").unwrap();
        let (handle, data) = rx.recv().unwrap();
        assert_eq!(handle, "ble-0");
        assert_eq!(data, b"hello");
    }

    #[test]
    fn channel_sender_broadcast_uses_star() {
        let (tx, rx) = flume::unbounded::<(String, Vec<u8>)>();
        let sender = ChannelSender::new(TransportType::Ble, tx);

        sender.broadcast(b"data").unwrap();
        let (handle, _) = rx.recv().unwrap();
        assert_eq!(handle, "*");
    }

    #[test]
    fn registry_send_routes_correctly() {
        let (tx, rx) = flume::unbounded::<(String, Vec<u8>)>();
        let mut registry = TransportRegistry::new();
        registry.register(Arc::new(ChannelSender::new(TransportType::Ble, tx)));

        registry.send(&TransportType::Ble, "ble-1", b"test").unwrap();
        let (handle, data) = rx.recv().unwrap();
        assert_eq!(handle, "ble-1");
        assert_eq!(data, b"test");
    }

    #[test]
    fn registry_send_unknown_transport_errors() {
        let registry = TransportRegistry::new();
        let result = registry.send(&TransportType::Ble, "ble-0", b"data");
        assert!(result.is_err());
    }

    #[test]
    fn registry_broadcast_all() {
        let (tx1, rx1) = flume::unbounded::<(String, Vec<u8>)>();
        let (tx2, rx2) = flume::unbounded::<(String, Vec<u8>)>();

        let mut registry = TransportRegistry::new();
        registry.register(Arc::new(ChannelSender::new(TransportType::Ble, tx1)));
        registry.register(Arc::new(ChannelSender::new(TransportType::Mdns, tx2)));

        registry.broadcast_all(b"broadcast-data");

        let (h1, _) = rx1.recv().unwrap();
        let (h2, _) = rx2.recv().unwrap();
        assert_eq!(h1, "*");
        assert_eq!(h2, "*");
    }

    #[test]
    fn transport_type_bandwidth_ordering() {
        assert!(TransportType::TcpDirect.bandwidth_score() > TransportType::Mdns.bandwidth_score());
        assert!(TransportType::Mdns.bandwidth_score() > TransportType::Hotspot.bandwidth_score());
        assert!(TransportType::Hotspot.bandwidth_score() > TransportType::Ble.bandwidth_score());
    }
}
