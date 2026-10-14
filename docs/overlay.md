# The mod overlay and the blacksmith wishlist

The first milestone of the mod layer in `roadmap.md`: an overlay drawn over the game, a read-only view of the game's memory for mods,
and a blacksmith wishlist built on the companion app's reverse engineering.

## Using it

- **F1**, or **Back (View) + right shoulder button (RB)** on a gamepad, opens and closes it. While it is open, the game gets no input
  at all: keyboard, mouse, gamepad and touch go to the overlay.
  - **On a gamepad:** D-pad down and up move between controls, A presses, and B closes it.
  - **The combination's buttons are kept from the game until they are let go.** Back pressed on its own still reaches the game.
- **`MH3U_OVERLAY=off`** turns the overlay off entirely.
- **When closed it costs one atomic load per frame**, and egui does not run. While the wishlist has something on it, the live save
  is also looked at four times a second, from the frame snapshot (a 35 KB copy), for the notices. Searching for it
  happens on a thread of its own.
- **The wishlist window:**
  1. Pick an equipment kind and type part of a name, then press `+` on a piece.
  2. Each wished piece shows what it is still missing. Open it for have/need per material (green when there is enough) and, for a
     the route the materials are for (see "Which recipe" below).
  3. The shopping list adds up everything on the list against the item box and pouch together. Two pieces that share a material can
     each be ready on their own and still be short together.
  4. `✖` takes a piece off the list.
- **Each hunter has a list of their own**, kept in `<build dir>/mods/wishlists/<hunter name>.json` (next to the translated C:
  `$Out` in `win-run.ps1`, `--out` in `run.sh`). See "Whose wishlist" below.
- **When a wished piece becomes craftable**, a notice shows at the top right for six seconds, whether the overlay is open or not, and
  `[overlay] craftable now: …` is logged.

**Run 26:** the overlay flickered, and its filter box could not be typed in. On frames when the window had sent no events, the game
thread made egui's input from nothing, with a scale of 1. On a scaled Windows display that alternated with the real scale, so
everything moved every frame and the text box could not keep focus. Now a frame with no events reuses the last input (scale, size,
focus) without its events. egui also hears every window event while closed, so it knows the window's size and scale before it first
opens. F1 toggles on its first press only (not on repeats or the release). The gamepad combination toggles only on the press that
completes it.

## Fullscreen

`--fullscreen` on `mh3u-tools run` (`-Fullscreen` in `win-run.ps1`, `--fullscreen` in `run.sh`), or `MH3U_FULLSCREEN=1`, starts in
borderless fullscreen on the window's monitor.
- **F11 or Alt+Enter** switches between fullscreen and the window while playing. These keys never reach the game, and F1 stays the
  overlay's.
- **The picture keeps its proportions**, with black bars at the sides or top; it is never stretched. The surface is set up again at
  the new size on the next frame.
- **The cursor is hidden in fullscreen**, except while the overlay is open.

## How it works

- **The overlay** (`mh3u-gfx/src/overlay.rs`) is `egui` 0.36 (`egui-wgpu` for wgpu 30, `egui-winit` for winit 0.30).
  - The window's thread feeds window events to `egui-winit` while the overlay is open, and hands the input to the game thread.
  - The game thread runs egui and draws it in its own pass at the end of the present, over the game's picture.
  - The clipboard and the cursor icon are not passed back to the window yet.
- **Read-only memory for mods** (`mh3u-rt/src/mods.rs`): a mod never reads guest memory itself.
  - It names the ranges it wants (`mods::watch`, up to 1 MiB). At each buffer swap the game thread copies them into a `Snapshot`.
    The game thread holds the baton there, so no game code runs during the copy and it cannot tear.
  - `mods::search(ranges, finder)` reads memory on a thread of its own while the game runs, with atomic word loads. A value can be
    stale or mixed, so what it finds are only candidates, checked again from a snapshot. It rests 1 ms per 8 MiB, skips pages the
    game never touched (on Windows reading them would commit memory), and never holds up the game thread or the audio thread.
- **The live save** (`mh3u-mods/src/live.rs`). As the companion's `docs/live.md` describes, the game holds three copies of the
  hunter's save data laid out like the `user1` file. The live one's header points at itself (the word at +0x08 is its address minus
  0xc, the word at +0x24 that plus 0x34); the other two have blank headers.
  1. **Search:** the port lays out guest memory as the console and Cemu do (MEM2 at 0x1080_0000, MEM1 at 0xf400_0000, the
     foreground bucket at 0xe000_0000; big-endian). All three are searched for both signs the companion uses: the self-pointing
     header, and the hunters' names (read from `user1`-`user3` in the game's save folder) at +0x2b, with the zero after them.
  2. **Check:** the candidates are copied at a frame and chosen, in this order:
     1. a header that points at itself;
     2. a hunter's name with the word at +0x24 pointing at itself. Run 26 showed the port's live block with +0x08 zero where Cemu has
        the pointer; the +0x24 word still pointed at itself.
     3. a hunter's name and a non-blank header. Run 26's first pick by this rule went away, a temporary copy.

     Name hits that are not at a word boundary are dropped.
  3. **Follow:** the block chosen is followed from the snapshot of each frame.
  4. **Retry:** a search that finds nothing, or a save that goes away (the hunter unloaded), is tried again after 5, 10, 20, 40,
     then every 60 seconds.

  Each reason is logged once (`[overlay] no live save: …`, listing the candidates' header words if there were any), and the overlay
  shows it with the time to the next try.
- **The game data** (`mh3u-mods/src/data.rs`): item and equipment names (the English text archive) and the forging and upgrade
  recipes (the executable's data section), read from the dump's base game on a background thread at start.

## The companion's code

[glinesbdev/mh3u-companion](https://github.com/glinesbdev/mh3u-companion) is public domain, like this project. Its crate
`mh3u-core` would be the natural dependency, but it does not build for Windows: its `/proc` memory reader uses `std::os::unix`, and
that is not optional. So the six modules the wishlist needs are copied into `crates/mh3u-mods/src/companion/`, from commit `563aa22`:
`arc`, `inflate`, `gmd`, `rpx`, `recipes` and `save`. Their header says where they come from and what changed (the tests and one
function that needs the drop tables were left out). If the companion puts its `/proc` reader behind `cfg(unix)` or a feature, the
copy can become a git dependency (the repository is private, so cargo would fetch it with `net.git-fetch-with-cli`).

## Run 27 (PR #11, merged in fab2efe): it works

- **The live save was found** at `0x2e686b00` (Shamus), by the hunter's name with the word at +0x24 pointing at itself. That is the
  port's form of the companion's header; the word at +0x08 is 0 here. The first search, before the hunter was loaded, found nothing
  and the next one 5 s later found it.
- **The game data** loaded once from the dump: 1551 items, 2155 recipes, 1437 upgrades.
- **Item counts, have/need per material and the shopping list are correct** against the game.
- **No flicker**: the filter box can be typed in. No stutter or audio pops while the search runs.
- **Fullscreen works**, with the picture's proportions kept.
- **Still open:** what the word at +0x08 holds in the port (0) against Cemu (a pointer); the clipboard and cursor icon for egui;
  hooks patched in at translation time.

## Hook events (PR #16)

When the build has the hooks from `mods/hooks.txt` (`docs/hooks.md`), the overlay registers callbacks on `item_gain`, `item_to_pouch`,
`menu_open`, `menu_page`, `pouch_remove_slot`, `money_add`, `craft_equipment`, `craft_jewel` and `quest_end` (`mh3u-gfx/src/events.rs`).
- **The callbacks only queue an event** (the item id, where it goes from the lr, the money change). They read nothing but
  their registers.
- **When the save is read:** on the 3 frames after an event (a hook runs before the game writes), and every 2 s as a
  fallback. Without the hooks, it is read every frame, as before.
- **The "craftable now" toast** comes from that read, so it appears within a few frames of the item gain.
- **The Events window** shows the last 8 events (`+ Spongy Hide -> pouch`, `- Potion used`, `item menu opened`, `-750z`, `crafted equipment`,
  `quest over`). An item used is `item_to_pouch` with a negative count (run 39); the `item_gain` call it makes is not counted as a
  gain.
  The log says `[overlay] N hooks feed the overlay` when they are on.
- **Quest pickups (PR #16 test):** during a quest, the game keeps pickups apart from the save and merges them into it at the
  quest's end. So the save's counts don't move during a quest.
  - Each `item_gain` therefore counts +1 for its item until the save's count of that item rises past what it was at the gain,
    or until `quest_end`. Out of a quest, the save gets the item at once, so nothing is counted twice.
  - The wishlist lists these as "Counted, not in the save yet (+N this quest)".
  - It assumes 1 per gain (the count argument isn't known), and the quest pouch itself isn't read.
- **Arrows:** the event text uses `->`, because egui's default font has no `→`.

**Confirmed (user, after PR #16 was merged):** quest pickups are counted at once during the quest, and the counts are right and
stay so after the quest ends (no double counting after the merge).

## The wishlist in the game's item menu (opt-in)

With `MH3U_NATIVE_TEXT=1` (`win-run.ps1 -NativeText`), the overlay's wishlist status also shows in the item menu, in the last
empty pouch slot, drawn by the game (`Wishlist: 1 of 2 ready`, `Wishlist: need 4 items`). The line is worked out where the
craftable notices are (`overlay.rs`, `native_line`), so it follows the same reads of the live save. `native-menu.md` has how
it works. With `MH3U_NATIVE_MENU=1` (`-NativeMenu`), the same place also gives the native Mods page one line a piece
(`page_lines`: `Name: ready` or `Name: need N`); hold Right Shift in the start menu and choose Mods.

## Which recipe

Most weapons can be made two ways, and the routes need different materials. The executable has a forging table (24-byte records:
piece, four item/count slots) and an upgrade table (28-byte records, by weapon: its children, and the materials to upgrade into it
from its parent). Cataclysm Sword (great sword 87) is forged with Spongy Hide x5, R.Ludroth Claw x3, R.Ludroth Crest x2 and Gobul
Fin x4, but upgraded from great sword 86 with Spongy Hide x4, Claw x2, Crest x1 and Gobul Spike x4.

`GameData::making_for(kind, id, owns)` picks what the blacksmith shows the hunter: the upgrade when the save owns a weapon it is
upgraded from, else the forging recipe, else the upgrade. The overlay and the Mods page use it; the wishlist window says which
route it chose ("upgrade from X (owned)", or "forge (or upgrade from X, not owned)"). Without a save it falls back to forging.

Before this, forging always won. Of the 592 weapons with a forging recipe, 457 can also be upgraded, and all 457 need different
materials by upgrade, so any of them was wrong for a hunter on the upgrade path. Armour has only forging. The update's tables are
byte-for-byte the base game's, so reading the base is not a cause.

## Whose wishlist

The live save holds no slot number, so a list belongs to the hunter's name (the name read from the live save, the one the search
found it by). The file name is the name with anything but ASCII letters, digits, `-` and `_` written as `~` and hex bytes.

- **In game or not:** the save alone does not tell. At the title screen ("PRESS A") the slot's data is already in memory (PR #25's
  first test showed Shamus's list there). So the overlay accepts a hunter only while the game's current area is an in-game one.
  It reads the area as `mh3u-rt/src/scene.rs` describes: `sArea`'s areas and each one's class. The village, harbor, quests, lobby,
  results and arena count as in game. The title (`aStartMenu`), boot, options, gallery and data transfer count as not. A change is
  logged as `[scene] InGame (areas: aVillage)`. Leaving for the title counts as unloading. `MH3U_SCENE_CHECK=off` skips the check.
- **Before a hunter is loaded** (title screen, slot select, or while the search is still running): the overlay says "No hunter
  loaded" and offers no picker or list. The native Mods page and the item-menu line say "No hunter loaded" too. No list is read.
- **A hunter is loaded:** once the live save is found and parsed, that hunter's list is read and everything works out from it.
- **The hunter goes away** (back to the title, or the block no longer parses as a save): the list is dropped. The craftable notices,
  the quest's not-yet-saved gains and the native pages are all reset, and the search starts again.
- **Another hunter in the same block** (a slot loaded without the old one going away first): the name changes, so the old list
  is dropped and the new hunter's is read.
- **Upgrading from one shared list:** before this, `mods/wishlist.json` was one list for every hunter, read at start and shown
  before any hunter was loaded. The first hunter loaded after the update takes it over, and it is renamed
  `wishlist.json.migrated` so no other hunter gets it.
- **Known limit:** two save slots with the same hunter name share a list.
- **Searching with the overlay closed:** the overlay looks for the live save now and then while any hunter's list is non-empty.
