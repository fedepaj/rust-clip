use crate::core::identity::RingIdentity;
use crate::core::discovery::PeerMap;
use crate::core::crypto::CryptoLayer;
use anyhow::Result;
use arboard::{Clipboard, ImageData};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, Duration};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering}; // <--- NUOVO
use sha2::{Sha256, Digest};
use std::collections::HashSet;
use std::borrow::Cow;
use image::ImageEncoder;

const MAX_PACKET_SIZE: usize = 50 * 1024 * 1024;

#[derive(serde::Serialize, serde::Deserialize, Debug)]
enum ClipContent {
    Text(String),
    Image(Vec<u8>), 
}

type RecentHashes = Arc<Mutex<HashSet<String>>>;

pub async fn start_clipboard_sync(identity: RingIdentity, peers: PeerMap) -> Result<()> {
    let crypto = Arc::new(CryptoLayer::new(&identity.shared_secret));
    let recent_hashes: RecentHashes = Arc::new(Mutex::new(HashSet::new()));

    // SEMAFORO: Se true, il Monitor smette di leggere per lasciare spazio al Server
    let busy_writing = Arc::new(AtomicBool::new(false));

    // SERVER (Receiver)
    let server_crypto = crypto.clone();
    let server_hashes = recent_hashes.clone();
    let server_busy = busy_writing.clone(); // Clone per il server
    
    tokio::spawn(async move {
        if let Err(e) = run_server(server_crypto, server_hashes, server_busy).await {
            eprintln!("❌ Errore Server TCP: {}", e);
        }
    });

    // MONITOR (Sender)
    run_monitor(crypto, peers, recent_hashes, busy_writing).await
}

// --- SENDER ---
async fn run_monitor(
    crypto: Arc<CryptoLayer>, 
    peers: PeerMap, 
    recent_hashes: RecentHashes,
    busy_writing: Arc<AtomicBool> // <--- Flag passato qui
) -> Result<()> {
    println!("📋 Monitor Clipboard Attivo...");
    
    // Inizializziamo la clipboard fuori dal loop per mantenerne il contesto (se possibile)
    // Su Windows arboard consiglia di ricrearla spesso se ci sono errori, ma proviamo a tenerla.
    let mut last_text_hash = String::new();
    let mut last_image_hash = String::new();

    loop {
        // 1. CONTROLLO SEMAFORO
        // Se il server sta scrivendo, noi saltiamo questo turno per non disturbare Windows
        if busy_writing.load(Ordering::Relaxed) {
            sleep(Duration::from_millis(200)).await;
            continue;
        }

        // Creiamo istanza fresca per evitare lock "stale" su Windows
        match Clipboard::new() {
            Ok(mut clipboard) => {
                let mut content_to_send: Option<ClipContent> = None;
                let mut content_hash = String::new();
                let mut has_text = false;

                // 2. CONTROLLO TESTO
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
                    Err(_) => { has_text = false; }
                }

                // 3. CONTROLLO IMMAGINE
                if content_to_send.is_none() {
                    // Nota: get_image su Windows può essere lento, facciamolo solo se non c'è testo
                    match clipboard.get_image() {
                        Ok(img) => {
                            let hash = hash_data(&img.bytes);
                            if hash != last_image_hash {
                                let is_from_network = { recent_hashes.lock().unwrap().contains(&hash) };
                                if !is_from_network {
                                    println!("🖼️  Copia rilevata: Immagine {}x{} - Hash: {:.8}", img.width, img.height, hash);
                                    
                                    // Spostiamo i dati necessari per il thread blocking
                                    let width = img.width;
                                    let height = img.height;
                                    let bytes = img.bytes.into_owned();

                                    let png_result = tokio::task::spawn_blocking(move || {
                                        encode_to_png(width, height, &bytes)
                                    }).await?;

                                    match png_result {
                                        Ok(png_bytes) => {
                                            println!("   Compresso PNG: {} bytes", png_bytes.len());
                                            content_to_send = Some(ClipContent::Image(png_bytes));
                                            content_hash = hash.clone();
                                            last_image_hash = hash;
                                            last_text_hash.clear();
                                        },
                                        Err(e) => eprintln!("❌ Err Compressione: {}", e),
                                    }
                                } else {
                                    last_image_hash = hash;
                                }
                            }
                        },
                        Err(e) => {
                            // Ignoriamo errori standard (clipboard vuota)
                            let msg = format!("{}", e);
                            if !msg.contains("empty") && !msg.contains("not available") && !has_text {
                                // Solo log molto verbose, commentabile
                                // eprintln!("Debug Img: {}", msg);
                            }
                        }
                    }
                }

                // 4. INVIO
                if let Some(content) = content_to_send {
                    {
                        let mut set = recent_hashes.lock().unwrap();
                        set.insert(content_hash);
                    }
                    if let Ok(raw) = bincode::serialize(&content) {
                        if let Ok(enc) = crypto.encrypt(&raw) {
                            for item in peers.iter() {
                                let addr = *item.value();
                                let data = enc.clone();
                                let name = item.key().clone();
                                tokio::spawn(async move {
                                    if let Err(_) = send_data(addr, data).await {
                                        // Silent fail
                                    } else {
                                        println!("🚀 Inviato a {}", name);
                                    }
                                });
                            }
                        }
                    }
                }
            },
            Err(_) => {
                // Errore apertura clipboard (magari lockata da altra app), riproviamo dopo
            }
        }

        sleep(Duration::from_millis(500)).await;
    }
}

async fn send_data(addr: std::net::SocketAddr, data: Vec<u8>) -> Result<()> {
    let stream = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(addr)).await??;
    let mut stream = stream;
    let len = data.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&data).await?;
    Ok(())
}

// --- RECEIVER ---
async fn run_server(
    crypto: Arc<CryptoLayer>, 
    recent_hashes: RecentHashes,
    busy_writing: Arc<AtomicBool> // <--- Flag ricevuto qui
) -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:5566").await?;
    
    loop {
        let (mut socket, _) = listener.accept().await?;
        let crypto_ref = crypto.clone();
        let hashes_ref = recent_hashes.clone();
        let busy_ref = busy_writing.clone();

        tokio::spawn(async move {
            let mut len_buf = [0u8; 4];
            if socket.read_exact(&mut len_buf).await.is_err() { return; }
            let len = u32::from_be_bytes(len_buf) as usize;
            if len > MAX_PACKET_SIZE { return; }

            let mut buf = vec![0u8; len];
            if socket.read_exact(&mut buf).await.is_err() { return; }

            if let Ok(decrypted) = crypto_ref.decrypt(&buf) {
                if let Ok(content) = bincode::deserialize::<ClipContent>(&decrypted) {
                    
                    // ALZIAMO IL SEMAFORO: "Sto scrivendo, Monitor stai fermo!"
                    busy_ref.store(true, Ordering::Relaxed);
                    
                    // Facciamo tutto in un thread blocking per non bloccare il runtime async
                    let _ = tokio::task::spawn_blocking(move || {
                        // Piccola pausa per assicurarsi che il Monitor abbia finito il suo ciclo corrente
                        std::thread::sleep(std::time::Duration::from_millis(50));

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
                                        println!("📩 Ricevuta Immagine ({} bytes)", png_bytes.len());
                                        
                                        if let Ok(image) = image::load_from_memory(&png_bytes) {
                                            let width = image.width() as usize;
                                            let height = image.height() as usize;
                                            let raw_bytes = image.to_rgba8().into_raw();
                                            
                                            // Aggiorna hash
                                            let hash = hash_data(&raw_bytes);
                                            hashes_ref.lock().unwrap().insert(hash);

                                            let img_data = ImageData {
                                                width,
                                                height,
                                                bytes: Cow::from(raw_bytes),
                                            };
                                            
                                            // SCRITTURA CRITICA
                                            if let Err(e) = cb.set_image(img_data) {
                                                eprintln!("❌ Errore SetClipboard: {}", e);
                                            } else {
                                                println!("✅ Immagine impostata!");
                                            }
                                        }
                                    }
                                }
                            },
                            Err(e) => eprintln!("❌ Errore Apertura Clip per Scrittura: {}", e),
                        }
                        
                        // Manteniamo il blocco ancora per un po' per lasciare che Windows processi i dati
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        
                        // ABBASSIAMO IL SEMAFORO
                        busy_ref.store(false, Ordering::Relaxed);

                    }).await;
                }
            }
        });
    }
}

// --- UTILS ---
fn encode_to_png(width: usize, height: usize, raw: &[u8]) -> Result<Vec<u8>> {
    let mut png_buffer = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png_buffer);
    encoder.write_image(raw, width as u32, height as u32, image::ColorType::Rgba8)?;
    Ok(png_buffer)
}

fn hash_data(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}