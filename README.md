# mh3u-port

A native PC port of **Monster Hunter 3 Ultimate** (Wii U) for **Windows and Linux**, made by *static recompilation*. The game's
PowerPC code is translated ahead of time into C and compiled on your machine. It runs against a runtime written in Rust that
stands in for the Wii U's system libraries. Graphics go through `wgpu` (Vulkan or Direct3D 12) and sound through `cpal`. No
emulator is involved at run time.

> **You must own a legal copy of the game and dump it yourself. No game files are provided.** This repository contains no game
> code, data or assets. The port will not start without your own dump, and it does not explain how to get the game any other way.

> **No online functionality.** Nintendo's Wii U servers for this game are closed. Online play, Miiverse and every other
> network feature are unsupported, and nothing in the port connects to the network.

## Status

Listed on [recomp.fyi](https://recomp.fyi/) (the listing comes from [`.recomp.json`](.recomp.json); edit that file to change it).
[![Listed on PlayPorted](https://playported.com/badge.svg)](https://playported.com/ports/919-monster-hunter-3-ultimate/)


Work in progress, and playable in parts:

- **Runs:** it boots and reaches the title screen, character creation (including naming a hunter) and the village. It runs quests,
  with the game's sound (voices, streams, reverb and filtering).
- **Graphics:** shaders are translated from the Wii U GPU's programs to WGSL. Much is drawn correctly, but some effects are
  incomplete. You can set the internal resolution (1x to 4x) and play in borderless fullscreen.
- **Input:** an Xbox-style gamepad or keyboard and mouse (WASD, mouse camera). Bindings are in a file you can edit.
- **Mods (optional):**
  - An overlay (F1) with a per-hunter blacksmith wishlist: materials needed against your item box and pouch, a shopping list and
    a notice when a piece can be made.
  - Opt-in native in-game menus.
- **Not yet:** a few graphics glitches (such as shadow streaking on the player outside the village), the hash check of the dump, and CI. See [`docs/roadmap.md`](docs/roadmap.md) for what is next.

## Supported game version

Only this release is supported:

| | Title ID | Version |
|---|---|---|
| Monster Hunter 3 Ultimate, **USA** | `0005000010118300` | base game |
| Its update | `0005000e10118300` | **v32** (the latest) |

Both are needed. The executable is taken from the update, and data from the update first, then the base game. Every address in
the code and docs is an address in the USA v32 executable. The EUR (`0005000010117200`) and JPN (*MH3G HD*) releases are
different builds and **do not work**.

## What you need

- **Your own dump** of the base game and update above, decrypted, in the usual folder layout (`code/`, `content/`, `meta/`).
  - The folders are found by their title IDs, for example `Monster Hunter 3 Ultimate [Game] [0005000010118300]` and
    `Monster Hunter 3 Ultimate [Update] [0005000e10118300]`.
  - Put both in one folder ("your games folder").
  - Dump them from your own console and disc or eShop purchase with a homebrew dumping tool.
- **A GPU and driver** with Vulkan or Direct3D 12, and an x86-64 CPU with FMA (any from 2013 on).
- **Disk space** for the translated C and its build, kept **outside** this repository. `translate` refuses a folder
  inside a git working tree.

## Setup on Windows

Full details and troubleshooting: [`docs/windows.md`](docs/windows.md).

1. Install the tools:
   - **Rust**, with the `x86_64-pc-windows-gnullvm` toolchain.
   - **LLVM-MinGW** clang: [llvm-mingw](https://github.com/mstorsjo/llvm-mingw) on your `PATH`.
   - **Git.**

   ```powershell
   winget install Rustlang.Rustup Git.Git
   rustup toolchain install stable-x86_64-pc-windows-gnullvm
   rustup default stable-x86_64-pc-windows-gnullvm
   [Environment]::SetEnvironmentVariable('CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER', 'clang', 'User')
   ```
2. Clone and build. This translates your game once, which takes a while, then compiles it to `game.dll`:

   ```powershell
   git clone https://github.com/glinesbdev/mh3u-port.git
   Set-Location mh3u-port
   powershell -ExecutionPolicy Bypass -File scripts\win-build.ps1 -Games 'D:\path\to\your\games'
   ```

   The output goes to `%LOCALAPPDATA%\mh3u\c` (`-Out` changes it).
3. Play. What the game prints also goes to `mh3u_latest.log`:

   ```powershell
   powershell -ExecutionPolicy Bypass -File scripts\win-run.ps1 -Games 'C:\path\to\your\games'
   ```

   Useful switches: `-Fullscreen`, `-Scale 2`, `-Fps`, `-PadLayout nintendo`, `-NativeMenu`, `-NativeText`,
   `-KeyboardControls`, `-NoBackgroundGamepad`, `-BackgroundKeyboard`.
   Instead of `-Games` each time, you can set the folder once: `$env:MH3U_GAMES = 'D:\path\to\your\games'` (or
   `[Environment]::SetEnvironmentVariable('MH3U_GAMES', '...', 'User')`). The scripts stop with a message when neither is set.
4. After pulling changes to the translator or the hooks, rebuild with `win-build.ps1 -Translate`.

## Setup on Linux (Arch)

1. Install the tools. For Vulkan, add your GPU's driver as well: `vulkan-radeon`, `vulkan-intel` or `nvidia-utils`.

   ```sh
   sudo pacman -S --needed rustup clang lld git alsa-lib vulkan-icd-loader
   rustup default stable
   ```

   On Debian or Ubuntu the sound library is `libasound2-dev`.
2. Clone, build the tools, translate your game and compile it:

   ```sh
   git clone https://github.com/glinesbdev/mh3u-port.git && cd mh3u-port
   cargo build --release -p mh3u-tools
   games=/path/to/your/games          # the folder with the [Game] and [Update] folders
   out="$HOME/.local/share/mh3u/c"    # outside the repository
   target/release/mh3u-tools dump "$games"          # checks it finds the right game
   target/release/mh3u-tools translate "$games" "$out"
   target/release/mh3u-tools build-game "$out" --native   # writes $out/libgame.so
   ```
3. Play. What the game prints also goes to `mh3u_latest.log`:

   ```sh
   scripts/run.sh --games "$games" --out "$out"     # or: export MH3U_GAMES="$games" MH3U_OUT="$out"
   ```

   Useful options: `--fullscreen`, `--scale 2`, `--fps`, `--pad-layout nintendo`, `--native-menu`, `--native-text`
   (`scripts/run.sh --help` lists them all).

## Controls

- **Gamepad:** an Xbox-style pad's A, B, X and Y are the Wii U GamePad's A, B, X and Y. If they come out swapped, use
  `MH3U_PAD_LAYOUT=nintendo` (`-PadLayout nintendo` or `--pad-layout nintendo`).
- **Keyboard and mouse:** off by default (since PR #34; they were on before). Turn them on with `-KeyboardControls`
  (`--keyboard-controls`, `MH3U_KEYBOARD_CONTROLS=1`). Without them the cursor is never grabbed and keys don't move the hunter, but
  F1, the overlay (mouse and typing), F11 and Alt+Enter still work.

  | Action | Key |
  |---|---|
  | Move | W A S D (Left Ctrl to walk) |
  | Camera | mouse |
  | X / A | left / right mouse button |
  | B / Y | Space / E |
  | L / R | Q / Left Shift |
  | ZL / ZR | F / R |
  | + / − | Escape / Tab |
  | D-pad | arrow keys |

  Edit `<out>/input/bindings.json` to change the bindings ([`docs/input.md`](docs/input.md)).
- **Window:**
  - F11 or Alt+Enter: fullscreen.
  - F1, or Back + RB on a gamepad: the overlay.
- **Naming a hunter:** when the game asks for text, a keyboard window opens in the overlay. Type the name and press Enter. You can
  also set `MH3U_SWKBD_TEXT=<name>` to skip the window.

## Mods and options

| Setting | What it does |
|---|---|
| F1 overlay (on by default; `MH3U_OVERLAY=off`) | The blacksmith wishlist, one per hunter. It shows only once a hunter is loaded. See [`docs/overlay.md`](docs/overlay.md). |
| `MH3U_NATIVE_TEXT=1` (`-NativeText` / `--native-text`) | The wishlist's status in the game's own item menu. |
| `MH3U_NATIVE_MENU=1` (`-NativeMenu` / `--native-menu`) | Hold Right Shift or ZL in the start menu and Gestures becomes **Mods**, a native page listing the wishlist ([`docs/native-menu.md`](docs/native-menu.md)). |
| `MH3U_BACKGROUND_GAMEPAD=0` (`-NoBackgroundGamepad` / `--no-background-gamepad`) | Gamepads play only while the window has the focus. By default they keep playing in the background ([`docs/input.md`](docs/input.md)). |
| `MH3U_KEYBOARD_CONTROLS=1` (`-KeyboardControls` / `--keyboard-controls`) | Keyboard and mouse game controls: WASD, the mouse as the camera, the cursor grabbed. **Off by default since PR #34** (they were on before). F1 and the overlay work without it. |
| `MH3U_BACKGROUND_KEYBOARD=1` (`-BackgroundKeyboard` / `--background-keyboard`) | With keyboard controls on, the keyboard also plays while the window is in the background (opt-in; never the mouse). |
| `MH3U_GFX_RESOLUTION_SCALE=1..4` (`-Scale` / `--scale`) | The internal resolution. |
| `MH3U_AUDIO=off`, `MH3U_AUDIO_FX=off` | No sound, or no reverb and filter. |

**Window icon:** the window, and on Windows the taskbar, use the game's own icon, read from your dump's `meta/iconTex.tga`
(the base game's, else the update's). It is only read, never copied, and no artwork is in this repository. If the file is
missing or can't be read, the default icon stays and the log says why in one `[icon]` line.

The mods only read the game's memory. The opt-in native menus change on-screen text and menu state, and never your save or
pouch. Saves go to `<out>/save`.

## Repository layout

| Crate | What it does |
|---|---|
| `mh3u-formats` | the game's executable (`.rpx`), archives (`.arc`) and text (`.gmd`) |
| `mh3u-dump` | finds your dump and checks that it is this game |
| `mh3u-ppc` | decodes the Wii U's PowerPC instructions, with a reference interpreter checked against QEMU |
| `mh3u-recomp` | the translator: each function of the game as C |
| `mh3u-rt` | the runtime: guest memory, threads, system libraries, input, hooks |
| `mh3u-gfx` | the graphics: GX2 to `wgpu`, and the overlay |
| `mh3u-latte` | translates the GPU's shader programs to WGSL |
| `mh3u-audio` | the sound: the game's voices mixed and played with `cpal` |
| `mh3u-mods` | the game's data tables, the live save and the wishlist |
| `mh3u-testkit` | the tests of the interpreter and the translated C |
| `mh3u-cemu` | reads a running Cemu's memory (Linux), to compare the port with the emulator |
| `mh3u-tools` | the command line: `translate`, `build-game`, `run`, `dump`, `disasm` and the explorers |

Documentation: [`docs/README.md`](docs/README.md). To contribute, see [`CONTRIBUTING.md`](CONTRIBUTING.md). Before every commit, run `scripts/check.sh` (or `scripts\check.ps1`): format,
clippy with warnings as errors, and the tests.

## Legal

- *Monster Hunter 3 Ultimate* is © CAPCOM CO., LTD. *Monster Hunter* and *Capcom* are trademarks of Capcom.
- *Wii U* and *Nintendo* are trademarks of Nintendo.
- This project is not affiliated with, endorsed or sponsored by Capcom or Nintendo.
- It contains none of their code or assets. Everything that makes the game comes from your own dump at build time.
- The translated C that `translate` writes is derived from the game: keep it to yourself and never commit or share it.

## License

The original port code and tools in this repository are licensed under the **GNU Affero General Public License v3.0 only**
(`AGPL-3.0-only`); see [`LICENSE`](LICENSE). The license covers only that original code. It does not cover the game's code or
assets, anything translated or extracted from your dump, or the trademarks of Capcom and Nintendo, which remain their owners'.

## Acknowledgements

- **[Cemu](https://cemu.info/):** the reference used to compare behaviour, frames and memory (`mh3u-cemu`).
- **[decaf-emu](https://github.com/decaf-emu/decaf-emu):** its sources were read for hardware facts (GPU registers, audio). No code
  was taken.
- **AMD's address library** ([decaf-emu/addrlib](https://github.com/decaf-emu/addrlib), MIT): used only to generate the texture
  tiling test vectors.
- **[QEMU](https://www.qemu.org/):** the PowerPC reference that the interpreter is tested against.
- **[Ghidra](https://ghidra-sre.org/)** and Maschell's **[GhidraRPXLoader](https://github.com/Maschell/GhidraRPXLoader):** for
  reading the game's code ([`docs/ghidra.md`](docs/ghidra.md)).
- **[mh3u-companion](https://github.com/glinesbdev/mh3u-companion):** the same author's save and data readers, where the first
  crates started.
- **Libraries:** [wgpu](https://wgpu.rs/), [winit](https://github.com/rust-windowing/winit), [egui](https://github.com/emilk/egui),
  [cpal](https://github.com/RustAudio/cpal), and the other crates in `Cargo.lock`.
