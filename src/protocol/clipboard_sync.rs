use sha2::{Sha256, Digest};
use std::time::Instant;

/// Clipboard deduplication and sync logic.
/// Prevents loops (paste received text → re-detect → re-send) and oscillation.
pub struct ClipboardSync {
    /// SHA-256 hash of the last propagated clipboard content
    last_hash: Option<[u8; 32]>,
    /// Peer ID that originated the last received clipboard update
    last_origin: Option<String>,
    /// When the last remote clipboard was applied (for cooldown)
    last_applied_at: Option<Instant>,
    /// Cooldown period after applying remote clipboard (ms)
    cooldown_ms: u64,
}

impl ClipboardSync {
    pub fn new() -> Self {
        Self {
            last_hash: None,
            last_origin: None,
            last_applied_at: None,
            cooldown_ms: 500,
        }
    }

    /// Check if a local clipboard change should be propagated.
    /// Returns false if it's a duplicate or we're in cooldown.
    pub fn should_propagate(&mut self, content: &[u8], my_peer_id: &str) -> bool {
        // Cooldown check: if we recently applied a remote clipboard, skip
        if let Some(applied_at) = self.last_applied_at {
            if applied_at.elapsed().as_millis() < self.cooldown_ms as u128 {
                return false;
            }
        }

        // Content hash deduplication
        let hash = Self::hash_content(content);
        if self.last_hash.as_ref() == Some(&hash) {
            return false;
        }

        // Update state
        self.last_hash = Some(hash);
        self.last_origin = Some(my_peer_id.to_string());
        true
    }

    /// Record that we received and applied a remote clipboard update.
    /// Returns false if this update should be ignored (our own origin, or duplicate).
    pub fn apply_remote(&mut self, content: &[u8], origin_peer_id: &str, my_peer_id: &str) -> bool {
        // Ignore our own updates echoed back
        if origin_peer_id == my_peer_id {
            return false;
        }

        // Deduplication
        let hash = Self::hash_content(content);
        if self.last_hash.as_ref() == Some(&hash) {
            return false;
        }

        // Apply
        self.last_hash = Some(hash);
        self.last_origin = Some(origin_peer_id.to_string());
        self.last_applied_at = Some(Instant::now());
        true
    }

    fn hash_content(content: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(content);
        hasher.finalize().into()
    }
}
