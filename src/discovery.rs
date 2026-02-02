use crate::identity::RingIdentity;
use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::thread;
use std::time::Duration;
use rand::Rng;

const SERVICE_TYPE: &str = "_rustclip._tcp.local.";

pub fn start_lan_discovery(identity: RingIdentity) -> Result<()> {
    println!("🌍 Avvio Discovery LAN (mDNS)...");

    let my_ring_id = identity.get_ring_id_hex().trim().to_string();
    
    let mut rng = rand::thread_rng();
    let device_id: u16 = rng.gen(); 
    
    let mdns = ServiceDaemon::new()?;

    let instance_name = format!("RustClip-{:04x}", device_id);
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
    
    println!("📢 Io sono: '{}'", instance_name);
    println!("🔐 Il mio Ring ID: '{}'", my_ring_id);
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
                    
                    if let Some(other_ring_id_prop) = props.get("ring_id") {
                        let raw_str = other_ring_id_prop.to_string();
                        
                        // --- FIX DEFINITIVO ---
                        // 1. Rimuoviamo virgolette e spazi
                        let mut clean_id = raw_str.trim().replace("\"", "");

                        // 2. Se la stringa inizia con "ring_id=", lo togliamo
                        if clean_id.starts_with("ring_id=") {
                            clean_id = clean_id.replace("ring_id=", "");
                        }

                        if clean_id == my_ring_id {
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
                            println!("   Nome Device: {}", found_fullname);
                            println!("   IP Address:  {}", ip_str);
                            println!("   Port:        {}", info.get_port());
                            println!("-------------------------------------------");
                        } else {
                            // Se fallisce ancora, vediamo perché nel log
                             println!("🔍 DEBUG MISMATCH:");
                             println!("   Mio ID:   '{}'", my_ring_id);
                             println!("   Ricevuto: '{}'", clean_id);
                        }
                    }
                }
                _ => {} 
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}