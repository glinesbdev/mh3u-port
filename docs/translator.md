# The translator: how it is tested

The translator turns the game's PowerPC code into C (see `pc-port.md`). The risk is not in writing it but in **getting a thousand
small meanings exactly right**: what `srawi` does to the carry bit, what `divw` gives for a zero, what `fmadds` does with a NaN. A mistake
there makes a game that runs and is subtly wrong. So the meaning of each instruction is checked against something that is not this
project's understanding of it.

## The reference interpreter (`mh3u-ppc`, `interp.rs`)

One core, no interrupts, no address translation: the *meaning* of each instruction in plain Rust, small enough to read against the
architecture book. It is what everything else is compared with. Not modelled: the floating point status register's exception bits (the
game's code never touches the register: spike 2), the hardware's estimate tables for `fres` and `frsqrte` (the exact reciprocal is used,
within the hardware's stated error), and the privileged and 64-bit instructions (the game uses none).

## QEMU as the oracle (`crates/mh3u-testkit/tests/`)

`qemu-ppc-static` (QEMU's user-mode PowerPC emulator, `-cpu 750`) is an independent implementation of the instruction set. The tests
build a small static 32-bit big-endian ELF for each case, with no linker: a stub loads a whole register state (32 GPRs, 32 FPRs, CR, XER,
LR, CTR and 256 bytes of scratch memory) from an array, runs the instruction under test, saves the state, and loops over the cases; at the
end it writes the arrays to standard output. The same states are run on the interpreter and the results compared bit for bit.

- `qemu_sanity.rs`: the harness itself (a known program, the state round trip, the loader).
- `qemu_diff.rs`: **every instruction** (the 1,250 or so words are random operands for each of about 150 instructions, 24 random states each
  with values chosen from the edge cases: 0, 1, -1, 0x7fffffff, 0x80000000, shift counts of 31, 32, 33, 63, 64; for floating point
  zeros, infinities, denormals, NaNs), every branch form with every kind of `BO`, the reservation pair, and the estimate instructions.
  The tests skip when QEMU is not installed.
- What it found in the interpreter: `lswi`/`lswx` cleared a register it should not have touched, and `lfs` made a signalling NaN quiet where the
  hardware only moves the fields. Both are fixed; the rest agreed from the start.

What QEMU does not check: the **paired singles** (QEMU has none), and the instructions it treats as illegal on the 750 (`fsqrt`, `fsqrts`:
the game does not use them). Their meaning has to come from the architecture book, and a second check (from the game itself: Cemu's
memory, see `docs/spikes.md`) is planned.

## The C against the interpreter (`crates/mh3u-testkit`, `crates/mh3u-recomp`)

`mh3u-recomp` writes C for each function: every instruction is a few statements using the helpers of `ppc.h` (the registers in a `Ctx`,
loads and stores through a base pointer, the carry and overflow rules, the condition register). `tests/c_diff.rs` translates every
instruction word of the same random cases, compiles them all into one program with the C compiler `build-game` would use, runs the
program on the same states and compares with the interpreter bit for bit: 1,320 words, every branch form that stays inside a
function, and the reservation pair. Together with the first part this is a chain: translated C = interpreter = QEMU. Two more tests
there check what the interpreter cannot: that the registers reach the runtime's hooks (`rt_call`, `rt_jump`, `rt_syscall`, whose test
versions read and change them) and come back from them, and that calls and tail calls between translated functions carry them.

**Registers in locals.** Inside a translated function the general-purpose and floating point registers (both halves of a paired
single) that it names are C locals (`R_3`, `F_1`, `P_1`), not fields of the `Ctx`: `FILL()` loads all of them on entry, `SPILL()`
writes back the ones the function ever changes before anything outside it can look (a call, a tail call, a jump through a register,
a return, `rt_import`, `rt_syscall`, `rt_trap`, `rt_unsupported`), and `FILL()` loads them again after a call or hook returns (it may
have changed any of them, or let another guest thread run). The two macros are defined before each function and undefined after it.
The condition, link, count and XER registers, the GQRs and the reservation stay in the `Ctx`. The translator writes every instruction
against `c->r[N]` and the like as before and then renames the ones with a constant `N` (`localize` in `mh3u-recomp`); to make every
index constant, `lmw`, `stmw`, `lswi` and `stswi` are unrolled and the paired-single halves are named directly. `lswx` and `stswx` (the
count is in XER, so the registers are picked at run time) still work on the `Ctx` and spill and fill around themselves. The function's
`Ctx *` is `restrict`: a `Ctx` lives in the host's memory, never in the guest's, so a guest store through `c->mem` cannot change it,
and the compiler need not reload the condition register and the rest after every store. Without this every register was a memory
access that every guest store forced to be redone. In a `--watch` build the store watch reports `r1` from the `Ctx`, which can be
behind the function's local.

A tail call to a translated function is `TAIL(f)`: `__attribute__((musttail)) return f(c)` where the compiler has it (clang, and
GCC from version 15), so a chain of tail calls is a chain of jumps and does not grow the host's stack at any optimisation level; with
an older compiler it is a call and a return.

Where the C is deliberately not the interpreter: a `bl` or `bctrl` is a *call* (a C call, or a hook for an address the translator does
not know) and `blr` is a *return*, where the interpreter changes the program counter; code that uses them as jumps into the middle
of something else is the translator's job to find (see "What is not translated yet").

## What is not translated yet

- Indirect calls (30,605 `rt_call(c, t)` sites) and the 4,312 indirect tail jumps that are not switches are not resolved ahead of
  time: each goes through `game_call` in `dispatch.c`, which looks the address up in a hash table (see "The whole executable").
- The imports: a call to one of the 568 import trampolines (placed after the end of `.text`, from 0x31963d8) is `rt_import(c, number)`,
  and `imports.h` names each number ("library:function"). Which import a trampoline stands for comes from the relocations of `.text`
  (7,231 name an import; each trampoline serves one import, checked); the runtime implements `rt_import`. Imports reached through a
  pointer instead of a call are not known yet.
- Not found: functions only reached by a tail jump, and the 6 words in the code that are not instructions (data between functions).

## Switch tables (`switch_tables` in `mh3u-recomp`)

The game's compiler makes a `switch` as: compare the index (`cmplwi`), branch to the default when above, shift the index by two, add it
to an address built by `addis`/`addi` that is the address *right after the `bctr`*, `mtctr`, `bctr`; after the `bctr` comes a table of
`b` instructions. 510 of the 4,822 `bctr` are of this kind (503 with a table length equal to the `cmplwi` bound; the translator takes
the smaller of the bound and the run of branches). The C is a `switch` on the index with a `goto` to each branch's label, and it is
checked against the interpreter in `c_diff.rs`. The other 4,312 `bctr` are tail calls through a function pointer or a vtable (a
`lwz`/`mtctr` chain), not tables.

## The whole executable (`mh3u-tools translate`, `build-game`, `run`)

The 82,209 functions become 412 C files of 200 functions (247 MB, written in 2 seconds, outside any git tree) with `game.h`
(declarations), `imports.h` (the names of the system functions), and `dispatch.c`: the table that finds a function or an import
trampoline by its address (for the 30,605 indirect calls and the indirect tail jumps; an import called through a pointer reaches the
host as an import), and the hooks that route `rt_*` to the host. Every file passes `clang -fsyntax-only`.

The lookup, `game_call`, is a hash table made by the translator (`dispatch_c` in `mh3u-tools/src/translate.rs`), not a search: open
addressing with linear probing, the start slot from the address times a constant, and at least twice as many slots as addresses. For
the game's 82,209 functions and 568 trampolines that is 2^18 slots of 8 bytes, 2 MiB of read-only data, at about 1.2 probes per
lookup on average. A slot holds the address and either the function's place in the `functions` array or the import's number (as
`-1 - n`); a function wins over a trampoline at the same address. Before this, every indirect call was a binary search over all the
functions and then over the trampolines. A test (`game_call_finds_every_function_and_import_and_nothing_else`) builds the table for
made-up addresses with the system C compiler and checks every hit and some misses.

`build-game` compiles the files on every core into a shared library (`libgame.so` / `game.dll`): 41 s at `-O0` on 24 cores (measured
before the registers moved into locals). The flags come from `mh3u_recomp::CFlags`, which the tests in `c_diff.rs` compile with too:
`-O2` by default (`--opt N` changes it), `-fno-strict-aliasing`, and `-ffp-contract=off` (so that the compiler never fuses a multiply
and an add that the PowerPC rounds twice, which it would do on a machine with FMA). Two choices are opt-in: `--native` adds
`-march=native` (faster on this machine; the library may not run on another), and `--lto` adds `-flto=thin` to the compiles and the
link (clang and `ld.lld` are needed; the link does the code generation, so it takes longer). `MH3U_CC` picks the compiler (clang,
then `cc`, then `gcc` otherwise; clang, then `gcc` on Windows, where `cl` and `clang-cl` are refused because the C uses clang's
extensions) and `MH3U_CFLAGS` adds flags to every compile and the link. `mh3u_recomp::toolchain` asks the compiler for its target
(`-dumpmachine`, so a `--target=` in `MH3U_CFLAGS` cross-compiles) and chooses what the target needs: `-fPIC -fvisibility=hidden` and
`-lm` for a shared object, nothing of those for `game.dll` (which exports through `GAME_API`) and lld for clang's link. On Windows
`ppc.h` does the fused multiply-adds with the FMA instruction, because some MinGW C libraries' `fma` is off by a bit
(mingw-w64's own, in its msvcrt builds); `run` checks for FMA there. The interpreter does them with the `libm` crate's `fma` and
`fmaf` (`mh3u_ppc::fused`), never `mul_add`, which calls the C library's (`clippy.toml` forbids it). `MH3U_TEST_CFLAGS` adds flags to the `c_diff.rs` tests and the dispatch test, to check a
build such as `-march=native` against the interpreter, and `MH3U_TEST_RUNNER` starts their programs through another (with
`MH3U_TEST_CFLAGS=--target=x86_64-w64-windows-gnu` and `MH3U_TEST_RUNNER=wine` they check the Windows build of the C from Linux).
Building on Windows: `docs/windows.md`.

## Running it (`mh3u-rt`, `mh3u-tools run`)

`mh3u-rt` is the only crate with `unsafe`. It reserves the guest's 4 GiB address space (`memory.rs`: one anonymous mapping on Linux;
on Windows a `VirtualAlloc` reservation whose pages an exception handler commits 64 KiB at a time on first touch), puts the executable's sections in it, loads the
library, and runs the entry point; every system function the game imports is a handler written in Rust (`cafe.rs` for memory, heaps
and the C library, `sync.rs` for threads and what they wait on) or, when none is written yet, a stub that returns 0 and shows as
`(stub)` in the report. `MH3U_TRACE=1` prints every system call with its result.

- **Threads** are host threads but only one runs guest code at a time: it holds a baton and passes it on when a system function has to
  wait (locked mutex, unset event, sleep), yields, or ends. A waiting function is run again when its thread gets the baton back.
  This makes runs repeatable; the cost is that a thread that waits by spinning on memory, never calling the system, would hold the
  baton forever, and that all guest threads share one core of the host for now.
- **Data imports** (variables the loader fills in, such as the default-heap function pointers) get an address the runtime treats as the
  function's entry, so a call through them reaches the same handler.
- **Relocations of the code**: the file holds stale halves in the `lis`/`addi`/`lwz` that build the address of an import; the
  translator patches them from the relocations (types 4 and 6) before translating.
- **Guest memory**: MEM2 (the application's, 0x10800000, 1.14 GB; the game asks for all of it, less a margin it computes from
  `OSGetMemBound` and `OSGetForegroundBucketFreeArea`), MEM1 (0xf4000000, 32 MB) and the foreground bucket (0xe0000000, 48 MB); the
  runtime's own structures are at 0x0e000000.

- **Files**: `FS*` and `SAVE*` (all synchronous: the game imports no async variant) read `/vol/content` from the update's `content` folder
  first, then the base game's, matching names without regard to case; saves go to a folder of the owner's choosing (`--save-dir`).
- **Graphics**: the surface sizes (`GX2CalcSurfaceSizeAndAlignment`: tile mode, pitch, size, alignment, swizzle and mip offsets) follow
  the console's address library rules, ported in `mh3u-formats/src/tiling.rs`. A function the game looks up by name at run time (`OSDynLoad_FindExport`) is
  found, and calling it does nothing and returns 0 (the error-display module is the only one so far).
- **Switch tables**: the table's address is found by following the constants (`lis`, `addi`, `addic`, `ori`) in the 16 instructions
  before the `mtctr`; 1,102 tables in the game (the first rule, `addis`/`addi` into the register of the `mtctr`, found only 510).

Result so far: the game starts, sets up its memory and threads, reads its archives from the owner's files, runs its render loop
(`GX2DrawEx`, texture and sampler binds, copies to the scan buffer: hundreds of frames) and stops only at the limit on system calls
that `run` sets. Nothing is drawn on a screen yet: every `GX2` call is a stub that does nothing. The to-do list is the `(stub)`
lines of the report, most-called first.

## Graphics (`mh3u-gfx`)

**API: `wgpu`**, which runs on Vulkan on Linux and Windows (Direct3D 12 and Metal are the other backends it can use). It was chosen over
raw Vulkan because the whole crate stays safe Rust (the workspace allows `unsafe` only in `mh3u-rt`), one code path serves both target
systems, and it is fast enough for a console from 2012. The test machine's GPU is an AMD Radeon RX 7800 XT on the Mesa Vulkan driver
(`mh3u-gfx/examples/probe.rs` opens a window and prints which adapter it got). If the game turns out to need what `wgpu` does not have
(geometry shaders, stream output), the answer is `wgpu-hal` or a Vulkan binding for that part only; the game imports
`GX2SetGeometryShader`, but how often it is called is not measured yet.

**Shape.** The window belongs to the thread that started the program; the game runs on another and draws from there: the backend
(`Gx2`, implementing `mh3u_rt::Gx2Backend`) is called by the runtime for every `gx2` import it has code for, on whichever guest thread
made the call, so it reads the guest's memory at the moment of the call and needs no copying or locking. A call that blocks (waiting
for the next frame) blocks there, with the other guest threads waiting.

**What it draws now**: the game reaches its character screen and the backend draws it, textured: the hunter model, the UI panels
with their text and the cloth background, upright, in the right places (the hunter's clothing still shows white speckles: not understood yet). The pieces:

- **State**: every `GX2Set*` call is recorded into the state of the current context (`GX2SetContextState`/`GX2SetupContextStateEx`
  switch between them); display lists (`GX2BeginDisplayListEx`...) are recorded and replayed with their arguments. A recorded call is
  a fixed-size record (which call, as a number, and the argument registers and stack words it reads), a list is a shared slice played
  back without copying, and a `GX2SetPixelTexture`/`GX2SetPixelSampler` that sets a unit to what the list already set it to since its
  last change of context is left out (the size `GX2EndDisplayList` returns still counts it). Graphics imports are given a number
  when the game is loaded (`Gx2Backend::resolve`) and dispatched by it.
- **A draw** (`draw.rs`): the vertex and pixel shaders are read from the guest, translated (cached by address and a hash of the
  program), the vertex attributes are decoded on the CPU into one stream of `vec4` per fetch-shader element (the fetch shader is not
  run: `GX2InitFetchShaderEx` keeps the attribute descriptions), the uniform blocks are copied with their words swapped, a pipeline is
  made from the state (blend, depth, cull, formats) and cached, and the draw is issued with generated indices (fans, quads and strips
  that `wgpu` lacks are expanded). A vertex shader's outputs reach the pixel shader through the semantic numbers in the shaders'
  headers (a pixel input takes the vertex output with the same semantic). A pass that samples the target it draws into gets a copy.
- **Geometry pipelines** (`geometry.rs`): nearly every draw of the 2D interface and the effects goes through the same geometry
  shader (it turns a point with a size and a rectangle into a quad). The hardware's vertex shader (writing a ring of 12 `vec4`
  per vertex) and geometry shader (reading it, writing a ring of 11 `vec4` per emitted vertex, and `EMIT_VERTEX`) are translated into
  compute shaders; the geometry shader also writes the triangles its strips make; the copy shader (a vertex shader that reads the
  second ring) draws them with the pixel shader. Only point inputs are done.
- **The DMA engine** (`DMAECopyMem`) is how the game fills the GPU's constant buffers: without it every uniform block was zero.
- `GX2CopySurface` copies between render targets (same size and format only); `GX2ClearColor`, `GX2ClearDepthStencilEx`.

- **Speed** (the village scene went from 3.7 to about 13 frames a second): the draws of a frame are recorded into one command buffer and
  one render pass while they go to the same targets (`flush_draws` hands them to the GPU at the next copy, clear or swap); the small
  buffers of a draw (uniform blocks, vertex streams, indices, the geometry rings) are cut from big arena buffers that are reused after
  each hand-over, instead of being made one by one; the geometry pipelines' compute work goes into a second command buffer that runs
  first; only the vertices a draw uses are decoded (its indices are made relative to the lowest); and the decoded vertex streams are
  kept on the GPU, found again by a hash of the vertex bytes (so a mesh the game draws every frame is decoded once). A strip's restart
  index (`GX2SetPrimitiveRestartIndex`, 0xffff here) becomes the API's restart value: before that, strips drew a vertex 65535 away
  and decoded all 64 thousand vertices before it for every such draw. `MH3U_GFX_TIMES=1` prints where the time goes (every ten seconds:
  per graphics call, and per part of a draw).
- **What a draw costs on the CPU** (about 50 microseconds by `MH3U_GFX_TIMES` before these changes, ~125 draws a frame in the village): nothing per draw is found by a
  formatted string or a hash of the whole program any more.
  - *Shaders*: a translation is found by a front cache (`shader_cache.rs`) keyed by what is cheap to read: the shader's address, its
    program's address and size, the fetch shader's version (a new shared slice per `GX2InitFetchShaderEx`), half z, and for pixel
    shaders a hash of the input words and the texture kinds. The program bytes still decide: each entry keeps their hash, the first
    draw of every frame that uses it hashes them again, and `GX2Invalidate` with the shader (8) or CPU (0x40) bit drops the entries
    whose programs it overlaps. The assumption: a program rewritten in place without `GX2Invalidate` may be drawn as it was for the
    rest of that frame (the console's GPU reads programs through its own cache too). Geometry pipelines are found the same way (their
    key also has half z and the ring sizes, which the translation uses).
  - *Uploads*: what a batch's draws read (uniform blocks, indices, geometry parameters) is written into a copy of the arena in memory
    and uploaded with one write per arena when the batch is flushed; a uniform block whose contents were already uploaded in the batch
    reuses those bytes (compared, not only hashed). The vertex streams' start table lives with the cached streams, and the geometry
    draws use the stream cache too.
  - *Bind groups*: cached by layout and the resources bound (`wgpu` handles compare by identity), pruned of what a second of frames did
    not use; pipelines keep their bind group layouts; texture, target and depth views are made once with their textures; samplers are
    keyed by the three `GX2Sampler` words, and the default and compare samplers are made once. A bind group with a per-draw snapshot
    of the target is not kept.
  - *System calls*: the report's counters are arrays indexed by import number, `MH3U_TRACE_FROM` is read once, and the timing report
    allocates nothing per call.

  The reports: `MH3U_GFX_TIMES=1` gives each line as a total, a count, and both per frame (`[gfx times]` per graphics call, `[draw
  times]` per part of a draw, where "geometry pass" counts only the draws with a geometry shader), and `[draw counts]`: hits, checks
  and misses of the shader caches, bind groups, texture views and uniform uploads, and the display list calls replayed and left out.
  `MH3U_TIMES_WINDOW=<seconds>` sets how long each report covers (10 by default); the per-frame numbers are the ones to compare
  between builds and scenes.
- **Textures** (`texture.rs`): a `GX2Texture` is read from the guest, untiled texel by texel (`tiling::address`) and uploaded in the
  format the GPU takes (the block-compressed ones as they are stored: the blocks are not byte-swapped; the packed ones are converted).
  The layout is `tiling::surface_info`, the same calculation the game's size call used. The tiling code is a restatement of the part of
  AMD's R6xx/R7xx address library (MIT, Copyright 2014 Advanced Micro Devices, Inc.) that maps coordinates to addresses and sizes
  surfaces; the order of steps of the surface size call and the rule for resolving the default tile mode are facts read from the
  decaf-emu sources, not copied. The memory configuration (2 pipes, 4 banks, 256-byte interleave, 2 KiB rows) is assumed from the
  result: the text and the cloth only come out right with it. Every mip level is read (level 1 at the mip pointer, level n at the pointer plus
  the n-th offset of the header) and the sampler's filters and level-of-detail range are used. A cube map is a texture of six layers, read as
  an array: the `CUBE` instruction (which turns a direction into a face number and the face's own coordinates) is done in the shader, so the
  lookup uses the face and coordinates as the hardware does.

- **Input** (`mh3u-rt/src/input.rs`): `VPADRead` returns what the player presses: the keyboard and mouse (window events) and gamepads
  (`gilrs`, on a thread of their own), combined in one shared state. Keys are named by position: WASD is the left stick, the arrow keys the
  right stick, Enter A, Backspace B, X, C the X and Y buttons, I/J/K/L the D-pad, Q/E L and R, 1/3 ZL and ZR, P plus, O minus, Left
  and Right Shift the stick clicks. A pushed stick also
  sets the stick's direction buttons (the menus read those). Gamepad face buttons go by label (the Xbox A is the GamePad's A); `MH3U_PAD_LAYOUT=position` puts them where the GamePad's are. The gamepad thread takes the pending events without waiting and sleeps 4 ms when there are none (it used to wait in
  gilrs's blocking call, and a Windows profile found it taking a whole core); `MH3U_GAMEPADS=0` turns gamepads off entirely (no thread,
  no backend), for comparing. The mouse is the touch panel (the picture is the TV's, so it only matters once
  the GamePad's screen is shown). `VPADGetTPCalibratedPoint` scales the raw touch values (0 to 4095) to 1280 x 720 (a guess). The Wii
  Remote and Pro Controller functions (`KPAD*`, `WPAD*`) are not answered: no controller of those kinds exists for the game.
  `MH3U_PRESS` still works and is added to the real input.

**Not done yet:** 3D textures, the component selectors of a texture (`compMap`), the rest of the sampler state (borders, the mip bias), alpha
test and stencil, depth bias, other geometry inputs (lines, triangles, instances), loops and relative
addressing in vertex shaders, rects and other primitive types, stream out, and the second render target (the GamePad's picture is
drawn but not shown). (Speed is in "Speed" above: draws are batched and their buffers come from arenas.)

Looking at what it draws: `MH3U_GFX_SAVE=<folder>` writes the TV picture as a PPM every `MH3U_GFX_SAVE_EVERY` swaps (120 by default),
`MH3U_GFX_SAVE_IMAGE=<hex image address>` saves a given colour buffer instead; `MH3U_GFX_SOLID=1` makes every pixel red (to see
where the geometry is); `MH3U_GFX_DUMP=<folder>` writes shader programs and their tables as the game gave them; `MH3U_GFX_LOG=1`
prints draw counts, the targets drawn into, and why draws were skipped. `MH3U_FPS=1` prints the frame rate (swaps per second) every
five seconds, for comparing builds.

## The shaders (`mh3u-latte`: `decode.rs`, `ops.rs`, `translate.rs`)

`decode` reads a program (control flow, ALU groups, fetches) with the R7xx bit layouts and the R6xx instruction numbers; `ops.rs` is the
table of numbers, names and slots (made from the open drivers' list for this family, which documents them; the bit layouts were read
from the same drivers' headers and checked against the game's 967 programs: `mh3u-tools latte-check` decodes all of them and counts
exactly what the first census did). Findings that differ from the textbook: the ALU word's two-operand instruction number is in bits 7
to 17 with the output modifier in bits 5 and 6 (R7xx), and an ALU clause has no end-of-program bit (bit 21 is part of its count).
The game's pixel shaders use 33 two-operand and 5 three-operand instructions, kcache banks 1 to 5, no relative addressing, no
predication per instruction, and uniform blocks indexed at run time through `VFETCH` on resource 128 + the block number.

`translate_pixel` turns a decoded program into WGSL: registers are `vec4<f32>` holding raw bits, each group reads the registers as they
were before it and writes together afterwards, `pv`/`ps` are the previous group's results, the exec-mask stack is a stack of booleans
(a lane that is not active writes nothing), and texture fetches run for every lane so derivatives stay defined (`diagnostic(off,
derivative_uniformity)`). **All 579 different pixel programs become WGSL that naga validates** (`mh3u-tools latte-wgsl`); `CUBE` follows the usual
OpenGL face convention (its exact output is not checked against a cube-map picture yet). Whether a translated
shader computes what the hardware does is not checked by anything but looking at the picture: the semantics are from the documents,
so an operation the translator does not know is an error, never a guess.
Vertex shaders are translated with the fetch shader replaced by decoded attribute streams (vertex pulling); loops and relative
addressing in them are not done. The geometry pipeline's three programs are translated by `translate_es`, `translate_gs` and
`translate_vertex` with a ring configuration (the hardware's ring writes are dwords at an absolute offset; the item sizes come from
the vertex shader's `ringItemsize` and the geometry shader's `sq_gs_vert_itemsize`).

## Getting the game to its first scene (what the runtime had to learn)

Each of these was found by looking at what the game was waiting for, with the debugging aids below:

- **`ProcUIProcessMessages`** delivers the foreground-acquire callback on its first call (`Step::Calls` runs guest callbacks with the
  kernel unlocked); **audio**: `AXRegisterFrameCallback` starts a service thread that runs the callbacks every 3 ms, and voices exist;
  **IPC buffer pools** (the keyboard library's start-up failed without them); **accounts and network** answer "one user, no network".
- **`GX2TempGetGPUVersion`** must return 2: the game's GFD header check compares it with the shader files' GPU version, and with a
  stub returning 0 every shader was rejected, silently, so every draw had no shader.
- **Input**: the game waits at its first screen for a button; see the input notes under "What it draws now". A press is reported once
  per frame (`VPADRead` is called twice a frame, and only the first call may see the edge).
- **Audio voices** keep what the game tells `AX` (offsets, loop, pitch, state) and a running voice moves on with the time at its pitch's
  rate: the game polls a voice's position to know when to refill its stream or that a sound ended. Nothing is mixed or played yet.
- **Alarms** (`OSSetAlarm`...) run their handlers on a thread of their own when they are due.
- **Software keyboard** (`swkbd`, found at run time with `OSDynLoad_FindExport`): its functions still return 0 (`Create` returns 1).
  The character screen's name field shows a notice and then does not move on, with input reaching the game correctly (checked with
  `MH3U_TRACE_PAD`); the cause is not found. `Swkbd*Appear*` is never called, so it is not waiting for the keyboard.

Debugging aids (all in `mh3u-tools run`): `MH3U_TRACE=1` (every system call with its result, and file opens), `MH3U_DUMP_AFTER=<s>`
(what every thread is doing and which system function it called last), `MH3U_GFX_LOG=1` (the first draws' state, and draw counts), and,
for a library built with `build-game --watch`: `MH3U_WATCH=<addr>,<len>` (every store, also by the runtime, to that range, with the
return address), `MH3U_ENTER=<function>` / `MH3U_ENTER_LR=<return address>` (report entries of a function, or of whatever is called
from a given call site), `MH3U_PROFILE=<s>` (a sampling profile of the translated code). `mh3u-tools disasm` prints readable
PowerPC (`mh3u_ppc::format`). More: `MH3U_TRACE_FROM=<s>` (trace only after that long), `MH3U_TRACE_DYNAMIC=1` (calls of functions found
at run time, with arguments and return address), `MH3U_TRACE_PAD=1` (what `VPADRead` returns), `MH3U_TRACE_AX=1` (voice positions),
`MH3U_PRESS_DUTY=<percent>` (how long of its period `MH3U_PRESS` holds), `MH3U_PRESS_SWEEP=<s>` and `MH3U_TOUCH_SWEEP=<s>` (press every
button, or touch a grid of points, in turn from then on), `MH3U_PAD_LAYOUT=position` (gamepad face buttons by position instead of label).
