//! The translator: the game's PowerPC code as C, one C function for each function of the game.
//!
//! Each instruction becomes a few C statements that give it the meaning the reference interpreter in `mh3u-ppc` gives it (the
//! interpreter is checked against QEMU; this against the interpreter, on the same random states). The statements work on a `Ctx` (the
//! registers) and use the helpers of `ppc.h`. Control flow inside a function is `goto`; a call is a C call; `blr` is `return`.
//!
//! Inside a function the general-purpose and floating point registers it uses live in C locals (`R_3`, `F_1`, `P_1`), not in the
//! `Ctx`: they are loaded from it on entry (`FILL`), written back to it before anything else can look at it (`SPILL`: a call, a jump
//! out, a return, a system or runtime hook) and loaded again after anything that may have changed it. The condition, link, count and
//! fixed-point exception registers stay in the `Ctx`; `c` is `restrict`, so guest stores (through `c->mem`) are known not to touch it.

pub mod discover;
pub mod toolchain;

use mh3u_ppc::{Insn, Op};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// The C header the generated code includes.
pub const RUNTIME_H: &str = include_str!("ppc.h");

/// How the generated C is compiled: the optimisation level, and two opt-in choices that make a library for this machine only
/// (`-march=native`) or optimise across files at link time (`-flto=thin`, which needs clang and lld).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CFlags {
    pub opt: u8,
    pub native: bool,
    pub lto: bool,
}

impl Default for CFlags {
    fn default() -> CFlags {
        CFlags {
            opt: 2,
            native: false,
            lto: false,
        }
    }
}

impl CFlags {
    /// The flags that decide what the generated C means and how fast it is, for compiling and (with LTO) linking. The library build
    /// and the tests that check the C against the interpreter both use these, so what is tested is what is built.
    ///
    /// `-fno-strict-aliasing`: guest memory is reached through byte pointers of every type. `-ffp-contract=off`: the compiler must not
    /// fuse a multiply and an add that the PowerPC does as two roundings (it would on a machine with FMA, so with `-march=native`).
    pub fn args(&self) -> Vec<String> {
        let mut v = vec![format!("-O{}", self.opt), "-fno-strict-aliasing".into(), "-ffp-contract=off".into()];
        if self.native {
            v.push("-march=native".into());
        }
        if self.lto {
            v.push("-flto=thin".into());
        }
        v
    }
}

/// What the translator needs to know about the function it is translating.
pub struct Function<'a> {
    pub start: u32,
    /// Where it ends (the address after its last instruction).
    pub end: u32,
    /// Where branches within it may go: the start of every block.
    pub labels: &'a BTreeSet<u32>,
    /// The starts of all the functions known, to tell a call or a tail jump that can be made directly.
    pub functions: &'a BTreeSet<u32>,
    /// The switch tables: the address of the `bctr` and how many branches the table after it holds.
    pub tables: &'a BTreeMap<u32, u32>,
    /// The import trampolines: the address a call goes to and the number of the import it stands for.
    pub imports: &'a BTreeMap<u32, u32>,
}

fn hex(v: u32) -> String {
    format!("{v:#x}u")
}

fn r(n: usize) -> String {
    format!("c->r[{n}]")
}

fn f(n: usize) -> String {
    format!("c->f[{n}].d")
}

/// The effective address of `d(ra)`.
fn ea_d(ra: usize, disp: i32) -> String {
    if ra == 0 {
        hex(disp as u32)
    } else {
        format!("{} + {}", r(ra), hex(disp as u32))
    }
}

/// The effective address of `ra + rb`.
fn ea_x(ra: usize, rb: usize) -> String {
    if ra == 0 { r(rb) } else { format!("{} + {}", r(ra), r(rb)) }
}

fn mask(mb: u32, me: u32) -> u32 {
    mh3u_ppc::interp::rotate_mask(mb, me)
}

/// The name of the C function for the PowerPC function at `address`.
pub fn function_name(address: u32) -> String {
    format!("f_{address:08x}")
}

fn label(address: u32) -> String {
    format!("L_{address:08x}")
}

/// One half of a paired-single register as a float (`i` 0 is the first half, kept in `f`; 1 the second, in `ps1`).
fn hf(n: usize, i: u32) -> String {
    format!("(float)c->{}[{n}].d", if i == 0 { "f" } else { "ps1" })
}

/// Sets one half of a paired-single register from a float.
fn sh(n: usize, i: u32, v: &str) -> String {
    format!("c->{}[{n}].d = (double)({v});", if i == 0 { "f" } else { "ps1" })
}

/// A call to a runtime hook: the registers are written back before it (the hook may look at them) and read again after it (it may
/// have changed them).
fn hook(call: &str) -> String {
    format!("{{ SPILL(); {call} FILL(); }}")
}

/// A statement that records the result into CR0 when the instruction has the record bit.
fn record(insn: &Insn, reg: usize) -> Vec<String> {
    if insn.rc() {
        vec![format!("rec(c, {});", r(reg))]
    } else {
        Vec::new()
    }
}

/// The C statements of one instruction at `addr`.
pub fn emit_insn(insn: Insn, addr: u32, fx: &Function) -> Vec<String> {
    use Op::*;
    let (rd, ra, rb) = (insn.rd(), insn.ra(), insn.rb());
    let oe = i32::from(insn.oe());
    let simm = insn.simm() as u32;
    let uimm = insn.uimm();
    let w = insn.word;
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| out.push(s);
    match insn.op {
        // ---- integer arithmetic
        Addi => push(format!(
            "{} = {};",
            r(rd),
            if ra == 0 { hex(simm) } else { format!("{} + {}", r(ra), hex(simm)) }
        )),
        Addis => {
            let imm = simm << 16;
            push(format!(
                "{} = {};",
                r(rd),
                if ra == 0 { hex(imm) } else { format!("{} + {}", r(ra), hex(imm)) }
            ));
        }
        Mulli => push(format!("{} = {} * {};", r(rd), r(ra), hex(simm))),
        Subfic => push(format!("{} = addx(c, ~{}, {}, 1, 1, 0);", r(rd), r(ra), hex(simm))),
        Addic => push(format!("{} = addx(c, {}, {}, 0, 1, 0);", r(rd), r(ra), hex(simm))),
        AddicDot => {
            push(format!("{} = addx(c, {}, {}, 0, 1, 0);", r(rd), r(ra), hex(simm)));
            push(format!("rec(c, {});", r(rd)));
        }
        Add | Addc | Adde | Addze | Addme | Subf | Subfc | Subfe | Subfze | Subfme | Neg => {
            let (a, b, cin, ca) = match insn.op {
                Add => (r(ra), r(rb), "0".to_string(), 0),
                Addc => (r(ra), r(rb), "0".to_string(), 1),
                Adde => (r(ra), r(rb), "carry(c)".to_string(), 1),
                Addze => (r(ra), "0".to_string(), "carry(c)".to_string(), 1),
                Addme => (r(ra), "0xffffffffu".to_string(), "carry(c)".to_string(), 1),
                Subf => (format!("~{}", r(ra)), r(rb), "1".to_string(), 0),
                Subfc => (format!("~{}", r(ra)), r(rb), "1".to_string(), 1),
                Subfe => (format!("~{}", r(ra)), r(rb), "carry(c)".to_string(), 1),
                Subfze => (format!("~{}", r(ra)), "0".to_string(), "carry(c)".to_string(), 1),
                Subfme => (format!("~{}", r(ra)), "0xffffffffu".to_string(), "carry(c)".to_string(), 1),
                _ => (format!("~{}", r(ra)), "0".to_string(), "1".to_string(), 0),
            };
            push(format!("{} = addx(c, {a}, {b}, {cin}, {ca}, {oe});", r(rd)));
            out.extend(record(&insn, rd));
        }
        Mullw => {
            push(format!("{} = mullw(c, {}, {}, {oe});", r(rd), r(ra), r(rb)));
            out.extend(record(&insn, rd));
        }
        Mulhw | Mulhwu => {
            let name = if insn.op == Mulhw { "mulhw" } else { "mulhwu" };
            push(format!("{} = {name}({}, {});", r(rd), r(ra), r(rb)));
            out.extend(record(&insn, rd));
        }
        Divw | Divwu => {
            let name = if insn.op == Divw { "divw" } else { "divwu" };
            push(format!("{} = {name}(c, {}, {}, {oe});", r(rd), r(ra), r(rb)));
            out.extend(record(&insn, rd));
        }
        Extsb | Extsh => {
            let name = if insn.op == Extsb { "ext8" } else { "ext16" };
            push(format!("{} = {name}({});", r(ra), r(rd)));
            out.extend(record(&insn, ra));
        }
        Cntlzw => {
            push(format!("{} = cntlzw({});", r(ra), r(rd)));
            out.extend(record(&insn, ra));
        }
        // ---- compares
        Cmpi => push(format!("cmps(c, {}, {}, {});", insn.crfd(), r(ra), hex(simm))),
        Cmpli => push(format!("cmpu(c, {}, {}, {});", insn.crfd(), r(ra), hex(uimm))),
        Cmp => push(format!("cmps(c, {}, {}, {});", insn.crfd(), r(ra), r(rb))),
        Cmpl => push(format!("cmpu(c, {}, {}, {});", insn.crfd(), r(ra), r(rb))),
        // ---- logic
        Ori => push(format!("{} = {} | {};", r(ra), r(rd), hex(uimm))),
        Oris => push(format!("{} = {} | {};", r(ra), r(rd), hex(uimm << 16))),
        Xori => push(format!("{} = {} ^ {};", r(ra), r(rd), hex(uimm))),
        Xoris => push(format!("{} = {} ^ {};", r(ra), r(rd), hex(uimm << 16))),
        AndiDot => {
            push(format!("{} = {} & {};", r(ra), r(rd), hex(uimm)));
            push(format!("rec(c, {});", r(ra)));
        }
        AndisDot => {
            push(format!("{} = {} & {};", r(ra), r(rd), hex(uimm << 16)));
            push(format!("rec(c, {});", r(ra)));
        }
        And | Or | Xor | Nand | Nor | Eqv | Andc | Orc => {
            let (s, b) = (r(rd), r(rb));
            let e = match insn.op {
                And => format!("{s} & {b}"),
                Or => format!("{s} | {b}"),
                Xor => format!("{s} ^ {b}"),
                Nand => format!("~({s} & {b})"),
                Nor => format!("~({s} | {b})"),
                Eqv => format!("~({s} ^ {b})"),
                Andc => format!("{s} & ~{b}"),
                _ => format!("{s} | ~{b}"),
            };
            push(format!("{} = {e};", r(ra)));
            out.extend(record(&insn, ra));
        }
        Slw | Srw => {
            let name = if insn.op == Slw { "slw" } else { "srw" };
            push(format!("{} = {name}({}, {});", r(ra), r(rd), r(rb)));
            out.extend(record(&insn, ra));
        }
        Sraw => {
            push(format!("{} = srawx(c, {}, {} & 0x3f);", r(ra), r(rd), r(rb)));
            out.extend(record(&insn, ra));
        }
        Srawi => {
            push(format!("{} = srawx(c, {}, {});", r(ra), r(rd), insn.sh()));
            out.extend(record(&insn, ra));
        }
        Rlwinm => {
            push(format!(
                "{} = rotl({}, {}) & {};",
                r(ra),
                r(rd),
                insn.sh(),
                hex(mask(insn.mb(), insn.me()))
            ));
            out.extend(record(&insn, ra));
        }
        Rlwnm => {
            push(format!(
                "{} = rotl({}, {}) & {};",
                r(ra),
                r(rd),
                r(rb),
                hex(mask(insn.mb(), insn.me()))
            ));
            out.extend(record(&insn, ra));
        }
        Rlwimi => {
            let m = mask(insn.mb(), insn.me());
            push(format!(
                "{} = (rotl({}, {}) & {}) | ({} & {});",
                r(ra),
                r(rd),
                insn.sh(),
                hex(m),
                r(ra),
                hex(!m)
            ));
            out.extend(record(&insn, ra));
        }
        // ---- branches
        B | Bc | Bclr | Bcctr => return emit_branch(insn, addr, fx),
        // ---- the condition register
        Mcrf => push(format!("setcrf(c, {}, crf(c, {}));", insn.crfd(), insn.crfs())),
        Crand | Crandc | Creqv | Crnand | Crnor | Cror | Crorc | Crxor => {
            let (a, b) = (format!("crbit(c, {ra})"), format!("crbit(c, {rb})"));
            let e = match insn.op {
                Crand => format!("{a} & {b}"),
                Crandc => format!("{a} & ({b} ^ 1)"),
                Creqv => format!("({a} ^ {b}) ^ 1"),
                Crnand => format!("({a} & {b}) ^ 1"),
                Crnor => format!("({a} | {b}) ^ 1"),
                Cror => format!("{a} | {b}"),
                Crorc => format!("{a} | ({b} ^ 1)"),
                _ => format!("{a} ^ {b}"),
            };
            push(format!("setcrbit(c, {rd}, {e});"));
        }
        Mfcr => push(format!("{} = c->cr;", r(rd))),
        Mtcrf => {
            let mut m = 0u32;
            for i in 0..8 {
                if insn.crm() & (0x80 >> i) != 0 {
                    m |= 0xf << (28 - 4 * i);
                }
            }
            push(format!("c->cr = (c->cr & {}) | ({} & {});", hex(!m), r(rd), hex(m)));
        }
        Mcrxr => {
            push(format!("setcrf(c, {}, c->xer >> 28);", insn.crfd()));
            push("c->xer &= 0x0fffffffu;".to_string());
        }
        // ---- special registers
        Mfspr | Mtspr => {
            let n = insn.spr();
            let place = match n {
                1 => Some("c->xer".to_string()),
                8 => Some("c->lr".to_string()),
                9 => Some("c->ctr".to_string()),
                912..=919 => Some(format!("c->gqr[{}]", n - 912)),
                _ => None,
            };
            match (place, insn.op) {
                (Some(p), Mfspr) => push(format!("{} = {p};", r(rd))),
                (Some(p), _) if n == 1 => push(format!("{p} = {} & 0xe000007fu;", r(rd))),
                (Some(p), _) => push(format!("{p} = {};", r(rd))),
                (None, _) => push(hook(&format!("rt_unsupported(c, {});", hex(w)))),
            }
        }
        Mftb => match insn.spr() {
            268 => push(format!("{} = (uint32_t)c->tb;", r(rd))),
            269 => push(format!("{} = (uint32_t)(c->tb >> 32);", r(rd))),
            _ => push(hook(&format!("rt_unsupported(c, {});", hex(w)))),
        },
        // ---- loads
        Lbz | Lbzu | Lbzx | Lbzux | Lhz | Lhzu | Lhzx | Lhzux | Lha | Lhau | Lhax | Lhaux | Lwz | Lwzu | Lwzx | Lwzux | Lhbrx | Lwbrx
        | Lwarx => {
            let indexed = matches!(
                insn.op,
                Lbzx | Lbzux | Lhzx | Lhzux | Lhax | Lhaux | Lwzx | Lwzux | Lhbrx | Lwbrx | Lwarx
            );
            let update = matches!(insn.op, Lbzu | Lbzux | Lhzu | Lhzux | Lhau | Lhaux | Lwzu | Lwzux);
            let ea = if indexed { ea_x(ra, rb) } else { ea_d(ra, insn.simm()) };
            let value = match insn.op {
                Lbz | Lbzu | Lbzx | Lbzux => "ld8(c, ea)",
                Lhz | Lhzu | Lhzx | Lhzux => "ld16(c, ea)",
                Lha | Lhau | Lhax | Lhaux => "ld16s(c, ea)",
                Lhbrx => "(uint32_t)__builtin_bswap16((uint16_t)ld16(c, ea))",
                Lwbrx => "bswap32(ld32(c, ea))",
                _ => "ld32(c, ea)",
            };
            let mut body = format!("uint32_t ea = {ea}; ");
            if insn.op == Lwarx {
                body += "c->reservation = ea; c->has_reservation = 1; ";
            }
            let _ = write!(body, "{} = {value};", r(rd));
            if update {
                let _ = write!(body, " {} = ea;", r(ra));
            }
            push(format!("{{ {body} }}"));
        }
        // unrolled, so every register is named by a constant (and can live in a local)
        Lmw => {
            let mut body = format!("{{ uint32_t ea = {};", ea_d(ra, insn.simm()));
            for (k, i) in (rd..32).enumerate() {
                let _ = write!(body, " c->r[{i}] = ld32(c, ea + {});", hex(4 * k as u32));
            }
            push(body + " }");
        }
        Lswi | Lswx => {
            let (ea, n) = if insn.op == Lswi {
                (if ra == 0 { "0u".to_string() } else { r(ra) }, insn.nb().to_string())
            } else {
                (ea_x(ra, rb), "(c->xer & 0x7f)".to_string())
            };
            if insn.op == Lswi {
                // the count is known: unrolled, with every register named by a constant
                let mut body = format!("{{ uint32_t ea = {ea};");
                for k in 0..insn.nb() as usize {
                    let q = (rd + k / 4) % 32;
                    if k % 4 == 0 {
                        let _ = write!(body, " c->r[{q}] = 0;");
                    }
                    let _ = write!(body, " c->r[{q}] |= ld8(c, ea + {}) << {};", hex(k as u32), 24 - 8 * (k % 4));
                }
                push(body + " }");
            } else {
                // the count is in XER: the registers are picked at run time, so this one works on the `Ctx` itself
                push(format!(
                    "{{ uint32_t ea = {ea}; SPILL(); uint32_t n = {n}; for (uint32_t k = 0; k < n; k++) {{ uint32_t q = ({rd} + k / 4) % 32; if (k % 4 == 0) c->r[q] = 0; c->r[q] |= ld8(c, ea + k) << (24 - 8 * (k % 4)); }} FILL(); }}"
                ));
            }
        }
        // ---- stores
        Stb | Stbu | Stbx | Stbux | Sth | Sthu | Sthx | Sthux | Stw | Stwu | Stwx | Stwux | Sthbrx | Stwbrx | Stwcx => {
            let indexed = matches!(insn.op, Stbx | Stbux | Sthx | Sthux | Stwx | Stwux | Sthbrx | Stwbrx | Stwcx);
            let update = matches!(insn.op, Stbu | Stbux | Sthu | Sthux | Stwu | Stwux);
            let ea = if indexed { ea_x(ra, rb) } else { ea_d(ra, insn.simm()) };
            let s = r(rd);
            let store = match insn.op {
                Stb | Stbu | Stbx | Stbux => format!("st8(c, ea, {s});"),
                Sth | Sthu | Sthx | Sthux => format!("st16(c, ea, {s});"),
                Sthbrx => format!("st16(c, ea, (uint32_t)__builtin_bswap16((uint16_t){s}));"),
                Stwbrx => format!("st32(c, ea, bswap32({s}));"),
                Stwcx => format!(
                    "int ok = c->has_reservation && c->reservation == ea; if (ok) st32(c, ea, {s}); c->has_reservation = 0; setcrf(c, 0, (ok ? 2 : 0) | ((c->xer >> 31) & 1));"
                ),
                _ => format!("st32(c, ea, {s});"),
            };
            let mut body = format!("uint32_t ea = {ea}; {store}");
            if update {
                let _ = write!(body, " {} = ea;", r(ra));
            }
            push(format!("{{ {body} }}"));
        }
        Stmw => {
            let mut body = format!("{{ uint32_t ea = {};", ea_d(ra, insn.simm()));
            for (k, i) in (rd..32).enumerate() {
                let _ = write!(body, " st32(c, ea + {}, c->r[{i}]);", hex(4 * k as u32));
            }
            push(body + " }");
        }
        Stswi | Stswx => {
            let (ea, n) = if insn.op == Stswi {
                (if ra == 0 { "0u".to_string() } else { r(ra) }, insn.nb().to_string())
            } else {
                (ea_x(ra, rb), "(c->xer & 0x7f)".to_string())
            };
            if insn.op == Stswi {
                let mut body = format!("{{ uint32_t ea = {ea};");
                for k in 0..insn.nb() as usize {
                    let q = (rd + k / 4) % 32;
                    let _ = write!(body, " st8(c, ea + {}, c->r[{q}] >> {});", hex(k as u32), 24 - 8 * (k % 4));
                }
                push(body + " }");
            } else {
                push(format!(
                    "{{ uint32_t ea = {ea}; SPILL(); uint32_t n = {n}; for (uint32_t k = 0; k < n; k++) {{ uint32_t q = ({rd} + k / 4) % 32; st8(c, ea + k, c->r[q] >> (24 - 8 * (k % 4))); }} }}"
                ));
            }
        }
        // ---- floating point memory
        Lfs | Lfsu | Lfsx | Lfsux | Lfd | Lfdu | Lfdx | Lfdux => {
            let indexed = matches!(insn.op, Lfsx | Lfsux | Lfdx | Lfdux);
            let update = matches!(insn.op, Lfsu | Lfsux | Lfdu | Lfdux);
            let ea = if indexed { ea_x(ra, rb) } else { ea_d(ra, insn.simm()) };
            let single = matches!(insn.op, Lfs | Lfsu | Lfsx | Lfsux);
            let mut body = format!("uint32_t ea = {ea}; ");
            if single {
                let _ = write!(body, "c->f[{rd}].d = widen(ld32(c, ea)); c->ps1[{rd}] = c->f[{rd}];");
            } else {
                let _ = write!(body, "c->f[{rd}].u = ld64(c, ea);");
            }
            if update {
                let _ = write!(body, " {} = ea;", r(ra));
            }
            push(format!("{{ {body} }}"));
        }
        Stfs | Stfsu | Stfsx | Stfsux | Stfd | Stfdu | Stfdx | Stfdux | Stfiwx => {
            let indexed = matches!(insn.op, Stfsx | Stfsux | Stfdx | Stfdux | Stfiwx);
            let update = matches!(insn.op, Stfsu | Stfsux | Stfdu | Stfdux);
            let ea = if indexed { ea_x(ra, rb) } else { ea_d(ra, insn.simm()) };
            let store = match insn.op {
                Stfs | Stfsu | Stfsx | Stfsux => format!("st32(c, ea, narrow(c->f[{rd}].u));"),
                Stfiwx => format!("st32(c, ea, (uint32_t)c->f[{rd}].u);"),
                _ => format!("st64(c, ea, c->f[{rd}].u);"),
            };
            let mut body = format!("uint32_t ea = {ea}; {store}");
            if update {
                let _ = write!(body, " {} = ea;", r(ra));
            }
            push(format!("{{ {body} }}"));
        }
        // ---- floating point arithmetic (the frC register is bits 6 to 10)
        Fadd | Fsub | Fmul | Fdiv | Fmadd | Fmsub | Fnmadd | Fnmsub => {
            let (a, b, cc) = (f(ra), f(rb), f(insn.rc_f()));
            let e = match insn.op {
                Fadd => format!("canon3({a} + {b}, {a}, {b}, {b})"),
                Fsub => format!("canon3({a} - {b}, {a}, {b}, {b})"),
                Fmul => format!("canon3({a} * {cc}, {a}, {cc}, {cc})"),
                Fdiv => format!("canon3({a} / {b}, {a}, {b}, {b})"),
                Fmadd => format!("canon3(fma({a}, {cc}, {b}), {a}, {cc}, {b})"),
                Fmsub => format!("canon3(fma({a}, {cc}, -{b}), {a}, {cc}, {b})"),
                Fnmadd => format!("negif(canon3(fma({a}, {cc}, {b}), {a}, {cc}, {b}))"),
                _ => format!("negif(canon3(fma({a}, {cc}, -{b}), {a}, {cc}, {b}))"),
            };
            push(format!("{} = {e};", f(rd)));
        }
        Fadds | Fsubs | Fmuls | Fdivs | Fmadds | Fmsubs | Fnmadds | Fnmsubs => {
            let (x, y, z) = (
                format!("tof({})", f(ra)),
                format!("tof({})", f(rb)),
                format!("tof({})", f(insn.rc_f())),
            );
            let e = match insn.op {
                Fadds => format!("canon3f({x} + {y}, {x}, {y}, {y})"),
                Fsubs => format!("canon3f({x} - {y}, {x}, {y}, {y})"),
                Fmuls => format!("canon3f({x} * {z}, {x}, {z}, {z})"),
                Fdivs => format!("canon3f({x} / {y}, {x}, {y}, {y})"),
                Fmadds => format!("canon3f(fmaf({x}, {z}, {y}), {x}, {z}, {y})"),
                Fmsubs => format!("canon3f(fmaf({x}, {z}, -{y}), {x}, {z}, {y})"),
                Fnmadds => format!("negiff(canon3f(fmaf({x}, {z}, {y}), {x}, {z}, {y}))"),
                _ => format!("negiff(canon3f(fmaf({x}, {z}, -{y}), {x}, {z}, {y}))"),
            };
            push(format!("{} = (double){e};", f(rd)));
        }
        Fsqrt => push(format!("{} = canon3(sqrt({}), {}, {}, {});", f(rd), f(rb), f(rb), f(rb), f(rb))),
        Fsqrts => push(format!(
            "{} = (double)canon3f(sqrtf(tof({})), tof({}), tof({}), tof({}));",
            f(rd),
            f(rb),
            f(rb),
            f(rb),
            f(rb)
        )),
        Fres => push(format!(
            "{} = (double)canon3f(1.0f / tof({}), tof({}), tof({}), tof({}));",
            f(rd),
            f(rb),
            f(rb),
            f(rb),
            f(rb)
        )),
        Frsqrte => push(format!(
            "{} = canon3(1.0 / sqrt({}), {}, {}, {});",
            f(rd),
            f(rb),
            f(rb),
            f(rb),
            f(rb)
        )),
        Fsel => push(format!("{} = {} >= 0.0 ? {} : {};", f(rd), f(ra), f(insn.rc_f()), f(rb))),
        Frsp => push(format!(
            "{} = (double)canon3f(tof({}), tof({}), tof({}), tof({}));",
            f(rd),
            f(rb),
            f(rb),
            f(rb),
            f(rb)
        )),
        Fctiwz => push(format!("c->f[{rd}].u = cvt_i32({}, 1);", f(rb))),
        Fctiw => push(format!("c->f[{rd}].u = cvt_i32({}, 0);", f(rb))),
        Fcmpu | Fcmpo => push(format!("fcmp(c, {}, {}, {});", insn.crfd(), f(ra), f(rb))),
        Fmr => push(format!("c->f[{rd}].u = c->f[{rb}].u;")),
        Fneg => push(format!("c->f[{rd}].u = c->f[{rb}].u ^ (1ull << 63);")),
        Fabs => push(format!("c->f[{rd}].u = c->f[{rb}].u & ~(1ull << 63);")),
        Fnabs => push(format!("c->f[{rd}].u = c->f[{rb}].u | (1ull << 63);")),
        Mffs => push(format!("c->f[{rd}].u = 0xfff8000000000000ull | c->fpscr;")),
        Mtfsf => {
            let fm = (w >> 17) & 0xff;
            let mut m = 0u32;
            for i in 0..8 {
                if fm & (0x80 >> i) != 0 {
                    m |= 0xf << (28 - 4 * i);
                }
            }
            push(format!(
                "c->fpscr = (c->fpscr & {}) | ((uint32_t)c->f[{rb}].u & {});",
                hex(!m),
                hex(m)
            ));
        }
        Mtfsfi => {
            let shift = 28 - 4 * insn.crfd() as u32;
            let imm = (w >> 12) & 0xf;
            push(format!("c->fpscr = (c->fpscr & {}) | {};", hex(!(0xf << shift)), hex(imm << shift)));
        }
        Mtfsb0 => push(format!("c->fpscr &= {};", hex(!(1u32 << (31 - rd as u32))))),
        Mtfsb1 => push(format!("c->fpscr |= {};", hex(1u32 << (31 - rd as u32)))),
        Mcrfs => {
            let shift = 28 - 4 * insn.crfs() as u32;
            push(format!("setcrf(c, {}, (c->fpscr >> {shift}) & 0xf);", insn.crfd()));
            push(format!("c->fpscr &= {};", hex(!(0xf << shift))));
        }
        // ---- paired singles
        PsAdd | PsSub | PsMul | PsDiv | PsMadd | PsMsub | PsNmadd | PsNmsub => {
            let c_reg = insn.rc_f();
            let mut body = String::from("{ ");
            for i in 0..2 {
                let (a, b, cc) = (hf(ra, i), hf(rb, i), hf(c_reg, i));
                let e = match insn.op {
                    PsAdd => format!("canon3f({a} + {b}, {a}, {b}, {b})"),
                    PsSub => format!("canon3f({a} - {b}, {a}, {b}, {b})"),
                    PsMul => format!("canon3f({a} * {cc}, {a}, {cc}, {cc})"),
                    PsDiv => format!("canon3f({a} / {b}, {a}, {b}, {b})"),
                    PsMadd => format!("canon3f(fmaf({a}, {cc}, {b}), {a}, {cc}, {b})"),
                    PsMsub => format!("canon3f(fmaf({a}, {cc}, -{b}), {a}, {cc}, {b})"),
                    PsNmadd => format!("negiff(canon3f(fmaf({a}, {cc}, {b}), {a}, {cc}, {b}))"),
                    _ => format!("negiff(canon3f(fmaf({a}, {cc}, -{b}), {a}, {cc}, {b}))"),
                };
                let _ = write!(body, "float r{i} = {e}; ");
            }
            let _ = write!(body, "{} {} }}", sh(rd, 0, "r0"), sh(rd, 1, "r1"));
            push(body);
        }
        PsMuls0 | PsMuls1 | PsMadds0 | PsMadds1 => {
            let which = u32::from(matches!(insn.op, PsMuls1 | PsMadds1));
            let mut body = format!("{{ float cc = {}; ", hf(insn.rc_f(), which));
            for i in 0..2 {
                let (a, b) = (hf(ra, i), hf(rb, i));
                let e = if matches!(insn.op, PsMuls0 | PsMuls1) {
                    format!("canon3f({a} * cc, {a}, cc, cc)")
                } else {
                    format!("canon3f(fmaf({a}, cc, {b}), {a}, cc, {b})")
                };
                let _ = write!(body, "float r{i} = {e}; ");
            }
            let _ = write!(body, "{} {} }}", sh(rd, 0, "r0"), sh(rd, 1, "r1"));
            push(body);
        }
        PsSum0 | PsSum1 => {
            let c_reg = insn.rc_f();
            let (a0, b1) = (hf(ra, 0), hf(rb, 1));
            let sum = format!("canon3f({a0} + {b1}, {a0}, {b1}, {b1})");
            if insn.op == PsSum0 {
                push(format!(
                    "{{ float s = {sum}; float k = {}; {} {} }}",
                    hf(c_reg, 1),
                    sh(rd, 0, "s"),
                    sh(rd, 1, "k")
                ));
            } else {
                push(format!(
                    "{{ float s = {sum}; float k = {}; {} {} }}",
                    hf(c_reg, 0),
                    sh(rd, 0, "k"),
                    sh(rd, 1, "s")
                ));
            }
        }
        PsSel => {
            let c_reg = insn.rc_f();
            let mut body = String::from("{ ");
            for i in 0..2 {
                let _ = write!(body, "float r{i} = {} >= 0.0f ? {} : {}; ", hf(ra, i), hf(c_reg, i), hf(rb, i));
            }
            let _ = write!(body, "{} {} }}", sh(rd, 0, "r0"), sh(rd, 1, "r1"));
            push(body);
        }
        PsRes | PsRsqrte => {
            let mut body = String::from("{ ");
            for i in 0..2 {
                let b = hf(rb, i);
                let e = if insn.op == PsRes {
                    format!("1.0f / {b}")
                } else {
                    format!("1.0f / sqrtf({b})")
                };
                let _ = write!(body, "float r{i} = canon3f({e}, {b}, {b}, {b}); ");
            }
            let _ = write!(body, "{} {} }}", sh(rd, 0, "r0"), sh(rd, 1, "r1"));
            push(body);
        }
        PsNeg | PsMr | PsNabs | PsAbs => {
            let f = |x: String| match insn.op {
                PsNeg => format!("{x} ^ (1ull << 63)"),
                PsNabs => format!("{x} | (1ull << 63)"),
                PsAbs => format!("{x} & ~(1ull << 63)"),
                _ => x,
            };
            push(format!(
                "{{ uint64_t b0 = c->f[{rb}].u, b1 = c->ps1[{rb}].u; c->f[{rd}].u = {}; c->ps1[{rd}].u = {}; }}",
                f("b0".to_string()),
                f("b1".to_string())
            ));
        }
        PsMerge00 | PsMerge01 | PsMerge10 | PsMerge11 => {
            let (x, y) = match insn.op {
                PsMerge00 => ("a0", "b0"),
                PsMerge01 => ("a0", "b1"),
                PsMerge10 => ("a1", "b0"),
                _ => ("a1", "b1"),
            };
            push(format!(
                "{{ uint64_t a0 = c->f[{ra}].u, a1 = c->ps1[{ra}].u, b0 = c->f[{rb}].u, b1 = c->ps1[{rb}].u; c->f[{rd}].u = {x}; c->ps1[{rd}].u = {y}; }}"
            ));
        }
        PsCmpu0 | PsCmpo0 | PsCmpu1 | PsCmpo1 => {
            let i = u32::from(matches!(insn.op, PsCmpu1 | PsCmpo1));
            push(format!("fcmp(c, {}, (double){}, (double){});", insn.crfd(), hf(ra, i), hf(rb, i)));
        }
        PsqL | PsqLu | PsqLx | PsqLux => {
            let indexed = matches!(insn.op, PsqLx | PsqLux);
            let ea = if indexed { ea_x(ra, rb) } else { ea_d(ra, insn.psq_d()) };
            let (w, i) = if indexed {
                (insn.psqx_w(), insn.psqx_i())
            } else {
                (insn.psq_w(), insn.psq_i())
            };
            let mut body = format!(
                "uint32_t ea = {ea}; psq_load(c, &c->f[{rd}], &c->ps1[{rd}], ea, {}, {i});",
                u32::from(w)
            );
            if matches!(insn.op, PsqLu | PsqLux) {
                let _ = write!(body, " {} = ea;", r(ra));
            }
            push(format!("{{ {body} }}"));
        }
        PsqSt | PsqStu | PsqStx | PsqStux => {
            let indexed = matches!(insn.op, PsqStx | PsqStux);
            let ea = if indexed { ea_x(ra, rb) } else { ea_d(ra, insn.psq_d()) };
            let (w, i) = if indexed {
                (insn.psqx_w(), insn.psqx_i())
            } else {
                (insn.psq_w(), insn.psq_i())
            };
            let mut body = format!(
                "uint32_t ea = {ea}; psq_store(c, c->f[{rd}].u, c->ps1[{rd}].u, ea, {}, {i});",
                u32::from(w)
            );
            if matches!(insn.op, PsqStu | PsqStux) {
                let _ = write!(body, " {} = ea;", r(ra));
            }
            push(format!("{{ {body} }}"));
        }
        // ---- cache and synchronisation
        Dcbz => push(format!("zero32(c, {});", ea_x(ra, rb))),
        Dcbf | Dcbi | Dcbst | Dcbt | Dcbtst | Icbi | Sync | Isync | Eieio => push(format!("/* {} */", insn.op.name())),
        // ---- traps and the system
        Sc => push(hook("rt_syscall(c);")),
        Tw | Twi => {
            let to = rd as u32;
            let (a, b) = (
                format!("(int32_t){}", r(ra)),
                if insn.op == Twi {
                    format!("(int32_t){simm}")
                } else {
                    format!("(int32_t){}", r(rb))
                },
            );
            let (ua, ub) = (r(ra), if insn.op == Twi { hex(simm) } else { r(rb) });
            let mut conds = Vec::new();
            if to & 16 != 0 {
                conds.push(format!("{a} < {b}"));
            }
            if to & 8 != 0 {
                conds.push(format!("{a} > {b}"));
            }
            if to & 4 != 0 {
                conds.push(format!("{ua} == {ub}"));
            }
            if to & 2 != 0 {
                conds.push(format!("{ua} < {ub}"));
            }
            if to & 1 != 0 {
                conds.push(format!("{ua} > {ub}"));
            }
            if !conds.is_empty() {
                push(format!("if ({}) {}", conds.join(" || "), hook(&format!("rt_trap(c, {});", hex(w)))));
            }
        }
        _ => push(hook(&format!("rt_unsupported(c, {}); /* {} */", hex(w), insn.op.name()))),
    }
    out
}

/// A branch: `b`, `bc`, `bclr`, `bcctr`.
fn emit_branch(insn: Insn, addr: u32, fx: &Function) -> Vec<String> {
    let next = addr.wrapping_add(4);
    let lk = insn.lk();
    let mut out = Vec::new();
    let inside = |t: u32| (fx.start..fx.end).contains(&t) && fx.labels.contains(&t);
    // a jump to another place: a tail call when it is a function, a runtime jump when it is not
    let leave = |t: u32| -> Vec<String> {
        if fx.functions.contains(&t) {
            vec![format!("SPILL(); TAIL({});", function_name(t))]
        } else if let Some(n) = fx.imports.get(&t) {
            vec![format!("SPILL(); rt_import(c, {n}); return;")]
        } else {
            vec![format!("SPILL(); rt_jump(c, {});", hex(t)), "return;".to_string()]
        }
    };
    let call = |t: u32| -> String {
        if fx.functions.contains(&t) {
            hook(&format!("{}(c);", function_name(t)))
        } else if let Some(n) = fx.imports.get(&t) {
            hook(&format!("rt_import(c, {n});"))
        } else {
            hook(&format!("rt_call(c, {});", hex(t)))
        }
    };
    match insn.op {
        Op::B => {
            let t = if insn.aa() {
                insn.li() as u32
            } else {
                addr.wrapping_add(insn.li() as u32)
            };
            if lk {
                if t == next {
                    // `bl` to the next instruction: how position-independent code reads its own address
                    out.push(format!("c->lr = {};", hex(next)));
                } else {
                    out.push(format!("c->lr = {};", hex(next)));
                    out.push(call(t));
                }
            } else if inside(t) {
                out.push(format!("goto {};", label(t)));
            } else {
                out.extend(leave(t));
            }
        }
        Op::Bc => {
            let t = if insn.aa() {
                insn.bd() as u32
            } else {
                addr.wrapping_add(insn.bd() as u32)
            };
            let cond = format!("bcond(c, {}, {}, 1)", insn.bo(), insn.bi());
            if lk {
                out.push(format!(
                    "if ({cond}) {{ c->lr = {}; {} }} else {{ c->lr = {}; }}",
                    hex(next),
                    call(t),
                    hex(next)
                ));
            } else if inside(t) {
                out.push(format!("if ({cond}) goto {};", label(t)));
            } else {
                out.push(format!("if ({cond}) {{ {} }}", leave(t).join(" ")));
            }
        }
        Op::Bclr => {
            let always = insn.bo() & 20 == 20;
            if always && !lk {
                out.push("SPILL(); return;".to_string());
            } else if always {
                out.push(format!(
                    "{{ uint32_t t = c->lr & ~3u; c->lr = {}; {} }}",
                    hex(next),
                    hook("rt_call(c, t);")
                ));
            } else {
                let cond = format!("bcond(c, {}, {}, 1)", insn.bo(), insn.bi());
                if lk {
                    out.push(format!(
                        "{{ uint32_t t = c->lr & ~3u; int go = {cond}; c->lr = {}; if (go) {} }}",
                        hex(next),
                        hook("rt_call(c, t);")
                    ));
                } else {
                    out.push(format!("if ({cond}) {{ SPILL(); return; }}"));
                }
            }
        }
        _ => {
            // bcctr
            let always = insn.bo() & 20 == 20;
            let go = if always {
                "1".to_string()
            } else {
                format!("bcond(c, {}, {}, 0)", insn.bo(), insn.bi())
            };
            if lk {
                out.push(format!(
                    "{{ uint32_t t = c->ctr & ~3u; int go = {go}; c->lr = {}; if (go) {} }}",
                    hex(next),
                    hook("rt_call(c, t);")
                ));
            } else if let (Some(&n), true) = (fx.tables.get(&addr), always) {
                // a switch: the table is `n` branches right after this instruction, and the target is one of them
                let mut cases = String::new();
                for k in 0..n {
                    let _ = write!(cases, "case {k}: goto {}; ", label(next + 4 * k));
                }
                out.push(format!(
                    "{{ uint32_t k = (c->ctr - {}) >> 2; switch (k) {{ {cases}default: SPILL(); rt_jump(c, c->ctr & ~3u); return; }} }}",
                    hex(next)
                ));
            } else {
                out.push(format!("if ({go}) {{ SPILL(); rt_jump(c, c->ctr & ~3u); return; }}"));
            }
        }
    }
    out
}

/// Where the blocks of a function start: its first instruction, every branch target inside it, and what follows a branch.
pub fn block_starts(start: u32, words: &[u32]) -> BTreeSet<u32> {
    let end = start + 4 * words.len() as u32;
    let mut set = BTreeSet::from([start]);
    for (i, &w) in words.iter().enumerate() {
        let addr = start + 4 * i as u32;
        let Some(insn) = Insn::decode(w) else { continue };
        match insn.op {
            Op::B | Op::Bc => {
                let t = if insn.op == Op::B {
                    if insn.aa() {
                        insn.li() as u32
                    } else {
                        addr.wrapping_add(insn.li() as u32)
                    }
                } else if insn.aa() {
                    insn.bd() as u32
                } else {
                    addr.wrapping_add(insn.bd() as u32)
                };
                if (start..end).contains(&t) {
                    set.insert(t);
                }
                if addr + 4 < end {
                    set.insert(addr + 4);
                }
            }
            Op::Bclr | Op::Bcctr if addr + 4 < end => {
                set.insert(addr + 4);
            }
            _ => {}
        }
    }
    set
}

/// The switch tables of a function: the compiler puts a table of `b` instructions right after the `bctr` and makes the jump go to the
/// address after it plus four times the index. Returns the address of each such `bctr` and the number of entries (the run of
/// branches after it, cut short by the `cmplwi` that bounds the index when that is smaller).
pub fn switch_tables(start: u32, words: &[u32]) -> BTreeMap<u32, u32> {
    let mut found = BTreeMap::new();
    for (i, &w) in words.iter().enumerate() {
        if w != 0x4e80_0420 {
            continue;
        }
        let addr = start + 4 * i as u32;
        let Some(m) = (i.saturating_sub(3)..i).rev().find(|&j| words[j] & 0xfc1f_ffff == 0x7c09_03a6) else {
            continue;
        };
        // the table's address is built by `lis` and `addi`/`addic`/`ori` in some register (then added to the scaled index, or not)
        // for each register: a value known outright, or a known part added to something unknown (the index)
        let mut known: [Option<u32>; 32] = [None; 32];
        let mut part: [Option<u32>; 32] = [None; 32];
        let mut built = false;
        for &v in &words[i.saturating_sub(16)..m] {
            let (op, rd, ra, imm) = (v >> 26, ((v >> 21) & 31) as usize, ((v >> 16) & 31) as usize, i32::from(v as i16));
            let (k, p) = match op {
                15 if ra == 0 => (Some((imm << 16) as u32), None),
                15 => (
                    known[ra].map(|x| x.wrapping_add((imm << 16) as u32)),
                    Some(
                        part[ra]
                            .or(known[ra].map(|_| 0))
                            .map_or((imm << 16) as u32, |x| x.wrapping_add((imm << 16) as u32)),
                    ),
                ),
                14 if ra == 0 => (Some(imm as u32), None),
                14 | 12 => (
                    known[ra].map(|x| x.wrapping_add(imm as u32)),
                    part[ra].map(|x| x.wrapping_add(imm as u32)),
                ),
                // ori rA, rS, imm: the destination is in the second field
                24 => {
                    let (rs, dest) = (rd, ra);
                    known[dest] = known[rs].map(|x| x | (v & 0xffff));
                    part[dest] = part[rs].map(|x| x | (v & 0xffff));
                    continue;
                }
                // other instructions that write their first field
                13 | 7 | 8 | 10 | 11 | 31 | 32..=35 | 40..=43 | 46 | 20..=23 | 28 | 29 => (None, None),
                _ => continue,
            };
            known[rd] = k;
            part[rd] = p;
            if k == Some(addr + 4) || p == Some(addr + 4) {
                built = true;
            }
        }
        if !built {
            continue;
        }
        let run = words[i + 1..].iter().take_while(|&&x| x >> 26 == 18 && x & 3 == 0).count() as u32;
        let bound = (i.saturating_sub(14)..i)
            .rev()
            .find(|&j| words[j] >> 26 == 10)
            .map(|j| (words[j] & 0xffff) + 1);
        let n = bound.map_or(run, |b| b.min(run));
        if n > 0 {
            found.insert(addr, n);
        }
    }
    found
}

/// One C function for the PowerPC function whose words start at `start`.
pub fn emit_function(start: u32, words: &[u32], functions: &BTreeSet<u32>) -> String {
    emit_function_with(start, words, functions, &BTreeMap::new())
}

/// The same, with the import trampolines known: a call to one is `rt_import(c, number)`.
pub fn emit_function_with(start: u32, words: &[u32], functions: &BTreeSet<u32>, imports: &BTreeMap<u32, u32>) -> String {
    let end = start + 4 * words.len() as u32;
    let tables = switch_tables(start, words);
    let mut labels = block_starts(start, words);
    for (&at, &n) in &tables {
        labels.extend((0..n).map(|k| at + 4 + 4 * k));
    }
    let fx = Function {
        tables: &tables,
        imports,
        start,
        end,
        labels: &labels,
        functions,
    };
    let mut body = String::new();
    for (i, &w) in words.iter().enumerate() {
        let addr = start + 4 * i as u32;
        if labels.contains(&addr) {
            let _ = writeln!(body, "{}:;", label(addr));
        }
        match Insn::decode(w) {
            Some(insn) => {
                for s in emit_insn(insn, addr, &fx) {
                    let _ = writeln!(body, "    {s}");
                }
            }
            None => {
                let _ = writeln!(
                    body,
                    "    {}",
                    hook(&format!("rt_unsupported(c, {}); /* not an instruction */", hex(w)))
                );
            }
        }
    }
    // running off the end of the function continues in whatever follows it
    if functions.contains(&end) {
        let _ = writeln!(body, "    SPILL(); TAIL({});", function_name(end));
    } else {
        body.push_str("    SPILL();\n");
    }
    let (body, regs) = localize(&body);
    let mut out = String::new();
    // the registers this function keeps in locals: written back by SPILL (the ones it changes), read again by FILL (all it uses)
    let (mut spill, mut fill, mut decls) = (String::new(), String::new(), String::new());
    for &(kind, n, dirty) in &regs {
        let (local, field, ty) = match kind {
            Reg::Gpr => (format!("R_{n}"), format!("c->r[{n}]"), "uint32_t"),
            Reg::Fpr => (format!("F_{n}"), format!("c->f[{n}]"), "FR"),
            Reg::Ps1 => (format!("P_{n}"), format!("c->ps1[{n}]"), "FR"),
        };
        if dirty {
            let _ = write!(spill, " {field} = {local};");
        }
        let _ = write!(fill, " {local} = {field};");
        let _ = writeln!(decls, "    {ty} {local};");
    }
    let _ = writeln!(out, "#define SPILL() do {{{spill} }} while (0)");
    let _ = writeln!(out, "#define FILL() do {{{fill} }} while (0)");
    let _ = writeln!(out, "void {}(Ctx *restrict c) {{", function_name(start));
    out.push_str(&decls);
    let _ = writeln!(out, "    ENTER(c, {});", hex(start));
    out.push_str("    FILL();\n");
    out.push_str(&body);
    out.push_str("}\n#undef SPILL\n#undef FILL\n");
    out
}

/// A register of the `Ctx` that a function may keep in a local.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reg {
    Gpr,
    Fpr,
    Ps1,
}

/// Turns every `c->r[N]`, `c->f[N]` and `c->ps1[N]` with a constant `N` into its local (`R_N`, `F_N`, `P_N`), and says which registers
/// were used and whether each is ever written (assigned, or its address taken). An access with an index computed at run time (the
/// string instructions with the count in XER) stays on the `Ctx`; the statement that makes it spills and fills around it.
fn localize(body: &str) -> (String, Vec<(Reg, u32, bool)>) {
    let mut out = String::with_capacity(body.len());
    let mut used: BTreeMap<(Reg, u32), bool> = BTreeMap::new();
    let mut rest = body;
    while let Some(at) = rest.find("c->") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 3..];
        let found = [("r[", Reg::Gpr, "R_"), ("f[", Reg::Fpr, "F_"), ("ps1[", Reg::Ps1, "P_")]
            .into_iter()
            .find_map(|(prefix, kind, local)| {
                let digits = after.strip_prefix(prefix)?;
                let len = digits.bytes().take_while(u8::is_ascii_digit).count();
                if len == 0 || !digits[len..].starts_with(']') {
                    return None;
                }
                let n: u32 = digits[..len].parse().ok()?;
                Some((kind, local, n, prefix.len() + len + 1))
            });
        match found {
            Some((kind, local, n, skip)) => {
                let taken = out.ends_with('&') && !out.ends_with("&&");
                let tail = &after[skip..];
                // what follows the register: a field of the union, then perhaps an assignment (`=` that is not `==`, or `|=`)
                let t = tail
                    .strip_prefix(".u")
                    .or_else(|| tail.strip_prefix(".d"))
                    .unwrap_or(tail)
                    .trim_start();
                let assigned = (t.starts_with('=') && !t.starts_with("==")) || t.starts_with("|=");
                let dirty = used.entry((kind, n)).or_insert(false);
                *dirty |= taken || assigned;
                let _ = write!(out, "{local}{n}");
                rest = tail;
            }
            None => {
                out.push_str("c->");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    (out, used.into_iter().map(|((k, n), d)| (k, n, d)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_switch_table_after_the_bctr_is_found_and_bounded() {
        let start = 0x1000;
        let base = start + 4 * 7;
        let lo = base as u16;
        let words = [
            0x2803_0001,                 // cmplwi r3, 1
            0x4181_0020,                 // bgt
            0x5464_103a,                 // slwi r4, r3, 2
            0x3ca4_0000,                 // addis r5, r4, 0
            0x38a5_0000 | u32::from(lo), // addi r5, r5, lo
            0x7ca9_03a6,                 // mtctr r5
            0x4e80_0420,                 // bctr
            0x4800_0010,                 // b
            0x4800_000c,                 // b
            0x4800_0008,                 // b (beyond the bound of two)
        ];
        assert_eq!(switch_tables(start, &words), BTreeMap::from([(start + 24, 2)]));
        assert!(emit_function(start, &words, &BTreeSet::new()).contains("switch (k)"));
    }
}
