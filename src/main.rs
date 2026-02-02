#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // NASCONDE IL TERMINALE SU WINDOWS

use rust_clip::core;
use rust_clip::ui;
use rust_clip::events;

use clap::{Parser, Subcommand};
use core::identity::RingIdentity;
use core::config::AppConfig;
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
            let (tx_ui, rx_core) = flume::unbounded::<UiCommand>(); 
            let (tx_core, rx_ui) = flume::unbounded::<CoreEvent>(); 
            std::thread::spawn(move || {
                let _ = run_async_backend(Some(rx_core), Some(tx_core));
            });
            ui::run_gui(tx_ui, rx_ui)?;
        }
    }
    Ok(())
}

fn run_async_backend(rx_cmd: Option<Receiver<UiCommand>>, tx_event: Option<Sender<CoreEvent>>) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut config = AppConfig::load();
        let paused = Arc::new(AtomicBool::new(false));
        let peers: discovery::PeerMap = Arc::new(DashMap::new());

        // Helper per caricare l'identità
        let load_ident = || RingIdentity::load().unwrap_or_else(|_| RingIdentity::create_new().unwrap());
        let mut identity = load_ident();

        // SE CLI MODE (tx_event is None), creiamo un canale locale per gestire notifiche
        let local_rx = if tx_event.is_none() {
            let (tx, rx) = flume::unbounded::<CoreEvent>();
            Some(rx)
        } else {
            None
        };
        
        // Placeholder per logica futura se serve riutilizzare il canale
        let _ = local_rx; 

        // --- REFACTOR LOGIC START ---
        
        let (tx_internal, rx_internal) = if let Some(tx) = tx_event.clone() {
             (tx, None)
        } else {
             let (tx, rx) = flume::unbounded::<CoreEvent>();
             (tx, Some(rx))
        };

        if let Some(tx) = &tx_event {
            let _ = tx.send(CoreEvent::IdentityLoaded(identity.clone()));
        }

        // Se abbiamo un listener locale (CLI mode), avviamo handler
        if let Some(rx) = rx_internal {
             tokio::spawn(async move {
                 while let Ok(event) = rx.recv_async().await {
                     match event {
                         CoreEvent::Notify { title, body } => {
                             let _ = notify_rust::Notification::new().summary(&title).body(&body).show();
                         },
                         CoreEvent::Log(l) => println!("[{}] {}", l.timestamp, l.message),
                         _ => {}
                     }
                 }
             });
        }

        // --- GESTIONE TASK DINAMICI (Hot Reload) ---
        let mut discovery_handle: Option<tokio::task::JoinHandle<()>> = None;
        let mut sync_handle: Option<tokio::task::JoinHandle<()>> = None;

        // Macro/Closure per avviare/riavviare tutto
        let mut restart_services = |id: RingIdentity, cfg: AppConfig, p: discovery::PeerMap, pz: Arc<AtomicBool>, tx: Option<Sender<CoreEvent>>| {
            println!("🔄 Avvio/Riavvio servizi core...");
            if let Some(h) = discovery_handle.take() { h.abort(); }
            if let Some(h) = sync_handle.take() { h.abort(); }
            p.clear();

            let id_d = id.clone();
            let p_d = p.clone();
            let cfg_d = cfg.clone();
            let tx_d = tx.clone();
            discovery_handle = Some(tokio::spawn(async move {
                let _ = discovery::start_lan_discovery(id_d, p_d, cfg_d, tx_d);
            }));

            let id_s = id.clone();
            let p_s = p.clone();
            let cfg_s = cfg.clone();
            let pz_s = pz.clone();
            let tx_s = tx.clone(); // Passiamo TX anche qui
            sync_handle = Some(tokio::spawn(async move {
                let _ = clipboard::start_clipboard_sync(id_s, p_s, cfg_s, pz_s, tx_s).await;
            }));
        };

        // Primo avvio
        restart_services(identity.clone(), config.clone(), peers.clone(), paused.clone(), Some(tx_internal.clone()));

        if let Some(rx) = rx_cmd {
            while let Ok(cmd) = rx.recv_async().await {
                match cmd {
                    UiCommand::SetPaused(p) => paused.store(p, Ordering::Relaxed),
                    UiCommand::UpdateConfig(new_cfg) => {
                        let restart_needed = new_cfg.device_name != config.device_name;
                        new_cfg.save().ok();
                        config = new_cfg;
                        if restart_needed {
                            restart_services(identity.clone(), config.clone(), peers.clone(), paused.clone(), Some(tx_internal.clone()));
                        }
                    },
                    UiCommand::JoinRing(phrase) => {
                        if let Ok(id) = RingIdentity::from_mnemonic(&phrase) {
                            id.save().ok();
                            identity = id;
                            if let Some(tx) = &tx_event { let _ = tx.send(CoreEvent::IdentityLoaded(identity.clone())); }
                            restart_services(identity.clone(), config.clone(), peers.clone(), paused.clone(), Some(tx_internal.clone()));
                        }
                    },
                    UiCommand::GenerateNewIdentity => {
                        identity = RingIdentity::create_new().unwrap();
                        if let Some(tx) = &tx_event { let _ = tx.send(CoreEvent::IdentityLoaded(identity.clone())); }
                        restart_services(identity.clone(), config.clone(), peers.clone(), paused.clone(), Some(tx_internal.clone()));
                    }
                    UiCommand::Quit => std::process::exit(0),
                }
            }
        } else { loop { tokio::time::sleep(std::time::Duration::from_secs(3600)).await; } }
        Ok(())
    })
}

fn attach_console_if_windows() {
    #[cfg(target_os = "windows")]
    unsafe { let _ = AttachConsole(ATTACH_PARENT_PROCESS); }
}