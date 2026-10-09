//! Plays two seconds of a 440 Hz tone through the same path as the game's sound: a looping 16-bit voice, mixed into 3 ms frames at
//! 32 kHz, resampled and queued for the default device, paced by the device. `cargo run -p mh3u-audio --example tone`.

use mh3u_audio::output::{Output, Pacer};
use mh3u_audio::voice::{PCM16, Volume};
use mh3u_audio::{Ax, Memory};
use std::time::{Duration, Instant};

struct Wave(Vec<u8>);

impl Memory for Wave {
    fn read(&self, at: u32, out: &mut [u8]) {
        for (k, o) in out.iter_mut().enumerate() {
            *o = self.0.get(at as usize + k).copied().unwrap_or(0);
        }
    }
}

fn main() {
    // one period of a sine at 32 kHz / 440 Hz is not whole: 32 periods of it over 2327 samples are close enough for a test
    let samples = 2327u32;
    let mut bytes = Vec::new();
    for k in 0..samples {
        let s = (f64::from(k) / f64::from(samples) * 32.0 * std::f64::consts::TAU).sin() * 12000.0;
        bytes.extend((s as i16).to_be_bytes());
    }
    let wave = Wave(bytes);
    let mut ax = Ax::default();
    let v = ax.voice(1);
    v.set_offsets(PCM16, true, 0, samples - 1, 0, 0);
    v.ve = Volume { volume: 0x8000, delta: 0 };
    v.running = true;
    let mut out = match Output::open(ax.rate) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("no sound: {e}");
            std::process::exit(1);
        }
    };
    println!("playing on {} ({} Hz, {} channels)", out.name, out.rate, out.channels);
    let start = Instant::now();
    let mut pacer = Pacer::new(start);
    while start.elapsed() < Duration::from_secs(2) {
        out.push(&ax.mix_frame(&wave).0);
        std::thread::sleep(pacer.wait(Instant::now(), Some(out.queued())));
        // (the runtime runs frames back to back while the wait is zero; here a zero sleep does the same)
    }
    std::thread::sleep(out.queued());
    println!(
        "{} frames, {} times the device found nothing queued",
        ax.frames,
        out.queue.underruns.load(std::sync::atomic::Ordering::Relaxed)
    );
}
