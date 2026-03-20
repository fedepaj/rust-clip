use chacha20poly1305::ChaCha20Poly1305;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;

use crate::transport::TransportType;

// ─── Link info ───────────────────────────────────────────────────

/// A link represents reachability to a peer via a specific transport.
#[derive(Clone, Debug)]
pub struct LinkInfo {
    /// Transport-specific address for sending (e.g. "192.168.1.5:12345", "ble").
    pub handle: String,
    pub last_seen: DateTime<Utc>,
    pub active: bool,
}

// ─── Mesh peer ───────────────────────────────────────────────────

/// A peer in the mesh network with known identity (handshake completed).
#[derive(Clone)]
pub struct MeshPeer {
    pub pubkey: Vec<u8>,
    pub rotating_id: String,
    pub session_key: Option<ChaCha20Poly1305>,
    pub links: HashMap<TransportType, LinkInfo>,
    pub last_seen: DateTime<Utc>,
}

impl MeshPeer {
    pub fn new(pubkey: Vec<u8>, rotating_id: String, session_key: Option<ChaCha20Poly1305>) -> Self {
        Self {
            pubkey,
            rotating_id,
            session_key,
            links: HashMap::new(),
            last_seen: Utc::now(),
        }
    }

    /// Add or update a link on this peer.
    pub fn set_link(&mut self, transport: TransportType, handle: String) {
        self.links.insert(
            transport,
            LinkInfo {
                handle,
                last_seen: Utc::now(),
                active: true,
            },
        );
        self.last_seen = Utc::now();
    }

    /// Mark a link as inactive.
    pub fn deactivate_link(&mut self, transport: &TransportType) {
        if let Some(link) = self.links.get_mut(transport) {
            link.active = false;
        }
    }

    /// Get the best active link (highest bandwidth_score).
    /// Returns (TransportType, handle) or None if no active link.
    pub fn best_link(&self) -> Option<(TransportType, String)> {
        self.links
            .iter()
            .filter(|(_, link)| link.active)
            .max_by_key(|(t, _)| t.bandwidth_score())
            .map(|(t, link)| (t.clone(), link.handle.clone()))
    }

    /// Check if this peer has any active link.
    pub fn is_reachable(&self) -> bool {
        self.links.values().any(|l| l.active)
    }

    /// Check if this peer has a session key.
    pub fn has_session(&self) -> bool {
        self.session_key.is_some()
    }
}

impl std::fmt::Debug for MeshPeer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeshPeer")
            .field("rotating_id", &self.rotating_id)
            .field("has_session", &self.session_key.is_some())
            .field("links", &self.links)
            .field("last_seen", &self.last_seen)
            .finish()
    }
}

// ─── Discovered peer ─────────────────────────────────────────────

/// A peer discovered by a transport but not yet identified (no handshake yet).
#[derive(Clone, Debug)]
pub struct DiscoveredPeer {
    pub transport: TransportType,
    pub handle: String,
    pub discovered_at: DateTime<Utc>,
}

// ─── Route entry ─────────────────────────────────────────────────

/// A multi-hop route to a peer not directly reachable.
#[derive(Clone, Debug)]
pub struct RouteEntry {
    /// StablePeerId of the next hop (relay peer).
    pub next_hop: String,
    /// Transport to reach the next hop.
    pub via_transport: TransportType,
    /// Number of hops to reach the destination.
    pub hop_count: u8,
    pub last_updated: DateTime<Utc>,
}

// ─── Topology ────────────────────────────────────────────────────

/// Network topology: tracks identified peers, discovered peers, and routes.
#[derive(Clone, Debug)]
pub struct Topology {
    /// Identified peers keyed by StablePeerId (handshake completed).
    pub peers: Arc<DashMap<String, MeshPeer>>,

    /// Discovered but unidentified peers keyed by transport-level peer_id
    /// (e.g. rotating_id for mDNS, "ble-peer" for BLE).
    pub discovered: Arc<DashMap<String, DiscoveredPeer>>,

    /// Rotating ID → StablePeerId mapping (set after handshake).
    pub rotating_map: Arc<DashMap<String, String>>,

    /// Multi-hop routes: destination StablePeerId → RouteEntry.
    pub routes: Arc<DashMap<String, RouteEntry>>,
}

impl Topology {
    pub fn new() -> Self {
        Self {
            peers: Arc::new(DashMap::new()),
            discovered: Arc::new(DashMap::new()),
            rotating_map: Arc::new(DashMap::new()),
            routes: Arc::new(DashMap::new()),
        }
    }

    // ─── Discovery (pre-handshake) ───────────────────────────────

    /// Record a discovered peer (not yet identified).
    pub fn add_discovered(&self, peer_id: String, transport: TransportType, handle: String) {
        self.discovered.insert(
            peer_id,
            DiscoveredPeer {
                transport,
                handle,
                discovered_at: Utc::now(),
            },
        );
    }

    /// Get info about a discovered peer.
    pub fn get_discovered(&self, peer_id: &str) -> Option<DiscoveredPeer> {
        self.discovered.get(peer_id).map(|e| e.value().clone())
    }

    /// Remove a discovered peer (after handshake completes or peer is lost).
    pub fn remove_discovered(&self, peer_id: &str) {
        self.discovered.remove(peer_id);
    }

    // ─── Identified peers (post-handshake) ───────────────────────

    /// Register an identified peer after handshake completion.
    /// Creates or updates a MeshPeer entry keyed by StablePeerId.
    pub fn register_peer(
        &self,
        stable_peer_id: String,
        pubkey: Vec<u8>,
        rotating_id: String,
        session_key: ChaCha20Poly1305,
        transport: TransportType,
        handle: String,
    ) {
        // Update or insert MeshPeer
        match self.peers.get_mut(&stable_peer_id) {
            Some(mut entry) => {
                if !pubkey.is_empty() {
                    entry.pubkey = pubkey;
                }
                entry.rotating_id = rotating_id.clone();
                entry.session_key = Some(session_key);
                entry.set_link(transport, handle);
            }
            None => {
                let mut peer = MeshPeer::new(pubkey, rotating_id.clone(), Some(session_key));
                peer.set_link(transport, handle);
                self.peers.insert(stable_peer_id.clone(), peer);
            }
        }

        // Map rotating_id → StablePeerId
        self.rotating_map
            .insert(rotating_id, stable_peer_id.clone());
    }

    /// Add a transport link to an existing peer.
    pub fn add_link(&self, stable_peer_id: &str, transport: TransportType, handle: String) {
        if let Some(mut entry) = self.peers.get_mut(stable_peer_id) {
            entry.set_link(transport, handle);
        }
    }

    /// Get session key for a peer.
    pub fn get_session_key(&self, stable_peer_id: &str) -> Option<ChaCha20Poly1305> {
        self.peers
            .get(stable_peer_id)
            .and_then(|p| p.session_key.clone())
    }

    /// Check if a peer has an established session.
    pub fn has_session(&self, peer_id: &str) -> bool {
        self.peers
            .get(peer_id)
            .map_or(false, |p| p.session_key.is_some())
    }

    // ─── Transport state changes ─────────────────────────────────

    /// Mark a transport link as inactive for a peer.
    pub fn deactivate_link(&self, stable_peer_id: &str, transport: &TransportType) {
        if let Some(mut entry) = self.peers.get_mut(stable_peer_id) {
            entry.deactivate_link(transport);
        }
    }

    /// Handle PeerLost: deactivate the link, clean up discovered.
    /// Returns the StablePeerId if found (for logging).
    pub fn handle_peer_lost(&self, discovery_id: &str, transport: &TransportType) -> Option<String> {
        // Remove from discovered
        self.discovered.remove(discovery_id);

        // Look up StablePeerId via rotating_map
        if let Some(entry) = self.rotating_map.get(discovery_id) {
            let stable_id = entry.value().clone();
            self.deactivate_link(&stable_id, transport);
            return Some(stable_id);
        }

        None
    }

    // ─── Lookup / correlation ────────────────────────────────────

    /// Resolve a rotating_id to a StablePeerId.
    pub fn resolve_rotating_id(&self, rotating_id: &str) -> Option<String> {
        self.rotating_map.get(rotating_id).map(|e| e.value().clone())
    }

    /// Get the best link for a peer (for sending).
    pub fn best_link_for(&self, stable_peer_id: &str) -> Option<(TransportType, String)> {
        self.peers.get(stable_peer_id).and_then(|p| p.best_link())
    }

    /// List all identified peers with active sessions.
    pub fn active_peers(&self) -> Vec<String> {
        self.peers
            .iter()
            .filter(|e| e.value().has_session() && e.value().is_reachable())
            .map(|e| e.key().clone())
            .collect()
    }

    // ─── Multi-hop routes ────────────────────────────────────────

    /// Add or update a multi-hop route.
    pub fn add_route(&self, destination: String, entry: RouteEntry) {
        // Only update if this route is better (fewer hops) or newer
        if let Some(existing) = self.routes.get(&destination) {
            if existing.hop_count <= entry.hop_count {
                return;
            }
        }
        self.routes.insert(destination, entry);
    }

    /// Get the route to a destination (if not directly reachable).
    pub fn get_route(&self, destination: &str) -> Option<RouteEntry> {
        self.routes.get(destination).map(|e| e.value().clone())
    }

    /// Remove stale routes (older than max_age_secs).
    pub fn cleanup_stale_routes(&self, max_age_secs: i64) {
        let now = Utc::now();
        self.routes.retain(|_, route| {
            (now - route.last_updated).num_seconds() < max_age_secs
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chacha20poly1305::{ChaCha20Poly1305, KeyInit};

    fn dummy_session_key() -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(&[0u8; 32].into())
    }

    #[test]
    fn register_and_lookup_peer() {
        let topo = Topology::new();
        topo.register_peer(
            "peer-a".into(),
            vec![1, 2, 3],
            "rot-a".into(),
            dummy_session_key(),
            TransportType::Mdns,
            "192.168.1.5:12345".into(),
        );

        assert!(topo.has_session("peer-a"));
        assert!(!topo.has_session("peer-unknown"));
        assert_eq!(topo.active_peers(), vec!["peer-a".to_string()]);
    }

    #[test]
    fn rotating_map_resolution() {
        let topo = Topology::new();
        topo.register_peer(
            "peer-a".into(),
            vec![],
            "rot-a".into(),
            dummy_session_key(),
            TransportType::Mdns,
            "10.0.0.1:1234".into(),
        );

        assert_eq!(topo.resolve_rotating_id("rot-a"), Some("peer-a".into()));
        assert_eq!(topo.resolve_rotating_id("unknown"), None);
    }

    #[test]
    fn best_link_prefers_higher_bandwidth() {
        let topo = Topology::new();
        topo.register_peer(
            "peer-a".into(),
            vec![],
            "rot-a".into(),
            dummy_session_key(),
            TransportType::Ble,
            "ble-0".into(),
        );
        topo.add_link("peer-a", TransportType::Mdns, "192.168.1.5:9999".into());

        let (transport, handle) = topo.best_link_for("peer-a").unwrap();
        assert_eq!(transport, TransportType::Mdns); // bandwidth 800 > BLE 10
        assert_eq!(handle, "192.168.1.5:9999");
    }

    #[test]
    fn deactivate_link_falls_back() {
        let topo = Topology::new();
        topo.register_peer(
            "peer-a".into(),
            vec![],
            "rot-a".into(),
            dummy_session_key(),
            TransportType::Mdns,
            "10.0.0.1:1234".into(),
        );
        topo.add_link("peer-a", TransportType::Ble, "ble-0".into());

        // Deactivate LAN link — should fall back to BLE
        topo.deactivate_link("peer-a", &TransportType::Mdns);
        let (transport, _) = topo.best_link_for("peer-a").unwrap();
        assert_eq!(transport, TransportType::Ble);
    }

    #[test]
    fn peer_lost_deactivates_link() {
        let topo = Topology::new();
        topo.register_peer(
            "peer-a".into(),
            vec![],
            "rot-a".into(),
            dummy_session_key(),
            TransportType::Mdns,
            "10.0.0.1:1234".into(),
        );

        let stable = topo.handle_peer_lost("rot-a", &TransportType::Mdns);
        assert_eq!(stable, Some("peer-a".into()));

        // Peer should no longer be in active_peers (link deactivated)
        assert!(topo.active_peers().is_empty());
        // But session still exists
        assert!(topo.has_session("peer-a"));
    }

    #[test]
    fn discovered_peers_lifecycle() {
        let topo = Topology::new();
        topo.add_discovered("rot-x".into(), TransportType::Mdns, "10.0.0.2:1234".into());

        let d = topo.get_discovered("rot-x").unwrap();
        assert_eq!(d.handle, "10.0.0.2:1234");

        topo.remove_discovered("rot-x");
        assert!(topo.get_discovered("rot-x").is_none());
    }

    #[test]
    fn route_prefers_fewer_hops() {
        let topo = Topology::new();

        topo.add_route(
            "peer-far".into(),
            RouteEntry {
                next_hop: "relay-a".into(),
                via_transport: TransportType::Mdns,
                hop_count: 3,
                last_updated: Utc::now(),
            },
        );

        // Better route (fewer hops) should replace
        topo.add_route(
            "peer-far".into(),
            RouteEntry {
                next_hop: "relay-b".into(),
                via_transport: TransportType::Mdns,
                hop_count: 1,
                last_updated: Utc::now(),
            },
        );

        let route = topo.get_route("peer-far").unwrap();
        assert_eq!(route.next_hop, "relay-b");
        assert_eq!(route.hop_count, 1);

        // Worse route should be ignored
        topo.add_route(
            "peer-far".into(),
            RouteEntry {
                next_hop: "relay-c".into(),
                via_transport: TransportType::Mdns,
                hop_count: 4,
                last_updated: Utc::now(),
            },
        );

        let route = topo.get_route("peer-far").unwrap();
        assert_eq!(route.next_hop, "relay-b"); // unchanged
    }

    #[test]
    fn stale_routes_cleanup() {
        let topo = Topology::new();
        topo.add_route(
            "peer-old".into(),
            RouteEntry {
                next_hop: "relay".into(),
                via_transport: TransportType::Mdns,
                hop_count: 1,
                last_updated: Utc::now() - chrono::Duration::seconds(100),
            },
        );

        topo.cleanup_stale_routes(90);
        assert!(topo.get_route("peer-old").is_none());
    }
}
