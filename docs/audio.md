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

## Run 23 (PR #9, merged in 757ca77): first sound

- **It works.** Played on `Speakers (Realtek(R) Audio)` at 48,000 Hz, stereo. The owner heard it as correct and clean, with no
  popping: sound effects tied to animations, NPCs, ambient music and menu sounds. No input or graphics lag; Moga Woods 82–86 FPS.
- **`snd_core`: 37 imported, 15 done.** Not done: `AXComputeLpfCoefs AXGetAuxCallback AXGetDRCVSMode AXQuit AXRegisterAuxCallback
  AXRegisterDeviceFinalMixCallback AXSetDRCVSDownmixBalance AXSetDRCVSLC AXSetDRCVSMode AXSetDRCVSOutputGain AXSetDRCVSSpeakerPosition
  AXSetDRCVSSurroundDepth AXSetDRCVSSurroundLevelGain AXSetDefaultMixerSelect AXSetVoiceLpf AXSetVoiceLpfCoefs AXSetVoiceMixerSelect
  AXSetVoiceSrcType AXSetVoiceType AXUserBegin AXUserEnd AXUserIsProtected`.
  - The audible ones: the voice low-pass filter (`AXSetVoiceLpf*`, `AXComputeLpfCoefs`), used for muffled and distant sounds, and the
    auxiliary buses' callbacks (`AXRegisterAuxCallback`), where the effects run.
  - The DRC VS functions set up the GamePad's virtual surround, and the rest is bookkeeping.
- **`snd_user`: 37 imported, 0 done, all of them `AXFX` reverbs** (no `MIX`, chorus or delay): `MultiChReverb` (`Init`,
  `SettingsUpdate`, `Shutdown`, `Callback`), and `ReverbHi`, `ReverbHiExp`, `ReverbStd`, `ReverbStdExp`, each with `Init`,
  `Settings`, `Shutdown` and `Callback` in a plain and a `Dpl2` (Dolby Pro Logic II, four-channel) version, plus `AXFXSetHooks`.
  Since the game does not use `MIX`, its voices get their mixes from `AXSetVoiceDeviceMix` directly, so the "unmixed voices are
  centred" fallback is not what the game is heard through.
- **What is missing** is therefore the reverb (caves and big areas sound dry) and the low-pass filter (sounds meant to be muffled,
  such as underwater, are not).

## Effects (PR #10): the low-pass filter, the auxiliary buses, the reverbs

`MH3U_AUDIO_FX=off` turns the filter off and makes every reverb silent (the dry sound is unchanged), to compare.

- **The voices' low-pass filter.** `AXSetVoiceLpf` takes an `AXPBLPF` (on, last output, `a0`, `b0`; the layout of the Wii's SDK,
  which decaf does not describe), `AXSetVoiceLpfCoefs` the two coefficients. The filter is the hardware's one-pole kind,
  `y = a0·x + b0·y₁` in Q15, after the resampling and before the voice's volume. **Exact in form**: the console's arithmetic
  (rounding, where it saturates) is not known, so it is done in floating point. `AXComputeLpfCoefs(cutoff)` comes from a table on
  the console. Here it is the one-pole filter 3 dB down at the cutoff at the renderer's rate, with `a0 = 1 - b0`. Close, but not
  bit-exact.
- **The auxiliary buses.** The mixer now keeps the TV's six channels on the main bus and on buses A, B and C, as the voices' device
  mixes give them. The steps of each 3 ms frame:
  1. The audio thread hands each bus whose callback the game registered (`AXRegisterAuxCallback`, read back with `AXGetAuxCallback`)
     to that callback, as the hardware does (facts from decaf): a pointer to six channels of 32-bit samples of 16-bit range, the user
     data, and `{channels, samples}`. The guest lock is released for the call, as for the frame callbacks.
  2. What comes back is mixed into the main bus at the bus's return volume (1.0, or what `AXSetAuxReturnVolume` sets). A bus with no
     callback goes back as it is (as in decaf).
  3. `AXRegisterDeviceFinalMixCallback` is honoured for the TV: the main bus is handed over the same way before the fold-down to
     stereo.

  The GamePad's callbacks are kept but not run, because the GamePad has no output of its own.
- **The `AXFX` reverbs**, all 37 of the game's `snd_user` imports, native in Rust (`mh3u_audio::fx`, `mh3u-rt/src/axfx.rs`).
  - **How they are reached.** The game registers a reverb's `Callback` import as a bus's aux callback, with its reverb structure as
    the user data. The audio thread calls that address like any callback, and the translated code's dispatch table already sends an
    import's address to its handler. So the callback runs natively, with nothing done in the guest. `AXFXSetHooks` (the delay lines'
    allocator) is not needed: the delay lines are the host's.
  - **The settings** are read from the game's structure at `Init` and `Settings`:
    - `ReverbHi`/`ReverbStd`: coloration, mix, time, damping, pre-delay (and crosstalk for Hi).
    - The `Exp` kinds: pre-delay, fused (late) time, coloration, damping, crosstalk, early, fused and output gains.
  - **The offsets are inferred**, not documented. decaf gives each structure's size and one offset (the pre-delay). The rest follow
    from the Wii SDK's field order, which fits those exactly. Every value read is range-checked. If a set is not all finite and in
    range, a default room is used. The first `Init` of each kind logs a line, so the log shows which reverbs the game uses and what
    it set:
    - `[audio fx] HiExp at 0x…: Params { pre_delay: …, time: … }`, the settings read;
    - `… does not look like settings …; a default room`, when the offsets were wrong.
  - **The `Dpl2` kinds** (four channels) use the same offsets, which may be wrong for them; the check catches that.
    **`MultiChReverb`**'s parameters are not known at all: it is a default room on the TV's six channels.
  - **The algorithm is approximate.** Nintendo's is not described anywhere, so this is a classic reverb driven by the same
    parameters:
    1. a pre-delay;
    2. four early reflections (7–23 ms) at `early_gain`;
    3. four damped comb filters (30–44 ms) whose feedback makes the tail fall 60 dB over `time`, with `damping` the low-pass in their
       loop, at `late_gain`;
    4. two allpasses whose coefficient is `coloration`;
    5. `crosstalk` between left and right, and the output gain ("mix" or "outGain").

    Each channel's delays are 2.3% longer than the previous one's, so the channels decorrelate. The output is the reverberation
    alone, as the game expects: its dry sound is on the main bus. So the rooms are the game's, but the timbre is not Nintendo's.
  - **Cost:** three six-channel reverbs, the most the buses can carry, take 73 µs a 3 ms frame in a release build (2.4% of one core
    on the audio thread). In practice it is one or two stereo reverbs.

## Run 24 (PR #10, merged in 2c12dbb): effects

- **The owner: "the sound is amazing".** Footsteps on each kind of ground, water, menus, and reverb in caves. FPS 80+.
- **Every audio import is done:** `snd_core` 37/37, `snd_user` 37/37.
- **Both reverbs the game made fell back to the default room**, so the rooms are this port's, not the game's:
  - `[audio fx] Hi at 0x2e60efd0`: every setting read as 0 (pre-delay, time, coloration, damping, crosstalk and mix). That is no
    real setting, so the check refused it.
  - `[audio fx] MultiCh at 0x2e60f884`: its parameters are not known.
- **Open: where `ReverbHi`'s settings come from.** All zeros looks like the structure read at `Init`, before the game writes its
  settings, rather than wrong offsets: a wrong place would rarely be all zero. To find out:
  1. Read the structure again at each `Settings` call and at the first callbacks (`MH3U_TRACE_AX` logs every `Init`/`Settings`), and
     take the settings once they become plausible.
  2. If they never do, dump the 0x160 bytes to see where the floats are.
  3. `MultiChReverb`'s parameters remain unknown.


- Windows: nothing more (WASAPI is part of Windows).
- Linux: `cpal` needs the ALSA development files: `alsa-lib` on Arch (`sudo pacman -S alsa-lib`), `libasound2-dev` on Debian and
  Ubuntu, `alsa-lib-devel` on Fedora. Sound reaches PulseAudio or PipeWire through ALSA's default device (`pipewire-alsa` or
  `pulseaudio-alsa` on Arch).

## Not done yet (the plan)

1. **The import list** is known (run 23, above).
2. **The low-pass filter and the reverbs**: PR #10 (above). To check in its first run: the `[audio fx]` lines (were the settings read,
   or are the rooms the default?).
3. **The GamePad's speaker.** Its mix is used only for voices with no TV mix. A setting could send it to the same device.
4. **The biquad filter** per voice (`AXSetVoiceBiquad*`), if a later import list shows the game uses it (run 23's does not).
5. **Better resampling** if linear sounds dull at high pitches. `AXSetVoiceSrcType` asks for 4-tap filtering for some voices.
6. **Streams and movies:** music is streamed by the game into looping voices, which should work as it is. The `h264` movies have no
   sound path yet.
