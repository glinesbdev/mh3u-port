//! Finding the functions of an executable: the places that something calls, or keeps a pointer to.

use std::collections::BTreeSet;

use mh3u_ppc::{Insn, Op};

/// Where the functions of the code in `words` (loaded at `base`) start: the entry point, every direct call (`bl`) and every word of
/// `pointers` (what the data holds) that points at an instruction right after a return, a jump or padding.
pub fn functions(base: u32, words: &[u32], pointers: &[u32], entry: u32) -> BTreeSet<u32> {
    let end = base.wrapping_add(4 * words.len() as u32);
    let inside = |a: u32| a.is_multiple_of(4) && (base..end).contains(&a);
    let mut set = BTreeSet::new();
    if inside(entry) {
        set.insert(entry);
    }
    // a pointer into the middle of a function is a jump table's entry, not a function; real starts follow the end of another function
    let follows_an_end = |a: u32| {
        a == base || {
            let prev = words[((a - base) / 4) as usize - 1];
            prev == 0 || prev == 0x6000_0000 || prev == 0x4e80_0020 || prev == 0x4e80_0420 || (prev >> 26 == 18 && prev & 1 == 0)
        }
    };
    for &p in pointers {
        if inside(p) && follows_an_end(p) {
            set.insert(p);
        }
    }
    for (i, &w) in words.iter().enumerate() {
        let Some(insn) = Insn::decode(w) else { continue };
        if insn.op == Op::B && insn.lk() {
            let addr = base + 4 * i as u32;
            let t = if insn.aa() {
                insn.li() as u32
            } else {
                addr.wrapping_add(insn.li() as u32)
            };
            if inside(t) {
                set.insert(t);
            }
        }
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_pointers_and_the_entry_start_functions() {
        // 0x100: bl 0x10c ; blr ; nop ; 0x10c: blr
        let words = [0x4800_000d, 0x4e80_0020, 0x6000_0000, 0x4e80_0020];
        let f = functions(0x100, &words, &[0x108, 0x5000], 0x100);
        assert_eq!(f.into_iter().collect::<Vec<_>>(), [0x100, 0x108, 0x10c]);
    }
}
