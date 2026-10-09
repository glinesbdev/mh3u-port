# Audio

How the game makes sound on the Wii U, what the port does with it, and what is left. The interface facts (struct layouts, constants,
offset units) come from the decaf-emu sources, read for facts only: no code is taken from them or from anywhere else.

## How the game makes sound

- The game imports 37 functions from `snd_core` (the audio system, `AX`) and 37 from `snd_user` (helper libraries built on `AX`: the
  `MIX` mixer, the `AXFX` effects such as reverb and chorus, and the `SP` sound tables). Spike 1 (`spikes.md`) has the counts; the
  names are printed by the runtime at start (below) and by `mh3u-tools imports <dump> --names`.
- `AX` plays **voices**. The game takes a voice (`AXAcquireVoice`), points it at samples in memory (`AXSetVoiceOffsets`: DSP-ADPCM,
  16-bit or 8-bit PCM, with a loop), sets its pitch (`AXSetVoiceSrc`, `AXSetVoiceSrcRatio`), its volume (`AXSetVoiceVe`) and how loud
  it is on each channel and bus of the TV and the GamePad (`AXSetVoiceDeviceMix`), and starts it (`AXSetVoiceState`).
- The console's DSP renders **3 ms frames** at the renderer's rate: 32 kHz (96 samples a frame; `AXInit`) or 48 kHz (144;
  `AXInitWithParams`). After each frame `AX` calls the game's **frame callbacks** (`AXRegisterFrameCallback`), where the game's sound
  engine moves its sounds on: it sets new volumes and pitches, starts and stops voices, and refills the buffers of streamed music,
  reading where each voice is (`AXGetVoiceOffsets`) to know what has played.
- Offsets are relative to the voice's data pointer, in the format's own unit: nibbles for ADPCM (16 to a frame of 8 bytes, the first
  two being the frame's header), samples for 16-bit PCM, bytes for 8-bit PCM. The end offset is the last sample played; with the loop
  on, playing goes back to the loop offset, and an ADPCM voice takes up the decoder state given by `AXSetVoiceAdpcmLoop` there.
- Volumes are 1.15 fixed point (0x8000 is 1.0), each with a delta. The TV has six channels (left, right, the two surrounds, centre,
  sub) and the GamePad four, each on four buses: the main one and three auxiliary ones that feed effects.

## What the port does (PR #9, `mh3u-audio`)

- **The mixer** (`crates/mh3u-audio`, no `unsafe`):
  - `adpcm.rs`: DSP-ADPCM decoding (header byte, eight coefficient pairs, two samples of history, rounding to nearest, clamping).
  - `voice.rs`: a voice's offsets (kept absolute, so the `Ex` calls with another base work), reading ADPCM (frame headers read and
    skipped), PCM16 (big-endian) and PCM8 (signed), looping, stopping after the last sample, the pitch (linear between samples, 16.16
    fixed point as `AXVoiceSrc` has it) and the `Ve` volume with its delta.
  - `mixer.rs`: a frame of all running voices. The TV's main bus is folded down to stereo (surrounds and centre at -3 dB into both
    sides, the sub left out); a voice with no TV mix uses its GamePad mix. A voice the game never gave any mix is played centred:
    if the game sets its mixes through `snd_user`'s `MIX` library (not done yet: those calls do nothing), its voices are still heard.
    The TV device volume (`AXSetDeviceVolume`) is applied and the result clamped.
  - `output.rs`: the frames are resampled (linear) from the renderer's rate to the device's and queued for it. The device is opened
    with **`cpal`** (WASAPI on Windows; ALSA on Linux, which reaches PulseAudio and PipeWire through their ALSA plugins), the one
    crate for both systems. Its stream lives on a thread of its own; its callback takes from the queue and plays silence when it is
    empty (counted).
- **The runtime** (`mh3u-rt`): the `AX` calls in `cafe.rs` feed the mixer, and `AXInit`, `AXInitWithParams` and the frame callbacks
  are in `sync.rs`. The **audio thread** (`game::spawn_audio`, a guest thread under the baton like the others) is started by
  `AXInit*` or the first frame callback. Each frame it runs the game's frame callbacks, mixes the voices, marks the voices that ended
  as stopped in their `AXVoice` (for a game that reads it), and queues the frame.
- **Timing.** The frames are paced by the device: a frame every 3 ms by the clock, sooner while the device's queue holds less than
  20 ms (up to 8 frames back to back), later while it holds more than 60 ms. They never run more than 30 ms ahead of the clock, so
  the game's sound engine never runs faster than real time, and there is no burst after a stall. At most 150 ms is kept queued.
  Without a device, or with `MH3U_AUDIO=off`, the frames run by the clock. Either way the game sees its voices play and end at the
  rate they would on the console.
- `[audio]` lines at start:
  - which audio imports the game has and which are not done;
  - the device played on, or why there is no sound.

  `MH3U_TRACE_AX=1` prints each voice's offsets and state changes.
- **Tests:** ADPCM decoding vectors (no prediction, scale, each coefficient, rounding, clamping); voices (each format, frame headers,
  looping and the loop context, pitch up and down, the end, `Ve` ramps); the mixer (volumes, fold-down, the GamePad fallback, unmixed
  voices, clamping, voices that end); the resampler (32 to 48 kHz continuity, 44.1 kHz rate over 3 s); the pacer (clock, queue low
  and high, stalls, an always-hungry device); and a voice set up, played and read back through the runtime's import handlers.
- `cargo run -p mh3u-audio --example tone` plays two seconds of a 440 Hz tone through the same voice, mixer and output, to check a
  machine's sound without the game.

## Building

- Windows: nothing more (WASAPI is part of Windows).
- Linux: `cpal` needs the ALSA development files: `alsa-lib` on Arch (`sudo pacman -S alsa-lib`), `libasound2-dev` on Debian and
  Ubuntu, `alsa-lib-devel` on Fedora. Sound reaches PulseAudio or PipeWire through ALSA's default device (`pipewire-alsa` or
  `pulseaudio-alsa` on Arch).

## Not done yet (the plan)

1. **The import list.** Run once and read the `[audio] snd_core: … not done:` lines: they say which of the 74 functions are still
   stubs. The likely ones: `AXSetVoiceLpf*`, `AXSetVoiceBiquad*` (filters), `AXSetVoicePriority`, `AXSetVoiceType`, `AXSetVoiceSrcType`
   (only linear resampling is done), `AXRegisterAuxCallback` and `AXSetAuxReturnVolume` (effects), `AXRegisterDeviceFinalMixCallback`.
2. **`snd_user`.** If the game uses `MIX` (input, pan, fader, aux sends computed into device mixes), write it, since its calls do nothing
   now and unmixed voices are only played centred. `AXFX` effects (reverb, chorus, delay) take the auxiliary buses through
   `AXRegisterAuxCallback`: the buses would need mixing and the callbacks running, or the effects done in Rust.
3. **The GamePad's speaker.** Its mix is used only for voices with no TV mix. A setting could send it to the same device.
4. **Filters and effects:** the low-pass and biquad filters per voice, then the effects.
5. **Better resampling** if linear sounds dull at high pitches. `AXSetVoiceSrcType` asks for 4-tap filtering for some voices.
6. **Streams and movies:** music is streamed by the game into looping voices, which should work as it is. The `h264` movies have no
   sound path yet.
