use crate::identity::RingIdentity;
use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::thread;
use std::time::Duration;
use rand::Rng; // Ci serve per generare un ID casuale per il dispositivo

const SERVICE_TYPE: &str = "_rustclip._tcp.local.";

pub fn start_lan_discovery(identity: RingIdentity) -> Result<()> {
    println!("🌍 Avvio Discovery LAN (mDNS)...");

    // Questo è il segreto condiviso (Ring ID)
    let my_ring_id = identity.get_ring_id_hex();
    
    // Generiamo un ID casuale per QUESTO dispositivo (Device ID)
    // Così Device A e Device B avranno nomi diversi anche se sono nello stesso Ring
    let mut rng = rand::thread_rng();
    let device_id: u16 = rng.gen(); 
    
    // 1. Inizializza il demone
    let mdns = ServiceDaemon::new()?;

    // 2. ADVERTISING
    // Il nome ora è univoco per ogni computer: es. "RustClip-a1b2"
    let instance_name = format!("RustClip-{:04x}", device_id);
    let ip = "0.0.0.0"; 
    let port = 5000;    

    // Mettiamo il Ring ID (la "password") nelle proprietà
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
    println!("🔐 Il mio Ring ID: '{}' (nascosto nei metadati)", my_ring_id);
    println!("👀 In ascolto sulla rete WiFi...\n");

    // 3. BROWSING
    let receiver = mdns.browse(SERVICE_TYPE)?;

    loop {
        while let Ok(event) = receiver.recv() {
            match event {
                mdns_sd::ServiceEvent::ServiceResolved(info) => {
                    let found_fullname = info.get_fullname();
                    
                    // Ora questo controllo ha senso: ignoriamo solo se il nome è IDENTICO al nostro.
                    // Poiché abbiamo nomi casuali, Device A non scarterà Device B.
                    if found_fullname.contains(&instance_name) {
                        continue; 
                    }

                    let props = info.get_properties();
                    
                    if let Some(other_ring_id_prop) = props.get("ring_id") {
                        let other_id_str = other_ring_id_prop.to_string();

                        // Qui avviene la magia: nomi diversi, ma STESSO Ring ID
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
                            println!("   Nome Device: {}", found_fullname);
                            println!("   IP Address:  {}", ip_str);
                            println!("   Port:        {}", info.get_port());
                            println!("-------------------------------------------");
                        } else {
                            println!("⚠️  Trovato RustClip estraneo (Ring ID diverso)");
                        }
                    }
                }
                _ => {} 
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}