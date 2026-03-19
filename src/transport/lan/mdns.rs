use anyhow::Result;
use flume::Sender;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::net::{IpAddr, SocketAddr};
use std::thread;

use crate::transport::{TransportEvent, TransportType};

/// mDNS service for LAN peer discovery.
///
/// Advertises this node's rotating_id and discovers other nodes.
/// Produces TransportEvents — does NOT interact with topology directly.
pub struct MdnsService {
    daemon: ServiceDaemon,
    rotating_id: String,
    port: u16,
}

const SERVICE_TYPE: &str = "_rustclip._tcp.local.";

impl MdnsService {
    pub fn new(rotating_id: String, port: u16) -> Result<Self> {
        let daemon = ServiceDaemon::new()?;
        Ok(Self { daemon, rotating_id, port })
    }

    /// Start advertising and browsing.
    /// Discovered peers are reported as `TransportEvent::PeerDiscovered`.
    /// Lost peers are reported as `TransportEvent::PeerLost`.
    pub fn start(&self, event_tx: Sender<TransportEvent>) -> Result<()> {
        // Register our service
        let my_service = ServiceInfo::new(
            SERVICE_TYPE,
            &self.rotating_id,
            "rust-clip-host.local.",
            "",
            self.port,
            &[("version", "1.0")][..],
        )?.enable_addr_auto();

        self.daemon.register(my_service)?;
        println!("  [mDNS] Registered: {}.{}", &self.rotating_id, SERVICE_TYPE);

        // Browse for peers
        let my_rotating_id = self.rotating_id.clone();
        let browse_daemon = self.daemon.clone();

        thread::spawn(move || {
            let receiver = match browse_daemon.browse(SERVICE_TYPE) {
                Ok(r) => r,
                Err(e) => {
                    println!("  [mDNS] Browse failed: {}", e);
                    return;
                }
            };

            while let Ok(event) = receiver.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        let rotating_id = extract_instance_name(info.get_fullname());

                        // Skip self
                        if rotating_id == my_rotating_id {
                            continue;
                        }

                        // Extract first IPv4 address
                        let ipv4 = info.get_addresses().iter()
                            .find(|addr| matches!(addr, IpAddr::V4(_)))
                            .copied();

                        if let Some(ip) = ipv4 {
                            let addr = SocketAddr::new(ip, info.get_port());
                            println!("  [mDNS] Peer discovered: {} at {}", &rotating_id, addr);
                            let _ = event_tx.send(TransportEvent::PeerDiscovered {
                                peer_id: rotating_id,
                                transport: TransportType::Mdns,
                                addr: Some(addr),
                            });
                        }
                    }

                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        let rotating_id = extract_instance_name(&fullname);
                        if rotating_id == my_rotating_id {
                            continue;
                        }
                        println!("  [mDNS] Peer lost: {}", &rotating_id);
                        let _ = event_tx.send(TransportEvent::PeerLost {
                            peer_id: rotating_id,
                            transport: TransportType::Mdns,
                        });
                    }

                    _ => {}
                }
            }
        });

        Ok(())
    }
}

/// Extract the instance name (rotating_id) from a fully qualified service name.
/// Format: "instance._type._tcp.local." → "instance"
fn extract_instance_name(fullname: &str) -> String {
    match fullname.find('.') {
        Some(idx) => fullname[..idx].to_string(),
        None => fullname.to_string(),
    }
}
