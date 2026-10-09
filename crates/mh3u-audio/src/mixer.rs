//! The voices mixed into a frame: each voice's samples go to the TV's channels at its device mix volumes (the main bus; the
//! auxiliary buses feed effects, which are not done yet), folded down to stereo, at the device's volume.

use crate::Memory;
use crate::voice::{BUSES, DRC_CHANNELS, TV_CHANNELS, Voice, Volume};
use std::collections::BTreeMap;

/// How the surround and centre channels are folded into left and right.
const FOLD: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// The auxiliary buses (A, B, C).
pub const AUX_BUSES: usize = 3;

/// A frame being mixed: the TV's six channels on the main bus and on each auxiliary bus, in 16-bit sample units (the hardware's: the
/// buses handed to the game's callbacks are 32-bit integers holding 16-bit samples), and the voices that stopped in it.
pub struct Frame {
    pub main: [Vec<f32>; TV_CHANNELS],
    pub aux: [[Vec<f32>; TV_CHANNELS]; AUX_BUSES],
    pub stopped: Vec<u32>,
}

impl Frame {
    fn new(n: usize) -> Frame {
        Frame {
            main: std::array::from_fn(|_| vec![0.0; n]),
            aux: std::array::from_fn(|_| std::array::from_fn(|_| vec![0.0; n])),
            stopped: Vec::new(),
        }
    }
}

/// The audio system: the renderer's rate, the voices by the address of the game's `AXVoice`, the devices' volumes, and the callbacks
/// the game gave for the auxiliary buses and the final mix.
pub struct Ax {
    /// 32,000 or 48,000 (`AXInitWithParams`; `AXInit` is 32 kHz).
    pub rate: u32,
    pub voices: BTreeMap<u32, Voice>,
    pub tv_volume: u16,
    pub drc_volume: u16,
    /// The callback and its user data of each auxiliary bus of the TV and the GamePad (`AXRegisterAuxCallback`), and the final mix
    /// callback of each device (`AXRegisterDeviceFinalMixCallback`). Only the TV's are run: the GamePad is not a device of its own.
    pub aux_callbacks: [[(u32, u32); AUX_BUSES]; 2],
    pub final_mix: [u32; 3],
    /// How loud each of the TV's auxiliary buses comes back into the main bus (1.15 fixed point).
    pub aux_return: [u16; AUX_BUSES],
    /// Whether effects run: the voices' low-pass filters and the reverbs (`MH3U_AUDIO_FX`).
    pub fx: bool,
    /// Frames mixed, and the counts of what happened (for the report).
    pub frames: u64,
    pub unmixed_frames: u64,
    pub most_voices: usize,
    scratch: Vec<f32>,
}

impl Default for Ax {
    fn default() -> Ax {
        Ax {
            rate: 32_000,
            voices: BTreeMap::new(),
            tv_volume: 0x8000,
            drc_volume: 0x8000,
            aux_callbacks: [[(0, 0); AUX_BUSES]; 2],
            final_mix: [0; 3],
            aux_return: [0x8000; AUX_BUSES],
            fx: true,
            frames: 0,
            unmixed_frames: 0,
            most_voices: 0,
            scratch: Vec::new(),
        }
    }
}

impl Ax {
    /// Samples in a 3 ms frame: 96 at 32 kHz, 144 at 48 kHz.
    pub fn samples_per_frame(&self) -> usize {
        (self.rate * 3 / 1000) as usize
    }

    /// The voice at `at`, made when first named.
    pub fn voice(&mut self, at: u32) -> &mut Voice {
        self.voices.entry(at).or_default()
    }

    /// `AXSetVoiceDeviceMix`: the volumes of a voice on each channel and bus of a device (0 TV, 1 GamePad), read by `volume(channel,
    /// bus)`.
    pub fn set_device_mix(&mut self, at: u32, device: u32, id: u32, volume: impl Fn(usize, usize) -> Volume) {
        let v = self.voice(at);
        match (device, id) {
            (0, 0) => {
                for c in 0..TV_CHANNELS {
                    for b in 0..BUSES {
                        v.tv[c][b] = volume(c, b);
                    }
                }
            }
            (1, 0) => {
                for c in 0..DRC_CHANNELS {
                    for b in 0..BUSES {
                        v.drc[c][b] = volume(c, b);
                    }
                }
            }
            _ => return,
        }
        v.mixed = true;
    }

    /// The voices of one frame onto the TV's buses: every running voice moves on by a frame.
    pub fn mix_voices(&mut self, m: &impl Memory) -> Frame {
        let n = self.samples_per_frame();
        let mut f = Frame::new(n);
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.resize(n, 0.0);
        let mut running = 0;
        let fx = self.fx;
        for (&at, v) in self.voices.iter_mut() {
            if !v.running {
                continue;
            }
            running += 1;
            v.render(m, &mut scratch, fx);
            if !v.running {
                f.stopped.push(at);
            }
            let add = |out: &mut Vec<f32>, gain: f32| {
                if gain > 0.0 {
                    for (o, s) in out.iter_mut().zip(&scratch) {
                        *o += s * gain;
                    }
                }
            };
            let on_tv = v.tv.iter().flatten().any(|b| b.volume > 0);
            if on_tv {
                for c in 0..TV_CHANNELS {
                    add(&mut f.main[c], v.tv[c][0].gain());
                    for bus in 0..AUX_BUSES {
                        add(&mut f.aux[bus][c], v.tv[c][bus + 1].gain());
                    }
                }
            } else if v.drc[0][0].volume > 0 || v.drc[1][0].volume > 0 {
                // a voice only on the GamePad is heard on the TV's left and right
                add(&mut f.main[0], v.drc[0][0].gain());
                add(&mut f.main[1], v.drc[1][0].gain());
            } else if !v.mixed {
                // a voice the game never gave a mix: centred
                self.unmixed_frames += 1;
                add(&mut f.main[0], FOLD);
                add(&mut f.main[1], FOLD);
            }
            // a mix's delta moves its volume on once a frame
            for c in v.tv.iter_mut().chain(v.drc.iter_mut()) {
                for b in c.iter_mut() {
                    b.volume = (i32::from(b.volume) + i32::from(b.delta)).clamp(0, 0xffff) as u16;
                }
            }
        }
        self.scratch = scratch;
        self.most_voices = self.most_voices.max(running);
        f
    }

    /// The auxiliary buses (after their callbacks, if any) back into the main bus at their return volumes.
    pub fn return_aux(&self, f: &mut Frame) {
        for bus in 0..AUX_BUSES {
            let gain = f32::from(self.aux_return[bus]) / 32768.0;
            for c in 0..TV_CHANNELS {
                let (main, aux) = (&mut f.main[c], &f.aux[bus][c]);
                for (o, s) in main.iter_mut().zip(aux) {
                    *o += s * gain;
                }
            }
        }
    }

    /// The main bus folded down to stereo (surrounds and centre at -3 dB into both sides, the sub left out), at the TV's volume,
    /// from -1 to 1.
    pub fn finish(&mut self, f: &Frame) -> Vec<[f32; 2]> {
        let gain = f32::from(self.tv_volume) / 32768.0 / 32768.0;
        let m = &f.main;
        let out = (0..m[0].len())
            .map(|i| {
                let l = m[0][i] + FOLD * (m[2][i] + m[4][i]);
                let r = m[1][i] + FOLD * (m[3][i] + m[4][i]);
                [(l * gain).clamp(-1.0, 1.0), (r * gain).clamp(-1.0, 1.0)]
            })
            .collect();
        self.frames += 1;
        out
    }

    /// A whole frame with no callbacks: the voices, the auxiliary buses returned as they are, the fold-down. Returns the frame and the
    /// voices that stopped in it.
    pub fn mix_frame(&mut self, m: &impl Memory) -> (Vec<[f32; 2]>, Vec<u32>) {
        let mut f = self.mix_voices(m);
        self.return_aux(&mut f);
        let out = self.finish(&f);
        (out, f.stopped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::tests::Mem;
    use crate::voice::{PCM16, Volume};

    #[test]
    fn the_auxiliary_sends_go_to_their_buses_and_come_back_at_their_return_volume() {
        let m = memory();
        let mut ax = Ax::default();
        voice_of(&mut ax, 0x100);
        // nothing on the main bus, all of it to bus B (bus 2) on the left
        ax.set_device_mix(0x100, 0, 0, |c, b| {
            if c == 0 && b == 2 {
                Volume { volume: 0x8000, delta: 0 }
            } else {
                Volume::default()
            }
        });
        let mut f = ax.mix_voices(&m);
        assert_eq!(f.main[0][5], 0.0);
        assert_eq!(f.aux[1][0][5], 16384.0);
        assert_eq!(f.aux[0][0][5], 0.0);
        ax.aux_return[1] = 0x4000;
        ax.return_aux(&mut f);
        assert_eq!(f.main[0][5], 8192.0);
        let out = ax.finish(&f);
        assert_eq!(out[5], [0.25, 0.0]);
    }

    /// A looping voice of one 16-bit sample, the one at `at`.
    fn voice_of(ax: &mut Ax, at: u32) {
        let v = ax.voice(at);
        v.set_offsets(PCM16, true, 0, 0, 0, at);
        v.ve = Volume { volume: 0x8000, delta: 0 };
        v.running = true;
    }

    fn memory() -> Mem {
        // a sample of 16384 at 0x100 and of -8192 at 0x200
        let mut m = vec![0u8; 0x300];
        m[0x100..0x102].copy_from_slice(&16384i16.to_be_bytes());
        m[0x200..0x202].copy_from_slice(&(-8192i16).to_be_bytes());
        Mem(m)
    }

    #[test]
    fn frames_are_three_milliseconds_at_the_renderers_rate() {
        let mut ax = Ax::default();
        assert_eq!(ax.samples_per_frame(), 96);
        ax.rate = 48_000;
        assert_eq!(ax.samples_per_frame(), 144);
        assert_eq!(ax.mix_frame(&memory()).0.len(), 144);
    }

    #[test]
    fn voices_add_up_at_their_mix_volumes() {
        let m = memory();
        let mut ax = Ax::default();
        voice_of(&mut ax, 0x100);
        voice_of(&mut ax, 0x200);
        let unity = Volume { volume: 0x8000, delta: 0 };
        // the first all left, the second half right
        ax.set_device_mix(0x100, 0, 0, |c, b| if c == 0 && b == 0 { unity } else { Volume::default() });
        ax.set_device_mix(0x200, 0, 0, |c, b| {
            if c == 1 && b == 0 {
                Volume { volume: 0x4000, delta: 0 }
            } else {
                Volume::default()
            }
        });
        let (out, stopped) = ax.mix_frame(&m);
        assert!(stopped.is_empty());
        // after the first output sample (0: between nothing and the first sample), the looped one-sample voices hold their value
        assert_eq!(out[5], [0.5, -0.125]);
    }

    #[test]
    fn a_voice_without_a_mix_is_heard_centred_and_one_given_a_silent_mix_is_not() {
        let m = memory();
        let mut ax = Ax::default();
        voice_of(&mut ax, 0x100);
        let (out, _) = ax.mix_frame(&m);
        assert!((out[5][0] - 0.5 * FOLD).abs() < 1e-6 && out[5][0] == out[5][1]);
        assert_eq!(ax.unmixed_frames, 1);
        ax.set_device_mix(0x100, 0, 0, |_, _| Volume::default());
        let (out, _) = ax.mix_frame(&m);
        assert_eq!(out[5], [0.0, 0.0]);
    }

    #[test]
    fn the_surround_and_centre_fold_into_both_sides_and_the_gamepad_is_the_fallback() {
        let m = memory();
        let mut ax = Ax::default();
        voice_of(&mut ax, 0x100);
        let unity = Volume { volume: 0x8000, delta: 0 };
        ax.set_device_mix(0x100, 0, 0, |c, b| if c == 4 && b == 0 { unity } else { Volume::default() });
        let (out, _) = ax.mix_frame(&m);
        assert!((out[5][0] - 0.5 * FOLD).abs() < 1e-6 && out[5][0] == out[5][1]);
        let mut ax = Ax::default();
        voice_of(&mut ax, 0x100);
        ax.set_device_mix(0x100, 1, 0, |c, b| if c == 1 && b == 0 { unity } else { Volume::default() });
        let (out, _) = ax.mix_frame(&m);
        assert_eq!(out[5], [0.0, 0.5]);
    }

    #[test]
    fn a_voice_that_ends_is_reported_once_and_the_loud_is_clamped() {
        let mut m = vec![0u8; 4];
        m[0..2].copy_from_slice(&i16::MAX.to_be_bytes());
        let m = Mem(m);
        let mut ax = Ax::default();
        for at in [0x10, 0x20, 0x30] {
            let v = ax.voice(at);
            v.set_offsets(PCM16, at != 0x10, 0, 0, 0, 0);
            v.ve = Volume { volume: 0xffff, delta: 0 };
            v.running = true;
        }
        let (out, stopped) = ax.mix_frame(&m);
        assert_eq!(stopped, [0x10]);
        assert_eq!(out[5], [1.0, 1.0]);
        assert!(ax.mix_frame(&m).1.is_empty());
        assert_eq!(ax.most_voices, 3);
    }
}
