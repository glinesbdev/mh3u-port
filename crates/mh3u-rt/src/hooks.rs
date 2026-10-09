//! Translation-time hooks (`docs/hooks.md`): the translator puts `rt_hook(c, address)` at the entry of each function listed in
//! `mods/hooks.txt`, and this registry runs the native callbacks registered for that function. The registry is installed once,
//! before the game starts, and only read afterwards.

use crate::Ctx;
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// What a callback sees: the hooked function (name and guest address) on entry, with the registers (`ctx.r[3]`... are the
/// arguments) and guest memory. A callback may change both.
pub struct Hook<'a> {
    pub name: &'a str,
    pub address: u32,
    /// How many times this hook ran before (0 the first time).
    pub calls: u64,
    pub ctx: &'a mut Ctx,
}

impl Hook<'_> {
    /// A big-endian word of guest memory.
    pub fn read_u32(&self, address: u32) -> u32 {
        let mut b = [0; 4];
        crate::guest::Guest::new(self.ctx.mem).read(address, &mut b);
        u32::from_be_bytes(b)
    }
}

pub type Callback = Box<dyn Fn(&mut Hook) + Send + Sync>;

struct Entry {
    name: String,
    callbacks: Vec<Callback>,
    calls: AtomicU64,
}

/// The hooks and their callbacks, built before the game starts.
#[derive(Default)]
pub struct Registry {
    hooks: HashMap<u32, Entry>,
}

impl Registry {
    /// The registry for a hooks list (address -> name, as `mh3u_recomp::parse_hooks` gives it), with no callbacks yet.
    pub fn new(list: &BTreeMap<u32, String>) -> Registry {
        let hooks = list
            .iter()
            .map(|(&a, n)| {
                (
                    a,
                    Entry {
                        name: n.clone(),
                        callbacks: Vec::new(),
                        calls: AtomicU64::new(0),
                    },
                )
            })
            .collect();
        Registry { hooks }
    }

    /// Runs `callback` when the hook called `name` is entered. False when no hook has that name.
    pub fn on(&mut self, name: &str, callback: Callback) -> bool {
        match self.hooks.values_mut().find(|e| e.name == name) {
            Some(e) => {
                e.callbacks.push(callback);
                true
            }
            None => false,
        }
    }

    /// The example callback, on every hook: logs the name and r3-r5, the first 20 calls of each, then every 1000th.
    pub fn log_all(&mut self) {
        for e in self.hooks.values_mut() {
            e.callbacks.push(Box::new(|h: &mut Hook| {
                if should_log(h.calls) {
                    let r = h.ctx.r;
                    eprintln!(
                        "[hook] {} ({:#x}) call {}: r3 {:#x}, r4 {:#x}, r5 {:#x}, lr {:#x}",
                        h.name,
                        h.address,
                        h.calls + 1,
                        r[3],
                        r[4],
                        r[5],
                        h.ctx.lr
                    );
                }
            }));
        }
    }

    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    /// Runs the callbacks of the hook at `address` (none if it has none).
    pub fn run(&self, address: u32, ctx: &mut Ctx) {
        if let Some(e) = self.hooks.get(&address) {
            let calls = e.calls.fetch_add(1, Relaxed);
            let mut h = Hook {
                name: &e.name,
                address,
                calls,
                ctx,
            };
            for cb in &e.callbacks {
                cb(&mut h);
            }
        }
    }
}

/// Whether the log example prints call `calls` (counted from 0).
pub fn should_log(calls: u64) -> bool {
    calls < 20 || (calls + 1).is_multiple_of(1000)
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// Installs the registry (once, before the game starts; later calls are ignored).
pub fn install(r: Registry) {
    let n = r.len();
    if REGISTRY.set(r).is_ok() {
        eprintln!("[hooks] {n} hooks installed");
    }
}

/// `host.hook`: a hooked function was entered.
pub(crate) extern "C" fn on_hook(c: *mut Ctx, function: u32) {
    if let Some(r) = REGISTRY.get() {
        // SAFETY: `c` is the running thread's context, passed by the translated code
        r.run(function, unsafe { &mut *c });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn callbacks_run_on_their_hook_only_and_see_and_change_the_registers() {
        let list: BTreeMap<u32, String> = [(0x0200_1000, "item_add".to_string()), (0x0200_2000, "quest".to_string())].into();
        let mut r = Registry::new(&list);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        assert!(r.on(
            "item_add",
            Box::new(move |h: &mut Hook| {
                s.lock().unwrap().push((h.name.to_string(), h.calls, h.ctx.r[3]));
                h.ctx.r[4] = 7;
            })
        ));
        assert!(!r.on("nope", Box::new(|_: &mut Hook| {})));
        r.log_all();
        let mut ctx = crate::game::new_ctx(std::ptr::null_mut());
        ctx.r[3] = 42;
        r.run(0x0200_1000, &mut ctx);
        r.run(0x0200_1000, &mut ctx);
        r.run(0x0200_2000, &mut ctx);
        r.run(0x0300_0000, &mut ctx);
        assert_eq!(*seen.lock().unwrap(), vec![("item_add".into(), 0, 42), ("item_add".into(), 1, 42)]);
        assert_eq!(ctx.r[4], 7);
        assert!(should_log(0) && should_log(19) && !should_log(20) && should_log(999) && !should_log(1000));
    }
}
