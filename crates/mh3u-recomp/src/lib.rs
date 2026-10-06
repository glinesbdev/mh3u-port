//! The translator: the game's PowerPC code as C, one C function for each function of the game.
//!
//! Each instruction becomes a few C statements that give it the meaning the reference interpreter in `mh3u-ppc` gives it (the
//! interpreter is checked against QEMU; this against the interpreter, on the same random states). The statements work on a `Ctx` (the
//! registers) and use the helpers of `ppc.h`. Control flow inside a function is `goto`; a call is a C call; `blr` is `return`.

pub mod discover;

use mh3u_ppc::{Insn, Op};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// The C header the generated code includes.
pub const RUNTIME_H: &str = include_str!("ppc.h");

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
                (None, _) => push(format!("rt_unsupported(c, {});", hex(w))),
            }
        }
        Mftb => match insn.spr() {
            268 => push(format!("{} = (uint32_t)c->tb;", r(rd))),
            269 => push(format!("{} = (uint32_t)(c->tb >> 32);", r(rd))),
            _ => push(format!("rt_unsupported(c, {});", hex(w))),
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
        Lmw => push(format!(
            "{{ uint32_t ea = {}; for (int i = {rd}; i < 32; i++) {{ c->r[i] = ld32(c, ea); ea += 4; }} }}",
            ea_d(ra, insn.simm())
        )),
        Lswi | Lswx => {
            let (ea, n) = if insn.op == Lswi {
                (if ra == 0 { "0u".to_string() } else { r(ra) }, insn.nb().to_string())
            } else {
                (ea_x(ra, rb), "(c->xer & 0x7f)".to_string())
            };
            push(format!(
                "{{ uint32_t ea = {ea}; uint32_t n = {n}; for (uint32_t k = 0; k < n; k++) {{ uint32_t q = ({rd} + k / 4) % 32; if (k % 4 == 0) c->r[q] = 0; c->r[q] |= ld8(c, ea + k) << (24 - 8 * (k % 4)); }} }}"
            ));
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
        Stmw => push(format!(
            "{{ uint32_t ea = {}; for (int i = {rd}; i < 32; i++) {{ st32(c, ea, c->r[i]); ea += 4; }} }}",
            ea_d(ra, insn.simm())
        )),
        Stswi | Stswx => {
            let (ea, n) = if insn.op == Stswi {
                (if ra == 0 { "0u".to_string() } else { r(ra) }, insn.nb().to_string())
            } else {
                (ea_x(ra, rb), "(c->xer & 0x7f)".to_string())
            };
            push(format!(
                "{{ uint32_t ea = {ea}; uint32_t n = {n}; for (uint32_t k = 0; k < n; k++) {{ uint32_t q = ({rd} + k / 4) % 32; st8(c, ea + k, c->r[q] >> (24 - 8 * (k % 4))); }} }}"
            ));
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
                let (a, b, cc) = (
                    format!("hf(c, {ra}, {i})"),
                    format!("hf(c, {rb}, {i})"),
                    format!("hf(c, {c_reg}, {i})"),
                );
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
            let _ = write!(body, "sh(c, {rd}, 0, r0); sh(c, {rd}, 1, r1); }}");
            push(body);
        }
        PsMuls0 | PsMuls1 | PsMadds0 | PsMadds1 => {
            let which = u32::from(matches!(insn.op, PsMuls1 | PsMadds1));
            let mut body = format!("{{ float cc = hf(c, {}, {which}); ", insn.rc_f());
            for i in 0..2 {
                let (a, b) = (format!("hf(c, {ra}, {i})"), format!("hf(c, {rb}, {i})"));
                let e = if matches!(insn.op, PsMuls0 | PsMuls1) {
                    format!("canon3f({a} * cc, {a}, cc, cc)")
                } else {
                    format!("canon3f(fmaf({a}, cc, {b}), {a}, cc, {b})")
                };
                let _ = write!(body, "float r{i} = {e}; ");
            }
            let _ = write!(body, "sh(c, {rd}, 0, r0); sh(c, {rd}, 1, r1); }}");
            push(body);
        }
        PsSum0 | PsSum1 => {
            let c_reg = insn.rc_f();
            let sum = format!("canon3f(hf(c, {ra}, 0) + hf(c, {rb}, 1), hf(c, {ra}, 0), hf(c, {rb}, 1), hf(c, {rb}, 1))");
            if insn.op == PsSum0 {
                push(format!(
                    "{{ float s = {sum}; float k = hf(c, {c_reg}, 1); sh(c, {rd}, 0, s); sh(c, {rd}, 1, k); }}"
                ));
            } else {
                push(format!(
                    "{{ float s = {sum}; float k = hf(c, {c_reg}, 0); sh(c, {rd}, 0, k); sh(c, {rd}, 1, s); }}"
                ));
            }
        }
        PsSel => {
            let c_reg = insn.rc_f();
            let mut body = String::from("{ ");
            for i in 0..2 {
                let _ = write!(
                    body,
                    "float r{i} = hf(c, {ra}, {i}) >= 0.0f ? hf(c, {c_reg}, {i}) : hf(c, {rb}, {i}); "
                );
            }
            let _ = write!(body, "sh(c, {rd}, 0, r0); sh(c, {rd}, 1, r1); }}");
            push(body);
        }
        PsRes | PsRsqrte => {
            let mut body = String::from("{ ");
            for i in 0..2 {
                let b = format!("hf(c, {rb}, {i})");
                let e = if insn.op == PsRes {
                    format!("1.0f / {b}")
                } else {
                    format!("1.0f / sqrtf({b})")
                };
                let _ = write!(body, "float r{i} = canon3f({e}, {b}, {b}, {b}); ");
            }
            let _ = write!(body, "sh(c, {rd}, 0, r0); sh(c, {rd}, 1, r1); }}");
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
            push(format!(
                "fcmp(c, {}, (double)hf(c, {ra}, {i}), (double)hf(c, {rb}, {i}));",
                insn.crfd()
            ));
        }
        PsqL | PsqLu | PsqLx | PsqLux => {
            let indexed = matches!(insn.op, PsqLx | PsqLux);
            let ea = if indexed { ea_x(ra, rb) } else { ea_d(ra, insn.psq_d()) };
            let (w, i) = if indexed {
                (insn.psqx_w(), insn.psqx_i())
            } else {
                (insn.psq_w(), insn.psq_i())
            };
            let mut body = format!("uint32_t ea = {ea}; psq_load(c, {rd}, ea, {}, {i});", u32::from(w));
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
            let mut body = format!("uint32_t ea = {ea}; psq_store(c, {rd}, ea, {}, {i});", u32::from(w));
            if matches!(insn.op, PsqStu | PsqStux) {
                let _ = write!(body, " {} = ea;", r(ra));
            }
            push(format!("{{ {body} }}"));
        }
        // ---- cache and synchronisation
        Dcbz => push(format!("zero32(c, {});", ea_x(ra, rb))),
        Dcbf | Dcbi | Dcbst | Dcbt | Dcbtst | Icbi | Sync | Isync | Eieio => push(format!("/* {} */", insn.op.name())),
        // ---- traps and the system
        Sc => push("rt_syscall(c);".to_string()),
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
                push(format!("if ({}) rt_trap(c, {});", conds.join(" || "), hex(w)));
            }
        }
        _ => push(format!("rt_unsupported(c, {}); /* {} */", hex(w), insn.op.name())),
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
            vec![format!("{}(c); return;", function_name(t))]
        } else {
            vec![format!("rt_jump(c, {});", hex(t)), "return;".to_string()]
        }
    };
    let call = |t: u32| -> String {
        if fx.functions.contains(&t) {
            format!("{}(c);", function_name(t))
        } else {
            format!("rt_call(c, {});", hex(t))
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
                out.push("return;".to_string());
            } else if always {
                out.push("{ uint32_t t = c->lr & ~3u; ".to_string() + &format!("c->lr = {}; rt_call(c, t); }}", hex(next)));
            } else {
                let cond = format!("bcond(c, {}, {}, 1)", insn.bo(), insn.bi());
                if lk {
                    out.push(format!(
                        "{{ uint32_t t = c->lr & ~3u; int go = {cond}; c->lr = {}; if (go) rt_call(c, t); }}",
                        hex(next)
                    ));
                } else {
                    out.push(format!("if ({cond}) return;"));
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
                    "{{ uint32_t t = c->ctr & ~3u; int go = {go}; c->lr = {}; if (go) rt_call(c, t); }}",
                    hex(next)
                ));
            } else if let (Some(&n), true) = (fx.tables.get(&addr), always) {
                // a switch: the table is `n` branches right after this instruction, and the target is one of them
                let mut cases = String::new();
                for k in 0..n {
                    let _ = write!(cases, "case {k}: goto {}; ", label(next + 4 * k));
                }
                out.push(format!(
                    "{{ uint32_t k = (c->ctr - {}) >> 2; switch (k) {{ {cases}default: rt_jump(c, c->ctr & ~3u); return; }} }}",
                    hex(next)
                ));
            } else {
                out.push(format!("if ({go}) {{ rt_jump(c, c->ctr & ~3u); return; }}"));
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
        let rs = (words[m] >> 21) & 31;
        // the address in the register is built by an addis and an addi
        let (mut lo, mut hi) = (None, None);
        for j in (i.saturating_sub(12)..m).rev() {
            let v = words[j];
            if v >> 26 == 14 && (v >> 21) & 31 == rs && (v >> 16) & 31 == rs && lo.is_none() {
                lo = Some(i32::from(v as i16));
            } else if v >> 26 == 15 && (v >> 21) & 31 == rs && hi.is_none() && lo.is_some() {
                hi = Some(i32::from(v as i16) << 16);
            }
        }
        let (Some(lo), Some(hi)) = (lo, hi) else { continue };
        if (hi + lo) as u32 != addr + 4 {
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
    let end = start + 4 * words.len() as u32;
    let tables = switch_tables(start, words);
    let mut labels = block_starts(start, words);
    for (&at, &n) in &tables {
        labels.extend((0..n).map(|k| at + 4 + 4 * k));
    }
    let fx = Function {
        tables: &tables,
        start,
        end,
        labels: &labels,
        functions,
    };
    let mut out = String::new();
    let _ = writeln!(out, "void {}(Ctx *c) {{", function_name(start));
    for (i, &w) in words.iter().enumerate() {
        let addr = start + 4 * i as u32;
        if labels.contains(&addr) {
            let _ = writeln!(out, "{}:;", label(addr));
        }
        match Insn::decode(w) {
            Some(insn) => {
                for s in emit_insn(insn, addr, &fx) {
                    let _ = writeln!(out, "    {s}");
                }
            }
            None => {
                let _ = writeln!(out, "    rt_unsupported(c, {}); /* not an instruction */", hex(w));
            }
        }
    }
    // running off the end of the function continues in whatever follows it
    if functions.contains(&end) {
        let _ = writeln!(out, "    {}(c);", function_name(end));
    }
    out.push_str("}\n");
    out
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
