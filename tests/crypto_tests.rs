use rust_clip::core::crypto::CryptoLayer;

#[test]
fn test_encrypt_decrypt() {
    let key = b"12345678901234567890123456789012"; // 32 bytes
    let crypto = CryptoLayer::new(key);
    let data = b"Hello World! This is a test.";
    
    let encrypted = crypto.encrypt(data).expect("Encryption failed");
    assert_ne!(data.to_vec(), encrypted);
    
    let decrypted = crypto.decrypt(&encrypted).expect("Decryption failed");
    assert_eq!(data.to_vec(), decrypted);
}

#[test]
fn test_wrong_key_fails() {
    let key1 = b"11111111111111111111111111111111";
    let key2 = b"22222222222222222222222222222222";
    let crypto1 = CryptoLayer::new(key1);
    let crypto2 = CryptoLayer::new(key2);
    let data = b"Secret Data";
    
    let encrypted = crypto1.encrypt(data).unwrap();
    let result = crypto2.decrypt(&encrypted);
    
    assert!(result.is_err());
}
