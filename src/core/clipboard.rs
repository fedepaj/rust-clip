// use crate::core::identity::RingIdentity;
// use crate::core::discovery::PeerMap;
// use crate::core::crypto::CryptoLayer;
// use crate::core::config::AppConfig;
// use anyhow::Result;
// use arboard::{Clipboard, ImageData};
// use tokio::io::{AsyncReadExt, AsyncWriteExt}; // AsyncWriteExt serve per stream.write_all
// use tokio::net::{TcpListener, TcpStream};    // TcpStream serve per connect
// use tokio::time::{sleep, Duration};
// use std::sync::{Arc, Mutex};
// use std::sync::atomic::{AtomicBool, Ordering};
// use sha2::{Sha256, Digest};
// use std::collections::HashSet;
// use std::borrow::Cow;
// use image::ImageEncoder;
// use notify_rust::Notification;

// const MAX_PACKET_SIZE: usize = 50 * 1024 * 1024;

// #[derive(serde::Serialize, serde::Deserialize, Debug)]
// enum ClipContent {
//     Text(String),
//     Image(Vec<u8>), 
// }

// type RecentHashes = Arc<Mutex<HashSet<String>>>;

// pub async fn start_clipboard_sync(
//     identity: RingIdentity, 
//     peers: PeerMap,
//     config: AppConfig,
//     global_pause: Arc<AtomicBool>
// ) -> Result<()> {
//     let crypto = Arc::new(CryptoLayer::new(&identity.shared_secret));
//     let recent_hashes: RecentHashes = Arc::new(Mutex::new(HashSet::new()));
//     let busy_writing = Arc::new(AtomicBool::new(false));

//     let server_crypto = crypto.clone();
//     let server_hashes = recent_hashes.clone();
//     let server_busy = busy_writing.clone();
//     let server_config = config.clone();
    
//     tokio::spawn(async move {
//         if let Err(e) = run_server(server_crypto, server_hashes, server_busy, server_config).await {
//             eprintln!("❌ Errore Server TCP: {}", e);
//         }
//     });

//     run_monitor(crypto, peers, recent_hashes, busy_writing, global_pause).await
// }

// async fn run_monitor(
//     crypto: Arc<CryptoLayer>, 
//     peers: PeerMap, 
//     recent_hashes: RecentHashes,
//     busy_writing: Arc<AtomicBool>,
//     global_pause: Arc<AtomicBool>
// ) -> Result<()> {
//     println!("📋 Monitor Clipboard Attivo...");
    
//     let mut last_text_hash = String::new();
//     let mut last_image_hash = String::new();

//     loop {
//         sleep(Duration::from_millis(500)).await;

//         if global_pause.load(Ordering::Relaxed) || busy_writing.load(Ordering::Relaxed) {
//             continue;
//         }

//         let read_result = tokio::task::spawn_blocking(move || {
//              let mut clipboard = match Clipboard::new() { Ok(c) => c, Err(_) => return None };
//              if let Ok(text) = clipboard.get_text() {
//                  if !text.is_empty() {
//                      let hash = hash_data(text.as_bytes());
//                      return Some(("text", hash, Some(ClipContent::Text(text))));
//                  }
//              }
//              if let Ok(img) = clipboard.get_image() {
//                  let hash = hash_data(&img.bytes);
//                  let width = img.width; let height = img.height; let bytes = img.bytes.into_owned();
//                  return Some(("image", hash, Some(ClipContent::Image(encode_raw(width, height, bytes))))); 
//              }
//              None
//         }).await?;

//         if let Some((kind, hash, content_wrapper)) = read_result {
//             match kind {
//                 "text" => {
//                     if hash != last_text_hash {
//                         let is_new = { !recent_hashes.lock().unwrap().contains(&hash) };
//                         if is_new {
//                             println!("📝 Testo rilevato -> Invio...");
//                             last_text_hash = hash.clone();
//                             last_image_hash.clear();
//                             if let Some(c) = content_wrapper { broadcast(c, hash, &crypto, &peers, &recent_hashes).await; }
//                         } else { last_text_hash = hash; }
//                     }
//                 },
//                 "image" => {
//                     if hash != last_image_hash {
//                         let is_new = { !recent_hashes.lock().unwrap().contains(&hash) };
//                         if is_new {
//                             println!("🖼️  Immagine rilevata -> Comprimo e Invio...");
//                             if let Some(ClipContent::Image(raw_data_fake)) = content_wrapper {
//                                 let (w, h, pixels) = decode_raw(raw_data_fake);
//                                 let png_res = tokio::task::spawn_blocking(move || encode_to_png(w, h, &pixels)).await?;
//                                 if let Ok(png_bytes) = png_res {
//                                     println!("   PNG: {} bytes", png_bytes.len());
//                                     last_image_hash = hash.clone();
//                                     last_text_hash.clear();
//                                     broadcast(ClipContent::Image(png_bytes), hash, &crypto, &peers, &recent_hashes).await;
//                                 }
//                             }
//                         } else { last_image_hash = hash; }
//                     }
//                 },
//                 _ => {}
//             }
//         }
//     }
// }

// async fn run_server(
//     crypto: Arc<CryptoLayer>, 
//     recent_hashes: RecentHashes,
//     busy_writing: Arc<AtomicBool>,
//     config: AppConfig
// ) -> Result<()> {
//     let listener = TcpListener::bind("0.0.0.0:5566").await?;
    
//     loop {
//         let (mut socket, _) = listener.accept().await?;
//         let crypto_ref = crypto.clone();
//         let hashes_ref = recent_hashes.clone();
//         let busy_ref = busy_writing.clone();
//         let config_ref = config.clone();

//         tokio::spawn(async move {
//             let mut len_buf = [0u8; 4];
//             if socket.read_exact(&mut len_buf).await.is_err() { return; }
//             let len = u32::from_be_bytes(len_buf) as usize;
//             if len > MAX_PACKET_SIZE { return; }

//             let mut buf = vec![0u8; len];
//             if socket.read_exact(&mut buf).await.is_err() { return; }

//             if let Ok(decrypted) = crypto_ref.decrypt(&buf) {
//                 if let Ok(content) = bincode::deserialize::<ClipContent>(&decrypted) {
                    
//                     busy_ref.store(true, Ordering::Relaxed);
                    
//                     let _ = tokio::task::spawn_blocking(move || {
//                         std::thread::sleep(std::time::Duration::from_millis(100));

//                         match Clipboard::new() {
//                             Ok(mut cb) => {
//                                 match content {
//                                     ClipContent::Text(text) => {
//                                         let hash = hash_data(text.as_bytes());
//                                         hashes_ref.lock().unwrap().insert(hash);
//                                         println!("📩 RX Testo: {:.20}...", text);
//                                         let _ = cb.set_text(text);
//                                         if config_ref.notifications_enabled {
//                                             let _ = Notification::new().summary("RustClip").body("📋 Testo copiato").show();
//                                         }
//                                     },
//                                     ClipContent::Image(png_bytes) => {
//                                         println!("📩 RX Immagine ({} b)", png_bytes.len());
//                                         if let Ok(image) = image::load_from_memory(&png_bytes) {
//                                             let w = image.width() as usize;
//                                             let h = image.height() as usize;
//                                             let raw = image.to_rgba8().into_raw();
//                                             let hash = hash_data(&raw);
//                                             hashes_ref.lock().unwrap().insert(hash);
//                                             let img_data = ImageData { width: w, height: h, bytes: Cow::from(raw) };
//                                             if let Err(e) = cb.set_image(img_data) {
//                                                 eprintln!("❌ Err Write Clip: {}", e);
//                                             } else {
//                                                 println!("✅ Immagine incollata!");
//                                                 if config_ref.notifications_enabled {
//                                                     let _ = Notification::new().summary("RustClip").body("🖼️ Immagine ricevuta").show();
//                                                 }
//                                             }
//                                         }
//                                     }
//                                 }
//                             },
//                             Err(e) => eprintln!("❌ Err Open Clip Server: {}", e),
//                         }
                        
//                         std::thread::sleep(std::time::Duration::from_millis(500));
//                         busy_ref.store(false, Ordering::Relaxed);
//                     }).await;
//                 }
//             }
//         });
//     }
// }

// // --- UTILS ---

// async fn broadcast(
//     content: ClipContent, 
//     hash: String, 
//     crypto: &CryptoLayer, 
//     peers: &PeerMap, 
//     hashes: &RecentHashes
// ) {
//     hashes.lock().unwrap().insert(hash);
//     let raw = match bincode::serialize(&content) { Ok(r) => r, Err(_) => return };
//     let enc = match crypto.encrypt(&raw) { Ok(e) => e, Err(_) => return };
//     for item in peers.iter() {
//         let addr = *item.value();
//         let data = enc.clone();
//         let name = item.key().clone();
//         tokio::spawn(async move {
//             // FIX: Ora send_data è definita qui sotto
//             if let Err(_) = send_data(addr, data).await {} else { println!("🚀 Sent to {}", name); }
//         });
//     }
// }

// // FIX: Ecco la funzione che mancava
// async fn send_data(addr: std::net::SocketAddr, data: Vec<u8>) -> Result<()> {
//     let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await??;
//     let len = data.len() as u32;
//     stream.write_all(&len.to_be_bytes()).await?;
//     stream.write_all(&data).await?;
//     Ok(())
// }

// fn encode_raw(w: usize, h: usize, bytes: Vec<u8>) -> Vec<u8> {
//     let mut v = Vec::with_capacity(16 + bytes.len());
//     v.extend_from_slice(&w.to_be_bytes());
//     v.extend_from_slice(&h.to_be_bytes());
//     v.extend_from_slice(&bytes);
//     v
// }

// fn decode_raw(v: Vec<u8>) -> (usize, usize, Vec<u8>) {
//     let (w_bytes, rest) = v.split_at(8);
//     let (h_bytes, pixels) = rest.split_at(8);
//     let w = usize::from_be_bytes(w_bytes.try_into().unwrap());
//     let h = usize::from_be_bytes(h_bytes.try_into().unwrap());
//     (w, h, pixels.to_vec())
// }

// fn encode_to_png(width: usize, height: usize, raw: &[u8]) -> Result<Vec<u8>> {
//     let mut png_buffer = Vec::new();
//     let encoder = image::codecs::png::PngEncoder::new(&mut png_buffer);
//     encoder.write_image(raw, width as u32, height as u32, image::ColorType::Rgba8)?;
//     Ok(png_buffer)
// }

// fn hash_data(data: &[u8]) -> String {
//     let mut hasher = Sha256::new();
//     hasher.update(data);
//     hex::encode(hasher.finalize())
// }
use crate::core::identity::RingIdentity;
use crate::core::discovery::PeerMap;
use crate::core::crypto::CryptoLayer;
use crate::core::config::AppConfig;
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
use notify_rust::Notification;

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
    config: AppConfig,
    global_pause: Arc<AtomicBool>
) -> Result<()> {
    let crypto = Arc::new(CryptoLayer::new(&identity.shared_secret));
    let recent_hashes: RecentHashes = Arc::new(Mutex::new(HashSet::new()));
    
    // Semaforo per evitare che il Monitor legga mentre il Server sta scrivendo (fondamentale su Windows)
    let busy_writing = Arc::new(AtomicBool::new(false));

    // Avvio del SERVER (Ricevitore)
    let server_crypto = crypto.clone();
    let server_hashes = recent_hashes.clone();
    let server_busy = busy_writing.clone();
    let server_config = config.clone();
    
    tokio::spawn(async move {
        if let Err(e) = run_server(server_crypto, server_hashes, server_busy, server_config).await {
            eprintln!("❌ Errore Server TCP: {}", e);
        }
    });

    // Avvio del MONITOR (Mittente)
    run_monitor(crypto, peers, recent_hashes, busy_writing, global_pause).await
}

// --- MONITOR (SENDER) ---
async fn run_monitor(
    crypto: Arc<CryptoLayer>, 
    peers: PeerMap, 
    recent_hashes: RecentHashes,
    busy_writing: Arc<AtomicBool>,
    global_pause: Arc<AtomicBool>
) -> Result<()> {
    println!("📋 Monitor Clipboard Attivo...");
    
    let mut last_text_hash = String::new();
    let mut last_image_hash = String::new();

    loop {
        sleep(Duration::from_millis(500)).await;

        // Se l'utente ha messo in pausa o il server sta scrivendo, saltiamo il ciclo
        if global_pause.load(Ordering::Relaxed) || busy_writing.load(Ordering::Relaxed) {
            continue;
        }

        // Lettura della clipboard in un thread dedicato per evitare blocchi async/OS
        let read_result = tokio::task::spawn_blocking(move || {
             let mut clipboard = match Clipboard::new() { Ok(c) => c, Err(_) => return None };

             // 1. Prova a leggere testo
             if let Ok(text) = clipboard.get_text() {
                 if !text.is_empty() {
                     let hash = hash_data(text.as_bytes());
                     return Some(("text", hash, Some(ClipContent::Text(text))));
                 }
             }

             // 2. Se non c'è testo, prova a leggere immagine
             if let Ok(img) = clipboard.get_image() {
                 let hash = hash_data(&img.bytes);
                 let width = img.width; 
                 let height = img.height; 
                 let bytes = img.bytes.into_owned();
                 // Usiamo un wrapper temporaneo per trasportare i dati raw fuori dal thread
                 return Some(("image", hash, Some(ClipContent::Image(encode_raw_buffer(width, height, bytes))))); 
             }

             None
        }).await?;

        // Gestione del contenuto rilevato
        if let Some((kind, hash, content_wrapper)) = read_result {
            match kind {
                "text" => {
                    if hash != last_text_hash {
                        let is_new = { !recent_hashes.lock().unwrap().contains(&hash) };
                        if is_new {
                            println!("📝 Testo locale rilevato -> Invio...");
                            last_text_hash = hash.clone();
                            last_image_hash.clear();
                            if let Some(c) = content_wrapper { 
                                broadcast(c, hash, &crypto, &peers, &recent_hashes).await; 
                            }
                        } else { last_text_hash = hash; }
                    }
                },
                "image" => {
                    if hash != last_image_hash {
                        let is_new = { !recent_hashes.lock().unwrap().contains(&hash) };
                        if is_new {
                            println!("🖼️  Immagine locale rilevata -> Compressione e Invio...");
                            if let Some(ClipContent::Image(raw_data_packed)) = content_wrapper {
                                let (w, h, pixels) = decode_raw_buffer(raw_data_packed);
                                
                                // Compressione PNG (operazione pesante)
                                let png_res = tokio::task::spawn_blocking(move || encode_to_png(w, h, &pixels)).await?;
                                
                                if let Ok(png_bytes) = png_res {
                                    last_image_hash = hash.clone();
                                    last_text_hash.clear();
                                    broadcast(ClipContent::Image(png_bytes), hash, &crypto, &peers, &recent_hashes).await;
                                }
                            }
                        } else { last_image_hash = hash; }
                    }
                },
                _ => {}
            }
        }
    }
}

// --- SERVER (RECEIVER) ---
async fn run_server(
    crypto: Arc<CryptoLayer>, 
    recent_hashes: RecentHashes,
    busy_writing: Arc<AtomicBool>,
    config: AppConfig
) -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:5566").await?;
    
    loop {
        let (mut socket, addr) = listener.accept().await?;
        let crypto_ref = crypto.clone();
        let hashes_ref = recent_hashes.clone();
        let busy_ref = busy_writing.clone();
        let config_ref = config.clone();

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
                        std::thread::sleep(std::time::Duration::from_millis(100));

                        match Clipboard::new() {
                            Ok(mut cb) => {
                                match content {
                                    ClipContent::Text(text) => {
                                        let hash = hash_data(text.as_bytes());
                                        hashes_ref.lock().unwrap().insert(hash);
                                        let _ = cb.set_text(text);
                                        
                                        if config_ref.notifications_enabled {
                                            show_notification("RustClip", "📋 Testo ricevuto");
                                        }
                                    },
                                    ClipContent::Image(png_bytes) => {
                                        if let Ok(image) = image::load_from_memory(&png_bytes) {
                                            let w = image.width() as usize;
                                            let h = image.height() as usize;
                                            let raw = image.to_rgba8().into_raw();
                                            let hash = hash_data(&raw);
                                            hashes_ref.lock().unwrap().insert(hash);

                                            let img_data = ImageData { width: w, height: h, bytes: Cow::from(raw) };
                                            if cb.set_image(img_data).is_ok() {
                                                if config_ref.notifications_enabled {
                                                    show_notification("RustClip", "🖼️ Immagine ricevuta");
                                                }
                                            }
                                        }
                                    }
                                }
                            },
                            Err(e) => eprintln!("❌ Errore Clipboard Server: {}", e),
                        }
                        
                        // Tempo di tregua per permettere all'OS di elaborare la clipboard
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        busy_ref.store(false, Ordering::Relaxed);
                    }).await;
                }
            }
        });
    }
}

// --- UTILS ---

async fn broadcast(
    content: ClipContent, 
    hash: String, 
    crypto: &CryptoLayer, 
    peers: &PeerMap, 
    hashes: &RecentHashes
) {
    hashes.lock().unwrap().insert(hash);
    let raw = match bincode::serialize(&content) { Ok(r) => r, Err(_) => return };
    let enc = match crypto.encrypt(&raw) { Ok(e) => e, Err(_) => return };
    
    for item in peers.iter() {
        let addr = item.value().addr; // Usiamo l'indirizzo della nuova struct PeerInfo
        let name = item.value().name.clone();
        let data = enc.clone();
        
        tokio::spawn(async move {
            if let Err(_) = send_data(addr, data).await {
                // Il peer verrà rimosso dal discovery se non più visto
            } else {
                println!("🚀 Inviato a {}", name);
            }
        });
    }
}

async fn send_data(addr: std::net::SocketAddr, data: Vec<u8>) -> Result<()> {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await??;
    let len = data.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&data).await?;
    Ok(())
}

fn show_notification(title: &str, body: &str) {
    let mut notif = Notification::new();
    notif.summary(title).body(body);

    // Fix specifico per macOS: forza l'associazione con il Bundle ID dell'app
    #[cfg(target_os = "macos")]
    notif.appname("com.rustclip.app");

    let _ = notif.show();
}

fn hash_data(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

// Helper per trasportare i dati dell'immagine tra thread
fn encode_raw_buffer(w: usize, h: usize, bytes: Vec<u8>) -> Vec<u8> {
    let mut v = Vec::with_capacity(16 + bytes.len());
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.extend_from_slice(&bytes);
    v
}

fn decode_raw_buffer(v: Vec<u8>) -> (usize, usize, Vec<u8>) {
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