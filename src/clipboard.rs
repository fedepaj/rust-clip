use crate::identity::RingIdentity;
use crate::discovery::PeerMap;
use crate::crypto::CryptoLayer;
use anyhow::Result;
use arboard::Clipboard;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, Duration};
use std::sync::{Arc, Mutex};
use sha2::{Sha256, Digest};
use std::collections::HashSet;

// Struttura dei dati che inviamo (per ora solo Testo)
#[derive(serde::Serialize, serde::Deserialize, Debug)]
enum ClipContent {
    Text(String),
}

// Stato condiviso per evitare i loop (Hash check)
// Usiamo un Set di hash recenti per sicurezza
type RecentHashes = Arc<Mutex<HashSet<String>>>;

pub async fn start_clipboard_sync(identity: RingIdentity, peers: PeerMap) -> Result<()> {
    let crypto = Arc::new(CryptoLayer::new(&identity.shared_secret));
    let recent_hashes: RecentHashes = Arc::new(Mutex::new(HashSet::new()));

    // Lanciamo il SERVER (Receiver) in un task parallelo
    let server_crypto = crypto.clone();
    let server_hashes = recent_hashes.clone();
    tokio::spawn(async move {
        if let Err(e) = run_server(server_crypto, server_hashes).await {
            eprintln!("❌ Errore Server TCP: {}", e);
        }
    });

    // Lanciamo il MONITOR (Sender) nel task corrente
    run_monitor(crypto, peers, recent_hashes).await
}

// --- SENDER ---
async fn run_monitor(crypto: Arc<CryptoLayer>, peers: PeerMap, recent_hashes: RecentHashes) -> Result<()> {
    println!("📋 Monitor Clipboard Attivo...");
    
    // Inizializza clipboard
    let mut clipboard = Clipboard::new().map_err(|e| anyhow::anyhow!("Clip init fail: {}", e))?;
    let mut last_text = String::new();

    loop {
        // 1. Leggi clipboard locale
        // Nota: getText su alcuni OS può fallire se la clip è vuota o non testuale
        if let Ok(text) = clipboard.get_text() {
            if text != last_text && !text.is_empty() {
                
                // 2. Calcola Hash
                let hash = hash_string(&text);

                // 3. Controlla se l'abbiamo appena ricevuta noi dalla rete (Loop prevention)
                let is_from_network = {
                    let set = recent_hashes.lock().unwrap();
                    set.contains(&hash)
                };

                if !is_from_network {
                    println!("📝 Rilevata copia locale ({:.20}...). Invio ai peer...", text);
                    
                    // Aggiungiamo ai recenti per non rispedirla a noi stessi se torna indietro
                    {
                        let mut set = recent_hashes.lock().unwrap();
                        set.insert(hash.clone());
                        // Pulizia base (opzionale): se il set cresce troppo potremmo svuotarlo
                    }

                    // 4. Prepara e Cifra il pacchetto
                    let content = ClipContent::Text(text.clone());
                    let raw_bytes = bincode::serialize(&content)?;
                    let encrypted_bytes = crypto.encrypt(&raw_bytes)?;

                    // 5. Invia a tutti i peer conosciuti
                    // Iteriamo sulla DashMap
                    for item in peers.iter() {
                        let addr = item.value();
                        let peer_name = item.key();
                        
                        // Inviamo in background per non bloccare il loop
                        let data_to_send = encrypted_bytes.clone();
                        let addr_clone = *addr;
                        let name_clone = peer_name.clone();
                        
                        tokio::spawn(async move {
                            if let Err(e) = send_data(addr_clone, data_to_send).await {
                                eprintln!("⚠️  Invio fallito a {}: {}", name_clone, e);
                            } else {
                                // println!("-> Inviato a {}", name_clone);
                            }
                        });
                    }
                }
                last_text = text;
            }
        }
        sleep(Duration::from_millis(500)).await;
    }
}

async fn send_data(addr: std::net::SocketAddr, data: Vec<u8>) -> Result<()> {
    let mut stream = TcpStream::connect(addr).await?;
    
    // Protocollo: [LUNGHEZZA (4 byte u32)] + [DATI]
    let len = data.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&data).await?;
    
    Ok(())
}

// --- RECEIVER ---
async fn run_server(crypto: Arc<CryptoLayer>, recent_hashes: RecentHashes) -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:5566").await?;
    println!("📥 Server Clipboard in ascolto sulla porta 5566");

    loop {
        let (mut socket, addr) = listener.accept().await?;
        let crypto_ref = crypto.clone();
        let hashes_ref = recent_hashes.clone();

        tokio::spawn(async move {
            // 1. Leggi lunghezza
            let mut len_buf = [0u8; 4];
            if socket.read_exact(&mut len_buf).await.is_err() { return; }
            let len = u32::from_be_bytes(len_buf) as usize;

            // Protezione DoS: Limite dimensione (es. 10MB)
            if len > 10 * 1024 * 1024 { return; }

            // 2. Leggi pacchetto
            let mut buf = vec![0u8; len];
            if socket.read_exact(&mut buf).await.is_err() { return; }

            // 3. Decifra
            match crypto_ref.decrypt(&buf) {
                Ok(decrypted_data) => {
                    // 4. Deserializza
                    if let Ok(ClipContent::Text(text)) = bincode::deserialize(&decrypted_data) {
                        println!("📩 Ricevuto da {}: {:.20}...", addr, text);
                        
                        // 5. Aggiorna Hash per evitare loop
                        let hash = hash_string(&text);
                        {
                            let mut set = hashes_ref.lock().unwrap();
                            set.insert(hash);
                        }

                        // 6. Scrivi nella clipboard OS
                        // Arboard deve girare in un thread che supporta la clipboard (spesso main thread, ma qui proviamo spawn blocking)
                        // Su Linux/Windows va bene, su Mac potrebbe lamentarsi se non è main thread.
                        tokio::task::spawn_blocking(move || {
                            match Clipboard::new() {
                                Ok(mut cb) => {
                                    let _ = cb.set_text(text);
                                },
                                Err(e) => eprintln!("Errore accesso clipboard locale: {}", e),
                            }
                        }).await.ok();
                    }
                },
                Err(e) => {
                    eprintln!("⛔ Tentativo di intrusione da {}: {}", addr, e);
                }
            }
        });
    }
}

fn hash_string(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hex::encode(hasher.finalize())
}