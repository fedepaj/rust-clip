use anyhow::{Context, Result, anyhow};
use bip39::{Mnemonic, Language};
use rand::{RngCore, thread_rng};
use serde::{Serialize, Deserialize};
use std::fs;
use std::path::Path;
use sha2::{Sha256, Digest};
use hkdf::Hkdf;
use aes_gcm::{
    aead::{Aead, KeyInit}, // Rimossa Payload
    Aes256Gcm, Nonce 
};
use machine_uid;

const IDENTITY_FILE: &str = ".identity.enc"; 

#[derive(Clone, Debug)]
pub struct RingIdentity {
    pub mnemonic: String,
    // Chiavi derivate 
    pub discovery_id: String,     
    pub shared_secret: [u8; 32],  
}

#[derive(Serialize, Deserialize)]
struct StoredIdentity {
    mnemonic: String,
}

impl RingIdentity {
    // --- CREAZIONE ---
    pub fn create_new() -> Result<Self> {
        let mut entropy = [0u8; 32];
        thread_rng().fill_bytes(&mut entropy);

        let mnemonic = Mnemonic::from_entropy_in(Language::English, &entropy)?;
        let phrase = mnemonic.to_string();

        println!("\n=== NUOVO RING (SECURE) ===");
        println!("Parole segrete (Salvale altrove, questo file è vincolato a questo PC):");
        println!("-------------------------------------------------------");
        println!("{}", phrase);
        println!("-------------------------------------------------------\n");
        
        let identity = Self::from_mnemonic(&phrase)?;
        identity.save()?;
        Ok(identity)
    }

    pub fn from_mnemonic(phrase: &str) -> Result<Self> {
        let mnemonic = Mnemonic::parse_in_normalized(Language::English, phrase)
            .context("Parole non valide")?;
        
        let entropy = mnemonic.to_entropy(); 

        // 1. Deriviamo il Discovery ID (Pubblico)
        let hkdf = Hkdf::<Sha256>::new(None, &entropy);
        let mut discovery_bytes = [0u8; 32];
        hkdf.expand(b"rustclip_discovery_v1", &mut discovery_bytes)
            .map_err(|_| anyhow!("HKDF error"))?;
        
        let discovery_id = hex::encode(&discovery_bytes[0..16]);

        // 2. Deriviamo lo Shared Secret (Privato)
        let mut secret_bytes = [0u8; 32];
        hkdf.expand(b"rustclip_secret_v1", &mut secret_bytes)
            .map_err(|_| anyhow!("HKDF error"))?;

        Ok(RingIdentity {
            mnemonic: phrase.to_string(),
            discovery_id,
            shared_secret: secret_bytes,
        })
    }

    // --- STORAGE SICURO (OBFUSCATION) ---
    fn get_machine_key() -> Result<[u8; 32]> {
        // FIX: Usiamo map_err invece di context per gestire l'errore Box<dyn Error>
        let machine_id = machine_uid::get()
            .map_err(|e| anyhow!("Impossibile leggere Machine ID: {}", e))?;
        
        let hkdf = Hkdf::<Sha256>::new(None, machine_id.as_bytes());
        let mut key = [0u8; 32];
        hkdf.expand(b"rustclip_storage_key", &mut key)
            .map_err(|_| anyhow!("Key expansion failed"))?;
        
        Ok(key)
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
            .map_err(|_| anyhow!("Errore cifratura file"))?;

        let mut file_content = Vec::new();
        file_content.extend_from_slice(&nonce_bytes);
        file_content.extend_from_slice(&ciphertext);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Scriviamo prima il file, poi cambiamo i permessi
            fs::write(IDENTITY_FILE, &file_content)?;
            let mut perms = fs::metadata(IDENTITY_FILE)?.permissions();
            perms.set_mode(0o600); 
            fs::set_permissions(IDENTITY_FILE, perms)?;
        }
        #[cfg(not(unix))]
        {
            fs::write(IDENTITY_FILE, &file_content)?;
        }

        println!("🔒 Identità salvata e blindata su questo hardware ({})", IDENTITY_FILE);
        Ok(())
    }

    pub fn load() -> Result<Self> {
        if !Path::new(IDENTITY_FILE).exists() {
            return Err(anyhow!("Nessuna identità trovata."));
        }

        let file_content = fs::read(IDENTITY_FILE)?;
        if file_content.len() < 12 {
            return Err(anyhow!("File identità corrotto (troppo corto)"));
        }

        let (nonce_bytes, ciphertext) = file_content.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);

        let key_bytes = Self::get_machine_key()?;
        let cipher = Aes256Gcm::new(&key_bytes.into());

        let plaintext = cipher.decrypt(nonce, ciphertext)
            .map_err(|_| anyhow!("Decifrazione fallita! File corrotto o PC diverso."))?;

        let stored: StoredIdentity = serde_json::from_slice(&plaintext)?;
        
        Self::from_mnemonic(&stored.mnemonic)
    }

    pub fn get_derived_device_id() -> String {
        // Usa machine_uid per ottenere l'ID univoco dell'hardware
        let machine_id = machine_uid::get().unwrap_or_else(|_| "unknown_device".to_string());
        
        // Facciamo l'hash per anonimizzarlo e accorciarlo
        let mut hasher = Sha256::new();
        hasher.update(machine_id.as_bytes());
        let result = hasher.finalize();
        
        // Prendiamo i primi 2 byte (4 caratteri hex)
        hex::encode(&result[0..2])
    }
}