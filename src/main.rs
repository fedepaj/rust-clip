mod identity;
mod discovery; // Include il modulo discovery.rs

use clap::{Parser, Subcommand};
use identity::RingIdentity;
use std::io::{self, Write};

#[derive(Parser)]
#[command(name = "rust-clip")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Crea nuovo Ring (Genera parole)
    New,
    /// Unisciti a Ring (Inserisci parole)
    Join,
    /// Avvia la sincronizzazione (Discovery LAN)
    Start,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::New => {
            let _id = RingIdentity::create_new()?;
            println!("✅ Ora puoi lanciare 'rust-clip start' su questo PC.");
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
            println!("📂 Caricamento configurazione...");
            let identity = match RingIdentity::load() {
                Ok(id) => id,
                Err(_) => {
                    println!("⚠️ Nessuna configurazione trovata. Ne creo una temporanea.");
                    RingIdentity::create_new()?
                }
            };
            
            // Avvia la discovery (Bloccante)
            discovery::start_lan_discovery(identity)?;
            Ok(())
        }
    }
}