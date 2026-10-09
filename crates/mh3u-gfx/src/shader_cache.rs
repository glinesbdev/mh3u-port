//! The front of the shader caches: a draw finds its translated shader by what it can read cheaply (the shader's address, where its
//! program is and how long, and the inputs the translation depends on) instead of reading and hashing the whole program every draw.
//!
//! The program bytes are still what decides: each entry remembers a hash of them, and the first draw of every frame that uses an entry
//! hashes them again (so a game that rewrites a program in place is followed within a frame). An entry is dropped at once when the game
//! says the GPU must read that memory again (`GX2Invalidate` with the shader or CPU bit over the program). What is assumed: a program
//! rewritten without `GX2Invalidate` may be drawn as it was for the rest of that frame, which is also what the console's GPU, reading
//! through its own cache, would do.

use std::collections::HashMap;
use std::hash::Hash;

/// The memory a cached translation was made from: up to three programs, as (start, length) (geometry pipelines have three).
pub(crate) type Ranges = [(u32, u32); 3];

struct Entry<V> {
    ranges: Ranges,
    hash: u64,
    /// The frame the program bytes were last hashed in.
    checked: u32,
    value: V,
}

pub(crate) struct FrontCache<K, V> {
    map: HashMap<K, Entry<V>>,
}

impl<K, V> Default for FrontCache<K, V> {
    fn default() -> Self {
        FrontCache { map: HashMap::new() }
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

impl<K: Hash + Eq, V: Clone> FrontCache<K, V> {
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
        self.map.remove(key);
        (None, Lookup::Miss)
    }

    pub(crate) fn insert(&mut self, key: K, ranges: Ranges, hash: u64, frame: u32, value: V) {
        self.map.insert(
            key,
            Entry {
                ranges,
                hash,
                checked: frame,
                value,
            },
        );
    }

    /// Forgets the entries made from memory that overlaps `size` bytes at `address`.
    pub(crate) fn invalidate(&mut self, address: u32, size: u32) {
        let end = u64::from(address) + u64::from(size);
        self.map.retain(|_, e| {
            !e.ranges
                .iter()
                .any(|&(at, len)| len != 0 && u64::from(at) < end && u64::from(address) < u64::from(at) + u64::from(len))
        });
    }

    #[cfg(test)]
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
    fn invalidate_modes() {
        assert!(invalidates_shaders(0x08));
        assert!(invalidates_shaders(0x40));
        assert!(invalidates_shaders(0x1ff));
        assert!(!invalidates_shaders(0x01 | 0x02 | 0x04 | 0x10 | 0x20 | 0x80 | 0x100));
    }
}
