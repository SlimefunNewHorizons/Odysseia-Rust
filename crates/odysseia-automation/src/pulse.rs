//! Sliding-window pulse counter for the redstone clock guard.
//!
//! Hot path (`record`) does one hash lookup and writes into a pre-allocated
//! ring buffer: no per-pulse heap allocation, no boxing. A ring is allocated
//! the first time a block is seen. Semantics match the Java `PulseWindow`
//! (prune `ts < now - long_window`, count fast window with `ts >= now - fast_window`).
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Key = (world id, x, y, z).
pub type PulseKey = [i32; 4];

#[derive(Default)]
pub struct FxHasher(u64);

impl Hasher for FxHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.write_u64(*b as u64);
        }
    }
    fn write_i32(&mut self, i: i32) {
        self.write_u64(i as u32 as u64);
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

type Fx = BuildHasherDefault<FxHasher>;

struct Ring {
    buf: Box<[i64]>,
    head: usize, // index of oldest
    len: usize,
}

impl Ring {
    fn new(cap: usize) -> Self {
        Ring { buf: vec![0; cap].into_boxed_slice(), head: 0, len: 0 }
    }
    #[inline]
    fn at(&self, i: usize) -> i64 {
        self.buf[(self.head + i) % self.buf.len()]
    }
    #[inline]
    fn newest(&self) -> i64 {
        if self.len == 0 { i64::MIN } else { self.at(self.len - 1) }
    }
}

pub struct PulseTable {
    rings: HashMap<PulseKey, Ring, Fx>,
    ring_cap: usize,
    max_entries: usize,
}

/// Result of `record`: pulses in the fast window and in the long window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PulseCounts {
    pub fast: u32,
    pub long: u32,
}

impl PulseTable {
    /// `ring_cap`: max timestamps kept per block (>= the long pulse limit).
    pub fn new(ring_cap: usize, max_entries: usize) -> Self {
        PulseTable {
            rings: HashMap::with_capacity_and_hasher(1024, Fx::default()),
            ring_cap: ring_cap.clamp(8, 1 << 16),
            max_entries: max_entries.max(16),
        }
    }

    pub fn ring_cap(&self) -> usize {
        self.ring_cap
    }

    pub fn len(&self) -> usize {
        self.rings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rings.is_empty()
    }

    /// Records a pulse. `None` if the table is full (caller must fail open).
    pub fn record(&mut self, key: PulseKey, now: i64, long_ms: i64, fast_ms: i64) -> Option<PulseCounts> {
        if self.rings.len() >= self.max_entries && !self.rings.contains_key(&key) {
            return None;
        }
        let cap = self.ring_cap;
        let ring = self.rings.entry(key).or_insert_with(|| Ring::new(cap));

        // push (overwrite oldest when full)
        if ring.len == cap {
            ring.buf[ring.head] = now;
            ring.head = (ring.head + 1) % cap;
        } else {
            let idx = (ring.head + ring.len) % cap;
            ring.buf[idx] = now;
            ring.len += 1;
        }
        // prune the long window
        let long_cut = now - long_ms;
        while ring.len > 0 && ring.at(0) < long_cut {
            ring.head = (ring.head + 1) % cap;
            ring.len -= 1;
        }
        // count the fast window from the newest backwards
        let fast_cut = now - fast_ms;
        let mut fast = 0u32;
        let mut i = ring.len;
        while i > 0 {
            if ring.at(i - 1) >= fast_cut {
                fast += 1;
                i -= 1;
            } else {
                break;
            }
        }
        Some(PulseCounts { fast, long: ring.len as u32 })
    }

    pub fn remove(&mut self, key: &PulseKey) {
        self.rings.remove(key);
    }

    /// Drops blocks whose last pulse is older than `idle_ms`. Returns how many.
    pub fn sweep(&mut self, now: i64, idle_ms: i64) -> usize {
        let before = self.rings.len();
        let cut = now - idle_ms;
        self.rings.retain(|_, r| r.newest() >= cut);
        before - self.rings.len()
    }

    pub fn clear(&mut self) {
        self.rings.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Reference = literal port of the Java PulseWindow.
    struct JavaRef(VecDeque<i64>);
    impl JavaRef {
        fn record(&mut self, now: i64, window: i64) {
            self.0.push_back(now);
            while let Some(&f) = self.0.front() {
                if f < now - window { self.0.pop_front(); } else { break; }
            }
        }
        fn count_since(&self, cut: i64) -> u32 {
            self.0.iter().filter(|&&p| p >= cut).count() as u32
        }
    }

    #[test]
    fn matches_java_reference() {
        let (long_ms, fast_ms) = (120_000, 10_000);
        let mut t = PulseTable::new(4000, 100);
        let mut r = JavaRef(VecDeque::new());
        let mut now = 1_000_000i64;
        let mut seed = 12345u64;
        for _ in 0..20_000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            now += ((seed >> 33) % 900) as i64; // 0..900 ms between pulses
            r.record(now, long_ms);
            let c = t.record([1, 2, 3, 4], now, long_ms, fast_ms).unwrap();
            assert_eq!(c.long as usize, r.0.len());
            assert_eq!(c.fast, r.count_since(now - fast_ms));
        }
    }

    #[test]
    fn full_table_fails_open_and_sweep_frees() {
        let mut t = PulseTable::new(8, 16);
        for i in 0..16 {
            assert!(t.record([0, i, 0, 0], 1000, 60_000, 10_000).is_some());
        }
        assert!(t.record([0, 99, 0, 0], 1000, 60_000, 10_000).is_none());
        assert!(t.record([0, 3, 0, 0], 1001, 60_000, 10_000).is_some());
        assert_eq!(t.sweep(500_000, 60_000), 16);
        assert!(t.is_empty());
    }

    #[test]
    fn ring_cap_still_triggers_long_limit() {
        let mut t = PulseTable::new(20, 16);
        let mut last = None;
        for i in 0..100 {
            last = t.record([0, 0, 0, 0], 1000 + i, 120_000, 10_000);
        }
        assert_eq!(last.unwrap().long, 20); // saturates at cap == long limit
    }
}
