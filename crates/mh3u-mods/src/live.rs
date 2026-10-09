//! The hunter's save data as the game keeps it while playing (the companion's `docs/live.md`). The game holds three copies laid out like
//! the `user1` file; the live one, which changes the moment an item moves, has a header that points at itself (the word at +0x08 is its
//! own address minus 0xc, the word at +0x24 that plus 0x34), and the other two have blank headers. The companion finds it by the hunter's
//! name (at +0x2b) and then checks the header; here both are looked for, over every memory the game's heaps hand out.

use crate::companion::save::{SAVE_LEN, Save};
use std::path::Path;

pub const LEN: u32 = SAVE_LEN as u32;
pub const NAME_OFFSET: u32 = 0x2b;
const NAME_LEN: usize = 0x15;

/// Where the game's heaps are (as on the console, and in Cemu): MEM2, MEM1 and the foreground bucket.
pub const RANGES: [(u32, u32); 3] = [(0x1080_0000, 0x4900_0000), (0xf400_0000, 0x0200_0000), (0xe000_0000, 0x0300_0000)];

/// Whether the block at `at` has the live save's header, `read` giving big-endian words.
pub fn is_live_block(read: &dyn Fn(u32) -> Option<u32>, at: u32) -> bool {
    let p = at.wrapping_sub(0xc);
    at >= 0xc && read(at + 8) == Some(p) && read(at + 0x24) == Some(p.wrapping_add(0x34))
}

fn word(bytes: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(o..o + 4)?.try_into().ok()?))
}

/// The search patterns for hunter names: the name and the zero after it (the three bytes before it differ between hunters).
pub fn name_patterns(names: &[String]) -> Vec<Vec<u8>> {
    names
        .iter()
        .filter(|n| !n.is_empty() && n.len() < NAME_LEN)
        .map(|n| {
            let mut p = n.as_bytes().to_vec();
            p.push(0);
            p
        })
        .collect()
}

/// The places in a piece of memory (read at guest address `base`) that may be the live save: blocks whose header points at itself, and
/// blocks with a hunter's name where the name goes. Torn reads are fine: every candidate is checked again (`choose`).
pub fn find_candidates(piece: &[u8], base: u32, names: &[Vec<u8>], out: &mut Vec<u32>) {
    for k in (0..piece.len().saturating_sub(0x28)).step_by(4) {
        let at = base.wrapping_add(k as u32);
        if word(piece, k + 8) == Some(at.wrapping_sub(0xc)) && word(piece, k + 0x24) == Some(at.wrapping_add(0x28)) && at >= 0xc {
            out.push(at);
        }
    }
    for name in names {
        for o in memchr::memmem::find_iter(piece, name) {
            if let Some(at) = (base + o as u32).checked_sub(NAME_OFFSET) {
                out.push(at);
            }
        }
    }
}

/// Which candidate is the live save, from consistent copies of each block (taken at a frame): the one whose header points at itself;
/// failing that, one with a hunter's name and a header that is not blank (the other two copies' are). Otherwise why not, for the log.
pub fn choose(blocks: &[(u32, &[u8])], names: &[String]) -> Result<(u32, &'static str), String> {
    let header = |at: u32, b: &[u8]| is_live_block(&|a| word(b, a.checked_sub(at)? as usize), at);
    if let Some(&(at, _)) = blocks
        .iter()
        .find(|(at, b)| b.len() >= SAVE_LEN && header(*at, b) && Save::parse(b).is_ok())
    {
        return Ok((at, "its header points at itself"));
    }
    let named = |b: &[u8]| Save::parse(b).is_ok_and(|s| names.contains(&s.hunter_name));
    if let Some(&(at, _)) = blocks
        .iter()
        .find(|(_, b)| b.len() >= SAVE_LEN && named(b) && b[..0x0b].iter().any(|&x| x != 0))
    {
        return Ok((at, "a hunter's name, and a header that is not blank"));
    }
    if blocks.is_empty() {
        return Err(if names.is_empty() {
            "no block with a live header, and no hunter names to look for (no user1-3 in the save folder)".into()
        } else {
            format!(
                "no block with a live header or with the name of {} (is a hunter loaded?)",
                names.join(", ")
            )
        });
    }
    let seen: Vec<String> = blocks
        .iter()
        .take(6)
        .map(|(at, b)| {
            format!(
                "{at:#x}: +8 {:08x} +0x24 {:08x}",
                word(b, 8).unwrap_or(0),
                word(b, 0x24).unwrap_or(0)
            )
        })
        .collect();
    Err(format!("{} candidate(s), none the live save ({})", blocks.len(), seen.join("; ")))
}

/// The block's bytes as a save, if its header still says it is the live save, or it still carries the name it was chosen by.
pub fn parse(block: &[u8], at: u32, name: Option<&str>) -> Option<Save> {
    if block.len() < SAVE_LEN {
        return None;
    }
    let live = is_live_block(&|a| word(block, a.checked_sub(at)? as usize), at);
    let save = Save::parse(block).ok()?;
    (live || name.is_some_and(|n| n == save.hunter_name)).then_some(save)
}

/// The hunters' names in the game's save files (`user1` to `user3`, anywhere a few folders down in the save folder).
pub fn hunter_names(save_dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, depth: u32, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() && depth > 0 {
                walk(&p, depth - 1, out);
            } else if p
                .file_name()
                .is_some_and(|n| matches!(n.to_str(), Some("user1" | "user2" | "user3")))
                && let Ok(bytes) = std::fs::read(&p)
                && let Ok(save) = Save::parse(&bytes)
                && !save.hunter_name.is_empty()
                && !out.contains(&save.hunter_name)
            {
                out.push(save.hunter_name);
            }
        }
    }
    let mut out = Vec::new();
    walk(save_dir, 4, &mut out);
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A save block as the game keeps it: the hunter's name, a pouch with `stacks`, and either the live header (pointing at `at`) or
    /// the blank one of the other copies.
    pub fn block(at: u32, name: &str, live: bool, stacks: &[(u16, u16)]) -> Vec<u8> {
        let mut b = vec![0u8; SAVE_LEN];
        if live {
            b[8..12].copy_from_slice(&(at - 0xc).to_be_bytes());
            b[0x24..0x28].copy_from_slice(&(at - 0xc + 0x34).to_be_bytes());
        }
        b[0x2b..0x2b + name.len()].copy_from_slice(name.as_bytes());
        for (k, (id, n)) in stacks.iter().enumerate() {
            b[0xd0 + 4 * k..0xd2 + 4 * k].copy_from_slice(&id.to_be_bytes());
            b[0xd2 + 4 * k..0xd4 + 4 * k].copy_from_slice(&n.to_be_bytes());
        }
        b
    }

    #[test]
    fn candidates_by_header_and_by_name_and_the_live_one_chosen() {
        // memory with the three copies: as loaded and the UI's (blank headers) and the live one
        let base = 0x2000_0000u32;
        let mut mem = vec![0u8; 0x30000];
        let (loaded, ui, live) = (0x1000u32, 0xa000u32, 0x15004u32);
        for (o, is_live) in [(loaded, false), (ui, false), (live, true)] {
            let b = block(base + o, "Bobby", is_live, &[(100, 3)]);
            mem[o as usize..o as usize + SAVE_LEN].copy_from_slice(&b);
        }
        let names = vec!["Bobby".to_string()];
        let mut found = Vec::new();
        // in two pieces, as the scanner reads it
        let half = 0x18000;
        find_candidates(&mem[..half + 0x100], base, &name_patterns(&names), &mut found);
        find_candidates(&mem[half..], base + half as u32, &name_patterns(&names), &mut found);
        found.sort_unstable();
        found.dedup();
        assert_eq!(found, [base + loaded, base + ui, base + live]);
        let blocks: Vec<(u32, &[u8])> = found.iter().map(|&a| (a, &mem[(a - base) as usize..][..SAVE_LEN])).collect();
        let (at, why) = choose(&blocks, &names).unwrap();
        assert_eq!(at, base + live);
        assert!(why.contains("header"));
        let save = parse(blocks[2].1, at, None).unwrap();
        assert_eq!(save.item_count(100), 3);
        // the blank copies alone are not the live save, and the log says what was seen
        let err = choose(&blocks[..2], &names).unwrap_err();
        assert!(err.contains("2 candidate(s)"), "{err}");
        assert!(choose(&[], &[]).unwrap_err().contains("no hunter names"));
        assert!(choose(&[], &names).unwrap_err().contains("Bobby"));
    }

    #[test]
    fn a_named_block_with_an_unexpected_header_is_taken_when_none_points_at_itself() {
        let mut b = block(0x3000_0000, "Ann", false, &[]);
        b[0] = 0x12; // not blank, but not the self-pointer either
        let names = vec!["Ann".to_string()];
        let (at, why) = choose(&[(0x3000_0000, &b)], &names).unwrap();
        assert_eq!(at, 0x3000_0000);
        assert!(why.contains("name"));
        // and it is followed by its name
        assert!(parse(&b, at, Some("Ann")).is_some());
        assert!(parse(&b, at, Some("Bob")).is_none());
    }

    #[test]
    fn hunter_names_come_from_the_save_files() {
        let dir = std::env::temp_dir().join(format!("mh3u-names-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("common/save")).unwrap();
        std::fs::write(dir.join("common/save/user1"), block(0, "Bobby", false, &[])).unwrap();
        std::fs::write(dir.join("common/save/user2"), block(0, "Ann", false, &[])).unwrap();
        std::fs::write(dir.join("common/save/system"), vec![0u8; 100]).unwrap();
        let mut names = hunter_names(&dir);
        names.sort();
        assert_eq!(names, ["Ann", "Bobby"]);
        assert_eq!(name_patterns(&names)[0], b"Ann\0");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
