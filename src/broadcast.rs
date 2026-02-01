use crate::identity::RingIdentity;
// IMPORT CRUCIALE: PeripheralImpl serve per chiamare .new()
use ble_peripheral_rust::{Peripheral, PeripheralImpl};
use ble_peripheral_rust::gatt::peripheral_event::PeripheralEvent;
use uuid::Uuid;
use tokio::sync::mpsc::channel;
use std::time::Duration;

pub async fn start_broadcasting(identity: RingIdentity) -> anyhow::Result<()> {
    println!("📢 Preparazione Broadcasting...");

    // 1. Canale per gli eventi
    let (sender_tx, mut receiver_rx) = channel::<PeripheralEvent>(256);

    // 2. Inizializzazione Periferica
    // Ora .new() funziona perché abbiamo importato PeripheralImpl
    let mut peripheral = Peripheral::new(sender_tx).await
        .map_err(|e| anyhow::anyhow!("Errore init peripheral: {:?}", e))?;

    println!("⏳ Attendo che il Bluetooth sia pronto...");
    while !peripheral.is_powered().await.unwrap_or(false) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!("✅ Bluetooth ACCESO!");

    // 3. Configurazione Nome e UUID
    let magic = identity.get_ble_magic_bytes();
    // Nome: RustClip-a1b2c3d4
    let local_name = format!("RustClip-{:02x}{:02x}{:02x}{:02x}", magic[0], magic[1], magic[2], magic[3]);
    
    // Usiamo un UUID standard (generico) per il servizio
    let service_uuid = Uuid::from_u128(0x1234_u128); 

    println!("🚀 AVVIO ADVERTISING!");
    println!("   Nome: {}", local_name);

    // 4. Start Advertising
    peripheral.start_advertising(&local_name, &[service_uuid]).await
        .map_err(|e| anyhow::anyhow!("Errore start advertising: {:?}", e))?;

    println!("📡 Sto trasmettendo... controlla l'altro device!");

    // 5. Manteniamo vivo il processo
    while let Some(_event) = receiver_rx.recv().await {
        // Qui ignoriamo gli eventi per ora, serve solo a tenere il loop attivo
    }

    Ok(())
}