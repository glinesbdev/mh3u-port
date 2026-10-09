//! The front of the shader caches: a draw finds its translated shader by what it can read cheaply (the shader's address, where its
//! program is and how long, and the inputs the translation depends on) instead of reading and hashing the whole program every draw.
//!
//! The program bytes are still what decides: each entry remembers a hash of them, and the first draw of every frame that uses an entry
//! hashes them again (so a game that rewrites a program in place is followed within a frame). An entry is dropped at once when the game
//! says the GPU must read that memory again (`GX2Invalidate` with the shader or CPU bit over the program). What is assumed: a program
//! rewritten without `GX2Invalidate` may be drawn as it was for the rest of that frame, which is also what the console's GPU, reading
//! through its own cache, would do.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

/// The memory a cached translation was made from: up to three programs, as (start, length) (geometry pipelines have three).
pub(crate) type Ranges = [(u32, u32); 3];

/// How many frames an entry is kept without being drawn with (about ten seconds at 60 frames a second).
pub(crate) const MAX_AGE: u32 = 600;
/// The most entries a front keeps; past it the least recently used go first.
pub(crate) const CAP: usize = 16384;

struct Entry<V> {
    ranges: Ranges,
    hash: u64,
    /// The frame the program bytes were last hashed in (or the entry used, which is the same frame or later).
    checked: u32,
    /// Its number in the index (`FrontCache::by_start`).
    serial: u64,
    value: V,
}

/// The entries by key, and indexed by where their programs start, so a `GX2Invalidate` looks only at the programs near the memory it
/// names, not at every entry (the game invalidates about a thousand times a frame).
pub(crate) struct FrontCache<K, V> {
    map: HashMap<K, Entry<V>>,
    /// (program start, entry serial) for each program of each entry.
    by_start: BTreeMap<(u32, u64), K>,
    /// The longest program any entry has had: a program that starts this far before an invalidated range can still overlap it.
    longest: u32,
    serial: u64,
    /// How many index entries the last `invalidate` looked at (for the tests and the report).
    pub(crate) examined: usize,
}

impl<K, V> Default for FrontCache<K, V> {
    fn default() -> Self {
        FrontCache {
            map: HashMap::new(),
            by_start: BTreeMap::new(),
            longest: 0,
            serial: 0,
            examined: 0,
        }
    }
}

/// How a lookup went (for the timing report's counters).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Lookup {
    /// Found, and already checked this frame.
    Hit,
    /// Found, and the bytes hashed again this frame were the same.
    Checked,
    /// Not there (or there and the bytes had changed: the entry is gone).
    Miss,
}

fn overlaps(&(at, len): &(u32, u32), address: u32, end: u64) -> bool {
    len != 0 && u64::from(at) < end && u64::from(address) < u64::from(at) + u64::from(len)
}

impl<K: Hash + Eq + Clone, V: Clone> FrontCache<K, V> {
    /// The value for `key`. `hash_ranges` hashes the program bytes, and is called at most once per entry per frame.
    pub(crate) fn get(&mut self, key: &K, frame: u32, hash_ranges: impl FnOnce(&Ranges) -> u64) -> (Option<V>, Lookup) {
        let Some(e) = self.map.get_mut(key) else {
            return (None, Lookup::Miss);
        };
        if e.checked == frame {
            return (Some(e.value.clone()), Lookup::Hit);
        }
        if hash_ranges(&e.ranges) == e.hash {
            e.checked = frame;
            return (Some(e.value.clone()), Lookup::Checked);
        }
        self.remove(key);
        (None, Lookup::Miss)
    }

    pub(crate) fn insert(&mut self, key: K, ranges: Ranges, hash: u64, frame: u32, value: V) {
        self.remove(&key);
        self.serial += 1;
        let serial = self.serial;
        for &(at, len) in ranges.iter().filter(|r| r.1 != 0) {
            self.by_start.insert((at, serial), key.clone());
            self.longest = self.longest.max(len);
        }
        self.map.insert(
            key,
            Entry {
                ranges,
                hash,
                checked: frame,
                serial,
                value,
            },
        );
    }

    fn remove(&mut self, key: &K) {
        if let Some(e) = self.map.remove(key) {
            for &(at, len) in &e.ranges {
                if len != 0 {
                    self.by_start.remove(&(at, e.serial));
                }
            }
        }
    }

    /// Forgets the entries made from memory that overlaps `size` bytes at `address`. Looks only at the programs that start between
    /// `longest` bytes before `address` and the end of the range, so its cost follows how many programs are near, not the cache's size.
    pub(crate) fn invalidate(&mut self, address: u32, size: u32) {
        let end = u64::from(address) + u64::from(size);
        let from = (address.saturating_sub(self.longest), 0);
        let mut doomed: Vec<K> = Vec::new();
        let mut examined = 0;
        let range = if end > u64::from(u32::MAX) {
            self.by_start.range(from..)
        } else {
            self.by_start.range(from..(end as u32, 0))
        };
        for (_, key) in range {
            examined += 1;
            if let Some(e) = self.map.get(key)
                && e.ranges.iter().any(|r| overlaps(r, address, end))
                && !doomed.contains(key)
            {
                doomed.push(key.clone());
            }
        }
        self.examined = examined;
        for key in &doomed {
            self.remove(key);
        }
    }

    /// Drops the entries not used for `MAX_AGE` frames, then the least recently used past `CAP`. Returns how many went.
    pub(crate) fn sweep(&mut self, frame: u32) -> usize {
        let before = self.map.len();
        let mut old: Vec<(u32, K)> = self
            .map
            .iter()
            .filter(|(_, e)| e.checked.saturating_add(MAX_AGE) < frame)
            .map(|(k, e)| (e.checked, k.clone()))
            .collect();
        if self.map.len() - old.len() > CAP {
            let mut rest: Vec<(u32, K)> = self
                .map
                .iter()
                .filter(|(_, e)| e.checked.saturating_add(MAX_AGE) >= frame)
                .map(|(k, e)| (e.checked, k.clone()))
                .collect();
            rest.sort_by_key(|r| r.0);
            let over = rest.len() - CAP;
            old.extend(rest.into_iter().take(over));
        }
        for (_, key) in &old {
            self.remove(key);
        }
        before - self.map.len()
    }

    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }
}

/// Whether a `GX2Invalidate` mode says the GPU must read shader programs again: the shader bit (8) or the CPU bit (0x40, the CPU wrote
/// the memory).
pub(crate) fn invalidates_shaders(mode: u32) -> bool {
    mode & (0x08 | 0x40) != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn one(at: u32, len: u32) -> Ranges {
        [(at, len), (0, 0), (0, 0)]
    }

    #[test]
    fn checked_once_per_frame() {
        let mut c: FrontCache<u32, &str> = FrontCache::default();
        c.insert(1, one(0x1000, 0x100), 42, 7, "vs");
        let calls = Cell::new(0);
        let hash = |_: &Ranges| {
            calls.set(calls.get() + 1);
            42
        };
        assert_eq!(c.get(&1, 7, hash), (Some("vs"), Lookup::Hit));
        assert_eq!(calls.get(), 0, "the frame it was made in needs no check");
        assert_eq!(c.get(&1, 8, hash), (Some("vs"), Lookup::Checked));
        assert_eq!(c.get(&1, 8, hash), (Some("vs"), Lookup::Hit));
        assert_eq!(calls.get(), 1);
        assert_eq!(c.get(&2, 8, hash), (None, Lookup::Miss));
    }

    #[test]
    fn rewritten_program_is_dropped_at_the_next_frame() {
        let mut c: FrontCache<u32, u8> = FrontCache::default();
        c.insert(1, one(0x1000, 0x100), 42, 7, 9);
        assert_eq!(c.get(&1, 8, |_| 43), (None, Lookup::Miss));
        assert_eq!(c.len(), 0);
        assert_eq!(c.get(&1, 8, |_| 42), (None, Lookup::Miss));
    }

    #[test]
    fn invalidate_drops_only_overlapping_programs() {
        let mut c: FrontCache<u32, u8> = FrontCache::default();
        c.insert(1, one(0x1000, 0x100), 0, 0, 1);
        c.insert(2, one(0x2000, 0x100), 0, 0, 2);
        c.insert(3, [(0x3000, 0x10), (0x4000, 0x10), (0x5000, 0x10)], 0, 0, 3);
        // touching the end is not overlapping
        c.invalidate(0x1100, 0x100);
        c.invalidate(0x0f00, 0x100);
        assert_eq!(c.len(), 3);
        c.invalidate(0x10ff, 1);
        assert_eq!(c.get(&1, 0, |_| 0).0, None);
        assert_eq!(c.len(), 2);
        // any of a pipeline's programs
        c.invalidate(0x500f, 0x1000);
        assert_eq!(c.get(&3, 0, |_| 0).0, None);
        assert_eq!(c.get(&2, 0, |_| 0).0, Some(2));
        // the whole address space, size wrapping past the end
        c.invalidate(0x1000, u32::MAX);
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn the_index_follows_inserts_replacements_and_removals() {
        let mut c: FrontCache<u32, u8> = FrontCache::default();
        c.insert(1, one(0x1000, 0x100), 0, 0, 1);
        // the same key again, somewhere else: the old program is no longer indexed
        c.insert(1, one(0x8000, 0x100), 0, 0, 2);
        c.invalidate(0x1000, 0x100);
        assert_eq!(c.len(), 1);
        c.invalidate(0x8080, 4);
        assert_eq!(c.len(), 0);
        assert!(c.by_start.is_empty());
        // a program that starts before the range and runs into it
        c.insert(2, one(0x2000, 0x1000), 0, 0, 3);
        c.insert(3, one(0x9000, 0x10), 0, 0, 4);
        c.invalidate(0x2ff0, 0x100);
        assert_eq!(c.get(&2, 0, |_| 0).0, None);
        assert_eq!(c.get(&3, 0, |_| 0).0, Some(4));
    }

    /// The benchmark of the invalidate: with a thousand entries or a hundred thousand, an invalidate of memory with no program near
    /// looks at the same handful of index entries (before, it walked every entry: a hundred times the work for a hundred times the
    /// entries), and the time per invalidate stays about the same (printed with `--nocapture`).
    #[test]
    fn invalidate_cost_does_not_grow_with_the_cache() {
        let time = |entries: u32| {
            let mut c: FrontCache<u32, u8> = FrontCache::default();
            for i in 0..entries {
                c.insert(i, one(0x1000_0000 + i * 0x200, 0x180), 0, 0, 0);
            }
            let rounds = 2000u32;
            let started = std::time::Instant::now();
            let mut examined = 0;
            for r in 0..rounds {
                // vertex and uniform data, below the programs: nothing there
                c.invalidate(0x0100_0000 + (r % 64) * 0x1000, 0x800);
                examined += c.examined;
            }
            assert_eq!(c.len(), entries as usize, "nothing overlapped");
            // one in the middle of the programs: only the neighbours are looked at
            c.invalidate(0x1000_0000 + (entries / 2) * 0x200 + 0x10, 0x10);
            assert_eq!(c.len(), entries as usize - 1);
            let near = c.examined;
            (started.elapsed() / rounds, examined / rounds as usize, near)
        };
        let (small_t, small_n, small_near) = time(1_000);
        let (big_t, big_n, big_near) = time(100_000);
        println!("invalidate: 1000 entries {small_t:?} ({small_n} looked at), 100000 entries {big_t:?} ({big_n} looked at)");
        assert_eq!(small_n, 0);
        assert_eq!(big_n, 0);
        assert!(small_near <= 2 && big_near <= 2, "{small_near} {big_near}");
    }

    #[test]
    fn unused_entries_age_out_and_the_cap_holds() {
        let mut c: FrontCache<u32, u8> = FrontCache::default();
        c.insert(1, one(0x1000, 0x10), 0, 0, 1);
        c.insert(2, one(0x2000, 0x10), 0, 0, 2);
        // entry 2 is used every frame, entry 1 never again
        for frame in 1..=MAX_AGE + 1 {
            let _ = c.get(&2, frame, |_| 0);
        }
        assert_eq!(c.sweep(MAX_AGE + 1), 1);
        assert_eq!(c.get(&1, MAX_AGE + 1, |_| 0).0, None);
        assert_eq!(c.get(&2, MAX_AGE + 1, |_| 0).0, Some(2));
        // past the cap, the least recently used go: entry 2 (last used at MAX_AGE + 1) first
        let base = MAX_AGE + 2;
        for i in 0..(CAP as u32 + 10) {
            c.insert(100 + i, one(0x10_0000 + i * 0x20, 0x10), 0, base + 1 + i / 64, 0);
        }
        c.sweep(base + 300);
        assert_eq!(c.len(), CAP);
        assert_eq!(c.by_start.len(), CAP);
        assert_eq!(c.get(&2, base + 300, |_| 0).0, None);
        assert_eq!(c.get(&(100 + CAP as u32 + 9), base + 300, |_| 0).0, Some(0));
    }

    #[test]
    fn invalidate_modes() {
        assert!(invalidates_shaders(0x08));
        assert!(invalidates_shaders(0x40));
        assert!(invalidates_shaders(0x1ff));
        assert!(!invalidates_shaders(0x01 | 0x02 | 0x04 | 0x10 | 0x20 | 0x80 | 0x100));
    }
}
