use anyhow::Result;
use crate::core::identity::RingIdentity;
use std::thread;

use objc2::runtime::{AnyObject, ProtocolObject}; 
use objc2::{define_class, msg_send, rc::Retained, MainThreadOnly};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSDictionary, NSError, NSObject, NSObjectProtocol, NSRunLoop,
    NSString,
    // NSData rimosso per eliminare il warning (usiamo msg_send!)
};
use objc2_core_bluetooth::{
    CBAdvertisementDataLocalNameKey, CBAdvertisementDataServiceUUIDsKey, CBPeripheralManager,
    CBPeripheralManagerDelegate, CBManagerState, CBUUID, CBMutableService, CBMutableCharacteristic,
    CBCharacteristicProperties, CBAttributePermissions, CBCharacteristic, CBATTRequest, CBATTError,
};

const SERVICE_UUID_STR: &str = "99999999-0000-0000-0000-000000000001";
const READ_CHAR_UUID:  &str = "99999999-0000-0000-0000-000000000002"; 
const WRITE_CHAR_UUID: &str = "99999999-0000-0000-0000-000000000003"; 
const LOCAL_NAME:      &str = "MacBook-Rust-Test";

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "BleDelegate"]
    struct BleDelegate;

    unsafe impl CBPeripheralManagerDelegate for BleDelegate {
        #[unsafe(method(peripheralManagerDidUpdateState:))]
        fn peripheral_manager_did_update_state(&self, manager: &CBPeripheralManager) {
            let state = unsafe { manager.state() };
            if state == CBManagerState::PoweredOn {
                println!("🔵 [Rust] Bluetooth ACCESO. Configuro Servizi GATT...");
                self.setup_gatt_service(manager, MainThreadMarker::from(self));
            }
        }

        #[unsafe(method(peripheralManager:didAddService:error:))]
        fn peripheral_manager_did_add_service(&self, manager: &CBPeripheralManager, _service: &CBMutableService, error: Option<&NSError>) {
            if let Some(err) = error {
                println!("❌ [Rust] Errore Service: {}", err.localizedDescription());
            } else {
                println!("✅ [Rust] Servizio registrato. Inizio Advertising...");
                self.start_advertising_helper(manager);
            }
        }

        #[unsafe(method(peripheralManager:didReceiveReadRequest:))]
        fn peripheral_manager_did_receive_read_request(&self, manager: &CBPeripheralManager, request: &CBATTRequest) {
            unsafe {
                println!("📖 [Rust] Lettura richiesta per UUID: {:?}", request.characteristic().UUID());
                let response_text = "Dato dal Mac: Clipboard Rust attiva!";
                
                if let Some(data) = NSString::from_str(response_text).dataUsingEncoding(4) {
                    let len: usize = msg_send![&*data, length];
                    if request.offset() > len {
                        manager.respondToRequest_withResult(request, CBATTError::InvalidOffset);
                        return;
                    }
                    request.setValue(Some(&data));
                    manager.respondToRequest_withResult(request, CBATTError::Success);
                }
            }
        }

        #[unsafe(method(peripheralManager:didReceiveWriteRequests:))]
        fn peripheral_manager_did_receive_write_requests(&self, manager: &CBPeripheralManager, requests: &NSArray<CBATTRequest>) {
            unsafe {
                for i in 0..requests.count() {
                    let request = requests.objectAtIndex(i);
                    if let Some(data) = request.value() {
                        // Accesso universale ai byte tramite msg_send
                        let ptr: *const std::os::raw::c_void = msg_send![&*data, bytes];
                        let len: usize = msg_send![&*data, length];
                        
                        let slice = std::slice::from_raw_parts(ptr.cast::<u8>(), len);
                        if let Ok(text) = std::str::from_utf8(slice) {
                            println!("� [CLIPBOARD RICEVUTA]: {}", text);
                        }
                    }
                }
                if requests.count() > 0 {
                    manager.respondToRequest_withResult(&requests.objectAtIndex(0), CBATTError::Success);
                }
            }
        }
    }

    unsafe impl NSObjectProtocol for BleDelegate {}
);

impl BleDelegate {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc();
        unsafe { msg_send![this, init] }
    }

    fn setup_gatt_service(&self, manager: &CBPeripheralManager, mtm: MainThreadMarker) {
        unsafe {
            // 1. Il nostro servizio principale
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
                CBCharacteristicProperties::Write,
                None,
                CBAttributePermissions::Writeable,
            );

            let service = CBMutableService::initWithType_primary(mtm.alloc(), &service_uuid, true);
            service.setCharacteristics(Some(&NSArray::from_retained_slice(&[
                Retained::cast_unchecked::<CBCharacteristic>(read_char),
                Retained::cast_unchecked::<CBCharacteristic>(write_char),
            ])));

            // 2. Servizio GAP (0x1800) per migliorare la stabilità del nome
            // FIX: Usiamo UUIDWithString con "1800" e "2A00"
            let gap_service_uuid = CBUUID::UUIDWithString(&NSString::from_str("1800"));
            let name_char_uuid = CBUUID::UUIDWithString(&NSString::from_str("2A00"));
            
            let name_data = NSString::from_str(LOCAL_NAME).dataUsingEncoding(4);
            let name_char = CBMutableCharacteristic::initWithType_properties_value_permissions(
                mtm.alloc(),
                &name_char_uuid,
                CBCharacteristicProperties::Read,
                name_data.as_ref().map(|d| &**d),
                CBAttributePermissions::Readable,
            );
            
            let gap_service = CBMutableService::initWithType_primary(mtm.alloc(), &gap_service_uuid, true);
            gap_service.setCharacteristics(Some(&NSArray::from_retained_slice(&[
                Retained::cast_unchecked::<CBCharacteristic>(name_char)
            ])));

            println!("⏳ [Rust] Registrazione servizi nel database di sistema...");
            manager.addService(&gap_service);
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

            println!("⏳ [Rust] Avvio Advertising...");
            manager.startAdvertising(Some(adv_data_any));
        }
    }
}

// Static to hold the delegate/manager alive since RunLoop doesn't own them directly in Rust memory model?
// Actually, `initWithDelegate` uses the ObjC runtime. The Delegate must be retained.
// We can just keep them on the stack of the `run_ble_runloop` function which never returns.

pub fn run_ble_runloop(_identity: RingIdentity) -> Result<()> {
    let mtm = MainThreadMarker::new().expect("Must run on Main Thread for macOS BLE");
    unsafe {
        println!("🚀 [Rust-Mac] Initializing BLE on Main Thread...");
        let delegate = BleDelegate::new(mtm);
        let delegate_proto = ProtocolObject::from_ref(&*delegate);
        
        // Queue = nil (Main Queue)
        // With RunLoop running, this works.
        let _manager = CBPeripheralManager::initWithDelegate_queue(
            mtm.alloc(),
            Some(delegate_proto),
            None, 
        );

        println!("👀 [Rust-Mac] RunLoop Starting. BLE should advertise now.");
        NSRunLoop::currentRunLoop().run();
    }
    // Unreachable
    Ok(())
}

// Deprecated/No-op for the trait used in async context, 
// since the real work is on the main thread now
pub async fn start_ble_service(_identity: RingIdentity) -> Result<()> {
   println!("⚠️ [BLE-Mac] start_ble_service called from async trait. Real implementation is in Main RunLoop.");
   Ok(())
}
