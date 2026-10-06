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
pub struct Reporting {
    pub names: Vec<String>,
    pub handlers: Vec<Option<crate::cafe::Handler>>,
    pub first: Vec<u32>,
    pub counts: BTreeMap<u32, u64>,
    pub first_args: BTreeMap<u32, [u32; 4]>,
    pub recent: VecDeque<(u32, [u32; 4], u32)>,
    pub calls: u64,
    pub max_imports: u64,
    /// Print every system call with its result (set MH3U_TRACE).
    pub trace: bool,
}

pub static KERNEL: Mutex<Option<Kernel>> = Mutex::new(None);
pub static BATON: Condvar = Condvar::new();

pub fn lock() -> MutexGuard<'static, Option<Kernel>> {
    KERNEL.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
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
            guest,
            threads,
            by_addr,
            current: 0,
            mutexes: HashMap::new(),
            events: HashMap::new(),
            semaphores: HashMap::new(),
            queues: HashMap::new(),
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
    k.current = next;
    BATON.notify_all();
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
    while guard.as_ref().expect("kernel").current != me {
        guard = BATON.wait(guard).unwrap_or_else(std::sync::PoisonError::into_inner);
    }
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
    k.current = k.next_after(me);
    BATON.notify_all();
    loop {
        guard = BATON.wait(guard).unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}
