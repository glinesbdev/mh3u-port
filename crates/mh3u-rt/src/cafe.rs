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
    started: std::time::Instant,
}

/// The time base's rate, in ticks per second (a quarter of the bus clock).
const TICKS_PER_SECOND: u64 = 62_156_250;

impl Os {
    /// Makes the structures the game reads (they live in the OS area of the guest's memory).
    pub fn new(g: &Guest) -> Os {
        let mut os = Os {
            heaps: BTreeMap::new(),
            base: BTreeMap::new(),
            thread: 0,
            system_info: 0,
            next_os: OS_AREA,
            started: std::time::Instant::now(),
        };
        os.thread = os.carve(g, 0x6a0);
        os.system_info = os.carve(g, 0x40);
        // the bus and core clocks, in hertz
        g.put_u32(os.system_info, 248_625_000);
        g.put_u32(os.system_info + 4, 1_243_125_000);
        os
    }

    fn carve(&mut self, g: &Guest, size: u32) -> u32 {
        let at = self.next_os;
        self.next_os += size.div_ceil(0x40) * 0x40;
        g.fill(at, 0, size);
        at
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
