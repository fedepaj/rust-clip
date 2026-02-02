mod identity;
mod discovery;
mod crypto;
mod clipboard;
mod firewall; // <--- AGGIUNGI QUESTO

use clap::{Parser, Subcommand};
use identity::RingIdentity;
use discovery::PeerMap;
use std::io::{self, Write};
use std::sync::Arc;
use dashmap::DashMap;

#[derive(Parser)]
#[command(name = "rust-clip")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    New,
    Join,
    Start,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::New => {
            let _id = RingIdentity::create_new()?;
            println!("✅ Configurazione completata.");
            Ok(())
        }
        Commands::Join => {
            print!("Inserisci le parole del ring: ");
            io::stdout().flush()?;
            let mut phrase = String::new();
            io::stdin().read_line(&mut phrase)?;
            
            match RingIdentity::from_mnemonic(phrase.trim()) {
                Ok(identity) => {
                    identity.save()?; 
                    println!("✅ Salvato. Ora lancia 'rust-clip start'.");
                },
                Err(e) => println!("❌ Errore: {}", e),
            }
            Ok(())
        }
        Commands::Start => {
            println!("🚀 Avvio RustClip...");

            // 1. CONFIGURAZIONE FIREWALL (Solo Windows)
            // Se fallisce, avvisa l'utente ma continua comunque
            firewall::ensure_open_port();
            
            // 2. Carica Identità
            let identity = match RingIdentity::load() {
                Ok(id) => id,
                Err(_) => {
                    println!("⚠️ Nessuna configurazione. Esegui 'new' o 'join' prima.");
                    return Ok(());
                }
            };

            // 3. Crea Mappa Peers condivisa
            let peers: PeerMap = Arc::new(DashMap::new());

            // 4. Avvia Discovery
            let disc_identity = identity.clone();
            let disc_peers = peers.clone();
            
            std::thread::spawn(move || {
                if let Err(e) = discovery::start_lan_discovery(disc_identity, disc_peers) {
                    eprintln!("Errore Discovery: {}", e);
                }
            });

            // 5. Avvia Clipboard Sync
            clipboard::start_clipboard_sync(identity, peers).await?;
            
            Ok(())
        }
    }
}