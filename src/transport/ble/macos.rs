use anyhow::Result;
use crate::core::identity::RingIdentity;
use crate::core::identity::RingIdentity;
use crate::core::packet::{WirePacket, PacketType, HandshakeMsg};
use std::thread;
use flume::{Sender, Receiver};
use std::sync::{OnceLock, Mutex};

use objc2::runtime::{AnyObject, ProtocolObject, Sel}; 
use objc2::{define_class, msg_send, rc::Retained, MainThreadOnly, sel, Message};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSDictionary, NSError, NSObject, NSObjectProtocol, NSRunLoop,
    NSString, NSTimer, NSDate,
};
use objc2_core_bluetooth::{
    CBAdvertisementDataLocalNameKey, CBAdvertisementDataServiceUUIDsKey, CBPeripheralManager,
    CBPeripheralManagerDelegate, CBManagerState, CBUUID, CBMutableService, CBMutableCharacteristic,
    CBCharacteristicProperties, CBAttributePermissions, CBCharacteristic, CBATTRequest, CBATTError,
    CBCentralManager, CBCentralManagerDelegate, CBPeripheral, CBPeripheralDelegate, CBService,
    CBCharacteristicWriteType,
};

const SERVICE_UUID_STR: &str = "99999999-0000-0000-0000-000000000001";
const READ_CHAR_UUID:  &str = "99999999-0000-0000-0000-000000000002"; 
const WRITE_CHAR_UUID: &str = "99999999-0000-0000-0000-000000000003"; 
const LOCAL_NAME:      &str = "MacBook-Rust-Test";

use std::cell::RefCell;

static CLIENT_TX: OnceLock<Sender<WirePacket>> = OnceLock::new();
static CLIENT_RX: OnceLock<Receiver<WirePacket>> = OnceLock::new();
static IDENTITY: OnceLock<RingIdentity> = OnceLock::new();

thread_local! {
    // Simplified: Keep track of ONE connected peripheral for testing
    static CONNECTED_PERIPHERAL: RefCell<Option<Retained<CBPeripheral>>> = RefCell::new(None);
    static WRITE_CHARACTERISTIC: RefCell<Option<Retained<CBCharacteristic>>> = RefCell::new(None);
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "BleDelegate"]
    struct BleDelegate;

    // --- TIMER TICK (Outbound Loop) ---
    impl BleDelegate {
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &NSTimer) {
            // Poll RX channel
            // Note: We use try_recv inside loop to drain.
            // Access thread_local? No, send_packet does that.
            if let Some(rx) = CLIENT_RX.get() {
                 while let Ok(packet) = rx.try_recv() {
                     self.send_packet(packet);
                 }
            }
        }
    }

    // --- PERIPHERAL MANAGER (Server) ---
    unsafe impl CBPeripheralManagerDelegate for BleDelegate {
        #[unsafe(method(peripheralManagerDidUpdateState:))]
        fn peripheral_manager_did_update_state(&self, manager: &CBPeripheralManager) {
            let state = unsafe { manager.state() };
            if state == CBManagerState::PoweredOn {
                println!("🔵 [Rust-Server] Bluetooth ON. Resetting Services...");
                unsafe { manager.removeAllServices(); }
                self.setup_gatt_service(manager, MainThreadMarker::from(self));
            }
        }

        #[unsafe(method(peripheralManager:didAddService:error:))]
        fn peripheral_manager_did_add_service(&self, manager: &CBPeripheralManager, _service: &CBMutableService, error: Option<&NSError>) {
            if let Some(err) = error {
                println!("❌ [Rust-Server] Add Service Error: {}", err.localizedDescription());
            } else {
                println!("✅ [Rust-Server] Service Registered. Starting Advertising...");
                self.start_advertising_helper(manager);
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
                        
                        println!("📥 [Rust-Server] Recv {} bytes", len);
                        
                        if let Ok(packet) = bincode::deserialize::<WirePacket>(slice) {
                             if let Some(tx) = CLIENT_TX.get() {
                                 let _ = tx.send(packet);
                             }
                        } else {
                             println!("⚠️ [Rust-Server] Parse Failed: {:?}", slice);
                        }
                    }
                }
                if requests.count() > 0 {
                    manager.respondToRequest_withResult(&requests.objectAtIndex(0), CBATTError::Success);
                }
            }
        }
    }

    // --- CENTRAL MANAGER (Client) ---
    unsafe impl CBCentralManagerDelegate for BleDelegate {
        #[unsafe(method(centralManagerDidUpdateState:))]
        fn central_manager_did_update_state(&self, central: &CBCentralManager) {
            let state = unsafe { central.state() };
            if state == CBManagerState::PoweredOn {
                println!("🔵 [Rust-Client] Bluetooth ON. Scanning...");
                self.start_scanning(central);
            }
        }

        #[unsafe(method(centralManager:didDiscoverPeripheral:advertisementData:RSSI:))]
        fn central_manager_did_discover_peripheral(&self, central: &CBCentralManager, peripheral: &CBPeripheral, _adv_data: &NSDictionary<NSString, AnyObject>, _rssi: &AnyObject) {
             // Logic: Check UUIDs? We scan only for Service UUID so any result is valid.
             // Connect!
             unsafe {
                 let name = peripheral.name().map(|n| n.to_string()).unwrap_or("Unknown".to_string());
                 println!("🔎 [Rust-Client] Discovered: {}. Connecting...", name);
                 
                 // Stop scan to save battery/noise
                 central.stopScan();
                 
                 // Connect
                 central.connectPeripheral_options(peripheral, None);
                 
                 // Keep it alive
                 CONNECTED_PERIPHERAL.with(|p| {
                     *p.borrow_mut() = Some(peripheral.retain());
                 });
             }
        }

        #[unsafe(method(centralManager:didConnectPeripheral:))]
        fn central_manager_did_connect_peripheral(&self, _central: &CBCentralManager, peripheral: &CBPeripheral) {
            unsafe {
                println!("✅ [Rust-Client] Connected to {}. Discovering Services...", peripheral.name().map(|n| n.to_string()).unwrap_or("Unknown".to_string()));
                peripheral.setDelegate(Some(ProtocolObject::from_ref(self)));
                
                // Discover target service
                let uuid = CBUUID::UUIDWithString(&NSString::from_str(SERVICE_UUID_STR));
                let uuids = NSArray::from_slice(&[&*uuid]);
                peripheral.discoverServices(Some(&uuids));
            }
        }
    }

    // --- PERIPHERAL DELEGATE (Client - Service Discovery) ---
    unsafe impl CBPeripheralDelegate for BleDelegate {
        #[unsafe(method(peripheral:didDiscoverServices:))]
        fn peripheral_did_discover_services(&self, peripheral: &CBPeripheral, error: Option<&NSError>) {
             if let Some(_) = error { return; }
             unsafe {
                 if let Some(services) = peripheral.services() {
                     for i in 0..services.count() {
                         let service = services.objectAtIndex(i);
                         println!("✅ [Rust-Client] Service Found. Discovering Chars...");
                         peripheral.discoverCharacteristics_forService(None, &service);
                     }
                 }
             }
        }

        #[unsafe(method(peripheral:didDiscoverCharacteristicsForService:error:))]
        fn peripheral_did_discover_chars(&self, peripheral: &CBPeripheral, service: &CBService, error: Option<&NSError>) {
             if let Some(_) = error { return; }
             unsafe {
                 if let Some(chars) = service.characteristics() {
                     for i in 0..chars.count() {
                         let ch = chars.objectAtIndex(i);
                         let uuid = ch.UUID().UUIDString();
                         println!("✅ [Rust-Client] Char Found: {}", uuid);
                         
                             if uuid.isEqualToString(&NSString::from_str(WRITE_CHAR_UUID)) {
                                 println!("🎯 [Rust-Client] WRITE Characteristic Found! Saved.");
                                 WRITE_CHARACTERISTIC.with(|c| {
                                     *c.borrow_mut() = Some(ch.retain());
                                 });
                                 println!("🎯 [Rust-Client] WRITE Characteristic Found! Sending Hello...");
                                 self.send_hello();
                             }
                     }
                 }
             }
        }
    }

    // impl BleDelegate (Moved to top)

    unsafe impl NSObjectProtocol for BleDelegate {}
);

impl BleDelegate {
    pub fn new(mtm: MainThreadMarker, identity: RingIdentity, tx_packet: Sender<WirePacket>, rx_packet: Receiver<WirePacket>) -> Retained<Self> {
        let _ = CLIENT_TX.set(tx_packet);
        let _ = CLIENT_RX.set(rx_packet);
        let _ = IDENTITY.set(identity);
        // Thread locals init lazily
        
        let this = mtm.alloc(); // Wait, mtm.alloc() is creating AnyObject?
        // BleDelegate::alloc() is better but let's cast or use ClassType
        // If ClassType is not imported, use explicit cast:
        let this: Retained<BleDelegate> = unsafe { msg_send![this, init] };
        
        // Schedule Timer
        unsafe {
            let selector = sel!(tick:);
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                0.1, // 100ms
                &this,
                selector,
                None,
                true
            );
        }
        
        this
    }

    fn setup_gatt_service(&self, manager: &CBPeripheralManager, mtm: MainThreadMarker) {
        unsafe {
            // ... (Same GATT Setup as before) ...
            // Simplified for brevity in this chunk
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

    fn start_advertising_helper(&self, manager: &CBPeripheralManager) {
        unsafe {
            let val_name = NSString::from_str(LOCAL_NAME);
            let uuid_obj = CBUUID::UUIDWithString(&NSString::from_str(SERVICE_UUID_STR));
            let val_uuids = NSArray::from_slice(&[&*uuid_obj]);
            let keys: [&NSString; 2] = [CBAdvertisementDataLocalNameKey, CBAdvertisementDataServiceUUIDsKey];
            let objects: [&NSObject; 2] = [&*val_name, &*val_uuids];
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

    fn send_packet(&self, packet: WirePacket) {
        // Serialize
        if let Ok(bytes) = bincode::serialize(&packet) {
            // Require Connected Peer AND Write Char
            CONNECTED_PERIPHERAL.with(|p| {
                if let Some(peer) = p.borrow().as_ref() {
                    WRITE_CHARACTERISTIC.with(|c| {
                        if let Some(char) = c.borrow().as_ref() {
                            unsafe {
                                let data = objc2_foundation::NSData::with_bytes(&bytes);
                                println!("📤 [Rust-Client] Sending {} bytes...", bytes.len());
                                peer.writeValue_forCharacteristic_type(
                                    &data,
                                    char,
                                    CBCharacteristicWriteType::WithResponse
                                );
                            }
                        }
                    });
                } else {
                     println!("⚠️ [Rust-Client] Drop packet. No Peer connected.");
                }
            });
        }
    }
}

pub fn run_ble_runloop(identity: RingIdentity, tx_packet: Sender<WirePacket>, rx_packet: Receiver<WirePacket>) -> Result<()> {
    let mtm = MainThreadMarker::new().expect("Must run on Main Thread for macOS BLE");
    unsafe {
        println!("🚀 [Rust-Mac] Initializing BLE (Client + Server)...");
        let delegate = BleDelegate::new(mtm, identity, tx_packet, rx_packet);
        
        // Explicitly cast for Server
        let server_delegate = ProtocolObject::<dyn CBPeripheralManagerDelegate>::from_ref(&*delegate);
        
        let _server = CBPeripheralManager::initWithDelegate_queue(
            mtm.alloc(),
            Some(server_delegate),
            None, 
        );
        
        // Explicitly cast for Client
        let client_delegate = ProtocolObject::<dyn CBCentralManagerDelegate>::from_ref(&*delegate);
        
        let _client = CBCentralManager::initWithDelegate_queue(
            mtm.alloc(),
            Some(client_delegate),
            None,
        );

        println!("👀 [Rust-Mac] RunLoop Starting.");
        NSRunLoop::currentRunLoop().run();
    }
    Ok(())
}

pub async fn start_ble_service(_identity: RingIdentity) -> Result<()> {
   Ok(())
}
