#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use rust_clip::core;
use rust_clip::events;
use clap::{Parser, Subcommand};
use core::identity::RingIdentity;
use core::config::AppConfig;
// use core::{discovery, clipboard}; // Legacy modules disabled for Phase 1
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use flume::{Sender, Receiver};
use events::{UiCommand, CoreEvent};
use crate::core::packet::{WirePacket, PacketType, HandshakeMsg};
use x25519_dalek::{EphemeralSecret, PublicKey};
use hkdf::Hkdf;
use sha2::Sha256;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit};
use rust_clip::mesh::topology::Topology;
use rust_clip::transport::TransportType;
use rust_clip::transport::lan::mdns::MdnsService;
use rust_clip::transport::lan::udp::UdpTransport;
use rust_clip::mesh::gossip::VectorClock;
use std::collections::HashMap;

#[cfg(target_os = "windows")]
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands { New, Join, Start, Gui }

fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    if args.command.is_some() && !matches!(args.command, Some(Commands::Gui)) {
        attach_console_if_windows();
    }

    match args.command {
        Some(Commands::Start) | None => {
            // 1. Load Identity on Main Thread (Safe)
            let load_ident = || RingIdentity::load().unwrap_or_else(|_| RingIdentity::create_new().unwrap());
            let identity = load_ident();

            println!("✅ Phase 1 Identity Verification");
            println!("------------------------------");
            println!("Mnemonic: [HIDDEN]");
            println!("Rotating ID: {}", identity.get_rotating_id());
            println!("Ed25519 PubKey: {:?}", hex::encode(identity.public_key.as_bytes()));
            println!("------------------------------");

            // 2. Clone Identity for Backend
            let id_backend = identity.clone();

            // 3. Create Data Channels (Packet Flow)
            // INBOUND: tx_packet (BLE) -> rx_packet (Backend)
            use rust_clip::core::packet::WirePacket;
            let (tx_packet, rx_packet) = flume::unbounded::<WirePacket>();
            let tx_backend = tx_packet.clone(); // For Windows (and self-sending if needed)

            // OUTBOUND: tx_out (Backend) -> Router (rx_out) -> [tx_ble (BLE) | udp (LAN)]
            let (tx_out, rx_out) = flume::unbounded::<WirePacket>();
            
            // BLE SPECIFIC CHANNEL
            let (tx_ble, rx_ble) = flume::unbounded::<WirePacket>();

            // 4. Initialize Topology (Mesh State)
            let topology = Topology::new();

            
            // 6. Spawn Router Thread (Hybrid Mesh Logic)
            let router_topology = topology.clone();
            // We need UDP Transport reference here if available.
            // But UDP is created locally inside match block.
            // Let's restructure to ensure UDP scope availability.
            let udp_transport = match UdpTransport::new() {
                 Ok(udp) => {
                     let port = udp.get_port();
                     println!("✅ [UDP] Bound to port {}", port);
                     
                     // Start Listener
                     if let Err(e) = udp.start(tx_packet.clone()) {
                          println!("⚠️ [UDP] Failed to start listener: {}", e);
                     }
 
                     // Start mDNS
                     let mdns_topology_clone = router_topology.clone(); // Re-clone locally
                     if let Ok(mdns_service) = MdnsService::new(&identity, mdns_topology_clone, port) {
                         if let Err(e) = mdns_service.start() {
                             println!("⚠️ [mDNS] Startup Failed: {}", e);
                         } else {
                             println!("✅ [mDNS] Service Started (Background)");
                         }
                     }
                     Some(udp)
                 },
                 Err(e) => {
                     println!("⚠️ [UDP] Failed to bind: {}", e);
                     None
                 }
            };

            let rx_out_router = rx_out.clone();
            std::thread::spawn(move || {
                // Router Loop
                while let Ok(packet) = rx_out_router.recv() {
                    let receiver = &packet.header.receiver_id;
                    let mut sent_via_lan = false;
                    
                    // Check Topology for Route
                    if let Some(entry) = router_topology.peers.get(receiver) {
                        if let Some((t_type, addr)) = entry.get_best_transport() {
                            match t_type {
                                TransportType::Mdns | TransportType::TcpDirect => {
                                    if let Some(ip) = addr {
                                        if let Some(udp) = &udp_transport {
                                            println!("🚀 [Router] Sending via LAN (UDP) to {}...", receiver);
                                            if let Err(e) = udp.send(ip, &packet) {
                                                println!("❌ [Router] UDP Send Failed: {}", e);
                                            } else {
                                                sent_via_lan = true;
                                            }
                                        }
                                    }
                                },
                                TransportType::Ble => {
                                    // Fallthrough to BLE
                                }
                            }
                        }
                    } 
                    
                    if !sent_via_lan {
                        // Default / Fallback to BLE
                        // println!("🚀 [Router] Sending via BLE to {}...", receiver);
                        let _ = tx_ble.send(packet);
                    }
                }
            });

            // 7. Spawn Tokio Backend in Background Thread
            let rx_out_clone = rx_out.clone();
            let topology_backend = topology.clone();

            std::thread::spawn(move || {
                run_async_backend(id_backend, Some(rx_packet), tx_backend, Some(tx_out), Some(rx_out_clone), topology_backend).expect("Backend Crashed");
            });

            // 5. Main Thread Platform Specifics
            #[cfg(target_os = "macos")]
            {
                // 5. Main Thread Platform Specifics (macOS needs RunLoop)
        use rust_clip::transport::ble::macos::run_ble_runloop;
        if let Err(e) = run_ble_runloop(identity, tx_packet, rx_ble) {
             eprintln!("🔥 [Rust-Mac] RunLoop Error: {}", e);
        }
    }

    #[cfg(target_os = "windows")]
    {
        // 5. Windows specific initialization
        // We use pure async with Tokio, but might need main thread for COM in some cases?
        // Typically Windows Runtime is fine on any thread if MTA.
        // But for consistency let's spawn a dedicated runtime block or just block.
        
        // We need to pass rx_ble, not rx_out!
        let rt = tokio::runtime::Runtime::new()?;
        use crate::transport::Transport;
        use crate::transport::ble::windows;

        let ble_transport = windows::BleTransport::new(identity, tx_packet, Some(rx_ble));

        if let Err(e) = rt.block_on(ble_transport.start()) {
             eprintln!("🔥 [Rust-Windows] BLE Start Error: {}", e);
        }
    }

            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                // On Linux, just park or join.
                loop { std::thread::park(); }
            }
        },
        Some(Commands::New) => { let _ = RingIdentity::create_new()?; }
        Some(Commands::Join) => {
             print!("Inserisci le parole del ring: ");
             use std::io::{self, Write};
             io::stdout().flush()?;
             let mut phrase = String::new();
             io::stdin().read_line(&mut phrase)?;
             let id = RingIdentity::from_mnemonic(phrase.trim())?;
             id.save()?;
        }
        Some(Commands::Gui) => {
            println!("GUI not implemented in Phase 1/2 Refactor");
        }
    }
    Ok(())
}

fn run_async_backend(
    identity: RingIdentity, 
    _rx_packet: Option<Receiver<WirePacket>>, 
    tx_packet: Sender<WirePacket>,
    _tx_out: Option<Sender<WirePacket>>,
    rx_out: Option<Receiver<WirePacket>>,
    topology: Topology,
) -> anyhow::Result<()> {
    // Phase 4: Gossip State
    let vector_clock = Arc::new(std::sync::Mutex::new(VectorClock::new()));
    let clock_backend = vector_clock.clone();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // --- PHASE 2: BLE START (Async Part) ---
        // On macOS, the actual BLE loop is on Main Thread.
        // On Windows, start() does the work.
        
        use rust_clip::transport::{Transport, ble::BleTransport};
        let ble = BleTransport::new(identity.clone(), tx_packet, rx_out);
        
        // This will print a warning on macOS and do nothing, which is correct now.
        // On Windows, it starts the WinRT service.
        if let Err(e) = ble.start().await {
             println!("❌ BLE Start Failed: {}", e);
        } else {
             #[cfg(not(target_os = "macos"))]
             println!("✅ BLE Service Initialized (Windows/Linux)");
        }
        
        // Receiver Loop: Print incoming packets
        if let Some(rx) = _rx_packet {
            let tx_out_loop = _tx_out.clone();
            let id_loop = identity.clone();
            
            tokio::spawn(async move {
                println!("👂 Backend listening for packets...");
                
                // State for Handshake (Pendings)
                // Note: In a real multi-peer scenario, this should be a HashMap<PeerID, EphemeralSecret>
                // For Phase 2.5/3 (Single Peer focus), we can use a single slot or map by SourceID (if LinkUp provides it?)
                // LinkUp currently has SourceID = Local Rotating ID.
                // But we need to map the SECRET to the PEER we sent Hello to.
                // Since we don't know PeerID when sending Hello (Client mode), we just hold one pending secret.
                // Limitation: Only one concurrent handshake initiation.
                let mut pending_secret: Option<EphemeralSecret> = None;

                while let Ok(packet) = rx.recv_async().await {
         // 3. Process Packet
        // Check RELAY Logic
        let my_id = identity.get_rotating_id();
        if packet.header.receiver_id != "broadcast" && packet.header.receiver_id != my_id {
            // Forward / Relay
            // If receiver_id IS NOT me, relay it back out via Router
            if let Some(tx) = &tx_out_loop { // Use tx_out_loop here
                println!("🔄 [Backend] Relaying packet for {}", packet.header.receiver_id);
                // We just put it back in the outbound queue. 
                // The Router will decide where it goes.
                // TODO: Verify TTL or Loop Prevention!
                let _ = tx.send(packet);
            }
            continue;
        }

        match packet.header.packet_type {
            PacketType::LinkUp => {
                            println!("🔗 [Backend] LinkUp received! Initiating Handshake...");
                            // 1. Generate Ephemeral Keys
                            let (secret, public) = RingIdentity::generate_ephemeral_key();
                            pending_secret = Some(secret);

                            // 2. Send Hello
                            if let Some(tx) = &tx_out_loop {
                                let hello_msg = HandshakeMsg::Hello {
                                    pubkey: id_loop.public_key.as_bytes().to_vec(),
                                    rotating_id: id_loop.get_rotating_id(),
                                    ephemeral_key: *public.as_bytes(),
                                    vector_clock: clock_backend.lock().unwrap().clocks.clone(), // Send Local Clock
                                };
                                if let Ok(bytes) = bincode::serialize(&hello_msg) {
                                    if let Ok(pkt) = WirePacket::new_plain(
                                        id_loop.get_rotating_id(),
                                        packet.header.sender_id.clone(),
                                        PacketType::Hello,
                                        &bytes,
                                        &id_loop.identity_key
                                    ) {
                                        let _ = tx.send_async(pkt).await;
                                    }
                                }
                            }
                        },
                        PacketType::Hello => {
                            // Parse Payload
                            if let Ok(msg) = bincode::deserialize::<HandshakeMsg>(&packet.payload) {
                                if let HandshakeMsg::Hello { pubkey: peer_pk_bytes, rotating_id, ephemeral_key: peer_ek_bytes, vector_clock: peer_clock } = msg {
                                    println!("👋 [Backend] Received Hello from {}! Reply Welcome...", rotating_id);
                                    
                                    // Merge Clocks (Passive Re-entry)
                                    // If remote has newer clock, we might be stale.
                                    // Merge remote clock into local.
                                    {
                                        let mut local_vc = clock_backend.lock().unwrap();
                                        // TODO: Intelligent merge? Or just union-max?
                                        local_vc.merge(&VectorClock { clocks: peer_clock.clone() });
                                        println!("🕰️ [Backend] Merged VectorClock: {:?}", local_vc);
                                    }
                                    
                                    // 1. Generate OUR Ephemeral Keys
                                    let (secret, public) = RingIdentity::generate_ephemeral_key();
                                    
                                    // 2. Compute Shared Secret
                                    let peer_public = PublicKey::from(peer_ek_bytes);
                                    let shared_secret = secret.diffie_hellman(&peer_public);
                                    
                                    // 3. Derive Session Key (HKDF)
                                    let hkdf = Hkdf::<Sha256>::new(Some(b"rust-clip-v1"), shared_secret.as_bytes());
                                    let mut key_bytes = [0u8; 32];
                                    if hkdf.expand(b"session_key", &mut key_bytes).is_ok() {
                                        println!("🔑 [Backend-Server] Session Key Derived: {:?}...", &key_bytes[0..4]);
                                        // Store in Topology
                                        let session_key = ChaCha20Poly1305::new(&key_bytes.into());
                                        topology.add_or_update(
                                            rotating_id.clone(), 
                                            peer_pk_bytes, 
                                            Some(session_key),
                                            Some((TransportType::Ble, None))
                                        );
                                        println!("🗂️ [Backend] Peer {} stored in Topology (BLE).", rotating_id);
                                    }
                                    
                                    // 4. Reply Welcome
                                    if let Some(tx) = &tx_out_loop {
                                        let welcome_msg = HandshakeMsg::Welcome {
                                            pubkey: id_loop.public_key.as_bytes().to_vec(),
                                            ephemeral_key: *public.as_bytes(),
                                            vector_clock: clock_backend.lock().unwrap().clocks.clone(), // Send Updated Local Clock
                                        };
                                        if let Ok(bytes) = bincode::serialize(&welcome_msg) {
                                            if let Ok(reply) = WirePacket::new_plain(
                                                id_loop.get_rotating_id(),
                                                packet.header.sender_id.clone(),
                                                PacketType::Welcome,
                                                &bytes,
                                                &id_loop.identity_key
                                            ) {
                                                let _ = tx.send_async(reply).await;
                                            }
                                        }
                                    }
                                }
                            }
                        },
                        PacketType::Welcome => {
                            if let Ok(msg) = bincode::deserialize::<HandshakeMsg>(&packet.payload) {
                                 if let HandshakeMsg::Welcome { pubkey: _peer_pk_bytes, ephemeral_key: peer_ek_bytes, vector_clock: peer_clock } = msg {
                                     println!("🤝 [Backend] Session ESTABLISHED! (Welcome received)");
                                     
                                     // Merge Clocks
                                     {
                                        let mut local_vc = clock_backend.lock().unwrap();
                                        local_vc.merge(&VectorClock { clocks: peer_clock });
                                        println!("🕰️ [Backend] VectorClock synced: {:?}", local_vc);
                                     }
                                     
                                     // 1. Retrieve Pending Secret
                                     if let Some(secret) = pending_secret.take() {
                                         // 2. Compute Shared Secret
                                         let peer_public = PublicKey::from(peer_ek_bytes);
                                         let shared_secret = secret.diffie_hellman(&peer_public);
                                         
                                         // 3. Derive Session Key
                                         let hkdf = Hkdf::<Sha256>::new(Some(b"rust-clip-v1"), shared_secret.as_bytes());
                                         let mut key_bytes = [0u8; 32];
                                         if hkdf.expand(b"session_key", &mut key_bytes).is_ok() {
                                            println!("🔑 [Backend-Client] Session Key Derived: {:?}...", &key_bytes[0..4]);
                                            // TODO: Store in Topology (We don't know PeerID exactly yet, handshake needs msg update?)
                                            // Actually HandshakeMsg::Welcome uses Peer PubKey, but Client needs to associate it.
                                            // For now, assume LinkUp provided SourceID? Unlikely.
                                            // HandshakeMsg::Welcome needs to carry PeerID if not already known.
                                            // Wait, Welcome packet has no ID field in my struct currently?
                                            // Checking packet.rs: Welcome { pubkey, ephemeral_key }
                                            // We can use the packet.header.sender_id!
                                            
                                            let peer_id = packet.header.sender_id.clone();
                                            let session_key = ChaCha20Poly1305::new(&key_bytes.into());
                                            topology.add_or_update(
                                                peer_id.clone(), 
                                                _peer_pk_bytes, 
                                                Some(session_key),
                                                Some((TransportType::Ble, None))
                                            );
                                             println!("🗂️ [Backend] Peer {} stored in Topology (BLE).", peer_id);
                                         }
                                     } else {
                                         println!("⚠️ [Backend] Received Welcome but no Pending Secret found!");
                                     }
                                 }
                            }
                        },
                        _ => println!("📦 [Backend] Received Data Packet: {} bytes", packet.payload.len()),
                    }
                }
            });
        }

        println!("Waiting for peers (Phase 2+)...");

        // Keep alive
        loop { tokio::time::sleep(std::time::Duration::from_secs(3600)).await; }
    })
}

fn attach_console_if_windows() {
    #[cfg(target_os = "windows")]
    unsafe { let _ = AttachConsole(ATTACH_PARENT_PROCESS); }
}