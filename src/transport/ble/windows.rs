use anyhow::{Result, anyhow};
use crate::core::identity::RingIdentity;
use windows::core::{HSTRING, GUID};
use windows::Devices::Bluetooth::Advertisement::*;
use windows::Devices::Bluetooth::GenericAttributeProfile::*;
use windows::Storage::Streams::DataWriter;
use windows::Foundation::TypedEventHandler;
// use windows::Foundation::Collections::IVector; // Unused if Append works inherently

// UUIDs costanti (che poi diverranno dinamici)
const SERVICE_UUID_STR: &str = "99999999-0000-0000-0000-000000000001";
const READ_CHAR_UUID:   &str = "99999999-0000-0000-0000-000000000002";
const WRITE_CHAR_UUID:  &str = "99999999-0000-0000-0000-000000000003";

struct WindowsBleServer {
    provider: GattServiceProvider,
    _publisher: BluetoothLEAdvertisementPublisher, // Keep alive
    _read_char: GattLocalCharacteristic,
    _write_char: GattLocalCharacteristic,
}

pub async fn start_ble_service(_identity: RingIdentity) -> Result<()> {
    println!("🔍 [BLE-Win] Step 1: Setup UUIDs");
    // 1. Setup Service UUID
    let service_uuid = GUID::from(SERVICE_UUID_STR);
    
    println!("🔍 [BLE-Win] Step 2: CreateAsync GattServiceProvider");
    // 2. Create GATT Service Provider
    let result = GattServiceProvider::CreateAsync(service_uuid)?.await?;
    // Workaround: GattServiceProviderError type not found in namespace for some reason.
    // Checking raw value: Success = 0
    if result.Error()?.0 != 0 {
        return Err(anyhow!("Failed to create GattServiceProvider: {:?}", result.Error()?));
    }
    let provider = result.ServiceProvider()?;

    // 3. Create Characteristics
    // READ Characteristic
    println!("🔍 [BLE-Win] Step 3: Create Read Param");
    let read_params = GattLocalCharacteristicParameters::new()?;
    read_params.SetCharacteristicProperties(GattCharacteristicProperties::Read)?;
    read_params.SetReadProtectionLevel(GattProtectionLevel::Plain)?;
    
    println!("🔍 [BLE-Win] Step 4: Create Read Char");
    let read_result = provider.Service()?.CreateCharacteristicAsync(
        GUID::from(READ_CHAR_UUID),
        &read_params
    )?.await?;
    let read_char = read_result.Characteristic()?;

    // READ Handler
    read_char.ReadRequested(&TypedEventHandler::new(|_: &Option<GattLocalCharacteristic>, args: &Option<GattReadRequestedEventArgs>| {
        if let Some(args) = args {
             if let Ok(deferral) = args.GetDeferral() {
                  // Respond logic
                  if let Ok(writer) = DataWriter::new() {
                       let _ = writer.WriteString(&HSTRING::from("RustClip-Win-Alive"));
                       if let Ok(buffer) = writer.DetachBuffer() {
                            // TODO: Use Request object Properly
                            // For now just ack
                       }
                  }
                  let _ = deferral.Complete();
             }
        }
        Ok(())
    }))?;

    // WRITE Characteristic
    println!("🔍 [BLE-Win] Step 5: Create Write Param");
    let write_params = GattLocalCharacteristicParameters::new()?;
    write_params.SetCharacteristicProperties(GattCharacteristicProperties::Write)?;
    write_params.SetWriteProtectionLevel(GattProtectionLevel::Plain)?;
    
    println!("🔍 [BLE-Win] Step 6: Create Write Char");
    let write_result = provider.Service()?.CreateCharacteristicAsync(
        GUID::from(WRITE_CHAR_UUID),
        &write_params
    )?.await?;
    let write_char = write_result.Characteristic()?;

    // WRITE Handler
    write_char.WriteRequested(&TypedEventHandler::new(|_: &Option<GattLocalCharacteristic>, args: &Option<GattWriteRequestedEventArgs>| {
        if let Some(args) = args {
             if let Ok(deferral) = args.GetDeferral() {
                  println!("📥 [BLE-Win] Write Received!");
                  let _ = deferral.Complete();
             }
        }
        Ok(())
    }))?;

    // 4. Start Advertising the Service
    println!("🔍 [BLE-Win] Step 7: Start Advertising Service");
    let adv_params = GattServiceProviderAdvertisingParameters::new()?;
    adv_params.SetIsConnectable(true)?;
    adv_params.SetIsDiscoverable(true)?;
    provider.StartAdvertisingWithParameters(&adv_params)?;

    // 5. Additional Manual Advertisement
    println!("🔍 [BLE-Win] Step 8: Additional Advertisement");
    let publisher = BluetoothLEAdvertisementPublisher::new()?;
    publisher.Advertisement()?.SetLocalName(&HSTRING::from("RustClip-Win"))?;
    
    // FIX: ServiceUuids() returns IVector<Guid>.
    // The previous error "no method named ServiceUuids" was likely due to missing feature or confusion.
    // In windows 0.52+, it should be ServiceUuids().
    // If it still fails, it might be that ServiceUuids property is read-only but returns a collection we can append to?
    // Yes, getting the property returns the collection.
    publisher.Advertisement()?.ServiceUuids()?.Append(service_uuid)?;
    
    println!("🔍 [BLE-Win] Step 9: Publisher Start");
    publisher.Start()?;

    println!("✅ [BLE-Win] Service Started & Advertising...");

    // Keep alive indefinitely
    let _server = Box::leak(Box::new(WindowsBleServer {
        provider,
        _publisher: publisher,
        _read_char: read_char,
        _write_char: write_char,
    }));

    Ok(())
}
