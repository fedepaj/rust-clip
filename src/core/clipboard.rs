use crate::core::identity::RingIdentity;
use crate::core::discovery::PeerMap;
use crate::core::crypto::CryptoLayer;
use anyhow::Result;
use arboard::{Clipboard, ImageData};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, Duration};
use std::sync::{Arc, Mutex};
use sha2::{Sha256, Digest};
use std::collections::HashSet;
use std::borrow::Cow;
use image::ImageEncoder; // Per comprimere PNG

// Limite massimo pacchetto (50MB per gestire screenshot 4K compressi)
const MAX_PACKET_SIZE: usize = 50 * 1024 * 1024;

#[derive(serde::Serialize, serde::Deserialize, Debug)]
enum ClipContent {
    Text(String),
    // Inviamo il PNG compresso, non i pixel raw (troppo pesanti)
    Image(Vec<u8>), 
}

// Stato condiviso per evitare i loop
type RecentHashes = Arc<Mutex<HashSet<String>>>;

pub async fn start_clipboard_sync(identity: RingIdentity, peers: PeerMap) -> Result<()> {
    let crypto = Arc::new(CryptoLayer::new(&identity.shared_secret));
    let recent_hashes: RecentHashes = Arc::new(Mutex::new(HashSet::new()));

    // SERVER (Receiver)
    let server_crypto = crypto.clone();
    let server_hashes = recent_hashes.clone();
    tokio::spawn(async move {
        if let Err(e) = run_server(server_crypto, server_hashes).await {
            eprintln!("❌ Errore Server TCP: {}", e);
        }
    });

    // MONITOR (Sender)
    run_monitor(crypto, peers, recent_hashes).await
}

// --- SENDER ---
async fn run_monitor(crypto: Arc<CryptoLayer>, peers: PeerMap, recent_hashes: RecentHashes) -> Result<()> {
    println!("📋 Monitor Clipboard Attivo (Testo + Immagini)...");
    
    let mut clipboard = Clipboard::new().map_err(|e| anyhow::anyhow!("Clip init fail: {}", e))?;
    
    let mut last_text_hash = String::new();
    let mut last_image_hash = String::new();

    loop {
        let mut content_to_send: Option<ClipContent> = None;
        let mut content_hash = String::new();
        let mut has_text = false;

        // 1. CONTROLLO TESTO
        match clipboard.get_text() {
            Ok(text) => {
                has_text = true;
                let hash = hash_data(text.as_bytes());
                if hash != last_text_hash && !text.is_empty() {
                    let is_from_network = { recent_hashes.lock().unwrap().contains(&hash) };
                    if !is_from_network {
                        println!("📝 Copia rilevata: Testo ({:.20}...) - Hash: {:.8}", text, hash);
                        content_to_send = Some(ClipContent::Text(text));
                        content_hash = hash.clone();
                        last_text_hash = hash;
                        last_image_hash.clear();
                    } else {
                        last_text_hash = hash;
                    }
                }
            },
            Err(_) => { 
                // Ignoriamo errori sul testo (spesso succede se c'è un'immagine)
                has_text = false; 
            }
        }

        // 2. CONTROLLO IMMAGINE
        // Controlliamo l'immagine se non stiamo già inviando testo nuovo
        if content_to_send.is_none() {
            match clipboard.get_image() {
                Ok(img) => {
                    let hash = hash_data(&img.bytes);

                    if hash != last_image_hash {
                        let is_from_network = { recent_hashes.lock().unwrap().contains(&hash) };
                        
                        if !is_from_network {
                            println!("🖼️  Copia rilevata: Immagine {}x{} - Hash: {:.8}", img.width, img.height, hash);
                            
                            let width = img.width;
                            let height = img.height;
                            let bytes = img.bytes.into_owned();

                            let png_result = tokio::task::spawn_blocking(move || {
                                encode_to_png(width, height, &bytes)
                            }).await?;

                            match png_result {
                                Ok(png_bytes) => {
                                    println!("   Compressione OK: {} bytes. Invio...", png_bytes.len());
                                    content_to_send = Some(ClipContent::Image(png_bytes));
                                    content_hash = hash.clone();
                                    last_image_hash = hash;
                                    last_text_hash.clear();
                                },
                                Err(e) => eprintln!("❌ Errore compressione PNG: {}", e),
                            }
                        } else {
                            last_image_hash = hash;
                        }
                    }
                },
                Err(e) => {
                    // --- DEBUG WINDOWS ---
                    // Su Windows questo errore è comune se la clipboard è "Locked".
                    // Stampiamo l'errore SOLO se non è il classico "ContentNotAvailable" (cioè clipboard vuota)
                    let msg = format!("{}", e);
                    if !msg.contains("The clipboard is empty") && !msg.contains("specified format is not available") {
                         // Se c'è del testo, è normale che get_image fallisca, non stampiamo nulla
                         if !has_text {
                             eprintln!("⚠️  Debug Windows - Errore lettura Immagine: {}", msg);
                         }
                    }
                }
            }
        }

        // 3. INVIO
        if let Some(content) = content_to_send {
            {
                let mut set = recent_hashes.lock().unwrap();
                set.insert(content_hash);
            }

            match bincode::serialize(&content) {
                Ok(raw_bytes) => {
                     match crypto.encrypt(&raw_bytes) {
                        Ok(encrypted_bytes) => {
                            for item in peers.iter() {
                                let addr = *item.value();
                                let data = encrypted_bytes.clone();
                                let peer_name = item.key().clone();
                                tokio::spawn(async move {
                                    if let Err(e) = send_data(addr, data).await {
                                        eprintln!("⚠️  Invio fallito a {}: {}", peer_name, e);
                                    } else {
                                        println!("🚀 Inviato a {}", peer_name);
                                    }
                                });
                            }
                        },
                        Err(e) => eprintln!("❌ Errore Cifratura: {}", e),
                     }
                },
                Err(e) => eprintln!("❌ Errore Serializzazione: {}", e),
            }
        }

        sleep(Duration::from_millis(500)).await;
    }
}

async fn send_data(addr: std::net::SocketAddr, data: Vec<u8>) -> Result<()> {
    // Timeout breve per non bloccare
    let stream = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(addr)).await??;
    let mut stream = stream;
    
    let len = data.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&data).await?;
    Ok(())
}

// --- RECEIVER ---
async fn run_server(crypto: Arc<CryptoLayer>, recent_hashes: RecentHashes) -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:5566").await?;
    
    loop {
        let (mut socket, _) = listener.accept().await?;
        let crypto_ref = crypto.clone();
        let hashes_ref = recent_hashes.clone();

        tokio::spawn(async move {
            // 1. Leggi lunghezza
            let mut len_buf = [0u8; 4];
            if socket.read_exact(&mut len_buf).await.is_err() { return; }
            let len = u32::from_be_bytes(len_buf) as usize;

            if len > MAX_PACKET_SIZE { 
                eprintln!("⚠️ Pacchetto troppo grande scartato ({} bytes)", len);
                return; 
            }

            // 2. Leggi dati
            let mut buf = vec![0u8; len];
            if socket.read_exact(&mut buf).await.is_err() { return; }

            // 3. Decifra
            if let Ok(decrypted) = crypto_ref.decrypt(&buf) {
                // 4. Deserializza
                if let Ok(content) = bincode::deserialize::<ClipContent>(&decrypted) {
                    
                    // Operazione sulla clipboard (Blocking)
                    let _ = tokio::task::spawn_blocking(move || {
                        match Clipboard::new() {
                            Ok(mut cb) => {
                                match content {
                                    ClipContent::Text(text) => {
                                        let hash = hash_data(text.as_bytes());
                                        hashes_ref.lock().unwrap().insert(hash);
                                        
                                        println!("📩 Ricevuto Testo: {:.30}...", text);
                                        let _ = cb.set_text(text);
                                    },
                                    ClipContent::Image(png_bytes) => {
                                        println!("📩 Ricevuta Immagine ({} bytes PNG)", png_bytes.len());
                                        
                                        // Decomprimiamo PNG -> RAW
                                        if let Ok(image) = image::load_from_memory(&png_bytes) {
                                            let width = image.width() as usize;
                                            let height = image.height() as usize;
                                            let raw_bytes = image.to_rgba8().into_raw();
                                            
                                            // Calcoliamo l'hash dei dati RAW (come fa il Monitor)
                                            // Così se l'utente fa un check subito dopo, risulta "Gia visto"
                                            let hash = hash_data(&raw_bytes);
                                            hashes_ref.lock().unwrap().insert(hash);

                                            let img_data = ImageData {
                                                width,
                                                height,
                                                bytes: Cow::from(raw_bytes),
                                            };
                                            let _ = cb.set_image(img_data);
                                            println!("✅ Immagine impostata nella clipboard!");
                                        }
                                    }
                                }
                            },
                            Err(_) => {}
                        }
                    }).await;
                }
            }
        });
    }
}

// --- UTILS ---

// Helper per comprimere RAW -> PNG
fn encode_to_png(width: usize, height: usize, raw: &[u8]) -> Result<Vec<u8>> {
    let mut png_buffer = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png_buffer);
    
    encoder.write_image(
        raw, 
        width as u32, 
        height as u32, 
        image::ColorType::Rgba8
    )?;
    
    Ok(png_buffer)
}

fn hash_data(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}