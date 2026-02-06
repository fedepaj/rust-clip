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
use crate::core::packet::WirePacket;

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

            // 3. Create Data Channel (Packet Flow)
            // tx_packet -> Passed to BLE Adapters (Producers)
            // rx_packet -> Passed to Backend (Consumer)
            use rust_clip::core::packet::WirePacket;
            let (tx_packet, rx_packet) = flume::unbounded::<WirePacket>();
            let tx_backend = tx_packet.clone(); // For Windows (and self-sending if needed)

            // 4. Spawn Tokio Backend in Background Thread
            std::thread::spawn(move || {
                run_async_backend(id_backend, Some(rx_packet), tx_backend).expect("Backend Crashed");
            });

            // 5. Main Thread Platform Specifics
            #[cfg(target_os = "macos")]
            {
                // BLOCKING CALL: Runs NSRunLoop forever
                use rust_clip::transport::ble::macos::run_ble_runloop;
                // Pass tx directly to delegate
                run_ble_runloop(identity, tx_packet)?;
            }

            #[cfg(not(target_os = "macos"))]
            {
                // On Windows/Linux, just park or join.
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
    tx_packet: Sender<WirePacket>
) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // --- PHASE 2: BLE START (Async Part) ---
        // On macOS, the actual BLE loop is on Main Thread.
        // On Windows, start() does the work.
        
        use rust_clip::transport::{Transport, ble::BleTransport};
        let ble = BleTransport::new(identity.clone(), tx_packet);
        
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
            tokio::spawn(async move {
                println!("👂 Backend listening for packets...");
                while let Ok(packet) = rx.recv_async().await {
                    println!("📦 Backend Received Packet: {:?}", packet);
                    // Here we will trigger Handshake Logic
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