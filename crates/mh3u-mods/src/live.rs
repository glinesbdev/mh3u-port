//! The hunter's save data as the game keeps it while playing (the companion's `docs/live.md`): the game holds three copies laid out
//! like the `user1` file, and the live one, which changes the moment an item moves, has a header that points at itself: the word at
//! +0x08 is its own address minus 0xc, and the word at +0x24 is that plus 0x34. The other two have blank headers.

use crate::companion::save::{SAVE_LEN, Save};

pub const LEN: u32 = SAVE_LEN as u32;

/// Where in guest memory to look: the application's MEM2, where the game's heaps are.
pub const SEARCH_FROM: u32 = 0x1080_0000;
pub const SEARCH_TO: u32 = 0x1080_0000 + 0x4900_0000 - LEN;

/// Whether the block at `at` is the live save (by its header), `read` giving big-endian words.
pub fn is_live_block(read: &dyn Fn(u32) -> Option<u32>, at: u32) -> bool {
    let p = at.wrapping_sub(0xc);
    at >= 0xc && read(at + 8) == Some(p) && read(at + 0x24) == Some(p.wrapping_add(0x34))
}

/// The block's bytes as a save, if they still are the live save (the hunter may have been unloaded since it was found).
pub fn parse(block: &[u8], at: u32) -> Option<Save> {
    let word = |o: u32| -> Option<u32> {
        let o = o.checked_sub(at)? as usize;
        Some(u32::from_be_bytes(block.get(o..o + 4)?.try_into().ok()?))
    };
    if block.len() < SAVE_LEN || !is_live_block(&word, at) {
        return None;
    }
    Save::parse(block).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_header_points_at_itself() {
        let at: u32 = 0x2000_0010;
        let mut block = vec![0u8; 0x30];
        block[8..12].copy_from_slice(&(at - 0xc).to_be_bytes());
        block[0x24..0x28].copy_from_slice(&(at - 0xc + 0x34).to_be_bytes());
        let word = |b: &[u8], a: u32| -> Option<u32> {
            let o = a.checked_sub(at)? as usize;
            Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?))
        };
        assert!(is_live_block(&|a| word(&block, a), at));
        // the same bytes anywhere else do not point at themselves
        assert!(!is_live_block(&|a| word(&block, a + 4), at - 4));
        // a blank header (the other copies)
        let blank = vec![0u8; 0x30];
        assert!(!is_live_block(&|a| word(&blank, a), at));
        // too short to be a save
        assert!(parse(&block, at).is_none());
    }
}
