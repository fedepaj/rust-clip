use serde::{Deserialize, Serialize};

use crate::mesh::topology::Topology;
use crate::transport::TransportType;

// ─── Route resolution ────────────────────────────────────────────

/// Result of resolving a route to a destination.
#[derive(Debug, Clone)]
pub enum Route {
    /// Peer is directly reachable via a transport link.
    Direct {
        transport: TransportType,
        handle: String,
    },
    /// Peer is reachable via a relay (multi-hop).
    Relay {
        next_hop_id: String,
        transport: TransportType,
        handle: String,
        hop_count: u8,
    },
}

pub struct Router;

impl Router {
    /// Resolve the best route to a destination peer.
    ///
    /// Priority:
    /// 1. Direct link (best transport by bandwidth_score)
    /// 2. Multi-hop route via routing table
    /// 3. None (peer unknown — caller should broadcast/flood)
    pub fn resolve(topology: &Topology, destination: &str) -> Option<Route> {
        // 1. Direct link
        if let Some((transport, handle)) = topology.best_link_for(destination) {
            return Some(Route::Direct { transport, handle });
        }

        // 2. Multi-hop via routing table
        if let Some(route_entry) = topology.get_route(destination) {
            // Verify the next_hop is actually reachable
            if let Some((transport, handle)) = topology.best_link_for(&route_entry.next_hop) {
                return Some(Route::Relay {
                    next_hop_id: route_entry.next_hop,
                    transport,
                    handle,
                    hop_count: route_entry.hop_count,
                });
            }
        }

        None
    }
}

// ─── Route announcement ─────────────────────────────────────────

/// Payload for RouteAnnounce packets.
/// Each peer periodically tells its neighbors which peers it can reach.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RouteAnnouncement {
    pub reachable: Vec<ReachablePeer>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReachablePeer {
    pub peer_id: String,
    pub hop_count: u8,
}

impl RouteAnnouncement {
    /// Build a route announcement from the current topology.
    /// Includes all directly connected peers (hop_count=0) and known multi-hop routes.
    pub fn from_topology(topology: &Topology, my_id: &str) -> Self {
        let mut reachable = Vec::new();

        // Direct peers (hop_count = 0)
        for entry in topology.peers.iter() {
            let peer_id = entry.key().clone();
            if peer_id == my_id {
                continue;
            }
            if entry.value().is_reachable() && entry.value().has_session() {
                reachable.push(ReachablePeer {
                    peer_id,
                    hop_count: 0,
                });
            }
        }

        // Multi-hop routes (hop_count > 0)
        for entry in topology.routes.iter() {
            let peer_id = entry.key().clone();
            if peer_id == my_id {
                continue;
            }
            // Don't re-announce direct peers
            if reachable.iter().any(|r| r.peer_id == peer_id) {
                continue;
            }
            reachable.push(ReachablePeer {
                peer_id,
                hop_count: entry.value().hop_count,
            });
        }

        RouteAnnouncement { reachable }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::topology::{RouteEntry, Topology};
    use chacha20poly1305::{ChaCha20Poly1305, KeyInit};
    use chrono::Utc;

    fn dummy_key() -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(&[0u8; 32].into())
    }

    #[test]
    fn resolve_direct_link() {
        let topo = Topology::new();
        topo.register_peer(
            "peer-a".into(),
            vec![],
            "rot-a".into(),
            dummy_key(),
            TransportType::Mdns,
            "10.0.0.1:1234".into(),
        );

        match Router::resolve(&topo, "peer-a") {
            Some(Route::Direct { transport, handle }) => {
                assert_eq!(transport, TransportType::Mdns);
                assert_eq!(handle, "10.0.0.1:1234");
            }
            other => panic!("Expected Direct route, got {:?}", other),
        }
    }

    #[test]
    fn resolve_relay_route() {
        let topo = Topology::new();

        // Register the relay peer (directly reachable)
        topo.register_peer(
            "relay".into(),
            vec![],
            "rot-relay".into(),
            dummy_key(),
            TransportType::Mdns,
            "10.0.0.2:1234".into(),
        );

        // Add a route to a far peer via the relay
        topo.add_route(
            "peer-far".into(),
            RouteEntry {
                next_hop: "relay".into(),
                via_transport: TransportType::Mdns,
                hop_count: 2,
                last_updated: Utc::now(),
            },
        );

        match Router::resolve(&topo, "peer-far") {
            Some(Route::Relay { next_hop_id, hop_count, .. }) => {
                assert_eq!(next_hop_id, "relay");
                assert_eq!(hop_count, 2);
            }
            other => panic!("Expected Relay route, got {:?}", other),
        }
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let topo = Topology::new();
        assert!(Router::resolve(&topo, "unknown").is_none());
    }

    #[test]
    fn direct_preferred_over_relay() {
        let topo = Topology::new();

        topo.register_peer(
            "peer-a".into(),
            vec![],
            "rot-a".into(),
            dummy_key(),
            TransportType::Mdns,
            "10.0.0.1:1234".into(),
        );

        // Also have a relay route to the same peer
        topo.add_route(
            "peer-a".into(),
            RouteEntry {
                next_hop: "relay".into(),
                via_transport: TransportType::Ble,
                hop_count: 1,
                last_updated: Utc::now(),
            },
        );

        // Direct should win
        match Router::resolve(&topo, "peer-a") {
            Some(Route::Direct { .. }) => {}
            other => panic!("Expected Direct route, got {:?}", other),
        }
    }

    #[test]
    fn route_announcement_from_topology() {
        let topo = Topology::new();
        topo.register_peer(
            "peer-a".into(),
            vec![],
            "rot-a".into(),
            dummy_key(),
            TransportType::Mdns,
            "10.0.0.1:1234".into(),
        );
        topo.register_peer(
            "peer-b".into(),
            vec![],
            "rot-b".into(),
            dummy_key(),
            TransportType::Ble,
            "ble-0".into(),
        );

        let ann = RouteAnnouncement::from_topology(&topo, "my-id");
        assert_eq!(ann.reachable.len(), 2);

        let ids: Vec<&str> = ann.reachable.iter().map(|r| r.peer_id.as_str()).collect();
        assert!(ids.contains(&"peer-a"));
        assert!(ids.contains(&"peer-b"));
    }
}
