use anyhow::Result;
use crate::core::identity::RingIdentity;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, rc::Retained, MainThreadOnly};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSDictionary, NSError, NSObject, NSObjectProtocol, NSRunLoop,
    NSString,
};
use objc2_core_bluetooth::{
    CBAdvertisementDataLocalNameKey, CBAdvertisementDataServiceUUIDsKey, CBPeripheralManager,
    CBPeripheralManagerDelegate, CBManagerState, CBUUID, CBMutableService, CBMutableCharacteristic,
    CBCharacteristicProperties, CBAttributePermissions, CBCharacteristic, CBATTRequest, CBATTError,
};
use std::thread;

// UUIDs costanti per il servizio
// In produzione, SERVICE_UUID dovrebbe essere derivato dal Rotating ID o fisso?
// Il manifesto dice: "Discovery ID: A time-based rotating hash used for BLE Service UUIDs".
// Quindi qui dobbiamo passare il UUID dinamicamente!
// Per ora usiamo costanti per il setup iniziale, poi le renderemo dinamiche.
const READ_CHAR_UUID:  &str = "99999999-0000-0000-0000-000000000002"; 
const WRITE_CHAR_UUID: &str = "99999999-0000-0000-0000-000000000003"; 

// Struct per passare i dati al Delegate (che è Objective-C e un po' rigido)
// Useremo variabili statiche o pattern singleton per semplicità in questa fase Initial,
// dato che objc2 define_class non supporta facilmente campi Rust complessi (Arc, Mutex).
// In futuro useremo ivars.

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
                println!("🔵 [BLE-Mac] Bluetooth ON. Configuring GATT...");
                // Qui dovremmo leggere l'identità o riceverla.
                // Per ora usiamo un UUID fisso di test o passiamo tramite un "Global State" trick se serve.
                let mtm = MainThreadMarker::from(self);
                self.setup_gatt_service(manager, mtm);
            } else {
                println!("⚠️ [BLE-Mac] Bluetooth State: {:?}", state);
            }
        }

        #[unsafe(method(peripheralManager:didAddService:error:))]
        fn peripheral_manager_did_add_service(&self, manager: &CBPeripheralManager, _service: &CBMutableService, error: Option<&NSError>) {
            if let Some(err) = error {
                println!("❌ [BLE-Mac] Error Adding Service: {}", err.localizedDescription());
            } else {
                println!("✅ [BLE-Mac] Service Added. Starting Advertising...");
                self.start_advertising_helper(manager);
            }
        }

        #[unsafe(method(peripheralManager:didReceiveReadRequest:))]
        fn peripheral_manager_did_receive_read_request(&self, manager: &CBPeripheralManager, request: &CBATTRequest) {
            unsafe {
                println!("📖 [BLE-Mac] Read Request on {:?}", request.characteristic().UUID());
                let response_text = "RustClip-Mac-Alive";
                
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
                        let ptr: *const std::os::raw::c_void = msg_send![&*data, bytes];
                        let len: usize = msg_send![&*data, length];
                        let slice = std::slice::from_raw_parts(ptr.cast::<u8>(), len);
                        
                        println!("📥 [BLE-Mac] Received {} bytes", len);
                        // TODO: Deserialize Packet and send to Event Bus
                        // let packet: WirePacket = bincode::deserialize(slice)...
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
            // TODO: Questo UUID deve venire dall'Identity!
            // Per ora hardcoded per verificare che compili e parta
            let service_uuid_str = "99999999-0000-0000-0000-000000000001"; 
            let service_uuid = CBUUID::UUIDWithString(&NSString::from_str(service_uuid_str));
            
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

            manager.addService(&service);
        }
    }

    fn start_advertising_helper(&self, manager: &CBPeripheralManager) {
        unsafe {
            // TODO: Nome dinamico
            let local_name = "RustClip-Mac"; 
            let service_uuid_str = "99999999-0000-0000-0000-000000000001";

            let val_name = NSString::from_str(local_name);
            let uuid_obj = CBUUID::UUIDWithString(&NSString::from_str(service_uuid_str));
            let val_uuids = NSArray::from_slice(&[&*uuid_obj]);

            let keys: [&NSString; 2] = [CBAdvertisementDataLocalNameKey, CBAdvertisementDataServiceUUIDsKey];
            let objects: [&NSObject; 2] = [&*val_name, &*val_uuids];
            let adv_data = NSDictionary::from_slices(&keys, &objects);

            let adv_data_any = &*(Retained::as_ptr(&adv_data) as *const NSDictionary<NSString, AnyObject>);

            manager.startAdvertising(Some(adv_data_any));
        }
    }
}

pub async fn start_ble_service(_identity: RingIdentity) -> Result<()> {
    // CoreBluetooth deve girare sul Main Thread su macOS per certe operazioni,
    // o almeno avere un RunLoop attivo.
    // Siccome siamo in una async task di Tokio, non siamo sul Main Thread dell'app principale.
    // Tuttavia, objc2 e Foundation permettono di creare un RunLoop sul thread corrente.
    
    // Spawn blocking thread per gestire il RunLoop BLE
    thread::spawn(move || {
        let mtm = unsafe { MainThreadMarker::new_unchecked() }; // ATTENZIONE: Hack per testing, in real app deve essere vero Main Thread o gestire RunLoop propriamente
        // Se `start_ble_service` viene chiamato da `main` prima di tokio, ok.
        // Ma qui siamo dentro tokio spawn.
        // CoreBluetooth spesso richiede il Main Thread UI vero (queue nil).
        // Se usiamo una queue dedicata dispatch_queue, potremmo evitare il MainThreadMarker check.
        
        // Per ora proviamo a vedere se NSRunLoop corrente basta.
        
        unsafe {
             println!("🚀 [BLE-Mac] Init...");
             // TODO: Pass identity to delegate via some mechanic
             let delegate = BleDelegate::new(mtm);
             let delegate_proto = ProtocolObject::from_ref(&*delegate);
             
             // Queue None = Main Queue. Se siamo in thread secondario, potrebbe non processare.
             // Proviamo a creare una dispatch queue o nil?
             let manager = CBPeripheralManager::initWithDelegate_queue(
                 mtm.alloc(),
                 Some(delegate_proto),
                 None, // Main Queue
             );
             
             // Blocchiamo questo thread col RunLoop
             NSRunLoop::currentRunLoop().run();
        }
    });

    Ok(())
}
