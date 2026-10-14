//! The game data the wishlist needs, read from the dump: the names of items and equipment (the game's English text) and the recipes
//! (the executable's data section). A slim version of the companion's `GameData`.

use crate::companion::{arc::Arc, gmd, recipes, rpx, save::ItemStack};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

/// The archive with the English text, in the dump.
const TEXT_ARCHIVE: &str = "content/nativeCafe/arc/ID/ID_arena_eng.arc";

/// Equipment kinds (the save's kind byte), their names and the text file of their pieces' names. Talismans (6) have no recipes.
pub const KINDS: &[(u8, &str, &str)] = &[
    (5, "Head", "Helm_eng"),
    (1, "Body", "Body_eng"),
    (2, "Arms", "Arm_eng"),
    (3, "Waist", "Waist_eng"),
    (4, "Legs", "Leg_eng"),
    (7, "Great Sword", "Lsword_eng"),
    (14, "Long Sword", "Lsword2_eng"),
    (8, "Sword & Shield", "Sword_eng"),
    (18, "Dual Blades", "WSword_eng"),
    (9, "Hammer", "Hammer_eng"),
    (19, "Hunting Horn", "Pipe_eng"),
    (10, "Lance", "Lance_eng"),
    (16, "Gunlance", "Gunlance_eng"),
    (15, "Switch Axe", "Axe_eng"),
    (13, "Light Bowgun", "Lbg_eng"),
    (11, "Heavy Bowgun", "Hbg_eng"),
    (17, "Bow", "Bow_eng"),
];

/// How a piece is made: forged from materials, or upgraded from another weapon (which it then needs too).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Making {
    pub materials: Vec<(u16, u16)>,
    /// For an upgrade: the weapons it can be upgraded from.
    pub from: Vec<u16>,
}

pub struct GameData {
    pub items: Vec<String>,
    pub names: HashMap<u8, Vec<String>>,
    pub recipes: HashMap<(u8, u16), recipes::Recipe>,
    pub upgrades: HashMap<(u8, u16), recipes::Upgrade>,
}

fn stacks(s: &[ItemStack]) -> Vec<(u16, u16)> {
    s.iter().map(|m| (m.id, m.count)).collect()
}

impl GameData {
    /// From the base game's folder (with `code` and `content`).
    pub fn load(base: &Path) -> Result<GameData> {
        let path = base.join(TEXT_ARCHIVE);
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let arc = Arc::parse(&bytes)?;
        let names = |file: &str| -> Result<Vec<String>> {
            let want = format!("GUI\\font\\{file}");
            let entry = arc
                .entries
                .iter()
                .find(|e| e.name == want)
                .with_context(|| format!("{want} is not in the archive"))?;
            Ok(gmd::parse(&arc.read(entry)?)?.into_iter().map(|n| n.trim().to_string()).collect())
        };
        let rpx = std::fs::read_dir(base.join("code"))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| p.extension().is_some_and(|x| x == "rpx"))
            .context("no .rpx in the dump's code folder")?;
        let data = rpx::section_at(&std::fs::read(&rpx)?, recipes::DATA_SECTION_ADDR)?;
        Ok(GameData {
            items: names("Item00_eng")?,
            names: KINDS.iter().map(|&(k, _, f)| Ok((k, names(f)?))).collect::<Result<_>>()?,
            recipes: recipes::parse(&data)?,
            upgrades: recipes::parse_upgrades(&data)?,
        })
    }

    pub fn item_name(&self, id: u16) -> &str {
        self.items.get(usize::from(id)).map_or("?", String::as_str)
    }

    /// The name of a piece that exists (not empty, not a `DUMMY`).
    pub fn piece_name(&self, kind: u8, id: u16) -> Option<&str> {
        let n = self.names.get(&kind)?.get(usize::from(id))?;
        (!n.is_empty() && !n.starts_with("DUMMY")).then_some(n.as_str())
    }

    /// How a piece is made when nothing is known of what the hunter owns: its forging recipe if it has one, else its upgrade.
    pub fn making(&self, kind: u8, id: u16) -> Option<Making> {
        self.making_for(kind, id, |_, _| false)
    }

    /// The piece's forging recipe and its upgrade (from the weapons it descends from), each if it has one. Most weapons have
    /// both, with different materials (an upgrade costs less and takes the parent weapon).
    pub fn routes(&self, kind: u8, id: u16) -> (Option<Making>, Option<Making>) {
        let forge = self.recipes.get(&(kind, id)).map(|r| Making {
            materials: stacks(&r.materials),
            from: Vec::new(),
        });
        let upgrade = self
            .upgrades
            .get(&(kind, id))
            .filter(|u| !u.materials.is_empty() && !u.parents.is_empty())
            .map(|u| Making {
                materials: stacks(&u.materials),
                from: u.parents.clone(),
            });
        (forge, upgrade)
    }

    /// How the hunter would make the piece, as the blacksmith offers it: the upgrade when they own a weapon it is upgraded from
    /// (`owns(kind, id)`), else the forging recipe, else the upgrade.
    pub fn making_for(&self, kind: u8, id: u16, owns: impl Fn(u8, u16) -> bool) -> Option<Making> {
        choose(self.routes(kind, id), |from| from.iter().any(|&f| owns(kind, f)))
    }

    /// The pieces of a kind that can be made, by id, with their names.
    pub fn pieces(&self, kind: u8) -> Vec<(u16, &str)> {
        let n = self.names.get(&kind).map_or(0, Vec::len).min(usize::from(u16::MAX)) as u16;
        (1..n)
            .filter_map(|id| Some((id, self.piece_name(kind, id)?)))
            .filter(|&(id, _)| self.making(kind, id).is_some())
            .collect()
    }
}

/// The route to show: the upgrade when a parent is owned, else forging, else the upgrade.
pub fn choose((forge, upgrade): (Option<Making>, Option<Making>), owns_parent: impl Fn(&[u16]) -> bool) -> Option<Making> {
    match (forge, upgrade) {
        (_, Some(u)) if owns_parent(&u.from) => Some(u),
        (Some(f), _) => Some(f),
        (None, u) => u,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(items: &[(u16, u16)], from: &[u16]) -> Making {
        Making {
            materials: items.to_vec(),
            from: from.to_vec(),
        }
    }

    /// Cataclysm Sword (great sword 87) in the update's tables: forged with 4 Gobul Fin (552), upgraded from 86 with 4 Gobul
    /// Spike (555). The blacksmith shows the upgrade to a hunter who owns 86.
    #[test]
    fn the_upgrade_is_shown_when_the_parent_is_owned() {
        let forge = m(&[(506, 5), (512, 3), (515, 2), (552, 4)], &[]);
        let upgrade = m(&[(506, 4), (512, 2), (515, 1), (555, 4)], &[86]);
        let routes = || (Some(forge.clone()), Some(upgrade.clone()));
        assert_eq!(choose(routes(), |f| f.contains(&86)), Some(upgrade.clone()));
        assert_eq!(choose(routes(), |_| false), Some(forge.clone()));
        assert_eq!(choose((None, Some(upgrade.clone())), |_| false), Some(upgrade.clone()));
        assert_eq!(choose((Some(forge.clone()), None), |_| true), Some(forge));
        assert_eq!(choose((None, None), |_| true), None);
    }

    /// With the game's files (`MH3U_GAME_DIR`): the names and recipes are read and fit together.
    #[test]
    fn the_dump_gives_names_and_recipes() {
        let Some(dir) = std::env::var_os("MH3U_GAME_DIR") else {
            eprintln!("skipped: MH3U_GAME_DIR is not set");
            return;
        };
        let dump = mh3u_dump::Dump::locate(Path::new(&dir)).unwrap();
        let data = GameData::load(&dump.base).unwrap();
        assert!(data.items.len() > 1000);
        for &(kind, name, _) in KINDS {
            assert!(!data.pieces(kind).is_empty(), "no {name} can be made");
        }
        let (id, _) = data.pieces(5)[0];
        let m = data.making(5, id).unwrap();
        assert!(m.materials.iter().all(|&(item, count)| data.item_name(item) != "?" && count > 0));
    }
}
