//! Read-only access to the game's memory for mods (the overlay and what it shows).
//!
//! A mod never reads guest memory itself. It says which ranges it wants (`watch`), and at each of the game's buffer swaps the game
//! thread, which holds the baton (so no game code runs meanwhile), copies them into a `Snapshot` that the mod reads whenever it likes.
//! A mod can also have a range of memory searched for something (`search`): the game thread looks through a slice of it at each swap,
//! so a search of a gigabyte takes a few seconds and never stalls a frame. With nothing watched or searched this costs one atomic load
//! a frame. All values in guest memory are big-endian.

use crate::guest::Guest;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

/// The most bytes watched at once (copied every frame).
pub const MOST_WATCHED: u32 = 1 << 20;
/// How much of a search is done per frame.
const SEARCH_PER_FRAME: u32 = 8 << 20;
/// The pieces a search reads at once; also the unit of the untouched-page check (Windows commits 64 KiB at a time).
const SEARCH_PIECE: u32 = 64 << 10;

/// Copies of the watched ranges, taken at one frame.
#[derive(Default, Debug)]
pub struct Snapshot {
    /// The frame (`crate::FRAMES`) it was taken at.
    pub frame: u64,
    pub ranges: Vec<(u32, Vec<u8>)>,
}

impl Snapshot {
    /// The `len` bytes at `at`, if one range holds them all.
    pub fn bytes(&self, at: u32, len: usize) -> Option<&[u8]> {
        self.ranges.iter().find_map(|(start, data)| {
            let from = at.checked_sub(*start)? as usize;
            data.get(from..from.checked_add(len)?)
        })
    }

    pub fn u32(&self, at: u32) -> Option<u32> {
        Some(u32::from_be_bytes(self.bytes(at, 4)?.try_into().ok()?))
    }

    pub fn u16(&self, at: u32) -> Option<u16> {
        Some(u16::from_be_bytes(self.bytes(at, 2)?.try_into().ok()?))
    }
}

/// What a search looks for: called for each 4-byte aligned address with a reader of the big-endian words near it (none outside the
/// piece being searched), it says whether this is the place.
pub type Test = fn(read: &dyn Fn(u32) -> Option<u32>, at: u32) -> bool;

/// How far a test may read past the address it is given (the pieces overlap by this much, so nothing is missed at their edges).
pub const SEARCH_REACH: u32 = 0x100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Searching {
    Idle,
    Running { done: u32, of: u32 },
    Found(u32),
    NotFound,
}

struct Search {
    next: u32,
    from: u32,
    to: u32,
    test: Test,
}

#[derive(Default)]
struct State {
    watched: Vec<(u32, u32)>,
    latest: Arc<Snapshot>,
    search: Option<Search>,
    result: Option<Option<u32>>,
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static STATE: std::sync::LazyLock<Mutex<State>> = std::sync::LazyLock::new(Default::default);
static SNAPSHOTS: AtomicU64 = AtomicU64::new(0);

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn update_active(s: &State) {
    ACTIVE.store(!s.watched.is_empty() || s.search.is_some(), Relaxed);
}

/// The ranges (start, length) copied at every frame from now on, replacing the ones before; ranges beyond `MOST_WATCHED` bytes in
/// all are left out. An empty list stops the copying.
pub fn watch(ranges: &[(u32, u32)]) {
    let mut total = 0u32;
    let kept: Vec<(u32, u32)> = ranges
        .iter()
        .copied()
        .filter(|&(at, len)| {
            let fits = u64::from(at) + u64::from(len) <= 1 << 32 && total + len <= MOST_WATCHED;
            if fits {
                total += len;
            }
            fits
        })
        .collect();
    let mut s = state();
    s.watched = kept;
    update_active(&s);
}

/// The latest copies of the watched ranges.
pub fn latest() -> Arc<Snapshot> {
    state().latest.clone()
}

/// Starts looking through `from..to` for the place `test` accepts (replacing a search under way).
pub fn search(from: u32, to: u32, test: Test) {
    let mut s = state();
    s.search = Some(Search {
        next: from & !3,
        from,
        to,
        test,
    });
    s.result = None;
    update_active(&s);
}

pub fn searching() -> Searching {
    let s = state();
    match (&s.search, s.result) {
        (Some(x), _) => Searching::Running {
            done: x.next - x.from,
            of: x.to - x.from,
        },
        (None, Some(Some(at))) => Searching::Found(at),
        (None, Some(None)) => Searching::NotFound,
        (None, None) => Searching::Idle,
    }
}

/// Called by the game thread at each buffer swap (with the baton): copies the watched ranges and goes on with the search.
pub(crate) fn on_frame(g: &Guest) {
    if !ACTIVE.load(Relaxed) {
        return;
    }
    let mut s = state();
    if !s.watched.is_empty() {
        let ranges = s
            .watched
            .iter()
            .map(|&(at, len)| {
                let mut data = vec![0; len as usize];
                g.read(at, &mut data);
                (at, data)
            })
            .collect();
        s.latest = Arc::new(Snapshot {
            frame: crate::FRAMES.load(Relaxed),
            ranges,
        });
        SNAPSHOTS.fetch_add(1, Relaxed);
    }
    if let Some(search) = s.search.as_mut() {
        let found = search_slice(g, search);
        if let Some(found) = found {
            s.result = Some(found);
            s.search = None;
            update_active(&s);
        }
    }
}

/// One frame's slice of a search: `Some(result)` when it ends.
fn search_slice(g: &Guest, x: &mut Search) -> Option<Option<u32>> {
    let end = x.to.min(x.next.saturating_add(SEARCH_PER_FRAME));
    while x.next < end {
        let piece = x.next;
        let len = SEARCH_PIECE.min(x.to - piece);
        x.next = piece.saturating_add(len);
        if !g.touched(piece) {
            continue;
        }
        // the piece and the reach after it (when touched), read in place
        let reach = if piece.checked_add(len + SEARCH_REACH).is_some_and(|e| e <= x.to && g.touched(e - 1)) {
            SEARCH_REACH
        } else {
            0
        };
        let hit = g.with_bytes(piece, (len + reach) as usize, |bytes| {
            let read = |at: u32| -> Option<u32> {
                let o = at.checked_sub(piece)? as usize;
                Some(u32::from_be_bytes(bytes.get(o..o + 4)?.try_into().ok()?))
            };
            (0..len / 4).map(|k| piece + 4 * k).find(|&at| (x.test)(&read, at))
        });
        if hit.is_some() {
            return Some(hit);
        }
    }
    (x.next >= x.to).then_some(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests share the one state, so they run as one.
    #[test]
    fn watched_ranges_are_copied_at_a_frame_and_a_search_finds_its_place_across_frames() {
        let mut memory = crate::memory::GuestMemory::reserve().expect("guest memory");
        let g = Guest::new(memory.as_mut_ptr());
        // nothing asked: nothing copied
        on_frame(&g);
        assert!(latest().ranges.is_empty());

        g.put_u32(0x1000_0000, 0x1234_5678);
        g.put_u16(0x1000_0010, 0xbeef);
        watch(&[(0x1000_0000, 0x20), (0x2000_0000, MOST_WATCHED)]);
        on_frame(&g);
        let snap = latest();
        // the second range did not fit beside the first
        assert_eq!(snap.ranges.len(), 1);
        assert_eq!(snap.u32(0x1000_0000), Some(0x1234_5678));
        assert_eq!(snap.u16(0x1000_0010), Some(0xbeef));
        assert_eq!(snap.bytes(0x1000_001e, 4), None, "past the end of the range");
        assert_eq!(snap.u32(0x0fff_fffe), None);
        // a write after the copy is not in it until the next frame
        g.put_u32(0x1000_0000, 1);
        assert_eq!(latest().u32(0x1000_0000), Some(0x1234_5678));
        on_frame(&g);
        assert_eq!(latest().u32(0x1000_0000), Some(1));
        watch(&[]);

        // a place whose word at +8 points 0xc before it and whose word at +0x24 is that plus 0x34 (the live save's header), placed
        // across two pieces' edge and two frames' slices into the search
        let at = 0x1100_0000 + SEARCH_PER_FRAME + SEARCH_PIECE - 0x10;
        g.put_u32(at + 8, at - 0xc);
        g.put_u32(at + 0x24, at - 0xc + 0x34);
        fn test(read: &dyn Fn(u32) -> Option<u32>, at: u32) -> bool {
            read(at + 8) == Some(at.wrapping_sub(0xc)) && read(at + 0x24) == Some(at.wrapping_add(0x28))
        }
        search(0x1100_0000, 0x1100_0000 + 4 * SEARCH_PER_FRAME, test);
        on_frame(&g);
        assert!(matches!(searching(), Searching::Running { .. }));
        on_frame(&g);
        assert_eq!(searching(), Searching::Found(at));
        // and one that is not there ends as not found
        search(0x1200_0000, 0x1200_0000 + SEARCH_PER_FRAME, test);
        on_frame(&g);
        assert_eq!(searching(), Searching::NotFound);
    }
}
