use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::time::Instant;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit};
use ed25519_dalek::{Verifier, VerifyingKey, Signature};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey};
use zeroize::Zeroize;

use crate::core::identity::RingIdentity;
use crate::core::packet::{WirePacket, PacketType, HandshakePayload};
use crate::transport::TransportType;

const HANDSHAKE_TIMEOUT_SECS: u64 = 30;
const HKDF_SALT: &[u8] = b"rustclip-v2";

/// Per-peer handshake state
enum HandshakeState {
    /// We sent Hello, waiting for Welcome
    AwaitingWelcome {
        ephemeral_secret: EphemeralSecret,
        initiated_at: Instant,
    },
    /// Handshake completed
    Complete,
}

/// Manages concurrent handshakes with multiple peers
pub struct HandshakeManager {
    pending: HashMap<String, HandshakeState>, // keyed by remote stable_peer_id
}

/// Result of processing a handshake packet
pub enum HandshakeResult {
    /// Send this packet to the remote peer
    SendPacket(WirePacket),
    /// Handshake completed, here's the session key (+ optional reply to send)
    SessionEstablished {
        peer_id: String,
        peer_pubkey: Vec<u8>,
        session_key: ChaCha20Poly1305,
        transport: TransportType,
        /// If we're the responder (received Hello), this is the Welcome packet to send back
        reply_packet: Option<WirePacket>,
    },
    /// Handshake failed
    Failed(String),
    /// Nothing to do (duplicate, etc.)
    Ignored,
}

impl HandshakeManager {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
        }
    }

    /// Initiate a handshake with a discovered peer.
    /// Called when LinkUp or PeerDiscovered is received.
    pub fn initiate(
        &mut self,
        identity: &RingIdentity,
        remote_peer_id: &str,
    ) -> Result<WirePacket> {
        // Generate ephemeral X25519 keys
        let (secret, public) = RingIdentity::generate_ephemeral_key();
        let timestamp = chrono::Utc::now().timestamp() as u64;

        // Sign the handshake data: {stable_peer_id, ed25519_pubkey, ephemeral_pubkey, timestamp}
        let mut sign_data = Vec::new();
        sign_data.extend_from_slice(identity.stable_peer_id().as_bytes());
        sign_data.extend_from_slice(identity.public_key.as_bytes());
        sign_data.extend_from_slice(public.as_bytes());
        sign_data.extend_from_slice(&timestamp.to_le_bytes());
        let inner_sig = identity.sign(&sign_data);

        let hello = HandshakePayload::Hello {
            stable_peer_id: identity.stable_peer_id().to_string(),
            ed25519_pubkey: identity.public_key.as_bytes().to_vec(),
            ephemeral_pubkey: *public.as_bytes(),
            timestamp,
            signature: inner_sig.to_bytes().to_vec(),
        };

        let payload_bytes = bincode::serialize(&hello)?;
        let packet = WirePacket::new_plain(
            identity.stable_peer_id().to_string(),
            remote_peer_id.to_string(),
            PacketType::Hello,
            &payload_bytes,
            &identity.identity_key,
        )?;

        // Store pending state
        self.pending.insert(
            remote_peer_id.to_string(),
            HandshakeState::AwaitingWelcome {
                ephemeral_secret: secret,
                initiated_at: Instant::now(),
            },
        );

        Ok(packet)
    }

    /// Process an incoming Hello packet (we are the responder).
    /// Returns a Welcome packet and the derived session key.
    pub fn process_hello(
        &mut self,
        identity: &RingIdentity,
        packet: &WirePacket,
    ) -> HandshakeResult {
        // Deserialize payload
        let hello = match bincode::deserialize::<HandshakePayload>(&packet.payload) {
            Ok(h) => h,
            Err(e) => return HandshakeResult::Failed(format!("Failed to deserialize Hello: {}", e)),
        };

        let (peer_stable_id, peer_pubkey_bytes, peer_eph_bytes, timestamp, inner_sig_bytes) = match hello {
            HandshakePayload::Hello { stable_peer_id, ed25519_pubkey, ephemeral_pubkey, timestamp, signature } => {
                (stable_peer_id, ed25519_pubkey, ephemeral_pubkey, timestamp, signature)
            }
            _ => return HandshakeResult::Failed("Expected Hello payload".to_string()),
        };

        // Verify inner signature
        let peer_vk = match VerifyingKey::from_bytes(
            peer_pubkey_bytes.as_slice().try_into().unwrap_or(&[0u8; 32])
        ) {
            Ok(vk) => vk,
            Err(_) => return HandshakeResult::Failed("Invalid peer public key".to_string()),
        };

        let mut sign_data = Vec::new();
        sign_data.extend_from_slice(peer_stable_id.as_bytes());
        sign_data.extend_from_slice(&peer_pubkey_bytes);
        sign_data.extend_from_slice(&peer_eph_bytes);
        sign_data.extend_from_slice(&timestamp.to_le_bytes());

        let inner_sig = match Signature::from_bytes(
            inner_sig_bytes.as_slice().try_into().unwrap_or(&[0u8; 64])
        ) {
            sig => sig,
        };

        if peer_vk.verify(&sign_data, &inner_sig).is_err() {
            return HandshakeResult::Failed("Hello inner signature verification failed".to_string());
        }

        // Verify stable_peer_id matches the pubkey
        let expected_id = RingIdentity::compute_stable_peer_id(&peer_vk);
        if expected_id != peer_stable_id {
            return HandshakeResult::Failed("StablePeerId mismatch".to_string());
        }

        // Generate our ephemeral keys
        let (secret, public) = RingIdentity::generate_ephemeral_key();
        let our_timestamp = chrono::Utc::now().timestamp() as u64;

        // Derive session key
        let peer_eph_public = PublicKey::from(peer_eph_bytes);
        let shared_secret = secret.diffie_hellman(&peer_eph_public);

        let session_key = match derive_session_key(
            shared_secret.as_bytes(),
            identity.stable_peer_id(),
            &peer_stable_id,
        ) {
            Ok(k) => k,
            Err(e) => return HandshakeResult::Failed(format!("Key derivation failed: {}", e)),
        };

        // Sign our Welcome
        let mut our_sign_data = Vec::new();
        our_sign_data.extend_from_slice(identity.stable_peer_id().as_bytes());
        our_sign_data.extend_from_slice(identity.public_key.as_bytes());
        our_sign_data.extend_from_slice(public.as_bytes());
        our_sign_data.extend_from_slice(&our_timestamp.to_le_bytes());
        let our_inner_sig = identity.sign(&our_sign_data);

        let welcome = HandshakePayload::Welcome {
            stable_peer_id: identity.stable_peer_id().to_string(),
            ed25519_pubkey: identity.public_key.as_bytes().to_vec(),
            ephemeral_pubkey: *public.as_bytes(),
            timestamp: our_timestamp,
            signature: our_inner_sig.to_bytes().to_vec(),
        };

        let payload_bytes = match bincode::serialize(&welcome) {
            Ok(b) => b,
            Err(e) => return HandshakeResult::Failed(format!("Serialize Welcome failed: {}", e)),
        };

        let reply = match WirePacket::new_plain(
            identity.stable_peer_id().to_string(),
            peer_stable_id.clone(),
            PacketType::Welcome,
            &payload_bytes,
            &identity.identity_key,
        ) {
            Ok(p) => p,
            Err(e) => return HandshakeResult::Failed(format!("Create Welcome packet failed: {}", e)),
        };

        self.pending.insert(peer_stable_id.clone(), HandshakeState::Complete);
        println!("  [Handshake] Hello processed from {}. Session key derived.", peer_stable_id);

        HandshakeResult::SessionEstablished {
            peer_id: peer_stable_id,
            peer_pubkey: peer_pubkey_bytes,
            session_key,
            transport: TransportType::Ble,
            reply_packet: Some(reply),
        }
    }

    /// Process an incoming Welcome packet (we initiated the handshake).
    pub fn process_welcome(
        &mut self,
        identity: &RingIdentity,
        packet: &WirePacket,
    ) -> HandshakeResult {
        // Deserialize payload
        let welcome = match bincode::deserialize::<HandshakePayload>(&packet.payload) {
            Ok(w) => w,
            Err(e) => return HandshakeResult::Failed(format!("Failed to deserialize Welcome: {}", e)),
        };

        let (peer_stable_id, peer_pubkey_bytes, peer_eph_bytes, timestamp, inner_sig_bytes) = match welcome {
            HandshakePayload::Welcome { stable_peer_id, ed25519_pubkey, ephemeral_pubkey, timestamp, signature } => {
                (stable_peer_id, ed25519_pubkey, ephemeral_pubkey, timestamp, signature)
            }
            _ => return HandshakeResult::Failed("Expected Welcome payload".to_string()),
        };

        // Retrieve pending state — try peer_stable_id, then sender_id, then "broadcast"
        // (when we initiated via LinkEstablished, we didn't know the peer's ID)
        let state = self.pending.remove(&peer_stable_id)
            .or_else(|| self.pending.remove(&packet.header.sender_id))
            .or_else(|| self.pending.remove("broadcast"));

        let state = match state {
            Some(s) => s,
            None => return HandshakeResult::Failed(format!("No pending handshake for {}", peer_stable_id)),
        };

        let ephemeral_secret = match state {
            HandshakeState::AwaitingWelcome { ephemeral_secret, initiated_at } => {
                if initiated_at.elapsed().as_secs() > HANDSHAKE_TIMEOUT_SECS {
                    return HandshakeResult::Failed("Handshake timed out".to_string());
                }
                ephemeral_secret
            }
            HandshakeState::Complete => return HandshakeResult::Ignored,
        };

        // Verify inner signature
        let peer_vk = match VerifyingKey::from_bytes(
            peer_pubkey_bytes.as_slice().try_into().unwrap_or(&[0u8; 32])
        ) {
            Ok(vk) => vk,
            Err(_) => return HandshakeResult::Failed("Invalid peer public key in Welcome".to_string()),
        };

        let mut sign_data = Vec::new();
        sign_data.extend_from_slice(peer_stable_id.as_bytes());
        sign_data.extend_from_slice(&peer_pubkey_bytes);
        sign_data.extend_from_slice(&peer_eph_bytes);
        sign_data.extend_from_slice(&timestamp.to_le_bytes());

        let inner_sig = Signature::from_bytes(
            inner_sig_bytes.as_slice().try_into().unwrap_or(&[0u8; 64])
        );

        if peer_vk.verify(&sign_data, &inner_sig).is_err() {
            return HandshakeResult::Failed("Welcome inner signature verification failed".to_string());
        }

        // Verify stable_peer_id
        let expected_id = RingIdentity::compute_stable_peer_id(&peer_vk);
        if expected_id != peer_stable_id {
            return HandshakeResult::Failed("Welcome StablePeerId mismatch".to_string());
        }

        // Derive session key
        let peer_eph_public = PublicKey::from(peer_eph_bytes);
        let shared_secret = ephemeral_secret.diffie_hellman(&peer_eph_public);
        // ephemeral_secret is consumed (moved into diffie_hellman), providing forward secrecy

        let session_key = match derive_session_key(
            shared_secret.as_bytes(),
            identity.stable_peer_id(),
            &peer_stable_id,
        ) {
            Ok(k) => k,
            Err(e) => return HandshakeResult::Failed(format!("Key derivation failed: {}", e)),
        };

        self.pending.insert(peer_stable_id.clone(), HandshakeState::Complete);

        println!("  [Handshake] Welcome processed from {}. Session established.", peer_stable_id);

        HandshakeResult::SessionEstablished {
            peer_id: peer_stable_id,
            peer_pubkey: peer_pubkey_bytes,
            session_key,
            transport: TransportType::Ble,
            reply_packet: None, // Client doesn't need to reply
        }
    }

    /// Clean up timed-out handshakes
    pub fn cleanup_stale(&mut self) {
        self.pending.retain(|_, state| {
            match state {
                HandshakeState::AwaitingWelcome { initiated_at, .. } => {
                    initiated_at.elapsed().as_secs() < HANDSHAKE_TIMEOUT_SECS
                }
                HandshakeState::Complete => false, // Remove completed
            }
        });
    }

    /// Check if we have a pending handshake with this peer
    pub fn has_pending(&self, peer_id: &str) -> bool {
        self.pending.contains_key(peer_id)
    }
}

/// Derive a ChaCha20Poly1305 session key from a DH shared secret.
/// Uses HKDF-SHA256 with both peer IDs sorted for deterministic binding.
fn derive_session_key(
    shared_secret: &[u8],
    my_id: &str,
    peer_id: &str,
) -> Result<ChaCha20Poly1305> {
    // Sort peer IDs for deterministic info string (prevents reflection attacks)
    let info = if my_id < peer_id {
        format!("{}:{}", my_id, peer_id)
    } else {
        format!("{}:{}", peer_id, my_id)
    };

    let hkdf = Hkdf::<Sha256>::new(Some(HKDF_SALT), shared_secret);
    let mut key_bytes = [0u8; 32];
    hkdf.expand(info.as_bytes(), &mut key_bytes)
        .map_err(|_| anyhow!("HKDF expansion failed for session key"))?;

    let key = ChaCha20Poly1305::new(&key_bytes.into());

    // Zeroize the raw key material
    key_bytes.zeroize();

    Ok(key)
}
