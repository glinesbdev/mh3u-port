# Performance: where it stands and what is open

A running list of measured costs and open concerns. Each entry says what was measured, on what, and what is only a guess
(rule 5 of `code-guidelines.md`). Update it when a number changes or a concern is fixed.

## How to measure

With the work in PRs #1–#3 (`perf/regs-flags-dispatch` → `windows-build` → `perf/draw-overhead`):

- `MH3U_FPS=1`: an `[fps]` line every 5 s.
- `MH3U_GFX_TIMES=1`: per report window (`MH3U_TIMES_WINDOW`, default 10 s), time per graphics call and per draw step, with
  per-frame columns; `[draw counts]` for cache hits and misses; `[baton]` for guest thread handoffs.
  - `MH3U_GFX_TIMES=calls` keeps only the per-call and per-frame times, without the draw steps.
  - The timing has a cost of its own. Before perf/entry-dips' second commit, a Moga Woods frame read the clock about 57,000 times
    (six readings per graphics call, about 25 per draw). That is roughly 2–2.5 ms of a 16 ms frame at ~36 ns a reading (the box;
    Windows' QPC is a little cheaper).
  - It is now two readings a call and a cheaper step table: 52 ns a step mark, down from 78. The profile's `Instant::now` (7.3%
    total) was this.
- With either of the two above, every window also has `[gfx frames]` (slowest frame, frames over 33 and 50 ms), `[gfx caches]`
  (cache sizes), `[gfx pipelines]` and `[gfx invalidates]`, and entering an area prints `[gfx] area change at frame N` and,
  180 frames later, how those frames went (PR #5; `translator.md` has the details). Since PR #6 the first line also splits the
  frame into time in graphics calls and in the game, and both lines give shaders translated, found on disk, modules made or
  found, draws left for a later frame, and textures decoded, each with its ms.
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
   the end of the list. Run 12 showed that most of this untimed part is wgpu's own bookkeeping, and that it grows with the
   number of GPU buffers (concern 3). `[draw times]` now has `render pass end` (wgpu checks and records a whole pass when it
   ends; the count is the number of passes), `arena uploads`, `command encoding (finish)` (where wgpu 30 really does that
   work; see concern 7, run 18) and `submit`.
2. **Pipeline creation when entering an area.** "pipeline" reached ~4 ms per frame in the first window after a load (0.2 ms
   otherwise). Run 11 (500c143): about 3.5 s of window 1 and 2.4 s of window 2 (village load, leaving for the field) went to
   it, against 0.1 ms a frame once the field was set up: the hitching the user feels on an area change. Texture decoding (at
   most 0.2 ms a frame) and shader translation (under 0.3 ms a frame) are small. *Guess:* the driver compiling each new
   pipeline on the render thread. **PR #5 (not yet measured in the game):** pipelines are made on worker threads (a new one's
   draws wait a frame or two instead of the frame waiting), and kept on disk (`<out>/cache/pipelines`) and made again at the
   next start, so a second visit or a second run should not compile at all while playing. `wgpu::PipelineCache` is not used
   (an `unsafe` call outside `mh3u-rt`; a decision for later). To verify: `[gfx] area change` lines should show "0 made with
   the draw waiting" and a slowest frame near the others. **Runs 12 and 13 (86ec8e0):**
   - First run: every new pipeline was made on a worker (11, 55, 73, 78 a window), "0 made with the draw waiting". On
     entering the field the frame with the change took 41.6 ms (12 new pipelines, 5 translations, 15 textures).
   - Second run: `217 pipelines to make again` loaded from disk and all were made on workers at startup (1.0 s of worker
     time). After that, 0 new pipelines all run. The field change frame was 89.3 ms, with 31 shader translations and 34
     texture decodes in it. Pipelines no longer cause the hitch.
   - Correction: the 89.3 ms is the whole frame, swap to swap, so it includes the game's own time. Texture decodes are cheap:
     run 13's entry window had 75 decodes, 6 ms decoding and 4 ms uploading in total (about 0.13 ms each, so about 4.5 ms for
     34). How the rest splits between translation, making shader modules and the game was not measured.
   - **PR #6 (perf/area-entry, merged in 33f473d):**
     - The area-change line now splits the frame: time in graphics calls versus in the game or the swap. Within graphics it
       gives translating, making shader modules and decoding textures, each with a count and ms. It also shows shaders found
       on disk, modules found made, and draws left for a later frame.
     - Translations are kept on disk (`<out>/cache/translations/<graphics sources hash>`) and read back in later runs.
     - Shader modules are shared with the pipeline workers, so a translation found on disk usually gets the module the workers
       already made at startup for a pipeline read from the disk.
     - Translating is limited to 5 ms a frame (`MH3U_GFX_SHADER_BUDGET_MS`). Draws needing more shaders wait a frame, as new
       pipelines already do.
     - Texture decodes stay on the render thread. At about 0.13 ms each, neither decoding on workers nor a disk cache of
       decoded textures (several MB to read back per area) looked worth it. The new line shows the decode ms, so this can
       be revisited if it turns out to be large.
     - To verify: run twice. In the second run the area-change line should show shaders "found on disk", modules "found
       made", 0 translated, and a slowest frame near the others.
   - **Runs 16 (cold caches) and 17 (warm), b1462e0:**
     - Run 17 started with `118 translations kept` and `241 pipelines to make again` (0.84 s of worker time at startup).
     - The area-change frame went from 35–51 ms in run 16 to 11–17 ms in run 17. In run 16 it was nearly all graphics
       calls, and most of that was making shader modules, not translating:
       - village: 8 modules took 28.9 ms, 8 translations 2.6 ms;
       - leaving the village: 7 modules 24.6 ms, 7 translations 2.5 ms;
       - the field: 5 modules 25.5 ms, 5 translations 2.8 ms.

       A module (wgpu parsing and checking the WGSL) costs 3–5 ms; a translation about 0.4 ms. In run 17 every shader
       was found on disk, and its module was found made.
     - The slowest frame in the 180 after entering the field went from 118.1 ms (frame 1609, 112 ms in graphics calls;
       38 modules took 117 ms and 40 translations 9.7 ms in those frames) to 39.4 ms (frame 1561, 34.0 ms in graphics
       calls). The second entry was 38.9 ms (frame 2166, 31.2 ms in graphics calls). Those 180 frames still decoded 57
       and 75 textures (7.7 and 17.7 ms in all), with 0 translations and 0 modules made. What the remaining 30–40 ms frames
       are is open (see concern 7).
     - Moga Woods then held 66–72 FPS, against about 115 in the village.
   - The 593 and 645 ms frames at 2400 are `MH3U_GFX_DUMP_FRAME` writing a frame, not the game.
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
   index entries by address so an invalidate touches only what overlaps; cap or age entries. **PR #5 (not yet measured in the
   game):** all three. Fetch shaders with the same attributes now share one slice; the front caches are indexed by program start
   (a test with 1000 and 100000 entries: 29 and 36 ns per invalidate in a release build, no entry looked at for memory with no
   program near, where the `retain` walked every entry); entries unused for 600 frames go, and past 16384 the least recently
   used. `[gfx caches]` now shows each cache's size, so if something else grows it will show. To verify: in the field the
   replay's ms per frame and `[gfx caches]` should stay flat from window to window.

   **Runs 12 and 13 (86ec8e0): still slower over time; the cause was the vertex streams, not the shader caches.** Standing still
   in the field, draws (~400 a frame) and the timed draw steps (~3.3 ms a frame) stayed flat, and so did the guest's time
   outside graphics calls (~3.8 ms). Translations stayed at 16/76/1. `GX2DirectCallDisplayList` and one cache kept growing:

   | run 12 window | fps | replay ms/frame | timed draw steps ms | vertex streams kept |
   |---|---|---|---|---|
   | 3 (entering) | 65.5 | 9.7 | 3.7 | 8,916 |
   | 4 | 64.1 | 10.1 | 3.1 | 18,292 |
   | 5 | 50.5 | 14.3 | 3.4 | 26,314 |
   | 6 | 45.8 | 16.4 | 3.3 | 31,507 |

   Run 13 was the same: 5,156 → 14,074 → 23,239 → 30,000 streams, replay 5.0 → 9.6 → 13.3 → 15.5 ms.
   - Why the streams grew: 12–15 a frame are made from vertex bytes the game rewrites every frame, and a stream was only let go
     once all of them passed 1 GiB.
   - Why it costs time: each stream is two GPU buffers. wgpu sizes every render pass's and submit's bookkeeping by the most
     buffers ever alive at once, so the cost per pass grows even with the draws unchanged. `live_buffers_cost` in
     `draw_bench.rs` (wgpu's no-op device, release build) measured a frame of 60 passes and a submit:
     - 1,000 buffers: 0.10–0.14 ms;
     - 62,000 buffers: 2.1–3.3 ms;
     - back to 1,000 buffers after the 62,000 were made: 3.4 ms, because it follows the peak.
   - Fix: a stream not drawn with in a later frame than the one it was made in goes at the next prune (once a second); any
     stream goes after 120 frames unused. `vertex_data_rewritten_every_frame_does_not_pile_up_streams` checks it: 1,000
     frames of one mesh that stays and one rewritten every frame keep at most 64 streams. `[gfx caches]` shows the streams'
     megabytes and `[draw counts]` shows `vertex streams aged out`.
   - **Run 15 (7b89bb8) confirmed the fix:** vertex streams stayed between about 460 and 1,500, and Moga Woods held a steady
     70–76 FPS while standing still (it used to slide to 46).
   - The shader fronts show 0 entries at each window's end because the game invalidates its shader memory every frame. The
     content-keyed caches behind them catch every miss (translations stay flat), so this costs a constant ~0.3 ms (`shaders`),
     not a slowdown.
4. **Vertex bytes hashed.** 1.5–3.5 MB per frame are hashed to check vertex streams. XXH3 keeps it cheap (~0.2 ms), but
   tracking writes through `GX2Invalidate` could skip most of it.
5. **Single core for guest threads.** All guest threads share one host core through the baton (`kernel.rs`). About 15 handoffs
   per frame now, cheap after the fix; a bigger change for later.
6. **Build size and time** from keeping registers in locals (#1): measured on random code only (C source 16 → 25 MB, `.so`
   18 → 30 MB); not measured on the game.

7. **The field's steady cost, and the 30–40 ms frames after entering it.** Runs 16 and 17, per `[draw times]` window. The
   field costs about 9 ms a frame in `GX2DirectCallDisplayList` against 1.5 ms in the village. The windows can only rank
   candidates:
   - **`arena uploads`:** 2.5–2.6 s a window in the field, about 3.7 ms a frame, against 0.48 ms in the village. That is
     about 12 `write_buffer` calls a frame, roughly 300 µs each in the field against about 50 µs in the village.
   - **Bind groups:** about 110 made a frame in the field (68–77 thousand misses a window) against about 1 in the village.
   - **Vertex streams:** about 20 new ones a frame (each two buffers and two writes). Their buffer creation was not timed.
   - **`submit`:** about 0.65 ms a frame.
   - **Texture decodes:** 57–75 in the 180 frames after an entry (7.7–17.7 ms in all). Too little to explain a 39 ms frame
     alone, but they cluster.

   Branch `perf/entry-dips` (0382f7b) adds `[gfx slow frame]`. Any frame of 25 ms or more (`MH3U_GFX_SLOW_MS`) prints its
   twelve longest steps, including each non-draw graphics call, with that frame's counts. It also times stream buffer creation
   and bind group creation, and counts arena uploads and their bytes.

   **Run 18 (ab8ddec, `MH3U_GFX_TIMES=1`, a PDB build, entering Moga Woods with a Visual Studio profile; Vulkan on AMD):**
   - **What the field's steady 3.6 ms "arena uploads" was:** it held more than the uploads. Only 3 of the 12 flushes a frame
     upload anything. The lap also covered `CommandEncoder::finish`, where wgpu 30 does the deferred work: checking the passes,
     tracking resources, barriers, and making the Vulkan commands (in `amdvk64.dll`). It is now its own step, `command
     encoding (finish)`. It is the largest steady CPU cost in the field, and it grows with the draws and state changes of a frame.
   - **The 26 ms frames every 60 frames in the field.** Frames 2099, 2219, 2279, 2399, 2519 and 2819 took 25.6–29.4 ms against
     ~16 ms. They are all frame index ≡ 59 (mod 60), and each counted 961–1,469 `vertex streams aged out`.
     - About 20 streams a frame were made from vertex data the game rewrites every frame: two buffers, two writes and a bind group
       each.
     - Once a second the prune destroyed 1,000–1,500 of them at once (about 2,400 buffers and their bind groups), on the render
       thread and in the driver.
     - Their "12 ms in graphics calls" hid the rest: the swap's own time was counted in the next frame. That is fixed, and the
       area-change line no longer shows more graphics time than the frame (11.3 ms with 13.6 ms in graphics calls).
   - **The 40 ms frames after entering:**
     - Frame 1446 (41.1 ms): `decode streams` 13.9 ms over 302 draws, 10.9 ms of it decoding 171 new streams (6.3 MB). Then
       texture views 5.7 ms (34 decodes, 3.5 ms), `arena uploads` 2.6 ms, stream buffers 2.6 ms and 1,026 bind groups made (2.2 ms).
     - Frame 1985 (39.7 ms): textures 9.5 ms (62 decodes, 5.9 ms), streams 6.8 ms, `arena uploads` 3.9 ms, 1,064 bind groups.

     This is mostly first-sight CPU work in this code, spread over the frame.
   - **One-off frames:** frames 71 (55 ms) and 2694 (37.6 ms) spent 35.8 and 19.8 ms in `geometry pass`. A new
     geometry-shader combination is translated and its two compute pipelines made on the render thread, and those are neither on
     disk nor on workers. Frame 491 (217 ms, 3 textures, 40 MB decoded) is a loading screen.
   - **The profile:** 37% of samples were at one address in `amdvk64.dll`. One address taking that much looks like a wait loop:
     the village runs at the display's 120 Hz with `GX2SwapScanBuffers` at 4.95 ms a frame, which is waiting for the
     presentation. It is not work, but the call stack above that address in Visual Studio says which wait it is. `Instant::now`
     at 7.3% total was the timing (see the top of this file).

   **The fix (perf/entry-dips):**
   - New vertex streams go into the batch's arena. They get buffers of their own only when drawn again in a later frame, so
     data rewritten every frame costs no buffer creation or destruction, and its bind groups are not kept.
   - Unused bind groups and streams go every frame, a few at a time, not 60 frames' worth at once.
   - What is still open: the post-entry decodes, geometry pipelines on workers or on disk, and the command encoding's size.

   To check it: `scripts\win-run.ps1 -Fps` in Moga Woods. There should be no `[gfx slow frame]` every 60 frames, `[draw counts]`
   should show `vertex streams given buffers` well under the old ~20 stream misses a frame, and `[gfx caches]` should show fewer
   bind groups. `command encoding (finish)` gives the steady cost that is left.

## Fixed (for the record)

- Field slowdown while standing still (runs 11–13): vertex streams made every frame were kept until 1 GiB, and wgpu's per-pass
  bookkeeping grew with them. Fixed in PR #5 (f88327a); run 15 held 70–76 FPS in Moga Woods (open concern 3 has the details).
- Gamepad face buttons: run 15's log showed `[gamepad] East is the GamePad's A` with the new default layout, so an Xbox pad's A is
  the game's A (7b89bb8; `MH3U_PAD_LAYOUT=nintendo` gives the earlier mapping).

- Gamepad thread suspected of spinning: it was not; that profile was mislabelled baton time.
- Textures re-decoded after every overlapping `GX2Invalidate` (~15k a second): now re-hashed and only decoded if changed.
