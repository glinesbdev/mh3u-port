//! Armor and weapon crafting recipes, read from the game executable's data section.
//!
//! Every recipe is a 24-byte big-endian record: a 4-byte header (byte 0 = `flag`, low u16 = piece
//! id), four (item id, count) slots of 4 bytes each (empty slots are zero), and a 4-byte tail
//! (byte 0 = `tier`). Each equipment kind has its own contiguous table; the offsets below are
//! for the US v32 executable and are checked on load. Zenny costs are not stored here.

use crate::companion::save::ItemStack;
use anyhow::{Result, bail};
use std::collections::HashMap;

/// Virtual address of the `.data` section that holds the tables.
pub const DATA_SECTION_ADDR: u32 = 0x1010_a000;

const RECORD_LEN: usize = 24;
const MAX_ITEM_ID: u16 = 1550;

/// (equipment kind, byte offset of the table within the data section).
const TABLES: &[(u8, usize)] = &[
    (5, 0x314f0),  // head
    (1, 0x338a8),  // body
    (2, 0x35c90),  // arms
    (3, 0x37eb0),  // waist
    (4, 0x3a190),  // legs
    (7, 0x462fc),  // great sword
    (8, 0x466bc),  // sword & shield
    (9, 0x46824),  // hammer
    (10, 0x46adc), // lance
    (11, 0x46d4c), // heavy bowgun
    (13, 0x4707c), // light bowgun
    (14, 0x47334), // long sword
    (15, 0x475a4), // switch axe
    (16, 0x477cc), // gunlance
    (17, 0x47a24), // bow
    (18, 0x47e2c), // dual blades
    (19, 0x480b4), // hunting horn
];

/// Weapon upgrade tables: (equipment kind, byte offset, record count). Each 28-byte record, indexed by
/// weapon id, is: 4 child ids (u16), the upgrade recipe (4 item/count slots), then 2 more child ids.
const UPGRADE_RECORD_LEN: usize = 28;
const UPGRADE_TABLES: &[(u8, usize, usize)] = &[
    (7, 0x3c600, 136),  // great sword
    (8, 0x3d4e0, 141),  // sword & shield
    (9, 0x3e44c, 135),  // hammer
    (10, 0x3f310, 146), // lance
    (11, 0x40308, 92),  // heavy bowgun
    (13, 0x40d18, 97),  // light bowgun
    (14, 0x417b4, 115), // long sword
    (15, 0x42448, 114), // switch axe
    (16, 0x430c0, 115), // gunlance
    (17, 0x43d54, 116), // bow
    (18, 0x44a04, 126), // dual blades
    (19, 0x457cc, 104), // hunting horn
];

/// How a weapon is upgraded from the weapon(s) it descends from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Upgrade {
    /// Materials for the upgrade; empty if the weapon can't be reached by upgrading.
    pub materials: Vec<ItemStack>,
    /// Ids of the weapons (same kind) this one can be upgraded from.
    pub parents: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    pub materials: Vec<ItemStack>,
    /// Header byte 0: `1` marks the starting gear (always offered by the blacksmith).
    pub flag: u8,
    /// Tail byte 0: 1 on starting gear, 2 on ordinary pieces, 0 on special village and event pieces (Yukumo armor and the like).
    pub tier: u8,
}

fn be16(d: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([d[o], d[o + 1]])
}

/// Parse the record at `o`, or `None` if it doesn't have the shape of a recipe.
fn parse_record(d: &[u8], o: usize) -> Option<(u16, Recipe)> {
    let r = d.get(o..o + RECORD_LEN)?;
    if r[1] != 0 || r[0] > 2 || r[21..24] != [0, 0, 0] || r[20] > 4 {
        return None;
    }
    let mut materials = Vec::new();
    for slot in r[4..20].as_chunks::<4>().0 {
        let (id, count) = (be16(slot, 0), be16(slot, 2));
        if id > MAX_ITEM_ID || count > 99 || (id == 0) != (count == 0) {
            return None;
        }
        if id != 0 {
            materials.push(ItemStack { id, count });
        }
    }
    if materials.is_empty() {
        return None;
    }
    Some((
        be16(r, 2),
        Recipe {
            materials,
            flag: r[0],
            tier: r[20],
        },
    ))
}

/// Parse every table from the data section into a map keyed by (equipment kind, piece id).
pub fn parse(data: &[u8]) -> Result<HashMap<(u8, u16), Recipe>> {
    let mut out = HashMap::new();
    for &(kind, start) in TABLES {
        let mut o = start;
        let mut found = 0;
        while o + RECORD_LEN <= data.len() {
            if data[o..o + RECORD_LEN].iter().all(|&b| b == 0) {
                o += RECORD_LEN; // padding between table halves
                continue;
            }
            let Some((id, recipe)) = parse_record(data, o) else { break };
            out.insert((kind, id), recipe);
            found += 1;
            o += RECORD_LEN;
        }
        if found < 10 {
            bail!("recipe table for kind {kind} at {start:#x} has only {found} valid records; unsupported executable?");
        }
    }
    Ok(out)
}

/// The piece ids of each kind's recipe table in the order of its rows. The blacksmith's menu lists pieces in this order, not by id.
pub fn row_order(data: &[u8]) -> HashMap<u8, Vec<u16>> {
    let mut out = HashMap::new();
    for &(kind, start) in TABLES {
        let (mut o, mut ids) = (start, Vec::new());
        while o + RECORD_LEN <= data.len() {
            if data[o..o + RECORD_LEN].iter().all(|&b| b == 0) {
                o += RECORD_LEN;
                continue;
            }
            let Some((id, _)) = parse_record(data, o) else { break };
            ids.push(id);
            o += RECORD_LEN;
        }
        out.insert(kind, ids);
    }
    out
}

/// Parse the weapon upgrade tables into a map keyed by (equipment kind, weapon id).
pub fn parse_upgrades(data: &[u8]) -> Result<HashMap<(u8, u16), Upgrade>> {
    let mut out: HashMap<(u8, u16), Upgrade> = HashMap::new();
    for &(kind, start, count) in UPGRADE_TABLES {
        let mut children: Vec<(u16, u16)> = Vec::new(); // (parent, child)
        for id in 0..count {
            let o = start + id * UPGRADE_RECORD_LEN;
            let Some(r) = data.get(o..o + UPGRADE_RECORD_LEN) else {
                bail!("upgrade table for kind {kind} runs past the data section; unsupported executable?");
            };
            let mut materials = Vec::new();
            for slot in r[8..24].as_chunks::<4>().0 {
                let (item, n) = (be16(slot, 0), be16(slot, 2));
                if item > MAX_ITEM_ID || n > 99 || (item == 0) != (n == 0) {
                    bail!("invalid upgrade record for kind {kind} id {id}; unsupported executable?");
                }
                if item != 0 {
                    materials.push(ItemStack { id: item, count: n });
                }
            }
            let child_ids = (0..4).map(|i| be16(r, i * 2)).chain((0..2).map(|i| be16(r, 24 + i * 2)));
            children.extend(child_ids.filter(|&c| c != 0).map(|c| (id as u16, c)));
            out.insert(
                (kind, id as u16),
                Upgrade {
                    materials,
                    parents: Vec::new(),
                },
            );
        }
        for (parent, child) in children {
            if let Some(u) = out.get_mut(&(kind, child)) {
                u.parents.push(parent);
            }
        }
    }
    if !out.values().any(|u| !u.materials.is_empty()) {
        bail!("no upgrade recipes found; unsupported executable?");
    }
    Ok(out)
}
