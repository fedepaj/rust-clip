mod identity;
mod ble; // <--- Importante: includiamo il modulo ble appena creato

use clap::{Parser, Subcommand};
use identity::RingIdentity;
use std::io::{self, Write};

#[derive(Parser)]
#[command(name = "rust-clip")]
#[command(about = "Clipboard Sync con Rust", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Crea una nuova identità
    New,
    /// Unisciti a un ring esistente
    Join,
    /// Avvia il demone di ascolto (Radar Bluetooth)
    Start,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::New => {
            let identity = RingIdentity::create_new()?;
            println!("Ring ID Hash (Magic Bytes): {:x?}", identity.get_ble_magic_bytes());
            Ok(())
        }
        Commands::Join => {
            print!("Inserisci le parole del ring: ");
            io::stdout().flush()?;
            let mut phrase = String::new();
            io::stdin().read_line(&mut phrase)?;
            
            let phrase = phrase.trim();
            
            match RingIdentity::from_mnemonic(phrase) {
                Ok(identity) => {
                    println!("\n✅ Successo! Identità verificata.");
                    println!("Ring ID Hash: {:x?}", identity.get_ble_magic_bytes());
                },
                Err(e) => println!("\n❌ Errore: {}", e),
            }
            Ok(())
        }
        Commands::Start => {
            println!("⚠️  Modalità DEMO: Avvio con identità temporanea per testare il Bluetooth.");
            // Per ora generiamo un'identità al volo solo per far partire il radar
            // Nella versione finale qui caricheremo quella salvata su file
            let temp_identity = RingIdentity::create_new()?;
            
            // Passiamo il controllo al modulo BLE
            ble::start_radar(temp_identity).await?;
            Ok(())
        }
    }
}