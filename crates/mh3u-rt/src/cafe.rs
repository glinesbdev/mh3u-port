//! The system functions the game imports, written for this host: memory heaps, the C library, and the plain bookkeeping calls.
//!
//! Each function gets the registers as the PowerPC calling convention gives them (arguments in r3 to r10, the result in r3, a 64-bit
//! result in r3 and r4) and the guest's memory. What a function does is written from what the game passes it and what the console's
//! documented behaviour is; the list of the functions that are still stubs (they do nothing and return 0) is the to-do list.

use crate::Ctx;
use crate::guest::Guest;
use crate::kernel::KernelHandler;
use std::collections::BTreeMap;

pub type SimpleHandler = fn(&mut Os, &Guest, &mut Ctx);

/// A system function: one that only needs the memory, or one that may have to wait.
#[derive(Clone, Copy)]
pub enum Handler {
    Simple(SimpleHandler),
    Kernel(KernelHandler),
    /// Done by the graphics backend, with the number it gave the import (`Gx2Backend::resolve`).
    Gx2(Option<u32>),
}

/// The look-up of an import by its "library:function" name.
pub fn find(name: &str) -> Option<Handler> {
    if let Some(h) = crate::sync::find(name).or_else(|| crate::fs::find(name)) {
        return Some(Handler::Kernel(h));
    }
    simple(name)
        .or_else(|| crate::axfx::find(name))
        .or_else(|| crate::gx2::find(name))
        .map(Handler::Simple)
}

fn simple(name: &str) -> Option<SimpleHandler> {
    Some(match name {
        "coreinit:memset" => |_, g, c| {
            g.fill(c.r[3], c.r[4] as u8, c.r[5]);
        },
        "coreinit:memcpy" | "coreinit:memmove" => |_, g, c| {
            g.copy(c.r[4], c.r[3], c.r[5]);
        },
        "coreinit:memcmp" => |_, g, c| {
            let (mut a, mut b) = (vec![0; c.r[5] as usize], vec![0; c.r[5] as usize]);
            g.read(c.r[3], &mut a);
            g.read(c.r[4], &mut b);
            c.r[3] = match a.cmp(&b) {
                std::cmp::Ordering::Less => u32::MAX,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
        },
        "coreinit:strlen" => |_, g, c| {
            c.r[3] = g.cstr(c.r[3]).len() as u32;
        },
        // the game's own diagnostic messages (printf-style: the integer arguments follow the format in registers)
        "coreinit:OSReport" | "coreinit:OSPanic" | "coreinit:OSFatal" => |_, g, c| {
            let (fmt, mut at) = if c.r[3] > 0x1000 && g.cstr(c.r[3]).contains('%') || true {
                (g.cstr(c.r[3]), 4usize)
            } else {
                (String::new(), 4)
            };
            let mut out = String::new();
            let mut chars = fmt.chars().peekable();
            while let Some(ch) = chars.next() {
                if ch != '%' {
                    out.push(ch);
                    continue;
                }
                while chars.peek().is_some_and(|c| "-+ #0123456789.l".contains(*c)) {
                    chars.next();
                }
                let arg = if at < 11 { c.r[at] } else { 0 };
                match chars.next() {
                    Some('d' | 'i') => out.push_str(&(arg as i32).to_string()),
                    Some('u') => out.push_str(&arg.to_string()),
                    Some('x') => out.push_str(&format!("{arg:x}")),
                    Some('X') => out.push_str(&format!("{arg:X}")),
                    Some('s') => out.push_str(&g.cstr(arg)),
                    Some('c') => out.push(arg as u8 as char),
                    Some('p') => out.push_str(&format!("{arg:#x}")),
                    Some('%') => {
                        out.push('%');
                        continue;
                    }
                    Some(other) => out.push(other),
                    None => break,
                }
                at += 1;
            }
            eprintln!("[game] {}", out.trim_end());
        },
        // buffers for the IPC with the system's services (used by the keyboard library): a bump allocator over the memory it is given
        "coreinit:IPCBufPoolCreate" => |os, g, c| {
            let pool = os.carve(g, 0x20);
            let start = (c.r[3] + 0x3f) & !0x3f;
            let end = c.r[3].saturating_add(c.r[4]);
            os.ipc_pools.insert(pool, (start, end));
            c.r[3] = pool;
        },
        "coreinit:IPCBufPoolAllocate" => |os, _, c| {
            let size = (c.r[4] + 0x3f) & !0x3f;
            c.r[3] = match os.ipc_pools.get_mut(&c.r[3]) {
                Some((next, end)) if next.saturating_add(size) <= *end => {
                    let at = *next;
                    *next += size;
                    at
                }
                _ => 0,
            };
        },
        "coreinit:IPCBufPoolFree" => |_, _, c| c.r[3] = 0,
        // the account: one signed-in user in slot 1
        "nn_act:Initialize__Q2_2nn3actFv" | "nn_act:Finalize__Q2_2nn3actFv" => |_, _, c| c.r[3] = 0,
        "nn_act:GetSlotNo__Q2_2nn3actFv" | "nn_act:GetNumOfAccounts__Q2_2nn3actFv" => |_, _, c| c.r[3] = 1,
        "nn_act:IsSlotOccupied__Q2_2nn3actFUc" => |_, _, c| c.r[3] = 1,
        "nn_act:GetPersistentId__Q2_2nn3actFv" => |_, _, c| c.r[3] = 0x8000_0001,
        "nn_act:HasNfsAccount__Q2_2nn3actFv" => |_, _, c| c.r[3] = 0,
        // there is no network
        "nn_ac:Initialize__Q2_2nn2acFv" | "nn_ac:Finalize__Q2_2nn2acFv" | "nn_ac:Close__Q2_2nn2acFv" => |_, _, c| c.r[3] = 0,
        "nn_ac:IsApplicationConnected__Q2_2nn2acFPb" => |_, g, c| {
            g.fill(c.r[3], 0, 1);
            c.r[3] = 0;
        },
        "nn_ac:Connect__Q2_2nn2acFv" => |_, _, c| c.r[3] = 0xffff_ffff,
        // the audio system (`AX`): the voices' settings go to the mixer (`mh3u-audio`), which the audio thread runs every 3 ms frame
        // (`game::spawn_audio`); `AXInit*` and the frame callbacks are in `sync.rs`
        "snd_core.rpl:AXAcquireVoice" | "snd_core.rpl:AXAcquireVoiceEx" => |os, g, c| {
            let (priority, callback, context) = (c.r[3], c.r[4], c.r[5]);
            let v = match os.free_voices.pop() {
                Some(v) => v,
                None => {
                    let v = os.carve(g, AX_VOICE_SIZE);
                    g.put_u32(v, os.voices);
                    os.voices += 1;
                    v
                }
            };
            // the `AXVoice` the game may read: its index, state, priority, callback and context
            g.put_u32(v + 4, 0);
            g.put_u32(v + 0x1c, priority);
            g.put_u32(v + 0x20, callback);
            g.put_u32(v + 0x24, context);
            os.ax.voices.insert(v, mh3u_audio::Voice::new());
            c.r[3] = v;
        },
        "snd_core.rpl:AXFreeVoice" => |os, _, c| {
            if os.ax.voices.remove(&c.r[3]).is_some() {
                os.free_voices.push(c.r[3]);
            }
            c.r[3] = 0;
        },
        "snd_core.rpl:AXIsInit" => |_, _, c| c.r[3] = 1,
        "snd_core.rpl:AXGetMaxVoices" => |_, _, c| c.r[3] = 96,
        "snd_core.rpl:AXCheckVoiceOffsets" => |_, _, c| c.r[3] = 1,
        "snd_core.rpl:AXGetInputSamplesPerSec" => |os, _, c| c.r[3] = os.ax.rate,
        "snd_core.rpl:AXGetInputSamplesPerFrame" => |os, _, c| c.r[3] = os.ax.samples_per_frame() as u32,
        "snd_core.rpl:AXGetRendererFreq" => |os, _, c| c.r[3] = u32::from(os.ax.rate == 48_000),
        // where a voice's samples are: format, loop, and the loop, end and current offsets relative to its data
        "snd_core.rpl:AXSetVoiceOffsets" => |os, g, c| set_offsets(os, g, c.r[3], c.r[4], g.u32(c.r[4] + 0x10)),
        "snd_core.rpl:AXSetVoiceOffsetsEx" => |os, g, c| set_offsets(os, g, c.r[3], c.r[4], c.r[5]),
        "snd_core.rpl:AXGetVoiceOffsets" => |os, g, c| {
            let data = os.ax.voice(c.r[3]).data;
            get_offsets(os, g, c.r[3], c.r[4], data);
        },
        "snd_core.rpl:AXGetVoiceOffsetsEx" => |os, g, c| get_offsets(os, g, c.r[3], c.r[4], c.r[5]),
        "snd_core.rpl:AXSetVoiceCurrentOffset" => |os, _, c| set_offset(os, c.r[3], c.r[4], None, Which::Current),
        "snd_core.rpl:AXSetVoiceCurrentOffsetEx" => |os, _, c| set_offset(os, c.r[3], c.r[4], Some(c.r[5]), Which::Current),
        "snd_core.rpl:AXSetVoiceLoopOffset" => |os, _, c| set_offset(os, c.r[3], c.r[4], None, Which::Loop),
        "snd_core.rpl:AXSetVoiceLoopOffsetEx" => |os, _, c| set_offset(os, c.r[3], c.r[4], Some(c.r[5]), Which::Loop),
        "snd_core.rpl:AXSetVoiceEndOffset" => |os, _, c| set_offset(os, c.r[3], c.r[4], None, Which::End),
        "snd_core.rpl:AXSetVoiceEndOffsetEx" => |os, _, c| set_offset(os, c.r[3], c.r[4], Some(c.r[5]), Which::End),
        "snd_core.rpl:AXGetVoiceCurrentOffsetEx" => |os, _, c| {
            let v = os.ax.voice(c.r[3]);
            c.r[3] = mh3u_audio::voice::relative(v.format, c.r[4], v.current);
        },
        "snd_core.rpl:AXSetVoiceLoop" => |os, _, c| os.ax.voice(c.r[3]).looping = c.r[4] & 0xffff != 0,
        "snd_core.rpl:AXGetVoiceLoopCount" => |os, _, c| c.r[3] = os.ax.voice(c.r[3]).loops,
        "snd_core.rpl:AXIsVoiceRunning" => |os, _, c| c.r[3] = u32::from(os.ax.voice(c.r[3]).running),
        "snd_core.rpl:AXSetVoiceState" => |os, g, c| {
            let running = c.r[4] != 0;
            os.ax.voice(c.r[3]).running = running;
            g.put_u32(c.r[3] + 4, u32::from(running));
            if std::env::var_os("MH3U_TRACE_AX").is_some() {
                eprintln!(
                    "[ax] voice {:#x} {}: {:?}",
                    c.r[3],
                    if running { "on" } else { "off" },
                    os.ax.voice(c.r[3])
                );
            }
        },
        // the pitch: samples of the voice per sample of the renderer, as a float or (`AXVoiceSrc`) in 16.16 fixed point
        "snd_core.rpl:AXSetVoiceSrcRatio" => |os, _, c| {
            let ratio = f64::from_bits(c.f[1]);
            os.ax.voice(c.r[3]).set_ratio(ratio);
            c.r[3] = if ratio < 0.0 { 0xffff_ffff } else { 0 };
        },
        "snd_core.rpl:AXSetVoiceSrc" => |os, g, c| {
            let v = os.ax.voice(c.r[3]);
            v.ratio = g.u32(c.r[4]);
            v.frac = u32::from(g.u16(c.r[4] + 4));
        },
        // volumes: the voice's own (`Ve`) and its mix on each channel and bus of a device (1.15 fixed point, with a delta)
        "snd_core.rpl:AXSetVoiceVe" => |os, g, c| {
            os.ax.voice(c.r[3]).ve = mh3u_audio::voice::Volume {
                volume: g.u16(c.r[4]),
                delta: g.u16(c.r[4] + 2) as i16,
            };
        },
        "snd_core.rpl:AXSetVoiceVeDelta" => |os, _, c| os.ax.voice(c.r[3]).ve.delta = c.r[4] as u16 as i16,
        "snd_core.rpl:AXSetVoiceDeviceMix" => |os, g, c| {
            let (voice, device, id, mix) = (c.r[3], c.r[4], c.r[5], c.r[6]);
            os.ax.set_device_mix(voice, device, id, |channel, bus| {
                let at = mix + (channel * 16 + bus * 4) as u32;
                mh3u_audio::voice::Volume {
                    volume: g.u16(at),
                    delta: g.u16(at + 2) as i16,
                }
            });
            c.r[3] = 0;
        },
        "snd_core.rpl:AXSetDeviceVolume" => |os, _, c| {
            match c.r[3] {
                0 => os.ax.tv_volume = c.r[5] as u16,
                1 => os.ax.drc_volume = c.r[5] as u16,
                _ => {}
            }
            c.r[3] = 0;
        },
        "snd_core.rpl:AXGetDeviceVolume" => |os, g, c| {
            g.put_u16(c.r[5], if c.r[3] == 1 { os.ax.drc_volume } else { os.ax.tv_volume });
            c.r[3] = 0;
        },
        // the ADPCM decoder's coefficients and state, and what it takes up at the loop start
        "snd_core.rpl:AXSetVoiceAdpcm" => |os, g, c| {
            let at = c.r[4];
            let v = os.ax.voice(c.r[3]);
            for k in 0..16 {
                v.adpcm.coefs[k] = g.u16(at + 2 * k as u32) as i16;
            }
            v.adpcm.pred_scale = g.u16(at + 0x22);
            v.adpcm.yn1 = g.u16(at + 0x24) as i16;
            v.adpcm.yn2 = g.u16(at + 0x26) as i16;
        },
        "snd_core.rpl:AXSetVoiceAdpcmLoop" => |os, g, c| {
            let at = c.r[4];
            os.ax.voice(c.r[3]).adpcm_loop = (g.u16(at), g.u16(at + 2) as i16, g.u16(at + 4) as i16);
        },
        // the voice's one-pole low-pass filter: `AXPBLPF` is on, yn1, a0, b0 (Q15: y = a0 x + b0 y1)
        "snd_core.rpl:AXSetVoiceLpf" => |os, g, c| {
            let at = c.r[4];
            let v = os.ax.voice(c.r[3]);
            v.lpf = (g.u16(at) != 0).then(|| (g.u16(at + 4), g.u16(at + 6)));
            v.lpf_y = f32::from(g.u16(at + 2) as i16);
        },
        "snd_core.rpl:AXSetVoiceLpfCoefs" => |os, _, c| {
            let v = os.ax.voice(c.r[3]);
            v.lpf = Some((c.r[4] as u16, c.r[5] as u16));
        },
        "snd_core.rpl:AXComputeLpfCoefs" => |os, g, c| {
            let (a0, b0) = lpf_coefs(c.r[3], os.ax.rate);
            g.put_u16(c.r[4], a0);
            g.put_u16(c.r[5], b0);
        },
        // the auxiliary buses' callbacks (device type 0 TV, 1 GamePad; device 0; bus A, B, C) and the devices' final mix callbacks
        "snd_core.rpl:AXRegisterAuxCallback" => |os, _, c| {
            let (device, id, bus) = (c.r[3] as usize, c.r[4], c.r[5] as usize);
            c.r[3] = if device < 2 && id == 0 && bus < 3 {
                os.ax.aux_callbacks[device][bus] = (c.r[6], c.r[7]);
                0
            } else {
                // AX_RESULT_INVALID_DEVICE_CONFIG
                (-13i32) as u32
            };
        },
        "snd_core.rpl:AXGetAuxCallback" => |os, g, c| {
            let (device, id, bus) = (c.r[3] as usize, c.r[4], c.r[5] as usize);
            let (cb, user) = if device < 2 && id == 0 && bus < 3 {
                os.ax.aux_callbacks[device][bus]
            } else {
                (0, 0)
            };
            if c.r[6] != 0 {
                g.put_u32(c.r[6], cb);
            }
            if c.r[7] != 0 {
                g.put_u32(c.r[7], user);
            }
            c.r[3] = 0;
        },
        "snd_core.rpl:AXSetAuxReturnVolume" => |os, _, c| {
            if c.r[3] == 0 && c.r[4] == 0 && c.r[5] < 3 {
                os.ax.aux_return[c.r[5] as usize] = c.r[6] as u16;
            }
            c.r[3] = 0;
        },
        "snd_core.rpl:AXRegisterDeviceFinalMixCallback" => |os, _, c| {
            if let Some(slot) = os.ax.final_mix.get_mut(c.r[3] as usize) {
                *slot = c.r[4];
            }
            c.r[3] = 0;
        },
        // bookkeeping the mixer has no use for: the GamePad's virtual surround, the mixer selection, the resampler's kind (linear is
        // the only one), the voice's type, and the "user" protection around voice changes (the mixer runs under the kernel's lock)
        "snd_core.rpl:AXGetDRCVSMode" => |_, g, c| {
            if c.r[3] != 0 {
                g.put_u32(c.r[3], 0);
            }
            c.r[3] = 0;
        },
        "snd_core.rpl:AXSetDRCVSMode"
        | "snd_core.rpl:AXSetDRCVSDownmixBalance"
        | "snd_core.rpl:AXSetDRCVSLC"
        | "snd_core.rpl:AXSetDRCVSOutputGain"
        | "snd_core.rpl:AXSetDRCVSSpeakerPosition"
        | "snd_core.rpl:AXSetDRCVSSurroundDepth"
        | "snd_core.rpl:AXSetDRCVSSurroundLevelGain"
        | "snd_core.rpl:AXSetDefaultMixerSelect"
        | "snd_core.rpl:AXSetVoiceMixerSelect"
        | "snd_core.rpl:AXSetVoiceSrcType"
        | "snd_core.rpl:AXSetVoiceType"
        | "snd_core.rpl:AXUserBegin"
        | "snd_core.rpl:AXUserEnd"
        | "snd_core.rpl:AXUserIsProtected"
        | "snd_core.rpl:AXQuit" => |_, _, c| c.r[3] = 0,
        "snd_core.rpl:AXGetDeviceMode" => |_, g, c| {
            // stereo ("surround" is 2)
            g.put_u32(c.r[4], 1);
            c.r[3] = 0;
        },
        // the GamePad: one sample of what the player presses (see `input`), with the buttons `MH3U_PRESS` holds added
        "vpad.rpl:VPADRead" => |os, g, c| {
            let (buffer, error) = (c.r[4], c.r[6]);
            g.fill(buffer, 0, 0xac);
            let input = &crate::input::INPUT;
            // MH3U_TOUCH_SWEEP=<seconds>: from then on, touch each point of a grid in turn
            if let Some(from) = std::env::var("MH3U_TOUCH_SWEEP").ok().and_then(|v| v.parse::<f64>().ok()) {
                let t = os.started.elapsed().as_secs_f64();
                if t >= from {
                    let slot = ((t - from) / 0.5) as usize;
                    let (w, h) = crate::input::TOUCH_SIZE;
                    let cell = (slot / 2) % 25;
                    input.set_touch(
                        slot.is_multiple_of(2)
                            .then_some(((cell % 5) as f32 * 0.2 * w + 0.1 * w, (cell / 5) as f32 * 0.2 * h + 0.1 * h)),
                    );
                }
            }
            let hold = os.pad_hold() | input.hold();
            // a press is reported once per frame, however many times the game reads in it: reads close together get the same answer
            let now = std::time::Instant::now();
            if now.duration_since(os.pad_read_at) > std::time::Duration::from_millis(4) {
                os.pad_edges = (hold & !os.pad_before, os.pad_before & !hold);
                os.pad_before = hold;
            }
            os.pad_read_at = now;
            if std::env::var_os("MH3U_TRACE_PAD").is_some() {
                eprintln!(
                    "[pad] hold {hold:#x} trigger {:#x} release {:#x} count {:#x}",
                    os.pad_edges.0, os.pad_edges.1, c.r[5]
                );
            }
            g.put_u32(buffer, hold);
            g.put_u32(buffer + 4, os.pad_edges.0);
            g.put_u32(buffer + 8, os.pad_edges.1);
            let sticks = input.sticks();
            for (i, v) in sticks.iter().enumerate() {
                g.put_u32(buffer + 0xc + 4 * i as u32, v.to_bits());
            }
            // the touch panel's raw values run from 0 to 4095 over the screen (`VPADGetTPCalibratedPoint` scales them back)
            let (size_x, size_y) = crate::input::TOUCH_SIZE;
            let (x, y, touched) = match input.touch() {
                Some((x, y)) => ((x / size_x * 4095.0) as u32, (y / size_y * 4095.0) as u32, 1),
                None => (0, 0, 0),
            };
            g.put_u16(buffer + 0x58 - 4, x as u16);
            g.put_u16(buffer + 0x58 - 2, y as u16);
            g.put_u16(buffer + 0x58, touched);
            // (0 means the point is valid)
            g.put_u16(buffer + 0x5a, 0);
            // the filtered points are the same
            g.copy(buffer + 0x54, buffer + 0x5c, 8);
            g.copy(buffer + 0x54, buffer + 0x64, 8);
            if error != 0 {
                g.put_u32(error, 0);
            }
            c.r[3] = 1;
        },
        // (channel, *calibrated, *raw): the raw touch values to screen coordinates
        "vpad.rpl:VPADGetTPCalibratedPoint" => |_, g, c| {
            let (out, raw) = (c.r[4], c.r[5]);
            let (size_x, size_y) = crate::input::TOUCH_SIZE;
            let (x, y) = (f32::from(g.u16(raw)), f32::from(g.u16(raw + 2)));
            g.put_u16(out, (x / 4095.0 * size_x) as u16);
            g.put_u16(out + 2, (y / 4095.0 * size_y) as u16);
            g.put_u16(out + 4, g.u16(raw + 4));
            g.put_u16(out + 6, g.u16(raw + 6));
        },
        // the DMA engine copies memory (the game uses it to fill the GPU's constant buffers); it is finished when the call returns
        "dmae.rpl:DMAECopyMem" => |_, g, c| {
            let (dst, src, words, endian) = (c.r[3], c.r[4], c.r[5], c.r[6]);
            dma_copy(g, dst, src, words, endian);
            c.r[3] = 0;
        },
        "dmae.rpl:DMAEWaitDone" => |_, _, c| c.r[3] = 1,
        "coreinit:OSGetCoreId" | "coreinit:OSGetMainCoreId" => |_, _, c| c.r[3] = 1,
        "coreinit:__ghsLock" | "coreinit:__ghsUnlock" => |_, _, _| {},
        "coreinit:OSIsDebuggerInitialized" => |_, _, c| c.r[3] = 0,
        "coreinit:OSGetTime" | "coreinit:OSGetSystemTime" => |os, _, c| {
            let t = os.ticks();
            c.r[3] = (t >> 32) as u32;
            c.r[4] = t as u32;
        },
        "coreinit:OSGetSystemInfo" => |os, _, c| c.r[3] = os.system_info,
        "coreinit:OSGetMemBound" => |_, g, c| {
            // the type: 1 is MEM1, 2 the application's MEM2
            let (start, size) = if c.r[3] == 1 { (MEM1.0, MEM1.1) } else { (MEM2.0, MEM2.1) };
            if c.r[4] != 0 {
                g.put_u32(c.r[4], start);
            }
            if c.r[5] != 0 {
                g.put_u32(c.r[5], size);
            }
            c.r[3] = 0;
        },
        "coreinit:OSGetForegroundBucketFreeArea" => |_, g, c| {
            if c.r[3] != 0 {
                g.put_u32(c.r[3], FOREGROUND.0);
            }
            if c.r[4] != 0 {
                g.put_u32(c.r[4], FOREGROUND.1);
            }
            c.r[3] = 1;
        },
        // the default heap is the application's MEM2; these are called through variables the loader fills in
        "coreinit:MEMAllocFromDefaultHeapEx" => |os, g, c| {
            let h = os.base_heap(g, 1);
            c.r[3] = os.heap(h).map_or(0, |h| h.alloc(c.r[3], c.r[4] as i32));
        },
        "coreinit:MEMAllocFromDefaultHeap" => |os, g, c| {
            let h = os.base_heap(g, 1);
            c.r[3] = os.heap(h).map_or(0, |h| h.alloc(c.r[3], 4));
        },
        "coreinit:MEMFreeToDefaultHeap" => |os, g, c| {
            let h = os.base_heap(g, 1);
            if let Some(h) = os.heap(h) {
                h.free(c.r[3]);
            }
        },
        // a frame heap takes blocks from either end and frees them all at once, which is what the expanding heap does until a block is freed
        "coreinit:MEMAllocFromFrmHeapEx" => |os, _, c| c.r[3] = os.heap(c.r[3]).map_or(0, |h| h.alloc(c.r[4], c.r[5] as i32)),
        "coreinit:MEMGetAllocatableSizeForFrmHeapEx" => |os, _, c| c.r[3] = os.heap(c.r[3]).map_or(0, |h| h.largest_free(c.r[4] as i32)),
        "coreinit:MEMFreeToFrmHeap" => |os, _, c| {
            if let Some(h) = os.heap(c.r[3]) {
                h.used.clear();
            }
        },
        "coreinit:MEMGetBaseHeapHandle" => |os, g, c| c.r[3] = os.base_heap(g, c.r[3]),
        "coreinit:MEMCreateExpHeapEx" => |os, g, c| c.r[3] = os.create_heap(g, c.r[3], c.r[4]),
        "coreinit:MEMAllocFromExpHeapEx" => |os, _, c| c.r[3] = os.heap(c.r[3]).map_or(0, |h| h.alloc(c.r[4], c.r[5] as i32)),
        "coreinit:MEMFreeToExpHeap" => |os, _, c| {
            if let Some(h) = os.heap(c.r[3]) {
                h.free(c.r[4]);
            }
        },
        "coreinit:MEMGetSizeForMBlockExpHeap" => |os, _, c| {
            let block = c.r[3];
            c.r[3] = os.heaps.values().find_map(|h| h.used.get(&block).copied()).unwrap_or(0);
        },
        "coreinit:MEMGetTotalFreeSizeForExpHeap" => |os, _, c| c.r[3] = os.heap(c.r[3]).map_or(0, |h| h.total_free()),
        "coreinit:MEMGetAllocatableSizeForExpHeapEx" => |os, _, c| c.r[3] = os.heap(c.r[3]).map_or(0, |h| h.largest_free(c.r[4] as i32)),
        _ => return None,
    })
}

/// (start, size) of the memories the game is given.
const MEM1: (u32, u32) = (0xf400_0000, 0x0200_0000);
const MEM2: (u32, u32) = (0x1080_0000, 0x4900_0000);
const FOREGROUND: (u32, u32) = (0xe000_0000, 0x0300_0000);
/// Where the runtime keeps the structures it makes for the game (the thread, the system information, heap headers).
const OS_AREA: u32 = 0x0e00_0000;
/// Where the runtime's structures end: the last MiB below `.rodata` (0x10000000) is the mods' own (`native_text::BUFFER`).
const OS_AREA_END: u32 = crate::native_text::BUFFER;

/// An expanding heap: blocks are taken from the low end, or from the high end for a negative alignment.
pub struct ExpHeap {
    start: u32,
    end: u32,
    /// The blocks in use: address to size.
    used: BTreeMap<u32, u32>,
}

impl ExpHeap {
    fn alloc(&mut self, size: u32, align: i32) -> u32 {
        let from_top = align < 0;
        let align = align.unsigned_abs().max(4);
        if !align.is_power_of_two() {
            return 0;
        }
        let size = size.div_ceil(4).saturating_mul(4).max(4);
        let gaps = self.gaps();
        let pick = |&(from, to): &(u32, u32)| -> Option<u32> {
            if from_top {
                let at = to.checked_sub(size)? & !(align - 1);
                (at >= from).then_some(at)
            } else {
                let at = from.checked_add(align - 1)? & !(align - 1);
                (at.checked_add(size)? <= to).then_some(at)
            }
        };
        let found = if from_top {
            gaps.iter().rev().find_map(pick)
        } else {
            gaps.iter().find_map(pick)
        };
        match found {
            Some(at) => {
                self.used.insert(at, size);
                at
            }
            None => 0,
        }
    }

    fn free(&mut self, at: u32) {
        self.used.remove(&at);
    }

    /// The free ranges, low to high.
    fn gaps(&self) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        let mut at = self.start;
        for (&a, &s) in &self.used {
            if a > at {
                out.push((at, a));
            }
            at = a + s;
        }
        if self.end > at {
            out.push((at, self.end));
        }
        out
    }

    fn total_free(&self) -> u32 {
        self.gaps().iter().map(|(a, b)| b - a).sum()
    }

    fn largest_free(&self, align: i32) -> u32 {
        let align = align.unsigned_abs().max(4);
        self.gaps()
            .iter()
            .map(|&(a, b)| {
                let a = a.saturating_add(align - 1) & !(align - 1);
                b.saturating_sub(a)
            })
            .max()
            .unwrap_or(0)
    }
}

/// What the system functions keep between calls.
pub struct Os {
    /// Heaps by their handle (the address of their header).
    heaps: BTreeMap<u32, ExpHeap>,
    /// The base heaps, by arena number, once made.
    base: BTreeMap<u32, u32>,
    pub thread: u32,
    system_info: u32,
    next_os: u32,
    /// IPC buffer pools by handle: where the next block comes from and where the pool ends.
    ipc_pools: BTreeMap<u32, (u32, u32)>,
    voices: u32,
    /// `AXVoice`s freed by the game, to be handed out again.
    free_voices: Vec<u32>,
    /// The audio system's voices and mixer.
    pub ax: mh3u_audio::Ax,
    /// The `AXFX` reverbs the game made, by the address of its structure, and the kinds whose settings were logged.
    pub reverbs: BTreeMap<u32, crate::axfx::Fx>,
    pub fx_logged: std::collections::BTreeSet<String>,
    /// Where the audio thread puts the buses it hands to the game's callbacks (carved when it starts).
    pub ax_buffers: u32,
    /// The buttons held at the last read of the GamePad (to tell what was just pressed).
    pub pad_before: u32,
    pad_edges: (u32, u32),
    pad_read_at: std::time::Instant,
    pad_script: Option<(u32, std::time::Duration)>,
    pub started: std::time::Instant,
}

/// Lines saying which audio functions (`snd_core`, `snd_user` and their newer names) the game imports, done and not done.
pub fn audio_imports(names: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for lib in ["snd_core", "snd_user", "sndcore2", "snduser2"] {
        let mine: Vec<&String> = names.iter().filter(|n| n.split(['.', ':']).next() == Some(lib)).collect();
        if mine.is_empty() {
            continue;
        }
        let short = |n: &&String| n.rsplit(':').next().unwrap_or(n).to_string();
        let (done, todo): (Vec<&String>, Vec<&String>) = mine.iter().partition(|n| find(n).is_some());
        out.push(format!(
            "[audio] {lib}: {} imported, {} done; not done: {}",
            mine.len(),
            done.len(),
            if todo.is_empty() {
                "none".into()
            } else {
                todo.iter().map(short).collect::<Vec<_>>().join(" ")
            }
        ));
    }
    out
}

/// `AXComputeLpfCoefs(frequency)`: the one-pole low-pass filter's coefficients (Q15) for a cutoff in Hz at the renderer's rate. The
/// console takes them from a table; this is the filter whose response is 3 dB down at the cutoff (b0 from the cutoff, a0 = 1 - b0, so
/// that it passes the lows at full level).
pub fn lpf_coefs(frequency: u32, rate: u32) -> (u16, u16) {
    let w = 2.0 * std::f64::consts::PI * f64::from(frequency) / f64::from(rate);
    let k = 2.0 - w.cos();
    let b = (k - (k * k - 1.0).sqrt()).clamp(0.0, 1.0);
    let b0 = (b * 32768.0).round().min(32767.0) as u16;
    (0x8000 - b0.min(0x7fff), b0)
}

/// The size of an `AXVoice`.
const AX_VOICE_SIZE: u32 = 0x58;

/// `AXSetVoiceOffsets(voice, offsets)` (data from the offsets) and `AXSetVoiceOffsetsEx(voice, offsets, data)`.
fn set_offsets(os: &mut Os, g: &Guest, voice: u32, at: u32, data: u32) {
    let v = os.ax.voice(voice);
    v.set_offsets(g.u16(at), g.u16(at + 2) != 0, g.u32(at + 4), g.u32(at + 8), g.u32(at + 12), data);
    if std::env::var_os("MH3U_TRACE_AX").is_some() {
        eprintln!("[ax] voice {voice:#x} offsets {v:?}");
    }
}

/// `AXGetVoiceOffsets` (relative to the voice's data) and `AXGetVoiceOffsetsEx` (relative to `data`).
fn get_offsets(os: &mut Os, g: &Guest, voice: u32, at: u32, data: u32) {
    let v = os.ax.voice(voice);
    let rel = |p| mh3u_audio::voice::relative(v.format, data, p);
    g.put_u16(at, v.format);
    g.put_u16(at + 2, u16::from(v.looping));
    g.put_u32(at + 4, rel(v.loop_at));
    g.put_u32(at + 8, rel(v.end));
    g.put_u32(at + 12, rel(v.current));
    g.put_u32(at + 0x10, data);
}

enum Which {
    Current,
    Loop,
    End,
}

/// `AXSetVoice{Current,Loop,End}Offset[Ex]`: one offset, relative to the voice's data or to `data`.
fn set_offset(os: &mut Os, voice: u32, offset: u32, data: Option<u32>, which: Which) {
    let v = os.ax.voice(voice);
    let at = mh3u_audio::voice::absolute(v.format, data.unwrap_or(v.data), offset);
    match which {
        Which::Current => {
            v.current = at;
            v.restart();
        }
        Which::Loop => v.loop_at = at,
        Which::End => v.end = at,
    }
}

impl mh3u_audio::Memory for Guest {
    fn read(&self, at: u32, out: &mut [u8]) {
        if u64::from(at) + out.len() as u64 <= 1 << 32 {
            Guest::read(self, at, out);
        } else {
            out.fill(0);
        }
    }
}

/// The time base's rate, in ticks per second (a quarter of the bus clock).
pub const TICKS_PER_SECOND: u64 = 62_156_250;

impl Os {
    /// Makes the structures the game reads (they live in the OS area of the guest's memory).
    pub fn new(g: &Guest) -> Os {
        let mut os = Os {
            heaps: BTreeMap::new(),
            base: BTreeMap::new(),
            thread: 0,
            system_info: 0,
            next_os: OS_AREA,
            ipc_pools: BTreeMap::new(),
            voices: 0,
            free_voices: Vec::new(),
            ax: {
                let mut ax = mh3u_audio::Ax::default();
                ax.fx = mh3u_audio::audio_on(std::env::var("MH3U_AUDIO_FX").ok().as_deref());
                ax
            },
            reverbs: BTreeMap::new(),
            fx_logged: std::collections::BTreeSet::new(),
            ax_buffers: 0,
            pad_before: 0,
            pad_edges: (0, 0),
            pad_read_at: std::time::Instant::now(),
            // MH3U_PRESS=<button mask in hex>,<period in ms>: hold the buttons for a tenth of every period (for finding what the game waits for)
            pad_script: std::env::var("MH3U_PRESS").ok().and_then(|v| {
                let (mask, ms) = v.split_once(',')?;
                Some((
                    u32::from_str_radix(mask.trim_start_matches("0x"), 16).ok()?,
                    std::time::Duration::from_millis(ms.parse().ok()?),
                ))
            }),
            started: std::time::Instant::now(),
        };
        os.thread = os.carve(g, 0x6a0);
        os.system_info = os.carve(g, 0x40);
        // the bus and core clocks, in hertz
        g.put_u32(os.system_info, 248_625_000);
        g.put_u32(os.system_info + 4, 1_243_125_000);
        os
    }

    pub fn carve(&mut self, g: &Guest, size: u32) -> u32 {
        let at = self.next_os;
        self.next_os += size.div_ceil(0x40) * 0x40;
        assert!(
            self.next_os <= OS_AREA_END,
            "the runtime's guest structures ran into the mods' area"
        );
        g.fill(at, 0, size);
        at
    }

    /// The buttons held now.
    fn pad_hold(&self) -> u32 {
        // MH3U_PRESS_SWEEP=<seconds>: from then on, press each button in turn (for finding what a screen waits for)
        if let Some(from) = std::env::var("MH3U_PRESS_SWEEP").ok().and_then(|v| v.parse::<f64>().ok()) {
            let t = self.started.elapsed().as_secs_f64();
            if t >= from {
                const ALL: [u32; 13] = [0x4000, 0x2000, 0x1000, 0x8, 0x4, 0x200, 0x100, 0x800, 0x400, 0x20, 0x10, 0x80, 0x40];
                let slot = ((t - from) / 0.6) as usize;
                return if slot.is_multiple_of(2) { ALL[(slot / 2) % ALL.len()] } else { 0 };
            }
        }
        match self.pad_script {
            Some((mask, period)) if !period.is_zero() => {
                let t = self.started.elapsed().as_millis() % period.as_millis();
                let duty: u128 = std::env::var("MH3U_PRESS_DUTY").ok().and_then(|d| d.parse().ok()).unwrap_or(10);
                if t < period.as_millis() * duty / 100 { mask } else { 0 }
            }
            _ => 0,
        }
    }

    fn ticks(&self) -> u64 {
        // the console's clock starts at some time in the past; the game only compares differences
        ticks_after(self.started.elapsed())
    }

    fn heap(&mut self, handle: u32) -> Option<&mut ExpHeap> {
        self.heaps.get_mut(&handle)
    }

    fn make_heap(&mut self, g: &Guest, start: u32, size: u32) -> u32 {
        // the header takes the first bytes, as on the console
        g.fill(start, 0, 0x70.min(size));
        let first = start + 0x70;
        self.heaps.insert(
            start,
            ExpHeap {
                start: first,
                end: start.saturating_add(size),
                used: BTreeMap::new(),
            },
        );
        start
    }

    fn create_heap(&mut self, g: &Guest, start: u32, size: u32) -> u32 {
        if size < 0x80 {
            return 0;
        }
        self.make_heap(g, start, size)
    }

    /// The heap that covers a whole memory: 0 is MEM1, 1 is MEM2, 8 is the foreground bucket.
    fn base_heap(&mut self, g: &Guest, arena: u32) -> u32 {
        if let Some(&h) = self.base.get(&arena) {
            return h;
        }
        let (start, size) = match arena {
            0 => MEM1,
            1 => MEM2,
            5 | 8 => FOREGROUND,
            _ => return 0,
        };
        let h = self.make_heap(g, start, size);
        self.base.insert(arena, h);
        h
    }
}

/// `DMAECopyMem(dst, src, words, endian)`: copies `words` 32-bit words, turning the bytes around as `endian` says (0 as they are, 1 in
/// every 16 bits, 2 in every 32 bits). The game sends its uniform blocks this way with 2: the GPU reads them as little-endian words.
pub(crate) fn dma_copy(g: &Guest, dst: u32, src: u32, words: u32, endian: u32) {
    static TOLD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let bit = 1 << endian.min(3);
    if TOLD.fetch_or(bit, std::sync::atomic::Ordering::Relaxed) & bit == 0 {
        eprintln!("[dmae] first copy with endian mode {endian}: {words} words {src:#x} -> {dst:#x}");
    }
    let len = u64::from(words) * 4;
    if words == 0 || u64::from(dst) + len > 1 << 32 || u64::from(src) + len > 1 << 32 {
        return;
    }
    match endian {
        1 | 2 => {
            let mut bytes = vec![0; len as usize];
            g.read(src, &mut bytes);
            if endian == 1 {
                bytes.as_chunks_mut::<2>().0.iter_mut().for_each(|h| h.reverse());
            } else {
                bytes.as_chunks_mut::<4>().0.iter_mut().for_each(|w| w.reverse());
            }
            g.write(dst, &bytes);
        }
        _ => g.copy(src, dst, words * 4),
    }
}

fn ticks_after(elapsed: std::time::Duration) -> u64 {
    // nanoseconds * tick rate in u64 wraps after a few minutes
    0x1_0000_0000 + (elapsed.as_nanos() * u128::from(TICKS_PER_SECOND) / 1_000_000_000) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_clock_ticks_advance_correctly_after_fifty_minutes() {
        let elapsed = std::time::Duration::from_secs(50 * 60);
        assert_eq!(ticks_after(elapsed) - 0x1_0000_0000, 50 * 60 * TICKS_PER_SECOND);
    }

    #[test]
    fn the_dma_engine_turns_the_bytes_around_as_asked() {
        let mut m = vec![0u8; 0x100];
        let g = Guest::new(m.as_mut_ptr());
        g.write(0x10, &[1, 2, 3, 4, 5, 6, 7, 8]);
        for (endian, want) in [
            (0, [1, 2, 3, 4, 5, 6, 7, 8]),
            (1, [2, 1, 4, 3, 6, 5, 8, 7]),
            (2, [4, 3, 2, 1, 8, 7, 6, 5]),
        ] {
            dma_copy(&g, 0x40, 0x10, 2, endian);
            let mut got = [0; 8];
            g.read(0x40, &mut got);
            assert_eq!(got, want, "endian {endian}");
        }
        // a big-endian float sent with 8-in-32 is the little-endian float the GPU reads
        g.put_u32(0x20, 0.5f32.to_bits());
        dma_copy(&g, 0x60, 0x20, 1, 2);
        let mut le = [0; 4];
        g.read(0x60, &mut le);
        assert_eq!(f32::from_le_bytes(le), 0.5);
    }

    fn heap() -> ExpHeap {
        ExpHeap {
            start: 0x1000,
            end: 0x2000,
            used: BTreeMap::new(),
        }
    }

    #[test]
    fn blocks_come_from_the_low_end_or_the_high_end() {
        let mut h = heap();
        assert_eq!(h.alloc(0x10, 4), 0x1000);
        assert_eq!(h.alloc(0x10, -4), 0x1ff0);
        assert_eq!(h.alloc(0x10, 0x100), 0x1100);
        assert_eq!(h.total_free(), 0x1000 - 0x30);
    }

    #[test]
    fn freed_space_is_used_again_and_a_full_heap_says_so() {
        let mut h = heap();
        let a = h.alloc(0x800, 4);
        h.alloc(0x7f0, 4);
        assert_eq!(h.alloc(0x100, 4), 0);
        h.free(a);
        assert_eq!(h.alloc(0x100, 4), a);
    }
}

#[cfg(test)]
mod audio_tests {
    use super::*;

    #[test]
    fn the_audio_imports_are_listed_with_those_not_done() {
        let names: Vec<String> = [
            "coreinit:memset",
            "snd_core.rpl:AXSetVoiceVe",
            "snd_core.rpl:AXSetVoiceBiquad",
            "snd_user.rpl:MIXSetInput",
        ]
        .map(String::from)
        .to_vec();
        let lines = audio_imports(&names);
        assert_eq!(
            lines,
            [
                "[audio] snd_core: 2 imported, 1 done; not done: AXSetVoiceBiquad",
                "[audio] snd_user: 1 imported, 0 done; not done: MIXSetInput"
            ]
        );
    }

    /// The imports called as the game calls them: a name, r3 onwards; gives r3.
    fn caller(g: &Guest) -> impl Fn(&mut Os, &str, &[u32]) -> u32 + '_ {
        move |os: &mut Os, name: &str, r: &[u32]| -> u32 {
            let mut c = crate::game::new_ctx(std::ptr::null_mut());
            c.r[3..3 + r.len()].copy_from_slice(r);
            let Some(Handler::Simple(h)) = find(name) else { panic!("{name}") };
            h(os, g, &mut c);
            c.r[3]
        }
    }

    #[test]
    fn every_audio_import_the_game_has_is_done() {
        // the lists of run 23
        let core = "AXComputeLpfCoefs AXGetAuxCallback AXGetDRCVSMode AXQuit AXRegisterAuxCallback AXRegisterDeviceFinalMixCallback \
            AXSetDRCVSDownmixBalance AXSetDRCVSLC AXSetDRCVSMode AXSetDRCVSOutputGain AXSetDRCVSSpeakerPosition AXSetDRCVSSurroundDepth \
            AXSetDRCVSSurroundLevelGain AXSetDefaultMixerSelect AXSetVoiceLpf AXSetVoiceLpfCoefs AXSetVoiceMixerSelect AXSetVoiceSrcType \
            AXSetVoiceType AXUserBegin AXUserEnd AXUserIsProtected";
        let user = "AXFXMultiChReverbCallback AXFXMultiChReverbInit AXFXMultiChReverbSettingsUpdate AXFXMultiChReverbShutdown \
            AXFXReverbHiCallback AXFXReverbHiCallbackDpl2 AXFXReverbHiExpCallback AXFXReverbHiExpCallbackDpl2 AXFXReverbHiExpInit \
            AXFXReverbHiExpInitDpl2 AXFXReverbHiExpSettings AXFXReverbHiExpSettingsDpl2 AXFXReverbHiExpShutdown AXFXReverbHiExpShutdownDpl2 \
            AXFXReverbHiInit AXFXReverbHiInitDpl2 AXFXReverbHiSettings AXFXReverbHiSettingsDpl2 AXFXReverbHiShutdown AXFXReverbHiShutdownDpl2 \
            AXFXReverbStdCallback AXFXReverbStdCallbackDpl2 AXFXReverbStdExpCallback AXFXReverbStdExpCallbackDpl2 AXFXReverbStdExpInit \
            AXFXReverbStdExpInitDpl2 AXFXReverbStdExpSettings AXFXReverbStdExpSettingsDpl2 AXFXReverbStdExpShutdown \
            AXFXReverbStdExpShutdownDpl2 AXFXReverbStdInit AXFXReverbStdInitDpl2 AXFXReverbStdSettings AXFXReverbStdSettingsDpl2 \
            AXFXReverbStdShutdown AXFXReverbStdShutdownDpl2 AXFXSetHooks";
        let names: Vec<String> = core
            .split_whitespace()
            .map(|n| format!("snd_core.rpl:{n}"))
            .chain(user.split_whitespace().map(|n| format!("snd_user.rpl:{n}")))
            .collect();
        assert_eq!(
            audio_imports(&names),
            [
                "[audio] snd_core: 22 imported, 22 done; not done: none",
                "[audio] snd_user: 37 imported, 37 done; not done: none"
            ]
        );
    }

    #[test]
    fn the_low_pass_filter_is_set_and_its_coefficients_computed() {
        let mut memory = crate::memory::GuestMemory::reserve().expect("guest memory");
        let g = Guest::new(memory.as_mut_ptr());
        let mut os = Os::new(&g);
        let call = caller(&g);
        // AXPBLPF: on, yn1, a0, b0
        g.put_u16(0x1000, 1);
        g.put_u16(0x1002, 0);
        g.put_u16(0x1004, 0x2000);
        g.put_u16(0x1006, 0x6000);
        call(&mut os, "snd_core.rpl:AXSetVoiceLpf", &[0x500, 0x1000]);
        assert_eq!(os.ax.voices[&0x500].lpf, Some((0x2000, 0x6000)));
        call(&mut os, "snd_core.rpl:AXSetVoiceLpfCoefs", &[0x500, 0x7000, 0x1000]);
        assert_eq!(os.ax.voices[&0x500].lpf, Some((0x7000, 0x1000)));
        g.put_u16(0x1000, 0);
        call(&mut os, "snd_core.rpl:AXSetVoiceLpf", &[0x500, 0x1000]);
        assert_eq!(os.ax.voices[&0x500].lpf, None);
        // the coefficients sum to one; a low cutoff keeps much of the last output, one near the Nyquist frequency little
        call(&mut os, "snd_core.rpl:AXComputeLpfCoefs", &[500, 0x1010, 0x1012]);
        let (a0, b0) = (g.u16(0x1010), g.u16(0x1012));
        assert_eq!(u32::from(a0) + u32::from(b0), 0x8000);
        assert!(b0 > 0x6000, "{b0:#x}");
        let (_, b_high) = lpf_coefs(15_000, 32_000);
        assert!(b_high < 0x2000 && b_high < b0 / 4, "{b_high:#x}");
        // 3 dB down at the cutoff: |a / (1 - b e^-jw)|^2 = 1/2
        let (a, b) = lpf_coefs(1000, 32_000);
        let (a, b) = (f64::from(a) / 32768.0, f64::from(b) / 32768.0);
        let w = 2.0 * std::f64::consts::PI * 1000.0 / 32_000.0;
        let gain = a * a / (1.0 - 2.0 * b * w.cos() + b * b);
        assert!((gain - 0.5).abs() < 0.01, "{gain}");
    }

    #[test]
    fn the_aux_callbacks_are_kept_per_bus_and_read_back() {
        let mut memory = crate::memory::GuestMemory::reserve().expect("guest memory");
        let g = Guest::new(memory.as_mut_ptr());
        let mut os = Os::new(&g);
        let call = caller(&g);
        assert_eq!(
            call(&mut os, "snd_core.rpl:AXRegisterAuxCallback", &[0, 0, 1, 0x0300_1234, 0x4000]),
            0
        );
        assert_eq!(os.ax.aux_callbacks[0], [(0, 0), (0x0300_1234, 0x4000), (0, 0)]);
        call(&mut os, "snd_core.rpl:AXGetAuxCallback", &[0, 0, 1, 0x1000, 0x1004]);
        assert_eq!((g.u32(0x1000), g.u32(0x1004)), (0x0300_1234, 0x4000));
        assert_ne!(call(&mut os, "snd_core.rpl:AXRegisterAuxCallback", &[0, 0, 5, 1, 2]), 0);
        call(&mut os, "snd_core.rpl:AXRegisterDeviceFinalMixCallback", &[0, 0x0300_5678]);
        assert_eq!(os.ax.final_mix[0], 0x0300_5678);
    }

    /// A reverb set up through its imports, and frames of a bus with a click at the start through its callback; gives the left
    /// channel's output.
    fn reverb_through_the_imports(fx: bool, settings: &[(u32, f32)]) -> (Vec<i32>, Os) {
        let mut memory = crate::memory::GuestMemory::reserve().expect("guest memory");
        let g = Guest::new(memory.as_mut_ptr());
        let mut os = Os::new(&g);
        os.ax.fx = fx;
        let call = caller(&g);
        let reverb = 0x2000;
        for &(o, v) in settings {
            g.put_u32(reverb + o, v.to_bits());
        }
        assert_eq!(call(&mut os, "snd_user.rpl:AXFXReverbHiExpInit", &[reverb]), 1);
        // the bus as the audio thread hands it over: three channel pointers, 96 samples each (32 kHz)
        let buffers = 0x3000;
        for k in 0..3 {
            g.put_u32(buffers + 4 * k, 0x4000 + 0x200 * k);
        }
        let mut out = Vec::new();
        for frame in 0..40 {
            for k in 0..3 {
                for i in 0..96 {
                    let click = frame == 0 && i == 0 && k < 2;
                    g.put_u32(0x4000 + 0x200 * k + 4 * i, if click { 20000 } else { 0 });
                }
            }
            call(&mut os, "snd_user.rpl:AXFXReverbHiExpCallback", &[buffers, reverb]);
            out.extend((0..96).map(|i| g.u32(0x4000 + 4 * i) as i32));
        }
        (out, os)
    }

    /// HiExp's settings as decided in `axfx`: pre-delay, time, coloration, damping, crosstalk, early, fused, out.
    const HI_EXP: [(u32, f32); 8] = [
        (0x114, 0.02),
        (0x11c, 2.0),
        (0x120, 0.5),
        (0x124, 0.3),
        (0x128, 0.0),
        (0x12c, 1.0),
        (0x130, 1.0),
        (0x13c, 1.0),
    ];

    #[test]
    fn an_axfx_reverb_reads_its_settings_and_reverberates_its_bus_in_place() {
        let (out, os) = reverb_through_the_imports(true, &HI_EXP);
        let p = os.reverbs[&0x2000].reverb.params;
        assert_eq!((p.pre_delay, p.time, p.damping, p.out_gain), (0.02, 2.0, 0.3, 1.0));
        // nothing during the pre-delay (640 samples at 32 kHz), then the reflections and the tail
        assert!(out[..640].iter().all(|&s| s == 0));
        assert!(out[640..].iter().any(|&s| s != 0));
        // and the shutdown forgets it
        let mut os = os;
        let g = Guest::new(std::ptr::null_mut());
        let mut c = crate::game::new_ctx(std::ptr::null_mut());
        c.r[3] = 0x2000;
        let Some(Handler::Simple(h)) = find("snd_user.rpl:AXFXReverbHiExpShutdown") else {
            panic!()
        };
        h(&mut os, &g, &mut c);
        assert!(os.reverbs.is_empty());
    }

    #[test]
    fn a_reverb_with_effects_off_is_silent_and_one_with_unreadable_settings_is_a_default_room() {
        let (out, _) = reverb_through_the_imports(false, &HI_EXP);
        assert!(out.iter().all(|&s| s == 0));
        let (out, os) = reverb_through_the_imports(true, &[(0x11c, f32::NAN)]);
        assert_eq!(os.reverbs[&0x2000].reverb.params, mh3u_audio::fx::Params::default());
        assert!(out.iter().any(|&s| s != 0));
    }

    #[test]
    fn a_voice_is_set_up_played_and_read_back_through_the_imports() {
        let mut memory = crate::memory::GuestMemory::reserve().expect("guest memory");
        let g = Guest::new(memory.as_mut_ptr());
        let mut os = Os::new(&g);
        let call = |os: &mut Os, name: &str, r: &[u32]| -> u32 {
            let mut c = crate::game::new_ctx(std::ptr::null_mut());
            c.r[3..3 + r.len()].copy_from_slice(r);
            let Some(Handler::Simple(h)) = find(name) else { panic!("{name}") };
            h(os, &g, &mut c);
            c.r[3]
        };
        let v = call(&mut os, "snd_core.rpl:AXAcquireVoice", &[31, 0, 0]);
        assert_ne!(v, 0);
        // PCM16 samples at 0x10000: offsets relative to it, the end at sample 99, looping from 10
        let offsets = 0x1000;
        g.put_u16(offsets, 0x0a);
        g.put_u16(offsets + 2, 1);
        g.put_u32(offsets + 4, 10);
        g.put_u32(offsets + 8, 99);
        g.put_u32(offsets + 12, 0);
        g.put_u32(offsets + 0x10, 0x10000);
        for k in 0..100u32 {
            g.put_u16(0x10000 + 2 * k, 1000);
        }
        call(&mut os, "snd_core.rpl:AXSetVoiceOffsets", &[v, offsets]);
        g.put_u16(0x1100, 0x8000);
        call(&mut os, "snd_core.rpl:AXSetVoiceVe", &[v, 0x1100]);
        call(&mut os, "snd_core.rpl:AXSetVoiceState", &[v, 1]);
        assert_eq!(g.u32(v + 4), 1);
        assert_eq!(call(&mut os, "snd_core.rpl:AXIsVoiceRunning", &[v]), 1);
        // a frame at 32 kHz moves a voice at pitch 1 on by 96 samples: not yet at its end (sample 99)
        let (frame, stopped) = os.ax.mix_frame(&g);
        assert!(stopped.is_empty());
        assert_eq!(frame.len(), 96);
        assert!(frame[10][0] > 0.0, "a voice without a mix is heard centred");
        call(&mut os, "snd_core.rpl:AXGetVoiceOffsets", &[v, 0x1200]);
        assert_eq!(g.u32(0x1200 + 0x10), 0x10000);
        let current = g.u32(0x1200 + 12);
        assert!((10..99).contains(&current), "{current}");
        assert_eq!(call(&mut os, "snd_core.rpl:AXGetVoiceLoopCount", &[v]), 0);
        // the next: to the end, then twice round the 90-sample loop (samples 10 to 99)
        os.ax.mix_frame(&g);
        assert_eq!(call(&mut os, "snd_core.rpl:AXGetVoiceLoopCount", &[v]), 2);
        // the current offset relative to another base
        assert_eq!(
            call(&mut os, "snd_core.rpl:AXGetVoiceCurrentOffsetEx", &[v, 0x10000 - 0x20]),
            call(&mut os, "snd_core.rpl:AXGetVoiceCurrentOffsetEx", &[v, 0x10000]) + 0x10
        );
        // a freed voice is handed out again
        call(&mut os, "snd_core.rpl:AXFreeVoice", &[v]);
        assert_eq!(call(&mut os, "snd_core.rpl:AXAcquireVoice", &[31, 0, 0]), v);
        assert!(!os.ax.voices[&v].running);
    }
}
