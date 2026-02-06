use anyhow::Result;
use async_trait::async_trait;
use serde::{Serialize, Deserialize};
use std::net::SocketAddr;
use crate::core::packet::WirePacket;

pub mod ble;
pub mod handshake;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerId(pub String); // Usually the Ed25519 Public Key (Hex/Base64)

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransportType {
    Mdns,
    Ble,
    TcpDirect,
}

#[derive(Debug, Clone)]
pub struct Peer {
    pub id: PeerId,
    pub name: String,
    pub transport_type: TransportType,
    pub address: Option<SocketAddr>, // For TCP/LAN
    pub is_active: bool,
    pub last_seen: u64, // Unix Timestamp
}

#[async_trait]
pub trait Transport: Send + Sync {
    /// Start the transport listener/advertiser
    async fn start(&self) -> Result<()>;

    /// Send a secure packet to a specific peer
    async fn send(&self, peer_id: &PeerId, packet: WirePacket) -> Result<()>;

    /// Broadcast a packet to all reachable peers (Best Effort)
    async fn broadcast(&self, packet: WirePacket) -> Result<()>;
}
