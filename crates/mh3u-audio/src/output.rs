//! The way to the host's sound device: the mixed frames are resampled from the renderer's rate to the device's and queued, and the
//! device's callback (on a thread of `cpal`'s) takes them from the queue. The guest's audio thread is paced by how much is queued, so
//! the game's frame callbacks run as often as the device plays frames, neither more (a queue that grows) nor less (gaps).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Linear resampling of a stereo stream from one rate to another, keeping its place across frames.
pub struct Resampler {
    from: u32,
    to: u32,
    /// Where the next output sample is, in input samples after `last`.
    pos: f64,
    last: [f32; 2],
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Resampler {
        Resampler {
            from,
            to,
            pos: 0.0,
            last: [0.0; 2],
        }
    }

    pub fn push(&mut self, input: &[[f32; 2]], out: &mut Vec<[f32; 2]>) {
        if self.from == self.to {
            out.extend_from_slice(input);
            return;
        }
        let step = f64::from(self.from) / f64::from(self.to);
        // positions run from the last sample of the previous frame (0) to the last of this one (input.len())
        let len = input.len() as f64;
        while self.pos < len {
            let i = self.pos.floor() as usize;
            let f = (self.pos - i as f64) as f32;
            let a = if i == 0 { self.last } else { input[i - 1] };
            let b = input[i.min(input.len() - 1)];
            out.push([a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f]);
            self.pos += step;
        }
        self.pos -= len;
        if let Some(&l) = input.last() {
            self.last = l;
        }
    }
}

/// What the device plays from: stereo samples at its rate, and how often it found the queue empty.
#[derive(Default)]
pub struct Queue {
    pub samples: Mutex<VecDeque<[f32; 2]>>,
    pub underruns: AtomicU64,
}

/// The device: its rate and the queue its callback reads. The `cpal` stream itself stays on the thread that made it (it may not move
/// between threads on every system), which lives as long as the program.
pub struct Output {
    pub rate: u32,
    pub channels: u16,
    pub name: String,
    pub queue: Arc<Queue>,
    resampler: Resampler,
    scratch: Vec<[f32; 2]>,
}

/// At most this much is kept queued (more is dropped, oldest first: after a stall the sound catches up instead of lagging).
const MOST_QUEUED: Duration = Duration::from_millis(150);

impl Output {
    /// Opens the default output device, for frames at `renderer_rate`.
    pub fn open(renderer_rate: u32) -> Result<Output, String> {
        let queue = Arc::new(Queue::default());
        let (tx, rx) = std::sync::mpsc::channel();
        let q = queue.clone();
        std::thread::Builder::new()
            .name("mh3u audio device".into())
            .spawn(move || match stream(q) {
                Ok((stream, rate, channels, name)) => {
                    let _ = tx.send(Ok((rate, channels, name)));
                    // the stream plays as long as it lives
                    loop {
                        std::thread::park();
                        let _ = &stream;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            })
            .map_err(|e| e.to_string())?;
        let (rate, channels, name) = rx.recv().map_err(|e| e.to_string())??;
        Ok(Output {
            rate,
            channels,
            name,
            queue,
            resampler: Resampler::new(renderer_rate, rate),
            scratch: Vec::new(),
        })
    }

    /// The renderer's rate changed (the game set the audio system up again).
    pub fn set_renderer_rate(&mut self, rate: u32) {
        if self.resampler.from != rate {
            self.resampler = Resampler::new(rate, self.rate);
        }
    }

    /// Queues a mixed frame.
    pub fn push(&mut self, frame: &[[f32; 2]]) {
        self.scratch.clear();
        self.resampler.push(frame, &mut self.scratch);
        let most = (f64::from(self.rate) * MOST_QUEUED.as_secs_f64()) as usize;
        if let Ok(mut q) = self.queue.samples.lock() {
            q.extend(self.scratch.iter().copied());
            let over = q.len().saturating_sub(most);
            q.drain(..over);
        }
    }

    /// How long what is queued lasts.
    pub fn queued(&self) -> Duration {
        let n = self.queue.samples.lock().map_or(0, |q| q.len());
        Duration::from_secs_f64(n as f64 / f64::from(self.rate))
    }
}

fn stream(queue: Arc<Queue>) -> Result<(cpal::Stream, u32, u16, String), String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("no output device")?;
    let name = device.description().map_or_else(|_| "?".into(), |d| d.name().to_string());
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let config = supported.config();
    let (rate, channels) = (config.sample_rate, config.channels);
    let err = |e: cpal::Error| eprintln!("[audio] the device stream failed: {e}");
    let s = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_output_stream(config, fill::<f32>(queue, channels), err, None),
        cpal::SampleFormat::I16 => device.build_output_stream(config, fill::<i16>(queue, channels), err, None),
        cpal::SampleFormat::U16 => device.build_output_stream(config, fill::<u16>(queue, channels), err, None),
        cpal::SampleFormat::I32 => device.build_output_stream(config, fill::<i32>(queue, channels), err, None),
        f => return Err(format!("the device's sample format {f:?}")),
    }
    .map_err(|e| e.to_string())?;
    s.play().map_err(|e| e.to_string())?;
    Ok((s, rate, channels, name))
}

/// The device's callback: left and right into the first two channels (both into a mono device), silence in the others and when the
/// queue runs dry.
fn fill<T: cpal::SizedSample + cpal::FromSample<f32>>(
    queue: Arc<Queue>,
    channels: u16,
) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo) + Send + 'static {
    move |data, _| {
        let channels = usize::from(channels.max(1));
        let Ok(mut q) = queue.samples.lock() else { return };
        let mut dry = false;
        for frame in data.chunks_mut(channels) {
            let s = q.pop_front().unwrap_or_else(|| {
                dry = true;
                [0.0; 2]
            });
            for (c, out) in frame.iter_mut().enumerate() {
                let v = match (channels, c) {
                    (1, _) => (s[0] + s[1]) * 0.5,
                    (_, 0) => s[0],
                    (_, 1) => s[1],
                    _ => 0.0,
                };
                *out = T::from_sample(v);
            }
        }
        if dry {
            queue.underruns.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// When the next audio frame is due: every 3 ms by the clock, but sooner when the device's queue is low and later when it is high,
/// never more than `AHEAD` before the clock (a device that always seems hungry must not run the game's frame callbacks faster than
/// real time), and never in a burst after a stall (the callbacks would run many times at once).
pub struct Pacer {
    due: Instant,
    /// The time the frames are counted from, and the frames since: frame n is due at `origin + n * FRAME` by the clock.
    origin: Instant,
    frames: u32,
}

pub const FRAME: Duration = Duration::from_millis(3);
pub const LOW: Duration = Duration::from_millis(20);
const HIGH: Duration = Duration::from_millis(60);
const TARGET: Duration = Duration::from_millis(40);
const AHEAD: Duration = Duration::from_millis(30);

impl Pacer {
    pub fn new(now: Instant) -> Pacer {
        Pacer {
            due: now,
            origin: now,
            frames: 0,
        }
    }

    /// The clock counts from `due` again (after a stall, or when the device asked for a wait).
    fn anchor(&mut self) {
        self.origin = self.due;
        self.frames = 0;
    }

    /// How long to wait before the next frame, after one was mixed at `now` with `queued` waiting at the device (None: no device).
    pub fn wait(&mut self, now: Instant, queued: Option<Duration>) -> Duration {
        if now > self.due + Duration::from_millis(100) {
            self.due = now;
            self.anchor();
        }
        self.due += FRAME;
        self.frames += 1;
        match queued {
            Some(q) if q < LOW => self.due = self.due.min(now),
            Some(q) if q > HIGH => {
                self.due = self.due.max(now + (q - TARGET));
                self.anchor();
            }
            _ => {}
        }
        let clock = self.origin + FRAME * self.frames;
        if let Some(earliest) = clock.checked_sub(AHEAD) {
            self.due = self.due.max(earliest);
        }
        self.due.saturating_duration_since(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_rate_passes_through() {
        let mut r = Resampler::new(48_000, 48_000);
        let mut out = Vec::new();
        r.push(&[[1.0, 2.0]; 144], &mut out);
        assert_eq!(out.len(), 144);
    }

    #[test]
    fn thirty_two_to_forty_eight_khz_gives_three_samples_for_two_and_stays_continuous() {
        let mut r = Resampler::new(32_000, 48_000);
        let mut out = Vec::new();
        // a ramp over two frames: the output is the same ramp, two thirds of a step apart (the first two start from silence)
        let ramp: Vec<[f32; 2]> = (0..192).map(|i| [i as f32, -(i as f32)]).collect();
        r.push(&ramp[..96], &mut out);
        r.push(&ramp[96..], &mut out);
        assert_eq!(out.len(), 288);
        for (k, s) in out.iter().enumerate().skip(2) {
            let want = k as f32 * 2.0 / 3.0 - 1.0;
            assert!((s[0] - want).abs() < 1e-3, "{k}: {s:?} want {want}");
            assert_eq!(s[1], -s[0]);
        }
    }

    #[test]
    fn forty_four_point_one_khz_keeps_the_rate_over_many_frames() {
        let mut r = Resampler::new(32_000, 44_100);
        let mut out = Vec::new();
        for _ in 0..1000 {
            r.push(&[[0.0; 2]; 96], &mut out);
        }
        // 3 s of input
        assert!((out.len() as i64 - 132_300).abs() <= 1, "{}", out.len());
    }

    #[test]
    fn the_pacer_keeps_three_milliseconds_and_follows_the_queue() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0);
        // by the clock: one frame after the last due time
        assert_eq!(p.wait(t0, None), FRAME);
        assert_eq!(p.wait(t0 + FRAME, None), FRAME);
        // late by a millisecond: the next is sooner, keeping the average
        assert_eq!(p.wait(t0 + FRAME * 2 + Duration::from_millis(1), None), Duration::from_millis(2));
        // a low queue: at once
        assert_eq!(p.wait(t0 + FRAME * 3, Some(Duration::from_millis(5))), Duration::ZERO);
        // a high one: after the excess has played
        let now = t0 + FRAME * 4;
        assert_eq!(p.wait(now, Some(Duration::from_millis(90))), Duration::from_millis(50));
        // after a stall: no burst, one frame from now
        let later = now + Duration::from_secs(1);
        assert_eq!(p.wait(later, Some(TARGET)), FRAME);
    }

    #[test]
    fn a_device_that_is_always_hungry_gets_at_most_thirty_milliseconds_ahead_of_the_clock() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0);
        let at_once = (0..100).take_while(|_| p.wait(t0, Some(Duration::ZERO)).is_zero()).count();
        assert_eq!(at_once, 10, "frames 1 to 10 are due by 30 ms by the clock: up to 30 ms ahead");
        // then one frame every 3 ms
        assert_eq!(p.wait(t0 + FRAME, Some(Duration::ZERO)), Duration::from_millis(3));
    }
}
