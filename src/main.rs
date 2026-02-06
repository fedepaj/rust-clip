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
        Some(Commands::Start) => run_async_backend(None, None)?,
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
        None | Some(Commands::Gui) => {
            // UI disabled for Phase 1 verification to avoid dependencies on legacy events
            // We just run the backend logic to verify Identity
            println!("Phase 1 Verification: Running Backend Logic only (GUI Temporarily Disabled)");
            run_async_backend(None, None)?;
        }
    }
    Ok(())
}

fn run_async_backend(rx_cmd: Option<Receiver<UiCommand>>, tx_event: Option<Sender<CoreEvent>>) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut config = AppConfig::load();
        let paused = Arc::new(AtomicBool::new(false));

        // Helper per caricare l'identità
        let load_ident = || RingIdentity::load().unwrap_or_else(|_| RingIdentity::create_new().unwrap());
        let mut identity = load_ident();

        println!("✅ Phase 1 Identity Verification");
        println!("------------------------------");
        println!("Mnemonic: [HIDDEN]");
        println!("Rotating ID: {}", identity.get_rotating_id());
        println!("Ed25519 PubKey: {:?}", hex::encode(identity.public_key.as_bytes()));
        println!("------------------------------");

        // --- PHASE 2: BLE START ---
        use rust_clip::transport::{Transport, ble::BleTransport};
        let ble = BleTransport::new(identity.clone());
        if let Err(e) = ble.start().await {
             println!("❌ BLE Start Failed: {}", e);
        } else {
             println!("✅ BLE Service Initialized (macOS: Advertising 'RustClip-Mac')");
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