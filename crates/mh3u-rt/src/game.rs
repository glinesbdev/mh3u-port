//! Loading the translated library, putting the executable in memory and running the entry point.

use crate::cafe::{self, Handler, Os};
use crate::guest::Guest;
use crate::kernel::{self, Kernel, Life, Reporting, Step, Thread, lock};
use crate::{Ctx, Piece};
use anyhow::{Context, Result, bail, ensure};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;

/// The calls the translated library makes back into the runtime (`Host` in `ppc.h`).
#[repr(C)]
struct Host {
    import: extern "C" fn(*mut Ctx, u32),
    unknown_call: extern "C" fn(*mut Ctx, u32),
    trap: extern "C" fn(*mut Ctx, u32),
    unsupported: extern "C" fn(*mut Ctx, u32),
    syscall: extern "C" fn(*mut Ctx),
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
                let a = r.first_args.get(&n).copied().unwrap_or_default();
                let stub = if r.handlers[n as usize].is_none() { "  (stub)" } else { "" };
                (
                    format!("{}({:#x}, {:#x}, {:#x}, {:#x}){stub}", r.names[n as usize], a[0], a[1], a[2], a[3]),
                    r.counts.get(&n).copied().unwrap_or(0),
                )
            })
            .collect(),
        end: end.to_string(),
    }
}

fn print_report(k: &Kernel, why: &str) {
    println!("the game stopped: {why}");
    println!("the last system calls (arguments r3 to r6):");
    for (n, a, lr) in &k.report.recent {
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
    let mut guard = lock();
    {
        let k = guard.as_mut().expect("the kernel is set while the game runs");
        let r = &mut k.report;
        r.calls += 1;
        if !r.counts.contains_key(&number) {
            r.first.push(number);
            r.first_args.insert(number, [c.r[3], c.r[4], c.r[5], c.r[6]]);
        }
        *r.counts.entry(number).or_default() += 1;
        if r.recent.len() == 12 {
            r.recent.pop_front();
        }
        r.recent.push_back((number, [c.r[3], c.r[4], c.r[5], c.r[6]], c.lr));
        if r.calls > r.max_imports {
            drop(guard);
            stop(c, "too many system calls".into());
        }
    }
    let args = [c.r[3], c.r[4], c.r[5], c.r[6]];
    loop {
        let k = guard.as_mut().expect("the kernel is set while the game runs");
        let step = match k.report.handlers[number as usize] {
            Some(Handler::Kernel(h)) => h(k, c),
            Some(Handler::Gx2) => {
                let guest = k.guest;
                if let Some(b) = k.backend.as_mut() {
                    b.call(&k.report.names[number as usize], c, &guest);
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
                if k.report.trace {
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
            if k.report.trace {
                eprintln!("[{}] (dynamic) {name}", k.current);
            }
            // SAFETY: `c` is the context the translated code passed us
            unsafe { (*c).r[3] = 0 };
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

/// Runs the game's entry point from the translated library at `library`, with `pieces` put in the guest's memory first.
pub fn run(library: &Path, pieces: &[Piece], options: Options) -> Result<Report> {
    let mut options_roots = crate::fs::Roots::default();
    let mut options = options;
    let backend = options.backend.take();
    std::mem::swap(&mut options_roots, &mut options.roots);
    let options = &options;
    let mut memory = memmap2::MmapOptions::new()
        .len(1 << 32)
        .map_anon()
        .context("reserving the guest's address space")?;
    for p in pieces {
        let at = p.address as usize;
        let end = at
            .checked_add(p.bytes.len())
            .filter(|&e| e as u64 <= 1 << 32)
            .context("a piece outside the address space")?;
        memory[at..end].copy_from_slice(&p.bytes);
        // the rest is zeros already (a fresh mapping), `zero` is only the length that stays so
        ensure!(end as u64 + u64::from(p.zero) <= 1 << 32, "a piece outside the address space");
    }
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
    // the threads made later call into the library after this function's frame is gone, so it stays loaded
    std::mem::forget(lib);
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
        Reporting {
            handlers: names
                .iter()
                .map(|n| cafe::find(n).or_else(|| backend.as_ref().filter(|b| b.implements(n)).map(|_| Handler::Gx2)))
                .collect(),
            names,
            first: Vec::new(),
            counts: BTreeMap::new(),
            first_args: BTreeMap::new(),
            recent: VecDeque::new(),
            calls: 0,
            max_imports: options.max_imports,
            trace: std::env::var_os("MH3U_TRACE").is_some(),
        },
    ));
    if let Some(k) = lock().as_mut() {
        k.fs.roots = roots;
        k.backend = backend;
    }
    let host = Host {
        import,
        unknown_call,
        trap,
        unsupported,
        syscall,
    };
    // `host` is copied by the library
    // SAFETY: the library reads the struct during the call and keeps a copy
    unsafe { set_host(&host) };
    ensure!(
        options.stack_size > 0x1000 && options.stack_top >= options.stack_size,
        "the stack does not fit"
    );
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
