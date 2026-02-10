use std::collections::{HashMap, VecDeque};
use serde::{Serialize, Deserialize};
use std::cmp::Ordering;

/// Vector Clock for Causality Tracking
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct VectorClock {
    // Map<NodeID, Counter>
    pub clocks: HashMap<String, u64>,
}

impl VectorClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn increment(&mut self, node_id: String) {
        let entry = self.clocks.entry(node_id).or_insert(0);
        *entry += 1;
    }

    pub fn merge(&mut self, other: &VectorClock) {
        for (node, &counter) in &other.clocks {
            let entry = self.clocks.entry(node.clone()).or_insert(0);
            if counter > *entry {
                *entry = counter;
            }
        }
    }

    /// Compare two vector clocks.
    /// Returns None if concurrent.
    pub fn partial_cmp(&self, other: &VectorClock) -> Option<Ordering> {
        let mut greater = false;
        let mut lesser = false;

        // Union of keys
        let mut all_keys: Vec<&String> = self.clocks.keys().collect();
        for k in other.clocks.keys() {
            if !self.clocks.contains_key(k) {
                all_keys.push(k);
            }
        }

        for k in all_keys {
            let v1 = self.clocks.get(k).unwrap_or(&0);
            let v2 = other.clocks.get(k).unwrap_or(&0);

            if v1 > v2 { greater = true; }
            if v1 < v2 { lesser = true; }
        }

        if greater && lesser {
            None // Concurrent
        } else if greater {
            Some(Ordering::Greater)
        } else if lesser {
            Some(Ordering::Less)
        } else {
            Some(Ordering::Equal)
        }
    }
}

/// Simple Deduplication Cache (Sliding Window of Packet Signatures/Hashes)
pub struct PacketCache {
    capacity: usize,
    hashes: VecDeque<Vec<u8>>, // Stores signature bytes
}

impl PacketCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            hashes: VecDeque::with_capacity(capacity),
        }
    }

    pub fn seen(&mut self, hash: &[u8]) -> bool {
        if self.hashes.iter().any(|h| h == hash) {
            return true;
        }

        if self.hashes.len() >= self.capacity {
            self.hashes.pop_front();
        }
        self.hashes.push_back(hash.to_vec());
        false
    }
}
