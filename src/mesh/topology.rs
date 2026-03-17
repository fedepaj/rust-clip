use dashmap::DashMap;
use std::sync::Arc;
use chacha20poly1305::ChaCha20Poly1305;
use chrono::{Utc, DateTime};

use crate::transport::TransportType;
use std::collections::HashMap;
use std::net::SocketAddr;

#[derive(Clone, Debug)]
pub struct TransportStatus {
    pub last_seen: DateTime<Utc>,
    pub addr: Option<SocketAddr>, // IP:Port for LAN/TCP
    pub is_active: bool,
}

#[derive(Clone)]
pub struct PeerEntry {
    pub rotating_id: String,
    pub pubkey: Vec<u8>, // Ed25519 Public Key (Identity)
    pub last_seen: DateTime<Utc>, // Global last seen
    // Session Key is Ephemeral (In-Memory Only)
    pub session_key: Option<ChaCha20Poly1305>, 
    
    // Hybrid Mesh Transports
    pub transports: HashMap<TransportType, TransportStatus>,
}

impl PeerEntry {
    pub fn new(rotating_id: String, pubkey: Vec<u8>, session_key: Option<ChaCha20Poly1305>) -> Self {
        Self {
            rotating_id,
            pubkey,
            last_seen: Utc::now(),
            session_key,
            transports: HashMap::new(),
        }
    }

    pub fn update_transport(&mut self, transport: TransportType, addr: Option<SocketAddr>) {
        self.last_seen = Utc::now();
        self.transports.insert(transport, TransportStatus {
            last_seen: Utc::now(),
            addr,
            is_active: true,
        });
    }

    pub fn get_best_transport(&self) -> Option<(TransportType, Option<SocketAddr>)> {
        // Preference: TCP > mDNS (LAN) > BLE
        // Logic: Check active transports
        
        // 1. TcpDirect (Highest Priority)
        if let Some(status) = self.transports.get(&TransportType::TcpDirect) {
            if status.is_active { return Some((TransportType::TcpDirect, status.addr)); }
        }

        // 2. mDNS (LAN UDP)
        if let Some(status) = self.transports.get(&TransportType::Mdns) {
            if status.is_active { return Some((TransportType::Mdns, status.addr)); }
        }

        // 3. BLE (Fallback)
        if let Some(status) = self.transports.get(&TransportType::Ble) {
            if status.is_active { return Some((TransportType::Ble, None)); }
        }

        None
    }
}

// Custom Debug to avoid printing keys
impl std::fmt::Debug for PeerEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerEntry")
            .field("rotating_id", &self.rotating_id)
            .field("last_seen", &self.last_seen)
            .field("has_session", &self.session_key.is_some())
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct Topology {
    // Map RotatingID -> PeerEntry
    pub peers: Arc<DashMap<String, PeerEntry>>, 
}

impl Topology {
    pub fn new() -> Self {
        Self {
            peers: Arc::new(DashMap::new()),
        }
    }

    /// Add or update a peer in the topology
    pub fn add_or_update(&self, rotating_id: String, pubkey: Vec<u8>, session_key: Option<ChaCha20Poly1305>, transport: Option<(TransportType, Option<SocketAddr>)>) {
        // Check if exists
        if let Some(mut entry) = self.peers.get_mut(&rotating_id) {
            entry.last_seen = Utc::now();
            if let Some(key) = session_key {
                entry.session_key = Some(key);
            }
            // If we have a new pubkey and the old one was empty, update it.
            if entry.pubkey.is_empty() && !pubkey.is_empty() {
                entry.pubkey = pubkey;
            }

            if let Some((t_type, addr)) = transport {
                entry.update_transport(t_type, addr);
            }
            return;
        }

        // else insert
        let mut entry = PeerEntry::new(rotating_id.clone(), pubkey, session_key);
        if let Some((t_type, addr)) = transport {
             entry.update_transport(t_type, addr);
        }
        self.peers.insert(rotating_id, entry);
    }

    pub fn get_session_key(&self, rotating_id: &str) -> Option<ChaCha20Poly1305> {
        self.peers.get(rotating_id).and_then(|p| p.session_key.clone())
    }
    
    pub fn list_peers(&self) -> Vec<String> {
        self.peers.iter().map(|kv| kv.key().clone()).collect()
    }
}
