#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use rust_clip::core::identity::RingIdentity;
use rust_clip::mesh::topology::Topology;
use rust_clip::mesh::swarm::Swarm;
use rust_clip::transport::lan::mdns::MdnsService;
use rust_clip::transport::lan::udp::UdpTransport;
use clap::{Parser, Subcommand};

#[cfg(target_os = "windows")]
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    New,
    Join,
    Start,
}

fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    if args.command.is_some() {
        attach_console_if_windows();
    }

    match args.command {
        Some(Commands::Start) | None => run_node()?,
        Some(Commands::New) => {
            let _ = RingIdentity::create_new()?;
        }
        Some(Commands::Join) => {
            print!("Enter ring mnemonic: ");
            use std::io::{self, Write};
            io::stdout().flush()?;
            let mut phrase = String::new();
            io::stdin().read_line(&mut phrase)?;
            let id = RingIdentity::from_mnemonic(phrase.trim())?;
            id.save()?;
        }
    }

    Ok(())
}

/// Main node startup: identity → transports → swarm
fn run_node() -> anyhow::Result<()> {
    // 1. Load or create identity
    let identity = RingIdentity::load().unwrap_or_else(|_| {
        RingIdentity::create_new().expect("Failed to create identity")
    });

    println!("  [Node] Identity loaded");
    println!("  [Node] StablePeerId: {}", identity.stable_peer_id());
    println!("  [Node] Ed25519 PubKey: {}", hex::encode(identity.public_key.as_bytes()));

    // 2. Create topology
    let topology = Topology::new();

    // 3. Create Swarm
    let mut swarm = Swarm::new(identity.clone(), topology.clone());
    let event_tx = swarm.event_sender();

    // 4. BLE send channel (Swarm → BLE transport)
    let (ble_send_tx, ble_send_rx) = flume::unbounded::<Vec<u8>>();

    // 5. Start UDP + mDNS (LAN transport)
    let udp_transport = match UdpTransport::new() {
        Ok(udp) => {
            let port = udp.get_port();
            println!("  [Node] UDP bound to port {}", port);

            // Start UDP listener → pushes TransportEvents
            let udp_event_tx = event_tx.clone();
            if let Err(e) = udp.start_with_events(udp_event_tx) {
                println!("  [Node] UDP listener failed: {}", e);
            }

            // Start mDNS discovery
            if let Ok(mdns) = MdnsService::new(&identity, topology.clone(), port) {
                if let Err(e) = mdns.start() {
                    println!("  [Node] mDNS failed: {}", e);
                } else {
                    println!("  [Node] mDNS started");
                }
            }

            Some(udp)
        }
        Err(e) => {
            println!("  [Node] UDP bind failed: {}", e);
            None
        }
    };

    // 6. Start BLE + Swarm
    // macOS: BLE needs the main thread RunLoop
    // Windows: BLE runs in tokio async context
    // The Swarm runs in a background thread

    let ble_event_tx = event_tx.clone();

    // Spawn Swarm in background thread
    let swarm_udp = udp_transport.clone();
    let swarm_ble_tx = ble_send_tx.clone();
    std::thread::spawn(move || {
        swarm.run(vec![], Some(swarm_ble_tx), swarm_udp)
            .expect("Swarm crashed");
    });

    // Platform-specific BLE startup (blocks main thread on macOS)
    #[cfg(target_os = "macos")]
    {
        use rust_clip::transport::ble::macos::run_ble_runloop;
        if let Err(e) = run_ble_runloop(ble_event_tx, ble_send_rx) {
            eprintln!("  [Node] macOS BLE RunLoop error: {}", e);
        }
    }

    #[cfg(target_os = "windows")]
    {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            use rust_clip::transport::ble::windows::start_ble_service;
            if let Err(e) = start_ble_service(ble_event_tx, ble_send_rx).await {
                eprintln!("  [Node] Windows BLE error: {}", e);
            }
            // Keep alive
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        });
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // Linux: no BLE yet, just park
        println!("  [Node] BLE not supported on this platform. Running LAN only.");
        loop {
            std::thread::park();
        }
    }

    Ok(())
}

fn attach_console_if_windows() {
    #[cfg(target_os = "windows")]
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
