//! Loading the translated library, putting the executable in memory and running the entry point.

use crate::cafe::{self, Handler, Os};
use crate::guest::Guest;
use crate::kernel::{self, Kernel, Life, Reporting, Step, Thread, lock};
use crate::{Ctx, Piece};
use anyhow::{Context, Result, bail, ensure};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// The calls the translated library makes back into the runtime (`Host` in `ppc.h`).
#[repr(C)]
struct Host {
    import: extern "C" fn(*mut Ctx, u32),
    unknown_call: extern "C" fn(*mut Ctx, u32),
    trap: extern "C" fn(*mut Ctx, u32),
    unsupported: extern "C" fn(*mut Ctx, u32),
    syscall: extern "C" fn(*mut Ctx),
    enter: extern "C" fn(*mut Ctx, u32),
    watch: extern "C" fn(*mut Ctx, u32, u64, u32),
}

/// A variable the loader fills in for the executable (data imported from a system library).
pub struct DataImport {
    /// Where the variable is in the guest's memory.
    pub slot: u32,
    /// The address a call through the variable goes to; the translated library treats it as the import's own entry.
    pub target: u32,
    pub name: String,
}

pub struct Options {
    /// Draws for the game; without one the `gx2` calls without code of their own do nothing.
    pub backend: Option<Box<dyn crate::Gx2Backend>>,
    /// Where the game's files are.
    pub roots: crate::fs::Roots,
    pub data_imports: Vec<DataImport>,
    /// Where the entry point is.
    pub entry: u32,
    /// How many calls of system functions to allow before stopping (a game that loops on a function that does nothing never ends).
    pub max_imports: u64,
    /// The stack: its top and its size.
    pub stack_top: u32,
    pub stack_size: u32,
}

/// What a run did.
#[derive(Debug)]
pub struct Report {
    /// The system functions called, in the order they were first called, with how many times each.
    pub imports: Vec<(String, u64)>,
    /// Why the run stopped.
    pub end: String,
}

fn report_of(k: &Kernel, end: &str) -> Report {
    let r = &k.report;
    Report {
        imports: r
            .first
            .iter()
            .map(|&n| {
                let a = r.first_args.get(n as usize).copied().unwrap_or_default();
                let stub = if r.handlers[n as usize].is_none() { "  (stub)" } else { "" };
                (
                    format!("{}({:#x}, {:#x}, {:#x}, {:#x}){stub}", r.names[n as usize], a[0], a[1], a[2], a[3]),
                    r.counts.get(n as usize).copied().unwrap_or(0),
                )
            })
            .collect(),
        end: end.to_string(),
    }
}

fn print_report(k: &Kernel, why: &str) {
    println!("the game stopped: {why}");
    println!("the last system calls (arguments r3 to r6):");
    for (n, a, lr) in k.report.recent.iter() {
        println!(
            "  {}({:#x}, {:#x}, {:#x}, {:#x})  from {lr:#x}",
            k.report.names[*n as usize], a[0], a[1], a[2], a[3]
        );
    }
    let report = report_of(k, why);
    println!("{} system functions called, in order of first call:", report.imports.len());
    for (name, n) in &report.imports {
        println!("  {n:>8}  {name}");
    }
}

/// Prints what happened and ends the process. Must not be called with the kernel locked.
pub fn stop_unlocked(why: String) -> ! {
    {
        let guard = lock();
        if let Some(k) = guard.as_ref() {
            print_report(k, &why);
        }
    }
    std::process::exit(2);
}

fn stop(c: *mut Ctx, why: String) -> ! {
    // SAFETY: `c` is the context the translated code passed us; it is valid for the call
    let regs = unsafe { (*c).r };
    let line = regs
        .iter()
        .enumerate()
        .map(|(i, v)| format!("r{i}={v:#x}"))
        .collect::<Vec<_>>()
        .join(" ");
    // SAFETY: as above
    let (lr, ctr) = unsafe { ((*c).lr, (*c).ctr) };
    println!("registers: {line}\nlr={lr:#x} ctr={ctr:#x}");
    stop_unlocked(why)
}

extern "C" fn import(c: *mut Ctx, number: u32) {
    // SAFETY: `c` is the context the translated code passed us; it is valid for the call, and nothing else uses it meanwhile
    let c = unsafe { &mut *c };
    // The kernel lock is taken on every call even though only the thread holding the baton runs guest code: the host's own threads (the
    // alarm and audio service threads, the dump timer, the window's input) take it too, and without contention it costs a few atomic
    // operations, so skipping it would buy little for a soundness argument this code does not want to depend on.
    let mut guard = lock();
    {
        let k = guard.as_mut().expect("the kernel is set while the game runs");
        let r = &mut k.report;
        r.note(number, [c.r[3], c.r[4], c.r[5], c.r[6]], c.lr);
        let (calls, max) = (r.calls, r.max_imports);
        let me = k.current;
        k.threads[me].last = Some((number, c.lr, c.r[1]));
        if calls > max {
            drop(guard);
            stop(c, "too many system calls".into());
        }
    }
    let args = [c.r[3], c.r[4], c.r[5], c.r[6]];
    loop {
        let k = guard.as_mut().expect("the kernel is set while the game runs");
        let step = match k.report.handlers[number as usize] {
            Some(Handler::Kernel(h)) => h(k, c),
            // by the number the backend gave the import when the game was loaded (no name matching per call)
            Some(Handler::Gx2(id)) => {
                let guest = k.guest;
                if let Some(b) = k.backend.as_mut() {
                    let name = &k.report.names[number as usize];
                    if k.gfx_times {
                        let t = std::time::Instant::now();
                        b.call_id(id, name, c, &guest);
                        k.times.record(number, t.elapsed(), &k.report.names);
                    } else {
                        b.call_id(id, name, c, &guest);
                    }
                }
                Step::Done
            }
            Some(Handler::Simple(h)) => {
                let guest = k.guest;
                h(&mut k.os, &guest, c);
                Step::Done
            }
            // a function that is not written yet does nothing and returns 0
            None => {
                c.r[3] = 0;
                Step::Done
            }
        };
        match step {
            Step::Done => {
                // MH3U_TRACE_FROM=<seconds>: trace only after that long (read once when the game was loaded)
                let from = k.report.trace_from;
                if k.report.trace || (from.is_finite() && k.started.elapsed().as_secs_f64() >= from) {
                    eprintln!(
                        "[{}] {}({:#x}, {:#x}, {:#x}, {:#x}) = {:#x}  from {:#x}",
                        k.current, k.report.names[number as usize], args[0], args[1], args[2], args[3], c.r[3], c.lr
                    );
                }
                kernel::progress(k);
                return;
            }
            Step::Wait => guard = kernel::pass_baton(guard, true),
            Step::Yield => {
                drop(kernel::pass_baton(guard, false));
                return;
            }
            Step::Calls(calls) => {
                let (call, mem) = (k.call, k.memory);
                drop(guard);
                let mut result = 0;
                for (target, param) in calls {
                    let mut inner = c.clone();
                    inner.mem = mem;
                    inner.r[1] = c.r[1].wrapping_sub(0x400);
                    inner.r[3] = param;
                    inner.lr = 0;
                    // SAFETY: the library is loaded for the rest of the run
                    unsafe { call(&mut inner, target) };
                    result = inner.r[3];
                }
                let _ = result;
                guard = lock();
                kernel::progress(guard.as_mut().expect("kernel"));
                return;
            }
            Step::Exit => kernel::retire(guard),
            Step::Stop(why) => {
                drop(guard);
                stop(c, why);
            }
        }
    }
}

extern "C" fn unknown_call(c: *mut Ctx, target: u32) {
    if (kernel::DYNAMIC_BASE..kernel::DYNAMIC_BASE + 0x1_0000).contains(&target) {
        let n = ((target - kernel::DYNAMIC_BASE) / 4) as usize;
        let mut guard = lock();
        let k = guard.as_mut().expect("the kernel is set while the game runs");
        if let Some(name) = k.dynamic.get(n) {
            if k.report.trace || std::env::var_os("MH3U_TRACE_DYNAMIC").is_some() {
                // SAFETY: as below
                let (r, lr) = unsafe { ((*c).r, (*c).lr) };
                eprintln!(
                    "[{}] (dynamic) {name} ({:#x}, {:#x}, {:#x}) from {lr:#x}",
                    k.current, r[3], r[4], r[5]
                );
            }
            // SAFETY: `c` is the context the translated code passed us
            unsafe { (*c).r[3] = u32::from(name.contains("Create")) };
            return;
        }
    }
    stop(c, format!("a call to {target:#x}, which is not a translated function"));
}

extern "C" fn trap(c: *mut Ctx, word: u32) {
    stop(c, format!("a trap instruction {word:#010x}"));
}

extern "C" fn unsupported(c: *mut Ctx, word: u32) {
    stop(c, format!("an instruction that was not translated: {word:#010x}"));
}

extern "C" fn syscall(c: *mut Ctx) {
    stop(c, "a system call instruction".into());
}

extern "C" fn enter(c: *mut Ctx, function: u32) {
    // SAFETY: `c` is the context the translated code passed us
    let (lr, r) = unsafe { ((*c).lr, (*c).r) };
    let thread = lock().as_ref().map_or(99, |k| k.current);
    eprintln!(
        "[enter] thread {thread}: {function:#x} from {lr:#x} (r3 {:#x}, r4 {:#x}, r5 {:#x}, r6 {:#x})",
        r[3], r[4], r[5], r[6]
    );
}

extern "C" fn watch(c: *mut Ctx, address: u32, value: u64, size: u32) {
    // SAFETY: `c` is the context the translated code passed us
    let (lr, r1) = unsafe { ((*c).lr, (*c).r[1]) };
    let thread = lock().as_ref().map_or(99, |k| k.current);
    eprintln!("[watch] thread {thread}: {size} bytes at {address:#x} <- {value:#x}   (lr {lr:#x}, sp {r1:#x})");
}

fn new_ctx(mem: *mut u8) -> Ctx {
    Ctx {
        r: [0; 32],
        f: [0; 32],
        ps1: [0; 32],
        cr: 0,
        xer: 0,
        lr: 0,
        ctr: 0,
        fpscr: 0,
        gqr: [0; 8],
        reservation: 0,
        has_reservation: 0,
        tb: 0,
        mem,
    }
}

/// Starts the host thread for guest thread `t`; it waits for the baton before it runs any guest code.
pub fn spawn(k: &Kernel, t: usize) {
    let (call, mem) = (k.call, k.memory as usize);
    let th = &k.threads[t];
    let (entry, argc, argv, top) = (th.entry, th.argc, th.argv, th.stack_top);
    std::thread::spawn(move || {
        let mut guard = lock();
        while guard.as_ref().expect("kernel").current != t {
            guard = kernel::BATON.wait(guard).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        drop(guard);
        let mut c = new_ctx(mem as *mut u8);
        c.r[1] = top.wrapping_sub(0x20);
        c.r[3] = argc;
        c.r[4] = argv;
        // SAFETY: the library is loaded for the rest of the run and the context's memory is the guest's address space
        unsafe { call(&mut c, entry) };
        let mut guard = lock();
        guard.as_mut().expect("kernel").finish_thread(c.r[3]);
        kernel::retire(guard)
    });
}

/// Starts the thread that runs the audio frame callbacks every audio frame (3 ms of sound). It is a guest thread like the others: it
/// holds the baton while it runs the game's code.
pub fn spawn_audio(k: &mut Kernel) {
    let t = k.threads.len();
    k.threads.push(Thread {
        addr: 0xffff_fff0,
        life: Life::Ready,
        suspend: 0,
        exit_code: 0,
        sleep_until: None,
        entry: 0,
        argc: 0,
        argv: 0,
        stack_top: 0xa0f0_0000,
        started: true,
        last: None,
    });
    let (call, mem) = (k.call, k.memory as usize);
    std::thread::spawn(move || {
        let mut guard = lock();
        while guard.as_ref().expect("kernel").current != t {
            guard = kernel::BATON.wait(guard).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        loop {
            guard = kernel::sleep_current(guard, std::time::Duration::from_millis(3));
            let callbacks = guard.as_ref().expect("kernel").ax_callbacks.clone();
            drop(guard);
            let mut c = new_ctx(mem as *mut u8);
            c.r[1] = 0xa0f0_0000 - 0x20;
            for cb in callbacks {
                // SAFETY: the library is loaded for the rest of the run
                unsafe { call(&mut c, cb) };
            }
            guard = lock();
        }
    });
}

/// Starts the thread that runs the handlers of the game's alarms when they are due.
pub fn spawn_alarms(k: &mut Kernel) {
    let t = k.threads.len();
    k.threads.push(Thread {
        addr: 0xffff_ffe0,
        life: Life::Ready,
        suspend: 0,
        exit_code: 0,
        sleep_until: None,
        entry: 0,
        argc: 0,
        argv: 0,
        stack_top: 0xa0e0_0000,
        started: true,
        last: None,
    });
    let (call, mem) = (k.call, k.memory as usize);
    std::thread::spawn(move || {
        let mut guard = lock();
        while guard.as_ref().expect("kernel").current != t {
            guard = kernel::BATON.wait(guard).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        loop {
            guard = kernel::sleep_current(guard, std::time::Duration::from_millis(2));
            let now = std::time::Instant::now();
            let k = guard.as_mut().expect("kernel");
            let mut due = Vec::new();
            for (addr, alarm) in k.alarms.iter_mut() {
                if alarm.due <= now {
                    due.push((*addr, alarm.handler));
                    if let Some(period) = alarm.period {
                        alarm.due += period;
                    }
                }
            }
            k.alarms.retain(|_, a| a.period.is_some() || a.due > now);
            drop(guard);
            for (addr, handler) in due {
                let mut c = new_ctx(mem as *mut u8);
                c.r[1] = 0xa0e0_0000 - 0x20;
                c.r[3] = addr;
                // SAFETY: the library is loaded for the rest of the run
                unsafe { call(&mut c, handler) };
            }
            guard = lock();
        }
    });
}

/// Runs the game's entry point from the translated library at `library`, with `pieces` put in the guest's memory first.
pub fn run(library: &Path, pieces: &[Piece], options: Options) -> Result<Report> {
    let mut options_roots = crate::fs::Roots::default();
    let mut options = options;
    let backend = options.backend.take();
    std::mem::swap(&mut options_roots, &mut options.roots);
    let options = &options;
    let mut memory = crate::memory::GuestMemory::reserve()?;
    for p in pieces {
        let at = p.address as usize;
        let end = at
            .checked_add(p.bytes.len())
            .filter(|&e| e as u64 <= 1 << 32)
            .context("a piece outside the address space")?;
        Guest::new(memory.as_mut_ptr()).write(p.address, &p.bytes);
        // the rest is zeros already (a fresh mapping), `zero` is only the length that stays so
        ensure!(end as u64 + u64::from(p.zero) <= 1 << 32, "a piece outside the address space");
    }
    // a Windows build of the game uses the FMA instructions for the fused multiply-adds (see ppc.h)
    #[cfg(all(windows, target_arch = "x86_64"))]
    ensure!(
        std::arch::is_x86_feature_detected!("fma"),
        "this processor has no FMA instructions, which the Windows build of the game needs (any x86-64 processor since 2013 has them)"
    );
    // SAFETY: loading a library runs its initialisers; this one is built from the translated game and has none
    let lib = unsafe { libloading::Library::new(library) }.with_context(|| format!("loading {}", library.display()))?;
    // SAFETY: the symbols are the ones dispatch.c exports, with these signatures
    let (set_host, call, count, name, ctx_size) = unsafe {
        (
            *lib.get::<unsafe extern "C" fn(*const Host)>(b"game_set_host\0")?,
            *lib.get::<unsafe extern "C" fn(*mut Ctx, u32) -> i32>(b"game_call\0")?,
            *lib.get::<unsafe extern "C" fn() -> u32>(b"game_import_count\0")?,
            *lib.get::<unsafe extern "C" fn(u32) -> *const std::ffi::c_char>(b"game_import_name\0")?,
            *lib.get::<unsafe extern "C" fn() -> u32>(b"game_ctx_size\0")?,
        )
    };
    // SAFETY: plain calls of functions that read constants
    let (size, imports) = unsafe { (ctx_size(), count()) };
    ensure!(
        size as usize == size_of::<Ctx>(),
        "the library's context is {size} bytes, the runtime's is {}",
        size_of::<Ctx>()
    );
    let names: Vec<String> = (0..imports)
        .map(|n| {
            // SAFETY: `name` returns a pointer to a constant string for every number below the count
            let p = unsafe { name(n) };
            // SAFETY: as above, the string ends with a zero
            unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
        })
        .collect();
    let mem = memory.as_mut_ptr();
    let guest = Guest::new(mem);
    for d in &options.data_imports {
        // the variables that hold a function point at the import of that name; the others start at zero
        let value = if cafe::find(&d.name).is_some() { d.target } else { 0 };
        guest.put_u32(d.slot, value);
    }
    let os = Os::new(&guest);
    let main = Thread {
        addr: os.thread,
        life: Life::Ready,
        suspend: 0,
        exit_code: 0,
        sleep_until: None,
        entry: options.entry,
        argc: 0,
        argv: 0,
        stack_top: options.stack_top,
        started: true,
        last: None,
    };
    let by_addr = HashMap::from([(os.thread, 0)]);
    let roots = std::mem::take(&mut options_roots);
    *lock() = Some(Kernel::new(
        os,
        guest,
        vec![main],
        by_addr,
        call,
        mem,
        Reporting::new(
            names
                .iter()
                .map(|n| cafe::find(n).or_else(|| backend.as_ref().filter(|b| b.implements(n)).map(|b| Handler::Gx2(b.resolve(n)))))
                .collect(),
            names,
            options.max_imports,
        ),
    ));
    if let Some(k) = lock().as_mut() {
        k.fs.roots = roots;
        k.backend = backend;
    }
    if let Some(secs) = std::env::var("MH3U_DUMP_AFTER").ok().and_then(|v| v.parse::<u64>().ok()) {
        // a look at what every thread is doing, for finding what the game is waiting for
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(secs));
            let guard = lock();
            if let Some(k) = guard.as_ref() {
                for (i, t) in k.threads.iter().enumerate() {
                    let last = t.last.map(|(n, lr, sp)| {
                        // the saved link registers up the stack (the frame at `sp` holds its caller's frame, and the return address at +4)
                        let mut frames = vec![lr];
                        let mut frame = sp;
                        for _ in 0..14 {
                            let (back, ret) = (k.guest.u32(frame), k.guest.u32(frame.wrapping_add(4)));
                            if back <= frame || back < 0x1000_0000 || ret == 0 {
                                break;
                            }
                            frames.push(ret);
                            frame = back;
                        }
                        format!(
                            "{} from {}",
                            k.report.names[n as usize],
                            frames.iter().map(|f| format!("{f:#x}")).collect::<Vec<_>>().join(" < ")
                        )
                    });
                    println!(
                        "thread {i}: life {} suspend {} started {} addr {:#x} entry {:#x} current {} last {last:?}",
                        match t.life {
                            Life::Ready => "ready",
                            Life::Suspended => "suspended",
                            Life::Done => "done",
                        },
                        t.suspend,
                        t.started,
                        t.addr,
                        t.entry,
                        k.current == i
                    );
                }
            }
            if let Some(k) = guard.as_ref() {
                print_report(k, "the dump was asked for");
            }
            drop(guard);
            std::process::exit(0);
        });
    }
    let host = Host {
        import,
        unknown_call,
        trap,
        unsupported,
        syscall,
        enter,
        watch,
    };
    // `host` is copied by the library
    // SAFETY: the library reads the struct during the call and keeps a copy
    unsafe { set_host(&host) };
    let hex = |name: &str| {
        std::env::var(name)
            .ok()
            .and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok())
    };
    if hex("MH3U_ENTER").is_some() || hex("MH3U_ENTER_LR").is_some() {
        // report entries of this function, and entries of any function called with this link register (the return address of a call)
        // SAFETY: the symbol exists only in a library built with the watch, with this signature
        match unsafe { lib.get::<unsafe extern "C" fn(u32, u32)>(b"game_set_enter\0") } {
            // SAFETY: the symbol has this signature
            Ok(set) => unsafe { set(hex("MH3U_ENTER").unwrap_or(1), hex("MH3U_ENTER_LR").unwrap_or(1)) },
            Err(_) => eprintln!("MH3U_ENTER is set but the library was not built with --watch"),
        }
    }
    if let Some(secs) = std::env::var("MH3U_PROFILE").ok().and_then(|v| v.parse::<u64>().ok()) {
        // a sampling profile of the translated code: the function most recently entered, every millisecond (needs a --watch build)
        // SAFETY: the symbol exists only in a library built with the watch, with this signature
        if let Ok(f) = unsafe { lib.get::<unsafe extern "C" fn() -> u32>(b"game_last_function\0") } {
            let f = *f;
            std::thread::spawn(move || {
                let mut counts: BTreeMap<u32, u32> = BTreeMap::new();
                let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
                while std::time::Instant::now() < end {
                    // SAFETY: a plain read of a global
                    *counts.entry(unsafe { f() }).or_default() += 1;
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                let mut v: Vec<_> = counts.into_iter().collect();
                v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
                for (addr, n) in v.iter().take(25) {
                    println!("profile {n:>6}  {addr:#x}");
                }
                std::process::exit(0);
            });
        }
    }
    if let Ok(spec) = std::env::var("MH3U_WATCH") {
        // "address,length" in hex: report every store to that range (needs a library built with --watch)
        let mut parts = spec.split(',').map(|p| u32::from_str_radix(p.trim_start_matches("0x"), 16));
        if let (Some(Ok(lo)), Some(Ok(len))) = (parts.next(), parts.next()) {
            crate::guest::WATCH.0.store(lo, std::sync::atomic::Ordering::Relaxed);
            crate::guest::WATCH
                .1
                .store(lo.saturating_add(len), std::sync::atomic::Ordering::Relaxed);
            // SAFETY: the symbol exists only in a library built with the watch, with this signature
            match unsafe { lib.get::<unsafe extern "C" fn(u32, u32)>(b"game_set_watch\0") } {
                // SAFETY: the symbol has this signature
                Ok(f) => unsafe { f(lo, lo.saturating_add(len)) },
                Err(_) => eprintln!("MH3U_WATCH is set but the library was not built with --watch"),
            }
        }
    }
    ensure!(
        options.stack_size > 0x1000 && options.stack_top >= options.stack_size,
        "the stack does not fit"
    );
    // the threads made later call into the library after this function's frame is gone, so it stays loaded
    std::mem::forget(lib);
    let mut c = new_ctx(mem);
    c.r[1] = options.stack_top - 0x20;
    // SAFETY: the context is valid and its `mem` covers the whole 32-bit address space
    let found = unsafe { call(&mut c, options.entry) };
    if found == 0 {
        bail!("{:#x} is not the start of a translated function", options.entry);
    }
    let guard = lock();
    let k = guard.as_ref().expect("kernel");
    Ok(report_of(k, "the entry point returned"))
}
