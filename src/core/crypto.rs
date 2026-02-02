use anyhow::{Result, anyhow};
use chacha20poly1305::{
    XChaCha20Poly1305, Key, XNonce, 
    aead::{Aead, KeyInit}
};
use rand::{RngCore, thread_rng};
use serde::{Serialize, Deserialize};
use std::time::{SystemTime, UNIX_EPOCH};

// Validità del pacchetto (es. 60 secondi) per evitare Replay Attacks
const REPLAY_WINDOW_SECONDS: u64 = 60;

#[derive(Serialize, Deserialize)]
struct SecurePayload {
    timestamp: u64,
    data: Vec<u8>,
}

pub struct CryptoLayer {
    cipher: XChaCha20Poly1305,
}

impl CryptoLayer {
    pub fn new(shared_secret: &[u8; 32]) -> Self {
        let key = Key::from_slice(shared_secret);
        let cipher = XChaCha20Poly1305::new(key);
        Self { cipher }
    }

    /// Cifra i dati aggiungendo Timestamp e Nonce casuale
    /// Output format: [NONCE (24b)] + [CIPHERTEXT (Variabile)]
    pub fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        // 1. Prepara il payload con timestamp
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let payload = SecurePayload {
            timestamp: now,
            data: data.to_vec(),
        };
        let payload_bytes = bincode::serialize(&payload)?;

        // 2. Genera Nonce (24 bytes per XChaCha20)
        let mut nonce_bytes = [0u8; 24];
        thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = XNonce::from_slice(&nonce_bytes);

        // 3. Cifra
        let ciphertext = self.cipher.encrypt(nonce, payload_bytes.as_slice())
            .map_err(|e| anyhow!("Errore cifratura: {}", e))?;

        // 4. Concatena [Nonce + Ciphertext]
        let mut final_packet = Vec::with_capacity(24 + ciphertext.len());
        final_packet.extend_from_slice(&nonce_bytes);
        final_packet.extend_from_slice(&ciphertext);

        Ok(final_packet)
    }

    /// Decifra e valida Timestamp e Integrità
    pub fn decrypt(&self, packet: &[u8]) -> Result<Vec<u8>> {
        if packet.len() < 24 {
            return Err(anyhow!("Pacchetto troppo corto"));
        }

        // 1. Estrai Nonce
        let (nonce_bytes, ciphertext) = packet.split_at(24);
        let nonce = XNonce::from_slice(nonce_bytes);

        // 2. Decifra
        let plaintext = self.cipher.decrypt(nonce, ciphertext)
            .map_err(|_| anyhow!("Decifrazione fallita (Chiave errata o pacchetto manomesso)"))?;

        // 3. Deserializza e controlla Timestamp
        let payload: SecurePayload = bincode::deserialize(&plaintext)?;
        
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        
        // Controllo Anti-Replay: Accettiamo solo messaggi recenti
        if payload.timestamp > now + 5 || payload.timestamp < now - REPLAY_WINDOW_SECONDS {
            return Err(anyhow!("Pacchetto scartato: Timestamp non valido (Replay Attack o orologio disallineato)"));
        }

        Ok(payload.data)
    }
}