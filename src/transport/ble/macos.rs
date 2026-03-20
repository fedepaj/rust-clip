use anyhow::Result;
use crate::transport::{TransportEvent, TransportType};
use crate::transport::ble::fragmentation::{self, Reassembler};
use flume::Sender;
use std::collections::HashMap;
use std::sync::{OnceLock, Mutex};
use std::cell::{RefCell, Cell};

use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, rc::Retained, MainThreadOnly, sel, Message};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSDictionary, NSError, NSObject, NSObjectProtocol, NSRunLoop,
    NSString, NSTimer,
};
use objc2_core_bluetooth::{
    CBAdvertisementDataServiceUUIDsKey, CBPeripheralManager,
    CBPeripheralManagerDelegate, CBManagerState, CBUUID, CBMutableService, CBMutableCharacteristic,
    CBCharacteristicProperties, CBAttributePermissions, CBCharacteristic, CBATTRequest, CBATTError,
    CBCentralManager, CBCentralManagerDelegate, CBPeripheral, CBPeripheralDelegate, CBService,
    CBCharacteristicWriteType,
};

const SERVICE_UUID_STR: &str = "99999999-0000-0000-0000-000000000001";
const READ_CHAR_UUID: &str = "99999999-0000-0000-0000-000000000002";
const WRITE_CHAR_UUID: &str = "99999999-0000-0000-0000-000000000003";

// Channels for communication between BLE RunLoop and Swarm
// EVENT_TX: BLE → Swarm (TransportEvents)
// SEND_RX: Swarm → BLE (tagged: handle + data)
static EVENT_TX: OnceLock<Sender<TransportEvent>> = OnceLock::new();
static SEND_RX: OnceLock<flume::Receiver<(String, Vec<u8>)>> = OnceLock::new();
static REASSEMBLER: OnceLock<Mutex<Reassembler>> = OnceLock::new();

thread_local! {
    /// Connected peers with write characteristic ready.
    /// handle ("ble-0", "ble-1", ...) → (peripheral, write_characteristic)
    static BLE_PEERS: RefCell<HashMap<String, (Retained<CBPeripheral>, Retained<CBCharacteristic>)>> = RefCell::new(HashMap::new());

    /// All known peripheral pointers → handle.
    /// Empty string means "connecting, write char not yet found".
    static PERIPHERAL_MAP: RefCell<HashMap<usize, String>> = RefCell::new(HashMap::new());

    /// Counter for generating unique handles.
    static NEXT_HANDLE_ID: Cell<u32> = Cell::new(0);
}

/// Get a stable key for a CBPeripheral reference (raw pointer as usize).
fn peripheral_key(peripheral: &CBPeripheral) -> usize {
    peripheral as *const CBPeripheral as usize
}

/// Write bytes to a peripheral's characteristic.
fn write_to_peripheral(peripheral: &CBPeripheral, write_char: &CBCharacteristic, bytes: &[u8]) {
    unsafe {
        let data = objc2_foundation::NSData::with_bytes(bytes);
        peripheral.writeValue_forCharacteristic_type(
            &data,
            write_char,
            CBCharacteristicWriteType::WithResponse,
        );
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "BleDelegate"]
    struct BleDelegate;

    // --- TIMER TICK (Outbound Loop: drain tagged send queue) ---
    impl BleDelegate {
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &NSTimer) {
            if let Some(rx) = SEND_RX.get() {
                while let Ok((handle, data)) = rx.try_recv() {
                    let packet_id = fragmentation::next_packet_id();
                    let fragments = fragmentation::fragment(&data, packet_id);
                    if fragments.len() > 1 {
                        println!("  [BLE-Mac] Sending {} bytes in {} fragments (→ {})", data.len(), fragments.len(), handle);
                    }
                    if handle == "*" {
                        for frag in &fragments {
                            self.send_to_all(frag);
                        }
                    } else {
                        for frag in &fragments {
                            self.send_to_handle(&handle, frag);
                        }
                    }
                }
            }
        }
    }

    // --- PERIPHERAL MANAGER (Server: advertise + receive writes) ---
    unsafe impl CBPeripheralManagerDelegate for BleDelegate {
        #[unsafe(method(peripheralManagerDidUpdateState:))]
        fn peripheral_manager_did_update_state(&self, manager: &CBPeripheralManager) {
            let state = unsafe { manager.state() };
            if state == CBManagerState::PoweredOn {
                println!("  [BLE-Mac] Bluetooth ON. Setting up GATT...");
                unsafe { manager.removeAllServices(); }
                self.setup_gatt_service(manager, MainThreadMarker::from(self));
            }
        }

        #[unsafe(method(peripheralManager:didAddService:error:))]
        fn peripheral_manager_did_add_service(&self, manager: &CBPeripheralManager, _service: &CBMutableService, error: Option<&NSError>) {
            if let Some(err) = error {
                println!("  [BLE-Mac] Add Service Error: {}", err.localizedDescription());
            } else {
                println!("  [BLE-Mac] Service registered. Advertising...");
                self.start_advertising(manager);
            }
        }

        #[unsafe(method(peripheralManager:didReceiveWriteRequests:))]
        fn peripheral_manager_did_receive_write_requests(&self, manager: &CBPeripheralManager, requests: &NSArray<CBATTRequest>) {
            unsafe {
                for i in 0..requests.count() {
                    let request = requests.objectAtIndex(i);
                    if let Some(data) = request.value() {
                        let ptr: *const std::os::raw::c_void = msg_send![&*data, bytes];
                        let len: usize = msg_send![&*data, length];
                        let slice = std::slice::from_raw_parts(ptr.cast::<u8>(), len);

                        // Feed fragment to reassembler
                        let reassembler = REASSEMBLER.get_or_init(|| Mutex::new(Reassembler::new()));
                        let complete = {
                            let mut r = reassembler.lock().unwrap();
                            r.feed(slice)
                        };

                        if let Some(data) = complete {
                            println!("  [BLE-Mac] Server received {} bytes", data.len());
                            if let Some(tx) = EVENT_TX.get() {
                                let _ = tx.send(TransportEvent::PacketReceived {
                                    data,
                                    from_transport: TransportType::Ble,
                                    from_handle: "ble".to_string(),
                                });
                            }
                        }
                    }
                }
                if requests.count() > 0 {
                    manager.respondToRequest_withResult(&requests.objectAtIndex(0), CBATTError::Success);
                }
            }
        }
    }

    // --- CENTRAL MANAGER (Client: scan + connect to multiple peers) ---
    unsafe impl CBCentralManagerDelegate for BleDelegate {
        #[unsafe(method(centralManagerDidUpdateState:))]
        fn central_manager_did_update_state(&self, central: &CBCentralManager) {
            let state = unsafe { central.state() };
            if state == CBManagerState::PoweredOn {
                println!("  [BLE-Mac] Scanning for peers...");
                self.start_scanning(central);
            }
        }

        #[unsafe(method(centralManager:didDiscoverPeripheral:advertisementData:RSSI:))]
        fn central_manager_did_discover_peripheral(&self, central: &CBCentralManager, peripheral: &CBPeripheral, _adv_data: &NSDictionary<NSString, AnyObject>, _rssi: &AnyObject) {
            let key = peripheral_key(peripheral);

            // Dedup: skip if we already know this peripheral
            let already_known = PERIPHERAL_MAP.with(|m| m.borrow().contains_key(&key));
            if already_known {
                return;
            }

            // Mark as connecting (empty handle = not ready yet)
            PERIPHERAL_MAP.with(|m| {
                m.borrow_mut().insert(key, String::new());
            });

            unsafe {
                println!("  [BLE-Mac] Peer discovered. Connecting...");
                // Do NOT stop scanning — keep discovering more peers
                central.connectPeripheral_options(peripheral, None);
            }
        }

        #[unsafe(method(centralManager:didConnectPeripheral:))]
        fn central_manager_did_connect_peripheral(&self, _central: &CBCentralManager, peripheral: &CBPeripheral) {
            unsafe {
                println!("  [BLE-Mac] Connected to peer. Discovering services...");
                peripheral.setDelegate(Some(ProtocolObject::from_ref(self)));

                let uuid = CBUUID::UUIDWithString(&NSString::from_str(SERVICE_UUID_STR));
                let uuids = NSArray::from_slice(&[&*uuid]);
                peripheral.discoverServices(Some(&uuids));
            }
        }

        #[unsafe(method(centralManager:didFailToConnectPeripheral:error:))]
        fn central_manager_did_fail_to_connect(&self, _central: &CBCentralManager, peripheral: &CBPeripheral, error: Option<&NSError>) {
            let key = peripheral_key(peripheral);
            PERIPHERAL_MAP.with(|m| { m.borrow_mut().remove(&key); });
            if let Some(err) = error {
                println!("  [BLE-Mac] Connection failed: {}", err.localizedDescription());
            }
        }

        #[unsafe(method(centralManager:didDisconnectPeripheral:error:))]
        fn central_manager_did_disconnect_peripheral(&self, central: &CBCentralManager, peripheral: &CBPeripheral, _error: Option<&NSError>) {
            let key = peripheral_key(peripheral);
            let handle = PERIPHERAL_MAP.with(|m| m.borrow_mut().remove(&key));

            if let Some(handle) = handle {
                if !handle.is_empty() {
                    BLE_PEERS.with(|p| { p.borrow_mut().remove(&handle); });
                    println!("  [BLE-Mac] Peer {} disconnected.", handle);

                    if let Some(tx) = EVENT_TX.get() {
                        let _ = tx.send(TransportEvent::PeerLost {
                            peer_id: handle,
                            transport: TransportType::Ble,
                        });
                    }
                }
            }

            // Restart scanning to rediscover disconnected peripherals
            unsafe {
                central.stopScan();
                self.start_scanning(central);
            }
        }
    }

    // --- PERIPHERAL DELEGATE (Client: service/characteristic discovery) ---
    unsafe impl CBPeripheralDelegate for BleDelegate {
        #[unsafe(method(peripheral:didDiscoverServices:))]
        fn peripheral_did_discover_services(&self, peripheral: &CBPeripheral, error: Option<&NSError>) {
            if error.is_some() { return; }
            unsafe {
                if let Some(services) = peripheral.services() {
                    for i in 0..services.count() {
                        let service = services.objectAtIndex(i);
                        peripheral.discoverCharacteristics_forService(None, &service);
                    }
                }
            }
        }

        #[unsafe(method(peripheral:didDiscoverCharacteristicsForService:error:))]
        fn peripheral_did_discover_chars(&self, peripheral: &CBPeripheral, service: &CBService, error: Option<&NSError>) {
            if error.is_some() { return; }
            unsafe {
                if let Some(chars) = service.characteristics() {
                    for i in 0..chars.count() {
                        let ch = chars.objectAtIndex(i);
                        let uuid = ch.UUID().UUIDString();

                        if uuid.isEqualToString(&NSString::from_str(WRITE_CHAR_UUID)) {
                            // Assign a unique handle
                            let handle = NEXT_HANDLE_ID.with(|id| {
                                let n = id.get();
                                id.set(n + 1);
                                format!("ble-{}", n)
                            });

                            let key = peripheral_key(peripheral);

                            // Update peripheral map with the assigned handle
                            PERIPHERAL_MAP.with(|m| {
                                m.borrow_mut().insert(key, handle.clone());
                            });

                            // Store in connected peers
                            BLE_PEERS.with(|p| {
                                p.borrow_mut().insert(handle.clone(), (peripheral.retain(), ch.retain()));
                            });

                            println!("  [BLE-Mac] Write char found → {}. Link ready.", handle);

                            // Signal peer discovered to Swarm
                            if let Some(tx) = EVENT_TX.get() {
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
    }

    unsafe impl NSObjectProtocol for BleDelegate {}
);

impl BleDelegate {
    pub fn init_delegate(
        mtm: MainThreadMarker,
        event_tx: Sender<TransportEvent>,
        send_rx: flume::Receiver<(String, Vec<u8>)>,
    ) -> Retained<Self> {
        let _ = EVENT_TX.set(event_tx);
        let _ = SEND_RX.set(send_rx);
        let _ = REASSEMBLER.set(Mutex::new(Reassembler::new()));

        let this: Retained<BleDelegate> = unsafe { msg_send![mtm.alloc(), init] };

        // Timer for outbound polling (100ms)
        unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                0.1,
                &this,
                sel!(tick:),
                None,
                true,
            );
        }

        this
    }

    fn setup_gatt_service(&self, manager: &CBPeripheralManager, mtm: MainThreadMarker) {
        unsafe {
            let service_uuid = CBUUID::UUIDWithString(&NSString::from_str(SERVICE_UUID_STR));
            let read_char = CBMutableCharacteristic::initWithType_properties_value_permissions(
                mtm.alloc(),
                &CBUUID::UUIDWithString(&NSString::from_str(READ_CHAR_UUID)),
                CBCharacteristicProperties::Read,
                None,
                CBAttributePermissions::Readable,
            );
            let write_char = CBMutableCharacteristic::initWithType_properties_value_permissions(
                mtm.alloc(),
                &CBUUID::UUIDWithString(&NSString::from_str(WRITE_CHAR_UUID)),
                CBCharacteristicProperties::Write | CBCharacteristicProperties::WriteWithoutResponse,
                None,
                CBAttributePermissions::Writeable,
            );
            let service = CBMutableService::initWithType_primary(mtm.alloc(), &service_uuid, true);
            service.setCharacteristics(Some(&NSArray::from_retained_slice(&[
                Retained::cast_unchecked::<CBCharacteristic>(read_char),
                Retained::cast_unchecked::<CBCharacteristic>(write_char),
            ])));
            manager.addService(&service);
        }
    }

    fn start_advertising(&self, manager: &CBPeripheralManager) {
        unsafe {
            let uuid_obj = CBUUID::UUIDWithString(&NSString::from_str(SERVICE_UUID_STR));
            let val_uuids = NSArray::from_slice(&[&*uuid_obj]);
            let keys: [&NSString; 1] = [CBAdvertisementDataServiceUUIDsKey];
            let objects: [&NSObject; 1] = [&*val_uuids];
            let adv_data = NSDictionary::from_slices(&keys, &objects);
            let adv_data_any = &*(Retained::as_ptr(&adv_data) as *const NSDictionary<NSString, AnyObject>);
            manager.startAdvertising(Some(adv_data_any));
        }
    }

    fn start_scanning(&self, central: &CBCentralManager) {
        unsafe {
            let uuid = CBUUID::UUIDWithString(&NSString::from_str(SERVICE_UUID_STR));
            let uuids = NSArray::from_slice(&[&*uuid]);
            central.scanForPeripheralsWithServices_options(Some(&uuids), None);
        }
    }

    /// Send raw bytes to a specific peer by handle.
    /// Falls back to broadcast if handle not found (e.g., "ble" from server-side).
    fn send_to_handle(&self, handle: &str, bytes: &[u8]) {
        BLE_PEERS.with(|p| {
            let peers = p.borrow();
            if let Some((peripheral, write_char)) = peers.get(handle) {
                write_to_peripheral(peripheral, write_char, bytes);
            } else {
                // Handle not found — broadcast to all connected peers
                for (peripheral, write_char) in peers.values() {
                    write_to_peripheral(peripheral, write_char, bytes);
                }
            }
        });
    }

    /// Broadcast raw bytes to all connected peers.
    fn send_to_all(&self, bytes: &[u8]) {
        BLE_PEERS.with(|p| {
            for (peripheral, write_char) in p.borrow().values() {
                write_to_peripheral(peripheral, write_char, bytes);
            }
        });
    }
}

/// Run the macOS BLE RunLoop. Must be called from the main thread.
/// This function blocks forever (RunLoop).
pub fn run_ble_runloop(
    event_tx: Sender<TransportEvent>,
    send_rx: flume::Receiver<(String, Vec<u8>)>,
) -> Result<()> {
    let mtm = MainThreadMarker::new().expect("Must run on Main Thread for macOS BLE");
    unsafe {
        println!("  [BLE-Mac] Initializing (Client + Server)...");
        let delegate = BleDelegate::init_delegate(mtm, event_tx, send_rx);

        // Server (Peripheral Manager)
        let server_delegate = ProtocolObject::<dyn CBPeripheralManagerDelegate>::from_ref(&*delegate);
        let _server = CBPeripheralManager::initWithDelegate_queue(
            mtm.alloc(),
            Some(server_delegate),
            None,
        );

        // Client (Central Manager)
        let client_delegate = ProtocolObject::<dyn CBCentralManagerDelegate>::from_ref(&*delegate);
        let _client = CBCentralManager::initWithDelegate_queue(
            mtm.alloc(),
            Some(client_delegate),
            None,
        );

        println!("  [BLE-Mac] RunLoop starting.");
        NSRunLoop::currentRunLoop().run();
    }
    Ok(())
}
