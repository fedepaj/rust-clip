use anyhow::{Result, anyhow};
use crate::transport::{TransportEvent, TransportType};
use crate::transport::ble::fragmentation::{self, Reassembler};
use windows::core::{HSTRING, GUID};
use windows::Devices::Bluetooth::Advertisement::*;
use windows::Devices::Bluetooth::GenericAttributeProfile::*;
use windows::Storage::Streams::{DataWriter, DataReader};
use windows::Foundation::TypedEventHandler;
use flume::Sender;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU32, Ordering};
use windows::Devices::Bluetooth::*;

const SERVICE_UUID_STR: &str = "99999999-0000-0000-0000-000000000001";
const READ_CHAR_UUID: &str = "99999999-0000-0000-0000-000000000002";
const WRITE_CHAR_UUID: &str = "99999999-0000-0000-0000-000000000003";

/// Multi-peer BLE state
struct BleState {
    /// handle → (device, write_characteristic)
    peers: HashMap<String, (BluetoothLEDevice, GattCharacteristic)>,
    /// Known Bluetooth addresses (for dedup — don't reconnect to same device)
    known_addrs: HashSet<u64>,
    /// Bluetooth address → handle (for disconnect lookup)
    addr_to_handle: HashMap<u64, String>,
}

static NEXT_HANDLE_ID: AtomicU32 = AtomicU32::new(0);

fn next_handle() -> String {
    let n = NEXT_HANDLE_ID.fetch_add(1, Ordering::Relaxed);
    format!("ble-{}", n)
}

/// Write bytes to a specific GATT characteristic (with fragmentation).
async fn write_fragments(ch: &GattCharacteristic, data: &[u8]) {
    let packet_id = fragmentation::next_packet_id();
    let fragments = fragmentation::fragment(data, packet_id);

    if fragments.len() > 1 {
        println!("  [BLE-Win] Sending {} bytes in {} fragments", data.len(), fragments.len());
    }

    for frag in &fragments {
        if let Ok(writer) = DataWriter::new() {
            let _ = writer.WriteBytes(frag);
            let op = if let Ok(buffer) = writer.DetachBuffer() {
                ch.WriteValueWithOptionAsync(&buffer, GattWriteOption::WriteWithResponse).ok()
            } else {
                None
            };
            if let Some(op) = op {
                if let Ok(status) = op.await {
                    if status != GattCommunicationStatus::Success {
                        println!("  [BLE-Win] Fragment send failed: {:?}", status);
                    }
                }
            }
        }
    }
}

/// Start the Windows BLE service (server + client).
/// Must be called from an async context (tokio runtime).
pub async fn start_ble_service(
    event_tx: Sender<TransportEvent>,
    send_rx: flume::Receiver<(String, Vec<u8>)>,
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

    // WRITE handler: receive bytes, reassemble fragments, forward as TransportEvent
    let tx_write = event_tx.clone();
    let rt_handle = tokio::runtime::Handle::current();
    let reassembler = Arc::new(Mutex::new(Reassembler::new()));

    let reassembler_write = reassembler.clone();
    write_char.WriteRequested(&TypedEventHandler::new(
        move |_: &Option<GattLocalCharacteristic>, args: &Option<GattWriteRequestedEventArgs>| {
            if let Some(args) = args {
                if let Ok(deferral) = args.GetDeferral() {
                    let args_clone = args.clone();
                    let tx = tx_write.clone();
                    let reassembler = reassembler_write.clone();
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

                                if let Some(fragment_bytes) = bytes_opt {
                                    let complete = {
                                        let mut r = reassembler.lock().unwrap();
                                        r.feed(&fragment_bytes)
                                    };

                                    if let Some(data) = complete {
                                        println!("  [BLE-Win] Server received {} bytes", data.len());
                                        let _ = tx.send(TransportEvent::PacketReceived {
                                            data,
                                            from_transport: TransportType::Ble,
                                            from_handle: "ble".to_string(),
                                        });
                                    }
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

    // --- CLIENT: Scanning + Connecting to multiple peers ---
    let state = Arc::new(Mutex::new(BleState {
        peers: HashMap::new(),
        known_addrs: HashSet::new(),
        addr_to_handle: HashMap::new(),
    }));

    let state_watcher = state.clone();
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

                        // Dedup by Bluetooth address
                        {
                            let lock = state.lock().unwrap();
                            if lock.known_addrs.contains(&addr) {
                                return Ok(());
                            }
                        }

                        // Mark as known immediately to prevent duplicate connections
                        {
                            let mut lock = state.lock().unwrap();
                            lock.known_addrs.insert(addr);
                        }

                        rt_handle.spawn(async move {
                            println!("  [BLE-Win] Peer found! Connecting...");
                            match BluetoothLEDevice::FromBluetoothAddressAsync(addr) {
                                Ok(op) => {
                                    if let Ok(device) = op.await {
                                        // Register disconnect handler
                                        let state_dc = state.clone();
                                        let tx_dc = tx.clone();
                                        let _ = device.ConnectionStatusChanged(&TypedEventHandler::new(
                                            move |device: &Option<BluetoothLEDevice>, _| {
                                                if let Some(device) = device {
                                                    if let Ok(status) = device.ConnectionStatus() {
                                                        if status == BluetoothConnectionStatus::Disconnected {
                                                            if let Ok(dev_addr) = device.BluetoothAddress() {
                                                                let mut lock = state_dc.lock().unwrap();
                                                                lock.known_addrs.remove(&dev_addr);
                                                                if let Some(handle) = lock.addr_to_handle.remove(&dev_addr) {
                                                                    lock.peers.remove(&handle);
                                                                    println!("  [BLE-Win] Peer {} disconnected.", handle);
                                                                    let _ = tx_dc.send(TransportEvent::PeerLost {
                                                                        peer_id: handle,
                                                                        transport: TransportType::Ble,
                                                                    });
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                Ok(())
                                            },
                                        ));

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
                                                                    let handle = next_handle();
                                                                    println!("  [BLE-Win] Write char found → {}. Link ready.", handle);

                                                                    {
                                                                        let mut lock = state.lock().unwrap();
                                                                        lock.peers.insert(handle.clone(), (device, ch));
                                                                        lock.addr_to_handle.insert(addr, handle.clone());
                                                                    }

                                                                    let _ = tx.send(TransportEvent::PeerDiscovered {
                                                                        peer_id: handle.clone(),
                                                                        transport: TransportType::Ble,
                                                                        handle,
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
                                Err(e) => {
                                    // Remove from known on connection failure
                                    state.lock().unwrap().known_addrs.remove(&addr);
                                    println!("  [BLE-Win] FromAddr error: {:?}", e);
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

    // --- SEND LOOP: Swarm → BLE (tagged channel with per-peer routing) ---
    let state_sender = state.clone();
    tokio::spawn(async move {
        println!("  [BLE-Win] Send loop started.");
        while let Ok((handle, data)) = send_rx.recv_async().await {
            if handle == "*" {
                // Broadcast to all connected peers
                let chars: Vec<GattCharacteristic> = {
                    let lock = state_sender.lock().unwrap();
                    lock.peers.values().map(|(_, ch)| ch.clone()).collect()
                };
                for ch in &chars {
                    write_fragments(ch, &data).await;
                }
            } else {
                // Send to specific peer
                let char_opt = {
                    let lock = state_sender.lock().unwrap();
                    lock.peers.get(&handle).map(|(_, ch)| ch.clone())
                };

                if let Some(ch) = char_opt {
                    write_fragments(&ch, &data).await;
                } else {
                    // Handle not found — broadcast as fallback
                    let chars: Vec<GattCharacteristic> = {
                        let lock = state_sender.lock().unwrap();
                        lock.peers.values().map(|(_, ch)| ch.clone()).collect()
                    };
                    for ch in &chars {
                        write_fragments(ch, &data).await;
                    }
                }
            }
        }
    });

    // Keep resources alive
    Box::leak(Box::new(provider));
    Box::leak(Box::new(read_char));
    Box::leak(Box::new(write_char));
    Box::leak(Box::new(watcher));
    Box::leak(Box::new(state));

    Ok(())
}
