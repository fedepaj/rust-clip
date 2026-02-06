use anyhow::{Result, anyhow};
use serde::{Serialize, Deserialize};
use crate::core::identity::RingIdentity;
use crate::core::packet::{WirePacket, PacketType};
use ed25519_dalek::{VerifyingKey, Signature};

#[derive(Serialize, Deserialize, Debug)]
pub struct HandshakePayload {
    pub device_name: String,
    pub public_key: [u8; 32],
}

pub struct HandshakeManager {
    identity: RingIdentity,
}

impl HandshakeManager {
    pub fn new(identity: RingIdentity) -> Self {
        Self { identity }
    }

    /// Process an incoming packet for handshake logic
    pub fn handle_packet(&self, packet: WirePacket) -> Result<()> {
        match packet.header.packet_type {
            PacketType::Hello => {
                println!("👋 [Handshake] Received Hello from {}", packet.header.sender_id);
                
                // 1. In our "Shared Mnemonic" model, we expect the sender to have the SAME public key
                // or at least be verifiable using our own public key if they are a twin.
                // For now, we use a simple shared session key (deterministically derived from root_secret)
                // for the initial handshake packet encryption.
                
                // --- Key Derivation (Handshake Specific) ---
                use chacha20poly1305::{ChaCha20Poly1305, KeyInit};
                let mut session_key_bytes = [0u8; 32];
                // Derive a deterministic initial key for the handshake
                // In a real scenario, we'd use Diffie-Hellman, but for "Ring" we can start with a shared secret.
                let mut salt = [0u8; 32]; // Fixed salt for initial hello
                let hkdf = hkdf::Hkdf::<sha2::Sha256>::new(Some(&salt), &self.identity.identity_key.to_bytes());
                hkdf.expand(b"initial_handshake_v1", &mut session_key_bytes)
                    .map_err(|_| anyhow!("HKDF failed"))?;
                
                let cipher = ChaCha20Poly1305::new(&session_key_bytes.into());

                // 2. Open the packet
                match packet.open(&cipher, &self.identity.public_key) {
                    Ok((header, payload)) => {
                        println!("✅ [Handshake] Packet Verified! Peer is authenticated as part of the Ring.");
                        if let Ok(data) = bincode::deserialize::<HandshakePayload>(&payload) {
                             println!("📱 Device Name: {}", data.device_name);
                             // TODO: Add to authenticated peers list
                        }
                        Ok(())
                    }
                    Err(e) => {
                        println!("❌ [Handshake] Verification Failed: {}", e);
                        Err(anyhow!("Unauthorized Peer"))
                    }
                }
            }
            _ => {
                // Ignore other packets for now if not authenticated
                Ok(())
            }
        }
    }
}
