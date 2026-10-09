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
    simple(name).or_else(|| crate::gx2::find(name)).map(Handler::Simple)
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
        // a voice of the audio system: only its index is looked at by the game
        "snd_core.rpl:AXAcquireVoice" => |os, g, c| {
            let v = os.carve(g, 0x100);
            g.put_u32(v, os.voices);
            os.voices += 1;
            c.r[3] = v;
        },
        "snd_core.rpl:AXFreeVoice" => |os, _, c| {
            os.voice_state.remove(&c.r[3]);
            c.r[3] = 0;
        },
        "snd_core.rpl:AXInit" | "snd_core.rpl:AXInitEx" => |_, _, c| c.r[3] = 0,
        // where a voice's sample data starts, loops and ends, and where it is now (in samples)
        "snd_core.rpl:AXSetVoiceOffsets" => |os, g, c| {
            let (voice, at) = (c.r[3], c.r[4]);
            let v = os.voice_state.entry(voice).or_default();
            v.advance();
            v.format = g.u16(at);
            v.looping = g.u16(at + 2) != 0;
            v.loop_offset = g.u32(at + 4);
            v.end = g.u32(at + 8);
            v.current = f64::from(g.u32(at + 12));
        },
        "snd_core.rpl:AXGetVoiceOffsets" => |os, g, c| {
            let (voice, at) = (c.r[3], c.r[4]);
            let v = os.voice_state.entry(voice).or_default();
            v.advance();
            g.put_u16(at, v.format);
            g.put_u16(at + 2, u16::from(v.looping));
            g.put_u32(at + 4, v.loop_offset);
            g.put_u32(at + 8, v.end);
            g.put_u32(at + 12, v.current as u32);
            if std::env::var_os("MH3U_TRACE_AX").is_some() {
                eprintln!("[ax] voice {voice:#x} {v:?}");
            }
        },
        "snd_core.rpl:AXSetVoiceState" => |os, _, c| {
            let v = os.voice_state.entry(c.r[3]).or_default();
            v.advance();
            v.running = c.r[4] != 0;
        },
        // the pitch: how many samples of the voice go by per sample of the output
        "snd_core.rpl:AXSetVoiceSrcRatio" => |os, _, c| {
            let v = os.voice_state.entry(c.r[3]).or_default();
            v.advance();
            v.ratio = f64::from_bits(c.f[1]);
        },
        "snd_core.rpl:AXSetVoiceSrc" => |os, g, c| {
            // the ratio is a fixed-point number with 16 fractional bits
            let v = os.voice_state.entry(c.r[3]).or_default();
            v.advance();
            v.ratio = f64::from(g.u32(c.r[4])) / 65536.0;
        },
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
            let (dst, src, words) = (c.r[3], c.r[4], c.r[5]);
            if words > 0 && u64::from(dst) + u64::from(words) * 4 <= 1 << 32 && u64::from(src) + u64::from(words) * 4 <= 1 << 32 {
                g.copy(src, dst, words * 4);
            }
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
    voice_state: BTreeMap<u32, Voice>,
    /// The buttons held at the last read of the GamePad (to tell what was just pressed).
    pub pad_before: u32,
    pad_edges: (u32, u32),
    pad_read_at: std::time::Instant,
    pad_script: Option<(u32, std::time::Duration)>,
    pub started: std::time::Instant,
}

/// What the game has told the audio system about a voice. Nothing is played yet, but the game watches how far a voice has got (to
/// know when a sound has ended), so a running voice moves on with the time at the rate of its pitch.
#[derive(Debug)]
struct Voice {
    format: u16,
    looping: bool,
    loop_offset: u32,
    end: u32,
    current: f64,
    running: bool,
    ratio: f64,
    last: std::time::Instant,
}

impl Default for Voice {
    fn default() -> Voice {
        Voice {
            format: 0,
            looping: false,
            loop_offset: 0,
            end: 0,
            current: 0.0,
            running: false,
            ratio: 1.0,
            last: std::time::Instant::now(),
        }
    }
}

/// The output's sample rate.
const SAMPLE_RATE: f64 = 48_000.0;

impl Voice {
    /// Moves a running voice forward to now.
    fn advance(&mut self) {
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        if !self.running {
            return;
        }
        self.current += elapsed * SAMPLE_RATE * self.ratio;
        let end = f64::from(self.end);
        if self.current >= end {
            if self.looping && end > f64::from(self.loop_offset) {
                let length = end - f64::from(self.loop_offset);
                self.current = f64::from(self.loop_offset) + (self.current - end) % length;
            } else {
                self.current = end;
                self.running = false;
            }
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
            voice_state: BTreeMap::new(),
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
        let elapsed = self.started.elapsed();
        0x1_0000_0000 + elapsed.as_nanos() as u64 * TICKS_PER_SECOND / 1_000_000_000
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

#[cfg(test)]
mod tests {
    use super::*;

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
