use rust_clip::core::clipboard;

#[test]
fn test_hash_consistency() {
    let data1 = b"some content";
    let data2 = b"some content";
    let data3 = b"different content";
    
    let h1 = clipboard::hash_data(data1);
    let h2 = clipboard::hash_data(data2);
    let h3 = clipboard::hash_data(data3);
    
    assert_eq!(h1, h2);
    assert_ne!(h1, h3);
}

#[test]
fn test_raw_encoding() {
    let width = 100usize;
    let height = 50usize;
    let bytes = vec![255u8; 100]; // Fake pixels
    
    let encoded = clipboard::encode_raw(width, height, bytes.clone());
    let (w, h, decoded_bytes) = clipboard::decode_raw(encoded);
    
    assert_eq!(width, w);
    assert_eq!(height, h);
    assert_eq!(bytes, decoded_bytes);
}
