mod core;
mod ui;
mod events;

use clap::{Parser, Subcommand};
use core::identity::RingIdentity;
use core::config::AppConfig; // <--- Import Config
use core::{discovery, clipboard};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
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
        Some(Commands::Start) => {
            attach_console_if_windows();
            run_async_backend(None, None)?;
        }
        Some(Commands::New) => { 
            attach_console_if_windows();
            let _ = RingIdentity::create_new()?;
        }
        Some(Commands::Join) => {
             attach_console_if_windows();
             print!("Inserisci le parole del ring: ");
             use std::io::{self, Write};
             io::stdout().flush()?;
             let mut phrase = String::new();
             io::stdin().read_line(&mut phrase)?;
             match RingIdentity::from_mnemonic(phrase.trim()) {
                 Ok(id) => { id.save()?; println!("✅ Salvato."); }
                 Err(e) => println!("❌ Errore: {}", e),
             }
        }
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
    rx_cmd: Option<Receiver<UiCommand>>, 
    tx_event: Option<Sender<CoreEvent>>  
) -> anyhow::Result<()> {
    
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        if let Some(tx) = &tx_event {
             let _ = tx.send(CoreEvent::Log(events::LogEntry {
                 timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                 level: events::LogLevel::Success,
                 message: "Backend Avviato!".to_string()
             }));
        }

        core::firewall::ensure_open_port();
        
        // 1. CARICA CONFIGURAZIONE
        let mut config = AppConfig::load(); // <--- LOAD

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
                RingIdentity::create_new()? 
            }
        };

        let peers: discovery::PeerMap = Arc::new(DashMap::new());
        let paused = Arc::new(AtomicBool::new(false)); 

        // --- AVVIO SERVIZI CON CONFIG ---
        let d_id = identity.clone();
        let d_peers = peers.clone();
        let d_tx = tx_event.clone();
        let d_config = config.clone(); // Clone config per discovery
        
        std::thread::spawn(move || {
            let _ = discovery::start_lan_discovery(d_id, d_peers, d_config, d_tx);
        });

        let c_id = identity.clone();
        let c_peers = peers.clone();
        let c_pause = paused.clone();
        let c_config = config.clone(); // Clone config per clipboard
        
        tokio::spawn(async move {
            let _ = clipboard::start_clipboard_sync(c_id, c_peers, c_config, c_pause).await;
        });

        // --- LOOP COMANDI ---
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
                    UiCommand::UpdateConfig(new_config) => { // <--- GESTIONE SALVATAGGIO
                        if let Err(e) = new_config.save() {
                            eprintln!("Errore salvataggio config: {}", e);
                        } else {
                            if let Some(tx) = &tx_event {
                                let _ = tx.send(CoreEvent::Log(events::LogEntry {
                                    timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                                    level: events::LogLevel::Info,
                                    message: "Configurazione salvata (alcune modifiche richiedono riavvio)".to_string()
                                }));
                            }
                        }
                        config = new_config; 
                    },
                    UiCommand::GenerateNewIdentity => {
                        let _ = RingIdentity::create_new();
                        if let Some(tx) = &tx_event {
                             let _ = tx.send(CoreEvent::Log(events::LogEntry {
                                timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                                level: events::LogLevel::Success,
                                message: "Nuova identità! RIAVVIA L'APP.".to_string()
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
                                        message: "Join OK! RIAVVIA L'APP.".to_string()
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
            loop { tokio::time::sleep(std::time::Duration::from_secs(3600)).await; }
        }

        Ok(())
    })
}

fn attach_console_if_windows() {
    #[cfg(target_os = "windows")]
    unsafe { let _ = AttachConsole(ATTACH_PARENT_PROCESS); }
}