//! Spike 4: how long does generated code take to compile? This translates a sample of the game's functions into Rust or C, in the shape
//! a recompiler would give them, only to be built and timed. It is **not** a translator: it gives the ordinary instructions their
//! meaning and everything else a placeholder call, and nothing checks that the result behaves like the game. The output is made from the
//! owner's game and must never go into the repository: `run` refuses an output folder inside a git working tree.

use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    C,
}

/// What the run produced, for the notes.
pub struct Report {
    pub functions: usize,
    pub instructions: usize,
    pub placeholders: usize,
    pub bytes: usize,
    pub lines: usize,
    pub files: usize,
}

fn si(word: u32) -> i32 {
    i32::from(word as i16)
}

fn rd(w: u32) -> u32 {
    (w >> 21) & 31
}
fn ra(w: u32) -> u32 {
    (w >> 16) & 31
}
fn rb(w: u32) -> u32 {
    (w >> 11) & 31
}

/// The target of a direct branch word at `addr` (`b` or `bc`), if it is one and not absolute.
fn branch_target(addr: u32, w: u32) -> Option<u32> {
    match w >> 26 {
        18 if w & 2 == 0 => {
            let li = ((w & 0x03ff_fffc) as i32) << 6 >> 6;
            Some(addr.wrapping_add(li as u32))
        }
        16 if w & 2 == 0 => Some(addr.wrapping_add(i32::from((w & 0xfffc) as u16 as i16) as u32)),
        _ => None,
    }
}

/// The mask `rlwinm` and friends use, from the bit numbers `mb` to `me` (bit 0 is the most significant).
fn mask(mb: u32, me: u32) -> u32 {
    let begin = 0xffff_ffffu32 >> mb;
    let end = 0xffff_ffffu32 << (31 - me);
    if mb <= me { begin & end } else { begin | end }
}

struct Gen {
    lang: Lang,
    placeholders: usize,
}

impl Gen {
    /// The first argument of a helper that takes the machine state.
    fn ctx(&self) -> &'static str {
        match self.lang {
            Lang::Rust => "c",
            Lang::C => "&c",
        }
    }

    /// The effective address of `d(ra)`, or of `ra + rb`.
    fn ea(&self, a: u32, disp: i32) -> String {
        if a == 0 {
            format!("{:#x}", disp as u32)
        } else if disp == 0 {
            format!("c.r[{a}]")
        } else {
            format!("add(c.r[{a}], {:#x})", disp as u32)
        }
    }

    fn ea_x(&self, a: u32, b: u32) -> String {
        if a == 0 {
            format!("c.r[{b}]")
        } else {
            format!("add(c.r[{a}], c.r[{b}])")
        }
    }

    fn placeholder(&mut self, w: u32) -> String {
        self.placeholders += 1;
        format!("unimpl({}, {w:#x});", self.ctx())
    }

    /// The statements for one instruction. `lookup` says whether an address is the start of a function that was generated.
    fn insn(&mut self, addr: u32, w: u32, block_starts: &BTreeSet<u32>, lo: u32, hi: u32, generated: &BTreeSet<u32>) -> Vec<String> {
        let ctx = self.ctx();
        let (d, a, b) = (rd(w), ra(w), rb(w));
        let rc = w & 1 != 0;
        let record = |dst: String| -> Vec<String> {
            if rc {
                vec![dst.clone(), format!("cr0({ctx}, c.r[{}]);", rd_of(&dst))]
            } else {
                vec![dst]
            }
        };
        let target = branch_target(addr, w);
        let s = |x: String| vec![x];
        match w >> 26 {
            7 => s(format!("c.r[{d}] = mul(c.r[{a}], {:#x});", si(w) as u32)),
            8 => s(format!("c.r[{d}] = sub({:#x}, c.r[{a}]);", si(w) as u32)),
            10 => s(format!("cmpu({ctx}, {}, c.r[{a}], {:#x});", d >> 2, w & 0xffff)),
            11 => s(format!("cmps({ctx}, {}, c.r[{a}], {:#x});", d >> 2, si(w) as u32)),
            12 => s(format!("c.r[{d}] = add(c.r[{a}], {:#x});", si(w) as u32)),
            14 => {
                if a == 0 {
                    s(format!("c.r[{d}] = {:#x};", si(w) as u32))
                } else {
                    s(format!("c.r[{d}] = add(c.r[{a}], {:#x});", si(w) as u32))
                }
            }
            15 => {
                let imm = (w & 0xffff) << 16;
                if a == 0 {
                    s(format!("c.r[{d}] = {imm:#x};"))
                } else {
                    s(format!("c.r[{d}] = add(c.r[{a}], {imm:#x});"))
                }
            }
            24 => s(format!("c.r[{a}] = c.r[{d}] | {:#x};", w & 0xffff)),
            25 => s(format!("c.r[{a}] = c.r[{d}] | {:#x};", (w & 0xffff) << 16)),
            26 => s(format!("c.r[{a}] = c.r[{d}] ^ {:#x};", w & 0xffff)),
            27 => s(format!("c.r[{a}] = c.r[{d}] ^ {:#x};", (w & 0xffff) << 16)),
            28 => vec![
                format!("c.r[{a}] = c.r[{d}] & {:#x};", w & 0xffff),
                format!("cr0({ctx}, c.r[{a}]);"),
            ],
            29 => vec![
                format!("c.r[{a}] = c.r[{d}] & {:#x};", (w & 0xffff) << 16),
                format!("cr0({ctx}, c.r[{a}]);"),
            ],
            20 => {
                let (sh, mb, me) = ((w >> 11) & 31, (w >> 6) & 31, (w >> 1) & 31);
                let m = mask(mb, me);
                record(format!("c.r[{a}] = (c.r[{a}] & {:#x}) | (rotl(c.r[{d}], {sh}) & {m:#x});", !m))
            }
            21 => {
                let (sh, mb, me) = ((w >> 11) & 31, (w >> 6) & 31, (w >> 1) & 31);
                record(format!("c.r[{a}] = rotl(c.r[{d}], {sh}) & {:#x};", mask(mb, me)))
            }
            23 => {
                let (mb, me) = ((w >> 6) & 31, (w >> 1) & 31);
                record(format!("c.r[{a}] = rotl(c.r[{d}], c.r[{b}] & 31) & {:#x};", mask(mb, me)))
            }
            32 => s(format!("c.r[{d}] = ld32({ctx}, {});", self.ea(a, si(w)))),
            33 => vec![
                format!("c.r[{a}] = {};", self.ea(a, si(w))),
                format!("c.r[{d}] = ld32({ctx}, c.r[{a}]);"),
            ],
            34 => s(format!("c.r[{d}] = ld8({ctx}, {});", self.ea(a, si(w)))),
            35 => vec![
                format!("c.r[{a}] = {};", self.ea(a, si(w))),
                format!("c.r[{d}] = ld8({ctx}, c.r[{a}]);"),
            ],
            40 => s(format!("c.r[{d}] = ld16({ctx}, {});", self.ea(a, si(w)))),
            41 => vec![
                format!("c.r[{a}] = {};", self.ea(a, si(w))),
                format!("c.r[{d}] = ld16({ctx}, c.r[{a}]);"),
            ],
            42 => s(format!("c.r[{d}] = ld16s({ctx}, {});", self.ea(a, si(w)))),
            36 => s(format!("st32({ctx}, {}, c.r[{d}]);", self.ea(a, si(w)))),
            37 => vec![
                format!("c.r[{a}] = {};", self.ea(a, si(w))),
                format!("st32({ctx}, c.r[{a}], c.r[{d}]);"),
            ],
            38 => s(format!("st8({ctx}, {}, c.r[{d}]);", self.ea(a, si(w)))),
            44 => s(format!("st16({ctx}, {}, c.r[{d}]);", self.ea(a, si(w)))),
            45 => vec![
                format!("c.r[{a}] = {};", self.ea(a, si(w))),
                format!("st16({ctx}, c.r[{a}], c.r[{d}]);"),
            ],
            48 => s(format!("c.f[{d}] = ldf32({ctx}, {});", self.ea(a, si(w)))),
            50 => s(format!("c.f[{d}] = ldf64({ctx}, {});", self.ea(a, si(w)))),
            52 => s(format!("stf32({ctx}, {}, c.f[{d}]);", self.ea(a, si(w)))),
            54 => s(format!("stf64({ctx}, {}, c.f[{d}]);", self.ea(a, si(w)))),
            18 => {
                let t = target.unwrap_or(0);
                let link = w & 1 != 0;
                if link {
                    let call = if generated.contains(&t) {
                        self.direct_call(t)
                    } else {
                        format!("call({ctx}, {t:#x});")
                    };
                    vec![format!("c.lr = {:#x};", addr.wrapping_add(4)), call]
                } else if (lo..hi).contains(&t) && block_starts.contains(&t) {
                    vec![self.jump(t)]
                } else {
                    vec![format!("call({ctx}, {t:#x});"), self.ret()]
                }
            }
            16 => {
                let t = target.unwrap_or(0);
                let (bo, bi) = ((w >> 21) & 31, (w >> 16) & 31);
                if (lo..hi).contains(&t) && block_starts.contains(&t) {
                    vec![self.cond_jump(&format!("bc({ctx}, {bo}, {bi})"), t)]
                } else {
                    vec![format!(
                        "if {} {{ call({ctx}, {t:#x}); {} }}",
                        self.paren(&format!("bc({ctx}, {bo}, {bi})")),
                        self.ret_inline()
                    )]
                }
            }
            19 => match (w >> 1) & 0x3ff {
                16 => {
                    let (bo, bi) = ((w >> 21) & 31, (w >> 16) & 31);
                    if bo & 20 == 20 {
                        vec![self.ret()]
                    } else {
                        vec![format!(
                            "if {} {{ {} }}",
                            self.paren(&format!("bc({ctx}, {bo}, {bi})")),
                            self.ret_inline()
                        )]
                    }
                }
                528 => vec![format!("call_ctr({ctx});"), self.ret()],
                193 | 449 | 33 | 129 | 225 | 257 | 289 | 417 => s(format!("crop({ctx}, {w:#x});")),
                _ => s(self.placeholder(w)),
            },
            31 => {
                let ext = (w >> 1) & 0x3ff;
                match ext & 0x1ff {
                    266 => return record(format!("c.r[{d}] = add(c.r[{a}], c.r[{b}]);")),
                    40 => return record(format!("c.r[{d}] = sub(c.r[{b}], c.r[{a}]);")),
                    235 => return record(format!("c.r[{d}] = mul(c.r[{a}], c.r[{b}]);")),
                    104 => return record(format!("c.r[{d}] = sub(0, c.r[{a}]);")),
                    _ => {}
                }
                match ext {
                    0 => s(format!("cmps({ctx}, {}, c.r[{a}], c.r[{b}]);", d >> 2)),
                    32 => s(format!("cmpu({ctx}, {}, c.r[{a}], c.r[{b}]);", d >> 2)),
                    28 => record(format!("c.r[{a}] = c.r[{d}] & c.r[{b}];")),
                    444 => record(format!("c.r[{a}] = c.r[{d}] | c.r[{b}];")),
                    316 => record(format!("c.r[{a}] = c.r[{d}] ^ c.r[{b}];")),
                    124 => record(format!("c.r[{a}] = !(c.r[{d}] | c.r[{b}]);")),
                    60 => record(format!("c.r[{a}] = c.r[{d}] & !c.r[{b}];")),
                    24 => record(format!("c.r[{a}] = slw(c.r[{d}], c.r[{b}]);")),
                    536 => record(format!("c.r[{a}] = srw(c.r[{d}], c.r[{b}]);")),
                    824 => record(format!("c.r[{a}] = srawi({ctx}, c.r[{d}], {b});")),
                    954 => record(format!("c.r[{a}] = ext8(c.r[{d}]);")),
                    922 => record(format!("c.r[{a}] = ext16(c.r[{d}]);")),
                    23 => s(format!("c.r[{d}] = ld32({ctx}, {});", self.ea_x(a, b))),
                    87 => s(format!("c.r[{d}] = ld8({ctx}, {});", self.ea_x(a, b))),
                    279 => s(format!("c.r[{d}] = ld16({ctx}, {});", self.ea_x(a, b))),
                    151 => s(format!("st32({ctx}, {}, c.r[{d}]);", self.ea_x(a, b))),
                    215 => s(format!("st8({ctx}, {}, c.r[{d}]);", self.ea_x(a, b))),
                    407 => s(format!("st16({ctx}, {}, c.r[{d}]);", self.ea_x(a, b))),
                    339 | 467 => {
                        let spr = ((w >> 16) & 31) | (((w >> 11) & 31) << 5);
                        let reg = match spr {
                            8 => "lr",
                            9 => "ctr",
                            1 => "xer",
                            _ => return s(self.placeholder(w)),
                        };
                        if ext == 339 {
                            s(format!("c.r[{d}] = c.{reg};"))
                        } else {
                            s(format!("c.{reg} = c.r[{d}];"))
                        }
                    }
                    598 | 854 => s("sync();".to_string()),
                    _ => s(self.placeholder(w)),
                }
            }
            59 | 63 => {
                let ext = (w >> 1) & 0x1f;
                let (fd, fa, fb, fc) = (d, a, b, (w >> 6) & 31);
                let single = w >> 26 == 59;
                let round = |e: String| if single { format!("round_single({e})") } else { e };
                match ext {
                    21 => s(format!("c.f[{fd}] = {};", round(format!("c.f[{fa}] + c.f[{fb}]")))),
                    20 => s(format!("c.f[{fd}] = {};", round(format!("c.f[{fa}] - c.f[{fb}]")))),
                    25 => s(format!("c.f[{fd}] = {};", round(format!("c.f[{fa}] * c.f[{fc}]")))),
                    18 => s(format!("c.f[{fd}] = {};", round(format!("c.f[{fa}] / c.f[{fb}]")))),
                    29 => s(format!("c.f[{fd}] = {};", round(format!("c.f[{fa}] * c.f[{fc}] + c.f[{fb}]")))),
                    28 => s(format!("c.f[{fd}] = {};", round(format!("c.f[{fa}] * c.f[{fc}] - c.f[{fb}]")))),
                    _ => match (w >> 1) & 0x3ff {
                        72 if !single => s(format!("c.f[{fd}] = c.f[{fb}];")),
                        40 if !single => s(format!("c.f[{fd}] = -c.f[{fb}];")),
                        264 if !single => s(format!("c.f[{fd}] = fabs(c.f[{fb}]);")),
                        0 if !single => s(format!("fcmp({ctx}, {}, c.f[{fa}], c.f[{fb}]);", d >> 2)),
                        12 if !single => s(format!("c.f[{fd}] = round_single(c.f[{fb}]);")),
                        _ => s(self.placeholder(w)),
                    },
                }
            }
            _ => s(self.placeholder(w)),
        }
    }

    fn paren(&self, cond: &str) -> String {
        match self.lang {
            Lang::Rust => cond.to_string(),
            Lang::C => format!("({cond})"),
        }
    }

    fn jump(&self, t: u32) -> String {
        match self.lang {
            Lang::Rust => format!("pc = {t:#x}; continue;"),
            Lang::C => format!("goto L_{t:08x};"),
        }
    }

    fn cond_jump(&self, cond: &str, t: u32) -> String {
        match self.lang {
            Lang::Rust => format!("if {cond} {{ pc = {t:#x}; continue; }}"),
            Lang::C => format!("if ({cond}) goto L_{t:08x};"),
        }
    }

    fn ret(&self) -> String {
        "return;".to_string()
    }

    fn ret_inline(&self) -> String {
        "return;".to_string()
    }

    fn direct_call(&self, t: u32) -> String {
        match self.lang {
            Lang::Rust => format!("f_{t:08x}(c);"),
            Lang::C => format!("f_{t:08x}(cp);"),
        }
    }
}

/// The destination register of a `c.r[N] = ...` statement, for recording the condition register.
fn rd_of(stmt: &str) -> String {
    stmt.strip_prefix("c.r[")
        .and_then(|s| s.split(']').next())
        .unwrap_or("0")
        .to_string()
}

fn function_starts(words: &[u32], base: u32) -> BTreeSet<u32> {
    let end = base + words.len() as u32 * 4;
    let mut out = BTreeSet::new();
    for (i, &w) in words.iter().enumerate() {
        if w >> 26 == 18 && w & 1 != 0 && w & 2 == 0 {
            let addr = base + i as u32 * 4;
            if let Some(t) = branch_target(addr, w).filter(|t| (base..end).contains(t) && t % 4 == 0) {
                out.insert(t);
            }
        }
    }
    out
}

pub fn in_a_working_tree(path: &Path) -> bool {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    absolute.ancestors().any(|p| p.join(".git").exists())
}

pub fn run(words: &[u32], base: u32, count: usize, lang: Lang, out: &Path, per_file: usize) -> Result<Report> {
    if in_a_working_tree(out) {
        bail!(
            "{} is inside a git working tree: what this writes is made from the game and must not go in a repository",
            out.display()
        );
    }
    let starts: Vec<u32> = function_starts(words, base).into_iter().collect();
    if starts.len() < count {
        bail!("only {} functions found", starts.len());
    }
    let end = base + words.len() as u32 * 4;
    // an even sample, so that small and large functions are in it in the proportion they have in the game
    let chosen: Vec<usize> = (0..count).map(|i| i * starts.len() / count).collect();
    let generated: BTreeSet<u32> = chosen.iter().map(|&i| starts[i]).collect();
    std::fs::create_dir_all(out.join("src"))?;
    let mut emitter = Gen { lang, placeholders: 0 };
    let mut report = Report {
        functions: 0,
        instructions: 0,
        placeholders: 0,
        bytes: 0,
        lines: 0,
        files: 0,
    };
    let ext = if lang == Lang::Rust { "rs" } else { "c" };
    let mut modules = Vec::new();
    for (chunk_no, chunk) in chosen.chunks(per_file).enumerate() {
        let mut text = String::new();
        match lang {
            Lang::Rust => text.push_str("#![allow(unused, unreachable_code, unused_mut, unused_parens, clippy::all)]\nuse crate::*;\n\n"),
            Lang::C => text.push_str("#include \"rt.h\"\n#include \"funcs.h\"\n\n"),
        }
        for &i in chunk {
            let lo = starts[i];
            let hi = starts.get(i + 1).copied().unwrap_or(end).min(lo + 8192 * 4);
            let first = ((lo - base) / 4) as usize;
            let body: Vec<u32> = words[first..((hi - base) / 4) as usize].to_vec();
            // block starts: the function's start, every branch target inside it and every address after a branch
            let mut block_starts: BTreeSet<u32> = BTreeSet::from([lo]);
            for (k, &w) in body.iter().enumerate() {
                let addr = lo + k as u32 * 4;
                if let Some(t) = branch_target(addr, w).filter(|t| (lo..hi).contains(t)) {
                    block_starts.insert(t);
                }
                if matches!(w >> 26, 16 | 18 | 19) && addr + 4 < hi {
                    block_starts.insert(addr + 4);
                }
            }
            match lang {
                Lang::Rust => {
                    let _ = writeln!(
                        text,
                        "pub fn f_{lo:08x}(c: &mut Ctx) {{\n    let mut pc: u32 = {lo:#x};\n    loop {{\n        match pc {{"
                    );
                }
                Lang::C => {
                    let _ = writeln!(text, "void f_{lo:08x}(Ctx *cp) {{");
                }
            }
            for (k, &w) in body.iter().enumerate() {
                let addr = lo + k as u32 * 4;
                if block_starts.contains(&addr) {
                    match lang {
                        Lang::Rust => {
                            if k > 0 {
                                let _ = writeln!(text, "                pc = {addr:#x};\n            }}");
                            }
                            let _ = writeln!(text, "            {addr:#x} => {{");
                        }
                        Lang::C => {
                            let _ = writeln!(text, "L_{addr:08x}:;");
                        }
                    }
                }
                for stmt in emitter.insn(addr, w, &block_starts, lo, hi, &generated) {
                    match lang {
                        Lang::Rust => {
                            let _ = writeln!(text, "                {stmt}");
                        }
                        Lang::C => {
                            let _ = writeln!(text, "    {stmt}");
                        }
                    }
                }
                report.instructions += 1;
            }
            match lang {
                Lang::Rust => text.push_str("            }\n            _ => return,\n        }\n    }\n}\n\n"),
                Lang::C => text.push_str("}\n\n"),
            }
            report.functions += 1;
        }
        let name = format!("chunk{chunk_no:04}");
        report.lines += text.lines().count();
        report.bytes += text.len();
        let path = if lang == Lang::Rust {
            out.join(format!("src/{name}.{ext}"))
        } else {
            out.join(format!("{name}.{ext}"))
        };
        std::fs::write(&path, text).with_context(|| path.display().to_string())?;
        modules.push(name);
        report.files += 1;
    }
    report.placeholders = emitter.placeholders;
    match lang {
        Lang::Rust => {
            let mut lib = String::from("pub mod rt;\npub use rt::*;\n");
            for m in &modules {
                let _ = writeln!(lib, "pub mod {m};\npub use {m}::*;");
            }
            std::fs::write(out.join("src/lib.rs"), lib)?;
            std::fs::write(out.join("src/rt.rs"), RUST_RT)?;
            std::fs::write(
                out.join("Cargo.toml"),
                "[package]\nname = \"bench\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n\n[profile.release]\nopt-level = 1\n",
            )?;
        }
        Lang::C => {
            std::fs::write(out.join("rt.h"), C_RT)?;
            let protos: String = generated.iter().map(|t| format!("void f_{t:08x}(Ctx *cp);\n")).collect();
            std::fs::write(out.join("funcs.h"), protos)?;
            std::fs::write(out.join("modules.txt"), modules.join("\n"))?;
        }
    }
    Ok(report)
}

const RUST_RT: &str = r#"#![allow(unused)]
pub struct Ctx { pub r: [u32; 32], pub f: [f64; 32], pub cr: [u8; 8], pub lr: u32, pub ctr: u32, pub xer: u32, pub mem: *mut u8 }
#[inline(always)] pub fn add(a: u32, b: u32) -> u32 { a.wrapping_add(b) }
#[inline(always)] pub fn sub(a: u32, b: u32) -> u32 { a.wrapping_sub(b) }
#[inline(always)] pub fn mul(a: u32, b: u32) -> u32 { a.wrapping_mul(b) }
#[inline(always)] pub fn rotl(a: u32, n: u32) -> u32 { a.rotate_left(n) }
#[inline(always)] pub fn slw(a: u32, b: u32) -> u32 { if b & 32 != 0 { 0 } else { a << (b & 31) } }
#[inline(always)] pub fn srw(a: u32, b: u32) -> u32 { if b & 32 != 0 { 0 } else { a >> (b & 31) } }
#[inline(always)] pub fn ext8(a: u32) -> u32 { a as u8 as i8 as i32 as u32 }
#[inline(always)] pub fn ext16(a: u32) -> u32 { a as u16 as i16 as i32 as u32 }
#[inline(always)] pub fn round_single(a: f64) -> f64 { a as f32 as f64 }
#[inline(always)] pub fn fabs(a: f64) -> f64 { a.abs() }
#[inline(always)] pub fn ld8(c: &mut Ctx, a: u32) -> u32 { unsafe { *c.mem.add(a as usize) as u32 } }
#[inline(always)] pub fn ld16(c: &mut Ctx, a: u32) -> u32 { unsafe { u16::from_be(std::ptr::read_unaligned(c.mem.add(a as usize) as *const u16)) as u32 } }
#[inline(always)] pub fn ld16s(c: &mut Ctx, a: u32) -> u32 { ld16(c, a) as u16 as i16 as i32 as u32 }
#[inline(always)] pub fn ld32(c: &mut Ctx, a: u32) -> u32 { unsafe { u32::from_be(std::ptr::read_unaligned(c.mem.add(a as usize) as *const u32)) } }
#[inline(always)] pub fn st8(c: &mut Ctx, a: u32, v: u32) { unsafe { *c.mem.add(a as usize) = v as u8 } }
#[inline(always)] pub fn st16(c: &mut Ctx, a: u32, v: u32) { unsafe { std::ptr::write_unaligned(c.mem.add(a as usize) as *mut u16, (v as u16).to_be()) } }
#[inline(always)] pub fn st32(c: &mut Ctx, a: u32, v: u32) { unsafe { std::ptr::write_unaligned(c.mem.add(a as usize) as *mut u32, v.to_be()) } }
#[inline(always)] pub fn ldf32(c: &mut Ctx, a: u32) -> f64 { f32::from_bits(ld32(c, a)) as f64 }
#[inline(always)] pub fn ldf64(c: &mut Ctx, a: u32) -> f64 { let h = ld32(c, a) as u64; let l = ld32(c, a.wrapping_add(4)) as u64; f64::from_bits(h << 32 | l) }
#[inline(always)] pub fn stf32(c: &mut Ctx, a: u32, v: f64) { st32(c, a, (v as f32).to_bits()) }
#[inline(always)] pub fn stf64(c: &mut Ctx, a: u32, v: f64) { let b = v.to_bits(); st32(c, a, (b >> 32) as u32); st32(c, a.wrapping_add(4), b as u32) }
pub fn cmps(c: &mut Ctx, f: usize, a: u32, b: u32) { let (a, b) = (a as i32, b as i32); c.cr[f] = if a < b { 8 } else if a > b { 4 } else { 2 }; }
pub fn cmpu(c: &mut Ctx, f: usize, a: u32, b: u32) { c.cr[f] = if a < b { 8 } else if a > b { 4 } else { 2 }; }
pub fn fcmp(c: &mut Ctx, f: usize, a: f64, b: f64) { c.cr[f] = if a < b { 8 } else if a > b { 4 } else if a == b { 2 } else { 1 }; }
pub fn cr0(c: &mut Ctx, v: u32) { let v = v as i32; c.cr[0] = if v < 0 { 8 } else if v > 0 { 4 } else { 2 }; }
pub fn crop(c: &mut Ctx, w: u32) { c.cr[0] ^= (w & 1) as u8; }
pub fn srawi(c: &mut Ctx, a: u32, n: u32) -> u32 { ((a as i32) >> n) as u32 }
pub fn bc(c: &mut Ctx, bo: u32, bi: u32) -> bool {
    let ctr_ok = if bo & 4 == 0 { c.ctr = c.ctr.wrapping_sub(1); (c.ctr != 0) == (bo & 2 == 0) } else { true };
    let bit = (c.cr[(bi / 4) as usize] >> (3 - bi % 4)) & 1 != 0;
    let cond_ok = if bo & 16 == 0 { bit == (bo & 8 != 0) } else { true };
    ctr_ok && cond_ok
}
pub fn call(c: &mut Ctx, target: u32) { c.ctr = target; }
pub fn call_ctr(c: &mut Ctx) { let t = c.ctr; call(c, t) }
pub fn sync() {}
pub fn unimpl(c: &mut Ctx, w: u32) { c.xer ^= w; }
"#;

const C_RT: &str = r#"#include <stdint.h>
#include <string.h>
#include <math.h>
typedef struct Ctx { uint32_t r[32]; double f[32]; uint8_t cr[8]; uint32_t lr, ctr, xer; uint8_t *mem; } Ctx;
#define c (*cp)
static inline uint32_t add(uint32_t a, uint32_t b) { return a + b; }
static inline uint32_t sub(uint32_t a, uint32_t b) { return a - b; }
static inline uint32_t mul(uint32_t a, uint32_t b) { return a * b; }
static inline uint32_t rotl(uint32_t a, uint32_t n) { n &= 31; return n ? (a << n) | (a >> (32 - n)) : a; }
static inline uint32_t slw(uint32_t a, uint32_t b) { return (b & 32) ? 0 : a << (b & 31); }
static inline uint32_t srw(uint32_t a, uint32_t b) { return (b & 32) ? 0 : a >> (b & 31); }
static inline uint32_t ext8(uint32_t a) { return (uint32_t)(int32_t)(int8_t)a; }
static inline uint32_t ext16(uint32_t a) { return (uint32_t)(int32_t)(int16_t)a; }
static inline double round_single(double a) { return (double)(float)a; }
static inline uint32_t bswap32(uint32_t v) { return __builtin_bswap32(v); }
static inline uint32_t ld8(Ctx *x, uint32_t a) { return x->mem[a]; }
static inline uint32_t ld16(Ctx *x, uint32_t a) { uint16_t v; memcpy(&v, x->mem + a, 2); return __builtin_bswap16(v); }
static inline uint32_t ld16s(Ctx *x, uint32_t a) { return ext16(ld16(x, a)); }
static inline uint32_t ld32(Ctx *x, uint32_t a) { uint32_t v; memcpy(&v, x->mem + a, 4); return bswap32(v); }
static inline void st8(Ctx *x, uint32_t a, uint32_t v) { x->mem[a] = (uint8_t)v; }
static inline void st16(Ctx *x, uint32_t a, uint32_t v) { uint16_t s = __builtin_bswap16((uint16_t)v); memcpy(x->mem + a, &s, 2); }
static inline void st32(Ctx *x, uint32_t a, uint32_t v) { uint32_t s = bswap32(v); memcpy(x->mem + a, &s, 4); }
static inline double ldf32(Ctx *x, uint32_t a) { uint32_t b = ld32(x, a); float f; memcpy(&f, &b, 4); return f; }
static inline double ldf64(Ctx *x, uint32_t a) { uint64_t b = ((uint64_t)ld32(x, a) << 32) | ld32(x, a + 4); double d; memcpy(&d, &b, 8); return d; }
static inline void stf32(Ctx *x, uint32_t a, double v) { float f = (float)v; uint32_t b; memcpy(&b, &f, 4); st32(x, a, b); }
static inline void stf64(Ctx *x, uint32_t a, double v) { uint64_t b; memcpy(&b, &v, 8); st32(x, a, (uint32_t)(b >> 32)); st32(x, a + 4, (uint32_t)b); }
static inline void cmps(Ctx *x, int f, uint32_t a, uint32_t b) { int32_t p = (int32_t)a, q = (int32_t)b; x->cr[f] = p < q ? 8 : p > q ? 4 : 2; }
static inline void cmpu(Ctx *x, int f, uint32_t a, uint32_t b) { x->cr[f] = a < b ? 8 : a > b ? 4 : 2; }
static inline void fcmp(Ctx *x, int f, double a, double b) { x->cr[f] = a < b ? 8 : a > b ? 4 : a == b ? 2 : 1; }
static inline void cr0(Ctx *x, uint32_t v) { int32_t s = (int32_t)v; x->cr[0] = s < 0 ? 8 : s > 0 ? 4 : 2; }
static inline void crop(Ctx *x, uint32_t w) { x->cr[0] ^= (uint8_t)(w & 1); }
static inline uint32_t srawi(Ctx *x, uint32_t a, uint32_t n) { return (uint32_t)((int32_t)a >> n); }
static inline int bc(Ctx *x, uint32_t bo, uint32_t bi) {
    int ctr_ok = 1, cond_ok = 1;
    if (!(bo & 4)) { x->ctr -= 1; ctr_ok = (x->ctr != 0) == ((bo & 2) == 0); }
    if (!(bo & 16)) { int bit = (x->cr[bi / 4] >> (3 - bi % 4)) & 1; cond_ok = bit == ((bo & 8) != 0); }
    return ctr_ok && cond_ok;
}
static inline void call(Ctx *x, uint32_t target) { x->ctr = target; }
static inline void call_ctr(Ctx *x) { call(x, x->ctr); }
static inline void sync(void) {}
static inline void unimpl(Ctx *x, uint32_t w) { x->xer ^= w; }
#define fabs(x) __builtin_fabs(x)
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rotate_masks_run_from_bit_mb_to_bit_me_and_wrap() {
        assert_eq!(mask(0, 31), 0xffff_ffff);
        assert_eq!(mask(24, 31), 0x0000_00ff);
        assert_eq!(mask(0, 0), 0x8000_0000);
        assert_eq!(mask(28, 3), 0xf000_000f, "a mask that wraps round the end");
    }

    #[test]
    fn branch_targets_are_relative_and_signed() {
        assert_eq!(branch_target(0x1000, 0x4800_0010), Some(0x1010), "b +16");
        assert_eq!(branch_target(0x1000, 0x4bff_fff0), Some(0x0ff0), "b -16");
        assert_eq!(branch_target(0x1000, 0x4800_0012), None, "absolute branches are not followed");
        assert_eq!(branch_target(0x1000, 0x4182_0008), Some(0x1008), "beq +8");
        assert_eq!(branch_target(0x1000, 0x7c00_0000), None);
    }

    #[test]
    fn nothing_is_written_inside_a_repository() {
        let here = std::env::current_dir().unwrap();
        assert!(in_a_working_tree(&here.join("some/generated")), "the workspace is a git repository");
        assert!(!in_a_working_tree(&std::env::temp_dir().join("mh3u-codegen-bench-test-not-a-repo")));
    }
}
