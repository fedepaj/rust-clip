use anyhow::{Context, Result, anyhow};
use bip39::{Mnemonic, Language};
use sha2::{Sha256, Digest};
use rand::{RngCore, thread_rng};
use serde::{Serialize, Deserialize};
use std::fs;
use std::path::Path;

const IDENTITY_FILE: &str = ".identity.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RingIdentity {
    pub mnemonic: String,
    pub ring_id: [u8; 32],     
}

impl RingIdentity {
    pub fn create_new() -> Result<Self> {
        let mut entropy = [0u8; 32];
        thread_rng().fill_bytes(&mut entropy);

        let mnemonic = Mnemonic::from_entropy_in(Language::English, &entropy)?;
        let phrase = mnemonic.to_string();

        println!("\n=== NUOVO RING CREATO ===");
        println!("Parole segrete (NON PERDERLE):");
        println!("-------------------------------------------------------");
        println!("{}", phrase);
        println!("-------------------------------------------------------\n");
        
        let identity = Self::from_mnemonic(&phrase)?;
        identity.save()?;
        Ok(identity)
    }

    pub fn load() -> Result<Self> {
        if !Path::new(IDENTITY_FILE).exists() {
            return Err(anyhow!("Nessuna identità trovata. Esegui 'rust-clip new' o 'rust-clip join'."));
        }
        let data = fs::read_to_string(IDENTITY_FILE).context("Errore lettura file")?;
        let identity: RingIdentity = serde_json::from_str(&data).context("File corrotto")?;
        Ok(identity)
    }

    pub fn from_mnemonic(phrase: &str) -> Result<Self> {
        let mnemonic = Mnemonic::parse_in_normalized(Language::English, phrase)
            .context("Parole non valide")?;
        let entropy = mnemonic.to_entropy();

        let mut hasher = Sha256::new();
        hasher.update(&entropy);
        let ring_id_full = hasher.finalize();

        Ok(RingIdentity {
            mnemonic: phrase.to_string(),
            ring_id: ring_id_full.into(),
        })
    }

    pub fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(IDENTITY_FILE, json)?;
        println!("💾 Identità salvata in '{}'", IDENTITY_FILE);
        Ok(())
    }

    // Helper per ottenere l'ID come stringa Hex (sicura per mDNS)
    pub fn get_ring_id_hex(&self) -> String {
        hex::encode(&self.ring_id[0..8])
    }
    
    // Helper se volessimo i bytes grezzi per il Bluetooth
    pub fn get_ble_magic_bytes(&self) -> [u8; 4] {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&self.ring_id[0..4]);
        bytes
    }
}