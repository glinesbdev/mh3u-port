use anyhow::{Result, bail};

pub const SAVE_LEN: usize = 35364;
const NAME_OFFSET: usize = 0x2b;
const NAME_LEN: usize = 0x15;
pub(crate) const ZENNY_OFFSET: usize = 0x49;
/// The Resource Points: a big-endian u32 (14 in every save until the points went to 20, and the number moved with them).
pub const RESOURCE_POINTS_OFFSET: usize = 0x5b50;
pub(crate) const POUCH_OFFSET: usize = 0xd0;
pub(crate) const POUCH_SLOTS: usize = 24;
pub(crate) const BOX_OFFSET: usize = 0x1b0;
pub(crate) const BOX_SLOTS: usize = 1000;
pub(crate) const EQUIP_OFFSET: usize = 0x1150;
pub(crate) const EQUIP_SLOTS: usize = 1000;
pub(crate) const EQUIP_LEN: usize = 16;
/// u16 pointer to the equipment box slot of the worn weapon; 0xffff = none.
pub(crate) const WORN_WEAPON_OFFSET: usize = 0xc0;
/// Five u16 pointers (body, arms, waist, legs, head) into the equipment box slots; 0xffff = nothing worn.
pub(crate) const WORN_OFFSET: usize = 0xc2;
pub(crate) const WORN_SLOTS: usize = 5;
/// The u16 pointer, right after the five armor ones, to the worn talisman's equipment box slot; 0xffff = none. The 16 bytes before the
/// pointers (from 0xb0) are a copy of the worn talisman's record (kind 6 and zeros when none is worn). Found by comparing a save with
/// a talisman worn (slot 17) and one without.
pub(crate) const WORN_TALISMAN_OFFSET: usize = 0xcc;
/// The guild card: play time in seconds (u32, right after the zenny), and the quests done in the village and in the guild hall (one
/// byte each, in the guild card's own record). Found by comparing saves with the numbers on the guild card, which matched (docs/formats.md).
const PLAY_SECONDS_OFFSET: usize = 0x4c;
/// The guild card's weapon usage: twelve u16 counts of the quests done with each weapon type, first for the village (from here) and then
/// for the guild hall (twelve u16 from `0x7b60`). The screen shows the two added together. Found by doing a Sword & Shield hall quest
/// after four with the great sword (only the second type's hall count moved), then a hammer hall quest (only the third moved) a bow village quest (the tenth entry of the
/// village list) a long sword village quest (the seventh) a dual blades village quest (the eleventh) a switch axe village quest (the eighth), a gunlance village quest (the ninth), a hunting horn
/// village quest (the twelfth) a lance village quest (the fourth) a light bowgun village quest (the sixth) and a heavy bowgun village quest (the fifth). All twelve types
/// were seen, in the order of the equipment kinds. the order of the other eight follows the equipment kinds
/// by guess, which the four that are known agree with.
const WEAPON_USAGE_VILLAGE: usize = 0x7b48;
const WEAPON_USAGE_GUILD: usize = 0x7b60;
/// The equipment kind of each of the twelve types, in that order.
pub const WEAPON_USAGE_KINDS: [u8; 12] = [7, 8, 9, 10, 11, 13, 14, 15, 16, 17, 18, 19];

/// The guild card's title is two big-endian u32 from here: the first is a word (its number in the game's `CardTitle` text, "Noob" is 1 and
/// "Fledgling" 4); in the second the top byte joins the words (0: nothing, else the word numbered 612 plus it: 6 is "to", 1 "of", 2 "and")
/// and the rest is the second word ("Hunter" is 6, "Excited" 51). Found by changing the title from "Fledgling Hunter" to "Noob to Excited".
const CARD_TITLE: usize = 0x7a28;
/// The Shakalaka sidekick's record starts with a byte that is 0x80 and the number of the mask it wears (0 before the sidekick joined).
const SIDEKICK_MASK: usize = 0x6f68;
/// The greeting on the card: text up to a NUL from here. Its longest length is not known.
const CARD_GREETING: usize = 0x7ad0;
const GREETING_MAX: usize = 48;
/// The hunter rank: a big-endian u16 at 0x5a48, next to the quest points at 0x5a46. 0 for a hunter who never went to the guild hall,
/// 1 for WornTester until the urgent quest "The Fisherman's Friend", 2 after it (the card agreed each time).
const HUNTER_RANK_OFFSET: usize = 0x5a48;
/// The guild card's Hunter's Journal: the last quests done, newest first, in records of 0xa0 bytes from here. A record has the quest's
/// title as text from byte 3 and, at byte 0x34, the date it was done on as day, month and a u16 year (`04 0a 07 ea` is 4 October 2026).
/// There are nine records; the oldest of the ten title strings in that area has no date and is left over. A quest done with a Shakalaka
/// sidekick took two records, the second with no title. The other fields (a u32 at 0x38, small numbers at 0x2e and 0x3c) are not understood.
const JOURNAL: usize = 0x7c00;
const JOURNAL_RECORD: usize = 0xa0;
const JOURNAL_RECORDS: usize = 9;
const VILLAGE_QUESTS_OFFSET: usize = 0x7a4d;
const GUILD_QUESTS_OFFSET: usize = 0x7a51;
/// One u16 per monster from here; entry `n` is for the monster with name id `n + 6`: how many times it was killed or captured.
const HUNTED_OFFSET: usize = 0x57a0;
const HUNTED_FIRST_MONSTER: u16 = 6;
const HUNTED_COUNT: usize = 90;

/// A stack of items: `id` indexes the game's item table, `count` is the quantity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemStack {
    pub id: u16,
    pub count: u16,
}

/// One equipment box slot (16 bytes). `kind` selects the equipment category (armor slot,
/// weapon type, talisman) and `id` indexes that category's name table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Equipment {
    /// Position in the equipment box; worn gear is stored as a pointer to this.
    pub slot: u16,
    pub kind: u8,
    /// Byte 1; its meaning (slot/upgrade state) isn't decoded yet.
    pub upgrade: u8,
    pub id: u16,
    /// Bytes 4..16: a talisman's skills (see [`Equipment::talisman_skills`]); the rest is not decoded.
    pub raw_tail: [u8; 12],
}

impl Equipment {
    /// A talisman's skills as (skill tree id, points): `(id, points)` byte pairs from byte 4, up to a zero id. Only the first pair
    /// has been seen (one Pawn Talisman: Auto-Guard +10, confirmed in game), so further pairs are a guess; anything but a
    /// talisman has none.
    pub fn talisman_skills(&self) -> Vec<(u8, i8)> {
        if self.kind != 6 {
            return Vec::new();
        }
        self.raw_tail[..4]
            .chunks(2)
            .take_while(|p| p[0] != 0)
            .map(|p| (p[0], p[1] as i8))
            .collect()
    }
}

impl Equipment {
    /// A talisman's gem slots (0 to 3): the record's second byte. Found by writing 3 into it in the running game, which gave the
    /// talisman 3 slots; a Pawn Talisman has 0. Anything but a talisman has none (its second byte is its upgrade state).
    pub fn talisman_slots(&self) -> u8 {
        if self.kind == 6 { self.upgrade.min(3) } else { 0 }
    }

    /// The decorations socketed in a talisman, as numbers into the game's decoration table (see `decorations`), one per slot in
    /// order. Seen so far: slot 1 of a 3-slot talisman held `0x0091` at bytes 8-9; slot 2 held `0x0015` at bytes 10-11 (confirmed); slot 3 held `0x0097` at bytes 12-13 (confirmed). Anything but a talisman has none here (see
    /// [`Equipment::decorations`]).
    pub fn talisman_decorations(&self) -> Vec<u16> {
        if self.kind != 6 {
            return Vec::new();
        }
        self.raw_tail[4..10]
            .chunks(2)
            .take(usize::from(self.talisman_slots()))
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect()
    }

    /// The decorations socketed in an armor piece or talisman, numbers into the game's decoration table, empty sockets skipped. An armor
    /// piece keeps them in the same place as a talisman (bytes 8-9 for a jewel in its first slot, seen on a Leather Headgear).
    pub fn decorations(&self) -> Vec<u16> {
        if !(1..=6).contains(&self.kind) {
            return Vec::new();
        }
        self.raw_tail[4..10]
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .filter(|&c| c != 0)
            .collect()
    }
}

/// One quest in the guild card's Hunter's Journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    pub title: String,
    /// (day, month, year), when the record has a date.
    pub date: Option<(u8, u8, u16)>,
}

#[derive(Debug, Clone)]
pub struct Save {
    pub hunter_name: String,
    pub zenny: u32,
    /// The Resource Points (see [`RESOURCE_POINTS_OFFSET`]).
    pub resource_points: u32,
    /// Time played, in seconds.
    pub play_seconds: u32,
    /// Quests done in the village, and in the guild hall (the guild card's two counts).
    pub village_quests: u8,
    pub guild_quests: u8,
    /// The hunter rank (HR) on the guild card: 0 before the guild hall, then 1 and up.
    pub hunter_rank: u16,
    /// Quests done with each weapon type, village and guild together, in the order of [`WEAPON_USAGE_KINDS`].
    pub weapon_uses: [u32; 12],
    /// The guild card's title as numbers into the game's title words: (first word, joining word, second word); see [`CARD_TITLE`].
    pub card_title: (u32, u8, u32),
    /// The guild card's greeting.
    pub greeting: String,
    /// The mask the Shakalaka sidekick wears, as a number into the game's list of masks (0 Acorn, 5 Fluffy); `None` until it joined.
    pub sidekick_mask: Option<u8>,
    /// The last quests done, newest first (the Hunter's Journal).
    pub journal: Vec<JournalEntry>,
    pub pouch: Vec<ItemStack>,
    pub item_box: Vec<ItemStack>,
    pub equipment_box: Vec<Equipment>,
    /// Monsters hunted, from monster 6 on (see `times_hunted`).
    pub hunted: Vec<u16>,
    /// Equipment box slots currently worn: the weapon first, then armor.
    pub worn_slots: Vec<u16>,
    /// The equipment box slot of the worn talisman.
    pub worn_talisman: Option<u16>,
}

fn be32(d: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

fn be16(d: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([d[o], d[o + 1]])
}

/// Read `slots` consecutive (id, count) pairs, dropping empty slots (count 0).
fn read_stacks(d: &[u8], offset: usize, slots: usize) -> Vec<ItemStack> {
    (0..slots)
        .map(|i| ItemStack {
            id: be16(d, offset + i * 4),
            count: be16(d, offset + i * 4 + 2),
        })
        .filter(|s| s.count > 0)
        .collect()
}

fn read_equipment(d: &[u8]) -> Vec<Equipment> {
    (0..EQUIP_SLOTS)
        .map(|i| {
            let o = EQUIP_OFFSET + i * EQUIP_LEN;
            Equipment {
                slot: i as u16,
                kind: d[o],
                upgrade: d[o + 1],
                id: be16(d, o + 2),
                raw_tail: d[o + 4..o + EQUIP_LEN].try_into().unwrap(),
            }
        })
        .filter(|e| e.kind != 0)
        .collect()
}

impl Save {
    /// True if the pouch and box together hold every material of the recipe.
    pub fn can_craft(&self, recipe: &crate::companion::recipes::Recipe) -> bool {
        recipe.materials.iter().all(|m| self.item_count(m.id) >= m.count as u32)
    }

    /// True if this equipment box entry is currently worn.
    pub fn is_worn(&self, e: &Equipment) -> bool {
        self.worn_slots.contains(&e.slot) || self.worn_talisman == Some(e.slot)
    }

    /// True if the equipment box holds this exact piece.
    pub fn owns_equipment(&self, kind: u8, id: u16) -> bool {
        self.equipment_box.iter().any(|e| e.kind == kind && e.id == id)
    }

    /// How many times the monster with this name id (the index in the game's monster names) has been killed or captured. Zero
    /// for monsters the table does not cover.
    pub fn times_hunted(&self, monster: u16) -> u16 {
        monster
            .checked_sub(HUNTED_FIRST_MONSTER)
            .and_then(|i| self.hunted.get(usize::from(i)))
            .copied()
            .unwrap_or(0)
    }

    /// Total quantity of an item across the pouch and the item box.
    pub fn item_count(&self, id: u16) -> u32 {
        self.pouch
            .iter()
            .chain(&self.item_box)
            .filter(|s| s.id == id)
            .map(|s| s.count as u32)
            .sum()
    }

    /// The weapon type the hunter has done the most quests with, as (equipment kind, quests); `None` before the first quest.
    pub fn most_used_weapon(&self) -> Option<(u8, u32)> {
        self.weapon_uses
            .iter()
            .enumerate()
            .filter(|&(_, &n)| n > 0)
            .max_by_key(|&(i, &n)| (n, std::cmp::Reverse(i)))
            .map(|(i, &n)| (WEAPON_USAGE_KINDS[i], n))
    }

    pub fn play_time(&self) -> String {
        let minutes = self.play_seconds / 60;
        format!("{} h {:02} min", minutes / 60, minutes % 60)
    }

    pub fn parse(d: &[u8]) -> Result<Save> {
        if d.len() != SAVE_LEN {
            bail!("unexpected save size {} (expected {SAVE_LEN})", d.len());
        }
        let name_bytes = &d[NAME_OFFSET..NAME_OFFSET + NAME_LEN];
        let end = name_bytes.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
        Ok(Save {
            hunter_name: String::from_utf8_lossy(&name_bytes[..end]).into_owned(),
            zenny: u32::from_be_bytes([0, d[ZENNY_OFFSET], d[ZENNY_OFFSET + 1], d[ZENNY_OFFSET + 2]]),
            resource_points: be32(d, RESOURCE_POINTS_OFFSET),
            play_seconds: u32::from_be_bytes([
                d[PLAY_SECONDS_OFFSET],
                d[PLAY_SECONDS_OFFSET + 1],
                d[PLAY_SECONDS_OFFSET + 2],
                d[PLAY_SECONDS_OFFSET + 3],
            ]),
            village_quests: d[VILLAGE_QUESTS_OFFSET],
            guild_quests: d[GUILD_QUESTS_OFFSET],
            hunter_rank: be16(d, HUNTER_RANK_OFFSET),
            weapon_uses: std::array::from_fn(|i| {
                u32::from(be16(d, WEAPON_USAGE_VILLAGE + 2 * i)) + u32::from(be16(d, WEAPON_USAGE_GUILD + 2 * i))
            }),
            card_title: {
                let (first, second) = (be32(d, CARD_TITLE), be32(d, CARD_TITLE + 4));
                (first & 0x00ff_ffff, (second >> 24) as u8, second & 0x00ff_ffff)
            },
            journal: (0..JOURNAL_RECORDS)
                .filter_map(|n| {
                    let at = JOURNAL + n * JOURNAL_RECORD;
                    let text = &d[at + 3..at + 0x2c];
                    let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
                    let title = String::from_utf8_lossy(&text[..end]).into_owned();
                    let (day, month, year) = (d[at + 0x34], d[at + 0x35], be16(d, at + 0x36));
                    (!title.is_empty()).then_some(JournalEntry {
                        title,
                        date: (year != 0).then_some((day, month, year)),
                    })
                })
                .collect(),
            greeting: {
                let text = &d[CARD_GREETING..CARD_GREETING + GREETING_MAX];
                let end = text.iter().position(|&b| b == 0).unwrap_or(GREETING_MAX);
                String::from_utf8_lossy(&text[..end]).into_owned()
            },
            sidekick_mask: (d[SIDEKICK_MASK] & 0x80 != 0).then_some(d[SIDEKICK_MASK] & 0x7f),
            pouch: read_stacks(d, POUCH_OFFSET, POUCH_SLOTS),
            item_box: read_stacks(d, BOX_OFFSET, BOX_SLOTS),
            equipment_box: read_equipment(d),
            hunted: (0..HUNTED_COUNT).map(|i| be16(d, HUNTED_OFFSET + 2 * i)).collect(),
            worn_slots: std::iter::once(be16(d, WORN_WEAPON_OFFSET))
                .chain((0..WORN_SLOTS).map(|i| be16(d, WORN_OFFSET + i * 2)))
                .filter(|&s| s != 0xffff)
                .collect(),
            worn_talisman: Some(be16(d, WORN_TALISMAN_OFFSET)).filter(|&s| s != 0xffff),
        })
    }
}
