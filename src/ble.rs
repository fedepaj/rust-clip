use crate::identity::RingIdentity;
use anyhow::Result;
// IMPORTANTE: 'Peripheral as _' importa il trait per poter chiamare .properties()
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::Manager;
use tokio::time;
use std::time::Duration;

// ID "Fake" per test
const TEST_MANUFACTURER_ID: u16 = 0xFFFF;

pub async fn run_ble_stack(identity: RingIdentity) -> Result<()> {
    println!("📡 Avvio Scanner (Cerco membri del Ring)...");

    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    if adapters.is_empty() { 
        println!("❌ Nessun adattatore Bluetooth trovato.");
        return Ok(()); 
    }
    let adapter = adapters.into_iter().nth(0).unwrap();

    // Avvio scansione
    if let Err(e) = adapter.start_scan(ScanFilter::default()).await {
        eprintln!("⚠️  Errore avvio Scanner: {}", e);
        return Ok(());
    }

    let my_magic_bytes = identity.get_ble_magic_bytes();
    let magic_hex = hex::encode(my_magic_bytes); // es "a1b2c3d4"

    println!("👀 In ascolto per Ring ID: {:x?}", my_magic_bytes);

    loop {
        let peripherals = adapter.peripherals().await.unwrap_or_default();
        
        for peripheral in peripherals {
            // Ora .properties() funziona grazie all'import corretto
            let properties = peripheral.properties().await.unwrap_or(None);
            
            if let Some(props) = properties {
                let name = props.local_name.unwrap_or_default();
                
                // METODO 1: Controllo tramite NOME (Usato da ble-peripheral-rust)
                // Se il nome contiene i nostri magic bytes (es "RustClip-a1b2c3d4")
                if !name.is_empty() && name.contains(&magic_hex) {
                        println!("\n✨ 🔗 TROVATO MEMBRO DEL RING (Via Nome)! 🔗 ✨");
                        println!("   Device: {}", name);
                        println!("   RSSI: {} dBm", props.rssi.unwrap_or(0));
                        println!("--------------------------------------");
                }

                // METODO 2: Controllo tramite Manufacturer Data (Legacy)
                if let Some(data) = props.manufacturer_data.get(&TEST_MANUFACTURER_ID) {
                    if data.starts_with(&my_magic_bytes) {
                        println!("\n✨ 🔗 TROVATO MEMBRO DEL RING (Via Dati)! 🔗 ✨");
                        println!("   Device: {}", name);
                        println!("   RSSI: {} dBm", props.rssi.unwrap_or(0));
                    }
                }
            }
        }
        time::sleep(Duration::from_millis(1000)).await;
    }
}