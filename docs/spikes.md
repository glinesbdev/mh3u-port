# Spikes: what has been measured

Each spike answers one question about whether the port is worth doing. The numbers come from the owner's own dump (the US game with its
update); nothing from the game is kept here beyond counts and the names of the system libraries it uses.

## 1. Imports: how big is the runtime we would owe? (done)

`mh3u-tools imports <dump>` lists what the executable takes from the Wii U's system libraries, found through its symbol table.

| Library | Functions | What it is |
|---|---:|---|
| `coreinit` | 146 (+10 data) | the operating system: threads, memory, files, time, cache, atomics |
| `gx2` | 101 | graphics |
| `snd_core`, `snd_user` | 37 + 37 | audio |
| `padscore` | 29 | Wii remote and Pro controller |
| `vpad` | 20 | the gamepad (screen, buttons, touch, motion) |
| `proc_ui` | 7 | the home-button and foreground/background protocol |
| `nn_save` | 9 | saves |
| `h264` | 12 | video decoding |
| `nsyshid`, `mic`, `usb_mic`, `dmae`, `sysapp` | 9, 8, 9, 2, 2 | USB devices, microphones, a memory copy engine, system app |
| `nsysnet`, `nlibcurl`, `nn_ac`, `nn_act`, `nn_fp`, `nn_uds`, `nn_ndm`, `nn_acp` | 29, 24, 13, 17, 32, 28, 5, 2 | network, accounts, friends, local multiplayer: online play |
| **all, 22 libraries** | **578 (+10 data)** | |

Reading it:

- The number is **small** for a game of this size. The libraries that would have to really work (`coreinit`, `gx2`, audio, `vpad`,
  `padscore`, `proc_ui`, `nn_save`) come to about 390 functions. The ~190 network, account and friends functions (`nsysnet`,
  `nlibcurl`, `nn_*` but `nn_save`) can answer "not available", since the game's servers are gone and the port has no online play.
  Whether the game copes with those answers is not known and is the first thing to test.
- `gx2` has 101 functions. Cemu implements all of them, so there is a reference; the real work is the translation into a PC graphics API.
- Not counted: what the game does with the *numbers* of those calls (state it expects to persist, timing, the order of calls).
- `h264` means the game has video (a movie) the port has to decode or skip.

## 2. Instruction coverage (done)

`mh3u-tools coverage <dump>` decodes every word of the code section (`.text`, at `0x02000020`: 7.9 MB in the file, **18,441,092 bytes
once unpacked: 4,610,273 instructions**) with `mh3u-ppc`, a decoder for the Wii U's PowerPC (Espresso) written from the architecture
book.

- **Coverage: 4,610,267 of 4,610,273 words decode (99.9999%).** 134 different instructions are used. The six that do not decode are the
  words `00400000` and `00600000` at `0x0312cf14` to `0x0312d244`, near the end of the section: data between functions, not code.
- **By kind:** integer arithmetic 20.9%, branches 18.6%, loads 14.2%, floating point loads and stores 11.6%, stores 10.2%, logic 9.2%,
  compares 5.2%, floating point arithmetic 3.9%, moves to and from special registers 3.6%, rotates 1.9%, **paired singles 0.37%
  (17,209)**, sync 0.33%, condition register logic 0.08%, cache control 194 (0.004%), `tw` 2.
- **Not used at all:** the 64-bit instructions, the floating point status register (`mtfsf`, `mffs` and the like: 0), `rfi`, `sc`,
  and no special register but the link register (LR) and count register (CTR). In particular the game's code never sets the quantization
  registers (GQR0-7) that paired singles use for `psq_l` and `psq_st`, so it relies on what the system leaves in them: the runtime has to
  set them the way the console does, and that value is not in the game.
- **Costly for a translator:** load and store multiple/string (`lmw`, `stmw`, `lswi`, ...) 26,911, mostly function entry and exit;
  reserved loads and stores (`lwarx`, `stwcx.`) 388, the lock operations; paired singles 17,209, two floats at a time, needing a
  translator that handles pairs. `sync`/`isync`/`eieio` 15,189 can be ignored on one thread and mapped to fences otherwise.
- **How the branches go:** calls (`bl`) 305,978; direct jumps 90,129; conditional branches 298,778 (loops and ifs); returns 115,463
  plus 11,614 conditional returns; **indirect jumps (`bctr`) 4,822, and indirect calls through a register (`bctrl`, `blrl`) 30,605**.
  Direct branches are easy. The 4,822 indirect jumps are jump tables (switch statements) and tail calls, and each one has to be
  resolved to the list of places it can go; that is the known hard part of a static recompiler and it is a few thousand, not tens of
  thousands. The 30,605 indirect calls (function pointers and virtual calls) go through a table from the game's own addresses to the
  translated functions, filled at start: they cost a lookup each and need no analysis. *(Later note: the table is a hash table made by
  the translator in `dispatch.c`; see "The whole executable" in `translator.md`.)*

Reading it:

- **The CPU side is small.** 134 instructions in 15 kinds, many of them variations of one another, with a reference interpreter to
  test the translator against. That is weeks of work, not a year (an estimate, not a measurement).
- **The size is not small.** 4.6 million instructions translate to many millions of lines of generated code. Whether that compiles in
  a reasonable time, and as what (Rust, or C for a C compiler the owner must then have), is **not known**, and `pc-port.md` plans to
  generate Rust. A spike to settle it is needed before going on: translate a few thousand functions, compile them as Rust and as C,
  and time both (spike 4 below). *(Later note: spike 4 settled it, and the translator emits C; see `translator.md`.)*
- About 127,000 returns means on the order of a hundred thousand functions: the translated program is split into many files, which the
  owner's machine builds once, on first start.

## 3. The graphics: what the game asks of the GPU (3a, 3b, 3d done: 579 pixel and 76 vertex programs, all in the game's files; a captured frame is not needed yet)

The Wii U's GPU (Latte, an AMD R7xx/Evergreen design) runs shaders as **microcode binaries** the game hands to GX2. A port has to turn
each one into something a PC GPU runs (SPIR-V or WGSL through `wgpu`), which is the largest single piece of what an emulator does for
graphics. How many there are, and how big, is the question; it can be answered from the game's files alone.

**3a. Census of the shaders in the game's files (done).** `shaderArc.arc` (1 MB, packed) holds one entry, `shaderPack`, 13.4 MB
unpacked: a short header (`GKP`, version 4, **967** shaders) followed by 967 GX2 shader files (`Gfx2`, version 7.1, the format Nintendo's
tools write). Each holds a header block and a program block (block types 6 and 7 in that format, which I take to be a pixel shader's
header and program) and the end marker:

- **967 programs, 2,833,536 bytes of microcode in all; 2,930 bytes on average and 229,360 for the largest.**
- 967 headers of up to 10,752 bytes (10,396,256 bytes in all): the shader's description, with the names and places of its uniforms and
  samplers.
- **No vertex shaders are in this pack**, and no other archive of the game holds a `Gfx2` file (the other three matches of the search are
  chance bytes in a sound and a model). Where the vertex shaders are is **not known**: they may be built into the executable's data, or
  made from a different container. This is the first thing to find.
- A search for the magic `Gfx2` is how the shaders were found (`mh3u-tools arcsearch`), and `mh3u-tools arcx` unpacks the pack; the
  parser for the pack and its blocks is not written yet.

Reading it: a few hundred to a thousand programs of a few kilobytes each is a **finite, ahead-of-time** job: all of them can be
translated once, on the owner's machine at first start, with the C translation, and not while playing (no shader-compile stutter). It
is still a **shader compiler for Latte microcode**, which is large work (Cemu's is one of the biggest parts of it). Cemu's source is
under the Mozilla Public Licence, which does not mix with this repository's public-domain licence; it can be read to learn what the
instructions mean but not copied from.

**3b. The shader instruction census (done).** `mh3u-tools shaders <dump>` reads the pack and walks every program with `mh3u-latte`
(control flow, then each ALU and texture clause it names). The check that the format is read right is strict: **every one of the 967
programs ends where it should, every clause names addresses inside its program, every ALU clause holds exactly the number of slots its
control flow says (294,482 slots in 14,310 clauses), and every program has exactly one `EXPORT_DONE`.** (Words are little-endian; read as
big-endian the same programs do not parse.)

- **Control flow, 25,546 instructions:** ALU clauses 14,310 (plain 7,226; push before 3,862; pop after 3,219; a rare fourth kind 3),
  texture clauses 3,030, `JUMP` 3,781, `ELSE` 2,978, `POP` 480, `EXPORT_DONE` 967. **No loops, no calls, no returns, no early kills in
  control flow**: only structured if/else. That is the simplest shape a shader can have.
- **ALU: 261,985 instructions in 82,799 groups** of one to five instructions issued together (1: 23,413; 2: 7,246; 3: 11,903; 4:
  12,814; 5: 27,423), with 32,497 slots of literal constants.
- **Only 33 different two-operand operations and 5 three-operand ones are used** in all of it, and 3 texture fetch kinds. By use:
  `MUL` 62,083, `MULADD` 26,525, `ADD` 25,416, `DOT4` 23,756, `MAX` 19,990, `DOT4_IEEE` 17,416, `MIN` 17,004, `CNDE_INT` 10,497, `MOV`
  10,322, then operations 48, 108, 112 and 105 (6,000 to 9,400 each), `MULADD_IEEE` 4,000, and 20 more in smaller numbers (the rest by
  number in the tool's output; I only name the ones whose numbers I am sure of).
- **Texture: 4,948 fetches in 3,030 clauses**: instruction 16 (a plain sample) 3,244, 24 (a sample with a depth comparison, for shadows)
  1,346, and 0 (358, not a sample).

Reading it: **the GPU side of the instruction set is small and plain.** About 40 operations, structured control flow, no loops. A
translator from this to SPIR-V or WGSL for the pixel shaders is weeks of work, not months, and the shader count (967) makes it an
ahead-of-time job. What remains is everything around the shaders (state, formats, the vertex side).

**3d. The vertex shaders (done).** The 967 pixel shader files are 579 *different* programs, in three places: `shaderArc.arc`'s `shaderPack`,
a loose copy of the same pack (`content/nativeCafe/shaderPack.spkg`, the same 13.4 MB) and 970 loose files in `content/nativeCafe/es/`
(967 of them GX2 shader files, one shader each, with smaller programs: 2.6 MB in all, the largest 6,480 bytes against 229,360 in the
pack; why they differ is not known). No vertex shader is in any of them, in any archive, or in the executable's data as a GX2 file.

`mh3u-tools scan-shaders <dump> <out>` (Linux; start Cemu on the game, press Enter in a scene) looks through all of the game's memory
at every 256-byte boundary for the pack's own pixel programs (exact match of their first 32 bytes) and for vertex-like programs (one that
starts by calling its fetch shader, parses cleanly with `census`, exports and does arithmetic). Run on a real Cemu with a scene up, three
scans gave the same result each time (2 s each):

- **All 579 different pixel programs of the pack are in memory**: Cemu loads the whole pack at start. That is the proof that the scan
  finds what is there.
- **76 vertex-like programs, no more** (and each scan the same 76): 11,262 ALU instructions in 3,449 groups, 836 exports (about 11 each:
  a position and its parameters), 28 two-operand and 4 three-operand operations, 333 texture fetches, and control flow that has what the
  pixel shaders did not: **loops** (8 programs: `LOOP_START_DX10`, `LOOP_END`, and `ALU_BREAK`; the skinning of a model by its bones is
  the likely use, but that is a guess).
- **Where they are in the files: `content/nativeCafe/LtUserShader/LtUserShaderPackage.lfx`** (896,508 bytes, 775 entries by its header,
  magic `MFX`). Every one of the 76 is in it whole, at a 4-byte boundary, and a scan of the file alone for the same structure (no Cemu)
  finds 77 hits, 76 different: the same set. So **the game's whole shader inventory is in its files: 579 different pixel programs and 76
  vertex programs.** The container (`MFX`) is not read yet; the programs can be found in it by structure, as the scan does.

Together the whole game uses **40 different two-operand ALU operations, 6 three-operand ones and 3 texture fetch kinds**, and control
flow of ALU and texture clauses, if/else, loops with break, and exports. That is the instruction set a shader translator has to cover:
small, plain and finite, and the programs can all be translated once, before the first frame.

**3c. One frame (later, and needs a capture).** What a draw call asks for at run time (the render targets, the textures' layouts, the
blend state), to show the rest of the graphics layer is reachable. A capture from Cemu through RenderDoc shows Vulkan calls, not GX2, and
it contains the game's textures, so it stays on the owner's machine and out of the repository. It is needed after 3a and 3b, not before.

**3e. Why the 3D was wrong (checked offline, October 2026).** The 2D pictures were right and the models and areas were not, so the
paths only 3D uses were checked against references, each with tests in the repository:

- *Tiling* against AMD's address library itself (built from its MIT sources on this machine, configured as the console's R7xx: family
  0x51, `gbAddrConfig` 0x44902): 1,256 of 9,828 cases differed before (macro-tiled pitches that are not a power of two, every thick
  mode, base alignments); none now. Bad untiling of exactly the 2D-tiled textures fits "UI right, models noisy".
- *Vertex attributes* against GX2's attribute format list and Cemu's fetch code: the scaled formats were read as raw integers (a
  position or weight of 3 read as a denormal), signed 10_10_10_2 unsigned, and 8-bit types were swapped as if 32-bit. That fits the
  long streaks.
- *Texel byte order* against GX2's format table (no swap for any texture format) and Cemu's decoders: 16-bit packed texels were
  read byte-swapped.
- *What is not known yet:* whether the remaining wrong pictures come from these, from a shader whose semantics are guessed (the
  translator errs rather than guesses, but the order of a group's writes, `CUBE`'s output and the rounding of `MOVA` are from the
  documents only), from state the game sets through `*Reg` calls this backend does not take, or from a texture format read
  approximately. `MH3U_GFX_REPORT` and `MH3U_GFX_DUMP_FRAME` (see `docs/translator.md`) are there to tell those apart on the owner's
  machine.

**3f. The village's scenery (owner's run 4, frame 4800 dumped).** Character select was right after 3e and the village was not: no
scenery, streaks on some models. `draws.txt` showed why: the fetch shaders of the scenery (`0x1dbd60f0`..`0x1dbd7ef0`, used by about
250 of the frame's 451 draws, vertex shaders `0x169eb340`, `0x169eee80`, `0x16978000`, `0x169ad300`, and 4 draws of the skinned
`0x1699c580`) had one `sint 16_16_16_16` stream at location 0 and every other stream all zero (location 0, offset 0, format 0), while
the vertex shaders read a float position in R1 and an integer colour in R3 (`INT_TO_FLT`). The character's fetch shaders were right.
The cause: the game makes these fetch shaders while it is recording the frame's display list, and `GX2InitFetchShaderEx` was recorded
like a GPU command, so it ran only when the list was played, reading the attribute array (the caller's, gone by then) from reused
memory; `GX2CalcFetchShaderSizeEx` returned nothing to its caller the same way. The `GX2Calc*`/`GX2Init*` calls (CPU-only: they fill
in an object) now run when they are made, recording or not, and `GX2InitFetchShaderEx` writes only the 0x20 bytes of the
`GX2FetchShader` (it wrote 0x28).

**3g. The village's black scenery (run 5).** With the fetch shaders right, the scenery has its shape but is black while the
characters, particles and sky are right. The scenery's pixel shaders (`ps-17101480`, `ps-1724c040`; draws 228/229 of run 4's frame
4800) are `vertex colour x texture 1`, fogged with a table read from uniform block 5 by index, with alpha = vertex alpha x texture
alpha x a material byte from block 1; `ps-17101480` adds four shadow lookups (`SAMPLE_C`) of texture 5, the shadow pass's
`r32f` colour buffer `0xf561e800`. Two things were clearly wrong and are fixed: that colour buffer was read through a 1x1 depth
stand-in (a colour buffer is not a depth texture), and every comparison was "less or equal" whatever the sampler said. The cause of
the black is not settled from the dumps on hand (run 5's frame dump was byte-identical to run 4's): the characters use the same
fog table and shadow lookups and are right, which leaves the scenery's own inputs (vertex colour/alpha through the vertex shader's
`INT_TO_FLT` path, texture 1, block 1's packed material bytes). `MH3U_GFX_DEBUG_PS` and the per-draw uniform blocks are there to
settle it in one run.

**3h. Why the scenery was black (debug runs on c5d6ec1).** With `MH3U_GFX_DEBUG_PS` on the two scenery pixel shaders: `alpha1` and
`input1` black, `tex` the whole village in flat texture colours, `uv` black. So the vertex colour was zero and every texture was read
at one texel (uv (0, 1): black under `fract`). In the scenery's vertex shaders (`vs-169eee80`, `vs-169eb340`) the colour is
constant 10 of block 1 (`MOV R1 <- KC0[10]`, or the decoded vertex colour times it), and the texture coordinates are the UV stream
times constants 8 and 9 (`DOT4 R4.x <- R2.x, KC0[8].x`...); position uses constants 0 to 7 and was right. The upload read only
`size` bytes of the block, and the hardware reads to the next 256 bytes (`GX2Set*UniformBlock` gives `SQ_ALU_CONST_BUFFER_SIZE` as
`(size + 255) >> 8`): a block given as 8 vec4s still has 16. Constants past the given size are now uploaded up to that rounding.
The integer colour path (`uint 8_8_8_8`, 0x10a, kept as integers for `INT_TO_FLT`) was checked and is right.

## 4. Does the generated code compile in a reasonable time? (done: as C yes, as one Rust crate no)

`mh3u-tools codegen-bench <dump> <out> --lang rust|c --functions N` translates an even sample of the game's functions (found as the
targets of direct calls) into Rust or C in the shape a recompiler would give them, to be built and timed. **It is not a translator**:
98% of the instructions get real code (integer arithmetic, rotates, compares, loads and stores, floating point, branches by block) and
the rest a placeholder call; nothing checks that it behaves like the game. It refuses to write inside a git working tree, because
its output is made from the game. The runs below are on one machine (24 threads, 31 GB, Linux, rustc 1.99, clang and gcc 16); the
sample is 4,000 functions, **486,782 instructions, 10.6% of the game**, and the "whole game" figures are that times 9.47.

| | sample (4,000 functions) | whole game, scaled |
|---|---|---|
| generated source, C | 18.9 MB, 644,000 lines | about 180 MB, 6.1 million lines |
| generated source, Rust | 31.7 MB, 860,000 lines | about 300 MB, 8.1 million lines |
| **C, clang `-O0`, one thread** | 11.4 s | about 108 s |
| **C, clang `-O1`, one thread** | 31.8 s | **about 5 minutes** (about 40 s on 8 cores) |
| C, clang `-O2`, one thread | 37.2 s | about 6 minutes |
| C, gcc `-O1` (500 functions only) | 4.6 s against clang's 3.6 s | about the same, a little slower |
| C, memory | 144 MB for one 100-function file | small, and per file |
| **Rust, one crate, `opt-level` 0 or 1** | 78 s wall (24 threads), 126 s of CPU, **6.2 GB peak** | about 12 minutes and **about 59 GB**: not possible |
| Rust, a crate of 500 functions | 17.6 s wall, 20 s CPU | 80 such crates: about 27 minutes of CPU |

What it says:

- **Generate C.** The same code as C builds about four times faster than as Rust and needs a tenth of the memory per unit, and the Rust
  front end (type checking and borrow checking of millions of lines in one crate) is what costs, so turning optimisation off does not
  help: `opt-level` 0 took as long as 1. Rust would have to be cut into dozens of crates with no calls between them (calls through a
  table), which is possible but is more machinery for a slower build.
- A C compiler is not a new requirement: on Linux `cc` is nearly always there, and the Rust toolchain on Windows needs the Visual Studio
  build tools, which have `cl`. **Not tested:** MSVC's `cl` on this code (a Windows machine is needed), and clang-cl.
- **Numbers to treat as lower limits.** The placeholders and the simple memory model (no address checks, no byte-order tricks beyond
  a swap) are cheaper than what a real translator has to emit, and paired singles, exact rounding of single precision and
  reservations will add code. Allow for twice the figures: about ten minutes for the first build on one core, and under two on eight.
- **Not measured at all:** how fast the generated code *runs*. At `-O1` compile time is acceptable; whether the game reaches a frame
  rate depends on the runtime and the graphics, not on this.
- Build once on first start (or on a change of the game's files) and keep the result in the owner's cache folder: ten minutes once is
  acceptable, ten minutes every start is not.

## The executable's layout (for reference)

`.text` at `0x02000020` (7.9 MB compressed in the file, 18.4 MB unpacked), `.rodata` at `0x10000000`, `.data` at `0x10164640`, `.bss` at `0x1030a740`
(1.4 MB), the imports at `0xc0006040` on, and relocation tables for text, rodata and data. See `mh3u-tools sections`.
