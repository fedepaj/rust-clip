use anyhow::{Context, Result, anyhow};
use bip39::{Mnemonic, Language};
use rand::{RngCore, thread_rng};
use serde::{Serialize, Deserialize};
use std::fs;
use std::path::PathBuf;
use sha2::{Sha256, Digest};
use hkdf::Hkdf;
use aes_gcm::{
    aead::{Aead, KeyInit}, 
    Aes256Gcm, Nonce 
};
use machine_uid;
use directories::ProjectDirs;

#[derive(Clone, Debug)]
pub struct RingIdentity {
    pub mnemonic: String,
    pub discovery_id: String,     
    pub shared_secret: [u8; 32],  
}

#[derive(Serialize, Deserialize)]
struct StoredIdentity {
    mnemonic: String,
}

impl RingIdentity {
    pub fn create_new() -> Result<Self> {
        let mut entropy = [0u8; 32];
        thread_rng().fill_bytes(&mut entropy);

        let mnemonic = Mnemonic::from_entropy_in(Language::English, &entropy)?;
        let phrase = mnemonic.to_string();

        println!("Nuovo Ring Creato");
        
        let identity = Self::from_mnemonic(&phrase)?;
        identity.save()?;
        Ok(identity)
    }

    pub fn from_mnemonic(phrase: &str) -> Result<Self> {
        let mnemonic = Mnemonic::parse_in_normalized(Language::English, phrase)
            .context("Parole non valide")?;
        
        let entropy = mnemonic.to_entropy(); 

        let hkdf = Hkdf::<Sha256>::new(None, &entropy);
        let mut discovery_bytes = [0u8; 32];
        hkdf.expand(b"rustclip_discovery_v1", &mut discovery_bytes)
            .map_err(|_| anyhow!("HKDF error"))?;
        
        let discovery_id = hex::encode(&discovery_bytes[0..16]);

        let mut secret_bytes = [0u8; 32];
        hkdf.expand(b"rustclip_secret_v1", &mut secret_bytes)
            .map_err(|_| anyhow!("HKDF error"))?;

        Ok(RingIdentity {
            mnemonic: phrase.to_string(),
            discovery_id,
            shared_secret: secret_bytes,
        })
    }

    fn get_machine_key() -> Result<[u8; 32]> {
        let machine_id = machine_uid::get()
            .map_err(|e| anyhow!("Impossibile leggere Machine ID: {}", e))?;
        
        let hkdf = Hkdf::<Sha256>::new(None, machine_id.as_bytes());
        let mut key = [0u8; 32];
        hkdf.expand(b"rustclip_storage_key", &mut key)
            .map_err(|_| anyhow!("Key expansion failed"))?;
        
        Ok(key)
    }

    // Funzione per ottenere il percorso assoluto e stabile su tutti gli OS
    fn get_identity_path() -> Result<PathBuf> {
        let proj = ProjectDirs::from("com", "rustclip", "rust-clip")
            .ok_or_else(|| anyhow::anyhow!("Impossibile determinare cartella home"))?;
        
        let config_dir = proj.config_dir();
        if !config_dir.exists() {
            fs::create_dir_all(config_dir)?;
        }
        
        Ok(config_dir.join("identity.enc"))
    }

    // --- FUNZIONE CHE MANCAVA ---
    pub fn get_derived_device_id() -> String {
        let machine_id = machine_uid::get().unwrap_or_else(|_| "unknown_device".to_string());
        
        let mut hasher = Sha256::new();
        hasher.update(machine_id.as_bytes());
        let result = hasher.finalize();
        
        // Primi 4 caratteri hex dell'hash
        hex::encode(&result[0..2])
    }
    // ----------------------------

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
        println!("🔑 Identity Path: {:?}", path);

        if !path.exists() {
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
            .map_err(|_| anyhow!("Decifrazione fallita!"))?;

        let stored: StoredIdentity = serde_json::from_slice(&plaintext)?;
        
        Self::from_mnemonic(&stored.mnemonic)
    }
}