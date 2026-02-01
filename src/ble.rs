use crate::identity::RingIdentity;
use anyhow::Result;
use btleplug::api::{Central, Manager as _, Peripheral, ScanFilter};
// Importiamo anche Peripheral (trait) per l'advertising, anche se l'API è su Adapter in btleplug 0.11
use btleplug::api::bleuuid::uuid_from_u16; 
use btleplug::platform::Manager;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time;

// ID "Fake" per test (0xFFFF è riservato).
const TEST_MANUFACTURER_ID: u16 = 0xFFFF;

pub async fn run_ble_stack(identity: RingIdentity) -> Result<()> {
    // Cloniamo l'identità per i due task
    let id_scan = identity.clone();
    let id_adv = identity.clone();

    // Lanciamo Scanner e Advertiser in parallelo
    println!("🔄 Avvio stack BLE Dual-Mode (Scan + Advertise)...");
    
    // Usiamo tokio::select! o join! per farli girare insieme
    let _ = tokio::join!(
        start_scanner(id_scan),
        start_advertising(id_adv)
    );

    Ok(())
}

async fn start_advertising(identity: RingIdentity) -> Result<()> {
    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    if adapters.is_empty() {
        eprintln!("⚠️  Nessun adattatore Bluetooth per Advertising.");
        return Ok(());
    }
    let adapter = adapters.into_iter().nth(0).unwrap();

    let magic_bytes = identity.get_ble_magic_bytes();
    
    // Costruiamo il pacchetto dati
    let mut manufacturer_data = HashMap::new();
    manufacturer_data.insert(TEST_MANUFACTURER_ID, magic_bytes.to_vec());

    // Configurazione dell'annuncio
    // Nota: Su Windows/Mac il nome locale potrebbe essere sovrascritto dal sistema
    let params = btleplug::api::LeAdvertisement {
        local_name: Some("RustClip Node".to_string()),
        manufacturer_data,
        services: vec![], // Potremmo aggiungere un UUID specifico qui in futuro
        service_data: HashMap::new(),
    };

    println!("📢 Tentativo avvio Advertising...");
    println!("   Payload (Magic Bytes): {:x?}", magic_bytes);

    // Proviamo ad avviare l'advertising
    // Questo è il punto critico: su alcuni OS potrebbe fallire se non hanno permessi
    match adapter.start_le_advertising(params).await {
        Ok(_) => println!("✅ Advertising ATTIVO! Sto trasmettendo la mia presenza."),
        Err(e) => {
            eprintln!("⚠️  Errore avvio Advertising: {}", e);
            eprintln!("   (Nota: Su Windows assicurati di avere il Bluetooth acceso e Developer Mode se necessario)");
            eprintln!("   (Nota: Su macOS l'app deve avere i permessi Bluetooth)");
        }
    }

    // Manteniamo il task vivo per sempre
    loop {
        time::sleep(Duration::from_secs(60)).await;
    }
}

async fn start_scanner(identity: RingIdentity) -> Result<()> {
    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    if adapters.is_empty() { return Ok(()); }
    let adapter = adapters.into_iter().nth(0).unwrap();

    // Avvio scansione
    if let Err(e) = adapter.start_scan(ScanFilter::default()).await {
        eprintln!("⚠️  Errore avvio Scanner: {}", e);
        return Ok(());
    }

    let my_magic_bytes = identity.get_ble_magic_bytes();
    println!("📡 Scanner attivo. Cerco Magic Bytes: {:x?}", my_magic_bytes);

    loop {
        let peripherals = adapter.peripherals().await.unwrap_or_default();
        
        for peripheral in peripherals {
            let properties = peripheral.properties().await.unwrap_or(None);
            
            if let Some(props) = properties {
                // Controlliamo i Manufacturer Data
                if let Some(data) = props.manufacturer_data.get(&TEST_MANUFACTURER_ID) {
                    
                    // Verifichiamo se i byte corrispondono
                    if data.starts_with(&my_magic_bytes) {
                        let name = props.local_name.unwrap_or("Device Sconosciuto".to_string());
                        let rssi = props.rssi.unwrap_or(0);
                        
                        // Usiamo un colore o un formato evidente
                        println!("\n✨ 🔗 TROVATO MEMBRO DEL RING! 🔗 ✨");
                        println!("   Device: {}", name);
                        println!("   RSSI: {} dBm", rssi);
                        println!("   Dati: {:x?}", data);
                        println!("--------------------------------------");
                    }
                }
            }
        }
        time::sleep(Duration::from_millis(1000)).await;
    }
}