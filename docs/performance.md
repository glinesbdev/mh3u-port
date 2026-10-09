# Performance: where it stands and what is open

A running list of measured costs and open concerns. Each entry says what was measured, on what, and what is only a guess
(rule 5 of `code-guidelines.md`). Update it when a number changes or a concern is fixed.

## How to measure

With the work in PRs #1–#3 (`perf/regs-flags-dispatch` → `windows-build` → `perf/draw-overhead`):

- `MH3U_FPS=1`: an `[fps]` line every 5 s.
- `MH3U_GFX_TIMES=1`: per report window (`MH3U_TIMES_WINDOW`, default 10 s), time per graphics call and per draw step, with
  per-frame columns; `[draw counts]` for cache hits and misses; `[baton]` for guest thread handoffs.
- A CPU profiler (Visual Studio on Windows). On `x86_64-pc-windows-gnullvm` the PDB only has public symbols, so a profiler
  gives each sample to the nearest public name before it: read names from `llvm-nm -n --demangle mh3u-tools.exe` instead
  (see `translator.md`).

## Measured (Windows, AMD GPU, `build-game --native`, Oct 2026)

| Where | Before PRs #1–#3 | After |
|---|---|---|
| Title screen | ~108 FPS | ~111 FPS |
| Village, standing | 13–17 FPS | ~73 FPS (~450 draws/frame) |
| Other areas and fights | not measured | 32–43 FPS |

The village gains came, in order of size, from: one baton wake-up per handoff instead of waking every finished guest thread;
uniform banks read by index no longer read, hashed and uploaded as a whole 64 KB per draw; cached bind groups, views and
shader lookups; display lists replayed without copies or string matching. The register/flags/dispatch work in #1 did not
move the village on its own.

## Open concerns

1. **Untimed time inside `GX2DirectCallDisplayList`.** In areas and fights it takes 16–19 ms per frame, but the timed draw
   steps add up to about 4 ms. The other 12+ ms is not measured. *Guess:* wgpu command encoding, submission or the flush at
   the end of the list. First step: time encode, submit and flush separately.
2. **Pipeline creation when entering an area.** "pipeline" reached ~4 ms per frame in the first window after a load (0.2 ms
   otherwise). *Guess:* new shaders compiled on the render thread. Options: a pipeline cache on disk, or compiling ahead.
3. **Shader cache misses that never stop.** About 30 pixel and 22–30 vertex shader misses per frame even while standing still.
   Not yet known whether the key changes needlessly (something in it that does not affect the translation) or the game
   really uses that many variants.
4. **Vertex bytes hashed.** 1.5–3.5 MB per frame are hashed to check vertex streams. XXH3 keeps it cheap (~0.2 ms), but
   tracking writes through `GX2Invalidate` could skip most of it.
5. **Single core for guest threads.** All guest threads share one host core through the baton (`kernel.rs`). About 15 handoffs
   per frame now, cheap after the fix; a bigger change for later.
6. **Build size and time** from keeping registers in locals (#1): measured on random code only (C source 16 → 25 MB, `.so`
   18 → 30 MB); not measured on the game.

## Fixed (for the record)

- Gamepad thread suspected of spinning: it was not; that profile was mislabelled baton time.
- Textures re-decoded after every overlapping `GX2Invalidate` (~15k a second): now re-hashed and only decoded if changed.
