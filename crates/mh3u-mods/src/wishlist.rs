//! The blacksmith wishlist: pieces of armour and weapons the player wants, what each needs against what the item box and pouch hold,
//! a shopping list of everything together, and the pieces that just became craftable (for a notice). Kept in a JSON file.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// A piece: the equipment kind (the save's kind byte) and its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Piece {
    pub kind: u8,
    pub id: u16,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wishlist {
    pub pieces: Vec<Piece>,
}

/// One material of a piece, or of the shopping list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Need {
    pub item: u16,
    pub need: u32,
    pub have: u32,
}

impl Need {
    pub fn missing(&self) -> u32 {
        self.need.saturating_sub(self.have)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub piece: Piece,
    pub materials: Vec<Need>,
    /// Every material is there (each piece on its own: two pieces that share a material may not both be).
    pub craftable: bool,
}

impl Wishlist {
    /// Adds a piece (once).
    pub fn add(&mut self, piece: Piece) -> bool {
        let new = !self.pieces.contains(&piece);
        if new {
            self.pieces.push(piece);
        }
        new
    }

    pub fn remove(&mut self, piece: Piece) {
        self.pieces.retain(|p| *p != piece);
    }

    /// Each piece's materials, from `materials(piece)` (item, count), against `have(item)`. Pieces with no recipe are left out.
    pub fn status(&self, materials: impl Fn(Piece) -> Option<Vec<(u16, u16)>>, have: impl Fn(u16) -> u32) -> Vec<Status> {
        self.pieces
            .iter()
            .filter_map(|&piece| {
                let list: Vec<Need> = materials(piece)?
                    .into_iter()
                    .map(|(item, count)| Need {
                        item,
                        need: u32::from(count),
                        have: have(item),
                    })
                    .collect();
                let craftable = list.iter().all(|n| n.missing() == 0);
                Some(Status {
                    piece,
                    materials: list,
                    craftable,
                })
            })
            .collect()
    }

    /// Everything the pieces need together, by item: what they need in all against what there is.
    pub fn shopping(statuses: &[Status]) -> Vec<Need> {
        let mut all: BTreeMap<u16, Need> = BTreeMap::new();
        for n in statuses.iter().flat_map(|s| &s.materials) {
            all.entry(n.item)
                .or_insert(Need {
                    item: n.item,
                    need: 0,
                    have: n.have,
                })
                .need += n.need;
        }
        all.into_values().collect()
    }

    pub fn load(path: &Path) -> Wishlist {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Writes the list (through a file beside it, so a crash never leaves half a file).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, path)
    }
}

/// Which pieces are craftable, to tell which just became so. The first look (and the first after the save went away) only takes note.
#[derive(Default)]
pub struct Craftable {
    seen: Option<HashSet<Piece>>,
}

impl Craftable {
    /// The pieces craftable now that were not at the last look.
    pub fn update(&mut self, statuses: &[Status]) -> Vec<Piece> {
        let now: HashSet<Piece> = statuses.iter().filter(|s| s.craftable).map(|s| s.piece).collect();
        let new = match &self.seen {
            Some(before) => statuses
                .iter()
                .map(|s| s.piece)
                .filter(|p| now.contains(p) && !before.contains(p))
                .collect(),
            None => Vec::new(),
        };
        self.seen = Some(now);
        new
    }

    /// The save is gone (no hunter loaded): the next look starts afresh.
    pub fn forget(&mut self) {
        self.seen = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOOD: Piece = Piece { kind: 5, id: 10 };
    const SWORD: Piece = Piece { kind: 7, id: 3 };

    fn materials(p: Piece) -> Option<Vec<(u16, u16)>> {
        match p {
            HOOD => Some(vec![(100, 2), (200, 1)]),
            SWORD => Some(vec![(100, 3), (300, 5)]),
            _ => None,
        }
    }

    #[test]
    fn each_piece_against_the_box_and_the_shopping_list_together() {
        let mut w = Wishlist::default();
        assert!(w.add(HOOD));
        assert!(w.add(SWORD));
        assert!(!w.add(HOOD), "once");
        w.add(Piece { kind: 1, id: 999 });
        let have = |item| match item {
            100 => 4,
            200 => 1,
            _ => 0,
        };
        let s = w.status(materials, have);
        assert_eq!(s.len(), 2, "a piece with no recipe is left out");
        assert!(s[0].craftable);
        assert!(!s[1].craftable);
        assert_eq!(
            s[1].materials[0],
            Need {
                item: 100,
                need: 3,
                have: 4
            }
        );
        assert_eq!(s[1].materials[1].missing(), 5);
        // together: 5 of item 100 against 4, so one is missing although each piece alone has enough
        let list = Wishlist::shopping(&s);
        assert_eq!(
            list,
            [
                Need {
                    item: 100,
                    need: 5,
                    have: 4
                },
                Need {
                    item: 200,
                    need: 1,
                    have: 1
                },
                Need {
                    item: 300,
                    need: 5,
                    have: 0
                }
            ]
        );
        assert_eq!(list.iter().map(Need::missing).sum::<u32>(), 6);
        w.remove(SWORD);
        assert_eq!(w.pieces, [HOOD, Piece { kind: 1, id: 999 }]);
    }

    #[test]
    fn a_notice_only_when_a_piece_becomes_craftable() {
        let mut w = Wishlist::default();
        w.add(HOOD);
        w.add(SWORD);
        let mut c = Craftable::default();
        let mut box_ = std::collections::HashMap::from([(100u16, 4u32), (200, 1)]);
        // the first look takes note of the hood but does not announce it
        assert!(c.update(&w.status(materials, |i| box_.get(&i).copied().unwrap_or(0))).is_empty());
        box_.insert(300, 5);
        assert_eq!(c.update(&w.status(materials, |i| box_.get(&i).copied().unwrap_or(0))), [SWORD]);
        assert!(
            c.update(&w.status(materials, |i| box_.get(&i).copied().unwrap_or(0))).is_empty(),
            "once"
        );
        box_.insert(300, 0);
        c.update(&w.status(materials, |i| box_.get(&i).copied().unwrap_or(0)));
        box_.insert(300, 5);
        assert_eq!(
            c.update(&w.status(materials, |i| box_.get(&i).copied().unwrap_or(0))),
            [SWORD],
            "again after it was not"
        );
    }

    #[test]
    fn the_list_is_kept_in_a_json_file() {
        let dir = std::env::temp_dir().join(format!("mh3u-wishlist-{}", std::process::id()));
        let path = dir.join("mods/wishlist.json");
        assert_eq!(Wishlist::load(&path), Wishlist::default(), "none yet: empty");
        let mut w = Wishlist::default();
        w.add(HOOD);
        w.add(SWORD);
        w.save(&path).unwrap();
        assert_eq!(Wishlist::load(&path), w);
        assert!(std::fs::read_to_string(&path).unwrap().contains("\"kind\": 5"));
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Wishlist::load(&path), Wishlist::default(), "a broken file is an empty list");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
