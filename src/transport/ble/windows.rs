use anyhow::{Result, anyhow};
use crate::core::identity::RingIdentity;
use windows::core::{HSTRING, GUID};
use windows::Devices::Bluetooth::Advertisement::*;
use windows::Devices::Bluetooth::GenericAttributeProfile::*;
use windows::Storage::Streams::{DataWriter, DataReader};
use windows::Foundation::TypedEventHandler;
// use windows::Foundation::Collections::IVector; // Unused if Append works inherently
use crate::core::packet::{WirePacket, PacketType};
use flume::{Sender, Receiver};
use std::sync::{Arc, Mutex};
use windows::Devices::Bluetooth::*;

// For client state
struct ClientState {
    device: Option<BluetoothLEDevice>,
    write_char: Option<GattCharacteristic>,
}

// UUIDs costanti (che poi diverranno dinamici)
const SERVICE_UUID_STR: &str = "99999999-0000-0000-0000-000000000001";
const READ_CHAR_UUID:   &str = "99999999-0000-0000-0000-000000000002";
const WRITE_CHAR_UUID:  &str = "99999999-0000-0000-0000-000000000003";

struct WindowsBleServer {
    provider: GattServiceProvider,
    _publisher: BluetoothLEAdvertisementPublisher, // Keep alive
    _read_char: GattLocalCharacteristic,
    _write_char: GattLocalCharacteristic,
    _sender: Sender<WirePacket>, // Keep channel alive if needed, though closure captures it
}

pub async fn start_ble_service(_identity: RingIdentity, tx_packet: Sender<WirePacket>, rx_packet: Receiver<WirePacket>) -> Result<()> {
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
                       if let Ok(_buffer) = writer.DetachBuffer() {
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
    let tx_clone = tx_packet.clone();
    let rt_handle = tokio::runtime::Handle::current();

    write_char.WriteRequested(&TypedEventHandler::new(move |_: &Option<GattLocalCharacteristic>, args: &Option<GattWriteRequestedEventArgs>| {
        if let Some(args) = args {
             if let Ok(deferral) = args.GetDeferral() {
                  let args_clone = args.clone();
                  let tx_clone = tx_clone.clone();
                  
                  // Spawn async task because GetRequestAsync returns IAsyncOperation
                  rt_handle.spawn(async move {
                       if let Ok(op) = args_clone.GetRequestAsync() {
                           if let Ok(req) = op.await {
                               // Read Value - Need to scope !Send types tightly
                               let packet_opt = if let Ok(_buffer) = req.Value() {
                                   if let Ok(reader) = DataReader::FromBuffer(&_buffer) {
                                       let len = _buffer.Length().unwrap_or(0) as usize;
                                       let mut bytes = vec![0u8; len];
                                       if reader.ReadBytes(&mut bytes).is_ok() {
                                            Some(bytes)
                                       } else { None }
                                   } else { None }
                               } else { None };

                               if let Some(bytes) = packet_opt {
                                    println!("📥 [BLE-Win] Payload received: {} bytes", bytes.len());
                                    // Deserialize & Send (Send safe now)
                                    if let Ok(packet) = bincode::deserialize::<WirePacket>(&bytes) {
                                        let _ = tx_clone.send_async(packet).await;
                                    } else {
                                        println!("⚠️ [BLE-Win] Packet Parse Failed");
                                    }
                               }
                           }
                       }
                       // Complete Deferral
                       let _ = deferral.Complete();
                  });
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
    // println!("🔍 [BLE-Win] Step 8: Additional Advertisement");
    // let publisher = BluetoothLEAdvertisementPublisher::new()?;
    // publisher.Advertisement()?.SetLocalName(&HSTRING::from("RustClip-Win"))?;
    // publisher.Advertisement()?.ServiceUuids()?.Append(service_uuid)?;
    
    // println!("🔍 [BLE-Win] Step 9: Publisher Start");
    // publisher.Start()?;

    println!("✅ [BLE-Win] Service Started & Advertising...");

    // --- CLIENT: Scanning & Sending ---
    println!("🔍 [BLE-Win-Client] Starting Watcher...");
    
    let client_state = Arc::new(Mutex::new(ClientState { device: None, write_char: None }));
    let state_in_watcher = client_state.clone();
    
    let watcher = BluetoothLEAdvertisementWatcher::new()?;
    watcher.SetScanningMode(BluetoothLEScanningMode::Active)?;
    
    // Filter by Service UUID? Or check in handler?
    // Handler is sync, so we spawn logic.
    let rt_handle = tokio::runtime::Handle::current();
    let identity_watcher = _identity.clone();
    let tx_connect = tx_packet.clone(); // Clone specifically for Watcher
    watcher.Received(&TypedEventHandler::new(move |_watcher, args: &Option<BluetoothLEAdvertisementReceivedEventArgs>| {
        if let Some(args) = args {
             // Check UUIDs
             if let Ok(uuids) = args.Advertisement()?.ServiceUuids() {
                 let target = GUID::from(SERVICE_UUID_STR);
                 let mut found = false;
                 // Iterate IVector
                 for i in 0..uuids.Size()? {
                     if uuids.GetAt(i)? == target { found = true; break; }
                 }
                 
                 if found {
                      // Found peer!
                      let addr = args.BluetoothAddress()?;
                      let state = state_in_watcher.clone();
                      let id = identity_watcher.clone();
                      
                      // Spawn connect task
                      let tx_inner = tx_connect.clone(); // Clone INSIDE closure for async block
                      rt_handle.spawn(async move {
                          // Check if already connected to avoid spam
                          let already_connected = {
                               state.lock().unwrap().device.is_some()
                          };
                          
                          if !already_connected {
                               println!("🔎 [BLE-Win-Client] Peer Found! Connecting...");
                               match BluetoothLEDevice::FromBluetoothAddressAsync(addr) {
                                   Ok(op) => match op.await {
                                       Ok(device) => {
                                           println!("✅ [BLE-Win-Client] Connected. Getting Services...");
                                           // Get Service
                                           match device.GetGattServicesForUuidAsync(GUID::from(SERVICE_UUID_STR)) {
                                               Ok(op) => match op.await {
                                                  Ok(services) => {
                                                      let service_to_use = {
                                                          // CRITICAL: Inspect IVectorView synchronously and extract needed item
                                                          // to avoid holding non-Send types across await.
                                                          if let Ok(list) = services.Services() {
                                                              if list.Size().unwrap_or(0) > 0 {
                                                                  Some(list.GetAt(0).expect("Service Index"))
                                                              } else { None }
                                                          } else { None }
                                                      };

                                                      if let Some(service) = service_to_use {
                                                          // Get Char
                                                          match service.GetCharacteristicsForUuidAsync(GUID::from(WRITE_CHAR_UUID)) {
                                                              Ok(chars_op) => {
                                                                 if let Ok(chars_res) = chars_op.await {
                                                                     let char_to_use = {
                                                                         if let Ok(char_list) = chars_res.Characteristics() {
                                                                             if char_list.Size().unwrap_or(0) > 0 {
                                                                                 Some(char_list.GetAt(0).unwrap())
                                                                             } else { None }
                                                                         } else { None }
                                                                     };
                                                                     
                                                                     if let Some(ch) = char_to_use {
                                                                         println!("🎯 [BLE-Win-Client] Write Char Found!");
                                                                          // Signal LinkUp instead of Hello
                                                                          // This packet goes to Backend (tx_packet)
                                                                          println!("🔗 [BLE-Win-Client] Signaling LinkUp to Backend...");
                                                                          
                                                                          let packet_opt = {
                                                                              let mut lock = state.lock().unwrap();
                                                                              lock.device = Some(device);
                                                                              lock.write_char = Some(ch.clone());
                                                                              
                                                                              // Build Packet inside lock but return it out
                                                                               WirePacket::new_plain(
                                                                                  id.get_rotating_id(),
                                                                                  "broadcast".to_string(), // receiver_id
                                                                                  PacketType::LinkUp,
                                                                                  &[], // Empty Payload
                                                                                  &id.identity_key
                                                                              ).ok()
                                                                          }; // Lock dropped here

                                                                          if let Some(packet) = packet_opt {
                                                                               // Send LOCALLY to Backend via tx_inner
                                                                               let _ = tx_inner.send_async(packet).await;
                                                                          }
                                                                     }
                                                                 }
                                                              }
                                                              Err(e) => println!("❌ GetChars Error: {:?}", e),
                                                          }
                                                      }
                                                  },
                                                  Err(e) => println!("❌ Service Error: {:?}", e),
                                               },
                                               Err(e) => println!("❌ GetServices Error: {:?}", e),
                                           }
                                       },
                                       Err(e) => println!("❌ Connection Failed: {:?}", e),
                                   },
                                   Err(e) => println!("❌ FromAddr Error: {:?}", e),
                               }
                          }
                      });
                 }
             }
        }
        Ok(())
    }))?;
    
    watcher.Start()?;
    
    // --- CLIENT: Sending Loop ---
    let state_sender = client_state.clone();
    tokio::spawn(async move {
        println!("👂 [BLE-Win-Client] Output Loop Started");
        while let Ok(packet) = rx_packet.recv_async().await {
             let mut char_opt = None;
             {
                 let lock = state_sender.lock().unwrap();
                 if let Some(ch) = &lock.write_char {
                     char_opt = Some(ch.clone()); 
                     // Need valid clone logic for WinRT objects? 
                     // Yes, they are ref-counted pointers usually. check Clone trait.
                     // windows crate objects implement Clone.
                 }
             }
             
             if let Some(ch) = char_opt {
                 if let Ok(bytes) = bincode::serialize(&packet) {
                      println!("📤 [BLE-Win] Sending {} bytes...", bytes.len());
                            if let Ok(writer) = DataWriter::new() {
                                 let _ = writer.WriteBytes(&bytes);
                                 
                                 // Scoping buffer tightly to avoid holding !Send type across await
                                 let op = if let Ok(buffer) = writer.DetachBuffer() {
                                      ch.WriteValueWithOptionAsync(&buffer, GattWriteOption::WriteWithResponse).ok()
                                 } else { None };

                                 if let Some(op) = op {
                                     if let Ok(status) = op.await {
                                         if status == GattCommunicationStatus::Success {
                                             println!("✅ [BLE-Win] SENT OK ({} bytes)", bytes.len());
                                         } else {
                                             println!("❌ [BLE-Win] SEND FAILED: {:?}", status);
                                         }
                                     }
                                 }
                            }
                 }
             } else {
                 println!("⚠️ [BLE-Win] Packet Dropped. No Peer.");
             }
        }
    });

    // Keep alive indefinitely
    let _server = Box::leak(Box::new(WindowsBleServer {
        provider,
        _publisher: BluetoothLEAdvertisementPublisher::new()?, // Start not called, just empty to satisfy struct
        _read_char: read_char,
        _write_char: write_char,
        _sender: tx_packet,
    }));
    
    // Keep watcher alive?
    // Box leak it too? Or just let it run?
    // It will drop at end of function if not kept.
    // _server keeps provider.
    // We need to keep watcher.
    Box::leak(Box::new(watcher)); 
    Box::leak(Box::new(client_state)); // Leak state to keep it valid for tasks

    Ok(())
}
