mod identity;
mod ble;       // Lo scanner (ricevitore)
mod broadcast; // Il nuovo advertising (trasmettitore)

use clap::{Parser, Subcommand};
use identity::RingIdentity;
use std::io::{self, Write};

#[derive(Parser)]
#[command(name = "rust-clip")]
#[command(about = "Clipboard Sync: Discovery & Mesh Network", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Crea una nuova identità e un nuovo Ring
    New,
    /// Unisciti a un Ring esistente inserendo le parole
    Join,
    /// Avvia la modalità ASCOLTO (Scanner BLE)
    Start,
    /// [TEST] Avvia la modalità TRASMISSIONE (Advertising BLE)
    Broadcast,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        // --- 1. CONFIGURAZIONE ---
        Commands::New => {
            // create_new genera, salva su file e stampa a video
            let identity = RingIdentity::create_new()?;
            println!("✅ Configurazione salvata.");
            println!("Ring ID (Magic Bytes): {:x?}", identity.get_ble_magic_bytes());
            Ok(())
        }
        Commands::Join => {
            print!("Inserisci le parole del ring: ");
            io::stdout().flush()?;
            let mut phrase = String::new();
            io::stdin().read_line(&mut phrase)?;
            
            let phrase = phrase.trim();
            
            // Verifica e salva
            match RingIdentity::from_mnemonic(phrase) {
                Ok(identity) => {
                    identity.save()?; 
                    println!("✅ Identità verificata e salvata su disco.");
                    println!("Ring ID Hash: {:x?}", identity.get_ble_magic_bytes());
                    println!("Ora puoi usare 'start' (ascolto) o 'broadcast' (trasmissione)");
                },
                Err(e) => println!("\n❌ Errore nelle parole: {}", e),
            }
            Ok(())
        }

        // --- 2. RUNTIME ---
        Commands::Start => {
            println!("📂 Caricamento identità...");
            match RingIdentity::load() {
                Ok(identity) => {
                    println!("👤 Identità caricata: {:x?}", identity.get_ble_magic_bytes());
                    println!("📡 Avvio SCANNER (Ricezione)...");
                    // Chiama la logica di scanning in ble.rs
                    ble::run_ble_stack(identity).await?;
                },
                Err(e) => {
                    eprintln!("❌ Errore: {}", e);
                    eprintln!("   (Esegui prima 'rust-clip new' o 'rust-clip join')");
                }
            }
            Ok(())
        }

        // --- 3. TEST TRASMISSIONE ---
        Commands::Broadcast => {
            println!("📂 Caricamento identità...");
            // Se non trova il file, ne crea una temporanea per il test veloce
            let identity = match RingIdentity::load() {
                Ok(id) => id,
                Err(_) => {
                    println!("⚠️ Nessuna identità salvata, ne creo una temporanea per il test.");
                    RingIdentity::create_new()?
                }
            };

            println!("👤 Identità attiva: {:x?}", identity.get_ble_magic_bytes());
            println!("📢 Avvio BROADCASTER (Trasmissione)...");
            
            // Chiama la logica della nuova libreria in broadcast.rs
            broadcast::start_broadcasting(identity).await?;
            Ok(())
        }
    }
}