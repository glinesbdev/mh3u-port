//! What a run's frames cost, for finding hitches and slowdowns (with MH3U_GFX_TIMES or MH3U_FPS set):
//!
//! - every report window (MH3U_TIMES_WINDOW seconds, ten by default): `[gfx frames]` (the slowest frame and how many were slow),
//!   `[gfx caches]` (how big each cache is: one that only grows shows here), `[gfx pipelines]` (pipelines made, where and how long)
//!   and `[gfx invalidates]` (how many `GX2Invalidate`, how many reached the shader caches, and their time);
//! - `[gfx] area change`: a frame that made many new pipelines or translated many shaders (entering an area), and after it how the next
//!   frames went (the slowest, the mean, the pipelines made).

use crate::pipelines::PipelineCounts;
use std::time::{Duration, Instant};

/// New pipelines and shader translations in one frame that mark an area change.
pub(crate) const AREA_WORK: u64 = 8;
/// How many frames after an area change are followed.
pub(crate) const AREA_FRAMES: u32 = 180;

/// The work of one frame or of a stretch of frames, as counted by the caches.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Work {
    pub translated: u64,
    pub decoded: u64,
}

#[derive(Default)]
struct Window {
    frames: u32,
    slowest: (f64, u32),
    over_33: u32,
    over_50: u32,
    invalidates: u64,
    shader_invalidates: u64,
    invalidate_time: Duration,
    examined: u64,
}

struct Area {
    frame: u32,
    left: u32,
    frames: u32,
    total_ms: f64,
    slowest: (f64, u32),
    over_50: u32,
    pipes: PipelineCounts,
    work: Work,
}

pub(crate) struct Perf {
    pub on: bool,
    since: Instant,
    last_swap: Option<Instant>,
    window: Window,
    area: Option<Area>,
    /// The counters as they were at the last swap (each frame's work is the difference).
    last_pipes: PipelineCounts,
    last_work: Work,
    /// Frames to wait before another area change is looked for.
    quiet: u32,
    /// The pipeline counters when the window began.
    window_pipes: PipelineCounts,
}

fn minus(a: PipelineCounts, b: PipelineCounts) -> PipelineCounts {
    PipelineCounts {
        made_here: a.made_here - b.made_here,
        made_here_ms: a.made_here_ms - b.made_here_ms,
        made_by_workers: a.made_by_workers - b.made_by_workers,
        workers_ms: a.workers_ms - b.workers_ms,
        queued: a.queued - b.queued,
        draws_waiting: a.draws_waiting - b.draws_waiting,
        found_made: a.found_made - b.found_made,
    }
}

fn plus(a: PipelineCounts, b: PipelineCounts) -> PipelineCounts {
    PipelineCounts {
        made_here: a.made_here + b.made_here,
        made_here_ms: a.made_here_ms + b.made_here_ms,
        made_by_workers: a.made_by_workers + b.made_by_workers,
        workers_ms: a.workers_ms + b.workers_ms,
        queued: a.queued + b.queued,
        draws_waiting: a.draws_waiting + b.draws_waiting,
        found_made: a.found_made + b.found_made,
    }
}

/// The pipelines line of a stretch of frames.
pub(crate) fn pipelines_text(p: &PipelineCounts) -> String {
    format!(
        "{} made with the draw waiting ({:.1} ms), {} handed to workers, {} made by workers ({:.1} ms on their threads), {} draws \
         not drawn while theirs was made, {} found made",
        p.made_here, p.made_here_ms, p.queued, p.made_by_workers, p.workers_ms, p.draws_waiting, p.found_made
    )
}

impl Perf {
    pub(crate) fn from_env() -> Perf {
        Perf {
            on: crate::draw::Lap::enabled() || std::env::var_os("MH3U_FPS").is_some(),
            ..Perf::idle()
        }
    }

    /// Off, measuring nothing.
    pub(crate) fn idle() -> Perf {
        Perf {
            on: false,
            since: Instant::now(),
            last_swap: None,
            window: Window::default(),
            area: None,
            last_pipes: PipelineCounts::default(),
            last_work: Work::default(),
            quiet: 0,
            window_pipes: PipelineCounts::default(),
        }
    }

    /// A `GX2Invalidate` that took `time`; `examined`: front cache index entries it looked at when it reached the shader caches.
    pub(crate) fn invalidate(&mut self, time: Duration, examined: Option<usize>) {
        self.window.invalidates += 1;
        self.window.invalidate_time += time;
        if let Some(n) = examined {
            self.window.shader_invalidates += 1;
            self.window.examined += n as u64;
        }
    }

    /// A swap: the frame that ended (`frame`, counted from 0), the pipeline and cache counters now, and a report of the caches' sizes
    /// (asked for only when a window ends). Returns the lines to print.
    pub(crate) fn swap(&mut self, frame: u32, pipes: PipelineCounts, work: Work, sizes: impl FnOnce() -> String) -> Vec<String> {
        let mut out = Vec::new();
        if !self.on {
            return out;
        }
        let now = Instant::now();
        let ms = self.last_swap.map_or(0.0, |t| now.duration_since(t).as_secs_f64() * 1000.0);
        self.last_swap = Some(now);
        let w = &mut self.window;
        w.frames += 1;
        if ms > w.slowest.0 {
            w.slowest = (ms, frame);
        }
        w.over_33 += u32::from(ms > 33.4);
        w.over_50 += u32::from(ms > 50.0);
        let this_pipes = minus(pipes, self.last_pipes);
        let this_work = Work {
            translated: work.translated - self.last_work.translated,
            decoded: work.decoded - self.last_work.decoded,
        };
        self.last_pipes = pipes;
        self.last_work = work;
        let new = this_pipes.queued + this_pipes.made_here + this_work.translated;
        if let Some(a) = &mut self.area {
            a.frames += 1;
            a.total_ms += ms;
            if ms > a.slowest.0 {
                a.slowest = (ms, frame);
            }
            a.over_50 += u32::from(ms > 50.0);
            a.pipes = plus(a.pipes, this_pipes);
            a.work.translated += this_work.translated;
            a.work.decoded += this_work.decoded;
            a.left -= 1;
            if a.left == 0 {
                out.push(format!(
                    "[gfx] area change at frame {}, the {} frames after it: slowest {:.1} ms (frame {}), mean {:.1} ms, {} over 50 ms; \
                     pipelines: {}; {} shaders translated, {} textures decoded",
                    a.frame,
                    a.frames,
                    a.slowest.0,
                    a.slowest.1,
                    a.total_ms / f64::from(a.frames.max(1)),
                    a.over_50,
                    pipelines_text(&a.pipes),
                    a.work.translated,
                    a.work.decoded
                ));
                self.area = None;
                self.quiet = 60;
            }
        } else if self.quiet > 0 {
            self.quiet -= 1;
        } else if new >= AREA_WORK {
            out.push(format!(
                "[gfx] area change at frame {frame}: {} new pipelines, {} shaders translated, {} textures decoded in it ({ms:.1} ms)",
                this_pipes.queued + this_pipes.made_here,
                this_work.translated,
                this_work.decoded
            ));
            self.area = Some(Area {
                frame,
                left: AREA_FRAMES,
                frames: 0,
                total_ms: 0.0,
                slowest: (0.0, frame),
                over_50: 0,
                pipes: PipelineCounts::default(),
                work: Work::default(),
            });
        }
        let wall = now.duration_since(self.since);
        if wall >= mh3u_rt::times_window() {
            let w = std::mem::take(&mut self.window);
            let secs = wall.as_secs_f64();
            out.push(format!(
                "[gfx frames] {secs:.1} s, {} frames: slowest {:.1} ms (frame {}), {} over 33 ms, {} over 50 ms",
                w.frames, w.slowest.0, w.slowest.1, w.over_33, w.over_50
            ));
            out.push(format!("[gfx caches] {}", sizes()));
            out.push(Perf::window_pipelines(pipes, self.window_pipes));
            self.window_pipes = pipes;
            let per = f64::from(w.frames.max(1));
            out.push(format!(
                "[gfx invalidates] {} ({:.0} a frame), {} reached the shader caches ({:.1} index entries looked at each), {:.2} ms a frame",
                w.invalidates,
                w.invalidates as f64 / per,
                w.shader_invalidates,
                w.examined as f64 / w.shader_invalidates.max(1) as f64,
                w.invalidate_time.as_secs_f64() * 1000.0 / per
            ));
            self.since = now;
        }
        out
    }

    /// The pipelines line of a window: what was made since `then`.
    pub(crate) fn window_pipelines(now: PipelineCounts, then: PipelineCounts) -> String {
        format!("[gfx pipelines] {}", pipelines_text(&minus(now, then)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(queued: u64) -> PipelineCounts {
        PipelineCounts {
            queued,
            ..Default::default()
        }
    }

    #[test]
    fn a_frame_of_new_pipelines_marks_an_area_change_and_the_frames_after_it_are_summed() {
        let mut p = Perf::from_env();
        p.on = true;
        let mut queued = 0;
        let mut lines = Vec::new();
        for frame in 0..(AREA_FRAMES + 10) {
            // frame 5 makes 20 pipelines, the frames after it two each for a while
            queued += match frame {
                5 => 20,
                6..=20 => 2,
                _ => 0,
            };
            lines.extend(p.swap(frame, counts(queued), Work::default(), String::new));
        }
        let marks: Vec<&String> = lines.iter().filter(|l| l.starts_with("[gfx] area change")).collect();
        assert_eq!(marks.len(), 2, "{lines:?}");
        assert!(marks[0].contains("at frame 5: 20 new pipelines"), "{}", marks[0]);
        assert!(marks[1].contains(&format!("the {AREA_FRAMES} frames after it")), "{}", marks[1]);
        assert!(marks[1].contains("30 handed to workers"), "{}", marks[1]);
    }

    #[test]
    fn quiet_frames_mark_nothing() {
        let mut p = Perf::from_env();
        p.on = true;
        for frame in 0..100 {
            let lines = p.swap(frame, counts(u64::from(frame)), Work::default(), String::new);
            assert!(lines.iter().all(|l| !l.starts_with("[gfx] area change")), "{lines:?}");
        }
    }
}
