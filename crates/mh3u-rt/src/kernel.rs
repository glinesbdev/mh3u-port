//! The guest's threads and the objects they wait on.
//!
//! Guest threads are host threads, but only one of them runs guest code at a time: the one that holds the baton (`Kernel::current`). A
//! thread passes the baton on when it calls a system function that has to wait (a locked mutex, an unset event, a sleep), when it
//! yields, and when it ends. A waiting function is run again each time its thread gets the baton back, and gives up the baton again if
//! what it waits for has not happened. This keeps the order of events the same from one run to the next, and needs no locking in the
//! guest's memory. It also means a thread that waits by spinning on memory, without calling the system, would hold the baton forever.

use crate::Ctx;
use crate::cafe::Os;
use crate::guest::Guest;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// What a system function that may have to wait says after one try.
pub enum Step {
    Done,
    /// Not yet: give the baton on and try again when it comes back.
    Wait,
    /// Done, but give the baton on first (`OSYieldThread`).
    Yield,
    /// Run these guest functions (each given one argument in r3) before returning to the caller, with the kernel unlocked: a system
    /// function that has to call back into the game (a callback registered with it) does this.
    Calls(Vec<(u32, u32)>),
    /// The thread has ended.
    Exit,
    /// The game asks to end the whole run (`exit`), or something went wrong that cannot go on.
    Stop(String),
}

pub type KernelHandler = fn(&mut Kernel, &mut Ctx) -> Step;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Life {
    /// Made and not started (or suspended).
    Suspended,
    Ready,
    Done,
}

pub struct Thread {
    /// Where the game keeps this thread's `OSThread`.
    pub addr: u32,
    pub life: Life,
    pub suspend: u32,
    pub exit_code: u32,
    /// When a sleep ends.
    pub sleep_until: Option<Instant>,
    pub entry: u32,
    pub argc: u32,
    pub argv: u32,
    pub stack_top: u32,
    pub started: bool,
    /// The last system function this thread called, and from where (for finding out what a thread is stuck on).
    pub last: Option<(u32, u32, u32)>,
}

#[derive(Default)]
pub struct MutexState {
    pub owner: Option<usize>,
    pub count: u32,
}

pub struct Queue {
    pub capacity: u32,
    pub items: VecDeque<[u32; 4]>,
}

/// Everything the system functions share.
pub struct Kernel {
    pub os: Os,
    pub fs: crate::fs::Fs,
    pub backend: Option<Box<dyn crate::Gx2Backend>>,
    pub guest: Guest,
    pub threads: Vec<Thread>,
    pub by_addr: HashMap<u32, usize>,
    /// The thread that holds the baton.
    pub current: usize,
    pub mutexes: HashMap<u32, MutexState>,
    /// Events: whether set, and whether they reset themselves when a thread passes.
    pub events: HashMap<u32, (bool, bool)>,
    pub semaphores: HashMap<u32, i32>,
    pub queues: HashMap<u32, Queue>,
    /// Functions found by name at run time (`OSDynLoad_FindExport`): a call to `DYNAMIC_BASE + 4 * n` is a call to the n-th of these.
    pub dynamic: Vec<String>,
    pub started: std::time::Instant,
    /// MH3U_GFX_TIMES: the time spent in each graphics call, reported every ten seconds
    pub gfx_times: bool,
    pub times: CallTimes,
    /// The callbacks registered with `ProcUIRegisterCallback`: (type, function, parameter, priority), and whether the first foreground
    /// acquisition has been delivered.
    pub procui_callbacks: Vec<(u32, u32, u32, u32)>,
    pub procui_acquired: bool,
    /// The audio frame callbacks (`AXRegisterFrameCallback`), run every audio frame by a service thread made when the first is added.
    pub ax_callbacks: Vec<u32>,
    /// The alarms that are set (by the address of the game's `OSAlarm`), and whether the thread that runs their handlers exists.
    pub alarms: BTreeMap<u32, Alarm>,
    pub alarm_thread: bool,
    pub alarm_user_data: BTreeMap<u32, u32>,
    /// How the translated library is called, for the threads made later.
    pub call: unsafe extern "C" fn(*mut Ctx, u32) -> i32,
    pub memory: *mut u8,
    pub report: Reporting,
    /// How many times in a row a thread had to wait without anything getting done.
    spins: u64,
    stuck_since: Option<Instant>,
}

// SAFETY: the raw memory pointer is only used by the thread that holds the baton, and the kernel is behind a mutex
unsafe impl Send for Kernel {}

/// What the run records for its report.
/// Kept on every system call, so it is all plain arrays indexed by import number (no map look-ups or allocations per call).
pub struct Reporting {
    pub names: Vec<String>,
    pub handlers: Vec<Option<crate::cafe::Handler>>,
    /// The imports in the order they were first called.
    pub first: Vec<u32>,
    /// Calls of each import, and the arguments (r3 to r6) of its first call.
    pub counts: Vec<u64>,
    pub first_args: Vec<[u32; 4]>,
    pub recent: Recent,
    pub calls: u64,
    pub max_imports: u64,
    /// Print every system call with its result (set MH3U_TRACE).
    pub trace: bool,
    /// MH3U_TRACE_FROM=<seconds>: print them from that long after the start (read once; infinite when not set).
    pub trace_from: f64,
}

impl Reporting {
    pub fn new(handlers: Vec<Option<crate::cafe::Handler>>, names: Vec<String>, max_imports: u64) -> Reporting {
        let n = names.len();
        Reporting {
            names,
            handlers,
            first: Vec::new(),
            counts: vec![0; n],
            first_args: vec![[0; 4]; n],
            recent: Recent::default(),
            calls: 0,
            max_imports,
            trace: std::env::var_os("MH3U_TRACE").is_some(),
            trace_from: std::env::var("MH3U_TRACE_FROM")
                .ok()
                .and_then(|v| v.trim().parse::<f64>().ok())
                .unwrap_or(f64::INFINITY),
        }
    }

    /// Notes a call of import `number` with these arguments (r3 to r6), from `lr`.
    pub fn note(&mut self, number: u32, args: [u32; 4], lr: u32) {
        self.calls += 1;
        let n = number as usize;
        if n >= self.counts.len() {
            self.counts.resize(n + 1, 0);
            self.first_args.resize(n + 1, [0; 4]);
        }
        if self.counts[n] == 0 {
            self.first.push(number);
            self.first_args[n] = args;
        }
        self.counts[n] += 1;
        self.recent.push((number, args, lr));
    }
}

/// The last twelve system calls (import number, arguments, return address), in a fixed ring.
#[derive(Default)]
pub struct Recent {
    items: [(u32, [u32; 4], u32); 12],
    next: usize,
    len: usize,
}

impl Recent {
    pub fn push(&mut self, item: (u32, [u32; 4], u32)) {
        self.items[self.next] = item;
        self.next = (self.next + 1) % self.items.len();
        self.len = (self.len + 1).min(self.items.len());
    }

    /// Oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &(u32, [u32; 4], u32)> {
        let n = self.items.len();
        (0..self.len).map(move |k| &self.items[(self.next + n - self.len + k) % n])
    }
}

/// Where the calls to functions found by name at run time go (no code is there).
pub const DYNAMIC_BASE: u32 = 0x0330_0000;

pub static KERNEL: Mutex<Option<Kernel>> = Mutex::new(None);
pub static BATON: Condvar = Condvar::new();

pub fn lock() -> MutexGuard<'static, Option<Kernel>> {
    KERNEL.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Gives the baton to guest thread `next` (the caller holds the lock and the baton).
fn hand_to(k: &mut Kernel, next: usize) {
    k.current = next;
    BATON.notify_all();
}

/// Waits, with the lock held on return, until guest thread `me` holds the baton.
pub fn wait_for_baton(mut guard: MutexGuard<'static, Option<Kernel>>, me: usize) -> MutexGuard<'static, Option<Kernel>> {
    while guard.as_ref().expect("kernel").current != me {
        guard = BATON.wait(guard).unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    guard
}

/// Something got done: a system function finished, so the threads that wait are not stuck.
pub fn progress(k: &mut Kernel) {
    k.spins = 0;
    k.stuck_since = None;
}

impl Kernel {
    pub fn new(
        os: Os,
        guest: Guest,
        threads: Vec<Thread>,
        by_addr: HashMap<u32, usize>,
        call: unsafe extern "C" fn(*mut Ctx, u32) -> i32,
        memory: *mut u8,
        report: Reporting,
    ) -> Kernel {
        Kernel {
            os,
            fs: crate::fs::Fs::default(),
            backend: None,
            guest,
            threads,
            by_addr,
            current: 0,
            mutexes: HashMap::new(),
            events: HashMap::new(),
            semaphores: HashMap::new(),
            queues: HashMap::new(),
            dynamic: Vec::new(),
            started: std::time::Instant::now(),
            gfx_times: std::env::var_os("MH3U_GFX_TIMES").is_some(),
            times: CallTimes::default(),
            procui_callbacks: Vec::new(),
            procui_acquired: false,
            ax_callbacks: Vec::new(),
            alarms: BTreeMap::new(),
            alarm_thread: false,
            alarm_user_data: BTreeMap::new(),
            call,
            memory,
            report,
            spins: 0,
            stuck_since: None,
        }
    }

    /// The next thread to run after `from`: a ready one, the ones after it first.
    fn next_after(&self, from: usize) -> usize {
        let n = self.threads.len();
        (1..=n)
            .map(|k| (from + k) % n)
            .find(|&t| self.threads[t].life == Life::Ready)
            .unwrap_or(from)
    }

    pub fn finish_thread(&mut self, code: u32) {
        let me = self.current;
        self.threads[me].life = Life::Done;
        self.threads[me].exit_code = code;
    }
}

/// Gives the baton to the next ready thread and waits until it comes back. Returns false when the thread is not ready any more
/// (it has ended), so it must not run guest code again.
pub fn pass_baton(mut guard: MutexGuard<'static, Option<Kernel>>, waiting: bool) -> MutexGuard<'static, Option<Kernel>> {
    let k = guard.as_mut().expect("the kernel is set while the game runs");
    let me = k.current;
    if waiting {
        k.spins += 1;
        k.stuck_since.get_or_insert_with(Instant::now);
    } else {
        k.spins = 0;
        k.stuck_since = None;
    }
    let next = k.next_after(me);
    hand_to(k, next);
    // when nothing else can run and this thread only waits for time to pass, do not spin the core
    let idle = next == me && waiting;
    if idle || (k.spins > 4 * k.threads.len() as u64 + 8 && waiting) {
        let stuck = k.stuck_since.is_some_and(|t| t.elapsed() > Duration::from_secs(20));
        if stuck && k.threads.iter().all(|t| t.sleep_until.is_none()) {
            drop(guard);
            crate::game::stop_unlocked("every thread is waiting and nothing can wake them (a deadlock)".into());
        }
        drop(guard);
        std::thread::sleep(Duration::from_micros(200));
        guard = lock();
    }
    wait_for_baton(guard, me)
}

/// Time spent in the graphics calls, by name.
#[derive(Default)]
pub struct CallTimes {
    /// Time and calls by import number (no allocation per call).
    by_import: Vec<(Duration, u64)>,
    since: Option<(Instant, u64)>,
}

impl CallTimes {
    /// Adds a call of import `n`; every report window (MH3U_TIMES_WINDOW, ten seconds by default) the busiest are printed, with their
    /// time per frame, using `names` (import number to name).
    pub fn record(&mut self, n: u32, took: Duration, names: &[String]) {
        let (since, frames_at) = *self
            .since
            .get_or_insert_with(|| (Instant::now(), crate::FRAMES.load(std::sync::atomic::Ordering::Relaxed)));
        let n = n as usize;
        if self.by_import.len() <= n {
            self.by_import.resize(n + 1, (Duration::ZERO, 0));
        }
        let e = &mut self.by_import[n];
        e.0 += took;
        e.1 += 1;
        if since.elapsed() >= crate::times_window() {
            let wall = since.elapsed().as_secs_f64();
            let frames = crate::frames_since(frames_at);
            let mut v: Vec<_> = self.by_import.iter().enumerate().filter(|(_, (_, c))| *c > 0).collect();
            v.sort_by_key(|(_, (d, _))| std::cmp::Reverse(*d));
            let total: f64 = v.iter().map(|(_, (d, _))| d.as_secs_f64()).sum();
            eprintln!(
                "[gfx times] {wall:.1} s, {frames} frames ({:.1} fps): {total:.1} s in graphics calls, {:.2} ms per frame",
                frames as f64 / wall,
                total * 1000.0 / frames as f64
            );
            for (i, (d, c)) in v.iter().take(8) {
                let name = names.get(*i).map_or("?", String::as_str);
                eprintln!(
                    "[gfx times]   {:>7.3} s {c:>8} calls  {:>7.3} ms {:>7.1} calls per frame  {name}",
                    d.as_secs_f64(),
                    d.as_secs_f64() * 1000.0 / frames as f64,
                    *c as f64 / frames as f64
                );
            }
            self.by_import.iter_mut().for_each(|e| *e = (Duration::ZERO, 0));
            self.since = None;
        }
    }
}

/// A timer the game set: it calls `handler(alarm, context)` when `due` has come, and again every `period` if it has one.
#[derive(Debug, Clone)]
pub struct Alarm {
    pub due: Instant,
    pub period: Option<Duration>,
    pub handler: u32,
    pub tag: u32,
}

/// Sleeps the calling thread for `duration`, letting the others run meanwhile.
pub fn sleep_current(mut guard: MutexGuard<'static, Option<Kernel>>, duration: Duration) -> MutexGuard<'static, Option<Kernel>> {
    let until = Instant::now() + duration;
    let me = guard.as_ref().expect("kernel").current;
    guard.as_mut().expect("kernel").threads[me].sleep_until = Some(until);
    while Instant::now() < until {
        guard = pass_baton(guard, true);
    }
    guard.as_mut().expect("kernel").threads[me].sleep_until = None;
    guard
}

/// Ends the calling thread for good: it passes the baton and never runs guest code again.
pub fn retire(mut guard: MutexGuard<'static, Option<Kernel>>) -> ! {
    let k = guard.as_mut().expect("kernel");
    let me = k.current;
    let alive = k.threads.iter().any(|t| t.life == Life::Ready);
    if !alive {
        drop(guard);
        crate::game::stop_unlocked("every thread has ended".into());
    }
    let next = k.next_after(me);
    hand_to(k, next);
    loop {
        guard = BATON.wait(guard).unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

#[cfg(test)]
mod reporting_tests {
    use super::*;

    #[test]
    fn recent_keeps_the_last_twelve_oldest_first() {
        let mut r = Recent::default();
        assert_eq!(r.iter().count(), 0);
        for n in 0..5 {
            r.push((n, [n; 4], n));
        }
        assert_eq!(r.iter().map(|e| e.0).collect::<Vec<_>>(), [0, 1, 2, 3, 4]);
        for n in 5..30 {
            r.push((n, [n; 4], n));
        }
        assert_eq!(r.iter().map(|e| e.0).collect::<Vec<_>>(), (18..30).collect::<Vec<_>>());
    }

    #[test]
    fn counts_and_first_calls() {
        let mut r = Reporting::new(vec![None; 3], vec!["a".into(), "b".into(), "c".into()], 100);
        r.note(2, [1, 2, 3, 4], 0x10);
        r.note(0, [5, 6, 7, 8], 0x20);
        r.note(2, [9, 9, 9, 9], 0x30);
        assert_eq!(r.first, [2, 0]);
        assert_eq!(r.counts, [1, 0, 2]);
        assert_eq!(r.first_args[2], [1, 2, 3, 4]);
        assert_eq!(r.calls, 3);
        // a number past the names grows the arrays instead of failing
        r.note(5, [0; 4], 0);
        assert_eq!(r.counts[5], 1);
    }
}

#[cfg(test)]
mod baton_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn thread() -> Thread {
        Thread {
            addr: 0,
            life: Life::Ready,
            suspend: 0,
            exit_code: 0,
            sleep_until: None,
            entry: 0,
            argc: 0,
            argv: 0,
            stack_top: 0,
            started: true,
            last: None,
        }
    }

    unsafe extern "C" fn no_call(_: *mut Ctx, _: u32) -> i32 {
        0
    }

    fn env(name: &str, default: u64) -> u64 {
        std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    /// The cost of passing the baton between live guest threads when many others have ended (a thread that returns from its entry
    /// point, or calls `OSExitThread`, is retired). The only test that sets the kernel: its threads are left blocked when it ends.
    /// `MH3U_BATON_RETIRED` (50), `MH3U_BATON_LIVE` (8) and `MH3U_BATON_PASSES` (20000) set its size; run with `--nocapture` to see the
    /// time per pass.
    #[test]
    fn baton_goes_round_the_live_threads_in_order() {
        let retired = env("MH3U_BATON_RETIRED", 50) as usize;
        let live = env("MH3U_BATON_LIVE", 8).max(1) as usize;
        let passes = env("MH3U_BATON_PASSES", 20_000).max(64);
        let mut memory = crate::memory::GuestMemory::reserve().expect("guest memory");
        let mem = memory.as_mut_ptr();
        let guest = Guest::new(mem);
        let os = Os::new(&guest);
        let n = retired + live;
        let threads = (0..n).map(|_| thread()).collect();
        *lock() = Some(Kernel::new(
            os,
            guest,
            threads,
            HashMap::new(),
            no_call,
            mem,
            Reporting::new(Vec::new(), Vec::new(), u64::MAX),
        ));
        // the kernel points into it, and the threads outlive the test
        std::mem::forget(memory);
        for t in 0..retired {
            std::thread::spawn(move || {
                let mut guard = wait_for_baton(lock(), t);
                guard.as_mut().expect("kernel").finish_thread(0);
                retire(guard)
            });
        }
        let count = Arc::new(AtomicU64::new(0));
        let order = Arc::new(Mutex::new(Vec::new()));
        let start = Arc::new(Mutex::new(None::<Instant>));
        let (done, finished) = std::sync::mpsc::channel();
        for t in retired..n {
            let (count, order, start, done) = (count.clone(), order.clone(), start.clone(), done.clone());
            std::thread::spawn(move || {
                let mut guard = wait_for_baton(lock(), t);
                loop {
                    // the kernel lock is held here, so the counting is in baton order
                    let c = count.fetch_add(1, Ordering::Relaxed) + 1;
                    if c == 1 {
                        *start.lock().unwrap() = Some(Instant::now());
                    }
                    if c <= 64 {
                        order.lock().unwrap().push(t);
                    }
                    if c == passes {
                        let took = start.lock().unwrap().expect("started").elapsed();
                        done.send(took).unwrap();
                        drop(guard);
                        loop {
                            std::thread::park();
                        }
                    }
                    guard = pass_baton(guard, false);
                }
            });
        }
        let took = finished.recv_timeout(Duration::from_secs(600)).expect("the baton went round");
        let per = took / u32::try_from(passes - 1).unwrap();
        eprintln!("[baton] {retired} retired, {live} live threads: {passes} passes in {took:.2?}, {per:.2?} per pass");
        let expected: Vec<usize> = (0..64).map(|k| retired + k % live).collect();
        assert_eq!(*order.lock().unwrap(), expected);
    }
}
