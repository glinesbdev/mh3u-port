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
   otherwise). Run 11 (500c143): about 3.5 s of window 1 and 2.4 s of window 2 (village load, leaving for the field) went to
   it, against 0.1 ms a frame once the field was set up: the hitching the user feels on an area change. Texture decoding (at
   most 0.2 ms a frame) and shader translation (under 0.3 ms a frame) are small. *Guess:* the driver compiling each new
   pipeline on the render thread. Options: build pipelines on a worker thread, a pipeline description cache on disk built at
   start-up, `wgpu::PipelineCache` on Vulkan.
3. **Shader cache misses that never stop, and a replay that slows down.** About 30 pixel and 22–30 vertex shader misses per frame
   even while standing still (run 11 in the field: ~48 pixel, ~28 vertex and 4 geometry pipeline misses a frame). In run 11 the
   field's draws (~426 a frame) and replayed display-list calls (~6190) stayed the same from window 3 to 9, but
   `GX2DirectCallDisplayList` rose evenly from 12.6 to 26.4 ms a frame (about 2.3 ms more every 10 s; 55.9 → 31.1 FPS):

   | window | fps | draws/frame | replay ms/frame (`GX2DirectCallDisplayList`) | `pipeline` ms/frame | PS / VS / geometry front misses per frame |
   |---|---|---|---|---|---|
   | 0 | 102.7 | 18 | 2.3 | 1.0 | 3.8 / 2.3 / 1.1 |
   | 1 | 75.3 | 93 | 7.6 | 4.7 | 10.0 / 5.2 / 2.6 |
   | 2 | 58.0 | 331 | 12.0 | 4.0 | 31.6 / 23.4 / 1.9 |
   | 3 | 55.9 | 455 | 12.6 | 0.2 | 46.4 / 28.0 / 3.2 |
   | 5 | 41.6 | 426 | 18.4 | 0.1 | 46.9 / 27.6 / 4.0 |
   | 7 | 35.4 | 426 | 22.6 | 0.1 | 47.8 / 27.8 / 4.0 |
   | 9 | 31.1 | 416 | 26.4 | 0.1 | 46.5 / 27.8 / 4.0 |

   *Guess, not proven (the log has no cache sizes):* the front caches' keys hold the address of the fetch shader's `Arc`, so a
   fetch shader the game makes again misses and adds an entry, and every `GX2Invalidate` with the shader or CPU bit (about 1100
   a frame) walks all entries of all three front caches (`FrontCache::invalidate`, a `retain`). Plan: measure cache sizes,
   pipelines made, shader invalidates and the slowest frame per window; key the front caches by the fetch shader's contents;
   index entries by address so an invalidate touches only what overlaps; cap or age entries.
4. **Vertex bytes hashed.** 1.5–3.5 MB per frame are hashed to check vertex streams. XXH3 keeps it cheap (~0.2 ms), but
   tracking writes through `GX2Invalidate` could skip most of it.
5. **Single core for guest threads.** All guest threads share one host core through the baton (`kernel.rs`). About 15 handoffs
   per frame now, cheap after the fix; a bigger change for later.
6. **Build size and time** from keeping registers in locals (#1): measured on random code only (C source 16 → 25 MB, `.so`
   18 → 30 MB); not measured on the game.

## Fixed (for the record)

- Gamepad thread suspected of spinning: it was not; that profile was mislabelled baton time.
- Textures re-decoded after every overlapping `GX2Invalidate` (~15k a second): now re-hashed and only decoded if changed.
