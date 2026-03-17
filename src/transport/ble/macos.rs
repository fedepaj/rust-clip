use anyhow::Result;
use crate::transport::{TransportEvent, TransportType};
use flume::Sender;
use std::sync::OnceLock;

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

use std::cell::RefCell;

const SERVICE_UUID_STR: &str = "99999999-0000-0000-0000-000000000001";
const READ_CHAR_UUID: &str = "99999999-0000-0000-0000-000000000002";
const WRITE_CHAR_UUID: &str = "99999999-0000-0000-0000-000000000003";

// Channels for communication between BLE RunLoop and Swarm
// EVENT_TX: BLE → Swarm (TransportEvents: received data, link established)
// SEND_RX: Swarm → BLE (raw bytes to send to connected peer)
static EVENT_TX: OnceLock<Sender<TransportEvent>> = OnceLock::new();
static SEND_RX: OnceLock<flume::Receiver<Vec<u8>>> = OnceLock::new();

thread_local! {
    static CONNECTED_PERIPHERAL: RefCell<Option<Retained<CBPeripheral>>> = RefCell::new(None);
    static WRITE_CHARACTERISTIC: RefCell<Option<Retained<CBCharacteristic>>> = RefCell::new(None);
    // Track the peer's identity UUID (read from IDENTITY characteristic)
    static PEER_ID: RefCell<Option<String>> = RefCell::new(None);
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "BleDelegate"]
    struct BleDelegate;

    // --- TIMER TICK (Outbound Loop: drain send queue) ---
    impl BleDelegate {
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &NSTimer) {
            if let Some(rx) = SEND_RX.get() {
                while let Ok(data) = rx.try_recv() {
                    self.send_raw_bytes(&data);
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

                        println!("  [BLE-Mac] Server received {} bytes", len);

                        // Forward raw bytes as TransportEvent::PacketReceived
                        if let Some(tx) = EVENT_TX.get() {
                            let _ = tx.send(TransportEvent::PacketReceived {
                                data: slice.to_vec(),
                                from_transport: TransportType::Ble,
                                from_addr: None,
                            });
                        }
                    }
                }
                if requests.count() > 0 {
                    manager.respondToRequest_withResult(&requests.objectAtIndex(0), CBATTError::Success);
                }
            }
        }
    }

    // --- CENTRAL MANAGER (Client: scan + connect) ---
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
            unsafe {
                // Don't use device name — not modifiable on macOS
                println!("  [BLE-Mac] Peer discovered. Connecting...");
                central.stopScan();
                central.connectPeripheral_options(peripheral, None);

                CONNECTED_PERIPHERAL.with(|p| {
                    *p.borrow_mut() = Some(peripheral.retain());
                });
            }
        }

        #[unsafe(method(centralManager:didConnectPeripheral:))]
        fn central_manager_did_connect_peripheral(&self, _central: &CBCentralManager, peripheral: &CBPeripheral) {
            unsafe {
                println!("  [BLE-Mac] Connected. Discovering services...");
                peripheral.setDelegate(Some(ProtocolObject::from_ref(self)));

                let uuid = CBUUID::UUIDWithString(&NSString::from_str(SERVICE_UUID_STR));
                let uuids = NSArray::from_slice(&[&*uuid]);
                peripheral.discoverServices(Some(&uuids));
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
                        println!("  [BLE-Mac] Service found. Discovering characteristics...");
                        peripheral.discoverCharacteristics_forService(None, &service);
                    }
                }
            }
        }

        #[unsafe(method(peripheral:didDiscoverCharacteristicsForService:error:))]
        fn peripheral_did_discover_chars(&self, _peripheral: &CBPeripheral, service: &CBService, error: Option<&NSError>) {
            if error.is_some() { return; }
            unsafe {
                if let Some(chars) = service.characteristics() {
                    for i in 0..chars.count() {
                        let ch = chars.objectAtIndex(i);
                        let uuid = ch.UUID().UUIDString();

                        if uuid.isEqualToString(&NSString::from_str(WRITE_CHAR_UUID)) {
                            println!("  [BLE-Mac] Write characteristic found. Link ready.");
                            WRITE_CHARACTERISTIC.with(|c| {
                                *c.borrow_mut() = Some(ch.retain());
                            });

                            // Signal LinkEstablished to Swarm
                            // We don't know the remote peer_id yet (will learn during handshake)
                            // Use a placeholder — the Swarm will initiate handshake
                            if let Some(tx) = EVENT_TX.get() {
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
    }

    unsafe impl NSObjectProtocol for BleDelegate {}
);

impl BleDelegate {
    pub fn init_delegate(
        mtm: MainThreadMarker,
        event_tx: Sender<TransportEvent>,
        send_rx: flume::Receiver<Vec<u8>>,
    ) -> Retained<Self> {
        let _ = EVENT_TX.set(event_tx);
        let _ = SEND_RX.set(send_rx);

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
            // Only advertise service UUID — do NOT use device name (not modifiable on macOS)
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

    fn send_raw_bytes(&self, bytes: &[u8]) {
        CONNECTED_PERIPHERAL.with(|p| {
            if let Some(peer) = p.borrow().as_ref() {
                WRITE_CHARACTERISTIC.with(|c| {
                    if let Some(ch) = c.borrow().as_ref() {
                        unsafe {
                            let data = objc2_foundation::NSData::with_bytes(bytes);
                            println!("  [BLE-Mac] Sending {} bytes", bytes.len());
                            peer.writeValue_forCharacteristic_type(
                                &data,
                                ch,
                                CBCharacteristicWriteType::WithResponse,
                            );
                        }
                    }
                });
            }
        });
    }
}

/// Run the macOS BLE RunLoop. Must be called from the main thread.
/// This function blocks forever (RunLoop).
pub fn run_ble_runloop(
    event_tx: Sender<TransportEvent>,
    send_rx: flume::Receiver<Vec<u8>>,
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
