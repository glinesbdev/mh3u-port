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

## QEMU as the oracle (`crates/mh3u-ppc/tests/`)

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
instruction word of the same random cases, compiles them all into one program with the system C compiler (clang, cc or gcc), runs the
program on the same states and compares with the interpreter bit for bit: 1,320 words, every branch form that stays inside a
function, and the reservation pair. Together with the first part this is a chain: translated C = interpreter = QEMU.

Where the C is deliberately not the interpreter: a `bl` or `bctrl` is a *call* (a C call, or a hook for an address the translator does
not know) and `blr` is a *return*, where the interpreter changes the program counter; code that uses them as jumps into the middle
of something else is the translator's job to find (see "What is not translated yet").

## What is not translated yet

- The table for indirect calls (30,605 `rt_call(c, t)` sites) and for the 4,312 indirect tail jumps that are not switches: both go through
  the runtime, which must map an address to a translated function.
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
(declarations), `imports.h` (the names of the system functions), and `dispatch.c`: the table that finds a function by address (for the
30,605 indirect calls and the indirect tail jumps), the table of import trampolines (an import called through a pointer reaches the
host as an import), and the hooks that route `rt_*` to the host. Every file passes `clang -fsyntax-only`.

`build-game` compiles the files on every core into a shared library (`libgame.so` / `game.dll`): 41 s at `-O0` on 24 cores.

## Running it (`mh3u-rt`, `mh3u-tools run`)

`mh3u-rt` is the only crate with `unsafe`. It reserves the guest's 4 GiB address space, puts the executable's sections in it, loads the
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
with their text and the cloth background, upright, in the right places (a few of the hunter's textures are still wrong: white
speckles on the clothing). The pieces:

- **State**: every `GX2Set*` call is recorded into the state of the current context (`GX2SetContextState`/`GX2SetupContextStateEx`
  switch between them); display lists (`GX2BeginDisplayListEx`...) are recorded and replayed with their arguments.
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

- **Textures** (`texture.rs`): a `GX2Texture` is read from the guest, untiled texel by texel (`tiling::address`) and uploaded in the
  format the GPU takes (the block-compressed ones as they are stored: the blocks are not byte-swapped; the packed ones are converted).
  The layout is `tiling::surface_info`, the same calculation the game's size call used. The tiling code is a restatement of the part of
  AMD's R6xx/R7xx address library (MIT, Copyright 2014 Advanced Micro Devices, Inc.) that maps coordinates to addresses and sizes
  surfaces; the order of steps of the surface size call and the rule for resolving the default tile mode are facts read from the
  decaf-emu sources, not copied. The memory configuration (2 pipes, 4 banks, 256-byte interleave, 2 KiB rows) is assumed from the
  result: the text and the cloth only come out right with it. Only level 0 is read so far.

**Not done yet:** mipmaps, cube maps and 3D textures, the rest of the sampler state, alpha
test and stencil, depth bias, other geometry inputs (lines, triangles, instances), `CUBE` in shaders, loops and relative
addressing in vertex shaders, rects and other primitive types, stream out, the second render target (the GamePad's picture is
drawn but not shown), and speed: every draw makes its own buffers and render pass.

Looking at what it draws: `MH3U_GFX_SAVE=<folder>` writes the TV picture as a PPM every `MH3U_GFX_SAVE_EVERY` swaps (120 by default),
`MH3U_GFX_SAVE_IMAGE=<hex image address>` saves a given colour buffer instead; `MH3U_GFX_SOLID=1` makes every pixel red (to see
where the geometry is); `MH3U_GFX_DUMP=<folder>` writes shader programs and their tables as the game gave them; `MH3U_GFX_LOG=1`
prints draw counts, the targets drawn into, and why draws were skipped.

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
derivative_uniformity)`). **572 of the 579 different pixel programs become WGSL that naga validates** (`mh3u-tools latte-wgsl`); the
other 7 use `CUBE` (not written: its output convention has to be worked out with a cube-map shader to look at). Whether a translated
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
- **Input**: the game waits at its first screen for a button. `VPADRead` gives one sample with the buttons held as
  `MH3U_PRESS=<mask hex>,<period ms>` says (0x8000 is A); real input comes later.

Debugging aids (all in `mh3u-tools run`): `MH3U_TRACE=1` (every system call with its result, and file opens), `MH3U_DUMP_AFTER=<s>`
(what every thread is doing and which system function it called last), `MH3U_GFX_LOG=1` (the first draws' state, and draw counts), and,
for a library built with `build-game --watch`: `MH3U_WATCH=<addr>,<len>` (every store, also by the runtime, to that range, with the
return address), `MH3U_ENTER=<function>` / `MH3U_ENTER_LR=<return address>` (report entries of a function, or of whatever is called
from a given call site), `MH3U_PROFILE=<s>` (a sampling profile of the translated code). `mh3u-tools disasm` prints readable
PowerPC (`mh3u_ppc::format`).
