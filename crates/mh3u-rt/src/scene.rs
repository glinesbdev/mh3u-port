//! Whether a hunter is in the game, or the game is at its title (read-only, for the overlay's wishlist).
//!
//! The live save can be in memory before a hunter is loaded: at the title screen ("PRESS A") the slot's data is already there.
//! The save alone does not say "in game". The game's current area does. The game is built on Capcom's MT Framework: its `sArea`
//! singleton (pointer at `AREA_SINGLETON`, update `.rpx`) keeps the areas being run in `mpArea[8]` at `+0x3840`, how many at `+0x3838`
//! (the `mpArea` property's getter `0x02b9dffc`, and `FUN_02117524`, which walks them calling each one's `getDTI`). Each area is an object of one class (`aStartMenu` for the title and slot select,
//! `aVillage` and so on in game), and its vtable's slot `+0x2c` is the class's `getDTI`. So the getDTI addresses below name the
//! class without running any code.
//!
//! Found statically: the DTI of each `a*` class (the static initialiser that names it), the `getDTI` that returns it, and the
//! vtable holding that. For `aStartMenu` this was checked against its constructor (`0x02025338`, vtable `0x10002ad8`).
//!
//! At every frame swap (`mods::on_frame`, with the baton) this reads the singleton, the 8 slots and each area's vtable slot. It
//! reads only pages the game has touched and only addresses that can be objects. Any slot showing a title-side class means
//! the title. Otherwise any in-game class means in game. Otherwise (no area, or a class not in either list) the last answer stands.

use crate::guest::Guest;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering::Relaxed};

/// `sArea`'s instance pointer.
pub const AREA_SINGLETON: u32 = 0x103d_2ef4;
/// `sArea::mpArea[8]`.
pub const AREAS: u32 = 0x3840;
pub const SLOTS: u32 = 8;
/// How many of them are in use (`mAreaPt`'s count; entries past it are stale).
pub const COUNT: u32 = 0x3838;
/// The vtable slot of `getDTI`.
pub const GET_DTI_SLOT: u32 = 0x2c;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    /// Not known yet (start-up, or no area seen).
    Unknown,
    /// The title, slot select and the menus reached from there: no hunter is playing.
    Title,
    /// A hunter is playing (village, harbor, quests, lobby, results).
    InGame,
}

/// The `getDTI` functions of the area classes (update `.rpx`), and which side each is on.
pub const CLASSES: &[(u32, &str, Option<Scene>)] = &[
    (0x0202_49ac, "aStartMenu", Some(Scene::Title)),
    (0x0202_a34c, "aBoot", Some(Scene::Title)),
    (0x0202_08cc, "aOption", Some(Scene::Title)),
    (0x0201_8a38, "aGalleryMenu", Some(Scene::Title)),
    (0x0201_73a4, "aGalleryDemo", Some(Scene::Title)),
    (0x0200_7c5c, "aDataTransfer", Some(Scene::Title)),
    (0x0200_8ed8, "aDLContents", Some(Scene::Title)),
    (0x0202_831c, "aVillage", Some(Scene::InGame)),
    (0x0202_6af8, "aVillageFarm", Some(Scene::InGame)),
    (0x0202_75bc, "aVillageHouse", Some(Scene::InGame)),
    (0x0202_8120, "aVillageSquare", Some(Scene::InGame)),
    (0x0201_e3bc, "aHarbor", Some(Scene::InGame)),
    (0x0201_b660, "aHarborGuild", Some(Scene::InGame)),
    (0x0201_c544, "aHarborRoomGuild", Some(Scene::InGame)),
    (0x0201_d37c, "aHarborRoomSquare", Some(Scene::InGame)),
    (0x0201_e1c0, "aHarborSquare", Some(Scene::InGame)),
    (0x0202_1f84, "aQuest", Some(Scene::InGame)),
    (0x0202_1cf0, "aQuestArea", Some(Scene::InGame)),
    (0x0201_6820, "aFreeQuest", Some(Scene::InGame)),
    (0x0201_6624, "aFreeQuestArea", Some(Scene::InGame)),
    (0x0202_6014, "aLobby", Some(Scene::InGame)),
    (0x0202_2c8c, "aResult", Some(Scene::InGame)),
    (0x0200_5f00, "aArenaMenu", Some(Scene::InGame)),
    (0x0202_4470, "aMenu", None),
    (0x0202_3b44, "aStaffroll", None),
    (0x0202_5888, "aViewer", None),
    (0x0200_5528, "aAppAreaBase", None),
];

static SCENE: AtomicU8 = AtomicU8::new(0);
/// The getDTI of the area that decided the scene last (for the log).
static DECIDED_BY: AtomicU32 = AtomicU32::new(0);

fn to_u8(s: Scene) -> u8 {
    match s {
        Scene::Unknown => 0,
        Scene::Title => 1,
        Scene::InGame => 2,
    }
}

/// The scene as last read.
pub fn scene() -> Scene {
    match SCENE.load(Relaxed) {
        1 => Scene::Title,
        2 => Scene::InGame,
        _ => Scene::Unknown,
    }
}

/// The class name of a getDTI address.
pub fn class_name(get_dti: u32) -> Option<&'static str> {
    CLASSES.iter().find(|c| c.0 == get_dti).map(|c| c.1)
}

/// The scene the areas' getDTI addresses show (`None`: they do not tell), and the one that told.
pub fn decide(get_dtis: &[u32]) -> Option<(Scene, u32)> {
    let side = |s| {
        get_dtis
            .iter()
            .copied()
            .find(|&f| CLASSES.iter().any(|c| c.0 == f && c.2 == Some(s)))
    };
    side(Scene::Title)
        .map(|f| (Scene::Title, f))
        .or_else(|| side(Scene::InGame).map(|f| (Scene::InGame, f)))
}

/// Whether `at` can be an object or vtable the game made: inside the game's data and heaps, on a page it has touched.
fn readable(g: &Guest, at: u32) -> bool {
    (0x1000_0000..0x5000_0000).contains(&at) && at.is_multiple_of(4) && g.touched(at) && g.touched(at + 3)
}

fn word(g: &Guest, at: u32) -> Option<u32> {
    readable(g, at).then(|| g.u32(at))
}

/// The getDTI of each area being run.
pub fn areas(g: &Guest) -> Vec<u32> {
    let Some(area) = word(g, AREA_SINGLETON).filter(|&p| p != 0) else {
        return Vec::new();
    };
    let count = word(g, area + COUNT).unwrap_or(0).min(SLOTS);
    (0..count)
        .filter_map(|i| {
            let obj = word(g, area + AREAS + 4 * i)?;
            let vtable = word(g, obj)?;
            word(g, vtable + GET_DTI_SLOT)
        })
        .collect()
}

/// Called at each frame swap: reads the areas and logs a change of scene.
pub(crate) fn on_frame(g: &Guest) {
    let found = areas(g);
    let Some((now, by)) = decide(&found) else { return };
    if SCENE.swap(to_u8(now), Relaxed) != to_u8(now) || DECIDED_BY.swap(by, Relaxed) != by {
        let names: Vec<String> = found
            .iter()
            .map(|&f| class_name(f).map_or_else(|| format!("{f:#x}"), str::to_string))
            .collect();
        eprintln!("[scene] {now:?} (areas: {})", names.join(", "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_wins_then_the_game() {
        assert_eq!(decide(&[]), None);
        assert_eq!(decide(&[0x0202_4470]), None, "aMenu alone does not tell");
        assert_eq!(decide(&[0x0202_831c]), Some((Scene::InGame, 0x0202_831c)));
        assert_eq!(decide(&[0x0202_831c, 0x0202_49ac]), Some((Scene::Title, 0x0202_49ac)));
        assert_eq!(class_name(0x0202_49ac), Some("aStartMenu"));
    }

    #[test]
    fn the_areas_are_read_through_the_singleton() {
        let mut memory = crate::memory::GuestMemory::reserve().expect("guest memory");
        let g = Guest::new(memory.as_mut_ptr());
        assert!(areas(&g).is_empty(), "nothing there: nothing read");
        let (area, obj, vtable) = (0x2000_0000, 0x2000_8000, 0x1000_2ad8);
        g.put_u32(AREA_SINGLETON, area);
        g.put_u32(area + COUNT, 2);
        g.put_u32(area + AREAS, 0);
        g.put_u32(area + AREAS + 4, obj);
        g.put_u32(obj, vtable);
        g.put_u32(vtable + GET_DTI_SLOT, 0x0202_49ac);
        assert_eq!(areas(&g), vec![0x0202_49ac]);
        g.put_u32(area + COUNT, 1);
        assert!(areas(&g).is_empty(), "past the count: stale");
        g.put_u32(area + COUNT, 2);
        on_frame(&g);
        assert_eq!(scene(), Scene::Title);
        g.put_u32(vtable + GET_DTI_SLOT, 0x0202_831c);
        on_frame(&g);
        assert_eq!(scene(), Scene::InGame);
    }
}
