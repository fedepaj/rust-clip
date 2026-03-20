use anyhow::{anyhow, Context, Result};
use flume::Sender;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;

use crate::transport::{TransportEvent, TransportSender, TransportType};

#[derive(Clone)]
pub struct UdpTransport {
    socket: Arc<UdpSocket>,
    port: u16,
}

impl UdpTransport {
    pub fn new() -> Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0").context("Failed to bind UDP socket")?;
        socket.set_nonblocking(false)?;
        let port = socket.local_addr()?.port();
        Ok(Self {
            socket: Arc::new(socket),
            port,
        })
    }

    pub fn get_port(&self) -> u16 {
        self.port
    }

    /// Start listening for incoming packets and forward as TransportEvents.
    pub fn start_listener(&self, event_tx: Sender<TransportEvent>) -> Result<()> {
        let socket = self.socket.clone();

        std::thread::spawn(move || {
            let mut buf = [0u8; 65535];
            loop {
                match socket.recv_from(&mut buf) {
                    Ok((amt, src)) => {
                        let data = buf[..amt].to_vec();
                        if let Err(e) = event_tx.send(TransportEvent::PacketReceived {
                            data,
                            from_transport: TransportType::Mdns,
                            from_handle: src.to_string(),
                        }) {
                            println!("  [UDP] Event send failed: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        println!("  [UDP] Receive error: {}", e);
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
            }
        });

        Ok(())
    }
}

impl TransportSender for UdpTransport {
    fn transport_type(&self) -> TransportType {
        TransportType::Mdns
    }

    fn send_to(&self, handle: &str, data: &[u8]) -> Result<()> {
        let addr: SocketAddr = handle
            .parse()
            .map_err(|e| anyhow!("Invalid address '{}': {}", handle, e))?;
        self.socket.send_to(data, addr)?;
        Ok(())
    }

    fn broadcast(&self, _data: &[u8]) -> Result<()> {
        Ok(())
    }
}
