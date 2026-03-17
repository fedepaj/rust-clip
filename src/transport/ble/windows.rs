use anyhow::{Result, anyhow};
use crate::transport::{TransportEvent, TransportType};
use windows::core::{HSTRING, GUID};
use windows::Devices::Bluetooth::Advertisement::*;
use windows::Devices::Bluetooth::GenericAttributeProfile::*;
use windows::Storage::Streams::{DataWriter, DataReader};
use windows::Foundation::TypedEventHandler;
use flume::Sender;
use std::sync::{Arc, Mutex};
use windows::Devices::Bluetooth::*;

const SERVICE_UUID_STR: &str = "99999999-0000-0000-0000-000000000001";
const READ_CHAR_UUID: &str = "99999999-0000-0000-0000-000000000002";
const WRITE_CHAR_UUID: &str = "99999999-0000-0000-0000-000000000003";

struct ClientState {
    device: Option<BluetoothLEDevice>,
    write_char: Option<GattCharacteristic>,
}

/// Start the Windows BLE service (server + client).
/// Must be called from an async context (tokio runtime).
pub async fn start_ble_service(
    event_tx: Sender<TransportEvent>,
    send_rx: flume::Receiver<Vec<u8>>,
) -> Result<()> {
    println!("  [BLE-Win] Starting...");

    // --- SERVER: GATT Service Provider ---
    let service_uuid = GUID::from(SERVICE_UUID_STR);

    let result = GattServiceProvider::CreateAsync(service_uuid)?.await?;
    if result.Error()?.0 != 0 {
        return Err(anyhow!("Failed to create GattServiceProvider"));
    }
    let provider = result.ServiceProvider()?;

    // READ Characteristic
    let read_params = GattLocalCharacteristicParameters::new()?;
    read_params.SetCharacteristicProperties(GattCharacteristicProperties::Read)?;
    read_params.SetReadProtectionLevel(GattProtectionLevel::Plain)?;

    let read_result = provider.Service()?.CreateCharacteristicAsync(
        GUID::from(READ_CHAR_UUID),
        &read_params,
    )?.await?;
    let read_char = read_result.Characteristic()?;

    // READ handler (simple alive response)
    read_char.ReadRequested(&TypedEventHandler::new(
        |_: &Option<GattLocalCharacteristic>, args: &Option<GattReadRequestedEventArgs>| {
            if let Some(args) = args {
                if let Ok(deferral) = args.GetDeferral() {
                    let _ = deferral.Complete();
                }
            }
            Ok(())
        },
    ))?;

    // WRITE Characteristic
    let write_params = GattLocalCharacteristicParameters::new()?;
    write_params.SetCharacteristicProperties(GattCharacteristicProperties::Write)?;
    write_params.SetWriteProtectionLevel(GattProtectionLevel::Plain)?;

    let write_result = provider.Service()?.CreateCharacteristicAsync(
        GUID::from(WRITE_CHAR_UUID),
        &write_params,
    )?.await?;
    let write_char = write_result.Characteristic()?;

    // WRITE handler: receive bytes and forward as TransportEvent
    let tx_write = event_tx.clone();
    let rt_handle = tokio::runtime::Handle::current();

    write_char.WriteRequested(&TypedEventHandler::new(
        move |_: &Option<GattLocalCharacteristic>, args: &Option<GattWriteRequestedEventArgs>| {
            if let Some(args) = args {
                if let Ok(deferral) = args.GetDeferral() {
                    let args_clone = args.clone();
                    let tx = tx_write.clone();
                    rt_handle.spawn(async move {
                        if let Ok(op) = args_clone.GetRequestAsync() {
                            if let Ok(req) = op.await {
                                let bytes_opt = if let Ok(buffer) = req.Value() {
                                    if let Ok(reader) = DataReader::FromBuffer(&buffer) {
                                        let len = buffer.Length().unwrap_or(0) as usize;
                                        let mut bytes = vec![0u8; len];
                                        if reader.ReadBytes(&mut bytes).is_ok() {
                                            Some(bytes)
                                        } else {
                                            None
                                        }
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                };

                                if let Some(bytes) = bytes_opt {
                                    println!("  [BLE-Win] Server received {} bytes", bytes.len());
                                    let _ = tx.send(TransportEvent::PacketReceived {
                                        data: bytes,
                                        from_transport: TransportType::Ble,
                                        from_addr: None,
                                    });
                                }
                            }
                        }
                        let _ = deferral.Complete();
                    });
                }
            }
            Ok(())
        },
    ))?;

    // Start advertising
    let adv_params = GattServiceProviderAdvertisingParameters::new()?;
    adv_params.SetIsConnectable(true)?;
    adv_params.SetIsDiscoverable(true)?;
    provider.StartAdvertisingWithParameters(&adv_params)?;

    println!("  [BLE-Win] Server advertising.");

    // --- CLIENT: Scanning + Connecting ---
    let client_state = Arc::new(Mutex::new(ClientState {
        device: None,
        write_char: None,
    }));

    let state_watcher = client_state.clone();
    let tx_connect = event_tx.clone();

    let watcher = BluetoothLEAdvertisementWatcher::new()?;
    watcher.SetScanningMode(BluetoothLEScanningMode::Active)?;

    let rt_handle = tokio::runtime::Handle::current();
    watcher.Received(&TypedEventHandler::new(
        move |_watcher, args: &Option<BluetoothLEAdvertisementReceivedEventArgs>| {
            if let Some(args) = args {
                if let Ok(uuids) = args.Advertisement()?.ServiceUuids() {
                    let target = GUID::from(SERVICE_UUID_STR);
                    let mut found = false;
                    for i in 0..uuids.Size()? {
                        if uuids.GetAt(i)? == target {
                            found = true;
                            break;
                        }
                    }

                    if found {
                        let addr = args.BluetoothAddress()?;
                        let state = state_watcher.clone();
                        let tx = tx_connect.clone();

                        rt_handle.spawn(async move {
                            let already_connected = state.lock().unwrap().device.is_some();

                            if !already_connected {
                                println!("  [BLE-Win] Peer found! Connecting...");
                                match BluetoothLEDevice::FromBluetoothAddressAsync(addr) {
                                    Ok(op) => {
                                        if let Ok(device) = op.await {
                                            match device.GetGattServicesForUuidAsync(GUID::from(SERVICE_UUID_STR)) {
                                                Ok(op) => {
                                                    if let Ok(services) = op.await {
                                                        let service_to_use = if let Ok(list) = services.Services() {
                                                            if list.Size().unwrap_or(0) > 0 {
                                                                Some(list.GetAt(0).expect("Service index"))
                                                            } else {
                                                                None
                                                            }
                                                        } else {
                                                            None
                                                        };

                                                        if let Some(service) = service_to_use {
                                                            if let Ok(chars_op) = service.GetCharacteristicsForUuidAsync(GUID::from(WRITE_CHAR_UUID)) {
                                                                if let Ok(chars_res) = chars_op.await {
                                                                    let char_to_use = if let Ok(char_list) = chars_res.Characteristics() {
                                                                        if char_list.Size().unwrap_or(0) > 0 {
                                                                            Some(char_list.GetAt(0).unwrap())
                                                                        } else {
                                                                            None
                                                                        }
                                                                    } else {
                                                                        None
                                                                    };

                                                                    if let Some(ch) = char_to_use {
                                                                        println!("  [BLE-Win] Write char found. Link ready.");
                                                                        {
                                                                            let mut lock = state.lock().unwrap();
                                                                            lock.device = Some(device);
                                                                            lock.write_char = Some(ch);
                                                                        }

                                                                        // Signal LinkEstablished
                                                                        let _ = tx.send(TransportEvent::LinkEstablished {
                                                                            peer_id: "unknown-ble-peer".to_string(),
                                                                            transport: TransportType::Ble,
                                                                        });
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                Err(e) => println!("  [BLE-Win] GetServices error: {:?}", e),
                                            }
                                        }
                                    }
                                    Err(e) => println!("  [BLE-Win] FromAddr error: {:?}", e),
                                }
                            }
                        });
                    }
                }
            }
            Ok(())
        },
    ))?;

    watcher.Start()?;
    println!("  [BLE-Win] Client scanning.");

    // --- SEND LOOP: Swarm → BLE ---
    let state_sender = client_state.clone();
    tokio::spawn(async move {
        println!("  [BLE-Win] Send loop started.");
        while let Ok(bytes) = send_rx.recv_async().await {
            let char_opt = {
                let lock = state_sender.lock().unwrap();
                lock.write_char.clone()
            };

            if let Some(ch) = char_opt {
                println!("  [BLE-Win] Sending {} bytes", bytes.len());
                if let Ok(writer) = DataWriter::new() {
                    let _ = writer.WriteBytes(&bytes);
                    let op = if let Ok(buffer) = writer.DetachBuffer() {
                        ch.WriteValueWithOptionAsync(&buffer, GattWriteOption::WriteWithResponse).ok()
                    } else {
                        None
                    };
                    if let Some(op) = op {
                        if let Ok(status) = op.await {
                            if status != GattCommunicationStatus::Success {
                                println!("  [BLE-Win] Send failed: {:?}", status);
                            }
                        }
                    }
                }
            } else {
                println!("  [BLE-Win] Packet dropped. No peer.");
            }
        }
    });

    // Keep resources alive
    Box::leak(Box::new(provider));
    Box::leak(Box::new(read_char));
    Box::leak(Box::new(write_char));
    Box::leak(Box::new(watcher));
    Box::leak(Box::new(client_state));

    Ok(())
}
