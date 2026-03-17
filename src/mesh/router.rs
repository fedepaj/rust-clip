use crate::transport::{TransportType, TransportAddr};
use crate::mesh::topology::Topology;

/// Decides the best route for a packet based on topology
pub struct Router;

impl Router {
    /// Determine the best transport and address to reach a given peer.
    /// Returns None if the peer is unknown (flood should be attempted).
    pub fn resolve_route(
        topology: &Topology,
        destination: &str,
    ) -> Option<(TransportType, TransportAddr)> {
        // 1. Direct peer lookup
        if let Some(entry) = topology.peers.get(destination) {
            if let Some((transport, addr)) = entry.get_best_transport() {
                let transport_addr = match addr {
                    Some(socket_addr) => TransportAddr::Socket(socket_addr),
                    None => TransportAddr::BlePeer(destination.to_string()),
                };
                return Some((transport, transport_addr));
            }
        }

        // 2. TODO: Routing table lookup (Phase 2 - multi-hop)
        // if let Some(routes) = topology.routes.get(destination) { ... }

        None
    }
}
