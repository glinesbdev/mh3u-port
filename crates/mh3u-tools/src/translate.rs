//! The whole executable as C: one C function for each function found, in files of a few hundred functions, with a header that declares
//! them all. The output is made from the owner's game and must never go into a repository: this refuses an output folder inside a git
//! working tree.

use anyhow::{Result, bail};
use mh3u_recomp::{RUNTIME_H, discover, emit_function_hooked, function_name};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

pub struct Report {
    pub functions: usize,
    pub files: usize,
    pub bytes: usize,
}

/// What the executable holds that the translation needs.
pub struct Input<'a> {
    pub words: &'a [u32],
    pub base: u32,
    /// The words of the data sections (code pointers are among them).
    pub pointers: &'a [u32],
    pub entry: u32,
    /// The names of the imports, and where the calls to each go.
    pub imports: &'a [String],
    pub targets: &'a BTreeMap<u32, u32>,
    /// The hooks list (`docs/hooks.md`): functions that call `rt_hook` on entry.
    pub hooks: &'a mh3u_recomp::HookList,
}

pub fn run(input: &Input, out: &Path, per_file: usize) -> Result<Report> {
    if crate::codegen_bench::in_a_working_tree(out) {
        bail!(
            "{} is inside a git working tree: what this writes is made from the game and must not go in a repository",
            out.display()
        );
    }
    let Input {
        words,
        base,
        pointers,
        entry,
        imports,
        targets,
        hooks,
    } = *input;
    let functions = discover::functions(base, words, pointers, entry);
    let starts: Vec<u32> = functions.iter().copied().collect();
    std::fs::create_dir_all(out)?;
    std::fs::write(out.join("ppc.h"), RUNTIME_H)?;
    let mut header = String::from("#include \"ppc.h\"\n");
    for &s in &starts {
        let _ = writeln!(header, "void {}(Ctx *c);", function_name(s));
    }
    std::fs::write(out.join("game.h"), &header)?;
    let mut names = String::from(
        "/* the system functions the game calls, by the number rt_import is given */\nstatic const char *const import_names[] = {\n",
    );
    for n in imports {
        let _ = writeln!(names, "    \"{n}\",");
    }
    names.push_str("};\n");
    std::fs::write(out.join("imports.h"), names)?;
    // the runtime reads the hooks' names from the build
    let (hooks, notes) = mh3u_recomp::resolve_hooks(hooks, &functions);
    for n in notes {
        println!("[hooks] {n}");
    }
    let list: String = hooks.iter().map(|(a, n)| format!("{a:#010x} {n}\n")).collect();
    std::fs::write(out.join("hooks.txt"), list)?;
    let end = base + 4 * words.len() as u32;
    let mut bytes = header.len();
    let mut files = 0;
    for (n, chunk) in starts.chunks(per_file).enumerate() {
        let mut src = String::from("#include \"game.h\"\n");
        for &s in chunk {
            let next = functions.range(s + 4..).next().copied().unwrap_or(end);
            let body = &words[((s - base) / 4) as usize..((next - base) / 4) as usize];
            src.push_str(&emit_function_hooked(s, body, &functions, targets, hooks.contains_key(&s)));
        }
        bytes += src.len();
        std::fs::write(out.join(format!("part_{n:04}.c")), src)?;
        files += 1;
    }
    let d = dispatch_c(&starts, targets);
    std::fs::write(out.join("dispatch.c"), &d)?;
    Ok(Report {
        functions: starts.len(),
        files,
        bytes,
    })
}

/// The bits of the index of the dispatch table's slots for `n` addresses: at least twice as many slots as addresses, so a lookup
/// rarely probes more than one or two.
fn table_bits(n: usize) -> u32 {
    (2 * n).max(1024).next_power_of_two().trailing_zeros()
}

/// Where the search for `a` starts in a table of `1 << bits` slots (the same sum as `slot` in the C of `dispatch_c`).
fn slot(a: u32, bits: u32) -> usize {
    ((a >> 2).wrapping_mul(0x9e37_79b1) >> (32 - bits)) as usize
}

/// `dispatch.c`: the table of the translated functions and the import trampolines by guest address, and the hooks the translated
/// code calls. `game_call` (every call through a register, a virtual call or a jump through a function pointer) finds an address in
/// a hash table built here, ahead of time: open addressing, linear probing, at least twice as many slots as addresses (for the
/// game's 82,209 functions and 568 trampolines: 2^18 slots of 8 bytes, 2 MiB of read-only data). A slot holds the address and either
/// the function's place in `functions` (0 and up) or the import's number as `-1 - n`; an empty slot has address 0, which is never
/// code. A function wins over a trampoline at the same address, as before.
pub fn dispatch_c(starts: &[u32], targets: &BTreeMap<u32, u32>) -> String {
    let bits = table_bits(starts.len() + targets.len());
    let mask = (1usize << bits) - 1;
    let mut slots: Vec<(u32, i64)> = vec![(0, 0); 1 << bits];
    let mut put = |a: u32, v: i64| {
        let mut i = slot(a, bits);
        while slots[i].0 != 0 {
            if slots[i].0 == a {
                return;
            }
            i = (i + 1) & mask;
        }
        slots[i] = (a, v);
    };
    for (k, &a) in starts.iter().enumerate() {
        if a != 0 {
            put(a, k as i64);
        }
    }
    for (&a, &n) in targets {
        if a != 0 {
            put(a, -1 - i64::from(n));
        }
    }
    let mut d = String::from(
        "#include \"game.h\"\n#include \"imports.h\"\n\
         static Host host;\n\
         typedef void (*Fn)(Ctx *);\n\
         static const struct { uint32_t a; Fn f; } functions[] = {\n",
    );
    for &s in starts {
        let _ = writeln!(d, "{{{s:#x}u,{}}},", function_name(s));
    }
    let _ = writeln!(
        d,
        "}};\n#define SLOT_BITS {bits}\nstatic const struct {{ uint32_t a; int32_t v; }} slots[1u << SLOT_BITS] = {{"
    );
    for (i, &(a, v)) in slots.iter().enumerate() {
        if a != 0 {
            let _ = writeln!(d, "[{i}]={{{a:#x}u,{v}}},");
        }
    }
    d.push_str(
        "};\n\
         GAME_API void game_set_host(const Host *h) { host = *h; }\n\
         #ifdef MH3U_WATCH\n\
         uint32_t g_watch_lo = 1, g_watch_hi = 0, g_enter = 1;\n\
         GAME_API void game_set_enter(uint32_t f, uint32_t lr) { g_enter = f; g_enter_lr = lr; }\n\
         uint32_t g_enter_lr = 1;\n\
         volatile uint32_t g_last_fn = 0;\n\
         GAME_API uint32_t game_last_function(void) { return g_last_fn; }\n\
         void rt_enter(Ctx *c, uint32_t f) { host.enter(c, f); }\n\
         GAME_API void game_set_watch(uint32_t lo, uint32_t hi) { g_watch_lo = lo; g_watch_hi = hi; }\n\
         void rt_watch(Ctx *c, uint32_t a, uint64_t v, uint32_t n) { host.watch(c, a, v, n); }\n\
         #endif\n\
         GAME_API uint32_t game_ctx_size(void) { return sizeof(Ctx); }\n\
         GAME_API uint32_t game_import_count(void) { return sizeof import_names / sizeof *import_names; }\n\
         GAME_API const char *game_import_name(uint32_t n) { return n < game_import_count() ? import_names[n] : 0; }\n\
         GAME_API int game_call(Ctx *c, uint32_t a) {\n\
         \x20   uint32_t i = ((a >> 2) * 0x9e3779b1u) >> (32 - SLOT_BITS);\n\
         \x20   while (slots[i].a != 0) {\n\
         \x20       if (slots[i].a == a) {\n\
         \x20           int32_t v = slots[i].v;\n\
         \x20           if (v >= 0) functions[v].f(c); else host.import(c, (uint32_t)(-1 - v));\n\
         \x20           return 1;\n\
         \x20       }\n\
         \x20       i = (i + 1) & ((1u << SLOT_BITS) - 1);\n\
         \x20   }\n\
         \x20   return 0;\n\
         }\n\
         void rt_call(Ctx *c, uint32_t t) { if (!game_call(c, t)) host.unknown_call(c, t); }\n\
         void rt_jump(Ctx *c, uint32_t t) { if (!game_call(c, t)) host.unknown_call(c, t); }\n\
         void rt_import(Ctx *c, uint32_t n) { host.import(c, n); }\n\
         void rt_hook(Ctx *c, uint32_t f) { host.hook(c, f); }\n\
         void rt_trap(Ctx *c, uint32_t w) { host.trap(c, w); }\n\
         void rt_unsupported(Ctx *c, uint32_t w) { host.unsupported(c, w); }\n\
         void rt_syscall(Ctx *c) { host.syscall(c); }\n",
    );
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn the_table_has_room_to_spare() {
        assert_eq!(table_bits(0), 10);
        assert_eq!(table_bits(82_209 + 568), 18);
        assert!(slot(0xffff_fffc, 18) < 1 << 18);
    }

    /// The generated `game_call` finds every function and trampoline by its address, a function before a trampoline at the same
    /// address, and nothing else. Built and run with the system C compiler (skipped without one).
    #[test]
    fn game_call_finds_every_function_and_import_and_nothing_else() {
        // MH3U_TEST_CFLAGS and MH3U_TEST_RUNNER as in mh3u-testkit's c_diff (such as --target=x86_64-w64-windows-gnu and wine)
        let extra: Vec<String> = std::env::var("MH3U_TEST_CFLAGS")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let tc = match mh3u_recomp::toolchain::Toolchain::detect(&extra) {
            Ok(tc) => tc,
            Err(e) => {
                eprintln!("skipped: {e}");
                return;
            }
        };
        // functions close together, as in the game, and spread out; trampolines after them, one at a function's address
        let mut starts: Vec<u32> = (0..3000u32).map(|k| 0x0200_0020 + 4 * (k * k % 7919 + 3 * k)).collect();
        starts.sort_unstable();
        starts.dedup();
        let targets: BTreeMap<u32, u32> = (0..40u32).map(|n| (0x0319_63d8 + 8 * n, n)).chain([(starts[5], 99)]).collect();
        let dir = std::env::temp_dir().join(format!("mh3u-dispatch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut game_h = String::from("#include \"ppc.h\"\n");
        let mut defs = String::from("#include \"game.h\"\n#include <stdio.h>\n");
        for &s in &starts {
            let _ = writeln!(game_h, "void {}(Ctx *c);", function_name(s));
            let _ = writeln!(defs, "void {}(Ctx *c) {{ c->r[3] = {s:#x}u; }}", function_name(s));
        }
        std::fs::write(dir.join("ppc.h"), RUNTIME_H).unwrap();
        std::fs::write(dir.join("game.h"), game_h).unwrap();
        std::fs::write(dir.join("imports.h"), "static const char *const import_names[] = { \"x\" };\n").unwrap();
        std::fs::write(dir.join("dispatch.c"), dispatch_c(&starts, &targets)).unwrap();
        // main: for each address on stdin, print what game_call did (the function's mark in r3, or the import's number, or a miss)
        defs.push_str(
            "GAME_API void game_set_host(const Host *h); GAME_API int game_call(Ctx *c, uint32_t a);\n\
             static void imp(Ctx *c, uint32_t n) { c->r[3] = 0xf0000000u | n; }\n\
             int main(void) { Host h = {0}; h.import = imp; game_set_host(&h); unsigned a;\n\
             while (scanf(\"%x\", &a) == 1) { Ctx c = {0}; int ok = game_call(&c, a); printf(\"%d %x\\n\", ok, c.r[3]); } return 0; }\n",
        );
        std::fs::write(dir.join("main.c"), defs).unwrap();
        let exe = dir.join(format!("prog{}", tc.exe_suffix()));
        let out = Command::new(&tc.cc)
            .args(["-O2", "-w"])
            .args(&extra)
            .arg("-I")
            .arg(&dir)
            .arg(dir.join("dispatch.c"))
            .arg(dir.join("main.c"))
            .arg("-o")
            .arg(&exe)
            .args(tc.link_args())
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let mut queries: Vec<u32> = starts.clone();
        queries.extend(targets.keys());
        queries.extend([
            0,
            4,
            0x0200_0024,
            0x0200_0022,
            0xffff_fffc,
            starts[0] + 2,
            *starts.last().unwrap() + 4,
        ]);
        let input: String = queries.iter().map(|q| format!("{q:x}\n")).collect();
        let runner = std::env::var("MH3U_TEST_RUNNER").unwrap_or_default();
        let mut words = runner.split_whitespace();
        let mut child = match words.next() {
            Some(r) => {
                let mut c = Command::new(r);
                c.args(words).arg(&exe);
                c
            }
            None => Command::new(&exe),
        };
        let mut child = child
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let text = String::from_utf8(out.stdout).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), queries.len());
        for (q, line) in queries.iter().zip(lines) {
            let want = if starts.binary_search(q).is_ok() {
                format!("1 {q:x}")
            } else if let Some(n) = targets.get(q) {
                format!("1 {:x}", 0xf000_0000u32 | n)
            } else {
                "0 0".to_string()
            };
            assert_eq!(line, want, "address {q:#x}");
        }
    }
}
