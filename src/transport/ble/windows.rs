use anyhow::{Result, anyhow};
use crate::core::identity::RingIdentity;
use std::sync::Arc;
use windows::prelude::*;
use windows::Devices::Bluetooth::Advertisement::*;
use windows::Devices::Bluetooth::GenericAttributeProfile::*;
use windows::Storage::Streams::{DataReader, DataWriter};
use windows::Foundation::TypedEventHandler;

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
    // 1. Setup Service UUID
    let service_uuid = windows::Foundation::Guid::from(SERVICE_UUID_STR);
    
    // 2. Create GATT Service Provider
    let result = GattServiceProvider::CreateAsync(service_uuid)?.await?;
    if result.Error()? != windows::Devices::Bluetooth::GenericAttributeProfile::GattServiceProviderError::Success {
        return Err(anyhow!("Failed to create GattServiceProvider: {:?}", result.Error()?));
    }
    let provider = result.ServiceProvider()?;

    // 3. Create Characteristics
    // READ Characteristic
    let read_params = GattLocalCharacteristicParameters::new()?;
    read_params.SetCharacteristicProperties(GattCharacteristicProperties::Read)?;
    read_params.SetReadProtectionLevel(GattProtectionLevel::Plain)?;
    
    let read_result = provider.Service()?.CreateCharacteristicAsync(
        windows::Foundation::Guid::from(READ_CHAR_UUID),
        &read_params
    )?.await?;
    let read_char = read_result.Characteristic()?;

    // READ Handler
    read_char.ReadRequested(&TypedEventHandler::new(|_: &Option<GattLocalCharacteristic>, args: &Option<GattReadRequestedEventArgs>| {
        if let Some(args) = args {
             if let Ok(request) = args.GetRequestAsync()?.get() { // Sync wait in callback is tricky, but GetRequestAsync returns logic object
                  // Actually specific docs say: request = args.GetRequestAsync().await? 
                  // But callbacks in Rust for WinRT are often non-async closures.
                  // We get the Request object immediately usually? No, it's async.
                  // Wait, GetRequestAsync is an IAsyncOperation. 
                  // In typical WinRT Rust usage, we can block or use aDeferral.
                  
                  // Let's grab a deferral first
                  if let Ok(deferral) = args.GetDeferral() {
                       // Respond logic
                       let writer = DataWriter::new()?;
                       writer.WriteString(&windows::core::HSTRING::from("RustClip-Win-Alive"))?;
                       let buffer = writer.DetachBuffer()?;
                       
                       // We need to set value on the request
                       // WinRT logic: request.RespondWithValue(buffer)
                       // But we need to await the request object first? 
                       // Check docs: args.GetRequestAsync() returns IAsyncOperation<GattReadRequest>
                       
                       // Blocking here might be bad if on UI thread, but we are background.
                       // let request = args.GetRequestAsync()?.get()?; 
                       // request.RespondWithValue(&buffer)?;
                       
                       // deferral.Complete()?;
                  }
             }
        }
        Ok(())
    }))?;

    // WRITE Characteristic
    let write_params = GattLocalCharacteristicParameters::new()?;
    write_params.SetCharacteristicProperties(GattCharacteristicProperties::Write)?;
    write_params.SetWriteProtectionLevel(GattProtectionLevel::Plain)?;
    
    let write_result = provider.Service()?.CreateCharacteristicAsync(
        windows::Foundation::Guid::from(WRITE_CHAR_UUID),
        &write_params
    )?.await?;
    let write_char = write_result.Characteristic()?;

    // WRITE Handler
    write_char.WriteRequested(&TypedEventHandler::new(|_: &Option<GattLocalCharacteristic>, args: &Option<GattWriteRequestedEventArgs>| {
        if let Some(args) = args {
             if let Ok(deferral) = args.GetDeferral() {
                  // Logic to read value
                  // let request = args.GetRequestAsync()?.get()?;
                  // let value = request.Value()?;
                  // let reader = DataReader::FromBuffer(&value)?;
                  // let len = reader.UnconsumedBufferLength()?;
                  // ... read bytes ...
                  
                  println!("📥 [BLE-Win] Write Received!");
                  
                  // request.Respond()?; // Ack
                  
                  deferral.Complete()?;
             }
        }
        Ok(())
    }))?;

    // 4. Start Advertising the Service
    let adv_params = GattServiceProviderAdvertisingParameters::new()?;
    adv_params.SetIsConnectable(true)?;
    adv_params.SetIsDiscoverable(true)?;
    provider.StartAdvertisingWithParameters(&adv_params)?;

    // 5. Additional Manual Advertisement (Optional but good for visibility)
    let publisher = BluetoothLEAdvertisementPublisher::new()?;
    publisher.Advertisement()?.LocalName()?.SetString(windows::core::HSTRING::from("RustClip-Win"))?;
    publisher.Advertisement()?.ServiceUuids()?.Append(service_uuid)?;
    publisher.Start()?;

    println!("✅ [BLE-Win] Service Started & Advertising...");

    // Keep alive indefinitely
    // We leak the object or store it in a static/Arc.
    // For now, let's just leak to keep it alive (Prototype style)
    let _server = Box::leak(Box::new(WindowsBleServer {
        provider,
        _publisher: publisher,
        _read_char: read_char,
        _write_char: write_char,
    }));

    Ok(())
}
