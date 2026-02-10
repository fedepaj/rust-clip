use dashmap::DashMap;
use std::sync::Arc;
use chacha20poly1305::ChaCha20Poly1305;
use serde::{Serialize, Deserialize};
use chrono::{Utc, DateTime};
use anyhow::Result;

#[derive(Clone)]
pub struct PeerEntry {
    pub rotating_id: String,
    pub pubkey: Vec<u8>, // Ed25519 Public Key (Identity)
    pub last_seen: DateTime<Utc>,
    // Session Key is Ephemeral (In-Memory Only)
    // Wrapped in Arc because ChaCha20Poly1305 might not be Clone, 
    // but actually it is Clone if the underlying AEAD is. 
    // Let's check. KeyInit::new() returns it. 
    // chacha20poly1305::ChaCha20Poly1305 implements Clone.
    pub session_key: Option<ChaCha20Poly1305>, 
}

impl PeerEntry {
    pub fn new(rotating_id: String, pubkey: Vec<u8>, session_key: Option<ChaCha20Poly1305>) -> Self {
        Self {
            rotating_id,
            pubkey,
            last_seen: Utc::now(),
            session_key,
        }
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
    pub fn add_or_update(&self, rotating_id: String, pubkey: Vec<u8>, session_key: Option<ChaCha20Poly1305>) {
        // Check if exists
        if let Some(mut entry) = self.peers.get_mut(&rotating_id) {
            entry.last_seen = Utc::now();
            if let Some(key) = session_key {
                entry.session_key = Some(key);
            }
            // Update pubkey if needed? Usually pubkey doesn't change for same ID in a session.
            // But RotatingID changes.
            // If RotatingID changes, it's a "new" peer entry effectively.
            return;
        }

        // else insert
        let entry = PeerEntry::new(rotating_id.clone(), pubkey, session_key);
        self.peers.insert(rotating_id, entry);
    }

    pub fn get_session_key(&self, rotating_id: &str) -> Option<ChaCha20Poly1305> {
        self.peers.get(rotating_id).and_then(|p| p.session_key.clone())
    }
    
    pub fn list_peers(&self) -> Vec<String> {
        self.peers.iter().map(|kv| kv.key().clone()).collect()
    }
}
