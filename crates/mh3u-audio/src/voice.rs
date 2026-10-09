//! One voice: where its samples are, how it reads them (format, loop), its pitch and its volumes.
//!
//! Offsets the game gives are relative to the voice's data pointer and counted in the format's own unit: nibbles for ADPCM (16 to a
//! frame of 8 bytes, the first two being the header), samples for 16-bit PCM, bytes for 8-bit PCM. Here they are kept absolute (from
//! guest address 0, in the same units), so a data pointer given later (`AXSetVoice*OffsetEx`) changes nothing already set.

use crate::Memory;
use crate::adpcm::Adpcm;

pub const ADPCM: u16 = 0x00;
pub const PCM16: u16 = 0x0a;
pub const PCM8: u16 = 0x19;

/// A volume and how much it changes (1.15 fixed point: 0x8000 is 1.0).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Volume {
    pub volume: u16,
    pub delta: i16,
}

impl Volume {
    pub fn gain(self) -> f32 {
        f32::from(self.volume) / 32768.0
    }

    /// Moves the volume on by `delta` `times`, kept in range.
    fn ramp(&mut self, times: i32) {
        self.volume = (i32::from(self.volume) + i32::from(self.delta) * times).clamp(0, 0xffff) as u16;
    }
}

/// The TV's six channels (left, right, left and right surround, centre, sub) and the GamePad's four, each on four buses (main and
/// the three auxiliary ones for effects).
pub const TV_CHANNELS: usize = 6;
pub const DRC_CHANNELS: usize = 4;
pub const BUSES: usize = 4;

#[derive(Debug, Clone)]
pub struct Voice {
    pub format: u16,
    pub looping: bool,
    /// The data pointer offsets are relative to.
    pub data: u32,
    /// Absolute positions (see the module's note): the loop start, the last sample, and the next sample to read.
    pub loop_at: u64,
    pub end: u64,
    pub current: u64,
    pub running: bool,
    /// The pitch: samples of the voice per sample of the renderer (16.16 fixed point), and the fraction between samples.
    pub ratio: u32,
    pub frac: u32,
    pub adpcm: Adpcm,
    /// What the ADPCM decoder takes up at the loop start: the header byte and the two samples before it.
    pub adpcm_loop: (u16, i16, i16),
    pub ve: Volume,
    pub tv: [[Volume; BUSES]; TV_CHANNELS],
    pub drc: [[Volume; BUSES]; DRC_CHANNELS],
    /// Whether the game ever gave it a device mix (a voice without one is played centred: see `Ax::mix_frame`).
    pub mixed: bool,
    pub loops: u32,
    /// The one-pole low-pass filter (`AXSetVoiceLpf`): on, its coefficients (Q15: y = a0 x + b0 y1) and its last output.
    pub lpf: Option<(u16, u16)>,
    pub lpf_y: f32,
    /// The samples either side of the read position (the output is between them, at `frac`), and whether they were read.
    prev: i16,
    next: i16,
    primed: bool,
    /// The last sample of a voice without a loop has been read.
    done: bool,
}

/// An offset relative to `data`, made absolute in `format`'s unit.
pub fn absolute(format: u16, data: u32, offset: u32) -> u64 {
    match format {
        ADPCM => u64::from(data) * 2 + u64::from(offset),
        PCM16 => u64::from(data / 2) + u64::from(offset),
        _ => u64::from(data) + u64::from(offset),
    }
}

/// An absolute position as an offset relative to `data`.
pub fn relative(format: u16, data: u32, at: u64) -> u32 {
    let base = absolute(format, data, 0);
    at.wrapping_sub(base) as u32
}

/// A voice as `AXAcquireVoice` gives it: stopped, nowhere, at pitch 1, silent.
impl Default for Voice {
    fn default() -> Voice {
        Voice {
            format: 0,
            looping: false,
            data: 0,
            loop_at: 0,
            end: 0,
            current: 0,
            running: false,
            ratio: 1 << 16,
            frac: 0,
            adpcm: Adpcm::default(),
            adpcm_loop: (0, 0, 0),
            ve: Volume::default(),
            tv: [[Volume::default(); BUSES]; TV_CHANNELS],
            drc: [[Volume::default(); BUSES]; DRC_CHANNELS],
            mixed: false,
            loops: 0,
            lpf: None,
            lpf_y: 0.0,
            prev: 0,
            next: 0,
            primed: false,
            done: false,
        }
    }
}

impl Voice {
    pub fn new() -> Voice {
        Voice::default()
    }

    /// `AXSetVoiceOffsets`: format, loop flag, and the loop, end and current offsets relative to `data`.
    pub fn set_offsets(&mut self, format: u16, looping: bool, loop_offset: u32, end: u32, current: u32, data: u32) {
        self.format = format;
        self.looping = looping;
        self.data = data;
        self.loop_at = absolute(format, data, loop_offset);
        self.end = absolute(format, data, end);
        self.current = absolute(format, data, current);
        self.restart();
    }

    /// The read position was moved: the samples around it are read again.
    pub fn restart(&mut self) {
        self.primed = false;
        self.done = false;
        self.frac = 0;
    }

    pub fn set_ratio(&mut self, ratio: f64) {
        self.ratio = (ratio.clamp(0.0, 65535.0) * 65536.0) as u32;
    }

    /// The next sample, and the position moved past it (looping or stopping at the end). None once a voice without a loop has
    /// played its last sample.
    fn take(&mut self, m: &impl Memory) -> Option<i16> {
        if self.done {
            return None;
        }
        let at = self.current;
        let s = match self.format {
            ADPCM => {
                let mut at = at;
                if at % 16 < 2 {
                    // a frame's header: the predictor and scale for its 14 samples
                    let mut b = [0u8];
                    m.read((at / 2) as u32 & !7, &mut b);
                    self.adpcm.pred_scale = u16::from(b[0]);
                    at = (at & !15) + 2;
                }
                let mut b = [0u8];
                m.read((at / 2) as u32, &mut b);
                let nibble = if at.is_multiple_of(2) { b[0] >> 4 } else { b[0] & 15 };
                self.current = at;
                self.adpcm.sample(nibble)
            }
            PCM16 => {
                let mut b = [0u8; 2];
                m.read((at * 2) as u32, &mut b);
                i16::from_be_bytes(b)
            }
            _ => {
                let mut b = [0u8];
                m.read(at as u32, &mut b);
                i16::from(b[0] as i8) << 8
            }
        };
        if self.current >= self.end {
            if self.looping {
                self.current = self.loop_at;
                self.loops = self.loops.wrapping_add(1);
                if self.format == ADPCM {
                    let (ps, yn1, yn2) = self.adpcm_loop;
                    self.adpcm.pred_scale = ps;
                    self.adpcm.yn1 = yn1;
                    self.adpcm.yn2 = yn2;
                }
            } else {
                self.done = true;
            }
        } else {
            self.current += 1;
        }
        Some(s)
    }

    /// One frame of the voice: `out.len()` samples at the renderer's rate, at its pitch (linear between samples), through its low-pass
    /// filter (with `fx`) and with its Ve volume. Zeros once it has stopped.
    pub fn render(&mut self, m: &impl Memory, out: &mut [f32], fx: bool) {
        out.fill(0.0);
        if !self.running {
            return;
        }
        if !self.primed {
            self.prev = 0;
            self.next = self.take(m).unwrap_or(0);
            self.primed = true;
        }
        // `ended`: no sample after `next`; `stopped`: `next` was played too, the rest of the frame is silent
        let (mut ended, mut stopped) = (false, false);
        let ve = self.ve;
        for (i, o) in out.iter_mut().enumerate() {
            if stopped {
                break;
            }
            let f = self.frac as f32 / 65536.0;
            let mut s = f32::from(self.prev) + (f32::from(self.next) - f32::from(self.prev)) * f;
            if fx && let Some((a0, b0)) = self.lpf {
                s = (f32::from(a0) * s + f32::from(b0) * self.lpf_y) / 32768.0;
                self.lpf_y = s;
            }
            let gain = (f32::from(ve.volume) + f32::from(ve.delta) * i as f32).clamp(0.0, 65535.0) / 32768.0;
            *o = s * gain;
            self.frac += self.ratio;
            while self.frac >= 1 << 16 {
                self.frac -= 1 << 16;
                if ended {
                    stopped = true;
                    break;
                }
                self.prev = self.next;
                match self.take(m) {
                    Some(s) => self.next = s,
                    None => {
                        self.next = 0;
                        ended = true;
                    }
                }
            }
        }
        self.ve.ramp(out.len() as i32);
        if stopped {
            self.running = false;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Guest memory for the tests: a byte array at address 0.
    pub struct Mem(pub Vec<u8>);

    impl Memory for Mem {
        fn read(&self, at: u32, out: &mut [u8]) {
            for (k, o) in out.iter_mut().enumerate() {
                *o = self.0.get(at as usize + k).copied().unwrap_or(0);
            }
        }
    }

    fn pcm16(samples: &[i16], at: usize) -> Mem {
        let mut m = vec![0u8; at + samples.len() * 2];
        for (k, s) in samples.iter().enumerate() {
            m[at + 2 * k..at + 2 * k + 2].copy_from_slice(&s.to_be_bytes());
        }
        Mem(m)
    }

    fn unity() -> Volume {
        Volume { volume: 0x8000, delta: 0 }
    }

    #[test]
    fn offsets_are_relative_to_the_data_in_the_formats_unit() {
        assert_eq!(absolute(ADPCM, 0x1000, 2), 0x2002);
        assert_eq!(absolute(PCM16, 0x1000, 3), 0x803);
        assert_eq!(absolute(PCM8, 0x1000, 3), 0x1003);
        for f in [ADPCM, PCM16, PCM8] {
            assert_eq!(relative(f, 0x1000, absolute(f, 0x1000, 77)), 77);
        }
    }

    #[test]
    fn pcm16_at_pitch_one_plays_each_sample_once_and_stops_after_the_last() {
        let m = pcm16(&[100, 200, 300, 400], 0x100);
        let mut v = Voice::new();
        v.set_offsets(PCM16, false, 0, 3, 0, 0x100);
        v.ve = unity();
        v.running = true;
        let mut out = [0f32; 8];
        v.render(&m, &mut out, true);
        // the first output sample is between nothing (0) and the first sample, at fraction 0
        assert_eq!(out, [0.0, 100.0, 200.0, 300.0, 400.0, 0.0, 0.0, 0.0]);
        assert!(!v.running);
    }

    #[test]
    fn a_looping_voice_goes_back_to_its_loop_start() {
        let m = pcm16(&[1, 2, 3, 4], 0);
        let mut v = Voice::new();
        v.set_offsets(PCM16, true, 2, 3, 0, 0);
        v.ve = unity();
        v.running = true;
        let mut out = [0f32; 9];
        v.render(&m, &mut out, true);
        assert_eq!(out, [0.0, 1.0, 2.0, 3.0, 4.0, 3.0, 4.0, 3.0, 4.0]);
        assert!(v.running);
        assert_eq!(v.loops, 4);
        assert!(v.current == 2 || v.current == 3);
    }

    #[test]
    fn half_pitch_interpolates_and_double_pitch_skips() {
        let m = pcm16(&[0, 1000, 2000, 3000, 4000, 5000, 6000, 7000], 0);
        let mut v = Voice::new();
        v.set_offsets(PCM16, false, 0, 7, 0, 0);
        v.ve = unity();
        v.running = true;
        v.set_ratio(0.5);
        let mut out = [0f32; 6];
        v.render(&m, &mut out, true);
        assert_eq!(out, [0.0, 0.0, 0.0, 500.0, 1000.0, 1500.0]);
        let mut v2 = Voice::new();
        v2.set_offsets(PCM16, false, 0, 7, 0, 0);
        v2.ve = unity();
        v2.running = true;
        v2.set_ratio(2.0);
        let mut out = [0f32; 4];
        v2.render(&m, &mut out, true);
        assert_eq!(out, [0.0, 1000.0, 3000.0, 5000.0]);
    }

    #[test]
    fn pcm8_is_signed_and_scaled_to_sixteen_bits() {
        let m = Mem(vec![0x7f, 0x80, 0x01]);
        let mut v = Voice::new();
        v.set_offsets(PCM8, false, 0, 2, 0, 0);
        v.ve = unity();
        v.running = true;
        let mut out = [0f32; 4];
        v.render(&m, &mut out, true);
        assert_eq!(out, [0.0, 32512.0, -32768.0, 256.0]);
    }

    #[test]
    fn adpcm_reads_frame_headers_and_skips_them() {
        // two frames: scale 0, nibbles 1..; offsets in nibbles: the first sample is nibble 2
        let mut bytes = vec![0x00, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11];
        bytes.extend([0x01, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22]);
        let m = Mem(bytes);
        let mut v = Voice::new();
        // 28 samples: nibbles 2..15 and 18..31
        v.set_offsets(ADPCM, false, 2, 31, 2, 0);
        v.ve = unity();
        v.running = true;
        let mut out = [0f32; 30];
        v.render(&m, &mut out, true);
        assert!(out[1..15].iter().all(|&s| s == 1.0), "{out:?}");
        // the second frame's header (scale 1): nibble 2 doubled
        assert!(out[15..29].iter().all(|&s| s == 4.0), "{out:?}");
        assert_eq!(out[29], 0.0);
        assert!(!v.running);
    }

    #[test]
    fn an_adpcm_loop_takes_up_the_loop_context() {
        let mut coefs = [0i16; 16];
        coefs[0] = 2048;
        let m = Mem(vec![0x00, 0x10, 0, 0, 0, 0, 0, 0]);
        let mut v = Voice::new();
        // loop over the first two samples: 1, then 1 + 0
        v.set_offsets(ADPCM, true, 2, 3, 2, 0);
        v.adpcm.coefs = coefs;
        v.adpcm_loop = (0x00, 100, 0);
        v.ve = unity();
        v.running = true;
        let mut out = [0f32; 5];
        v.render(&m, &mut out, true);
        // first pass: 1, 1; after the loop the history is the loop context's 100: 101, 101
        assert_eq!(out, [0.0, 1.0, 1.0, 101.0, 101.0]);
    }

    #[test]
    fn the_low_pass_filter_is_one_pole_and_off_without_effects() {
        let m = pcm16(&[10000; 8], 0);
        let make = || {
            let mut v = Voice::new();
            v.set_offsets(PCM16, true, 0, 7, 0, 0);
            v.ve = unity();
            v.running = true;
            // a0 = 0.25, b0 = 0.75
            v.lpf = Some((0x2000, 0x6000));
            v
        };
        let mut v = make();
        let mut out = [0f32; 4];
        v.render(&m, &mut out, true);
        // the step through the filter: 0 (the first output is between nothing and the first sample), then 2500, 4375, 5781.25
        assert_eq!(out, [0.0, 2500.0, 4375.0, 5781.25]);
        let mut v = make();
        v.render(&m, &mut out, false);
        assert_eq!(out, [0.0, 10000.0, 10000.0, 10000.0]);
    }

    #[test]
    fn the_ve_volume_scales_and_ramps_and_keeps_its_last_value() {
        let m = pcm16(&[1000; 16], 0);
        let mut v = Voice::new();
        v.set_offsets(PCM16, true, 0, 15, 0, 0);
        v.running = true;
        v.ve = Volume {
            volume: 0x4000,
            delta: 0x100,
        };
        let mut out = [0f32; 4];
        v.render(&m, &mut out, true);
        assert_eq!(out[1], 1000.0 * (0x4100 as f32 / 32768.0));
        assert_eq!(v.ve.volume, 0x4400);
    }
}
