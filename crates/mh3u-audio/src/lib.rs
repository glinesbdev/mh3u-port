//! The Wii U's audio system (`AX`, in `snd_core`) for this host: the game's voices are decoded (DSP-ADPCM, 16-bit and 8-bit PCM),
//! resampled at their pitch, given their volumes and mixed into 3 ms frames at the renderer's rate (32 or 48 kHz), the way the
//! console's DSP does it. The frames go to the host's sound device through `cpal` (WASAPI on Windows, ALSA on Linux, which reaches
//! PulseAudio and PipeWire too).
//!
//! The struct layouts and constants are facts about the console's interface (as documented by the decaf-emu sources); no code is
//! taken from anywhere. The runtime (`mh3u-rt`) owns an [`Ax`], feeds it the game's `AXSetVoice*` calls, and mixes a frame after
//! the game's frame callbacks have run.

pub mod adpcm;
pub mod mixer;
pub mod output;
pub mod voice;

pub use mixer::Ax;
pub use voice::Voice;

/// Where voices read their samples: the guest's memory.
pub trait Memory {
    fn read(&self, at: u32, out: &mut [u8]);
}

/// Whether sound goes to the host's device: unless `MH3U_AUDIO` is `off` (or `0`). The voices are mixed either way, so the game sees
/// them play and end as it would.
pub fn audio_on(value: Option<&str>) -> bool {
    !matches!(value.map(str::trim), Some("off" | "0"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting() {
        assert!(audio_on(None));
        assert!(audio_on(Some("on")));
        assert!(!audio_on(Some("off")));
        assert!(!audio_on(Some(" 0 ")));
    }
}
