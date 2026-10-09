# A PC port of Monster Hunter 3 Ultimate: a proposal

Status: **written as a proposal before the port was built; the spikes are done (`docs/spikes.md`) and the port now exists.** The
translator, the runtime and the graphics run the game to its character screen and the village (steps 4 to 6 below, and input from step 7);
`docs/translator.md` says how, and what is not done. The plan below is kept as it was argued, with the crate table brought up to date.
This document says what the project would be, what it needs from you, how it would be
built and in what order, and what is not known. It was meant to be argued with before any of it was started. The companion app in this
repository was the groundwork; this repository started from copies of its readers (see "What the companion app gave"), and the port
itself is new crates here.

## You must own the game

The port ships **no game code and no game data**: no executable, no models, no textures, no sound, no text, no tables. It cannot
run without them. To use it you need:

- **A copy of Monster Hunter 3 Ultimate for the Wii U that you own**, dumped by you from your own console and disc or download (the base
  game, title id `0005000010118300`, and the update, `0005000e10118300`). The project does not explain how to get a game from anywhere
  but your own copy and will not link to one.
- The port starts by asking where that dump is, checks that it is the right game (the title ids, the executable's and the data
  archives' sizes and hashes), and **refuses to start without it**. There is no demo mode and no fallback.

What this does and does not mean:

- It is how the port stays a *tool for people who own the game* rather than a way to get the game: everything that makes it MH3U
  comes from your files.
- It cannot prove you bought the game. A check can only tell whether the files are the right ones, not where they came from. The
  rule is a term of use, and it is stated up front: the port is for owners of the Wii U game.
- The Wii U servers for this game are gone, so there is no online play and nothing in the port connects to anything.

## What it is, and what it is not

Goals:

1. Run the game from the owner's own dump on **Linux and Windows**, natively, without an emulator.
2. Be a better base for what an emulator cannot easily do: a modern renderer and window, any resolution and frame rate, native
   keyboard, mouse and gamepad input, and room for mods that change data (the game's tables are already readable here).
3. Stay honest about what works: a list of what runs, what does not, and what is guessed.

Not goals:

- **No piracy tooling**, no bypass of the ownership check, no asset or code redistribution.
- **No online features**, and nothing that touches Nintendo's or Capcom's services.
- **Not a rewrite of the game** and not a "remake": the game's own code is what runs (see "Approach").
- **Not a replacement for Cemu.** Cemu already runs the game on PC; the port is only worth building for what it can do beyond that.
  If it never gets past Cemu, the companion app still stands on its own.

## Language and portability

**Rust**, for everything we write. Reasons:

- The groundwork is already Rust (`mh3u-core` reads the executable, the archives and the save; `mh3u-tools` explores them), so it is
  reused as it is, with its tests and its hostile-input checks.
- One toolchain builds on Linux and Windows with no per-platform code for most of it. Where the platform matters (windows, input,
  audio, the graphics API) we use portable crates (`winit`, `wgpu`, `cpal`, a gamepad crate) and nothing else.
- The workspace forbids `unsafe` (`unsafe_code = "forbid"`). A recompiled game needs raw memory access for the guest's address space,
  so the runtime would be the first place that needs `unsafe`. It would live in **one small crate** that is allowed to, kept apart
  and audited, and every other crate keeps the rule.
- The recompiled game itself (see below) is generated code, and **it is generated as C**, not Rust. It is made on the owner's machine
  from their own files, so no generated code is ever stored in or shipped from this repository. We measured it (`docs/spikes.md`,
  spike 4): the same code builds about four times faster as C and takes a tenth of the memory, where one Rust crate of the whole game
  would need about 59 GB to compile. The owner needs a C compiler (`cc` on Linux; on Windows the Visual Studio build tools, which Rust
  needs anyway). Everything else, including the program that runs the generated code, is Rust.

## Approach

Two ways to run the game's PowerPC code without an emulator exist, and they are not the same amount of work:

- **A decompilation** rebuilds readable source that compiles back to the same machine code, function by function. That is the most
  useful result and by far the most work: it is years of effort by a team for a game this size, and nobody is known to have started one
  for this game (a quick search found none; not an exhaustive one). We do not plan on it. It can grow out of the other path, one
  function at a time, if people want it.
- **A static recompilation** translates the game's PowerPC (the Wii U's "Espresso" cores) instruction by instruction into code for
  the PC, once, ahead of time, and runs it against a runtime that stands in for the Wii U's operating system and graphics. The result
  is not readable, but it runs the real game. This is the plan.

The static recompiler is a CPU translator and nothing more. What makes the game *run* is the runtime, and that is the larger part:

- **The Wii U's system layer** (Cafe OS): what the game imports from `coreinit` (threads, memory, files, time), `gx2` (graphics),
  `vpad`/`kpad` (controllers), `sndcore`/`mix` (audio), `nn_*` (accounts, saves, network) and the rest. The size of this surface is
  **not known yet** and is the first thing to measure (spike 1 below).
- **Graphics**: the game talks to the GX2 graphics API with its own shaders, in the console's formats. Those have to be turned into
  calls to a PC graphics API (via `wgpu`, so Vulkan, Direct3D 12 and Metal come with it). Textures are stored in the Wii U's swizzled
  layout and must be unswizzled. This is where most of the effort and most of the risk are.
- **The engine's file formats** (Capcom's MT Framework): archives and text we already read; models, animations, effects and the
  scripting would be read by the game's own code, which we run, so we do not have to understand them to run the game. We only need
  to understand them for features beyond running it (viewers, mods).

## Architecture

New crates, beside the existing ones (nothing is renamed):

| Crate | What it does | Built from |
|---|---|---|
| `mh3u-formats` | the game's files (planned as `mh3u-core`; the save is not read here) | exists |
| `mh3u-dump` | finds the owner's dump and checks that it is this game (title ids and files; the hash check is not written yet); the only way any other crate learns where the game is | exists |
| `mh3u-ppc` | decodes Espresso instructions; the reference interpreter | exists |
| `mh3u-recomp` | translates the code to C (the recompiler; planned as part of `mh3u-ppc`) | exists |
| `mh3u-rt` | the runtime: guest memory, threads, the Cafe OS functions, the entry point, input. **The one crate that may use `unsafe`** | exists |
| `mh3u-gfx` | GX2 to `wgpu`: render state, draws, textures, the window | exists |
| `mh3u-latte` | the GPU's shader programs, to WGSL | exists |
| `mh3u-testkit` | the interpreter against QEMU, the translated C against the interpreter | exists |
| `mh3u-platform` | window, input, audio, saves on disk | not made: the window is in `mh3u-gfx`, input and saves in `mh3u-rt`, no audio |
| `mh3u-port` | the program: asks for the dump, runs the recompiler on first start, then the game | not made: `mh3u-tools translate`, `build-game` and `run` do it by hand |
| `mh3u-tools` | explorers, and the commands that translate, build and run the game | exists |

First start: the owner points the port at their dump; `mh3u-dump` verifies it; `mh3u-ppc` reads the executable from it and writes
the translated program into the owner's own cache folder (never into the repository); later starts reuse it. Delete the cache and
it is made again. If the dump changes or disappears, the cache is refused.

## Order of work

Each step ends with something that can be checked, and the project stops being worth it at the first step that cannot be passed.

**Spikes first, each a few days, none depends on the others:**

1. **Imports (done: 578 functions and 10 data in 22 libraries, about 390 that have to work; see `docs/spikes.md`).** List every function the executable imports from the system libraries, from its import tables (the RPX reader here
   already unpacks the sections). The result is the size of the runtime we would owe. If it is a few hundred functions, go on. If it is
   many more, or leans on parts nobody has a good account of, say so and stop.
2. **Instruction coverage (done: 99.9999% decoded, 134 instructions, 0.37% paired singles, 4,822 indirect jumps; a translator is weeks; see `docs/spikes.md`).** Decode the whole code section and count the instructions by kind and how many are unusual (paired
   singles, special registers, cache control). The result is whether a translator is a month or a year.
3. **The graphics (mostly done: 579 different pixel programs and 76 vertex programs, all in the game's files, using 40 + 6 ALU operations, 3 texture fetches and plain control flow with loops; see `docs/spikes.md`).** Take one known draw call from a Cemu capture and say what GX2 state and shader it needs. The result is whether
   the graphics layer is reachable at all.

**Before the translator: spike 4 (done: as C, about 5 minutes on one core for the whole game, as one Rust crate impossible; so the translator emits C; see `docs/spikes.md`).**

**Then:**

4. **The recompiler runs a small function** (started: `mh3u-ppc` has a reference interpreter that agrees with QEMU's PowerPC 750 on every instruction but the paired singles; see `docs/translator.md`), checked against Cemu or against a reference interpreter on made-up input (the
   interpreter is also what tests the translator, so it is built first).
5. **The runtime starts the game's entry point** and reaches the first system call it cannot answer, and the list of those is the to-do list.
6. **A window and a cleared screen**, then **the title screen**, then **a village**. Each is a milestone with a recording.
7. **Input, audio, saves** (the save is the same file format the companion app reads, so the two work together).
8. **Quests**, then the rest.

Not before step 5 works: any promise of dates.

## Rules for contributors

- **No game material in the repository**: no bytes, tables, strings, text, icons, shaders, disassembly or recompiled output taken from the
  game. The companion app's data tables (`crates/mh3u-core/data`) are hand-made and joined to names read from the owner's own dump at
  run time; the port follows the same rule. Anything derived from the game stays on the owner's machine.
- **No pirated material in issues, pull requests or chat**: no links to games, keys or dumps, and no help getting them. Bug reports
  use hashes and addresses, not the game's contents.
- The ownership check is part of the program and is not to be weakened, made optional or hidden behind a flag.
- Third-party data used for what the game's files do not hold is credited, as in the companion app.
- Licence: this repository is public domain (see `LICENSE`). The port's own code would be the same. That covers *our* code only;
  it does not say anything about the game, which stays Capcom's.

## Risks and open questions

- **Legal.** Capcom owns the game. A project that needs the owner's own files and ships none of them is the usual shape of
  such ports and is what this proposal follows, but it is not a guarantee of anything, and a rights holder can still object to a
  project. Anyone taking part should read the licence and the rules above and decide for themselves. This document is not legal
  advice.
- **Size.** The graphics layer and the Cafe OS surface are the unknowns, and the spikes exist to turn them into numbers. A team of
  a few people over a year or more is the honest guess for a game that reaches the village; one person in spare time should expect to
  stop after the spikes, and that is a good result if the spikes say so.
- **Correctness.** The Wii U's floating point, its multi-core timing and its unusual instructions have to be matched exactly or the
  game misbehaves in ways that are hard to find. The interpreter-versus-translator tests and Cemu captures are how this is caught.
- **Why not just improve Cemu?** A fair question, and the answer may be that we should: contributions to Cemu fix the same problems for
  every game. The port is only the better route if its goals (a native build with its own window, input and mods, built around this
  one game's data) are worth a second implementation of the system layer.
- Unknown: whether the executable loads other code at run time (`.rpl` modules), whether the save and the online parts can be
  stubbed out cleanly, and how much of the game's own code depends on the console's exact timing.

## What the companion app gave

- The **executable's sections**, unpacked and bounds-checked (`mh3u-core/src/rpx.rs`), including the data section the tables are read from.
- The **archives and the game's text** (`arc.rs`, `gmd.rs`), with the bounded decompression that hostile files need.
- The **save format** and what is known of its fields, the **live-memory reader** (`procmem.rs`, `live.rs`), which also serves as a way to
  check a port against Cemu: both can be asked what the guest's memory holds.
- The **tables**: items, weapons, armor, skills, quests, drops, zones, songs; useful to a viewer or a mod tool, and as test data
  ("does the port's item box show what Cemu's does?").
- The **habits**: tests that skip without a dump, hostile-input tests, `scripts/check.sh`, no `unsafe`, no network code.

What it does not give is any understanding of the executable's code, the graphics, or the system layer. That is all new.
