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
    Ok(Report {
        functions: starts.len(),
        files,
        bytes,
    })
}
