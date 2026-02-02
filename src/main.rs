mod core;
mod ui;
mod events;

use clap::{Parser, Subcommand};
use core::identity::RingIdentity;
use core::{discovery, clipboard};
use std::io::{self, Write};
use std::sync::Arc;
use dashmap::DashMap;
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
enum Commands {
    New, Join, Start, Gui
}

// Entry point principale (non è async perché Egui vuole il main thread)
fn main() -> anyhow::Result<()> {
    let args = Cli::parse();

    match args.command {
        // CLI Pura
        Some(Commands::Start) => {
            attach_console_if_windows();
            run_async_backend(None, None)?; // Nessun canale UI
        }
        Some(Commands::New) => { /* (Codice CLI uguale a prima...) */ 
            attach_console_if_windows();
            let _ = RingIdentity::create_new()?;
        }
        Some(Commands::Join) => { /* (Codice CLI uguale a prima...) */
             attach_console_if_windows();
             // ... logica join ...
        }
        
        // GUI MODE (Default)
        None | Some(Commands::Gui) => {
            // 1. Creiamo i canali
            let (tx_ui, rx_core) = flume::unbounded::<UiCommand>(); // UI -> Core
            let (tx_core, rx_ui) = flume::unbounded::<CoreEvent>(); // Core -> UI

            // 2. Lanciamo il Backend in un thread separato
            std::thread::spawn(move || {
                if let Err(e) = run_async_backend(Some(rx_core), Some(tx_core)) {
                    eprintln!("CRITICAL BACKEND ERROR: {}", e);
                }
            });

            // 3. Lanciamo la GUI (Blocca il main thread)
            ui::run_gui(tx_ui, rx_ui)?;
        }
    }
    Ok(())
}

// Wrapper per avviare il runtime Tokio
fn run_async_backend(
    rx_cmd: Option<Receiver<UiCommand>>, 
    tx_event: Option<Sender<CoreEvent>>
) -> anyhow::Result<()> {
    
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // --- AVVIO LOGICA CORE ---
        // TODO: Qui integreremo i canali nel prossimo step.
        // Per ora avviamo il sync normale come prima, ma mandiamo un log di prova.
        
        if let Some(tx) = &tx_event {
             let _ = tx.send(CoreEvent::Log(events::LogEntry {
                 timestamp: "00:00:00".to_string(),
                 level: events::LogLevel::Info,
                 message: "Backend Avviato!".to_string()
             }));
        }

        // Caricamento Identità
        core::firewall::ensure_open_port();
        let identity = match RingIdentity::load() {
            Ok(id) => {
                if let Some(tx) = &tx_event {
                    let _ = tx.send(CoreEvent::IdentityLoaded(id.clone()));
                }
                id
            },
            Err(_) => {
                if let Some(tx) = &tx_event {
                    let _ = tx.send(CoreEvent::Log(events::LogEntry {
                        timestamp: "".to_string(), level: events::LogLevel::Error,
                        message: "Nessuna identità trovata! Vai nelle impostazioni.".to_string()
                    }));
                }
                // Rimaniamo vivi per permettere alla GUI di fare "New Ring"
                loop { tokio::time::sleep(std::time::Duration::from_secs(1)).await; }
            }
        };

        let peers: discovery::PeerMap = Arc::new(DashMap::new());

        // Avvio moduli
        let d_id = identity.clone();
        let d_peers = peers.clone();
        std::thread::spawn(move || {
            let _ = discovery::start_lan_discovery(d_id, d_peers);
        });

        clipboard::start_clipboard_sync(identity, peers).await
    })
}

fn attach_console_if_windows() {
    #[cfg(target_os = "windows")]
    unsafe { let _ = AttachConsole(ATTACH_PARENT_PROCESS); }
}