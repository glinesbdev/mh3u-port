# mh3u-port

A native PC port of Monster Hunter 3 Ultimate (Wii U) for Linux and Windows, in progress. **Private for now.** The game's PowerPC code
is translated ahead of time into C, built into a library on the owner's machine, and run against a runtime in Rust that stands in for
the console's system libraries, with the graphics on `wgpu`. It starts, reaches its character screen and the village, draws them with
translated shaders, and reads keyboard, mouse and gamepads, and plays the game's sound voices through `cpal` (first steps: no effects, see
[`docs/audio.md`](docs/audio.md)); much of the graphics is incomplete. Tested on
Linux; the Windows build is checked from Linux (see [`docs/windows.md`](docs/windows.md) for building and running it there). Read [`docs/pc-port.md`](docs/pc-port.md) for what the project is meant to be, [`docs/spikes.md`](docs/spikes.md)
for what was measured before it was built, [`docs/performance.md`](docs/performance.md) for measured costs and open performance concerns, and [`docs/translator.md`](docs/translator.md) for how the translator, the runtime and the
graphics work and what is not done, and [`docs/audio.md`](docs/audio.md) for the sound. On Linux, building needs the ALSA development files
(`alsa-lib` on Arch, `libasound2-dev` on Debian and Ubuntu).

**You must own the game.** This repository contains no game code and no game data, and the port will not run without your own dump of
your own copy of the Wii U game (base game `0005000010118300` and its update `0005000e10118300`). See the first section of
`docs/pc-port.md`.

## What is here

| Crate | What it does |
|---|---|
| `mh3u-formats` | the game's executable (`.rpx`: sections, imports), archives (`.arc`) and text (`.gmd`), with bounded decompression and tests on damaged files |
| `mh3u-dump` | finds the owner's dump and checks that it is this game |
| `mh3u-ppc` | decodes the Wii U's PowerPC instructions, and a reference interpreter checked against QEMU |
| `mh3u-recomp` | the translator: each function of the game's code as C (`ppc.h` gives each instruction its meaning), function discovery, switch tables |
| `mh3u-rt` | the runtime: guest memory, threads, the system libraries' functions, input; loads the translated library and runs the game (the one crate that may use `unsafe`) |
| `mh3u-gfx` | the graphics: GX2 calls to `wgpu` (state, draws, textures, geometry shaders as compute shaders) |
| `mh3u-latte` | reads the Wii U GPU's shader programs and translates them to WGSL |
| `mh3u-testkit` | the tests of the interpreter against QEMU and of the translated C against the interpreter |
| `mh3u-cemu` | reads the memory of a running Cemu (Linux), to compare the port with the emulator |
| `mh3u-tools` | the program and the explorers: `translate`, `build-game`, `run`, `disasm`, `functions`, `dump`, `sections`, `imports`, `coverage`, `shaders`, `latte-*`, `scan-shaders` (needs Cemu), `program-census`, `section`, `codegen-bench` (spike 4), `arcls`, `arcx`, `gmd`, `arcsearch` |

```
cargo run -p mh3u-tools -- dump "/path/to/your/games folder"
cargo run -p mh3u-tools -- sections "/path/to/your/games folder"
cargo run -p mh3u-tools -- imports "/path/to/your/games folder"
```

The first four crates started as copies of the readers in the [companion app](https://github.com/glinesbdev/mh3u-companion) (public
domain, same author); from here on they change on their own. Run `scripts/check.sh` (on Windows, `scripts\check.ps1`) before every
commit; there is no CI.

To build and play on Windows, `scripts\win-build.ps1` builds the tools and `game.dll`, and `scripts\win-run.ps1` runs the game in a
window, also writing what it prints to `mh3u_latest.log`. Their switches include `-Profile`, `-Lto`, `-Fps`, `-Report`, `-Dump`
and `-PadLayout` (see [`docs/windows.md`](docs/windows.md)). On Linux, `scripts/run.sh` runs the game the same way.

## Licence

Public domain (`LICENSE`), for the code here. It says nothing about the game, which is Capcom's. The repository is private; decide
before it goes public whether this is still the licence.
