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
     weapon that is upgraded rather than forged, what it is upgraded from.
  3. The shopping list adds up everything on the list against the item box and pouch together. Two pieces that share a material can
     each be ready on their own and still be short together.
  4. `✖` takes a piece off the list.
- **The list is kept in `<build dir>/mods/wishlist.json`**: next to the translated C (`$Out` in `win-run.ps1`, `--out` in `run.sh`).
- **When a wished piece becomes craftable**, a notice shows at the top right for six seconds, whether the overlay is open or not, and
  `[overlay] craftable now: …` is logged.

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
  2. **Check:** the candidates are copied at a frame and chosen: a self-pointing header first, else a block with a hunter's name
     and a header that is not blank.
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

## Not verified yet

- **Finding the live save in the port.** Run 25 (PR #11's first test) never found it, with only MEM2 searched, for the header alone,
  and the search on the game thread made the game stutter and the sound drop. Since the fix it is tested on fake memory with the
  three copies, not on the game. Watch for `[overlay] the live save is at 0x… (why): <hunter>`, or the one `no live save: …` line
  saying what was seen.
- **The game data**, read from a real dump. The test that does it runs only with `MH3U_GAME_DIR` set, and there is no dump where
  this was written. Watch for `[overlay] game data: N items, N recipes, N upgrades` at start.
- **How it looks and feels**, and fullscreen on a real monitor: the tests run egui and the draw path on a no-op GPU, and the window
  logic only as functions.
