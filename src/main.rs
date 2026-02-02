mod core;
mod ui;
mod events;

use clap::{Parser, Subcommand};
use core::identity::RingIdentity;
use core::{discovery, clipboard};
use std::io::{self, Write};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}}; // <--- Atomic
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

fn main() -> anyhow::Result<()> {
    let args = Cli::parse();

    match args.command {
        // CLI (Nessuna modifica importante qui)
        Some(Commands::Start) => {
            attach_console_if_windows();
            run_async_backend(None, None)?;
        }
        Some(Commands::New) => { /* ... */ 
            attach_console_if_windows();
            let _ = RingIdentity::create_new()?;
        }
        Some(Commands::Join) => { /* ... */
             attach_console_if_windows();
             // ... codice join ...
        }
        
        // GUI MODE
        None | Some(Commands::Gui) => {
            let (tx_ui, rx_core) = flume::unbounded::<UiCommand>(); 
            let (tx_core, rx_ui) = flume::unbounded::<CoreEvent>(); 

            std::thread::spawn(move || {
                if let Err(e) = run_async_backend(Some(rx_core), Some(tx_core)) {
                    eprintln!("CRITICAL BACKEND ERROR: {}", e);
                }
            });

            ui::run_gui(tx_ui, rx_ui)?;
        }
    }
    Ok(())
}

fn run_async_backend(
    rx_cmd: Option<Receiver<UiCommand>>, // Canale comandi (Opzionale se CLI)
    tx_event: Option<Sender<CoreEvent>>  // Canale eventi (Opzionale se CLI)
) -> anyhow::Result<()> {
    
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // --- INIZIALIZZAZIONE ---
        if let Some(tx) = &tx_event {
             let _ = tx.send(CoreEvent::Log(events::LogEntry {
                 timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                 level: events::LogLevel::Success,
                 message: "Backend Avviato!".to_string()
             }));
        }

        core::firewall::ensure_open_port();
        
        // Carica identità
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
                        timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                        level: events::LogLevel::Warn,
                        message: "Nessuna identità! Crea o unisciti a un Ring.".to_string()
                    }));
                }
                // Se non c'è identità, creiamo un placeholder vuoto per permettere al loop comandi di girare
                // In una versione avanzata gestiremo il restart completo.
                // Per ora: Crash gentile o attesa comandi.
                // Creiamo una identità temporanea per non crashare, ma avvisiamo.
                RingIdentity::create_new()? 
            }
        };

        // Stato Condiviso
        let peers: discovery::PeerMap = Arc::new(DashMap::new());
        let paused = Arc::new(AtomicBool::new(false)); // False = Attivo

        // --- AVVIO SERVIZI ---
        let d_id = identity.clone();
        let d_peers = peers.clone();
        let d_tx = tx_event.clone();
        
        // Discovery Task
        std::thread::spawn(move || {
            let _ = discovery::start_lan_discovery(d_id, d_peers, d_tx);
        });

        // Clipboard Task
        let c_id = identity.clone();
        let c_peers = peers.clone();
        let c_pause = paused.clone();
        tokio::spawn(async move {
            let _ = clipboard::start_clipboard_sync(c_id, c_peers, c_pause).await;
        });

        // --- LOOP GESTIONE COMANDI (Controller) ---
        // Se siamo in GUI mode, ascoltiamo i comandi. Se CLI, aspettiamo all'infinito.
        if let Some(rx) = rx_cmd {
            while let Ok(cmd) = rx.recv_async().await {
                match cmd {
                    UiCommand::SetPaused(state) => {
                        paused.store(state, Ordering::Relaxed);
                        let msg = if state { "Sincronizzazione PAUSA" } else { "Sincronizzazione ATTIVA" };
                        if let Some(tx) = &tx_event {
                            let _ = tx.send(CoreEvent::ServiceStateChanged { running: !state });
                            let _ = tx.send(CoreEvent::Log(events::LogEntry {
                                timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                                level: events::LogLevel::Info,
                                message: msg.to_string()
                            }));
                        }
                    },
                    UiCommand::GenerateNewIdentity => {
                        // TODO: Implementare hot-reload (complesso).
                        // Per ora: Creiamo, salviamo e chiediamo riavvio.
                        let _ = RingIdentity::create_new();
                        if let Some(tx) = &tx_event {
                             let _ = tx.send(CoreEvent::Log(events::LogEntry {
                                timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                                level: events::LogLevel::Success,
                                message: "Nuova identità creata! RIAVVIA L'APP.".to_string()
                            }));
                        }
                    },
                    UiCommand::JoinRing(phrase) => {
                        match RingIdentity::from_mnemonic(&phrase) {
                            Ok(id) => {
                                let _ = id.save();
                                if let Some(tx) = &tx_event {
                                     let _ = tx.send(CoreEvent::Log(events::LogEntry {
                                        timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                                        level: events::LogLevel::Success,
                                        message: "Join effettuato! RIAVVIA L'APP.".to_string()
                                    }));
                                }
                            },
                            Err(e) => {
                                if let Some(tx) = &tx_event {
                                     let _ = tx.send(CoreEvent::Log(events::LogEntry {
                                        timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                                        level: events::LogLevel::Error,
                                        message: format!("Errore Join: {}", e)
                                    }));
                                }
                            }
                        }
                    },
                    UiCommand::Quit => {
                        std::process::exit(0);
                    }
                }
            }
        } else {
            // CLI Mode: Keep alive forever
            loop { tokio::time::sleep(std::time::Duration::from_secs(3600)).await; }
        }

        Ok(())
    })
}

fn attach_console_if_windows() {
    #[cfg(target_os = "windows")]
    unsafe { let _ = AttachConsole(ATTACH_PARENT_PROCESS); }
}