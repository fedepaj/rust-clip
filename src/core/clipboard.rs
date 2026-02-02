use crate::core::identity::RingIdentity;
use crate::core::discovery::PeerMap;
use crate::core::crypto::CryptoLayer;
use anyhow::Result;
use arboard::{Clipboard, ImageData};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, Duration};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use sha2::{Sha256, Digest};
use std::collections::HashSet;
use std::borrow::Cow;
use image::ImageEncoder;

const MAX_PACKET_SIZE: usize = 50 * 1024 * 1024; // 50MB

#[derive(serde::Serialize, serde::Deserialize, Debug)]
enum ClipContent {
    Text(String),
    Image(Vec<u8>), 
}

type RecentHashes = Arc<Mutex<HashSet<String>>>;

pub async fn start_clipboard_sync(
    identity: RingIdentity, 
    peers: PeerMap,
    global_pause: Arc<AtomicBool> // <--- NUOVO PARAMETRO
) -> Result<()> {
    let crypto = Arc::new(CryptoLayer::new(&identity.shared_secret));
    let recent_hashes: RecentHashes = Arc::new(Mutex::new(HashSet::new()));
    let busy_writing = Arc::new(AtomicBool::new(false));

    // SERVER
    let server_crypto = crypto.clone();
    let server_hashes = recent_hashes.clone();
    let server_busy = busy_writing.clone();
    
    tokio::spawn(async move {
        if let Err(e) = run_server(server_crypto, server_hashes, server_busy).await {
            eprintln!("❌ Errore Server TCP: {}", e);
        }
    });

    // MONITOR (Passiamo anche global_pause)
    run_monitor(crypto, peers, recent_hashes, busy_writing, global_pause).await
}

// --- SENDER ---
async fn run_monitor(
    crypto: Arc<CryptoLayer>, 
    peers: PeerMap, 
    recent_hashes: RecentHashes,
    busy_writing: Arc<AtomicBool>,
    global_pause: Arc<AtomicBool> // <--- NUOVO
) -> Result<()>  {
    println!("📋 Monitor Clipboard Attivo...");
    
    let mut last_text_hash = String::new();
    let mut last_image_hash = String::new();

    loop {
        sleep(Duration::from_millis(500)).await;

        // 0. CONTROLLO PAUSA GLOBALE (GUI)
        if global_pause.load(Ordering::Relaxed) {
            // Se siamo in pausa, saltiamo tutto il ciclo
            continue; 
        }

        // 1. CONTROLLO SEMAFORO (Scrittura in corso)
        if busy_writing.load(Ordering::Relaxed) {
            continue;
        }

        // 2. LETTURA CLIPBOARD (In un thread separato/blocking per sicurezza OS)
        // Questo task restituisce: Option<(TipoContenuto, Hash, DatiGrezziOpzionali)>
        let read_result = tokio::task::spawn_blocking(move || {
            // Creiamo istanza fresca ogni volta per evitare thread-affinity issues
            let mut clipboard = match Clipboard::new() {
                Ok(c) => c,
                Err(_) => return None, // Errore apertura, riprova prossimo giro
            };

            // Prima proviamo Testo
            if let Ok(text) = clipboard.get_text() {
                if !text.is_empty() {
                    let hash = hash_data(text.as_bytes());
                    return Some(("text", hash, Some(ClipContent::Text(text))));
                }
            }

            // Poi proviamo Immagine
            if let Ok(img) = clipboard.get_image() {
                let hash = hash_data(&img.bytes);
                // Ritorniamo l'immagine raw per comprimerla fuori se serve
                // Convertiamo ImageData in owned data per passarlo tra thread
                let width = img.width;
                let height = img.height;
                let bytes = img.bytes.into_owned();
                return Some(("image", hash, Some(ClipContent::Image(encode_raw(width, height, bytes))))); 
                // Nota: usiamo una variante temporanea, la compressione vera la facciamo dopo
            }

            None
        }).await?;

        // 3. LOGICA DI INVIO
        if let Some((kind, hash, content_wrapper)) = read_result {
            match kind {
                "text" => {
                    if hash != last_text_hash {
                        let is_new = { !recent_hashes.lock().unwrap().contains(&hash) };
                        if is_new {
                            println!("📝 Testo rilevato -> Invio...");
                            last_text_hash = hash.clone();
                            last_image_hash.clear();
                            // Invia
                            if let Some(c) = content_wrapper { broadcast(c, hash, &crypto, &peers, &recent_hashes).await; }
                        } else {
                            last_text_hash = hash;
                        }
                    }
                },
                "image" => {
                    if hash != last_image_hash {
                        let is_new = { !recent_hashes.lock().unwrap().contains(&hash) };
                        if is_new {
                            println!("🖼️  Immagine rilevata -> Comprimo e Invio...");
                            
                            // Estraiamo i dati grezzi dal wrapper temporaneo
                            if let Some(ClipContent::Image(raw_data_fake)) = content_wrapper {
                                // Qui facciamo la vera compressione PNG (CPU heavy)
                                // raw_data_fake contiene [width, height, pixels...]
                                let (w, h, pixels) = decode_raw(raw_data_fake);
                                
                                let png_res = tokio::task::spawn_blocking(move || {
                                    encode_to_png(w, h, &pixels)
                                }).await?;

                                if let Ok(png_bytes) = png_res {
                                    println!("   PNG Compresso: {} bytes", png_bytes.len());
                                    last_image_hash = hash.clone();
                                    last_text_hash.clear();
                                    broadcast(ClipContent::Image(png_bytes), hash, &crypto, &peers, &recent_hashes).await;
                                }
                            }
                        } else {
                            last_image_hash = hash;
                        }
                    }
                },
                _ => {}
            }
        }
    }
}

// Funzione helper per inviare
async fn broadcast(
    content: ClipContent, 
    hash: String, 
    crypto: &CryptoLayer, 
    peers: &PeerMap, 
    hashes: &RecentHashes
) {
    // 1. Aggiungi hash ai recenti
    hashes.lock().unwrap().insert(hash);

    // 2. Serializza e Cifra
    let raw = match bincode::serialize(&content) {
        Ok(r) => r,
        Err(_) => return,
    };
    let enc = match crypto.encrypt(&raw) {
        Ok(e) => e,
        Err(_) => return,
    };

    // 3. Invia
    for item in peers.iter() {
        let addr = *item.value();
        let data = enc.clone();
        let name = item.key().clone();
        tokio::spawn(async move {
            if let Err(e) = send_data(addr, data).await {
                eprintln!("⚠️  Fail send to {}: {}", name, e);
            } else {
                println!("🚀 Sent to {}", name);
            }
        });
    }
}

async fn send_data(addr: std::net::SocketAddr, data: Vec<u8>) -> Result<()> {
    let stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await??;
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
    busy_writing: Arc<AtomicBool>
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
                    
                    busy_ref.store(true, Ordering::Relaxed);
                    
                    let _ = tokio::task::spawn_blocking(move || {
                        // Pausa tattica
                        std::thread::sleep(std::time::Duration::from_millis(100));

                        match Clipboard::new() {
                            Ok(mut cb) => {
                                match content {
                                    ClipContent::Text(text) => {
                                        let hash = hash_data(text.as_bytes());
                                        hashes_ref.lock().unwrap().insert(hash);
                                        println!("📩 RX Testo: {:.20}...", text);
                                        let _ = cb.set_text(text);
                                    },
                                    ClipContent::Image(png_bytes) => {
                                        println!("📩 RX Immagine ({} b)", png_bytes.len());
                                        if let Ok(image) = image::load_from_memory(&png_bytes) {
                                            let w = image.width() as usize;
                                            let h = image.height() as usize;
                                            let raw = image.to_rgba8().into_raw();
                                            
                                            let hash = hash_data(&raw);
                                            hashes_ref.lock().unwrap().insert(hash);

                                            let img_data = ImageData { width: w, height: h, bytes: Cow::from(raw) };
                                            if let Err(e) = cb.set_image(img_data) {
                                                eprintln!("❌ Err Write Clip: {}", e);
                                            } else {
                                                println!("✅ Immagine incollata!");
                                            }
                                        }
                                    }
                                }
                            },
                            Err(e) => eprintln!("❌ Err Open Clip Server: {}", e),
                        }
                        
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        busy_ref.store(false, Ordering::Relaxed);
                    }).await;
                }
            }
        });
    }
}

// --- UTILS (Serializzazione "Finta" per passare raw data tra thread) ---
// Poiché non possiamo passare ImageData tra thread facilmente senza lifetime issues,
// usiamo un trucco: serializziamo width, height e bytes in un Vec<u8> temporaneo.

fn encode_raw(w: usize, h: usize, bytes: Vec<u8>) -> Vec<u8> {
    // Format: [w (8b)][h (8b)][bytes...]
    let mut v = Vec::with_capacity(16 + bytes.len());
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.extend_from_slice(&bytes);
    v
}

fn decode_raw(v: Vec<u8>) -> (usize, usize, Vec<u8>) {
    let (w_bytes, rest) = v.split_at(8);
    let (h_bytes, pixels) = rest.split_at(8);
    let w = usize::from_be_bytes(w_bytes.try_into().unwrap());
    let h = usize::from_be_bytes(h_bytes.try_into().unwrap());
    (w, h, pixels.to_vec())
}

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