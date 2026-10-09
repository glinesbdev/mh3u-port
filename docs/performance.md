# Performance notes

What the timing reports (`MH3U_FPS`, `MH3U_GFX_TIMES`, `MH3U_GFX_REPORT`: a block every 10 s) have shown, and what is still open.

## Open: area changes and the slowing replay (run 11, 500c143)

The user felt stutter on leaving the village. Run 11's log has ten 10-second windows: title and village load (windows 0-1),
leaving for the field (window 2; frame 2400, in the field, is written at the start of window 3), and the field (windows 3-9).

| window | fps | draws/frame | replay ms/frame (`GX2DirectCallDisplayList`) | `pipeline` ms/frame | PS / VS / geometry front misses per frame |
|---|---|---|---|---|---|
| 0 | 102.7 | 18 | 2.3 | 1.0 | 3.8 / 2.3 / 1.1 |
| 1 | 75.3 | 93 | 7.6 | 4.7 | 10.0 / 5.2 / 2.6 |
| 2 | 58.0 | 331 | 12.0 | 4.0 | 31.6 / 23.4 / 1.9 |
| 3 | 55.9 | 455 | 12.6 | 0.2 | 46.4 / 28.0 / 3.2 |
| 5 | 41.6 | 426 | 18.4 | 0.1 | 46.9 / 27.6 / 4.0 |
| 7 | 35.4 | 426 | 22.6 | 0.1 | 47.8 / 27.8 / 4.0 |
| 9 | 31.1 | 416 | 26.4 | 0.1 | 46.5 / 27.8 / 4.0 |

Two separate things:

1. **Pipelines made while playing.** The `pipeline` step (pipeline cache lookup and, on a miss, `create_render_pipeline`, which
   compiles the shaders in the driver) took about 3.5 s of window 1 and 2.4 s of window 2, against 0.1 ms a frame once the field
   was set up. That is the hitching on an area change: every new mesh, material or state combination stops the frame while the
   driver compiles. Texture decoding is small (0.2 ms/frame at most), and shader translation (`shaders`) under 0.3 ms/frame.
   The log has only 10-second sums: no slowest frame, no count of pipelines made, and no line at an area change.
2. **The replay slows down with nothing more to draw.** From window 3 to 9 the draws (about 426 a frame) and the display-list
   calls replayed (about 6190 a frame) stay the same, but the replay's cost rises evenly from 12.6 to 26.4 ms a frame (about 2.3 ms
   more every 10 s), and the timed steps of a draw add up to only about 3.5 ms a frame; the rest is not timed. Something grows
   every frame. The likeliest suspect: the shader front caches still miss about 80 times a frame in a scene that does not
   change (their keys hold the address of the fetch shader's `Arc`, and a fetch shader the game makes again gets a new one), each
   miss can add an entry, and every replayed `GX2Invalidate` with the shader or CPU bit (the list replays about 1100 invalidates a
   frame) walks all the entries of all three front caches (`FrontCache::invalidate`, a `retain`). Not yet proven: the log has
   no cache sizes.

### Plan, in order of value

1. **Measure (small, needed first).** Per window: the sizes of the front caches, the pipeline cache, the bind group cache and the
   texture cache; pipelines made (count and ms); invalidates that touch shaders; the slowest frame and the time spent in
   `invalidate_shaders`. A `[gfx] area change` line when the set of targets or the draw count jumps, with the frames around it timed one
   by one.
2. **Stop the slowdown (large value: the field goes from 31 fps back toward 55 fps and stays there).** If the front caches grow: key
   them by the fetch shader's contents (or its address and a hash of its attributes), not the `Arc`'s address; index the entries
   by program address (a `BTreeMap` of ranges) so an invalidate touches only what overlaps; and cap or age the entries. If it is
   another cache, the same treatment there.
3. **Hide pipeline compiles on area changes (medium to large value: the 2-6 s of hitching on first entry to an area).** In order:
   (a) build pipelines on a worker thread (`wgpu` devices are `Send + Sync`) and skip the draw, or draw it with a cheap stand-in
   pipeline, until its pipeline is ready (what Cemu's async shader compile does; it costs a frame or two of missing objects on
   first sight); (b) keep pipeline descriptions on disk (shader hashes and the key's state) and build them all at start-up or on
   the loading screen, so a second visit, or a second run, never compiles while playing; (c) `wgpu::PipelineCache` (Vulkan only,
   `Features::PIPELINE_CACHE`) saved to disk shortens each compile but does not remove it; on D3D12 the driver keeps its own cache
   already. (a) and (b) together remove nearly all of it; (c) alone saves part of each compile, and only on Vulkan.
4. Smaller: the untimed part of the replay (about 3.5 ms of 12.6 ms is timed, even early in the field) is worth timing too,
   once the growth is gone.
