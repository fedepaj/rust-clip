use anyhow::Result;
use serde::{Serialize, Deserialize};
use std::net::SocketAddr;

pub mod ble;
pub mod lan;

// --- Transport Types ---

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

// --- Transport Events ---
// All transports produce these events into a unified channel.
// The Swarm consumes them.

#[derive(Debug, Clone)]
pub enum TransportEvent {
    /// A new peer was discovered (e.g., via mDNS or BLE scan)
    PeerDiscovered {
        peer_id: String,
        transport: TransportType,
        addr: Option<SocketAddr>,
    },
    /// A peer is no longer reachable
    PeerLost {
        peer_id: String,
        transport: TransportType,
    },
    /// Raw bytes received from a transport
    PacketReceived {
        data: Vec<u8>,
        from_transport: TransportType,
        from_addr: Option<SocketAddr>,
    },
    /// A transport-level link is established (e.g., BLE GATT connected)
    LinkEstablished {
        peer_id: String,
        transport: TransportType,
    },
    /// Transport error
    Error {
        transport: TransportType,
        error: String,
    },
}

// --- Transport Address ---
// Where to send data for a specific transport

#[derive(Debug, Clone)]
pub enum TransportAddr {
    /// IP:Port for UDP/TCP
    Socket(SocketAddr),
    /// BLE peer identifier (platform-specific, opaque)
    BlePeer(String),
}

// --- Transport Trait ---
// Transports deal in raw bytes. Packet serialization is handled by the Swarm.

pub trait Transport: Send + Sync {
    /// What kind of transport this is
    fn transport_type(&self) -> TransportType;

    /// Start the transport. It should begin producing TransportEvents.
    fn start(&self, event_tx: flume::Sender<TransportEvent>) -> Result<()>;

    /// Send raw bytes to a specific address
    fn send_to(&self, addr: &TransportAddr, data: &[u8]) -> Result<()>;

    /// Broadcast raw bytes to all reachable peers (best effort)
    fn broadcast(&self, data: &[u8]) -> Result<()>;

    /// Maximum payload size for a single send (BLE ~500, UDP ~65000, TCP unlimited)
    fn max_payload_size(&self) -> usize;
}
