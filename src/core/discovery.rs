use crate::core::identity::RingIdentity;
use crate::core::config::AppConfig;
use crate::events::{CoreEvent, PeerInfo};
use flume::Sender;
use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::thread;
use std::time::Duration;
use std::sync::Arc;
use dashmap::DashMap;
use std::net::SocketAddr;

const SERVICE_TYPE: &str = "_rustclip._tcp.local.";
const TCP_PORT: u16 = 5566; 

pub type PeerMap = Arc<DashMap<String, PeerInfo>>;

pub fn start_lan_discovery(
    identity: RingIdentity, 
    peers: PeerMap, 
    config: AppConfig, 
    tx_event: Option<Sender<CoreEvent>>
) -> Result<()> {
    println!("🌍 Starting LAN Discovery...");

    let my_discovery_id = identity.discovery_id.clone();
    let my_device_id = RingIdentity::get_derived_device_id();
    let mdns = ServiceDaemon::new()?;

    // --- COSTRUZIONE NOME SERVICE ---
    // Puliamo il nome scelto dall'utente per renderlo compatibile con mDNS
    let _safe_name = sanitize_device_name(&config.device_name);
    // Usiamo direttamente il device_id come hostname parte del service
    // In questo modo è stabile
    let instance_name = format!("rustclip-{}", my_device_id);
    let ip = "0.0.0.0"; 
    
    // Nelle properties mettiamo le info "umane"
    let properties = [
        ("version", "1.0"), 
        ("ring_id", &my_discovery_id),
        ("device_name", &config.device_name),
        ("device_id", &my_device_id)
    ];

    let service_info = ServiceInfo::new(
        SERVICE_TYPE,
        &instance_name,
        &format!("{}.local.", instance_name),
        ip,
        TCP_PORT,
        &properties[..],
    )?.enable_addr_auto();

    mdns.register(service_info)?;
    
    println!("📢 Announcement active: '{}' (ID: {})", instance_name, my_device_id);
    
    let receiver = mdns.browse(SERVICE_TYPE)?;

    let send_update = |peers_map: &PeerMap| {
        if let Some(tx) = &tx_event {
            let list: Vec<PeerInfo> = peers_map
                .iter()
                .map(|r| r.value().clone())
                .collect();
            let _ = tx.send(CoreEvent::PeersUpdated(list));
        }
    };

    loop {
        while let Ok(event) = receiver.recv() {
            match event {
                mdns_sd::ServiceEvent::ServiceResolved(info) => {
                    let found_fullname = info.get_fullname();
                    if found_fullname.contains(&instance_name) { continue; } // Ignora me stesso

                    let props = info.get_properties();
                    if let Some(other_prop) = props.get("ring_id") {
                        let clean_prop = |p: &str| p.trim().replace("\"", "").replace("ring_id=", "");
                        let clean_id = clean_prop(&other_prop.to_string());

                        if clean_id == my_discovery_id {
                            // Extract metadata
                            // FIX: Strip "device_name=" if present
                            let raw_name = props.get("device_name")
                                .map(|s| s.to_string().replace("\"", ""))
                                .unwrap_or_else(|| "Unknown".to_string());
                            
                            let device_name = raw_name.replace("device_name=", ""); // FIX 1
                            
                            let peer_device_id = props.get("device_id")
                                .map(|s| s.to_string().replace("\"", ""))
                                .unwrap_or_else(|| found_fullname.to_string());


                            // FIND ADDRESS (Prefer IPv4)
                            let mut target_addr: Option<SocketAddr> = None;
                            
                            // Prima cerchiamo esplicitamente IPv4
                            for ip in info.get_addresses() {
                                if ip.is_ipv4() {
                                    target_addr = Some(SocketAddr::new(*ip, info.get_port()));
                                    break; 
                                }
                            }
                            // Se non c'è IPv4, accettiamo IPv6 ma è rischioso per link-local scope id
                            if target_addr.is_none() {
                                if let Some(ip) = info.get_addresses().iter().next() {
                                     target_addr = Some(SocketAddr::new(*ip, info.get_port()));
                                }
                            }

                            if let Some(addr) = target_addr {
                                // CHIAVE MAPPA = DEVICE_ID (stabile)
                                // Se l'abbiamo gia, aggiorniamo IP e nome (se cambiato)
                                let peer_info = PeerInfo {
                                    name: device_name.clone(),
                                    ip: addr,
                                    device_id: peer_device_id.clone(),
                                    last_seen: std::time::SystemTime::now(),
                                };

                                let mut changed = false;
                                if let Some(mut existing) = peers.get_mut(&peer_device_id) {
                                    if existing.ip != addr || existing.name != device_name {
                                        *existing = peer_info.clone();
                                        changed = true;
                                    }
                                } else {
                                    peers.insert(peer_device_id.clone(), peer_info);
                                    changed = true;
                                    let msg = format!("➕ Peer Added: {} ({}) -> {}", device_name, peer_device_id, addr);
                                    println!("{}", msg);
                                    if let Some(tx) = &tx_event {
                                        let _ = tx.send(CoreEvent::Log(crate::events::LogEntry::new(&msg)));
                                    }
                                }
                                
                                if changed {
                                    send_update(&peers);
                                }
                            }
                        }
                    }
                }
                mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => {
                    // Questa rimozione è basata sul fullname del servizio mDNS, ma noi usiamo device_id come chiave.
                    // Dobbiamo trovare quale device_id corrisponde a questo service fullname? 
                    // Purtroppo ServiceRemoved non ci da le property.
                    // Tuttavia, abbiamo costruito il nome del servizio come "rustclip-{device_id}".
                    // Proviamo a estrarlo se possibile, altrimenti potremmo dover fare una ricerca inversa.
                    
                    // Se il fullname contiene rustclip-UUID...
                    if let Some(start) = fullname.find("rustclip-") {
                         let rest = &fullname[start + 9..];
                         let end = rest.find('.').unwrap_or(rest.len());
                         let extracted_id = &rest[..end];
                         
                         if peers.contains_key(extracted_id) {
                             println!("➖ Servizio mDNS Rimosso: {} ({})", fullname, extracted_id);
                             // peers.remove(extracted_id); // NON RIMUOVIAMO SUBITO! 
                             // Il problema 1 dice: "i peer quando si disconnettono non vengono rimossi dalla lista".
                             // Ma se rimuoviamo qui, risolviamo quel problema?
                             // mDNS dice che il servizio è andato via (es. sleep o chiusura corretta).
                             // Se crasha, non manda ServiceRemoved.
                             // Quindi: se riceviamo questo, è sicuro rimuovere? Sì.
                             if peers.remove(extracted_id).is_some() {
                                 send_update(&peers);
                             }
                         }
                    }
                }
                _ => {} 
            }
        }
        thread::sleep(Duration::from_millis(500));
        
        // OPZIONALE: Cleanup peer vecchi?
        // Per ora ci basiamo sull'errore di connessione implementato in clipboard.rs e su mDNS remove.
    }
}

pub fn sanitize_device_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}