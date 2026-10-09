//! The reverbs of `snd_user`'s `AXFX` library, done natively: the game hands its auxiliary buses to them through
//! `AXRegisterAuxCallback`, and what they write back (the reverberation alone) is mixed into the main bus.
//!
//! The library's own algorithm is not known (no source describes it), so this is a reverb of the classic kind driven by the same
//! parameters: a pre-delay, early reflections, four damped comb filters whose feedback gives the reverb time, two allpass filters
//! whose coefficient is the "coloration", crosstalk between left and right, and the output gains.

/// What the game sets: in seconds, 0 to 1, or gains.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    pub pre_delay: f32,
    /// The time the late reverberation takes to fall by 60 dB.
    pub time: f32,
    /// The allpass filters' coefficient: higher is denser and more coloured.
    pub coloration: f32,
    /// How fast the high frequencies die away: 0 none, 1 most.
    pub damping: f32,
    /// How much of each side goes into the other.
    pub crosstalk: f32,
    pub early_gain: f32,
    pub late_gain: f32,
    /// The level of the whole output ("mix" in the older reverbs, "outGain" in the newer).
    pub out_gain: f32,
}

impl Default for Params {
    /// A medium room, for a reverb whose settings could not be read.
    fn default() -> Params {
        Params {
            pre_delay: 0.02,
            time: 1.8,
            coloration: 0.6,
            damping: 0.4,
            crosstalk: 0.1,
            early_gain: 0.5,
            late_gain: 1.0,
            out_gain: 0.5,
        }
    }
}

impl Params {
    /// Whether these look like real settings (values read at a wrong place are rarely all finite and in range).
    pub fn plausible(&self) -> bool {
        let in_range = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        in_range(self.pre_delay, 0.0, 2.0)
            && in_range(self.time, 0.01, 30.0)
            && in_range(self.coloration, 0.0, 1.0)
            && in_range(self.damping, 0.0, 1.0)
            && in_range(self.crosstalk, 0.0, 1.0)
            && in_range(self.early_gain, 0.0, 4.0)
            && in_range(self.late_gain, 0.0, 4.0)
            && in_range(self.out_gain, 0.0, 4.0)
    }
}

/// The comb filters' and allpasses' delays, in milliseconds (mutually prime-ish lengths, so their echoes do not line up), and the
/// early reflections' taps after the pre-delay.
const COMBS_MS: [f32; 4] = [29.7, 37.1, 41.1, 43.7];
const ALLPASS_MS: [f32; 2] = [5.0, 1.7];
const EARLY_MS: [(f32, f32); 4] = [(7.0, 0.8), (11.3, 0.6), (17.9, 0.45), (23.1, 0.35)];
/// Each channel's delays are a little longer than the one before (stereo width).
const SPREAD: f32 = 0.023;
const MOST_PRE_DELAY: f32 = 1.0;

struct Line {
    buf: Vec<f32>,
    at: usize,
}

impl Line {
    fn new(len: usize) -> Line {
        Line {
            buf: vec![0.0; len.max(1)],
            at: 0,
        }
    }

    /// The sample written `back` samples ago (1 is the last).
    fn tap(&self, back: usize) -> f32 {
        let n = self.buf.len();
        self.buf[(self.at + n - back.min(n)) % n]
    }

    fn push(&mut self, v: f32) {
        self.buf[self.at] = v;
        self.at = (self.at + 1) % self.buf.len();
    }
}

struct Comb {
    line: Line,
    feedback: f32,
    low: f32,
}

struct Allpass {
    line: Line,
}

struct Channel {
    pre: Line,
    pre_len: usize,
    early: Vec<(usize, f32)>,
    combs: Vec<Comb>,
    allpasses: Vec<Allpass>,
}

/// One reverb: its settings and the state of each channel it reverberates.
pub struct Reverb {
    pub params: Params,
    rate: u32,
    channels: Vec<Channel>,
}

impl Reverb {
    pub fn new(params: Params, rate: u32, channels: usize) -> Reverb {
        let mut r = Reverb {
            params,
            rate,
            channels: Vec::new(),
        };
        r.channels = (0..channels).map(|k| r.channel(k)).collect();
        r
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    fn samples(&self, ms: f32, k: usize) -> usize {
        ((ms * (1.0 + SPREAD * k as f32)) * self.rate as f32 / 1000.0).round().max(1.0) as usize
    }

    fn channel(&self, k: usize) -> Channel {
        let p = &self.params;
        let pre_len = (p.pre_delay.clamp(0.0, MOST_PRE_DELAY) * self.rate as f32) as usize;
        let longest_early = self.samples(EARLY_MS[3].0, k);
        Channel {
            pre: Line::new(pre_len + longest_early + 1),
            pre_len,
            early: EARLY_MS.iter().map(|&(ms, g)| (self.samples(ms, k), g)).collect(),
            combs: COMBS_MS
                .iter()
                .map(|&ms| {
                    let len = self.samples(ms, k);
                    Comb {
                        line: Line::new(len),
                        // a fall of 60 dB over `time`
                        feedback: 10f32.powf(-3.0 * len as f32 / self.rate as f32 / p.time.max(0.01)).min(0.98),
                        low: 0.0,
                    }
                })
                .collect(),
            allpasses: ALLPASS_MS
                .iter()
                .map(|&ms| Allpass {
                    line: Line::new(self.samples(ms, k)),
                })
                .collect(),
        }
    }

    /// New settings: the delays are made again (and emptied) only when the pre-delay or the time changed.
    pub fn set(&mut self, params: Params) {
        let remake = params.pre_delay != self.params.pre_delay || params.time != self.params.time;
        self.params = params;
        if remake {
            let n = self.channels.len();
            self.channels = (0..n).map(|k| self.channel(k)).collect();
        }
    }

    /// Reverberates the channels in place: each comes back as its reverberation alone.
    pub fn process(&mut self, channels: &mut [Vec<f32>]) {
        let p = self.params;
        let damp = p.damping.clamp(0.0, 1.0) * 0.9;
        let colour = p.coloration.clamp(0.0, 1.0) * 0.85;
        let n = channels.first().map_or(0, Vec::len);
        for (k, ch) in channels.iter_mut().enumerate().take(self.channels.len()) {
            let c = &mut self.channels[k];
            for s in ch.iter_mut().take(n) {
                c.pre.push(*s);
                let x = c.pre.tap(c.pre_len + 1);
                let early: f32 = c.early.iter().map(|&(d, g)| c.pre.tap(c.pre_len + d) * g).sum();
                let mut late = 0.0;
                for comb in &mut c.combs {
                    let y = comb.line.tap(comb.line.buf.len());
                    comb.low = y * (1.0 - damp) + comb.low * damp;
                    comb.line.push(x * 0.25 + comb.low * comb.feedback);
                    late += y;
                }
                for a in &mut c.allpasses {
                    let delayed = a.line.tap(a.line.buf.len());
                    let v = late + delayed * colour;
                    a.line.push(v);
                    late = delayed - v * colour;
                }
                *s = (early * p.early_gain + late * p.late_gain) * p.out_gain;
            }
        }
        // crosstalk between the first two channels (left and right)
        if channels.len() >= 2 && p.crosstalk > 0.0 {
            let x = p.crosstalk.clamp(0.0, 1.0) * 0.5;
            let (l, r) = channels.split_at_mut(1);
            for (a, b) in l[0].iter_mut().zip(r[0].iter_mut()) {
                let (na, nb) = (*a * (1.0 - x) + *b * x, *b * (1.0 - x) + *a * x);
                *a = na;
                *b = nb;
            }
        }
    }

    /// Silence: what a reverb gives when effects are off.
    pub fn mute(channels: &mut [Vec<f32>]) {
        for ch in channels {
            ch.fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn impulse(frames: usize, n: usize) -> Vec<Vec<f32>> {
        let mut v = vec![vec![0.0; frames * n]; 2];
        v[0][0] = 10000.0;
        v[1][0] = 10000.0;
        v
    }

    fn energy(v: &[f32]) -> f32 {
        v.iter().map(|s| s * s).sum()
    }

    /// Runs `frames` frames of `n` samples through, frame by frame (as the callbacks do), and gives the output.
    fn run(r: &mut Reverb, input: Vec<Vec<f32>>, n: usize) -> Vec<Vec<f32>> {
        let mut out = vec![Vec::new(); input.len()];
        let frames = input[0].len() / n;
        for f in 0..frames {
            let mut chunk: Vec<Vec<f32>> = input.iter().map(|c| c[f * n..(f + 1) * n].to_vec()).collect();
            r.process(&mut chunk);
            for (o, c) in out.iter_mut().zip(chunk) {
                o.extend(c);
            }
        }
        out
    }

    #[test]
    fn nothing_comes_out_before_the_pre_delay_and_then_it_decays() {
        let p = Params {
            pre_delay: 0.05,
            ..Params::default()
        };
        let mut r = Reverb::new(p, 32_000, 2);
        // 2 s of 3 ms frames
        let out = run(&mut r, impulse(667, 96), 96);
        let before = (0.05 * 32_000.0) as usize;
        assert_eq!(energy(&out[0][..before]), 0.0, "silent during the pre-delay");
        let first = energy(&out[0][before..before + 16_000]);
        let last = energy(&out[0][out[0].len() - 16_000..]);
        assert!(first > 0.0 && last < first * 0.01, "{first} then {last}");
    }

    #[test]
    fn a_longer_time_rings_longer() {
        let tail = |time: f32| {
            let mut r = Reverb::new(Params { time, ..Params::default() }, 48_000, 2);
            let out = run(&mut r, impulse(500, 144), 144);
            energy(&out[0][48_000..])
        };
        assert!(tail(4.0) > tail(1.0) * 4.0);
    }

    #[test]
    fn damping_takes_the_highs_out_of_the_tail() {
        // a high-frequency measure: the energy of the differences between neighbouring samples, against the energy
        let brightness = |damping: f32| {
            let mut r = Reverb::new(
                Params {
                    damping,
                    ..Params::default()
                },
                32_000,
                2,
            );
            let out = run(&mut r, impulse(200, 96), 96);
            let t = &out[0][4000..];
            let d: f32 = t.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();
            d / energy(t)
        };
        assert!(brightness(0.9) < brightness(0.0) * 0.5);
    }

    #[test]
    fn crosstalk_puts_one_side_into_the_other_and_the_gains_scale() {
        let mut input = vec![vec![0.0; 96 * 100]; 2];
        input[0][0] = 10000.0;
        let mut alone = Reverb::new(
            Params {
                crosstalk: 0.0,
                ..Params::default()
            },
            32_000,
            2,
        );
        let out = run(&mut alone, input.clone(), 96);
        assert_eq!(energy(&out[1]), 0.0);
        let mut mixed = Reverb::new(
            Params {
                crosstalk: 1.0,
                ..Params::default()
            },
            32_000,
            2,
        );
        assert!(energy(&run(&mut mixed, input.clone(), 96)[1]) > 0.0);
        let mut quiet = Reverb::new(
            Params {
                out_gain: 0.25,
                crosstalk: 0.0,
                ..Params::default()
            },
            32_000,
            2,
        );
        let q = run(&mut quiet, input, 96);
        let ratio = energy(&q[0]) / energy(&out[0]);
        // 0.25 against the default 0.5: half the amplitude, a quarter of the energy
        assert!((ratio - 0.25).abs() < 1e-3, "{ratio}");
    }

    #[test]
    fn settings_read_from_the_wrong_place_are_not_taken() {
        assert!(Params::default().plausible());
        assert!(
            !Params {
                time: f32::NAN,
                ..Params::default()
            }
            .plausible()
        );
        assert!(
            !Params {
                damping: 3.0e9,
                ..Params::default()
            }
            .plausible()
        );
    }

    #[test]
    fn three_buses_of_six_channels_take_well_under_a_millisecond_a_frame() {
        // what the audio thread does at most each 3 ms frame: three reverbs of six channels at 48 kHz
        let mut reverbs: Vec<Reverb> = (0..3).map(|_| Reverb::new(Params::default(), 48_000, 6)).collect();
        let mut bufs = vec![vec![0.0f32; 144]; 6];
        let frames = 1000;
        let start = std::time::Instant::now();
        for f in 0..frames {
            for r in &mut reverbs {
                for (k, b) in bufs.iter_mut().enumerate() {
                    b.iter_mut().enumerate().for_each(|(i, s)| *s = ((f * 144 + i + k) % 97) as f32);
                }
                r.process(&mut bufs);
            }
        }
        let per_frame = start.elapsed() / frames as u32;
        println!("three six-channel reverbs: {per_frame:?} a frame (of 3 ms)");
        // generous for unoptimised test builds; a release build takes a few tens of microseconds
        assert!(per_frame < std::time::Duration::from_millis(3), "{per_frame:?}");
    }
}
