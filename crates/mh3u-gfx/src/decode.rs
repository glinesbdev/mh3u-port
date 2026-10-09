//! Decoding first-seen textures, vertex streams and geometry pipelines on worker threads (`MH3U_GFX_ASYNC_DECODE=off` keeps them on
//! the render thread).
//!
//! A worker never reads guest memory: the render thread copies what a decode reads into a [`Snapshot`] (a copy is a memcpy, the
//! decode is not), and the worker decodes from that. A draw whose texture, streams or geometry pipeline is still being made is not
//! drawn that frame, as a draw whose render pipeline is being made is not; a texture being decoded again after the game changed it
//! is drawn with the one it replaces meanwhile.

use mh3u_rt::Guest;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// What a decoder reads guest bytes from.
pub(crate) trait Memory {
    fn read(&self, at: u32, out: &mut [u8]);
}

impl Memory for Guest {
    fn read(&self, at: u32, out: &mut [u8]) {
        Guest::read(self, at, out);
    }
}

/// Copies of ranges of guest memory, taken on the render thread for a decode elsewhere. A read outside them gives zeros (and is
/// counted: the ranges are made to cover everything the decode reads).
pub(crate) struct Snapshot {
    parts: Vec<(u32, Vec<u8>)>,
    outside: AtomicU64,
}

impl Snapshot {
    /// The bytes of each `(address, length)` range (kept below 4 GiB).
    pub(crate) fn take(g: &Guest, ranges: &[(u32, usize)]) -> Snapshot {
        let parts = ranges
            .iter()
            .filter(|r| r.1 > 0)
            .map(|&(at, len)| {
                let len = len.min(((1u64 << 32) - u64::from(at)) as usize);
                let mut bytes = vec![0u8; len];
                g.read(at, &mut bytes);
                (at, bytes)
            })
            .collect();
        Snapshot {
            parts,
            outside: AtomicU64::new(0),
        }
    }

    /// The `len` bytes at `at`, when one range holds them all.
    pub(crate) fn get(&self, at: u32, len: usize) -> Option<&[u8]> {
        self.parts.iter().find_map(|(base, bytes)| {
            let from = at.checked_sub(*base)? as usize;
            bytes.get(from..from.checked_add(len)?)
        })
    }

    /// How many reads fell outside the ranges.
    pub(crate) fn outside(&self) -> u64 {
        self.outside.load(Ordering::Relaxed)
    }
}

impl Memory for Snapshot {
    fn read(&self, at: u32, out: &mut [u8]) {
        match self.get(at, out.len()) {
            Some(b) => out.copy_from_slice(b),
            None => {
                out.fill(0);
                self.outside.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// How many threads one big texture is decoded on (its rows split between them).
pub(crate) fn texture_threads() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 8))
}

type Job = Box<dyn FnOnce() -> Done + Send>;

/// A finished decode, handed back to the render thread (see `Gx2::collect_decodes`).
pub(crate) enum Done {
    Texture(crate::draw::TextureDone),
    Streams(crate::draw::StreamsDone),
    Geometry(crate::geometry::GeometryDone),
    /// A job that panicked (counted, so that waiting for every result ends).
    Lost,
}

/// Runs `f`, turning a panic into an error (the decode and wgpu's validation run on workers too).
pub(crate) fn caught<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err("it panicked".into()))
}

/// The decode workers: jobs go in, results come back when the render thread collects them (once a frame).
pub(crate) struct Pool {
    jobs: Sender<Job>,
    done: Receiver<Done>,
    /// Jobs handed out and not collected yet.
    pub outstanding: usize,
}

impl Pool {
    pub(crate) fn new() -> Pool {
        let (jobs, rx) = channel::<Job>();
        let (tx, done) = channel();
        let rx = Arc::new(Mutex::new(rx));
        let n = std::thread::available_parallelism().map_or(2, |n| n.get() / 2).clamp(1, 4);
        for k in 0..n {
            let (rx, tx) = (rx.clone(), tx.clone());
            let _ = std::thread::Builder::new().name(format!("mh3u decode {k}")).spawn(move || {
                loop {
                    let job = match rx.lock() {
                        Ok(r) => r.recv(),
                        Err(_) => return,
                    };
                    let Ok(job) = job else { return };
                    let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).unwrap_or(Done::Lost);
                    if tx.send(done).is_err() {
                        return;
                    }
                }
            });
        }
        Pool {
            jobs,
            done,
            outstanding: 0,
        }
    }

    pub(crate) fn submit(&mut self, job: impl FnOnce() -> Done + Send + 'static) {
        if self.jobs.send(Box::new(job)).is_ok() {
            self.outstanding += 1;
        }
    }

    /// The results ready now (or, with `wait`, every result still to come).
    pub(crate) fn finished(&mut self, wait: bool) -> Vec<Done> {
        let mut out = Vec::new();
        while self.outstanding > 0 {
            let next = if wait { self.done.recv().ok() } else { self.done.try_recv().ok() };
            let Some(d) = next else { break };
            self.outstanding -= 1;
            out.push(d);
        }
        out
    }
}

/// Whether decodes may go to the workers: unless `MH3U_GFX_ASYNC_DECODE` is `off` (or `0`).
pub(crate) fn async_decode(value: Option<&str>) -> bool {
    !matches!(value.map(str::trim), Some("off" | "0"))
}

/// How long first-seen decodes may take on the render thread in one frame before the rest go to the workers
/// (`MH3U_GFX_DECODE_BUDGET_MS`, 2 by default). Vertex data the game rewrites every frame stays well inside it, so it is never left
/// out; an area's first frames are not.
pub(crate) fn decode_budget(value: Option<&str>) -> std::time::Duration {
    let ms = value
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(2.0);
    std::time::Duration::from_secs_f64(ms / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_reads_what_it_copied_and_zeros_elsewhere() {
        let mut m = vec![0u8; 0x1_0000];
        let g = Guest::new(m.as_mut_ptr());
        g.write(0x100, &[1, 2, 3, 4, 5, 6, 7, 8]);
        g.write(0x900, &[9, 9]);
        let s = Snapshot::take(&g, &[(0x100, 8), (0x900, 2), (0x50, 0)]);
        // later writes do not reach it
        g.write(0x100, &[0xff; 8]);
        let mut out = [0u8; 4];
        s.read(0x102, &mut out);
        assert_eq!(out, [3, 4, 5, 6]);
        assert_eq!(s.get(0x900, 2), Some(&[9u8, 9][..]));
        // across the end of a range, or outside every range: zeros, counted
        s.read(0x106, &mut out);
        assert_eq!(out, [0; 4]);
        s.read(0x2000, &mut out);
        assert_eq!(s.outside(), 2);
    }

    #[test]
    fn the_settings() {
        assert!(async_decode(None) && async_decode(Some("on")) && !async_decode(Some("off")) && !async_decode(Some(" 0")));
        let ms = std::time::Duration::from_millis;
        assert_eq!(decode_budget(None), ms(2));
        assert_eq!(decode_budget(Some("5")), ms(5));
        assert_eq!(decode_budget(Some("0")), ms(0));
        assert_eq!(decode_budget(Some("junk")), ms(2));
    }

    #[test]
    fn the_pool_hands_back_every_result() {
        let mut p = Pool::new();
        for k in 0..20u32 {
            p.submit(move || {
                Done::Streams(crate::draw::StreamsDone {
                    key: (u64::from(k), 0),
                    data: vec![k as u8],
                    meta: Vec::new(),
                })
            });
        }
        let done = p.finished(true);
        assert_eq!((done.len(), p.outstanding), (20, 0));
    }
}
