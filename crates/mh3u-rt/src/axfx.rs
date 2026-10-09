//! `snd_user`'s `AXFX` reverbs, done natively (`mh3u_audio::fx`). The game sets a reverb's parameters in a structure of its own and
//! calls `Init`/`Settings`; it registers the reverb's `Callback` as an auxiliary bus's callback (`AXRegisterAuxCallback`) with the
//! structure as the user data. The audio thread calls that address like any callback, and the translated code's dispatch table sends
//! an import's address to its handler, so the callback lands here with the bus's channel pointers and the structure.
//!
//! Where the parameters are in the structures is not documented for the Wii U. The offsets below follow from the sizes and the one
//! offset each that decaf gives (the pre-delay at 0x158 of `AXFXReverbHi` and 0x114 of `AXFXReverbHiExp`, 0xb8 of
//! `AXFXReverbStdExp`) with the fields in the order of the Wii's SDK. They are checked when read: settings that are not all finite and
//! in range are not taken, a default room is used, and the log says so.

use crate::Ctx;
use crate::cafe::{Os, SimpleHandler};
use crate::guest::Guest;
use mh3u_audio::fx::{Params, Reverb};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Hi,
    HiExp,
    Std,
    StdExp,
    MultiCh,
}

/// A reverb the game made: what kind, and its state.
pub struct Fx {
    pub kind: Kind,
    pub reverb: Reverb,
}

/// The most samples a callback is given (a 3 ms frame at 48 kHz).
const MOST_SAMPLES: u32 = 144;

fn f32_at(g: &Guest, at: u32) -> f32 {
    f32::from_bits(g.u32(at))
}

/// The settings in the game's structure at `at`.
pub fn read_params(g: &Guest, at: u32, kind: Kind) -> Params {
    let d = Params::default();
    let f = |o: u32| f32_at(g, at + o);
    match kind {
        // the older reverbs: coloration, mix, time, damping, pre-delay (and crosstalk) after the working state
        Kind::Hi => Params {
            coloration: f(0x148),
            out_gain: f(0x14c),
            time: f(0x150),
            damping: f(0x154),
            pre_delay: f(0x158),
            crosstalk: f(0x15c),
            ..d
        },
        Kind::Std => Params {
            coloration: f(0xe8),
            out_gain: f(0xec),
            time: f(0xf0),
            damping: f(0xf4),
            pre_delay: f(0xf8),
            crosstalk: 0.0,
            ..d
        },
        // the newer: pre-delay, fused (late) time, coloration, damping, (crosstalk), early and fused gains, the buses, out and send gains
        Kind::HiExp => Params {
            pre_delay: f(0x114),
            time: f(0x11c),
            coloration: f(0x120),
            damping: f(0x124),
            crosstalk: f(0x128),
            early_gain: f(0x12c),
            late_gain: f(0x130),
            out_gain: f(0x13c),
        },
        Kind::StdExp => Params {
            pre_delay: f(0xb8),
            time: f(0xc0),
            coloration: f(0xc4),
            damping: f(0xc8),
            crosstalk: 0.0,
            early_gain: f(0xcc),
            late_gain: f(0xd0),
            out_gain: f(0xdc),
        },
        // the multichannel reverb's parameters are not known at all
        Kind::MultiCh => d,
    }
}

fn name(kind: Kind, dpl2: bool) -> String {
    format!("{kind:?}{}", if dpl2 { "Dpl2" } else { "" })
}

/// `Init` and `Settings`: the settings read (or the default room), and the reverb made or changed.
fn settings(os: &mut Os, g: &Guest, c: &mut Ctx, kind: Kind, dpl2: bool) {
    let at = c.r[3];
    let read = read_params(g, at, kind);
    let params = if read.plausible() { read } else { Params::default() };
    let first = os.fx_logged.insert(name(kind, dpl2));
    if first || std::env::var_os("MH3U_TRACE_AX").is_some() {
        if kind == Kind::MultiCh {
            eprintln!(
                "[audio fx] {} at {at:#x}: its parameters are not known; a default room",
                name(kind, dpl2)
            );
        } else if read.plausible() {
            eprintln!("[audio fx] {} at {at:#x}: {read:?}", name(kind, dpl2));
        } else {
            eprintln!(
                "[audio fx] {} at {at:#x}: what was read does not look like settings ({read:?}); a default room",
                name(kind, dpl2)
            );
        }
    }
    let channels = channels(kind, dpl2);
    let rate = os.ax.rate;
    match os.reverbs.get_mut(&at) {
        Some(fx) if fx.kind == kind => fx.reverb.set(params),
        _ => {
            os.reverbs.insert(
                at,
                Fx {
                    kind,
                    reverb: Reverb::new(params, rate, channels),
                },
            );
        }
    }
    c.r[3] = 1;
}

fn shutdown(os: &mut Os, c: &mut Ctx) {
    os.reverbs.remove(&c.r[3]);
    c.r[3] = 1;
}

/// The channels each kind reverberates: left, right, surround; the Dpl2 ones left, right and the two surrounds; the multichannel one
/// all six of the TV.
fn channels(kind: Kind, dpl2: bool) -> usize {
    match (kind, dpl2) {
        (Kind::MultiCh, _) => 6,
        (_, true) => 4,
        _ => 3,
    }
}

/// `Callback(buffers, reverb[, info])`: the bus's channels reverberated in place.
fn callback(os: &mut Os, g: &Guest, c: &mut Ctx, kind: Kind, dpl2: bool) {
    let (buffers, at) = (c.r[3], c.r[4]);
    let (mut count, mut n) = (channels(kind, dpl2), os.ax.samples_per_frame() as u32);
    if kind == Kind::MultiCh && c.r[5] != 0 {
        count = count.min(g.u32(c.r[5]) as usize);
        n = g.u32(c.r[5] + 4);
    }
    let n = n.min(MOST_SAMPLES);
    let pointers: Vec<u32> = (0..count as u32).map(|k| g.u32(buffers + 4 * k)).collect();
    if buffers == 0 || pointers.contains(&0) {
        return;
    }
    let mut data: Vec<Vec<f32>> = pointers
        .iter()
        .map(|&p| (0..n).map(|i| g.u32(p + 4 * i) as i32 as f32).collect())
        .collect();
    let (rate, fx_on) = (os.ax.rate, os.ax.fx);
    let fx = os.reverbs.entry(at).or_insert_with(|| Fx {
        kind,
        reverb: Reverb::new(Params::default(), rate, channels(kind, dpl2)),
    });
    if fx.reverb.rate() != rate {
        fx.reverb = Reverb::new(fx.reverb.params, rate, channels(kind, dpl2));
    }
    if fx_on {
        fx.reverb.process(&mut data);
    } else {
        Reverb::mute(&mut data);
    }
    for (&p, ch) in pointers.iter().zip(&data) {
        for (i, s) in ch.iter().enumerate() {
            g.put_u32(p + 4 * i as u32, s.round().clamp(-32768.0, 32767.0) as i32 as u32);
        }
    }
}

macro_rules! reverbs {
    ($name:expr, $( $prefix:literal $suffix:literal => $kind:expr, $dpl2:expr; )*) => {
        match $name {
            $(
                concat!("snd_user.rpl:AXFX", $prefix, "Init", $suffix) | concat!("snd_user.rpl:AXFX", $prefix, "Settings", $suffix) => {
                    |os, g, c| settings(os, g, c, $kind, $dpl2)
                }
                concat!("snd_user.rpl:AXFX", $prefix, "Shutdown", $suffix) => |os, _, c| shutdown(os, c),
                concat!("snd_user.rpl:AXFX", $prefix, "Callback", $suffix) => |os, g, c| callback(os, g, c, $kind, $dpl2),
            )*
            "snd_user.rpl:AXFXMultiChReverbInit" | "snd_user.rpl:AXFXMultiChReverbSettingsUpdate"
            | "snd_user.rpl:AXFXMultiChReverbSettingsUpdateNoReset" => |os, g, c| settings(os, g, c, Kind::MultiCh, false),
            "snd_user.rpl:AXFXMultiChReverbShutdown" => |os, _, c| shutdown(os, c),
            "snd_user.rpl:AXFXMultiChReverbCallback" => |os, g, c| callback(os, g, c, Kind::MultiCh, false),
            // the allocator the library would take its delay lines from: these are the host's
            "snd_user.rpl:AXFXSetHooks" => |_, _, _| {},
            _ => return None,
        }
    };
}

pub fn find(name: &str) -> Option<SimpleHandler> {
    Some(reverbs!(name,
        "ReverbHi" "" => Kind::Hi, false;
        "ReverbHi" "Dpl2" => Kind::Hi, true;
        "ReverbHiExp" "" => Kind::HiExp, false;
        "ReverbHiExp" "Dpl2" => Kind::HiExp, true;
        "ReverbStd" "" => Kind::Std, false;
        "ReverbStd" "Dpl2" => Kind::Std, true;
        "ReverbStdExp" "" => Kind::StdExp, false;
        "ReverbStdExp" "Dpl2" => Kind::StdExp, true;
    ))
}
