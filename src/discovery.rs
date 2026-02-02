use crate::identity::RingIdentity;
use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::thread;
use std::time::Duration;
use rand::Rng;
use std::sync::Arc;
use dashmap::DashMap;
use std::net::SocketAddr;

const SERVICE_TYPE: &str = "_rustclip._tcp.local.";
const TCP_PORT: u16 = 5566; // Porta fissa per il trasferimento dati

// La mappa condivisa: ID Univoco -> Indirizzo IP
pub type PeerMap = Arc<DashMap<String, SocketAddr>>;

pub fn start_lan_discovery(identity: RingIdentity, peers: PeerMap) -> Result<()> {
    println!("🌍 Avvio Discovery LAN (Secure Mode)...");

    let my_discovery_id = identity.discovery_id.clone();
    
    let mut rng = rand::thread_rng();
    let device_id: u16 = rng.gen(); 
    
    let mdns = ServiceDaemon::new()?;

    // Annunciamo la porta TCP vera (5566)
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
    
    println!("📢 Annuncio attivo: '{}'", instance_name);
    println!("👀 In ascolto sulla rete WiFi...\n");

    let receiver = mdns.browse(SERVICE_TYPE)?;

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
                        if clean_id.starts_with("ring_id=") {
                            clean_id = clean_id.replace("ring_id=", "");
                        }

                        if clean_id == my_discovery_id {
                            // Trovato un peer valido!
                            if let Some(ip) = info.get_addresses().iter().next() {
                                let addr = SocketAddr::new(*ip, info.get_port());
                                
                                // Inseriamo nella mappa (o aggiorniamo)
                                // Usiamo il nome completo come chiave univoca
                                if !peers.contains_key(found_fullname) {
                                    println!("➕ Peer Aggiunto: {} -> {}", found_fullname, addr);
                                    peers.insert(found_fullname.to_string(), addr);
                                }
                            }
                        }
                    }
                }
                // Gestione rimozione peer (se si disconnettono)
                mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => {
                    if peers.contains_key(&fullname) {
                        println!("➖ Peer Rimosso: {}", fullname);
                        peers.remove(&fullname);
                    }
                }
                _ => {} 
            }
        }
        thread::sleep(Duration::from_millis(500));
    }
}