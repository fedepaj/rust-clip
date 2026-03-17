use std::collections::HashMap;
use std::time::Instant;

/// BLE fragment MTU (total size including header).
/// Conservative value safe for all BLE 4.2+ devices.
pub const BLE_FRAGMENT_MTU: usize = 480;

/// Fragment header: packet_id(4) + index(1) + total(1) + payload_len(2) = 8 bytes
const HEADER_SIZE: usize = 8;

/// Max payload bytes per fragment
const MAX_PAYLOAD: usize = BLE_FRAGMENT_MTU - HEADER_SIZE;

/// Reassembly timeout (seconds)
const REASSEMBLY_TIMEOUT_SECS: u64 = 5;

/// Split data into BLE-sized fragments.
/// Each fragment: [packet_id:u32][index:u8][total:u8][len:u16][payload]
pub fn fragment(data: &[u8], packet_id: u32) -> Vec<Vec<u8>> {
    if data.is_empty() {
        return vec![];
    }

    let chunks: Vec<&[u8]> = data.chunks(MAX_PAYLOAD).collect();
    let total = chunks.len().min(255) as u8;
    let mut fragments = Vec::with_capacity(chunks.len());

    for (i, chunk) in chunks.iter().enumerate() {
        let mut frag = Vec::with_capacity(HEADER_SIZE + chunk.len());
        frag.extend_from_slice(&packet_id.to_le_bytes());
        frag.push(i as u8);
        frag.push(total);
        frag.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        frag.extend_from_slice(chunk);
        fragments.push(frag);
    }

    fragments
}

/// State for one in-progress reassembly
struct ReassemblyEntry {
    total: u8,
    received: Vec<Option<Vec<u8>>>,
    started_at: Instant,
}

/// Reassembles fragments back into complete packets.
pub struct Reassembler {
    pending: HashMap<u32, ReassemblyEntry>,
}

impl Reassembler {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
        }
    }

    /// Feed a raw fragment. Returns Some(complete_data) when all fragments are received.
    pub fn feed(&mut self, raw: &[u8]) -> Option<Vec<u8>> {
        // Cleanup stale entries first
        self.cleanup();

        if raw.len() < HEADER_SIZE {
            return None;
        }

        let packet_id = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        let index = raw[4] as usize;
        let total = raw[5];
        let payload_len = u16::from_le_bytes([raw[6], raw[7]]) as usize;

        if total == 0 || index >= total as usize {
            return None;
        }

        if raw.len() < HEADER_SIZE + payload_len {
            return None;
        }

        let payload = raw[HEADER_SIZE..HEADER_SIZE + payload_len].to_vec();

        let entry = self.pending.entry(packet_id).or_insert_with(|| ReassemblyEntry {
            total,
            received: vec![None; total as usize],
            started_at: Instant::now(),
        });

        // Validate total matches
        if entry.total != total {
            return None;
        }

        entry.received[index] = Some(payload);

        // Check if complete
        if entry.received.iter().all(|slot| slot.is_some()) {
            let entry = self.pending.remove(&packet_id).unwrap();
            let mut complete = Vec::new();
            for slot in entry.received {
                complete.extend_from_slice(&slot.unwrap());
            }
            Some(complete)
        } else {
            None
        }
    }

    fn cleanup(&mut self) {
        self.pending.retain(|_, entry| {
            entry.started_at.elapsed().as_secs() < REASSEMBLY_TIMEOUT_SECS
        });
    }
}

/// Generate a random packet ID for fragmentation
pub fn next_packet_id() -> u32 {
    use rand::RngCore;
    rand::thread_rng().next_u32()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_fragment() {
        let data = b"hello world";
        let pid = 42;
        let frags = fragment(data, pid);
        assert_eq!(frags.len(), 1);

        let mut r = Reassembler::new();
        let result = r.feed(&frags[0]);
        assert_eq!(result, Some(data.to_vec()));
    }

    #[test]
    fn test_multi_fragment() {
        // Create data larger than MAX_PAYLOAD
        let data: Vec<u8> = (0..1500).map(|i| (i % 256) as u8).collect();
        let pid = 123;
        let frags = fragment(&data, pid);
        assert!(frags.len() > 1);

        let mut r = Reassembler::new();
        for (i, frag) in frags.iter().enumerate() {
            let result = r.feed(frag);
            if i < frags.len() - 1 {
                assert!(result.is_none());
            } else {
                assert_eq!(result, Some(data.clone()));
            }
        }
    }

    #[test]
    fn test_out_of_order() {
        let data: Vec<u8> = (0..1500).map(|i| (i % 256) as u8).collect();
        let pid = 456;
        let mut frags = fragment(&data, pid);
        frags.reverse(); // Receive in reverse order

        let mut r = Reassembler::new();
        for (i, frag) in frags.iter().enumerate() {
            let result = r.feed(frag);
            if i < frags.len() - 1 {
                assert!(result.is_none());
            } else {
                assert_eq!(result, Some(data.clone()));
            }
        }
    }
}
