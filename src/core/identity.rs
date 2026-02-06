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

// Type alias per HMAC-SHA256
type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug)]
pub struct RingIdentity {
    pub mnemonic: String,
    pub identity_key: SigningKey, // Ed25519 Private Key
    pub public_key: VerifyingKey, // Ed25519 Public Key
    root_secret: [u8; 32],        // Derived from Mnemonic
}

#[derive(Serialize, Deserialize)]
struct StoredIdentity {
    mnemonic: String,
}

impl RingIdentity {
    /// Crea una nuova identità generando una mnemonica casuale
    pub fn create_new() -> Result<Self> {
        let mut entropy = [0u8; 32];
        thread_rng().fill_bytes(&mut entropy);

        let mnemonic = Mnemonic::from_entropy_in(Language::English, &entropy)?;
        let phrase = mnemonic.to_string();

        println!("🆕 Nuova Ring Identity Generata");
        
        let identity = Self::from_mnemonic(&phrase)?;
        identity.save()?;
        Ok(identity)
    }

    /// Ripristina l'identità da una mnemonica esistente
    pub fn from_mnemonic(phrase: &str) -> Result<Self> {
        let mnemonic = Mnemonic::parse_in_normalized(Language::English, phrase)
            .context("Parole non valide")?;
        
        // 1. Deriviamo la Root Seed (Entropy)
        let entropy = mnemonic.to_entropy(); 
        
        // 2. HKDF per derivare la chiave Ed25519 deterministica
        // Salt opzionale, usiamo una stringa costante per consistenza
        let hkdf = Hkdf::<Sha256>::new(Some(b"rust-clip-salt-v1"), &entropy);
        
        let mut key_bytes = [0u8; 32];
        hkdf.expand(b"ed25519_identity_key", &mut key_bytes)
            .map_err(|_| anyhow!("HKDF expansion failed for Identity Key"))?;

        let signing_key = SigningKey::from_bytes(&key_bytes);
        let verifying_key = signing_key.verifying_key();

        // 3. Root Secret per derivare altre sotto-chiavi (es. Discovery)
        let mut root_secret = [0u8; 32];
        hkdf.expand(b"root_secret_v1", &mut root_secret)
            .map_err(|_| anyhow!("HKDF expansion failed for Root Secret"))?;

        Ok(RingIdentity {
            mnemonic: phrase.to_string(),
            identity_key: signing_key,
            public_key: verifying_key,
            root_secret,
        })
    }

    /// Genera un Discovery ID rotante basato sull'ora corrente (Time-based Rotating ID)
    /// Format: HMAC(RootSecret, CurrentWindow) -> Truncated UUID-like string
    pub fn get_rotating_id(&self) -> String {
        let now = Utc::now();
        // Ruota ogni ora. Per maggiore privacy, potremmo fare ogni 15 min.
        // Usiamo l'ora corrente come "Message"
        let time_window = now.hour() as u64 + (now.day() as u64 * 24); 
        let time_bytes = time_window.to_be_bytes();

        let mut mac = <HmacSha256 as Mac>::new_from_slice(&self.root_secret)
            .expect("HMAC can take key of any size");
        mac.update(b"discovery_id_rotation");
        mac.update(&time_bytes);
        
        let result = mac.finalize().into_bytes();
        
        // Prendiamo i primi 16 byte per fare un UUID pseudo-casuale
        let uuid_bytes: [u8; 16] = result[0..16].try_into().unwrap();
        let uuid = uuid::Builder::from_bytes(uuid_bytes).into_uuid();
        
        uuid.to_string()
    }

    /// Firma un messaggio con la chiave Ed25519
    pub fn sign(&self, message: &[u8]) -> Signature {
        self.identity_key.sign(message)
    }

    /// Verifica una firma (static method utility)
    pub fn verify(public_key: &VerifyingKey, message: &[u8], signature: &Signature) -> Result<()> {
        public_key.verify(message, signature)
            .map_err(|e| anyhow!("Invalid signature: {}", e))
    }

    // --- PERSISTENZA (Cifratura AES-GCM del Local Store) ---

    fn get_machine_key() -> Result<[u8; 32]> {
        let machine_id = machine_uid::get()
            .map_err(|e| anyhow!("Impossibile leggere Machine ID: {}", e))?;
        
        let hkdf = Hkdf::<Sha256>::new(None, machine_id.as_bytes());
        let mut key = [0u8; 32];
        hkdf.expand(b"rustclip_storage_key_v2", &mut key)
            .map_err(|_| anyhow!("Key expansion failed"))?;
        
        Ok(key)
    }

    fn get_identity_path() -> Result<PathBuf> {
        let proj = ProjectDirs::from("com", "rustclip", "rust-clip")
            .ok_or_else(|| anyhow::anyhow!("Impossibile determinare cartella home"))?;
        
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

        println!("🔒 Identità salvata in {:?}", path);
        Ok(())
    }

    pub fn load() -> Result<Self> {
        let path = Self::get_identity_path()?;
        
        if !path.exists() {
            // Se non esiste la v2, proviamo a migrare o crearne una nuova
            // Per ora torniamo errore per forzare la creazione
             return Err(anyhow!("Nessuna identità trovata in {:?}", path));
        }

        let file_content = fs::read(path)?;
        if file_content.len() < 12 {
            return Err(anyhow!("File identità corrotto"));
        }

        let (nonce_bytes, ciphertext) = file_content.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);

        let key_bytes = Self::get_machine_key()?;
        let cipher = Aes256Gcm::new(&key_bytes.into());

        let plaintext = cipher.decrypt(nonce, ciphertext)
            .map_err(|_| anyhow!("Decifrazione fallita (password o machine id cambiati?)"))?;

        let stored: StoredIdentity = serde_json::from_slice(&plaintext)?;
        
        Self::from_mnemonic(&stored.mnemonic)
    }
}