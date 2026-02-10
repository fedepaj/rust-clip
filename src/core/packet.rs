use anyhow::{Result, anyhow};
use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use ed25519_dalek::{Verifier, Signature, SigningKey, VerifyingKey, Signer};
use chacha20poly1305::{
    aead::Aead, 
    ChaCha20Poly1305, Nonce 
};
use rand::{RngCore, thread_rng};

// Header is PLAIN TEXT but SIGNED
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PacketHeader {
    pub sender_id: String,      // Public Key (Base64 or Hex representation)
    pub receiver_id: String,    // Target Peer ID (or "broadcast")
    pub packet_type: PacketType,
    pub timestamp: u64,
    pub nonce: [u8; 12],        // Public Nonce for Encryption
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum PacketType {
    Hello,
    Welcome,
    ClipboardText,
    FileChunk,
    Ack,
    LinkUp, // New: Transport signals connection to Backend
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum HandshakeMsg {
    Hello {
        pubkey: Vec<u8>,
        rotating_id: String,
        ephemeral_key: [u8; 32],
        vector_clock: HashMap<String, u64>,
    },
    Welcome {
        pubkey: Vec<u8>,
        ephemeral_key: [u8; 32],
        vector_clock: HashMap<String, u64>,
    },
}

// The structure sent over the wire
#[derive(Serialize, Deserialize, Debug)]
pub struct WirePacket {
    pub header: PacketHeader,
    pub payload: Vec<u8>,       // Encrypted
    pub signature: Signature,   // Signs (Header + EncryptedPayload)
}

impl WirePacket {
    /// Create a new secure packet
    pub fn new(
        sender_id: String,
        receiver_id: String,
        packet_type: PacketType,
        payload_plain: &[u8],
        session_key: &ChaCha20Poly1305, // Symmetric Session Key
        signing_key: &SigningKey,       // Identity Key
    ) -> Result<Self> {
        // 1. Generate Nonce
        let mut nonce_bytes = [0u8; 12];
        thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        // 2. Encrypt Payload
        let ciphertext = session_key.encrypt(nonce, payload_plain)
            .map_err(|_| anyhow!("Encryption failed"))?;

        // 3. Create Header
        let header = PacketHeader {
            sender_id,
            receiver_id,
            packet_type,
            timestamp: chrono::Utc::now().timestamp() as u64,
            nonce: nonce_bytes,
        };

        // 4. Sign (Header Bytes + Ciphertext)
        let header_bytes = bincode::serialize(&header)?;
        let mut sign_data = Vec::with_capacity(header_bytes.len() + ciphertext.len());
        sign_data.extend_from_slice(&header_bytes);
        sign_data.extend_from_slice(&ciphertext);

        let signature = signing_key.sign(&sign_data);

        Ok(WirePacket {
            header,
            payload: ciphertext,
            signature,
        })
    }

    /// Verify and Open a packet
    pub fn open(
        &self,
        session_key: &ChaCha20Poly1305,
        verify_key: &VerifyingKey
    ) -> Result<(PacketHeader, Vec<u8>)> {
        // 1. Verify Signature
        let header_bytes = bincode::serialize(&self.header)?;
        let mut sign_data = Vec::with_capacity(header_bytes.len() + self.payload.len());
        sign_data.extend_from_slice(&header_bytes);
        sign_data.extend_from_slice(&self.payload);

        verify_key.verify(&sign_data, &self.signature)
            .map_err(|_| anyhow!("Invalid Signature! Packet may be tampered."))?;

        // 2. Decrypt Payload
        let nonce = Nonce::from_slice(&self.header.nonce);
        let plaintext = session_key.decrypt(nonce, self.payload.as_ref())
            .map_err(|_| anyhow!("Decryption failed! Wrong session key?"))?;

        Ok((self.header.clone(), plaintext))
    }

    /// Create a plaintext packet (e.g. Handshake) - Signed but NOT Encrypted
    pub fn new_plain(
        sender_id: String,
        receiver_id: String,
        packet_type: PacketType,
        payload_plain: &[u8],
        signing_key: &SigningKey,
    ) -> Result<Self> {
        // 1. Generate Nonce (dummy for plain, or random)
        let mut nonce_bytes = [0u8; 12];
        thread_rng().fill_bytes(&mut nonce_bytes);

        // 2. Clear Payload (No Encryption)
        let payload = payload_plain.to_vec();

        // 3. Create Header
        let header = PacketHeader {
            sender_id,
            receiver_id,
            packet_type,
            timestamp: chrono::Utc::now().timestamp() as u64,
            nonce: nonce_bytes,
        };

        // 4. Sign (Header Bytes + Payload)
        // Note: Sign plain payload
        let header_bytes = bincode::serialize(&header)?;
        let mut sign_data = Vec::with_capacity(header_bytes.len() + payload.len());
        sign_data.extend_from_slice(&header_bytes);
        sign_data.extend_from_slice(&payload);

        let signature = signing_key.sign(&sign_data);

        Ok(WirePacket {
            header,
            payload, // Plaintext
            signature,
        })
    }

    /// Verify and Open a Plaintext packet
    pub fn open_plain(
        &self,
        verify_key: &VerifyingKey
    ) -> Result<(PacketHeader, Vec<u8>)> {
        // 1. Verify Signature
        let header_bytes = bincode::serialize(&self.header)?;
        let mut sign_data = Vec::with_capacity(header_bytes.len() + self.payload.len());
        sign_data.extend_from_slice(&header_bytes);
        sign_data.extend_from_slice(&self.payload);

        verify_key.verify(&sign_data, &self.signature)
            .map_err(|e| anyhow!("Invalid Signature! {}", e))?;

        // 2. Return Payload as is
        Ok((self.header.clone(), self.payload.clone()))
    }
}
