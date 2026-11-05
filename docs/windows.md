# Building and running on Windows

The port builds and runs on Windows with the same tools as on Linux: Rust for the tools and the runtime, and **clang** for the C that
the translator writes from your dump. The main path, and the one this page describes, is a MinGW-style clang (target
`x86_64-w64-windows-gnu`, as [llvm-mingw](https://github.com/mstorsjo/llvm-mingw) or MSYS2's CLANG64 environment give you) with Rust's
matching `x86_64-pc-windows-gnullvm` toolchain, so nothing from Visual Studio is needed. A clang for the MSVC target (the official LLVM
installer's, with the Visual Studio build tools) works too; see the end. Microsoft's `cl` and `clang-cl` do not: the generated C uses
clang's extensions, and `build-game` says so if they are all it finds.

What has been checked, and how: the Windows builds of every crate are compiled and linted from Linux (`cargo clippy` for
`x86_64-pc-windows-gnullvm`, `-gnu` and `-msvc`), the test suites of the `-gnullvm` build (linked by llvm-mingw's msvcrt and its UCRT
build) and of the `-gnu` build pass under Wine, the C tests (`c_diff.rs`, the dispatch test) pass as Windows programs built by
llvm-mingw's clang 23 (both builds) and run under Wine, and `build-game` makes a `game.dll` with that clang, with and without `--native`
and `--lto`. On a real Windows machine (llvm-mingw 20261006, msvcrt) the first run found the interpreter's fused multiply-adds wrong:
see below.

All commands are for **Windows PowerShell** (or PowerShell 7). Paths with spaces or square brackets need single quotes: in PowerShell
`[` and `]` are wildcards for commands such as `Test-Path` and `Get-ChildItem`, so use `-LiteralPath` with those. The port's own tools
take paths as they are, brackets and all.

## What you need

- **clang for `x86_64-w64-windows-gnu`** on `PATH`: llvm-mingw (unpack a `llvm-mingw-*-ucrt-x86_64.zip` or
  `llvm-mingw-*-msvcrt-x86_64.zip` release and add its `bin` folder to `PATH`; either works, UCRT is the newer C runtime and the one
  to pick when starting fresh) or MSYS2 (`pacman -S mingw-w64-clang-x86_64-clang` in MSYS2, then add `C:\msys64\clang64\bin` to `PATH`). Check:

  ```powershell
  clang --version        # "Target: x86_64-w64-windows-gnu"
  ```

- **Rust**, with the `x86_64-pc-windows-gnullvm` host (it links with your clang, against your clang's C runtime). From a fresh
  machine:

  ```powershell
  # rustup's installer (the same file for every host; the host is chosen below)
  Invoke-WebRequest -Uri 'https://win.rustup.rs/x86_64' -OutFile "$env:TEMP\rustup-init.exe"
  & "$env:TEMP\rustup-init.exe" -y --default-host x86_64-pc-windows-gnullvm --default-toolchain stable --profile default
  # open a new PowerShell window so that %USERPROFILE%\.cargo\bin is on PATH, then:
  rustup default stable-x86_64-pc-windows-gnullvm
  rustc -vV              # "host: x86_64-pc-windows-gnullvm"
  ```

  (`winget install Rustlang.Rustup` installs rustup too; it sets the MSVC host up by default, so run
  `rustup toolchain install stable-x86_64-pc-windows-gnullvm` and the `rustup default` line above after it.)

  Rust's linker for that target is `x86_64-w64-mingw32-clang`, which llvm-mingw has. With MSYS2's clang, or to be sure, tell cargo to
  link with `clang`, once for your user:

  ```powershell
  [Environment]::SetEnvironmentVariable('CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER', 'clang', 'User')
  $env:CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER = 'clang'    # this window too
  ```

- **git** (`winget install Git.Git`) and the GitHub CLI if you clone with it.
- **A GPU driver with Vulkan or Direct3D 12** (any current NVIDIA, AMD or Intel driver). `wgpu` picks the backend.
- **A processor with FMA** (every x86-64 processor since 2013): the Windows build uses the FMA instruction for the PowerPC's fused
  multiply-adds, because some MinGW C libraries' `fma` is not exact, and `run` stops with a message on a processor without it.
  (mingw-w64's own `fma`, which the msvcrt builds link because msvcrt.dll has none, is off by a bit in some cases and gives NaN for a
  finite product plus an infinity. Nothing of the port calls it: the C uses the instruction and the Rust interpreter uses the `libm`
  crate's exact `fma`, `mh3u_ppc::fused`.)
- **A sound device** (any; WASAPI's default output is used). Nothing to install for it. `$env:MH3U_AUDIO = 'off'` runs without
  sound, and `$env:MH3U_AUDIO_FX = 'off'` without the reverbs and the low-pass filter (see `audio.md`).
- **Keyboard and mouse** (see `input.md`): **off by default since PR #34** (they were on before); `win-run.ps1 -KeyboardControls`
  (`$env:MH3U_KEYBOARD_CONTROLS = '1'`) turns them on. Then WASD moves, the mouse turns the camera (the cursor is grabbed), and the
  left and right buttons are X and A; the bindings are in `$Out\input\bindings.json`. Without them the cursor is never grabbed and
  keys don't play, but F1, the overlay (mouse and typing), F11 and Alt+Enter work.
- **The overlay and fullscreen** (see `overlay.md`): F1 or Back + RB opens the blacksmith wishlist (`$env:MH3U_OVERLAY = 'off'` turns it
  off); `win-run.ps1 -Fullscreen` starts in borderless fullscreen, and F11 or Alt+Enter switches while playing.
- **Your dump** of the game, base game and update (see the first section of `pc-port.md`).

## Build, translate, run

```powershell
git clone https://github.com/glinesbdev/mh3u-port.git
Set-Location mh3u-port
powershell -ExecutionPolicy Bypass -File scripts\check.ps1     # optional: format, lints, tests (as scripts/check.sh)
cargo build --release -p mh3u-tools
$tools = (Resolve-Path '.\target\release\mh3u-tools.exe').Path
```

The release profile in `Cargo.toml` is fat LTO with one codegen unit: the build is slower (2 min 17 s for this command on a
Ryzen 5 9600X) and `mh3u-tools.exe` is smaller and faster. It does not set `target-cpu=native`, so the executable runs on any x86-64
CPU (the game library's `--native` is a separate build, made on the machine that runs it).

The tools take the folder that holds your dumps (or the base game's own folder); they find the base game (`... [Game]
[0005000010118300]`) and the update beside it (`... [Update] [0005000e10118300]`). The executable comes from the update and the content
from the update first, then the base game, so both should be there. The translated C is made from your game and must go to a folder
outside any git working tree, which `translate` checks:

```powershell
$games = 'D:\path\to\your\games'           # the folder with the [Game] and [Update] folders
$out   = "$env:LOCALAPPDATA\mh3u\c"       # anywhere outside the repository
& $tools dump $games                       # says which folders it found
& $tools translate $games $out
& $tools build-game $out                   # writes $out\game.dll
& $tools run $games $out --window --max-imports 1000000000000
```

`build-game` prints the compiler and the target it found (`for a Windows DLL (MinGW runtime)`). `MH3U_CC` picks another compiler
(`$env:MH3U_CC = 'C:\llvm-mingw\bin\clang.exe'`) and `MH3U_CFLAGS` adds flags to every compile and the link. Saves go to `$out\save`
unless `--save-dir` says otherwise. `run` stops after `--max-imports` system calls (100,000 by default, for exploring), hence the big
number to play. The variables of `translator.md` work the same in PowerShell: `$env:MH3U_GFX_LOG = '1'`, and
`Remove-Item Env:MH3U_GFX_LOG` to turn one off again. An Xbox pad's A, B, X and Y are the game's A, B, X and Y; if they come out
swapped, `$env:MH3U_PAD_LAYOUT = 'nintendo'` swaps A with B and X with Y (the log's `[gamepad] … is the GamePad's …` lines say what each
face button was taken as). The pad keeps playing while the window is in the background (read with XInput then, so Xbox-style pads
only); `$env:MH3U_BACKGROUND_GAMEPAD = '0'` (`win-run.ps1 -NoBackgroundGamepad`) turns that off, and
`$env:MH3U_BACKGROUND_KEYBOARD = '1'` (`-BackgroundKeyboard`, with `-KeyboardControls`) lets the keyboard play in the background too
(`input.md`).

The window and the taskbar show the game's icon from your dump (`meta/iconTex.tga`, read at start and kept only in memory).
`[icon] the game's icon from …` in the log says it was found. Without it, Windows shows the default icon.

### The same with the scripts

`scripts\win-build.ps1` and `scripts\win-run.ps1` do the steps above, for Windows PowerShell 5.1 or PowerShell 7. They take the
games from `-Games`, or from `MH3U_GAMES` when it is not given (one of them is required; `win-build.ps1` needs it only to translate), and put the C, `game.dll` and its caches in `-Out`
(`$env:LOCALAPPDATA\mh3u\c` by default). Paths with square brackets, such as the dumps' own folders, are fine. Run them from the
repository or from anywhere:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\win-build.ps1            # cargo build --release -p mh3u-tools, then build-game --native
powershell -ExecutionPolicy Bypass -File scripts\win-build.ps1 -Lto       # build-game --native --lto
powershell -ExecutionPolicy Bypass -File scripts\win-build.ps1 -Profile   # with PDBs, for Visual Studio's profiler
powershell -ExecutionPolicy Bypass -File scripts\win-run.ps1              # play; what it prints also goes to .\mh3u_latest.log
powershell -ExecutionPolicy Bypass -File scripts\win-run.ps1 -Fps         # MH3U_FPS and MH3U_GFX_TIMES
powershell -ExecutionPolicy Bypass -File scripts\win-run.ps1 -Dump frames -Frames 2400,3000 -Report
powershell -ExecutionPolicy Bypass -File scripts\win-run.ps1 -DebugPs tex -PadLayout nintendo -Log tex.log
```

**`win-build.ps1`:**
- It translates first when `-Out` has no `game.h`. `-Translate` makes it translate again, which is needed after the translator
  changes.
- `-Profile` sets `RUSTFLAGS='-C debuginfo=1 -C link-arg=-Wl,--pdb='` for cargo and `MH3U_CFLAGS='-g -gcodeview -Wl,--pdb='` for
  `build-game`, then puts both back. Changing `RUSTFLAGS` rebuilds every crate once, both when turning it on and when turning it
  off.

`win-build.ps1` and `check.ps1` pass cargo's and the compiler's stderr through as plain lines, so their output can be redirected
or piped (`*>&1 | Tee-Object build.log`) on PowerShell 5 and 7 without the first `Compiling` line stopping the script; a step
fails on its exit code only.

**`win-run.ps1`:**
- It first clears the debugging variables an earlier run may have left in the session: `MH3U_FPS`, `MH3U_GFX_TIMES`,
  `MH3U_GFX_REPORT`, `MH3U_GFX_LOG`, `MH3U_GFX_DUMP_*`, `MH3U_GFX_DEBUG_PS*`, `MH3U_GFX_SOLID`, `MH3U_GFX_DEPTH_BIAS`,
  `MH3U_GFX_TEXDUMP`, `MH3U_GFX_SAVE*`, `MH3U_PAD_LAYOUT`, `MH3U_GFX_RESOLUTION_SCALE`, `MH3U_NATIVE_TEXT`, `MH3U_MENU_PROBE`, `MH3U_NATIVE_MENU` and `MH3U_NATIVE_MENU_KEY`.
- Then it sets the ones its switches ask for:
  - `-Fps`: `MH3U_FPS` and `MH3U_GFX_TIMES`;
  - `-Report`: `MH3U_GFX_REPORT`;
  - `-Dump <dir>`: `MH3U_GFX_DUMP_DIR`, with `-Frames` (2400 by default) as `MH3U_GFX_DUMP_FRAME`;
  - `-DebugPs <mode>`: `MH3U_GFX_DEBUG_PS`;
  - `-PadLayout xbox|nintendo`: `MH3U_PAD_LAYOUT`;
  - `-Scale 1|2|3|4`: `MH3U_GFX_RESOLUTION_SCALE` (native size if omitted);
  - `-NativeText`: `MH3U_NATIVE_TEXT=1` (the wishlist in the item menu, `native-menu.md`);
  - `-MenuProbe`: `MH3U_MENU_PROBE=1` (logs the start menu's state, read-only, `native-menu.md`);
  - `-NativeMenu`: `MH3U_NATIVE_MENU=1` (hold Right Shift or ZL in the start menu: Gestures reads "Mods" and opens the Mods page, `native-menu.md`), and
    `-NativeMenuKey K` (`MH3U_NATIVE_MENU_KEY`) for another key.

  It removes them again when the game ends.
- It lists any other `MH3U_` variable still set (the cache and budget switches) as `also set:`.
- It writes the output to `.\mh3u_latest.log` as UTF-8, after renaming an older `mh3u_latest.log` to
  `mh3u_<time it was written>.log`. `-Log <file>` writes somewhere else instead.
- Lines the game writes to stderr come out as plain text, without Windows PowerShell's `NativeCommandError`/`RemoteException`
  wrapping.
- It returns the game's exit code.

On Linux, `scripts/run.sh` does the same as `win-run.ps1` (`--fps`, `--report`, `--dump DIR --frames N`, `--debug-ps MODE`,
`--pad-layout L`, `--scale N`, `--native-text`, `--menu-probe`, `--native-menu`, `--native-menu-key K`, `--log FILE`; `--games` and `--out`, or `MH3U_GAMES` and `MH3U_OUT`).

## Comparing builds (frame rate)

`MH3U_FPS=1` prints the frame rate (swaps per second) every five seconds, as `[fps] 16.3`. Build each variant into its own folder and
run the same scene with each:

```powershell
$env:MH3U_FPS = '1'
& $tools build-game $out                    # -O2, for any x86-64 processor
& $tools run $games $out --window --max-imports 1000000000000
& $tools build-game $out --native           # also -march=native: only for this processor
& $tools build-game $out --native --lto     # also -flto=thin: optimised across files at the link, which takes longer
```

`MH3U_GFX_RESOLUTION_SCALE` renders GX2 color and depth targets at a multiple of their game-requested width and height. For example,
`scripts\win-run.ps1 -Scale 2` (or `$env:MH3U_GFX_RESOLUTION_SCALE = '2'` before `mh3u-tools run`) renders at twice the dimensions
(four times the pixels). Integer values from 1 to 4 are accepted; the adapter's texture-size limit may cap the scale for large
targets. This is supersampling, not texture replacement, and increases GPU memory use and rendering cost. Leave it unset (or set
it to `1`) for the original resolution. The run scripts do not turn it on by themselves.

Where the time of a frame goes: `MH3U_GFX_TIMES` prints, every `MH3U_TIMES_WINDOW` seconds (10 by default), the graphics calls and
the parts of a draw that took the most, as totals and per frame, and the cache hit and miss counts (`translator.md`, "What a draw
costs on the CPU"). Stand in the same place for a whole window and compare the per-frame columns:

```powershell
$env:MH3U_GFX_TIMES = '1'
$env:MH3U_TIMES_WINDOW = '10'
& $tools run $games $out --window --max-imports 1000000000000
```

The same report has `[gfx frames]` (the slowest frame of the window), `[gfx caches]` (each cache's size: steady while standing
still, or something grows), `[gfx pipelines]`, `[gfx invalidates]`, and an `[gfx] area change` line when entering an area, with how
the 180 frames after it went (`translator.md`). Pipelines are made on worker threads and kept in `$out\cache\pipelines`, so a second
visit to an area, or a second run, does not hitch; `$env:MH3U_GFX_ASYNC_PIPELINES = 'off'` makes each draw wait for its pipeline
instead (the old behaviour, for comparing), `$env:MH3U_GFX_PIPELINE_CACHE = 'off'` leaves the disk out, and deleting
`$out\cache` is always safe. Translated shaders are kept in `$out\cache\translations` (`[gfx] shader cache …: N translations kept`
at the start). `$env:MH3U_GFX_SHADER_CACHE = 'off'` leaves them out, and `$env:MH3U_GFX_SHADER_BUDGET_MS` sets how long translating
may take in one frame (5 ms by default, `0` for no limit). The `[gfx] area change` lines show where an area entry's time went:
translating, making shader modules, decoding textures, and the game. `[gfx slow frame]` lines break down any frame of 25 ms or more
(`$env:MH3U_GFX_SLOW_MS` changes that) by its longest steps. `$env:MH3U_GFX_TIMES = 'calls'` times only the calls and frames,
which adds the least to what is measured. `[draw times]` also has `render pass end`, `arena uploads`, `command encoding (finish)` and `submit` (wgpu's own work, which used to be the untimed part of
`GX2DirectCallDisplayList`). `[gfx caches]` shows `vertex streams N (X MB)`, which should stay roughly flat while standing still.
First-seen textures, vertex streams and geometry pipelines past `$env:MH3U_GFX_DECODE_BUDGET_MS` a frame (2 ms by default; big
textures always) are decoded on worker threads. Their draws wait a frame or two ("… decoded in a later frame" in the skipped
draws), so a new area's objects can pop in slightly late. `$env:MH3U_GFX_ASYNC_DECODE = 'off'` decodes them with the draw waiting,
as before, for comparing. `[draw counts]` shows `textures decoded on workers`, `vertex streams decoded on workers`, `geometry
pipelines made on workers` and `texture decodes dropped as stale`.
Leave `MH3U_GFX_DUMP_FRAME` unset when timing: writing the frame takes about 0.6 s and shows as the window's slowest frame.

**Sampling profilers and the Rust side.** A `-gnullvm` build's PDB (`-C link-arg=-Wl,--pdb=`) holds only the public symbols
(about 12 thousand): the Rust code's own functions are local symbols, which lld does not put in it, so Visual Studio's profiler
gives each sample to the public symbol before it in memory, whatever function it really is in (`--icf=none` does not change that).
In such a build of `perf/draw-overhead`, `Gx2::texture_view` was followed by `cached_streams`, `decode_texture` and
`uniform_buffer` (its 14.5 KB were the four of them), `Gx2::new` by all of the graphics dispatch (`run`, `exec`, `draw`,
`geometry_pass`, `bind_group`, the arenas, `flush_draws`: 57 KB), and `__rust_no_alloc_shim_is_unstable_v2` (a one-byte static of
the allocator) by drop glue, two closures of `draw_call`, and `copy_surface`. The local symbols are in the executable's own symbol
table, so a sample's address can be named with `llvm-nm -n --demangle mh3u-tools.exe` (addresses in the PDB are
`0001:offset`, from `.text` at 0x140001000). A profiler that reads DWARF from a Windows executable would name them directly in a build
with `-C debuginfo=line-tables-only`. The `MH3U_GFX_TIMES` timers are the numbers to go by.

`--lto` needs clang with lld, which llvm-mingw and MSYS2's CLANG64 both are. To compare with the code before a change of the
translator, translate with each version of the tools into its own folder (the C differs, not only the flags).

## With the MSVC-target clang instead

The official LLVM installer (`winget install LLVM.LLVM`) gives a clang whose target is `x86_64-pc-windows-msvc`; it needs the Visual
Studio build tools (C++ workload) for the C runtime and Windows SDK, and goes with Rust's default `x86_64-pc-windows-msvc` toolchain.
`build-game` links with `lld-link` (`-fuse-ld=lld`) there and says `for a Windows DLL (MSVC runtime)`. This path is checked less than
the MinGW one (lints of the Rust only).

## How the Windows build differs

- The library is `game.dll`; nothing is compiled with `-fPIC` and nothing links `-lm` (`mh3u_recomp::toolchain` chooses the flags from
  `clang -dumpmachine`).
- The guest's 4 GiB is reserved with `VirtualAlloc` and committed 64 KiB at a time on first touch (an exception handler), so that, as
  on Linux, untouched memory costs nothing and reads as zeros (`crates/mh3u-rt/src/memory.rs`).
- The tests' C programs put their memory at a fixed address with `VirtualAlloc` instead of `mmap` and switch stdin and stdout to binary.
