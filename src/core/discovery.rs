use crate::core::identity::RingIdentity;
use crate::events::CoreEvent; // <--- NUOVO
use flume::Sender;            // <--- NUOVO
use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::thread;
use std::time::Duration;
use rand::Rng;
use std::sync::Arc;
use dashmap::DashMap;
use std::net::SocketAddr;

const SERVICE_TYPE: &str = "_rustclip._tcp.local.";
const TCP_PORT: u16 = 5566; 

pub type PeerMap = Arc<DashMap<String, SocketAddr>>;

// Aggiungiamo tx_event alla firma
pub fn start_lan_discovery(
    identity: RingIdentity, 
    peers: PeerMap, 
    tx_event: Option<Sender<CoreEvent>> // <--- NUOVO
) -> Result<()> {
    println!("🌍 Avvio Discovery LAN...");

    let my_discovery_id = identity.discovery_id.clone();
    let mut rng = rand::thread_rng();
    let device_id: u16 = rng.gen(); 
    let mdns = ServiceDaemon::new()?;
    let instance_name = format!("RustClip-{:04x}", device_id);
    let ip = "0.0.0.0"; 
    
    let properties = [("version", "1.0"), ("ring_id", &my_discovery_id)];

    let service_info = ServiceInfo::new(
        SERVICE_TYPE,
        &instance_name,
        &format!("{}.local.", instance_name),
        ip,
        TCP_PORT,
        &properties[..],
    )?.enable_addr_auto();

    mdns.register(service_info)?;
    let receiver = mdns.browse(SERVICE_TYPE)?;

    // Helper per inviare aggiornamenti alla GUI
    let send_update = |peers_map: &PeerMap| {
        if let Some(tx) = &tx_event {
            let list: Vec<(String, SocketAddr)> = peers_map
                .iter()
                .map(|r| (r.key().clone(), *r.value()))
                .collect();
            let _ = tx.send(CoreEvent::PeersUpdated(list));
        }
    };

    loop {
        while let Ok(event) = receiver.recv() {
            match event {
                mdns_sd::ServiceEvent::ServiceResolved(info) => {
                    let found_fullname = info.get_fullname();
                    if found_fullname.contains(&instance_name) { continue; }

                    let props = info.get_properties();
                    if let Some(other_prop) = props.get("ring_id") {
                        let raw_str = other_prop.to_string();
                        let mut clean_id = raw_str.trim().replace("\"", "");
                        if clean_id.starts_with("ring_id=") { clean_id = clean_id.replace("ring_id=", ""); }

                        if clean_id == my_discovery_id {
                            let mut target_addr: Option<SocketAddr> = None;
                            for ip in info.get_addresses() {
                                if ip.is_ipv4() {
                                    target_addr = Some(SocketAddr::new(*ip, info.get_port()));
                                    break; 
                                }
                            }
                            if target_addr.is_none() {
                                if let Some(ip) = info.get_addresses().iter().next() {
                                     target_addr = Some(SocketAddr::new(*ip, info.get_port()));
                                }
                            }

                            if let Some(addr) = target_addr {
                                if !peers.contains_key(found_fullname) {
                                    println!("➕ Peer: {} -> {}", found_fullname, addr);
                                    peers.insert(found_fullname.to_string(), addr);
                                    send_update(&peers); // <--- AGGIORNA GUI
                                }
                            }
                        }
                    }
                }
                mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => {
                    if peers.contains_key(&fullname) {
                        println!("➖ Peer perso: {}", fullname);
                        peers.remove(&fullname);
                        send_update(&peers); // <--- AGGIORNA GUI
                    }
                }
                _ => {} 
            }
        }
        thread::sleep(Duration::from_millis(500));
    }
}