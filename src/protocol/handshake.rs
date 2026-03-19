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


const HANDSHAKE_TIMEOUT_SECS: u64 = 30;
const HKDF_SALT: &[u8] = b"rustclip-v2";

/// Per-peer handshake state
struct PendingHandshake {
    ephemeral_secret: EphemeralSecret,
    initiated_at: Instant,
}

/// Manages concurrent handshakes with multiple peers.
/// Transport-agnostic: works identically over BLE, UDP, TCP, etc.
pub struct HandshakeManager {
    /// Pending handshakes keyed by a lookup key.
    /// For BLE: "broadcast" (don't know peer ID yet)
    /// For mDNS/LAN: rotating_id of the discovered peer
    pending: HashMap<String, PendingHandshake>,
}

/// Result of processing a handshake packet
pub enum HandshakeResult {
    /// Handshake completed — session key derived
    SessionEstablished {
        peer_id: String,
        peer_pubkey: Vec<u8>,
        peer_rotating_id: String,
        session_key: ChaCha20Poly1305,
        /// Welcome packet to send back (only set for Hello responder)
        reply_packet: Option<WirePacket>,
    },
    /// Handshake failed
    Failed(String),
    /// Nothing to do (simultaneous Hello tie-break, duplicate, etc.)
    Ignored,
}

impl HandshakeManager {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
        }
    }

    /// Initiate a handshake with a discovered peer.
    ///
    /// `pending_key`: key for tracking the pending state.
    ///   - BLE: "broadcast" (peer ID unknown until handshake completes)
    ///   - mDNS: the peer's rotating_id (from discovery)
    ///
    /// The Hello packet always uses receiver_id = "broadcast" so any peer
    /// will process it (they can't match by StablePeerId before handshake).
    pub fn initiate(
        &mut self,
        identity: &RingIdentity,
        pending_key: &str,
    ) -> Result<WirePacket> {
        let (secret, public) = RingIdentity::generate_ephemeral_key();
        let timestamp = chrono::Utc::now().timestamp() as u64;

        // Sign: {stable_peer_id, ed25519_pubkey, ephemeral_pubkey, timestamp}
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
            rotating_id: identity.get_rotating_id(),
        };

        let payload_bytes = bincode::serialize(&hello)?;
        let packet = WirePacket::new_plain(
            identity.stable_peer_id().to_string(),
            "broadcast".to_string(), // Always broadcast — receiver checks ring membership
            PacketType::Hello,
            &payload_bytes,
            &identity.identity_key,
        )?;

        self.pending.insert(
            pending_key.to_string(),
            PendingHandshake {
                ephemeral_secret: secret,
                initiated_at: Instant::now(),
            },
        );

        Ok(packet)
    }

    /// Process an incoming Hello packet (we are the responder).
    ///
    /// Includes tie-break for simultaneous Hello:
    /// if both peers sent Hello, the one with the larger StablePeerId
    /// ignores the incoming Hello (they wait for Welcome instead).
    pub fn process_hello(
        &mut self,
        identity: &RingIdentity,
        packet: &WirePacket,
    ) -> HandshakeResult {
        let hello = match bincode::deserialize::<HandshakePayload>(&packet.payload) {
            Ok(h) => h,
            Err(e) => return HandshakeResult::Failed(format!("Deserialize Hello failed: {}", e)),
        };

        let (peer_stable_id, peer_pubkey_bytes, peer_eph_bytes, timestamp, inner_sig_bytes, peer_rotating_id) = match hello {
            HandshakePayload::Hello { stable_peer_id, ed25519_pubkey, ephemeral_pubkey, timestamp, signature, rotating_id } => {
                (stable_peer_id, ed25519_pubkey, ephemeral_pubkey, timestamp, signature, rotating_id)
            }
            _ => return HandshakeResult::Failed("Expected Hello payload".to_string()),
        };

        // --- Simultaneous Hello tie-break ---
        // If we also sent a Hello (pending exists), the peer with the LARGER
        // StablePeerId ignores the Hello and waits for Welcome.
        // The peer with the SMALLER StablePeerId becomes the responder.
        let has_pending = !self.pending.is_empty();
        if has_pending {
            let my_id = identity.stable_peer_id();
            if my_id > peer_stable_id.as_str() {
                // I have the larger ID → I'm the initiator → ignore this Hello
                println!("  [Handshake] Simultaneous Hello from {}. Tie-break: I'm initiator, ignoring.", &peer_stable_id[..8]);
                return HandshakeResult::Ignored;
            }
            // I have the smaller ID → I'm the responder → process Hello, abandon my pending
            println!("  [Handshake] Simultaneous Hello from {}. Tie-break: I'm responder.", &peer_stable_id[..8]);
            self.pending.clear(); // Drop all pending (we're switching to responder role)
        }

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

        let inner_sig = Signature::from_bytes(
            inner_sig_bytes.as_slice().try_into().unwrap_or(&[0u8; 64])
        );

        if peer_vk.verify(&sign_data, &inner_sig).is_err() {
            return HandshakeResult::Failed("Hello signature verification failed".to_string());
        }

        // Verify stable_peer_id matches the pubkey
        let expected_id = RingIdentity::compute_stable_peer_id(&peer_vk);
        if expected_id != peer_stable_id {
            return HandshakeResult::Failed("StablePeerId mismatch".to_string());
        }

        // Generate our ephemeral keys and derive session key
        let (secret, public) = RingIdentity::generate_ephemeral_key();
        let our_timestamp = chrono::Utc::now().timestamp() as u64;

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

        // Build Welcome reply
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
            rotating_id: identity.get_rotating_id(),
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
            Err(e) => return HandshakeResult::Failed(format!("Create Welcome failed: {}", e)),
        };

        println!("  [Handshake] Session derived with {} (responder).", &peer_stable_id[..8]);

        HandshakeResult::SessionEstablished {
            peer_id: peer_stable_id,
            peer_pubkey: peer_pubkey_bytes,
            peer_rotating_id,
            session_key,
            reply_packet: Some(reply),
        }
    }

    /// Process an incoming Welcome packet (we initiated the handshake).
    pub fn process_welcome(
        &mut self,
        identity: &RingIdentity,
        packet: &WirePacket,
    ) -> HandshakeResult {
        let welcome = match bincode::deserialize::<HandshakePayload>(&packet.payload) {
            Ok(w) => w,
            Err(e) => return HandshakeResult::Failed(format!("Deserialize Welcome failed: {}", e)),
        };

        let (peer_stable_id, peer_pubkey_bytes, peer_eph_bytes, timestamp, inner_sig_bytes, peer_rotating_id) = match welcome {
            HandshakePayload::Welcome { stable_peer_id, ed25519_pubkey, ephemeral_pubkey, timestamp, signature, rotating_id } => {
                (stable_peer_id, ed25519_pubkey, ephemeral_pubkey, timestamp, signature, rotating_id)
            }
            _ => return HandshakeResult::Failed("Expected Welcome payload".to_string()),
        };

        // Lookup chain: try multiple keys to find our pending handshake
        // 1. peer's StablePeerId (if we knew it when initiating)
        // 2. peer's rotating_id (if we initiated via mDNS discovery)
        // 3. packet sender_id
        // 4. "broadcast" (BLE fallback — we didn't know peer ID)
        let state = self.pending.remove(&peer_stable_id)
            .or_else(|| self.pending.remove(&peer_rotating_id))
            .or_else(|| self.pending.remove(&packet.header.sender_id))
            .or_else(|| self.pending.remove("broadcast"));

        let pending = match state {
            Some(s) => s,
            None => return HandshakeResult::Failed(format!("No pending handshake for {}", &peer_stable_id[..8])),
        };

        if pending.initiated_at.elapsed().as_secs() > HANDSHAKE_TIMEOUT_SECS {
            return HandshakeResult::Failed("Handshake timed out".to_string());
        }

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
            return HandshakeResult::Failed("Welcome signature verification failed".to_string());
        }

        let expected_id = RingIdentity::compute_stable_peer_id(&peer_vk);
        if expected_id != peer_stable_id {
            return HandshakeResult::Failed("Welcome StablePeerId mismatch".to_string());
        }

        // Derive session key (ephemeral_secret is consumed → forward secrecy)
        let peer_eph_public = PublicKey::from(peer_eph_bytes);
        let shared_secret = pending.ephemeral_secret.diffie_hellman(&peer_eph_public);

        let session_key = match derive_session_key(
            shared_secret.as_bytes(),
            identity.stable_peer_id(),
            &peer_stable_id,
        ) {
            Ok(k) => k,
            Err(e) => return HandshakeResult::Failed(format!("Key derivation failed: {}", e)),
        };

        println!("  [Handshake] Session established with {} (initiator).", &peer_stable_id[..8]);

        HandshakeResult::SessionEstablished {
            peer_id: peer_stable_id,
            peer_pubkey: peer_pubkey_bytes,
            peer_rotating_id,
            session_key,
            reply_packet: None,
        }
    }

    /// Clean up timed-out handshakes
    pub fn cleanup_stale(&mut self) {
        self.pending.retain(|_, p| {
            p.initiated_at.elapsed().as_secs() < HANDSHAKE_TIMEOUT_SECS
        });
    }

    pub fn has_pending(&self, key: &str) -> bool {
        self.pending.contains_key(key)
    }

    pub fn has_any_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

/// Derive a ChaCha20Poly1305 session key from a DH shared secret.
/// Uses HKDF-SHA256 with both peer IDs sorted for deterministic binding.
fn derive_session_key(
    shared_secret: &[u8],
    my_id: &str,
    peer_id: &str,
) -> Result<ChaCha20Poly1305> {
    let info = if my_id < peer_id {
        format!("{}:{}", my_id, peer_id)
    } else {
        format!("{}:{}", peer_id, my_id)
    };

    let hkdf = Hkdf::<Sha256>::new(Some(HKDF_SALT), shared_secret);
    let mut key_bytes = [0u8; 32];
    hkdf.expand(info.as_bytes(), &mut key_bytes)
        .map_err(|_| anyhow!("HKDF expansion failed"))?;

    let key = ChaCha20Poly1305::new(&key_bytes.into());
    key_bytes.zeroize();
    Ok(key)
}
