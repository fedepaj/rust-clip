use anyhow::{Result, Context};
use std::net::{UdpSocket, SocketAddr};
use std::sync::Arc;
use flume::Sender;
use crate::core::packet::{WirePacket, PacketType};
use crate::transport::TransportType;

#[derive(Clone)]
pub struct UdpTransport {
    socket: Arc<UdpSocket>,
    port: u16,
}

impl UdpTransport {
    pub fn new() -> Result<Self> {
        // Bind to 0.0.0.0:0 (OS assigned)
        let socket = UdpSocket::bind("0.0.0.0:0").context("Failed to bind UDP socket")?;
        socket.set_nonblocking(false)?; // Blocking read in dedicated thread is fine
        let port = socket.local_addr()?.port();
        
        println!("UDP Socket bound to port: {}", port);

        Ok(Self {
            socket: Arc::new(socket),
            port,
        })
    }

    pub fn get_port(&self) -> u16 {
        self.port
    }

    /// Start listening for incoming UDP packets and forward to Backend
    pub fn start(&self, tx_packet: Sender<WirePacket>) -> Result<()> {
        let socket = self.socket.clone();
        
        std::thread::spawn(move || {
            let mut buf = [0u8; 65535]; // Max UDP size
            loop {
                match socket.recv_from(&mut buf) {
                    Ok((amt, src)) => {
                        let data = &buf[..amt];
                        // Deserialize WirePacket
                        match bincode::deserialize::<WirePacket>(data) {
                            Ok(packet) => {
                                // Inject Source Addr into Metadata? Or handle at higher level?
                                // Ideally WirePacket metadata. We can update Topology here directly?
                                // Or backend updates topology based on PacketHeader?
                                // PacketHeader has SenderID.
                                // We can augment the packet or send a specific internal message.
                                // For now, just forward.
                                
                                // TODO: Update Topology with src addr using a side channel or shared state?
                                // Actually, Backend receives WirePacket. It doesn't know IP.
                                // Maybe we need a "NetworkEvent" enum instead of just WirePacket?
                                // Or define a special internal PacketType for "Transport Update"?
                                // Or just let mDNS handle discovery and UDP handle data.
                                // But UDP packets prove connectivity better.
                                
                                if let Err(e) = tx_packet.send(packet) {
                                    println!("❌ [UDP] Failed to forward packet to backend: {}", e);
                                    break;
                                }
                            },
                            Err(e) => {
                                println!("⚠️ [UDP] Failed to deserialize packet from {}: {}", src, e);
                            }
                        }
                    },
                    Err(e) => {
                        println!("⚠️ [UDP] Receive Error: {}", e);
                        // Backoff?
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
            }
        });
        
        Ok(())
    }

    pub fn send(&self, addr: SocketAddr, packet: &WirePacket) -> Result<()> {
        let data = bincode::serialize(packet)?;
        self.socket.send_to(&data, addr)?;
        Ok(())
    }
}
