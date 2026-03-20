use anyhow::{Context, Result, anyhow};
use bip39::{Mnemonic, Language};
use rand::{RngCore, thread_rng};
use serde::{Serialize, Deserialize};
use std::fs;
use std::path::PathBuf;
use sha2::Sha256;
use hmac::{Hmac, Mac};
use hkdf::Hkdf;
use aes_gcm::{
    aead::{Aead, KeyInit}, 
    Aes256Gcm, Nonce 
};
use ed25519_dalek::{SigningKey, VerifyingKey, Signer, Verifier, Signature};
use chrono::{Utc, Timelike, Datelike};
use directories::ProjectDirs;
use x25519_dalek::{EphemeralSecret, PublicKey};

// Type alias for HMAC-SHA256
type HmacSha256 = Hmac<Sha256>;

/// Stable peer identifier — SHA-256 of Ed25519 public key, truncated to 20 bytes, hex-encoded.
/// This never changes for a given identity and is the canonical routing identifier.
pub type StablePeerId = String;

#[derive(Clone, Debug)]
pub struct RingIdentity {
    pub mnemonic: String,
    pub identity_key: SigningKey, // Ed25519 Private Key
    pub public_key: VerifyingKey, // Ed25519 Public Key
    root_secret: [u8; 32],        // Derived from Mnemonic
    stable_peer_id: StablePeerId, // Cached stable ID
}

#[derive(Serialize, Deserialize)]
struct StoredIdentity {
    mnemonic: String,
}

impl RingIdentity {
    /// Create a new identity by generating a random mnemonic.
    pub fn create_new() -> Result<Self> {
        let mut entropy = [0u8; 32];
        thread_rng().fill_bytes(&mut entropy);

        let mnemonic = Mnemonic::from_entropy_in(Language::English, &entropy)?;
        let phrase = mnemonic.to_string();

        println!("  [Identity] New ring identity created");

        let identity = Self::from_mnemonic(&phrase)?;
        identity.save()?;
        Ok(identity)
    }

    /// Restore identity from an existing mnemonic phrase.
    pub fn from_mnemonic(phrase: &str) -> Result<Self> {
        let mnemonic = Mnemonic::parse_in_normalized(Language::English, phrase)
            .context("Invalid mnemonic words")?;

        let entropy = mnemonic.to_entropy();

        let hkdf = Hkdf::<Sha256>::new(Some(b"rust-clip-salt-v1"), &entropy);

        let mut key_bytes = [0u8; 32];
        hkdf.expand(b"ed25519_identity_key", &mut key_bytes)
            .map_err(|_| anyhow!("HKDF expansion failed for Identity Key"))?;

        let signing_key = SigningKey::from_bytes(&key_bytes);
        let verifying_key = signing_key.verifying_key();

        let mut root_secret = [0u8; 32];
        hkdf.expand(b"root_secret_v1", &mut root_secret)
            .map_err(|_| anyhow!("HKDF expansion failed for Root Secret"))?;

        let stable_peer_id = Self::compute_stable_peer_id(&verifying_key);

        Ok(RingIdentity {
            mnemonic: phrase.to_string(),
            identity_key: signing_key,
            public_key: verifying_key,
            root_secret,
            stable_peer_id,
        })
    }

    /// Generate a time-based rotating discovery ID.
    /// Format: HMAC(RootSecret, CurrentWindow) -> Truncated UUID-like string.
    /// Rotates every hour for privacy.
    pub fn get_rotating_id(&self) -> String {
        let now = Utc::now();
        let time_window = now.hour() as u64 + (now.day() as u64 * 24);
        let time_bytes = time_window.to_be_bytes();

        let mut mac = <HmacSha256 as Mac>::new_from_slice(&self.root_secret)
            .expect("HMAC can take key of any size");
        mac.update(b"discovery_id_rotation");
        mac.update(&time_bytes);

        let result = mac.finalize().into_bytes();

        // Take first 16 bytes to form a pseudo-random UUID
        let uuid_bytes: [u8; 16] = result[0..16].try_into().unwrap();
        let uuid = uuid::Builder::from_bytes(uuid_bytes).into_uuid();

        uuid.to_string()
    }

    /// Sign a message with the Ed25519 identity key.
    pub fn sign(&self, message: &[u8]) -> Signature {
        self.identity_key.sign(message)
    }

    /// Verify a signature against a public key.
    pub fn verify(public_key: &VerifyingKey, message: &[u8], signature: &Signature) -> Result<()> {
        public_key.verify(message, signature)
            .map_err(|e| anyhow!("Invalid signature: {}", e))
    }

    /// Generate an ephemeral X25519 keypair for session key exchange.
    pub fn generate_ephemeral_key() -> (EphemeralSecret, PublicKey) {
        let secret = EphemeralSecret::random_from_rng(thread_rng());
        let public = PublicKey::from(&secret);
        (secret, public)
    }

    /// Compute a ring membership proof for a given peer.
    /// Uses HMAC(root_secret, "ring-membership" || peer_stable_id).
    /// Only peers sharing the same mnemonic can produce matching proofs.
    pub fn ring_proof_for(&self, peer_stable_id: &str) -> Vec<u8> {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(&self.root_secret)
            .expect("HMAC accepts any key size");
        mac.update(b"ring-membership");
        mac.update(peer_stable_id.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }

    /// Verify that a peer's ring proof matches our computation.
    /// Returns true if the peer shares the same mnemonic (ring membership).
    pub fn verify_ring_proof(&self, peer_stable_id: &str, proof: &[u8]) -> bool {
        let expected = self.ring_proof_for(peer_stable_id);
        // Constant-time comparison
        expected.len() == proof.len()
            && expected
                .iter()
                .zip(proof.iter())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }

    /// Stable peer ID: SHA-256 of Ed25519 public key, truncated to 20 bytes, hex-encoded.
    /// Never changes for a given identity. Used for routing.
    pub fn stable_peer_id(&self) -> &str {
        &self.stable_peer_id
    }

    /// Compute stable peer ID from a verifying key
    pub fn compute_stable_peer_id(pubkey: &VerifyingKey) -> StablePeerId {
        use sha2::Digest;
        let hash = Sha256::digest(pubkey.as_bytes());
        hex::encode(&hash[..20])
    }

    /// Compute stable peer ID from raw public key bytes
    pub fn stable_peer_id_from_bytes(pubkey_bytes: &[u8]) -> Option<StablePeerId> {
        if pubkey_bytes.len() != 32 {
            return None;
        }
        let bytes: [u8; 32] = pubkey_bytes.try_into().ok()?;
        let vk = VerifyingKey::from_bytes(&bytes).ok()?;
        Some(Self::compute_stable_peer_id(&vk))
    }

    // --- Persistence (AES-GCM encrypted local store) ---

    fn get_machine_key() -> Result<[u8; 32]> {
        let machine_id = machine_uid::get()
            .map_err(|e| anyhow!("Failed to read machine ID: {}", e))?;
        
        let hkdf = Hkdf::<Sha256>::new(None, machine_id.as_bytes());
        let mut key = [0u8; 32];
        hkdf.expand(b"rustclip_storage_key_v2", &mut key)
            .map_err(|_| anyhow!("Key expansion failed"))?;
        
        Ok(key)
    }

    fn get_identity_path() -> Result<PathBuf> {
        let proj = ProjectDirs::from("com", "rustclip", "rust-clip")
            .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
        
        let config_dir = proj.config_dir();
        if !config_dir.exists() {
            fs::create_dir_all(config_dir)?;
        }
        
        Ok(config_dir.join("identity_v2.enc"))
    }

    pub fn save(&self) -> Result<()> {
        let stored = StoredIdentity { mnemonic: self.mnemonic.clone() };
        let json = serde_json::to_string(&stored)?;

        let key_bytes = Self::get_machine_key()?;
        let cipher = Aes256Gcm::new(&key_bytes.into());
        
        let mut nonce_bytes = [0u8; 12];
        thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher.encrypt(nonce, json.as_bytes())
            .map_err(|_| anyhow!("Encryption failed"))?;

        let mut file_content = Vec::new();
        file_content.extend_from_slice(&nonce_bytes);
        file_content.extend_from_slice(&ciphertext);

        let path = Self::get_identity_path()?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::write(&path, &file_content)?;
            let mut perms = fs::metadata(&path)?.permissions();
            perms.set_mode(0o600); 
            fs::set_permissions(&path, perms)?;
        }
        #[cfg(not(unix))]
        {
            fs::write(&path, &file_content)?;
        }

        println!("  [Identity] Saved to {:?}", path);
        Ok(())
    }

    pub fn load() -> Result<Self> {
        let path = Self::get_identity_path()?;
        
        if !path.exists() {
             return Err(anyhow!("No identity found at {:?}", path));
        }

        let file_content = fs::read(path)?;
        if file_content.len() < 12 {
            return Err(anyhow!("Corrupted identity file"));
        }

        let (nonce_bytes, ciphertext) = file_content.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);

        let key_bytes = Self::get_machine_key()?;
        let cipher = Aes256Gcm::new(&key_bytes.into());

        let plaintext = cipher.decrypt(nonce, ciphertext)
            .map_err(|_| anyhow!("Decryption failed (machine ID changed?)"))?;

        let stored: StoredIdentity = serde_json::from_slice(&plaintext)?;
        
        Self::from_mnemonic(&stored.mnemonic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MNEMONIC_A: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const MNEMONIC_B: &str = "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo wrong";

    fn id_a() -> RingIdentity {
        RingIdentity::from_mnemonic(MNEMONIC_A).unwrap()
    }

    fn id_b() -> RingIdentity {
        RingIdentity::from_mnemonic(MNEMONIC_B).unwrap()
    }

    #[test]
    fn deterministic_identity_from_mnemonic() {
        let a1 = id_a();
        let a2 = id_a();
        assert_eq!(a1.stable_peer_id(), a2.stable_peer_id());
        assert_eq!(a1.public_key.as_bytes(), a2.public_key.as_bytes());
    }

    #[test]
    fn different_mnemonics_different_identities() {
        let a = id_a();
        let b = id_b();
        assert_ne!(a.stable_peer_id(), b.stable_peer_id());
    }

    #[test]
    fn stable_peer_id_from_bytes() {
        let a = id_a();
        let computed = RingIdentity::stable_peer_id_from_bytes(a.public_key.as_bytes());
        assert_eq!(computed.unwrap(), a.stable_peer_id());
    }

    #[test]
    fn stable_peer_id_from_bytes_invalid() {
        assert!(RingIdentity::stable_peer_id_from_bytes(&[0u8; 16]).is_none()); // wrong length
    }

    #[test]
    fn sign_and_verify() {
        let a = id_a();
        let sig = a.sign(b"test message");
        assert!(RingIdentity::verify(&a.public_key, b"test message", &sig).is_ok());
        assert!(RingIdentity::verify(&a.public_key, b"wrong message", &sig).is_err());
    }

    #[test]
    fn rotating_id_deterministic_within_window() {
        let a1 = id_a();
        let a2 = id_a();
        // Same mnemonic, same time window → same rotating ID
        assert_eq!(a1.get_rotating_id(), a2.get_rotating_id());
    }

    #[test]
    fn rotating_id_different_mnemonics() {
        let a = id_a();
        let b = id_b();
        assert_ne!(a.get_rotating_id(), b.get_rotating_id());
    }

    #[test]
    fn ring_proof_same_mnemonic() {
        let a = id_a();
        // Same mnemonic peers can verify each other's ring proofs
        let proof = a.ring_proof_for(a.stable_peer_id());
        assert!(a.verify_ring_proof(a.stable_peer_id(), &proof));
    }

    #[test]
    fn ring_proof_different_mnemonic_fails() {
        let a = id_a();
        let b = id_b();

        // A produces proof using A's root_secret
        let proof_a = a.ring_proof_for(a.stable_peer_id());
        // B cannot verify it (different root_secret)
        assert!(!b.verify_ring_proof(a.stable_peer_id(), &proof_a));
    }

    #[test]
    fn ring_proof_wrong_peer_id_fails() {
        let a = id_a();
        let proof = a.ring_proof_for("wrong-peer-id");
        // Proof for wrong peer_id doesn't match
        assert!(!a.verify_ring_proof(a.stable_peer_id(), &proof));
    }
}