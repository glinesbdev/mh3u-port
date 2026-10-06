//! Loading the translated library, putting the executable in memory and running the entry point.

use crate::{Ctx, Piece};
use anyhow::{Context, Result, bail, ensure};
use std::cell::RefCell;
use std::collections::BTreeMap;
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

pub struct Options {
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

struct State {
    names: Vec<String>,
    first: Vec<u32>,
    counts: BTreeMap<u32, u64>,
    calls: u64,
    max_imports: u64,
    ended: Option<String>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(s.borrow_mut().as_mut().expect("the runtime state is set while the game runs")))
}

fn stop(c: *mut Ctx, why: String) -> ! {
    // there is no way to unwind through the translated code, so the report is made here and the run ends
    let report = with_state(|s| {
        s.ended = Some(why);
        report_of(s)
    });
    // SAFETY: `c` is the context the translated code passed us; it is valid for the call
    let at = unsafe { (*c).lr };
    print_report(&report, at);
    // SAFETY: as above
    let regs = unsafe { (*c).r };
    println!(
        "registers: {}",
        regs.iter()
            .enumerate()
            .map(|(i, v)| format!("r{i}={v:#x}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    std::process::exit(2);
}

fn report_of(s: &State) -> Report {
    Report {
        imports: s
            .first
            .iter()
            .map(|&n| (s.names[n as usize].clone(), s.counts.get(&n).copied().unwrap_or(0)))
            .collect(),
        end: s.ended.clone().unwrap_or_default(),
    }
}

fn print_report(r: &Report, lr: u32) {
    println!("the game stopped: {} (link register {lr:#x})", r.end);
    println!("{} system functions called, in order of first call:", r.imports.len());
    for (name, n) in &r.imports {
        println!("  {n:>8}  {name}");
    }
}

extern "C" fn import(c: *mut Ctx, number: u32) {
    let over = with_state(|s| {
        s.calls += 1;
        if !s.counts.contains_key(&number) {
            s.first.push(number);
        }
        *s.counts.entry(number).or_default() += 1;
        s.calls > s.max_imports
    });
    if over {
        stop(c, "too many system calls".into());
    }
    // nothing is implemented yet: every system function does nothing and returns 0
    // SAFETY: `c` is the context the translated code passed us; it is valid for the call
    unsafe { (*c).r[3] = 0 };
}

extern "C" fn unknown_call(c: *mut Ctx, target: u32) {
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

/// Runs the game's entry point from the translated library at `library`, with `pieces` put in the guest's memory first.
pub fn run(library: &Path, pieces: &[Piece], options: &Options) -> Result<Report> {
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
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            names,
            first: Vec::new(),
            counts: BTreeMap::new(),
            calls: 0,
            max_imports: options.max_imports,
            ended: None,
        });
    });
    let host = Host {
        import,
        unknown_call,
        trap,
        unsupported,
        syscall,
    };
    // SAFETY: `host` lives until the end of this function, which is as long as the library is loaded and called
    unsafe { set_host(&host) };
    let mut c = Ctx {
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
        mem: memory.as_mut_ptr(),
    };
    ensure!(
        options.stack_size > 0x1000 && options.stack_top >= options.stack_size,
        "the stack does not fit"
    );
    c.r[1] = options.stack_top - 0x20;
    // SAFETY: the context is valid and its `mem` covers the whole 32-bit address space
    let found = unsafe { call(&mut c, options.entry) };
    if found == 0 {
        bail!("{:#x} is not the start of a translated function", options.entry);
    }
    let report = with_state(|s| {
        s.ended = Some("the entry point returned".into());
        report_of(s)
    });
    Ok(report)
}
