use anyhow::Result;
use async_trait::async_trait;
use crate::transport::{Transport, TransportType, Peer, PeerId};
use crate::core::packet::WirePacket;
use crate::core::identity::RingIdentity;
use std::sync::Arc;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "linux")]
pub mod linux; 

pub struct BleTransport {
    identity: RingIdentity,
}

impl BleTransport {
    pub fn new(identity: RingIdentity) -> Arc<Self> {
        Arc::new(Self { identity })
    }
}

#[async_trait]
impl Transport for BleTransport {
    async fn start(&self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            return macos::start_ble_service(self.identity.clone()).await;
        }

        #[cfg(target_os = "windows")]
        {
            return windows::start_ble_service(self.identity.clone()).await;
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            println!("🔒 BLE not supported on this OS yet.");
            Ok(())
        }
    }

    async fn send(&self, _peer_id: &PeerId, _packet: WirePacket) -> Result<()> {
        // TODO: Implement send logic routing to specific OS implementation
        println!("TODO: BLE Send");
        Ok(())
    }

    async fn broadcast(&self, _packet: WirePacket) -> Result<()> {
        // UDP broadcast equivalent for BLE is not standard, mainly Advertising data
        Ok(())
    }
}
