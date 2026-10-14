# Roadmap

What comes after the current work. Order: graphics and performance, then audio (in progress, see `audio.md`), then the items below.

## Keyboard and mouse (PR #12)

WASD and mouse-look onto the virtual GamePad, a bindings file, and a grabbed cursor (`input.md`). Merged and tested: the bindings and camera
settings remap correctly. It is a bit awkward only because the game is built for a controller. Later, with translation-time hooks:
- mouse-friendly features (direct mouse-look on the camera, the wheel for the item bar, touch-screen actions on keys);
- a rebinding screen in the overlay.

## After audio: mod hooks and an overlay

**First milestone done in PR #11** (merged in fab2efe; confirmed in run 27, see `overlay.md`): the overlay (F1 or Back + RB),
read-only memory through frame snapshots and a search thread, the blacksmith wishlist over the live save (have/need, a shopping
list, notices), and borderless fullscreen. **Hooks done in PRs #13 to #17** (`hooks.md`): translation-time hooks
on item gains and uses, pouch and box moves, money, crafting, quests and menu opens (all confirmed in game, the last in run 42)
feed the overlay's events. Still to do: more overlay features from the companion (drops, hunt plans), and native in-game menus.

A layer for mods that read the game without changing it:

- **Read-only guest memory access**: a safe view of the game's memory for mods, with no writes (the same rule as the companion's live mode).
- **An overlay**: `egui` drawn on top of the game in the `wgpu` backend, with its own input focus (a key to show and hide it).
- **Hooks patched in at translation time**: the translator (`mh3u-tools translate`) emits a call into the runtime at chosen guest
  addresses (function entries, for example when an item moves or a menu opens), so that mods are told about events instead of
  polling memory each frame. **(done)**

**First target: a blacksmith wishlist.** Pick armour pieces or weapons, track the materials they need against the item box (and
pouch), and show a notification when everything for a piece is ready.

**Later, possibly:** native in-game menus (research in `native-menu.md`, with Ghidra as in `ghidra.md`; first step, opt-in game-drawn text, in PR #20; a "Mods" row in the start menu opening a mod list is planned there, starting with a read-only probe, `MH3U_MENU_PROBE`; the opt-in entry, `MH3U_NATIVE_MENU`, takes the Gestures row over while a key is held, PR #21), by hooking the game's own menu functions so that mod entries appear in them instead of in
the overlay.

### Reuse the companion's reverse engineering

[glinesbdev/mh3u-companion](https://github.com/glinesbdev/mh3u-companion), a terminal app that reads the save and, live, Cemu's
memory, already has most of the data this needs. Reuse it (ideally its `mh3u-core` crate as a dependency) rather than redoing it.
From a light skim of its README and `docs/` (`formats.md`, `live.md`):

- **The save layout** (`user1`) and how to find the live copy in memory (the game holds three copies of the save data while a hunter is
  loaded; the live one has self-pointers): item box, pouch, equipment box, worn gear, guild card, monsters hunted.
- **Data from the executable and archives**: recipes (create and upgrade), armour and weapon stats, monster drops, skills and their
  tiers, carry limits, prices, gathering spots, quests, and the `.gmd` text.
- **The blacksmith**: when pieces unlock and the order of the menu.
- **A wishlist** with have/need counts and a shopping list, which is the overlay's first feature already worked out as a TUI.

In the port the data is read from guest memory directly (no `/proc`), so the companion's live-mode lookup maps onto the read-only view.
