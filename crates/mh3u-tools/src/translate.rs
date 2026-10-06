//! The whole executable as C: one C function for each function found, in files of a few hundred functions, with a header that declares
//! them all. The output is made from the owner's game and must never go into a repository: this refuses an output folder inside a git
//! working tree.

use anyhow::{Result, bail};
use mh3u_recomp::{RUNTIME_H, discover, emit_function_with, function_name};
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
    let end = base + 4 * words.len() as u32;
    let mut bytes = header.len();
    let mut files = 0;
    for (n, chunk) in starts.chunks(per_file).enumerate() {
        let mut src = String::from("#include \"game.h\"\n");
        for &s in chunk {
            let next = functions.range(s + 4..).next().copied().unwrap_or(end);
            let body = &words[((s - base) / 4) as usize..((next - base) / 4) as usize];
            src.push_str(&emit_function_with(s, body, &functions, targets));
        }
        bytes += src.len();
        std::fs::write(out.join(format!("part_{n:04}.c")), src)?;
        files += 1;
    }
    let mut d = String::from(
        "#include \"game.h\"\n#include \"imports.h\"\n\
         static Host host;\n\
         typedef void (*Fn)(Ctx *);\n\
         static const struct { uint32_t a; Fn f; } functions[] = {\n",
    );
    for &s in &starts {
        let _ = writeln!(d, "{{{s:#x}u,{}}},", function_name(s));
    }
    d.push_str("};\nstatic const struct { uint32_t a; uint32_t n; } trampolines[] = {\n");
    for (&a, &n) in targets {
        let _ = writeln!(d, "{{{a:#x}u,{n}}},");
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
         \x20   size_t lo = 0, hi = sizeof functions / sizeof *functions;\n\
         \x20   while (lo < hi) { size_t m = (lo + hi) / 2; if (functions[m].a < a) lo = m + 1; else hi = m; }\n\
         \x20   if (lo != sizeof functions / sizeof *functions && functions[lo].a == a) { functions[lo].f(c); return 1; }\n\
         \x20   lo = 0; hi = sizeof trampolines / sizeof *trampolines;\n\
         \x20   while (lo < hi) { size_t m = (lo + hi) / 2; if (trampolines[m].a < a) lo = m + 1; else hi = m; }\n\
         \x20   if (lo != sizeof trampolines / sizeof *trampolines && trampolines[lo].a == a) { host.import(c, trampolines[lo].n); return 1; }\n\
         \x20   return 0;\n\
         }\n\
         void rt_call(Ctx *c, uint32_t t) { if (!game_call(c, t)) host.unknown_call(c, t); }\n\
         void rt_jump(Ctx *c, uint32_t t) { if (!game_call(c, t)) host.unknown_call(c, t); }\n\
         void rt_import(Ctx *c, uint32_t n) { host.import(c, n); }\n\
         void rt_trap(Ctx *c, uint32_t w) { host.trap(c, w); }\n\
         void rt_unsupported(Ctx *c, uint32_t w) { host.unsupported(c, w); }\n\
         void rt_syscall(Ctx *c) { host.syscall(c); }\n",
    );
    std::fs::write(out.join("dispatch.c"), &d)?;
    Ok(Report {
        functions: starts.len(),
        files,
        bytes,
    })
}
