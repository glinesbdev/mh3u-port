//! A fast hasher for the renderer's per-draw lookups: the keys are small integers and tuples of them, so SipHash's DoS resistance only costs time.

use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default, Clone, Copy)]
pub struct FxHasher(u64);

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    #[inline]
    fn add(&mut self, w: u64) {
        self.0 = (self.0.rotate_left(5) ^ w).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for c in chunks {
            self.add(u64::from_le_bytes(*c));
        }
        for &b in rest {
            self.add(b as u64);
        }
    }
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(i as u64);
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(i as u64);
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

pub type HashMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<FxHasher>>;
pub type HashSet<K> = std::collections::HashSet<K, BuildHasherDefault<FxHasher>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{BuildHasher, Hash};

    fn hash_of<T: Hash>(v: &T) -> u64 {
        BuildHasherDefault::<FxHasher>::default().hash_one(v)
    }

    #[test]
    fn same_input_same_hash_and_different_inputs_differ() {
        assert_eq!(hash_of(&(1u32, 2u64)), hash_of(&(1u32, 2u64)));
        assert_ne!(hash_of(&(1u32, 2u64)), hash_of(&(2u32, 1u64)));
        assert_ne!(hash_of(&0u32), hash_of(&1u32));
    }

    #[test]
    fn byte_slices_of_every_length_hash_and_differ_in_the_tail() {
        let data: Vec<u8> = (0..20).collect();
        let mut seen = std::collections::HashSet::new();
        for n in 0..=data.len() {
            assert!(seen.insert(hash_of(&data[..n].to_vec())), "length {n} collided");
        }
    }

    #[test]
    fn works_as_a_map_and_a_set() {
        let mut map: HashMap<(u32, u32), &str> = HashMap::default();
        map.insert((1, 2), "a");
        map.insert((2, 1), "b");
        assert_eq!(map.get(&(1, 2)), Some(&"a"));
        assert_eq!(map.get(&(2, 1)), Some(&"b"));
        assert_eq!(map.get(&(3, 3)), None);
        let set: HashSet<u64> = (0..1000).collect();
        assert_eq!(set.len(), 1000);
        assert!(set.contains(&999) && !set.contains(&1000));
    }
}
