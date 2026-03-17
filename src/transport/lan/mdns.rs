use mdns_sd::{ServiceDaemon, ServiceInfo, ServiceEvent};
use std::thread;
use crate::core::identity::RingIdentity;
use crate::transport::TransportType;
use crate::mesh::topology::Topology;
use std::net::{IpAddr, SocketAddr};

pub struct MdnsService {
    daemon: ServiceDaemon,
    rotating_id: String,
    topology: Topology,
    port: u16,
}

impl MdnsService {
    pub fn new(identity: &RingIdentity, topology: Topology, port: u16) -> anyhow::Result<Self> {
        let daemon = ServiceDaemon::new()?;
        let rotating_id = identity.get_rotating_id();
        Ok(Self { daemon, rotating_id, topology, port })
    }

    pub fn start(&self) -> anyhow::Result<()> {
        let service_type = "_rustclip._tcp.local.";
        let instance_name = &self.rotating_id;
        
        let port = self.port;
        let properties = [("version", "1.0")];

        // Register
        let my_service = ServiceInfo::new(
            service_type,
            instance_name,
            "rust-clip-host.local.",
            "", // IP (empty = all interfaces)
            port,
            &properties[..],
        )?.enable_addr_auto();

        self.daemon.register(my_service)?;
        println!("📢 [LAN-mDNS] Service Registered: {}.{}", instance_name, service_type);

        // Browse
        let browse_daemon = self.daemon.clone();
        let service_type_clone = service_type.to_string();
        let topology_scanner = self.topology.clone();
        
        thread::spawn(move || {
            if let Ok(receiver) = browse_daemon.browse(&service_type_clone) {
                while let Ok(event) = receiver.recv() {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            let mut rotating_id = info.get_fullname().to_string();
                            // Format is usually: Instance._type._tcp.local.
                            // We just want Instance which is our rotating_id
                            if let Some(idx) = rotating_id.find('.') {
                                rotating_id = rotating_id[0..idx].to_string();
                            }

                            println!("🔎 [LAN-mDNS] Peer Discovered: {} ({:?}:{})", 
                                rotating_id, 
                                info.get_addresses(), 
                                info.get_port()
                            );
                            
                            // Extract first IPv4 address
                            if let Some(ip) = info.get_addresses().iter().find(|addr| matches!(addr, IpAddr::V4(_))) {
                                let addr = SocketAddr::new(*ip, info.get_port());
                                topology_scanner.add_or_update(
                                    rotating_id,
                                    vec![], // Empty PubKey (Get from Handshake later)
                                    None,   // No Session Key yet
                                    Some((TransportType::Mdns, Some(addr)))
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
        });

        Ok(())
    }
}
