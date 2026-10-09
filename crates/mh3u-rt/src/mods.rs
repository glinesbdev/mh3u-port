//! Read-only access to the game's memory for mods (the overlay and what it shows).
//!
//! A mod never reads guest memory itself. It says which ranges it wants (`watch`), and at each of the game's buffer swaps the game
//! thread, which holds the baton (so no game code runs meanwhile), copies them into a `Snapshot` that the mod reads whenever it likes.
//! A mod can also have memory searched for something (`search`): a thread of its own reads it while the game runs (tolerantly: what it
//! finds is to be checked again from a snapshot), so a search never holds up the game or its audio. With nothing watched or searched this costs one atomic load
//! a frame. All values in guest memory are big-endian.

use crate::guest::Guest;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

/// The most bytes watched at once (copied every frame).
pub const MOST_WATCHED: u32 = 1 << 20;
/// How much the search reads between short rests.
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

/// What a search looks for: given a piece of memory and the guest address it was read at, it adds the places that may be it.
pub type Finder = Arc<dyn Fn(&[u8], u32, &mut Vec<u32>) + Send + Sync>;

/// How far past a piece a finder may need to see (the pieces overlap by this much, so nothing is missed at their edges).
pub const SEARCH_REACH: u32 = 0x100;
/// The most candidates a search gives.
const MOST_CANDIDATES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Searching {
    Idle,
    Running {
        done: u64,
        of: u64,
    },
    /// The candidates found (sorted, each once).
    Done(Vec<u32>),
}

#[derive(Default)]
struct State {
    watched: Vec<(u32, u32)>,
    latest: Arc<Snapshot>,
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static STATE: std::sync::LazyLock<Mutex<State>> = std::sync::LazyLock::new(Default::default);
static SEARCH: Mutex<Searching> = Mutex::new(Searching::Idle);
/// Which search is the current one (a newer one stops the older).
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Where guest memory is in the host (set at the first frame), for the search thread.
static GUEST_BASE: AtomicUsize = AtomicUsize::new(0);

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
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
    ACTIVE.store(!kept.is_empty(), Relaxed);
    s.watched = kept;
}

/// The latest copies of the watched ranges.
pub fn latest() -> Arc<Snapshot> {
    state().latest.clone()
}

/// Looks through `ranges` for what `finder` finds, on a thread of its own (replacing a search under way). The game's memory is read
/// while the game runs: a value may be torn or stale, so the candidates are only that, and are to be checked from a snapshot.
pub fn search(ranges: Vec<(u32, u32)>, finder: Finder) {
    let generation = GENERATION.fetch_add(1, Relaxed) + 1;
    let of = ranges.iter().map(|r| u64::from(r.1)).sum();
    *SEARCH.lock().unwrap_or_else(|e| e.into_inner()) = Searching::Running { done: 0, of };
    let _ = std::thread::Builder::new().name("mods search".into()).spawn(move || {
        let current = || GENERATION.load(Relaxed) == generation;
        // the guest's memory is known once the game has drawn a frame
        let base = loop {
            match GUEST_BASE.load(Relaxed) {
                0 if current() => std::thread::sleep(std::time::Duration::from_millis(50)),
                0 => return,
                b => break b,
            }
        };
        let mut found = Vec::new();
        let mut done = 0u64;
        let mut buf = vec![0u8; (SEARCH_PIECE + SEARCH_REACH) as usize];
        let mut since_rest = 0u32;
        for (from, len) in ranges {
            let to = from.saturating_add(len);
            let mut at = from & !3;
            while at < to {
                if !current() {
                    return;
                }
                let n = SEARCH_PIECE.min(to - at);
                if crate::memory::touched((base + at as usize) as *const u8) {
                    let reach_ok = at.checked_add(n + SEARCH_REACH).is_some_and(|e| e <= to)
                        && crate::memory::touched((base + (at + n) as usize) as *const u8);
                    let len = (n + if reach_ok { SEARCH_REACH } else { 0 }) as usize;
                    read_tolerant(base, at, &mut buf[..len]);
                    finder(&buf[..len], at, &mut found);
                    found.truncate(MOST_CANDIDATES * 4);
                }
                at = at.saturating_add(n);
                done += u64::from(n);
                since_rest += n;
                // a moment's rest every 8 MiB: the search is in no hurry, the game's threads are
                if since_rest >= SEARCH_PER_FRAME {
                    since_rest = 0;
                    if let Ok(mut s) = SEARCH.lock()
                        && current()
                    {
                        *s = Searching::Running { done, of };
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        found.truncate(MOST_CANDIDATES);
        if current() {
            *SEARCH.lock().unwrap_or_else(|e| e.into_inner()) = Searching::Done(found);
        }
    });
}

/// Reads guest memory while the game may be writing it: word by word with atomic loads, so a value can be stale or mixed but the read
/// itself is sound. `at` is 4-aligned and the range touched (see `memory::touched`).
fn read_tolerant(base: usize, at: u32, out: &mut [u8]) {
    for (k, w) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let p = (base + at as usize + 4 * k) as *const u32;
        // SAFETY: inside the guest's reserved range, aligned to 4 and committed (the caller checked `touched`); an atomic load does
        // not tear and is sound alongside the game's writes
        let v = unsafe { std::sync::atomic::AtomicU32::from_ptr(p.cast_mut()) }.load(Relaxed);
        w.copy_from_slice(&v.to_ne_bytes());
    }
}

pub fn searching() -> Searching {
    SEARCH.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Stops a search under way.
pub fn stop_search() {
    GENERATION.fetch_add(1, Relaxed);
    *SEARCH.lock().unwrap_or_else(|e| e.into_inner()) = Searching::Idle;
}

/// Called by the game thread at each buffer swap (with the baton): copies the watched ranges and goes on with the search.
pub(crate) fn on_frame(g: &Guest) {
    GUEST_BASE.store(g.base_addr(), Relaxed);
    if !ACTIVE.load(Relaxed) {
        return;
    }
    let mut s = state();
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests share the one state, so they run as one.
    #[test]
    fn watched_ranges_are_copied_at_a_frame_and_a_search_thread_finds_its_place() {
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

        // a place whose header points at itself, across two pieces' edge, found by the search thread while "the game" writes
        // elsewhere
        let at = 0x1100_0000 + 3 * SEARCH_PIECE - 0x10;
        g.put_u32(at + 8, at - 0xc);
        g.put_u32(at + 0x24, at - 0xc + 0x34);
        let finder: Finder = Arc::new(|piece: &[u8], base: u32, out: &mut Vec<u32>| {
            for k in (0..piece.len().saturating_sub(0x28)).step_by(4) {
                let w = |o: usize| u32::from_be_bytes(piece[o..o + 4].try_into().unwrap());
                let a = base + k as u32;
                if w(k + 8) == a.wrapping_sub(0xc) && w(k + 0x24) == a.wrapping_add(0x28) {
                    out.push(a);
                }
            }
        });
        let wait = || {
            let t = std::time::Instant::now();
            loop {
                if let Searching::Done(v) = searching() {
                    return v;
                }
                assert!(t.elapsed() < std::time::Duration::from_secs(20), "the search did not end");
                g.put_u32(0x1200_0000, t.elapsed().as_nanos() as u32);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        };
        search(
            vec![(0x1100_0000, 4 * SEARCH_PER_FRAME), (0x1300_0000, SEARCH_PIECE)],
            finder.clone(),
        );
        assert_eq!(wait(), [at]);
        // an empty search ends with nothing
        search(vec![(0x1400_0000, SEARCH_PER_FRAME)], finder);
        assert_eq!(wait(), Vec::<u32>::new());
        stop_search();
        assert_eq!(searching(), Searching::Idle);
    }
}

/// F9 and F10 presses (the window counts them): F9 closes a call-diff window (`MH3U_CALLDIFF`), F10 searches for copies of the
/// pouch again (`MH3U_WATCH=live`; on a quest, the quest pouch). See `docs/hooks.md`.
pub static KEY_F9: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
pub static KEY_F10: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The functions sampled far more often in window `b` than in window `a` (counts of the most recently entered function, `na` and
/// `nb` samples): (function, count in a, count in b), most first. A function needs 5 samples in `b` and 4 times its rate in `a`.
pub fn call_diff(a: &std::collections::HashMap<u32, u32>, b: &std::collections::HashMap<u32, u32>) -> Vec<(u32, u32, u32)> {
    call_diff_min(a, b, 5)
}

/// As `call_diff`, with the fewest calls in `b` given (1 for exact counts: a function called once, only in `b`, is kept).
pub fn call_diff_min(a: &std::collections::HashMap<u32, u32>, b: &std::collections::HashMap<u32, u32>, min: u32) -> Vec<(u32, u32, u32)> {
    let (na, nb) = (a.values().sum::<u32>().max(1) as f64, b.values().sum::<u32>().max(1) as f64);
    let mut out: Vec<(u32, u32, u32, f64)> = b
        .iter()
        .filter(|&(_, &n)| n >= min)
        .filter_map(|(&f, &n)| {
            let before = a.get(&f).copied().unwrap_or(0);
            let ratio = (f64::from(n) / nb) / (f64::from(before).max(0.5) / na);
            (ratio >= 4.0).then_some((f, before, n, ratio))
        })
        .collect();
    out.sort_by(|x, y| y.3.total_cmp(&x.3).then(y.2.cmp(&x.2)));
    out.into_iter().map(|(f, a, b, _)| (f, a, b)).collect()
}

/// Whether this is an `MH3U_WATCH=live` run of a watch build (the overlay then looks for the live save even while closed).
pub fn watching_live() -> bool {
    WATCH_FNS.get().is_some()
}

/// `MH3U_WATCH=live` (a `--watch` build, `docs/hooks.md`): stores into a range found at run time (the live save) are logged with
/// what they write (a label from `label`), the function most recently entered and the link register, so the functions that
/// change items, equipment and money can be found and hooked.
pub struct WriteWatch {
    label: Box<dyn Fn(u32) -> String + Send + Sync>,
    seen: Mutex<std::collections::HashMap<(u32, u32, String), u32>>,
}

/// The watch build's `game_set_watch` and `game_last_function`, when `MH3U_WATCH=live`.
pub(crate) static WATCH_FNS: std::sync::OnceLock<(unsafe extern "C" fn(u32, u32), unsafe extern "C" fn() -> u32)> =
    std::sync::OnceLock::new();
static WRITE_WATCH: Mutex<Option<Arc<WriteWatch>>> = Mutex::new(None);

/// Watches stores to `lo..hi` (guest addresses), naming each with `label(address)`. False when this is not an `MH3U_WATCH=live`
/// run of a watch build (then nothing is watched).
pub fn watch_writes(lo: u32, hi: u32, label: Box<dyn Fn(u32) -> String + Send + Sync>) -> bool {
    let Some((set, _)) = WATCH_FNS.get() else { return false };
    *WRITE_WATCH.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(WriteWatch {
        label,
        seen: Default::default(),
    }));
    crate::guest::WATCH.0.store(lo, std::sync::atomic::Ordering::Relaxed);
    crate::guest::WATCH.1.store(hi, std::sync::atomic::Ordering::Relaxed);
    // SAFETY: the watch build's own function, with this signature
    unsafe { set(lo, hi) };
    eprintln!("[watch-live] watching {lo:#x}..{hi:#x}: pick up, use, sell or craft things now");
    true
}

/// Whether a store's log line is printed: the first 5 from each writer (function, lr, field kind), then every 100th.
pub fn should_report(count: u32) -> bool {
    count < 5 || count.is_multiple_of(100)
}

/// Logs a watched store in a live watch; false when there is none (the plain watch log then reports it).
pub(crate) fn report_write(address: u32, value: u64, size: u32, lr: u32) -> bool {
    let Some(w) = WRITE_WATCH.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        return false;
    };
    let Some((_, last)) = WATCH_FNS.get() else { return false };
    // SAFETY: a plain read of a global of the watch build
    let function = unsafe { last() };
    let label = (w.label)(address);
    if label.is_empty() {
        // outside what is watched in a wide range (all the pouch copies at once): not reported
        return true;
    }
    let kind = label.split(' ').filter(|p| p.parse::<u32>().is_err()).collect::<Vec<_>>().join(" ");
    let count = {
        let mut seen = w.seen.lock().unwrap_or_else(|e| e.into_inner());
        let n = seen.entry((function, lr, kind)).or_insert(0);
        *n += 1;
        *n - 1
    };
    if should_report(count) {
        eprintln!(
            "[watch-live] {label} <- {value:#x} ({size} bytes at {address:#x}) by f_{function:08x}, lr {lr:#x}{}",
            if count > 0 {
                format!(" (write {} from here)", count + 1)
            } else {
                String::new()
            }
        );
    }
    true
}

/// Where `pattern` occurs (word-aligned) in `piece`, which starts at guest address `base`.
pub fn copies_in(piece: &[u8], base: u32, pattern: &[u8], out: &mut Vec<u32>) {
    copies_in_masked(piece, base, pattern, &vec![true; pattern.len()], out);
}

/// As `copies_in`, comparing only the bytes `mask` marks (a pouch's filled slots: on a quest, the empty ones may hold supplies).
pub fn copies_in_masked(piece: &[u8], base: u32, pattern: &[u8], mask: &[bool], out: &mut Vec<u32>) {
    if pattern.is_empty() || piece.len() < pattern.len() {
        return;
    }
    let first = mask.iter().position(|&m| m).unwrap_or(0);
    for i in (0..=piece.len() - pattern.len()).step_by(4) {
        if piece[i + first] == pattern[first] && (0..pattern.len()).all(|k| !mask[k] || piece[i + k] == pattern[k]) {
            out.push(base + i as u32);
        }
    }
}

/// The copies to watch at once: the largest group within `span` bytes (sorted input), as (first, last).
pub fn cluster(copies: &[u32], span: u32) -> Option<(u32, u32)> {
    let mut best: Option<(usize, u32, u32)> = None;
    for (i, &a) in copies.iter().enumerate() {
        let n = copies[i..].iter().take_while(|&&c| c >= a && c - a <= span).count();
        let last = copies[i + n - 1];
        if best.is_none_or(|b| n > b.0) {
            best = Some((n, a, last));
        }
    }
    best.map(|b| (b.1, b.2))
}

/// `MH3U_WATCH=live`: the save the overlay found may be only the save image (written when saving), so this looks for other copies
/// of its `len` bytes at `at` (the pouch) in `ranges`, in the background, logs them, and watches the copy `MH3U_WATCH_COPY` (0, the
/// first, by default) as `lo..hi` around it; `label` names a watched address by its distance from the copy.
pub fn watch_copies(at: u32, len: u32, ranges: Vec<(u32, u32)>, around: (i64, i64), label: Arc<dyn Fn(i64) -> String + Send + Sync>) {
    if WATCH_FNS.get().is_none() {
        return;
    }
    let _ = std::thread::Builder::new().name("watch copies".into()).spawn(move || {
        let base = loop {
            match GUEST_BASE.load(Relaxed) {
                0 => std::thread::sleep(std::time::Duration::from_millis(50)),
                b => break b,
            }
        };
        let mut pattern = vec![0u8; len as usize];
        read_tolerant(base, at, &mut pattern);
        // only the filled slots are compared (on a quest the empty ones may have been given supplies)
        let mask: Vec<bool> = pattern.as_chunks::<4>().0.iter().flat_map(|s| [s[..2] != [0, 0]; 4]).collect();
        let filled = pattern.as_chunks::<4>().0.iter().filter(|s| s[..2] != [0, 0]).count();
        if filled < 3 {
            eprintln!("[watch-live] the pouch has {filled} items; put at least 3 in it and run again so its copies can be found");
            return;
        }
        let mut found = Vec::new();
        let piece = 0x10_0000u32;
        let mut buf = vec![0u8; (piece + len) as usize];
        for (from, n) in ranges {
            let to = from.saturating_add(n);
            let mut p = from & !3;
            while p < to {
                let m = piece.min(to - p);
                let slice = &mut buf[..(m + len).min(to - p) as usize];
                read_tolerant(base, p, slice);
                copies_in_masked(slice, p, &pattern, &mask, &mut found);
                p = p.saturating_add(m);
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
        }
        found.sort_unstable();
        found.dedup();
        found.retain(|&c| c != at);
        if found.is_empty() {
            eprintln!("[watch-live] the pouch is in no other place than the save at {at:#x}; still watching the save");
            return;
        }
        let list: Vec<String> = found.iter().take(16).map(|c| format!("{c:#x}")).collect();
        eprintln!(
            "[watch-live] the pouch is also at {} (MH3U_WATCH_COPY=n picks one, 0 = the first)",
            list.join(", ")
        );
        let pick = std::env::var("MH3U_WATCH_COPY")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let Some(&copy) = found.get(pick).or(found.first()) else { return };
        let lo = (i64::from(copy) + around.0).max(0) as u32;
        let hi = (i64::from(copy) + around.1).min(i64::from(u32::MAX)) as u32;
        let label = label.clone();
        watch_writes(
            lo,
            hi,
            Box::new(move |a| format!("copy {copy:#x}: {}", label(i64::from(a) - i64::from(copy)))),
        );
    });
}

#[cfg(test)]
mod write_watch_tests {
    #[test]
    fn the_call_diff_keeps_the_functions_that_only_run_in_the_second_window() {
        let a: std::collections::HashMap<u32, u32> = [(1, 500), (2, 500)].into();
        let b: std::collections::HashMap<u32, u32> = [(1, 450), (2, 400), (3, 100), (4, 3), (5, 50)].into();
        let mut a2 = a.clone();
        a2.insert(5, 40);
        let d = super::call_diff(&a2, &b);
        assert_eq!(
            d.iter().map(|x| x.0).collect::<Vec<_>>(),
            vec![3],
            "5 ran before too, 4 too rarely, 1 and 2 always"
        );
        assert_eq!(d[0], (3, 0, 100));
        // exact counts: a function called once, only in the second window, is kept
        let b: std::collections::HashMap<u32, u32> = [(1, 450), (9, 1)].into();
        assert_eq!(super::call_diff_min(&a, &b, 1), vec![(9, 0, 1)]);
    }
    #[test]
    fn copies_of_the_pouch_are_found_word_aligned() {
        let pat = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let mut piece = vec![0u8; 64];
        piece[8..16].copy_from_slice(&pat);
        piece[41..49].copy_from_slice(&pat); // not aligned
        let mut out = Vec::new();
        super::copies_in(&piece, 0x1000, &pat, &mut out);
        assert_eq!(out, vec![0x1008]);
        // masked: a changed byte outside the mask still matches
        let mut piece2 = piece.clone();
        piece2[12] = 99;
        let mut mask = vec![true; 8];
        mask[4] = false;
        let mut out = Vec::new();
        super::copies_in_masked(&piece2, 0x1000, &pat, &mask, &mut out);
        assert_eq!(out, vec![0x1008]);
        assert_eq!(super::cluster(&[0x100, 0x200, 0x300, 0x5000_0000], 0x1000), Some((0x100, 0x300)));
    }

    #[test]
    fn reports_are_rate_limited_per_writer() {
        let shown: Vec<u32> = (0..250).filter(|&n| super::should_report(n)).collect();
        assert_eq!(shown, vec![0, 1, 2, 3, 4, 100, 200]);
        assert!(!super::watch_writes(0, 4, Box::new(|_| String::new())), "not a live watch run");
    }
}
