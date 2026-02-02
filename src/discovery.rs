use crate::identity::RingIdentity;
use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::thread;
use std::time::Duration;

const SERVICE_TYPE: &str = "_rustclip._tcp.local.";

pub fn start_lan_discovery(identity: RingIdentity) -> Result<()> {
    println!("🌍 Avvio Discovery LAN (mDNS)...");

    let my_ring_id = identity.get_ring_id_hex();
    
    // 1. Inizializza il demone
    let mdns = ServiceDaemon::new()?;

    // 2. ADVERTISING
    let instance_name = format!("RustClip-{}", my_ring_id);
    let ip = "0.0.0.0"; 
    let port = 5000;    

    let properties = [("version", "1.0"), ("ring_id", &my_ring_id)];

    let service_info = ServiceInfo::new(
        SERVICE_TYPE,
        &instance_name,
        &format!("{}.local.", instance_name),
        ip,
        port,
        &properties[..],
    )?.enable_addr_auto();

    mdns.register(service_info)?;
    println!("📢 Annuncio attivo: {} (Ring ID: {})", instance_name, my_ring_id);
    println!("👀 In ascolto sulla rete WiFi...\n");

    // 3. BROWSING
    let receiver = mdns.browse(SERVICE_TYPE)?;

    loop {
        while let Ok(event) = receiver.recv() {
            match event {
                mdns_sd::ServiceEvent::ServiceResolved(info) => {
                    let found_fullname = info.get_fullname();
                    
                    if found_fullname.contains(&instance_name) {
                        continue; // Ignora noi stessi
                    }

                    let props = info.get_properties();
                    
                    if let Some(other_ring_id_prop) = props.get("ring_id") {
                        
                        // --- FIX: Convertiamo la proprietà in Stringa ---
                        let other_id_str = other_ring_id_prop.to_string();

                        if other_id_str == my_ring_id {
                            let addrs = info.get_addresses();
                            let ip_str = if !addrs.is_empty() {
                                addrs.iter()
                                     .map(|ip| ip.to_string())
                                     .collect::<Vec<_>>()
                                     .join(", ")
                            } else {
                                "IP Sconosciuto".to_string()
                            };

                            println!("🚀 🔗 TROVATO DISPOSITIVO DEL RING! 🔗 🚀");
                            println!("   Nome: {}", found_fullname);
                            println!("   IP:   {}", ip_str);
                            println!("   Port: {}", info.get_port());
                            println!("-------------------------------------------");
                        } else {
                            println!("⚠️  Trovato RustClip estraneo (Ring: {})", other_id_str);
                        }
                    }
                }
                _ => {} 
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}