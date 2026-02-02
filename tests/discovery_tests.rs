use rust_clip::core::discovery;
use rust_clip::events::PeerInfo;
use dashmap::DashMap;
use std::sync::Arc;
use std::net::{SocketAddr, IpAddr, Ipv4Addr};
use std::time::SystemTime;

#[test]
fn test_sanitize_device_name() {
    assert_eq!(discovery::sanitize_device_name("My Device 123!"), "MyDevice123");
    assert_eq!(discovery::sanitize_device_name("iPad (Federico)"), "iPadFederico");
    assert_eq!(discovery::sanitize_device_name("Clean-Name"), "Clean-Name");
    assert_eq!(discovery::sanitize_device_name("Under_Score"), "Under_Score");
}

#[test]
fn test_peer_map_insertion() {
    let peer_map: discovery::PeerMap = Arc::new(DashMap::new());
    
    let peer_info = PeerInfo {
        name: "TestDevice".to_string(),
        ip: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080),
        device_id: "device_unique_id".to_string(),
        last_seen: SystemTime::now(),
    };
    
    peer_map.insert(peer_info.device_id.clone(), peer_info.clone());
    
    assert!(peer_map.contains_key("device_unique_id"));
    assert_eq!(peer_map.get("device_unique_id").unwrap().name, "TestDevice");
}
