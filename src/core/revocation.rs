use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use crate::core::identity::RingIdentity;

/// A signed notice revoking a peer from the ring.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RevocationEntry {
    /// StablePeerId of the revoked peer.
    pub revoked_peer_id: String,
    /// StablePeerId of the peer that issued the revocation.
    pub revoker_id: String,
    /// Reason for revocation.
    pub reason: String,
    /// When the revocation was issued.
    pub timestamp: DateTime<Utc>,
    /// Ed25519 signature of {revoked_peer_id, revoker_id, reason, timestamp}.
    pub signature: Vec<u8>,
}

impl RevocationEntry {
    /// Create a signed revocation entry.
    pub fn new(
        revoked_peer_id: String,
        identity: &RingIdentity,
        reason: String,
    ) -> Self {
        let timestamp = Utc::now();
        let sign_data = Self::sign_data(&revoked_peer_id, identity.stable_peer_id(), &reason, &timestamp);
        let sig = identity.sign(&sign_data);

        RevocationEntry {
            revoked_peer_id,
            revoker_id: identity.stable_peer_id().to_string(),
            reason,
            timestamp,
            signature: sig.to_bytes().to_vec(),
        }
    }

    /// Verify the signature on this revocation entry.
    pub fn verify(&self, revoker_pubkey: &VerifyingKey) -> Result<()> {
        let sign_data = Self::sign_data(
            &self.revoked_peer_id,
            &self.revoker_id,
            &self.reason,
            &self.timestamp,
        );

        let sig = Signature::from_bytes(
            self.signature.as_slice().try_into()
                .map_err(|_| anyhow!("Invalid signature length"))?,
        );

        revoker_pubkey
            .verify(&sign_data, &sig)
            .map_err(|e| anyhow!("Revocation signature invalid: {}", e))
    }

    fn sign_data(
        revoked: &str,
        revoker: &str,
        reason: &str,
        timestamp: &DateTime<Utc>,
    ) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(revoked.as_bytes());
        data.extend_from_slice(revoker.as_bytes());
        data.extend_from_slice(reason.as_bytes());
        data.extend_from_slice(timestamp.to_rfc3339().as_bytes());
        data
    }
}

/// Local revocation list tracking which peers have been revoked.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RevocationList {
    pub entries: Vec<RevocationEntry>,
    /// Set of revoked StablePeerIds for fast lookup.
    #[serde(skip)]
    revoked_set: HashSet<String>,
}

impl RevocationList {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            revoked_set: HashSet::new(),
        }
    }

    /// Check if a peer is revoked.
    pub fn is_revoked(&self, peer_id: &str) -> bool {
        self.revoked_set.contains(peer_id)
    }

    /// Add a revocation entry. Returns true if newly added.
    pub fn add(&mut self, entry: RevocationEntry) -> bool {
        if self.revoked_set.contains(&entry.revoked_peer_id) {
            return false;
        }
        self.revoked_set.insert(entry.revoked_peer_id.clone());
        self.entries.push(entry);
        true
    }

    /// Merge another revocation list (union). Returns number of new entries added.
    pub fn merge(&mut self, other: &RevocationList) -> usize {
        let mut added = 0;
        for entry in &other.entries {
            if self.add(entry.clone()) {
                added += 1;
            }
        }
        added
    }

    /// Rebuild the lookup set from entries (needed after deserialization).
    pub fn rebuild_index(&mut self) {
        self.revoked_set = self.entries.iter().map(|e| e.revoked_peer_id.clone()).collect();
    }

    /// Persist the revocation list to disk.
    pub fn save(&self) -> Result<()> {
        let path = Self::get_path()?;
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&path, json)?;
        Ok(())
    }

    /// Load from disk. Returns empty list if file doesn't exist.
    pub fn load() -> Self {
        match Self::try_load() {
            Ok(mut list) => {
                list.rebuild_index();
                list
            }
            Err(_) => Self::new(),
        }
    }

    fn try_load() -> Result<Self> {
        let path = Self::get_path()?;
        if !path.exists() {
            return Err(anyhow!("No revocation list file"));
        }
        let content = fs::read_to_string(path)?;
        let list: RevocationList = serde_json::from_str(&content)?;
        Ok(list)
    }

    fn get_path() -> Result<PathBuf> {
        let proj = directories::ProjectDirs::from("com", "rustclip", "rust-clip")
            .ok_or_else(|| anyhow!("Could not determine config directory"))?;

        let config_dir = proj.config_dir();
        if !config_dir.exists() {
            fs::create_dir_all(config_dir)?;
        }

        Ok(config_dir.join("revocations.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_identity() -> RingIdentity {
        RingIdentity::from_mnemonic(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .unwrap()
    }

    fn test_identity_b() -> RingIdentity {
        RingIdentity::from_mnemonic(
            "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo wrong",
        )
        .unwrap()
    }

    #[test]
    fn create_and_verify_revocation() {
        let identity = test_identity();
        let entry = RevocationEntry::new(
            "peer-to-revoke".into(),
            &identity,
            "Device stolen".into(),
        );

        assert_eq!(entry.revoked_peer_id, "peer-to-revoke");
        assert_eq!(entry.revoker_id, identity.stable_peer_id());

        // Verify with correct key
        assert!(entry.verify(&identity.public_key).is_ok());

        // Verify with wrong key should fail
        let other = test_identity_b();
        assert!(entry.verify(&other.public_key).is_err());
    }

    #[test]
    fn revocation_list_add_and_check() {
        let identity = test_identity();
        let mut list = RevocationList::new();

        assert!(!list.is_revoked("peer-a"));

        let entry = RevocationEntry::new("peer-a".into(), &identity, "stolen".into());
        assert!(list.add(entry));
        assert!(list.is_revoked("peer-a"));
        assert!(!list.is_revoked("peer-b"));
    }

    #[test]
    fn revocation_list_no_duplicates() {
        let identity = test_identity();
        let mut list = RevocationList::new();

        let entry1 = RevocationEntry::new("peer-a".into(), &identity, "reason1".into());
        let entry2 = RevocationEntry::new("peer-a".into(), &identity, "reason2".into());

        assert!(list.add(entry1));
        assert!(!list.add(entry2)); // Duplicate — should return false
        assert_eq!(list.entries.len(), 1);
    }

    #[test]
    fn revocation_list_merge() {
        let identity = test_identity();

        let mut list_a = RevocationList::new();
        list_a.add(RevocationEntry::new("peer-1".into(), &identity, "r1".into()));
        list_a.add(RevocationEntry::new("peer-2".into(), &identity, "r2".into()));

        let mut list_b = RevocationList::new();
        list_b.add(RevocationEntry::new("peer-2".into(), &identity, "r2".into()));
        list_b.add(RevocationEntry::new("peer-3".into(), &identity, "r3".into()));

        let added = list_a.merge(&list_b);
        assert_eq!(added, 1); // Only peer-3 is new
        assert!(list_a.is_revoked("peer-1"));
        assert!(list_a.is_revoked("peer-2"));
        assert!(list_a.is_revoked("peer-3"));
        assert_eq!(list_a.entries.len(), 3);
    }

    #[test]
    fn revocation_list_rebuild_index() {
        let identity = test_identity();
        let mut list = RevocationList::new();
        list.add(RevocationEntry::new("peer-x".into(), &identity, "test".into()));

        // Simulate deserialization (revoked_set would be empty)
        let json = serde_json::to_string(&list).unwrap();
        let mut loaded: RevocationList = serde_json::from_str(&json).unwrap();

        assert!(!loaded.is_revoked("peer-x")); // Set not rebuilt yet
        loaded.rebuild_index();
        assert!(loaded.is_revoked("peer-x")); // Now it works
    }
}
