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
  The baton goes to the next ready thread in thread order (a sleeping thread stays ready and looks at the clock when the baton
  reaches it). Each guest thread has its own condition variable, so a handoff wakes only the host thread that receives the baton;
  a thread that has ended is never woken again (its host thread ends if its entry function returned, and parks for good if it
  called `OSExitThread` from inside translated code). The baton used to be one condition variable woken for all on every pass, with
  ended threads waiting on it forever: every pass woke every one of them, so the cost grew with each thread the game had finished
  (a Windows profile had 64% of the time in the ended threads' waits; the kernel test `baton_goes_round_the_live_threads_in_order`
  measures it: 50 ended and 8 live threads went from about 150 us per pass to 5 us). With `MH3U_GFX_TIMES=1` a `[baton]` line
  every report window gives the passes and idle sleeps per frame and how many guest threads have ended.
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
  switch between them); display lists (`GX2BeginDisplayListEx`...) are recorded and replayed with their arguments; the calls that only fill in an object in
  memory (`GX2Calc*`, `GX2Init*`, such as `GX2InitFetchShaderEx` and `GX2InitSampler*`) are never recorded but run at once, as on the
  console (their inputs may be gone when the list is played). A recorded call is
  a fixed-size record (which call, as a number, and the argument registers and stack words it reads), a list is a shared slice played
  back without copying, and a `GX2SetPixelTexture`/`GX2SetPixelSampler` that sets a unit to what the list already set it to since its
  last change of context is left out (the size `GX2EndDisplayList` returns still counts it). Graphics imports are given a number
  when the game is loaded (`Gx2Backend::resolve`) and dispatched by it.
- **A draw** (`draw.rs`): the vertex and pixel shaders are read from the guest, translated (cached by address and a hash of the
  program), the vertex attributes are decoded on the CPU into one stream of `vec4` per fetch-shader element (the fetch shader is not
  run: `GX2InitFetchShaderEx` keeps the attribute descriptions), the uniform blocks are copied as they are (little-endian words), a pipeline is
  made from the state (blend, depth, cull, formats) and cached, and the draw is issued with generated indices (fans, quads and strips
  that `wgpu` lacks are expanded). A vertex shader's outputs reach the pixel shader through the semantic numbers in the shaders'
  headers (a pixel input takes the vertex output with the same semantic). A pass that samples the target it draws into gets a copy.
  The position is `@invariant`, so a mesh's depth-only and colour draws (same vertex shader, different pixel shaders) give the same
  depth. Colour buffers of the 32-bit float formats (`0x80e`, `0x80d`, `0x823`) are made as `R32Float`, `Rg32Float` and
  `Rgba32Float` and are never blended, as on the hardware; other unlisted formats are still `Rgba8Unorm`.
- **Geometry pipelines** (`geometry.rs`): nearly every draw of the 2D interface and the effects goes through the same geometry
  shader (it turns a point with a size and a rectangle into a quad). The hardware's vertex shader (writing a ring of 12 `vec4`
  per vertex) and geometry shader (reading it, writing a ring of 11 `vec4` per emitted vertex, and `EMIT_VERTEX`) are translated into
  compute shaders; the geometry shader also writes the triangles its strips make; the copy shader (a vertex shader that reads the
  second ring) draws them with the pixel shader. Only point inputs are done.
- **The DMA engine** (`DMAECopyMem`) is how the game fills the GPU's constant buffers: without it every uniform block was zero. Its
  last argument turns the bytes around (0 none, 1 in every 16 bits, 2 in every 32 bits); the game's float blocks must come with 2 (copied
  without the swap they were big-endian in memory, which the GPU could not read), and the GPU reads uniform blocks as little-endian words with no swap of its own (packed colours the game writes as bytes, red first,
  are read with red in the low byte). The first copy of each mode is logged (`[dmae] first copy with endian mode N`). An indexed
  uniform fetch (`VFETCH` on resource 128 + n) honours its own `ENDIAN_SWAP`.
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
    program's address and size, the fetch shader's contents (`GX2InitFetchShaderEx` makes one shared slice per distinct attribute
    description, so its address names the contents: the game makes some fetch shaders again every frame, and with a new slice each
    time every draw with them missed and left an entry behind), half z, and for pixel shaders a hash of the input words and the
    texture kinds. The program bytes still decide: each entry keeps their hash, the first draw of every frame that uses it hashes
    them again, and `GX2Invalidate` with the shader (8) or CPU (0x40) bit drops the entries whose programs it overlaps. The entries
    are indexed by where their programs start, so an invalidate looks only at the programs near the memory it names (the game
    invalidates about a thousand times a frame); entries not drawn with for 600 frames go, and past 16384 the least recently used. The
    assumption: a program rewritten in place without `GX2Invalidate` may be drawn as it was for the rest of that frame (the console's
    GPU reads programs through its own cache too). Geometry pipelines are found the same way (their key also has half z and the ring
    sizes, which the translation uses).
  - *Render pipelines* (`pipelines.rs`): found by the shaders' translations and the state the pipeline holds; a new one is made on a
    worker thread and its draw is not drawn until it is there (usually a frame or two; `MH3U_GFX_ASYNC_PIPELINES=off` makes the draw
    wait instead, as before). Each one made is written down under `<out>/cache/pipelines/<key>/` (its shaders' WGSL in `shaders/`,
    its state as a line of `pipelines.txt`; the key is a hash of the graphics sources (`SHADER_SOURCE`, below) and the adapter and
    driver, written in `about.txt`), and all
    of them are made again on the workers at the next start, found then by a hash of the WGSL whatever addresses the shaders have.
    `MH3U_GFX_PIPELINE_CACHE=off` turns the disk cache off, `MH3U_GFX_CACHE_DIR` puts it elsewhere; deleting the folder is always
    safe. `wgpu::PipelineCache` (Vulkan's own cache of compiled pipelines) is not used: making one is an `unsafe` call, and only
    `mh3u-rt` may have `unsafe` code.
  - *Translated shaders on disk* (`translations.rs`): each vertex and pixel shader translated is written down under
    `<out>/cache/translations/<SHADER_SOURCE>/`. Its WGSL goes in `shaders/<hash of the WGSL>.wgsl`, and a line in
    `translations.txt` has its key and its interface, with a check hash at the end.
    - The key is a hash of the program bytes and everything else the translation depends on: the fetch shader, half z,
      the semantic table and the loop constants for a vertex shader; the inputs, the texture kinds and selectors, the alpha
      test and the loop constants for a pixel shader.
    - `SHADER_SOURCE` is a hash of the sources of `mh3u-latte` and `mh3u-gfx`, made by `build.rs`. The cache stays valid
      across commits that do not touch them, and a build whose translator differs at all never reads it.
    - A shader whose key is there is not translated. A line that does not check out, or whose WGSL is missing or changed,
      is not used.
    - Shader modules are kept by the hash of their WGSL and shared with the pipeline workers. The modules the workers make
      at startup for the pipelines read from disk serve the draws too, so a translation found on disk usually costs no
      module either.
    - `MH3U_GFX_SHADER_CACHE=off` leaves this cache out. It is also left out while shader texts are kept for a frame dump,
      with `MH3U_GFX_SOLID`, and with a pixel shader debug view on.
    - Translating is limited to `MH3U_GFX_SHADER_BUDGET_MS` a frame (5 by default; `0` or `off`: no limit). The first
      translation of a frame always goes ahead. A draw that needs another is not drawn that frame (counted as "shader
      translated in a later frame"), and nothing is kept for it, so the next frame asks again. Geometry pipelines are not on
      disk and not limited by it. With `MH3U_GFX_ASYNC_DECODE` on (the default), a new one is translated and its modules and
      compute pipelines are made on a decode worker (`decode.rs`), from copies of its programs. Its draws are not drawn until it is
      back ("geometry pipeline made in a later frame").
  - *Decoding on workers* (`decode.rs`): first-seen texture and vertex-stream decodes may take `MH3U_GFX_DECODE_BUDGET_MS` a frame
    on the render thread (2 by default). Past that, and for any texture with 256 KB or more of source, they go to the decode
    workers. A worker never reads guest memory: the render thread copies what the decode reads into a `Snapshot` (for a texture,
    `texture::sources`: the image and levels at least as big as the header says and as laid out), and the worker decodes from that.
    A read outside the copy gives zeros and is counted ("texture reads outside what was copied"). The worker makes the GPU texture
    too. Results are taken in at the swap (`collect_decodes`).
    - A draw waiting for a new texture or new streams is not drawn ("texture decoded in a later frame", "vertex streams decoded in
      a later frame"). A changed texture is drawn as it was until its new decode is back.
    - `GX2Invalidate` over a texture being decoded marks the job stale. The result is dropped ("texture decodes dropped as
      stale"), and that texture is decoded on the render thread from then on. A texture that cannot be read is too.
    - A job that panics is counted ("decode jobs that panicked"). A texture or geometry pipeline whose job panics is reported as
      not read or not translated.
    - Streams' keys hash the bytes that are copied, so a worker's streams are always what their key names. Streams from workers
      that age out before they are drawn are counted ("vertex streams from workers never drawn").
    - `MH3U_GFX_ASYNC_DECODE=off` (or `0`) decodes everything on the render thread, as before. The tests do too, unless they
      turn the workers on.
  - *Uploads*: what a batch's draws read (uniform blocks, indices, geometry parameters) is written into a copy of the arena in memory
    and uploaded with one write per arena when the batch is flushed. The vertex streams' start table lives with the cached streams,
    and the geometry draws use the stream cache too.
  - *Uniform blocks*: each upload of the batch keeps the guest bytes it was made from, by (address, vec4 count, bytes read); a draw
    that finds the same bytes there (compared in place: no copy, no hash, no word swapping) reads that upload. Only new contents are
    swapped and written. A bank the shader reads by index (`VFETCH` on resource 128 + n) is declared 4096 vec4 (64 KiB) and must be
    bound whole, with zeros past the block; such blocks go to 64 KiB slots of their own that are kept zero past what was written, so
    only the block's own bytes are copied and uploaded. Before, every use read, swapped, hashed and compared all 64 KiB, and every
    new upload zeroed and uploaded 64 KiB: that was most of the 2.2 ms per frame of "uniforms and groups 0, 1" in the village.
  - *Vertex streams and indices*: the vertex bytes of a draw are hashed where they are, with XXH3, not copied out and hashed a byte
    at a time. A draw's index list (made relative to its lowest vertex, fans and quads made into triangles) is kept by the draw and an
    XXH3 hash of its index bytes, which are also compared: the first time it goes into the arena, the second time it gets a buffer of
    its own that later frames use as it is, so a mesh drawn every frame builds and uploads its list once. Lists not drawn again
    within two frames are forgotten. Decoded vertex streams follow the same rule:
    - A new stream goes into the batch's arena. Its bytes are kept with it, and it is written into the arena again for a later
      batch of the same frame.
    - Drawn with in a later frame, it gets two GPU buffers of its own: the streams, and where each starts.
    - A stream not drawn with in a later frame than the one it was made in (vertex data the game rewrites every frame) goes two
      frames later, never having had buffers. Any stream goes after 120 frames unused. Both are checked every frame.

    wgpu's per-pass and per-submit bookkeeping grows with the most buffers ever alive (run 12: 31,000 streams kept, the replay
    three times slower at the same draws; see `docs/performance.md`, open concern 3). Making and destroying buffers for streams
    used once cost 26 ms frames every 60 frames (run 18, concern 7).
  - *Textures*: a texture already made is found by the five header words of its key in one look-up. `GX2Invalidate` over its memory
    no longer throws it away: it is marked, and at its next use the header words and the bytes of its image and smaller levels are
    hashed and compared with what it was decoded from; it is decoded again only if they differ (a hash pass instead of untiling every
    texel). A marked texture not drawn for a second is forgotten, as an invalidated one was before.
  - *Bind groups*: cached by layout and the resources bound (`wgpu` handles compare by identity). Every frame, the ones a second of
    frames did not use go, so they are destroyed a few at a time, not a second's worth at once. A bind group with vertex streams
    still in the batch's arena is not kept; pipelines keep their bind group layouts; texture, target and depth views are made once with their textures; samplers are
    keyed by the three `GX2Sampler` words, and the default and compare samplers are made once. A bind group with a per-draw snapshot
    of the target is not kept.
  - *System calls*: the report's counters are arrays indexed by import number, `MH3U_TRACE_FROM` is read once, and the timing report
    allocates nothing per call.

  With `MH3U_GFX_TIMES=1` (or `MH3U_FPS=1`), every window also has `[gfx frames]` (the slowest frame and how many took over 33 and
  50 ms), `[gfx caches]` (the size of every cache: one that only grows shows here), `[gfx pipelines]` (made with the draw waiting,
  handed to workers, made by them and their time, draws not drawn while theirs was made, found made) and `[gfx invalidates]` (how many
  `GX2Invalidate`, how many reached the shader caches and how many index entries each looked at, and their time a frame); and a frame
  that makes eight or more new pipelines, shader translations, shaders found on disk or texture decodes prints `[gfx] area change at
  frame N: <ms> (<ms> in graphics calls, <ms> in the game or the swap); <n> new pipelines; shaders: <n> translated (<ms>), <n> found
  on disk, <n> modules made (<ms>) and <n> found made, <n> draws left for a later frame; textures: <n> decoded (<ms>)`. 180 frames
  later it prints how those frames went: the slowest (with its time in graphics calls), the mean, how many took over 50 ms, the
  pipelines, and the same shader and texture counts. `[gfx caches]` also shows the translations on disk and how many were found or
  not.

  **`[gfx slow frame]`.** With `MH3U_GFX_TIMES=1`, a frame of 25 ms or more (`MH3U_GFX_SLOW_MS`) prints `[gfx slow frame] frame N:
  <ms> (<ms> in graphics calls); steps: ...; counts: ...`.
  - Its steps are that frame's twelve longest draw steps and graphics calls, each with its time and count. Steps nest, so they do
    not add up.
  - The non-draw calls (copies, clears, `GX2Invalidate`, ...) are timed one by one for this, with the same two clock readings that
    give the frame's time in graphics calls and the runtime's `[gfx times]`.
  - A frame ends inside the swap, after presenting and after the caches let go of what they no longer use. That much of the swap
    counts in the frame it ends: `gx2:GX2SwapScanBuffers` and `caches aged out (at the swap)`.
  - Its counts are that frame's `[draw counts]`.

  `[draw times]` also has:
  - `stream miss: buffers and upload`: putting a new vertex stream in the arena, or giving one drawn again buffers of its own;
  - `bind group made (in groups)`;
  - `arena uploads`: the batch's arena writes;
  - `command encoding (finish)`: wgpu 30 checks a batch's passes, tracks their resources and makes the Vulkan or D3D12 commands
    only when the command buffer is finished, so this is most of what the draws cost on the CPU outside this code. The batch's
    encoders are finished and submitted on the `mh3u gpu queue` thread (`queue.rs`), so this step and `submit` are one,
    `command encoding and submit (handed over)`; with `MH3U_GFX_THREAD=off` the same step covers the whole work, done on the
    game's thread.

  `[draw counts]` adds `vertex stream bytes made`, `vertex streams given buffers`, `vertex streams aged out`, `bind groups aged
  out`, `arena uploads` and `arena bytes uploaded`. `MH3U_GFX_TIMES=calls` leaves out the draw steps and counts (about 25 clock
  readings a draw). It keeps the calls' and frames' times, for timing with as little added as possible.

  The reports: `MH3U_GFX_TIMES=1` gives each line as a total, a count, and both per frame (`[gfx times]` per graphics call, `[draw
  times]` per part of a draw, where "geometry pass" counts only the draws with a geometry shader), and `[draw counts]`: hits, checks
  and misses of the shader caches, bind groups and uniform uploads, texture decodes (and bytes), textures invalidated and
  invalidations of textures found unchanged, and the display list calls replayed and left out; "uniform bytes uploaded" and
  "uniform blocks read by index" (the 64 KiB banks), "index list hits" and "misses", and "vertex bytes hashed"; and "textures
  decoded on workers", "vertex streams decoded on workers", "geometry pipelines made on workers" and the decode counters above.
  "texture copy and hash (in texture views)", "texture decode (in texture views)" and "texture upload and hash (in texture views)"
  are timed inside "texture views", not added to it. "stream miss: copy for a worker" is the copy of a stream handed to a worker,
  and "decodes from workers taken in (at the swap)" is the swap's share. "index list"
  covers the whole index path (found or made, and uploaded), and "decode streams" only the vertex streams (the indices used to be
  made relative to the lowest vertex inside it).
  `cargo test -p mh3u-gfx --release draw_bench -- --nocapture` times the same parts on wgpu's no-op device with a synthetic frame shaped
  like the village's (190 draws, half of them with a bank read by index), and the playback of a display list of state calls; the
  graphics tests run on that device too (no window, nothing drawn, everything validated).
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
  lookup uses the face and coordinates as the hardware does. A texture is decoded from a copy of its image and levels (`decode` takes
  any `decode::Memory`; `decode_guest` copies and decodes). Each level of 256 KB or more is split by rows between up to 8 threads,
  and the result is the same as on one (a test checks it).

- **Input** (`mh3u-rt/src/input.rs`): `VPADRead` returns what the player presses: the keyboard and mouse (window events) and gamepads
  (`gilrs`, on a thread of their own), combined in one shared state. Keys are named by position: WASD is the left stick, the arrow keys the
  right stick, Enter A, Backspace B, X, C the X and Y buttons, I/J/K/L the D-pad, Q/E L and R, 1/3 ZL and ZR, P plus, O minus, Left
  and Right Shift the stick clicks. A pushed stick also
  sets the stick's direction buttons (the menus read those). Gamepad face buttons: by default the button gilrs names East is the GamePad's A, South B, North X, West Y. With the user's Xbox pad on Windows, this makes the Xbox A the game's A (run 15's log: `[gamepad] East is the GamePad's A`); the earlier mapping (South A, East B, West X, North Y) had the Xbox A acting as the game's B. `MH3U_PAD_LAYOUT=nintendo` brings the earlier mapping back (`position`, the old setting, now means the default). The log says which layout is on (`gamepad buttons: …`), and the first press of each face button what it was taken as (`[gamepad] South is the GamePad's B`). The gamepad thread takes the pending events without waiting and sleeps 4 ms when there are none (it used to wait in
  gilrs's blocking call; a Windows profile seemed to put a whole core there, but the time was the baton's, see Threads above); `MH3U_GAMEPADS=0` turns gamepads off entirely (no thread,
  no backend), for comparing. The mouse is the touch panel (the picture is the TV's, so it only matters once
  the GamePad's screen is shown). `VPADGetTPCalibratedPoint` scales the raw touch values (0 to 4095) to 1280 x 720 (a guess). The Wii
  Remote and Pro Controller functions (`KPAD*`, `WPAD*`) are not answered: no controller of those kinds exists for the game.
  `MH3U_PRESS` still works and is added to the real input.

- **Rendering correctness** (the 3D models and the areas were wrong while the 2D pictures were right; checked offline, not yet against
  the game's picture):
  - *Tiling* now matches AMD's address library for this chip (R7xx: no dual pitch, no doubled macro-tile bytes, thick tiles in the
    displayable order with the slice bits 6 and 7): `mh3u-formats/src/tiling_vectors.txt` holds every 15th of 9,828 cases made by the
    library itself (`tools/addrlib_vectors.cpp`); all 9,828 match (pitch, height, size, base alignment and a hash of every address).
    The 2D-tiled textures with a pitch that is not a power of two, and all thick ones, were laid out wrong before.
  - *Vertex attributes* are read by their type and the GX2 flags: the scaled formats (integer to float) were raw integers or halves,
    signed 10_10_10_2 was read unsigned, and the default swap was applied to every size; now bits are swapped per unit (none for 8-bit
    types, 16-bit units for 16-bit ones, 32-bit for the rest) and sign-extended, a missing component reads 0 and the fourth 1.
  - *Texels* are little-endian (GX2's format table asks for no swap; Cemu reads them that way): the 16-bit packed formats had their
    bytes swapped. Volumes are 3D textures; depth formats include 0x1c.
  - *Shaders*: loops (`LOOP_START`, `_DX10`, `_NO_AL`, `LOOP_END`, `LOOP_BREAK`, `LOOP_CONTINUE`, and the ALU clauses that break,
    continue or end with `ELSE`), the address register (`MOVA`, `MOVA_FLOOR`, `MOVA_INT`) and the loop index for relatively addressed
    registers, uniform blocks and constants; counted loops read the shader's `GX2LoopVar`s; a loop with no count stops after 4,096
    trips. The one vertex shader the village could not translate (`0x169ac000`: `LOOP_START_DX10`, `ALU_BREAK`, `LOOP_END`) is in this
    set. A program that indexes its registers has them as an array.
  - *State*: the texture's component selectors (`compSel`, applied to what a lookup returns, as Cemu's image views do), the alpha test
    (a discard in the pixel shader), the stencil test (`GX2SetDepthStencilControl`'s 13 arguments and `GX2SetStencilMask`; one read and
    write mask for both faces), depth bias (`GX2SetPolygonOffset` and the enables of `GX2SetPolygonControl`; one bias for both faces;
    `GX2SetCullOnlyControl` writes the same register with the offset off, so it turns the bias off),
    the viewport's depth range, and a draw that culls both faces draws nothing.
  - **Diagnostics**: `MH3U_GFX_REPORT=1` prints a `[gfx report]` block every report window: draws and skipped draws by reason, the
    shaders not translated and why (with the draws skipped for each), the texture formats / tile modes / kinds read (binds, images,
    largest size, levels, selectors, and `APPROXIMATE` or `NOT DECODED`), textures that failed to decode and header pitches that differ
    from the computed one, the vertex attribute formats with their swap mode, and once the `gx2` imports that do nothing here.
    `MH3U_GFX_DUMP_FRAME=<n>[,<n>...]` writes frame `n` (counting swaps) to `MH3U_GFX_DUMP_DIR` (`mh3u-frames` by default) as
    `frame-<n>/`: `draws.txt` (every draw's shaders, state, attributes, textures and whether it was drawn or why not),
    `shaders/vs-*.txt` and `ps-*.txt` (listing and WGSL, or why not translated), `textures/*.png` and `tv.png`. Each draw also lists
    its sampler's settings, marks a texture that is a buffer drawn earlier in the frame (`[the colour buffer of draws a-b of this
    frame]`), and shows its uniform blocks (the pixel shader's first 32 vec4s, the vertex shader's first 16, as floats and as hex
    words, both as the GPU reads them: little-endian); `targets.txt` is the frame's render-target chain (each image drawn into, by which draws, and which draws sampled it).
  - **Build stamp**: the log's `[gfx] build <commit>` line and the first line of each `draws.txt` name the commit the binary was
    built from (`+changes` when the working tree differed).
  - **Depth bias off**: `MH3U_GFX_DEPTH_BIAS=off` drops every depth bias (a debug toggle: without it the shadow pass gets acne), to
    check whether a draw loses to the depth its own depth-only pass wrote with a different bias.
  - **Pixel shader override**: `MH3U_GFX_DEBUG_PS=<mode>` replaces what pixel shaders put out (after the program, with no alpha test),
    to tell apart where a wrong picture comes from; `MH3U_GFX_DEBUG_PS_ONLY=<hex addresses>` limits it to those shaders. Modes:
    `white` (the geometry: what is covered), `alpha1` (the program's colour made opaque: a picture that appears means the alpha was
    0 and blending hid it), `tex` (the first ordinary texture lookup's texels: black means the texture or its binding), `uv` (that
    lookup's coordinates as red/green, `fract`ed: flat colour means the coordinates are wrong), `inputN` (input register N as it
    arrives from the vertex shader, e.g. `input1` the vertex colour of many shaders: black means the vertex stream or the vertex
    shader) and `normalN`/`normals` (the same mapped from -1..1). Magenta: the shader has no such lookup or input.
  - **Uniform blocks** are read to the next 256 bytes past the size the game gives (`SQ_ALU_CONST_BUFFER_SIZE` is in 256-byte units),
    as the hardware does.
  - **Vertex attributes** go to the registers the vertex shader's semantic table (`SQ_VTX_SEMANTIC`, `GX2VertexShader` +0x40/+0x44)
    names for their location (the fetch shader's semantic id), not `location + 1`; an attribute the table does not name is not
    loaded.
  - **Shadow maps drawn as colour**: a buffer drawn into as colour (the village's 1024x1024 `r32f`) and read with a comparison
    (`SAMPLE_C`) is copied into a depth texture (first component, clamped to 0..1) when it has changed since the last copy, and the
    comparison is the sampler's own (`GX2InitSamplerDepthCompare`; less or equal when none was set). Before, such a lookup read a
    1x1 depth stand-in. The game's shadow bias is in the receiving shader (reference `z - c`, `c` from pixel block 4), not a depth
    bias, so the map must keep 32-bit precision (up to fb2f3db it was made 8-bit, and it was blended).

**Not done yet:** the rest of the sampler state (borders, the mip bias), separate stencil masks and depth bias for back faces, other
geometry inputs (lines, triangles, instances), rects and other primitive types, stream out, a viewport larger than its target (it is
clamped, which squeezes the picture), the `*Reg` forms of the state calls (`GX2SetDepthStencilControlReg`...; the report lists which
the game imports), and the second render target (the GamePad's picture is drawn but not shown). (Speed is in "Speed" above: draws are
batched and their buffers come from arenas.)

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
Vertex shaders are translated with the fetch shader replaced by decoded attribute streams (vertex pulling). Loops become WGSL `loop`s
with the exec-mask stack pushed at the start and popped at the end: a lane that is off at the top of the loop leaves it, a breaking
clause sends the lanes it turned off out of the loop, a continuing one sends them to the next trip (live again), and `LOOP_BREAK` /
`LOOP_CONTINUE` do that to the lanes that are on. Relatively addressed operands add `ar.<c>` (set by `MOVA*` after its group) or
`al` (the loop index of `LOOP_START`), clamped to the register file, the 4,096-entry bank or the 256 constants. The geometry pipeline's three programs are translated by `translate_es`, `translate_gs` and
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
- **Audio** (`audio.md`): the `AX` calls feed the mixer in `mh3u-audio`. The audio thread runs the game's frame callbacks and mixes a
  3 ms frame of the voices, paced by the sound device (`cpal`). A voice's position, which the game polls to refill its streams and to
  know that a sound ended, moves with what is mixed. `MH3U_AUDIO=off` mixes without playing.
- **Alarms** (`OSSetAlarm`...) run their handlers on a thread of their own when they are due.
- **Tiling apertures** (`GX2AllocateTilingApertureEx`, `GX2FreeTilingAperture`, with `GX2GetSurfaceMipPitch` and
  `GX2GetSurfaceFormatBits`), in `mh3u-rt/src/gx2.rs`: every custom colour in character creation came out black (issue #31). The
  picker (`FUN_0274c1ec`) reads the texel under the cursor from a CPU copy of its palette texture, which `FUN_0274a9f0` makes through
  a tiling aperture (`FUN_02957070`), a linear view of the tiled texture. With the four calls stubbed the aperture's address was 0,
  so the copy was zeros and every pick was black (the presets are a table, so they worked). Now an aperture is a linear copy of
  the level's slice at `0xe8000000` and up, with the bytes turned around as the endian mode asks, and closing it writes the copy
  back into the surface.
- **Software keyboard** (`swkbd.rpl`, found at run time with `OSDynLoad_FindExport`): answered by the host in
  `mh3u-rt/src/swkbd.rs`. The name field on the character screen used to stop after its notice. The cause: the game's keyboard
  set-up (`FUN_0293af30`) calls `SwkbdInitLearnDic` once per controller and gives up at the first 0, and every stub returned 0. So
  the keyboard was never made and `SwkbdAppearInputForm` never came.
  - **Set-up:** `Create` and `InitLearnDic` now return 1.
  - **Showing it:** `AppearInputForm` makes the form's state 2 (shown).
  - **Answering:** OK is reported once the text is decided. `GetInputFormString` returns it as UTF-16 at
    `native_text::BUFFER + 0x8000`, at most 10 characters.
  - **Where the text comes from:** `MH3U_SWKBD_TEXT` when it is set (taken at once), otherwise the overlay opens with a keyboard
    window (type, then Enter or OK; Cancel cancels). See `docs/overlay.md`.

Debugging aids (all in `mh3u-tools run`): `MH3U_TRACE=1` (every system call with its result, and file opens), `MH3U_DUMP_AFTER=<s>`
(what every thread is doing and which system function it called last), `MH3U_GFX_LOG=1` (the first draws' state, and draw counts), and,
for a library built with `build-game --watch`: `MH3U_WATCH=<addr>,<len>` (every store, also by the runtime, to that range, with the
return address), `MH3U_ENTER=<function>` / `MH3U_ENTER_LR=<return address>` (report entries of a function, or of whatever is called
from a given call site), `MH3U_PROFILE=<s>` (a sampling profile of the translated code). `mh3u-tools disasm` prints readable
PowerPC (`mh3u_ppc::format`). More: `MH3U_TRACE_FROM=<s>` (trace only after that long), `MH3U_TRACE_DYNAMIC=1` (calls of functions found
at run time, with arguments and return address), `MH3U_TRACE_PAD=1` (what `VPADRead` returns), `MH3U_TRACE_AX=1` (voice offsets and state changes),
`MH3U_PRESS_DUTY=<percent>` (how long of its period `MH3U_PRESS` holds), `MH3U_PRESS_SWEEP=<s>` and `MH3U_TOUCH_SWEEP=<s>` (press every
button, or touch a grid of points, in turn from then on), `MH3U_PAD_LAYOUT=nintendo` (gamepad face buttons with A and B, and X and Y, swapped back to the earlier mapping).
