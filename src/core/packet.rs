use anyhow::{Result, anyhow};
use serde::{Serialize, Deserialize};
use ed25519_dalek::{SigningKey, VerifyingKey, Signer, Verifier, Signature};
use chacha20poly1305::{
    aead::Aead,
    ChaCha20Poly1305, Nonce,
};
use rand::{RngCore, thread_rng};

/// Packet header — signed plaintext metadata
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PacketHeader {
    pub sender_id: String,       // StablePeerId of sender
    pub receiver_id: String,     // StablePeerId of target, or "broadcast"
    pub packet_type: PacketType,
    pub timestamp: u64,
    pub nonce: [u8; 12],
    pub ttl: u8,                 // Time-to-live for multi-hop (max 4)
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum PacketType {
    // --- Handshake ---
    Hello,
    Welcome,
    Ping,        // Session confirmation after handshake

    // --- Data ---
    ClipboardText,
    ClipboardAck,

    // --- File Transfer ---
    FileOffer,
    FileAccept,
    FileReject,
    FileData,
    FileAck,

    // --- Mesh Routing ---
    RouteAnnounce,
    RouteRequest,
    RouteReply,

    // --- Revocation ---
    RevocationNotice,

    // --- Hotspot ---
    HotspotRequest,
    HotspotReady,
    HotspotConnected,

    // --- Internal (never sent over wire) ---
    LinkUp,
}

/// Handshake messages carried inside Hello/Welcome payloads
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum HandshakePayload {
    Hello {
        stable_peer_id: String,
        ed25519_pubkey: Vec<u8>,
        ephemeral_pubkey: [u8; 32],
        timestamp: u64,
        signature: Vec<u8>,  // Ed25519 signature of {stable_peer_id, ed25519_pubkey, ephemeral_pubkey, timestamp}
        rotating_id: String, // mDNS rotating ID for LAN correlation
        ring_proof: Vec<u8>, // HMAC(root_secret, "ring-membership" || peer_id) — proves shared mnemonic
    },
    Welcome {
        stable_peer_id: String,
        ed25519_pubkey: Vec<u8>,
        ephemeral_pubkey: [u8; 32],
        timestamp: u64,
        signature: Vec<u8>,
        rotating_id: String,
        ring_proof: Vec<u8>,
    },
}

/// Maximum allowed clock skew between peers (seconds).
/// Packets with timestamps outside this window are rejected (anti-replay).
pub const TIMESTAMP_WINDOW_SECS: u64 = 60;

/// The structure sent over the wire
#[derive(Serialize, Deserialize, Debug)]
pub struct WirePacket {
    pub header: PacketHeader,
    pub payload: Vec<u8>,       // Encrypted (or plaintext for handshake)
    pub signature: Signature,   // Ed25519 signature of (header + payload)
}

impl WirePacket {
    /// Create an encrypted packet (post-handshake communication)
    pub fn new_encrypted(
        sender_id: String,
        receiver_id: String,
        packet_type: PacketType,
        plaintext: &[u8],
        session_key: &ChaCha20Poly1305,
        signing_key: &SigningKey,
    ) -> Result<Self> {
        let mut nonce_bytes = [0u8; 12];
        thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = session_key.encrypt(nonce, plaintext)
            .map_err(|_| anyhow!("Encryption failed"))?;

        let header = PacketHeader {
            sender_id,
            receiver_id,
            packet_type,
            timestamp: chrono::Utc::now().timestamp() as u64,
            nonce: nonce_bytes,
            ttl: 4,
        };

        let header_bytes = bincode::serialize(&header)?;
        let mut sign_data = Vec::with_capacity(header_bytes.len() + ciphertext.len());
        sign_data.extend_from_slice(&header_bytes);
        sign_data.extend_from_slice(&ciphertext);
        let signature = signing_key.sign(&sign_data);

        Ok(WirePacket { header, payload: ciphertext, signature })
    }

    /// Create a plaintext signed packet (for handshake messages)
    pub fn new_plain(
        sender_id: String,
        receiver_id: String,
        packet_type: PacketType,
        payload: &[u8],
        signing_key: &SigningKey,
    ) -> Result<Self> {
        let mut nonce_bytes = [0u8; 12];
        thread_rng().fill_bytes(&mut nonce_bytes);

        let header = PacketHeader {
            sender_id,
            receiver_id,
            packet_type,
            timestamp: chrono::Utc::now().timestamp() as u64,
            nonce: nonce_bytes,
            ttl: 4,
        };

        let header_bytes = bincode::serialize(&header)?;
        let payload = payload.to_vec();
        let mut sign_data = Vec::with_capacity(header_bytes.len() + payload.len());
        sign_data.extend_from_slice(&header_bytes);
        sign_data.extend_from_slice(&payload);
        let signature = signing_key.sign(&sign_data);

        Ok(WirePacket { header, payload, signature })
    }

    /// Verify packet signature
    pub fn verify_signature(&self, verify_key: &VerifyingKey) -> Result<()> {
        let header_bytes = bincode::serialize(&self.header)?;
        let mut sign_data = Vec::with_capacity(header_bytes.len() + self.payload.len());
        sign_data.extend_from_slice(&header_bytes);
        sign_data.extend_from_slice(&self.payload);
        verify_key.verify(&sign_data, &self.signature)
            .map_err(|e| anyhow!("Invalid signature: {}", e))
    }

    /// Decrypt payload using session key (post-handshake)
    pub fn decrypt_payload(&self, session_key: &ChaCha20Poly1305) -> Result<Vec<u8>> {
        let nonce = Nonce::from_slice(&self.header.nonce);
        session_key.decrypt(nonce, self.payload.as_ref())
            .map_err(|_| anyhow!("Decryption failed"))
    }

    /// Validate packet timestamp against current time.
    /// Rejects packets older than TIMESTAMP_WINDOW_SECS or from the future.
    pub fn validate_timestamp(&self) -> Result<()> {
        let now = chrono::Utc::now().timestamp() as u64;
        let ts = self.header.timestamp;

        if ts > now + TIMESTAMP_WINDOW_SECS {
            return Err(anyhow!("Packet timestamp {} is in the future (now={})", ts, now));
        }
        if now > ts + TIMESTAMP_WINDOW_SECS {
            return Err(anyhow!("Packet timestamp {} is too old (now={})", ts, now));
        }

        Ok(())
    }

    /// Decrement TTL. Returns false if packet should be dropped.
    pub fn decrement_ttl(&mut self) -> bool {
        if self.ttl() == 0 {
            return false;
        }
        self.header.ttl -= 1;
        true
    }

    pub fn ttl(&self) -> u8 {
        self.header.ttl
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chacha20poly1305::KeyInit;
    use ed25519_dalek::SigningKey;
    use rand::thread_rng;
    use rand::RngCore;

    fn test_signing_key() -> SigningKey {
        let mut bytes = [0u8; 32];
        thread_rng().fill_bytes(&mut bytes);
        SigningKey::from_bytes(&bytes)
    }

    #[test]
    fn plain_packet_roundtrip() {
        let key = test_signing_key();
        let vk = key.verifying_key();

        let packet = WirePacket::new_plain(
            "sender".into(),
            "receiver".into(),
            PacketType::Hello,
            b"test payload",
            &key,
        )
        .unwrap();

        assert_eq!(packet.header.sender_id, "sender");
        assert_eq!(packet.header.receiver_id, "receiver");
        assert_eq!(packet.payload, b"test payload");
        assert!(packet.verify_signature(&vk).is_ok());
    }

    #[test]
    fn signature_verification_fails_with_wrong_key() {
        let key = test_signing_key();
        let wrong_key = test_signing_key();
        let wrong_vk = wrong_key.verifying_key();

        let packet = WirePacket::new_plain(
            "sender".into(),
            "receiver".into(),
            PacketType::Ping,
            b"data",
            &key,
        )
        .unwrap();

        assert!(packet.verify_signature(&wrong_vk).is_err());
    }

    #[test]
    fn encrypted_packet_roundtrip() {
        let key = test_signing_key();
        let session_key = ChaCha20Poly1305::new(&[42u8; 32].into());

        let packet = WirePacket::new_encrypted(
            "sender".into(),
            "receiver".into(),
            PacketType::ClipboardText,
            b"secret clipboard",
            &session_key,
            &key,
        )
        .unwrap();

        let plaintext = packet.decrypt_payload(&session_key).unwrap();
        assert_eq!(plaintext, b"secret clipboard");
    }

    #[test]
    fn decrypt_fails_with_wrong_key() {
        let key = test_signing_key();
        let session_key = ChaCha20Poly1305::new(&[42u8; 32].into());
        let wrong_key = ChaCha20Poly1305::new(&[99u8; 32].into());

        let packet = WirePacket::new_encrypted(
            "sender".into(),
            "receiver".into(),
            PacketType::ClipboardText,
            b"secret",
            &session_key,
            &key,
        )
        .unwrap();

        assert!(packet.decrypt_payload(&wrong_key).is_err());
    }

    #[test]
    fn timestamp_validation_accepts_fresh_packet() {
        let key = test_signing_key();
        let packet = WirePacket::new_plain(
            "sender".into(),
            "receiver".into(),
            PacketType::Ping,
            b"data",
            &key,
        )
        .unwrap();

        assert!(packet.validate_timestamp().is_ok());
    }

    #[test]
    fn timestamp_validation_rejects_old_packet() {
        let key = test_signing_key();
        let mut packet = WirePacket::new_plain(
            "sender".into(),
            "receiver".into(),
            PacketType::Ping,
            b"data",
            &key,
        )
        .unwrap();

        // Set timestamp to 2 minutes ago
        packet.header.timestamp = chrono::Utc::now().timestamp() as u64 - 120;
        assert!(packet.validate_timestamp().is_err());
    }

    #[test]
    fn timestamp_validation_rejects_future_packet() {
        let key = test_signing_key();
        let mut packet = WirePacket::new_plain(
            "sender".into(),
            "receiver".into(),
            PacketType::Ping,
            b"data",
            &key,
        )
        .unwrap();

        // Set timestamp 2 minutes in the future
        packet.header.timestamp = chrono::Utc::now().timestamp() as u64 + 120;
        assert!(packet.validate_timestamp().is_err());
    }

    #[test]
    fn ttl_decrement() {
        let key = test_signing_key();
        let mut packet = WirePacket::new_plain(
            "sender".into(),
            "receiver".into(),
            PacketType::Ping,
            b"data",
            &key,
        )
        .unwrap();

        assert_eq!(packet.ttl(), 4);
        assert!(packet.decrement_ttl());
        assert_eq!(packet.ttl(), 3);

        // Drain TTL
        packet.header.ttl = 0;
        assert!(!packet.decrement_ttl());
    }
}
