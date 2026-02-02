use crate::identity::RingIdentity;
use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::thread;
use std::time::Duration;
use rand::Rng;

const SERVICE_TYPE: &str = "_rustclip._tcp.local.";

pub fn start_lan_discovery(identity: RingIdentity) -> Result<()> {
    println!("🌍 Avvio Discovery LAN (Secure Mode)...");

    // --- FIX: Usiamo il campo pubblico derivato da HKDF ---
    let my_discovery_id = identity.discovery_id.clone();
    // -----------------------------------------------------

    let mut rng = rand::thread_rng();
    let device_id: u16 = rng.gen(); 
    
    let mdns = ServiceDaemon::new()?;

    let instance_name = format!("RustClip-{:04x}", device_id);
    let ip = "0.0.0.0"; 
    let port = 5000;    

    let properties = [("version", "1.0"), ("ring_id", &my_discovery_id)];

    let service_info = ServiceInfo::new(
        SERVICE_TYPE,
        &instance_name,
        &format!("{}.local.", instance_name),
        ip,
        port,
        &properties[..],
    )?.enable_addr_auto();

    mdns.register(service_info)?;
    
    println!("📢 Annuncio attivo: '{}'", instance_name);
    println!("🔐 Secure ID (Public): '{}'", my_discovery_id);
    println!("👀 In ascolto sulla rete WiFi...\n");

    let receiver = mdns.browse(SERVICE_TYPE)?;

    loop {
        while let Ok(event) = receiver.recv() {
            match event {
                mdns_sd::ServiceEvent::ServiceResolved(info) => {
                    let found_fullname = info.get_fullname();
                    
                    if found_fullname.contains(&instance_name) {
                        continue; 
                    }

                    let props = info.get_properties();
                    
                    if let Some(other_prop) = props.get("ring_id") {
                        let raw_str = other_prop.to_string();
                        // Pulizia stringa
                        let mut clean_id = raw_str.trim().replace("\"", "");
                        if clean_id.starts_with("ring_id=") {
                            clean_id = clean_id.replace("ring_id=", "");
                        }

                        if clean_id == my_discovery_id {
                            let addrs = info.get_addresses();
                            let ip_str = if !addrs.is_empty() {
                                addrs.iter().map(|ip| ip.to_string()).collect::<Vec<_>>().join(", ")
                            } else { "Unknown".to_string() };

                            println!("🚀 🔗 TROVATO DISPOSITIVO DEL RING! 🔗 🚀");
                            println!("   Nome: {}", found_fullname);
                            println!("   IP:   {}", ip_str);
                            println!("-------------------------------------------");
                        }
                    }
                }
                _ => {} 
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}