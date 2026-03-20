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

#[cfg(test)]
mod tests {
    use super::*;

    // ─── VectorClock tests ──────────────────────────────────────

    #[test]
    fn vector_clock_increment() {
        let mut vc = VectorClock::new();
        vc.increment("node-a".into());
        vc.increment("node-a".into());
        vc.increment("node-b".into());

        assert_eq!(vc.clocks["node-a"], 2);
        assert_eq!(vc.clocks["node-b"], 1);
    }

    #[test]
    fn vector_clock_merge() {
        let mut vc1 = VectorClock::new();
        vc1.increment("a".into());
        vc1.increment("a".into());

        let mut vc2 = VectorClock::new();
        vc2.increment("a".into());
        vc2.increment("b".into());
        vc2.increment("b".into());

        vc1.merge(&vc2);
        assert_eq!(vc1.clocks["a"], 2); // max(2,1)
        assert_eq!(vc1.clocks["b"], 2); // max(0,2)
    }

    #[test]
    fn vector_clock_ordering() {
        let mut vc1 = VectorClock::new();
        vc1.increment("a".into());

        let mut vc2 = VectorClock::new();
        vc2.increment("a".into());
        vc2.increment("a".into());

        // vc1 < vc2 (vc2 happened after vc1)
        assert_eq!(vc1.partial_cmp(&vc2), Some(Ordering::Less));
        assert_eq!(vc2.partial_cmp(&vc1), Some(Ordering::Greater));
    }

    #[test]
    fn vector_clock_concurrent() {
        let mut vc1 = VectorClock::new();
        vc1.increment("a".into());

        let mut vc2 = VectorClock::new();
        vc2.increment("b".into());

        // Concurrent: vc1 has a=1 but no b, vc2 has b=1 but no a
        assert_eq!(vc1.partial_cmp(&vc2), None);
    }

    #[test]
    fn vector_clock_equal() {
        let mut vc1 = VectorClock::new();
        vc1.increment("a".into());

        let mut vc2 = VectorClock::new();
        vc2.increment("a".into());

        assert_eq!(vc1.partial_cmp(&vc2), Some(Ordering::Equal));
    }

    // ─── PacketCache tests ──────────────────────────────────────

    #[test]
    fn packet_cache_dedup() {
        let mut cache = PacketCache::new(10);
        assert!(!cache.seen(b"packet-1"));
        assert!(cache.seen(b"packet-1")); // seen again
        assert!(!cache.seen(b"packet-2"));
    }

    #[test]
    fn packet_cache_eviction() {
        let mut cache = PacketCache::new(3);
        cache.seen(b"a");
        cache.seen(b"b");
        cache.seen(b"c");

        // Cache full. "a" should be evicted after next insert.
        cache.seen(b"d");
        assert!(!cache.seen(b"a")); // "a" was evicted, not seen anymore
        assert!(cache.seen(b"d")); // "d" is still in cache
    }
}
