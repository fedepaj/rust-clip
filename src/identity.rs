use anyhow::{Context, Result};
use bip39::{Mnemonic, Language};
use sha2::{Sha256, Digest};
use rand::{RngCore, thread_rng}; // Importiamo il generatore di numeri casuali

#[derive(Clone, Debug)]
pub struct RingIdentity {
    pub mnemonic: String,
    pub ring_id: [u8; 32],     
}

impl RingIdentity {
    pub fn create_new() -> Result<Self> {
        // 1. Generiamo manualmente 32 byte di casualità (entropia)
        // 32 bytes = 256 bits = 24 parole
        let mut entropy = [0u8; 32];
        thread_rng().fill_bytes(&mut entropy);

        // 2. Creiamo il Mnemonic da questa entropia
        // Usa from_entropy_in per specificare la lingua
        let mnemonic = Mnemonic::from_entropy_in(Language::English, &entropy)?;
        
        let phrase = mnemonic.to_string(); // Ottieni la stringa delle parole

        println!("\n=== NUOVO RING CREATO ===");
        println!("Parole segrete (NON PERDERLE):");
        println!("-------------------------------------------------------");
        println!("{}", phrase);
        println!("-------------------------------------------------------\n");
        
        // Passiamo la stringa, non l'oggetto Mnemonic, alla funzione helper
        Self::from_mnemonic(&phrase)
    }

    pub fn from_mnemonic(phrase: &str) -> Result<Self> {
        // 3. Parsing delle parole esistenti
        // parse_in_normalized è il metodo corretto nella v2.0
        let mnemonic = Mnemonic::parse_in_normalized(Language::English, phrase)
            .context("Parole non valide! Controlla di averle scritte giuste.")?;
            
        // Otteniamo i byte originali (entropy) dalle parole
        let entropy = mnemonic.to_entropy();

        // 4. Deriviamo il Ring ID
        let mut hasher = Sha256::new();
        hasher.update(&entropy); // Usiamo l'entropia come seed
        let ring_id_full = hasher.finalize();

        Ok(RingIdentity {
            mnemonic: phrase.to_string(),
            ring_id: ring_id_full.into(),
        })
    }

    pub fn get_ble_magic_bytes(&self) -> [u8; 4] {
        let mut bytes = [0u8; 4];
        // Prendi i primi 4 byte dell'hash come identificativo
        bytes.copy_from_slice(&self.ring_id[0..4]);
        bytes
    }
}