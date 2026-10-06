//! Translating a Latte pixel or vertex program to WGSL.
//!
//! Registers are `vec4<f32>` variables that hold raw 32-bit values (integer instructions reinterpret the bits). Each ALU instruction
//! group is computed from the registers as they were before the group (its results are written together afterwards), the way the
//! hardware issues it, and the previous group's results are available as `pv` and `ps`. The exec-mask stack of the hardware's control
//! flow (`PUSH`, `ELSE`, `POP`) is a stack of booleans: a lane that is not live does not write. Where the code can be sampled with
//! derivatives, texture fetches are run for every lane and only their writes are masked, so a quad's derivatives stay defined.
//!
//! The instruction semantics are those of the R6xx/R7xx documents; what a translated shader does on the real hardware is checked by
//! looking at what it draws (there is no other reference), so every operation the translator does not know is an error and not a guess.

use crate::decode::{AluInst, AluKind, Cf, Decoded, Fetch, Group, Kcache, Operand, Source};
use crate::ops::{self, Slots};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// What the translated program needs from its surroundings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Interface {
    /// Constant buffers by the number of the kcache bank, with how many `vec4` the program may read from each.
    pub banks: BTreeMap<u32, u32>,
    /// Whether the constant file is read, and how far.
    pub const_file: u32,
    /// Texture resources used, and which sampler is used with which resource, and whether with comparison.
    pub textures: BTreeMap<u32, TexKind>,
    pub samplers: BTreeSet<u32>,
    pub compare_samplers: BTreeSet<u32>,
    /// Colour targets written (0 to 7), and whether depth is.
    pub colour_outputs: BTreeSet<u32>,
    pub depth_output: bool,
    /// Whether the program can discard the pixel.
    pub kills: bool,
}

#[derive(Debug)]
pub struct Translated {
    pub wgsl: String,
    pub interface: Interface,
}

/// What kind of texture a resource is bound to (it decides how many coordinates are used and the WGSL type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TexKind {
    D2,
    D3,
    Cube,
    /// A depth texture sampled with a comparison.
    Depth2d,
}

/// How many `vec4` a uniform block has when the program indexes it dynamically (the size the block was bound with is not known
/// when the program is translated).
pub const DYNAMIC_BANK: u32 = 4096;

/// What a pixel shader receives.
#[derive(Debug, Clone, Default)]
pub struct PixelInputs {
    /// How many interpolated inputs (they are in registers 0 and up).
    pub count: u32,
    /// Which of them are flat-shaded.
    pub flat: Vec<bool>,
}

pub struct Context<'a> {
    pub texture_kind: &'a dyn Fn(u32) -> TexKind,
}

fn swz(c: u32) -> char {
    ['x', 'y', 'z', 'w'][(c & 3) as usize]
}

struct Gen<'a> {
    out: String,
    iface: Interface,
    ctx: &'a Context<'a>,
    kcache: [Kcache; 2],
    predicated: bool,
    max_gpr: u32,
    errors: Vec<String>,
}

const HEADER: &str = "diagnostic(off, derivative_uniformity);
fn fi(x: i32) -> f32 { return bitcast<f32>(x); }
fn fu(x: u32) -> f32 { return bitcast<f32>(x); }
fn ii(x: f32) -> i32 { return bitcast<i32>(x); }
fn iu(x: f32) -> u32 { return bitcast<u32>(x); }
fn mul_dx9(a: f32, b: f32) -> f32 { if (a == 0.0 || b == 0.0) { return 0.0; } return a * b; }
fn set_f(c: bool) -> f32 { return select(0.0, 1.0, c); }
fn set_i(c: bool) -> f32 { return fu(select(0u, 0xffffffffu, c)); }
fn cmp_max(a: f32, b: f32) -> f32 { return select(b, a, a >= b); }
fn cmp_min(a: f32, b: f32) -> f32 { return select(b, a, a < b); }
fn f_to_i(x: f32) -> f32 { return fi(i32(x)); }
fn f_to_u(x: f32) -> f32 { return fu(u32(max(x, 0.0))); }
fn i_to_f(x: f32) -> f32 { return f32(ii(x)); }
fn u_to_f(x: f32) -> f32 { return f32(iu(x)); }
fn sat(x: f32) -> f32 { return clamp(x, 0.0, 1.0); }
";

impl Gen<'_> {
    fn line(&mut self, indent: usize, text: &str) {
        let _ = writeln!(self.out, "{}{}", "    ".repeat(indent), text);
    }

    fn err(&mut self, text: String) {
        if !self.errors.contains(&text) {
            self.errors.push(text);
        }
    }

    fn gpr(&mut self, n: u32) -> String {
        self.max_gpr = self.max_gpr.max(n + 1);
        format!("r{n}")
    }

    /// The expression for one operand's value (before source modifiers).
    fn operand_value(&mut self, o: &Operand, literals: &[u32; 4]) -> String {
        if o.relative {
            self.err("a relatively addressed operand".into());
        }
        let c = swz(o.channel);
        let base = match o.source {
            Source::Gpr(n) => format!("{}.{c}", self.gpr(n)),
            Source::Kcache { bank, index } => {
                let k = self.kcache[bank as usize];
                let at = k.line * 16 + index;
                let entry = self.iface.banks.entry(k.bank).or_default();
                *entry = (*entry).max(at + 1);
                format!("cb{}[{at}].{c}", k.bank)
            }
            Source::ConstFile(n) => {
                self.iface.const_file = self.iface.const_file.max(n + 1);
                format!("cfile[{n}].{c}")
            }
            Source::Zero => "0.0".into(),
            Source::One => "1.0".into(),
            Source::OneInt => "fu(1u)".into(),
            Source::MinusOneInt => "fu(0xffffffffu)".into(),
            Source::Half => "0.5".into(),
            Source::Literal => literal(literals[(o.channel & 3) as usize]),
            Source::PreviousVector => format!("pv.{c}"),
            Source::PreviousScalar => "ps".into(),
            Source::Other(n) => {
                self.err(format!("a source operand numbered {n}"));
                "0.0".into()
            }
        };
        let mut v = base;
        if o.absolute {
            v = format!("abs({v})");
        }
        if o.negate {
            v = format!("(-{v})");
        }
        v
    }
}

/// A literal constant as an expression: a plain number when it is finite, otherwise the bits reinterpreted at run time (the shader
/// language does not allow a constant that is not a number).
fn literal(bits: u32) -> String {
    let f = f32::from_bits(bits);
    if f.is_finite() {
        // an integer's bits are a denormal or a tiny number: spell the bits and let the compiler fold them
        if f == 0.0 && bits != 0 || (f != 0.0 && f.abs() < 1.0e-30) {
            return format!("fu({bits}u)");
        }
        let text = format!("{f:?}");
        if text.parse::<f32>().is_ok_and(|p| p.to_bits() == bits) {
            return if text.contains('.') || text.contains('e') {
                format!("({text})")
            } else {
                format!("({text}.0)")
            };
        }
    }
    format!("fu({bits}u)")
}

/// The slot (0 to 3 vector, 4 transcendental) of each instruction of a group.
fn slots(group: &Group) -> Vec<usize> {
    let mut taken = [false; 5];
    let mut out = Vec::new();
    for i in &group.insts {
        let table = if i.three { ops::op3(i.op) } else { ops::op2(i.op) };
        let scalar_only = table.is_some_and(|t| t.3 == Slots::Scalar);
        let mut slot = if scalar_only { 4 } else { i.dst_channel as usize };
        if taken[slot] {
            slot = 4;
        }
        taken[slot] = true;
        out.push(slot);
    }
    out
}

impl Gen<'_> {
    fn alu_group(&mut self, g: &Group, indent: usize) {
        let slot_of = slots(g);
        // the sources of every instruction, read before anything in the group is written
        let mut results: Vec<(usize, String)> = Vec::new(); // (slot, expression of the result)
        let reduction = g
            .insts
            .iter()
            .any(|i| !i.three && ops::op2(i.op).is_some_and(|t| t.3 == Slots::Reduction));
        let mut reduction_done = false;
        for (k, i) in g.insts.iter().enumerate() {
            let used = if i.three { 3 } else { 2 };
            let src: Vec<String> = (0..used).map(|n| self.operand_value(&i.src[n], &g.literals)).collect();
            let name = if i.three {
                ops::op3(i.op).map(|t| t.1)
            } else {
                ops::op2(i.op).map(|t| t.1)
            };
            let Some(name) = name else {
                self.err(format!("an instruction numbered {:#x}", i.op));
                continue;
            };
            let slot = slot_of[k];
            let expr = if reduction && ops::op2(i.op).is_some_and(|t| t.3 == Slots::Reduction) {
                if reduction_done {
                    // every slot of the group gets the same result
                    results.push((slot, "red".to_string()));
                    continue;
                }
                reduction_done = true;
                let mut pairs = Vec::new();
                for (kk, j) in g.insts.iter().enumerate() {
                    if ops::op2(j.op).is_some_and(|t| t.1 == name) {
                        let a = self.operand_value(&j.src[0], &g.literals);
                        let b = self.operand_value(&j.src[1], &g.literals);
                        pairs.push((a, b, kk));
                    }
                }
                let mul = if name == "DOT4_IEEE" {
                    |a: &str, b: &str| format!("({a} * {b})")
                } else {
                    |a: &str, b: &str| format!("mul_dx9({a}, {b})")
                };
                match name {
                    "DOT4" | "DOT4_IEEE" => {
                        let sum = pairs.iter().map(|(a, b, _)| mul(a, b)).collect::<Vec<_>>().join(" + ");
                        self.line(indent, &format!("let red = {sum};"));
                        "red".to_string()
                    }
                    other => {
                        self.err(format!("the reduction instruction {other}"));
                        "0.0".to_string()
                    }
                }
            } else {
                match self.alu_expr(name, i, &src) {
                    Some(e) => e,
                    None => {
                        self.err(format!("the instruction {name}"));
                        "0.0".to_string()
                    }
                }
            };
            let mut e = expr;
            e = match i.omod {
                1 => format!("({e} * 2.0)"),
                2 => format!("({e} * 4.0)"),
                3 => format!("({e} * 0.5)"),
                _ => e,
            };
            if i.clamp {
                e = format!("sat({e})");
            }
            results.push((slot, e));
        }
        // evaluate, then write
        let mut names = Vec::new();
        for (n, (slot, e)) in results.iter().enumerate() {
            self.line(indent, &format!("let t{n} = {e};"));
            names.push((*slot, n));
        }
        for (k, i) in g.insts.iter().enumerate() {
            let Some(&(_, n)) = names.get(k) else { continue };
            if i.write && !self.is_predicate_only(i) {
                if i.dst_relative {
                    self.err("a relatively addressed destination".into());
                }
                let r = self.gpr(i.dst_gpr);
                self.line(indent, &format!("{r}.{} = t{n};", swz(i.dst_channel)));
            }
        }
        // the group's results become the previous vector and scalar
        for (k, _) in g.insts.iter().enumerate() {
            let Some(&(slot, n)) = names.get(k) else { continue };
            if slot < 4 {
                self.line(indent, &format!("pv.{} = t{n};", swz(slot as u32)));
            } else {
                self.line(indent, &format!("ps = t{n};"));
            }
        }
        // predicate-setting instructions update the exec mask
        for (k, i) in g.insts.iter().enumerate() {
            if i.update_exec_mask || i.update_pred {
                let Some(&(_, n)) = names.get(k) else { continue };
                if i.update_exec_mask {
                    let cond = self.predicate_condition(i, &g.literals);
                    let _ = n;
                    self.line(indent, &format!("live = live && ({cond});"));
                }
            }
        }
        // killing instructions
        for i in &g.insts {
            let name = if i.three { None } else { ops::op2(i.op).map(|t| t.1) };
            if let Some(n) = name.filter(|n| n.starts_with("KILL")) {
                self.iface.kills = true;
                let cond = self.compare_condition(n.trim_start_matches("KILL"), i, &g.literals);
                self.line(indent, &format!("if (live && ({cond})) {{ discard; }}"));
            }
        }
    }

    fn is_predicate_only(&self, i: &AluInst) -> bool {
        // a predicate-setting instruction writes 0 or 1 into its destination only when its write mask says so
        !i.three && ops::op2(i.op).is_some_and(|t| t.1.starts_with("PRED_SET") && !i.write)
    }

    /// The condition of a `PRED_SETxx` instruction.
    fn predicate_condition(&mut self, i: &AluInst, lit: &[u32; 4]) -> String {
        let name = ops::op2(i.op).map_or("", |t| t.1);
        let tail = name.trim_start_matches("PRED_SET").trim_end_matches("_PUSH");
        self.compare_condition(tail, i, lit)
    }

    /// `E`, `GT`, `GE`, `NE`, with an optional `_INT`/`_UINT`: the comparison of the two sources.
    fn compare_condition(&mut self, tail: &str, i: &AluInst, lit: &[u32; 4]) -> String {
        let a = self.operand_value(&i.src[0], lit);
        let b = self.operand_value(&i.src[1], lit);
        let (op, kind) = match tail {
            "E" => ("==", 'f'),
            "GT" => (">", 'f'),
            "GE" => (">=", 'f'),
            "NE" => ("!=", 'f'),
            "E_INT" => ("==", 'i'),
            "GT_INT" => (">", 'i'),
            "GE_INT" => (">=", 'i'),
            "NE_INT" => ("!=", 'i'),
            "GT_UINT" => (">", 'u'),
            "GE_UINT" => (">=", 'u'),
            other => {
                self.err(format!("the comparison {other}"));
                return "false".into();
            }
        };
        match kind {
            'f' => format!("{a} {op} {b}"),
            'i' => format!("ii({a}) {op} ii({b})"),
            _ => format!("iu({a}) {op} iu({b})"),
        }
    }

    /// The result of an ALU instruction; `None` for the ones not written yet.
    fn alu_expr(&mut self, name: &str, i: &AluInst, s: &[String]) -> Option<String> {
        let a = s.first().cloned().unwrap_or_default();
        let b = s.get(1).cloned().unwrap_or_default();
        let c = s.get(2).cloned().unwrap_or_default();
        Some(match name {
            "ADD" => format!("({a} + {b})"),
            "MUL" => format!("mul_dx9({a}, {b})"),
            "MUL_IEEE" => format!("({a} * {b})"),
            "MAX" => format!("cmp_max({a}, {b})"),
            "MIN" => format!("cmp_min({a}, {b})"),
            "MAX_DX10" => format!("max({a}, {b})"),
            "MIN_DX10" => format!("min({a}, {b})"),
            "SETE" => format!("set_f({a} == {b})"),
            "SETGT" => format!("set_f({a} > {b})"),
            "SETGE" => format!("set_f({a} >= {b})"),
            "SETNE" => format!("set_f({a} != {b})"),
            "SETE_DX10" => format!("set_i({a} == {b})"),
            "SETGT_DX10" => format!("set_i({a} > {b})"),
            "SETGE_DX10" => format!("set_i({a} >= {b})"),
            "SETNE_DX10" => format!("set_i({a} != {b})"),
            "FRACT" => format!("fract({a})"),
            "TRUNC" => format!("trunc({a})"),
            "CEIL" => format!("ceil({a})"),
            "RNDNE" => format!("round({a})"),
            "FLOOR" => format!("floor({a})"),
            "MOV" => a,
            "ALU_NOP" => "0.0".into(),
            "AND_INT" => format!("fu(iu({a}) & iu({b}))"),
            "OR_INT" => format!("fu(iu({a}) | iu({b}))"),
            "XOR_INT" => format!("fu(iu({a}) ^ iu({b}))"),
            "NOT_INT" => format!("fu(~iu({a}))"),
            "ADD_INT" => format!("fi(ii({a}) + ii({b}))"),
            "SUB_INT" => format!("fi(ii({a}) - ii({b}))"),
            "MAX_INT" => format!("fi(max(ii({a}), ii({b})))"),
            "MIN_INT" => format!("fi(min(ii({a}), ii({b})))"),
            "MAX_UINT" => format!("fu(max(iu({a}), iu({b})))"),
            "MIN_UINT" => format!("fu(min(iu({a}), iu({b})))"),
            "SETE_INT" => format!("set_i(ii({a}) == ii({b}))"),
            "SETGT_INT" => format!("set_i(ii({a}) > ii({b}))"),
            "SETGE_INT" => format!("set_i(ii({a}) >= ii({b}))"),
            "SETNE_INT" => format!("set_i(ii({a}) != ii({b}))"),
            "SETGT_UINT" => format!("set_i(iu({a}) > iu({b}))"),
            "SETGE_UINT" => format!("set_i(iu({a}) >= iu({b}))"),
            "ASHR_INT" => format!("fi(ii({a}) >> (iu({b}) & 31u))"),
            "LSHR_INT" => format!("fu(iu({a}) >> (iu({b}) & 31u))"),
            "LSHL_INT" => format!("fu(iu({a}) << (iu({b}) & 31u))"),
            "FLT_TO_INT" => format!("f_to_i({a})"),
            "FLT_TO_UINT" => format!("f_to_u({a})"),
            "INT_TO_FLT" => format!("i_to_f({a})"),
            "UINT_TO_FLT" => format!("u_to_f({a})"),
            "EXP_IEEE" => format!("exp2({a})"),
            "LOG_IEEE" => format!("log2({a})"),
            "LOG_CLAMPED" => format!("max(log2({a}), -3.4028235e38)"),
            "RECIP_IEEE" | "RECIP_FF" => format!("(1.0 / {a})"),
            "RECIP_CLAMPED" => format!("clamp(1.0 / {a}, -3.4028235e38, 3.4028235e38)"),
            "RECIPSQRT_IEEE" | "RECIPSQRT_FF" => format!("inverseSqrt({a})"),
            "RECIPSQRT_CLAMPED" => format!("min(inverseSqrt({a}), 3.4028235e38)"),
            "SQRT_IEEE" => format!("sqrt({a})"),
            // the hardware's sine and cosine take their argument in turns
            "SIN" => format!("sin({a} * 6.2831855)"),
            "COS" => format!("cos({a} * 6.2831855)"),
            "MULADD" => format!("(mul_dx9({a}, {b}) + {c})"),
            "MULADD_M2" => format!("((mul_dx9({a}, {b}) + {c}) * 2.0)"),
            "MULADD_M4" => format!("((mul_dx9({a}, {b}) + {c}) * 4.0)"),
            "MULADD_D2" => format!("((mul_dx9({a}, {b}) + {c}) * 0.5)"),
            "MULADD_IEEE" => format!("(({a} * {b}) + {c})"),
            "CNDE" => format!("select({c}, {b}, {a} == 0.0)"),
            "CNDGT" => format!("select({c}, {b}, {a} > 0.0)"),
            "CNDGE" => format!("select({c}, {b}, {a} >= 0.0)"),
            "CNDE_INT" => format!("select({c}, {b}, iu({a}) == 0u)"),
            "CNDGT_INT" => format!("select({c}, {b}, ii({a}) > 0)"),
            "CNDGE_INT" => format!("select({c}, {b}, ii({a}) >= 0)"),
            n if n.starts_with("PRED_SET") || n.starts_with("KILL") => {
                // the result written when the destination is written: 0.0 when the condition holds, else 1.0 for the float
                // forms; the conditions themselves are applied by the caller
                let _ = i;
                "0.0".into()
            }
            _ => return None,
        })
    }
}

/// Translates a pixel shader.
pub fn translate_pixel(decoded: &Decoded, inputs: &PixelInputs, ctx: &Context) -> Result<Translated, String> {
    let mut g = Gen {
        out: String::new(),
        iface: Interface::default(),
        ctx,
        kcache: [Kcache { bank: 0, mode: 0, line: 0 }; 2],
        predicated: false,
        max_gpr: inputs.count,
        errors: Vec::new(),
    };
    g.predicated = decoded.cf.iter().any(|c| match c {
        Cf::Flow { inst, .. } => matches!(inst, 10..=17),
        Cf::Alu { kind, .. } => *kind != AluKind::Plain,
        _ => false,
    });
    let mut pixel_outputs: Vec<(u32, [u32; 4], u32)> = Vec::new();
    for cf in &decoded.cf {
        match cf {
            Cf::Alu { kind, kcache, groups } => {
                g.kcache = *kcache;
                if matches!(kind, AluKind::PushBefore) {
                    g.line(1, "stack[sp] = live; sp = sp + 1;");
                }
                for grp in groups {
                    if g.predicated {
                        g.line(1, "if (live) {");
                        g.alu_group(grp, 2);
                        g.line(1, "}");
                    } else {
                        g.line(1, "{");
                        g.alu_group(grp, 2);
                        g.line(1, "}");
                    }
                }
                match kind {
                    AluKind::PopAfter => g.line(1, "sp = sp - 1; live = stack[sp];"),
                    AluKind::Pop2After => g.line(1, "sp = sp - 2; live = stack[sp];"),
                    AluKind::Plain | AluKind::PushBefore => {}
                    other => g.err(format!("the ALU clause kind {other:?}")),
                }
            }
            Cf::Fetch { vertex, fetches } => {
                if *vertex {
                    g.err("a vertex fetch clause in a pixel shader".into());
                }
                for f in fetches {
                    g.fetch(f);
                }
            }
            Cf::Export {
                kind,
                array_base,
                gpr,
                swizzle,
                burst,
                ..
            } => {
                if *kind != 0 {
                    g.err(format!("an export of kind {kind} in a pixel shader"));
                    continue;
                }
                for b in 0..*burst {
                    pixel_outputs.push((*array_base + b, *swizzle, *gpr + b));
                }
                for b in 0..*burst {
                    let (target, sel, reg) = (*array_base + b, *swizzle, *gpr + b);
                    let r = g.gpr(reg);
                    let comp = |s: u32, k: usize| match s {
                        0..=3 => format!("{r}.{}", swz(s)),
                        4 => "0.0".to_string(),
                        5 => "1.0".to_string(),
                        _ => format!(
                            "out_{}.{}",
                            if target == 61 { "z".to_string() } else { target.to_string() },
                            swz(k as u32)
                        ),
                    };
                    if target == 61 {
                        g.iface.depth_output = true;
                        let v = comp(sel[0], 0);
                        let w = if g.predicated {
                            "if (live) { out_z = VALUE; }"
                        } else {
                            "out_z = VALUE;"
                        };
                        g.line(1, &w.replace("VALUE", &v));
                    } else if target < 8 {
                        g.iface.colour_outputs.insert(target);
                        let v = format!(
                            "vec4<f32>({}, {}, {}, {})",
                            comp(sel[0], 0),
                            comp(sel[1], 1),
                            comp(sel[2], 2),
                            comp(sel[3], 3)
                        );
                        let w = if g.predicated {
                            format!("if (live) {{ out_{target} = {v}; }}")
                        } else {
                            format!("out_{target} = {v};")
                        };
                        g.line(1, &w);
                    } else {
                        g.err(format!("an export to target {target}"));
                    }
                }
            }
            Cf::Flow { inst, pop_count, .. } => match inst {
                // a jump only skips code that no lane would run; running it under the mask does the same
                10 | 11 => {}
                // ELSE: the lanes that were live at the matching push and are not now become live
                13 => g.line(1, "live = stack[sp - 1] && !live;"),
                14 => {
                    for _ in 0..*pop_count {
                        g.line(1, "sp = sp - 1; live = stack[sp];");
                    }
                }
                other => g.err(format!("the control flow instruction {other:#x}")),
            },
            Cf::Nop => {}
            Cf::CallFetchShader => g.err("a call to a fetch shader in a pixel shader".into()),
            Cf::Return => {}
        }
    }
    if !g.errors.is_empty() {
        return Err(g.errors.join("; "));
    }
    // assemble the module
    let mut m = String::from(HEADER);
    for (bank, len) in &g.iface.banks {
        let _ = writeln!(
            m,
            "@group(0) @binding({bank}) var<uniform> cb{bank}: array<vec4<f32>, {}>;",
            (*len).max(1)
        );
    }
    if g.iface.const_file > 0 {
        let _ = writeln!(
            m,
            "@group(0) @binding(15) var<uniform> cfile: array<vec4<f32>, {}>;",
            g.iface.const_file
        );
    }
    for (r, kind) in &g.iface.textures {
        let ty = match kind {
            TexKind::D2 => "texture_2d<f32>",
            TexKind::D3 => "texture_3d<f32>",
            TexKind::Cube => "texture_cube<f32>",
            TexKind::Depth2d => "texture_depth_2d",
        };
        let _ = writeln!(m, "@group(1) @binding({r}) var tex{r}: {ty};");
    }
    for s in &g.iface.samplers {
        let _ = writeln!(m, "@group(1) @binding({}) var smp{s}: sampler;", 16 + s);
    }
    for s in &g.iface.compare_samplers {
        let _ = writeln!(m, "@group(1) @binding({}) var cmp{s}: sampler_comparison;", 32 + s);
    }
    m.push_str("struct In {\n");
    for i in 0..inputs.count {
        let flat = if inputs.flat.get(i as usize).copied().unwrap_or(false) {
            " @interpolate(flat)"
        } else {
            ""
        };
        let _ = writeln!(m, "    @location({i}){flat} i{i}: vec4<f32>,");
    }
    m.push_str("    @builtin(position) frag: vec4<f32>,\n}\n");
    m.push_str("struct Out {\n");
    for t in &g.iface.colour_outputs {
        let _ = writeln!(m, "    @location({t}) c{t}: vec4<f32>,");
    }
    if g.iface.depth_output {
        m.push_str("    @builtin(frag_depth) z: f32,\n");
    }
    m.push_str("}\n");
    m.push_str("@fragment fn main(in: In) -> Out {\n");
    for n in 0..g.max_gpr {
        let init = if n < inputs.count {
            format!("in.i{n}")
        } else {
            "vec4<f32>(0.0)".to_string()
        };
        let _ = writeln!(m, "    var r{n}: vec4<f32> = {init};");
    }
    m.push_str("    var pv = vec4<f32>(0.0);\n    var ps = 0.0;\n    var live = true;\n    var stack: array<bool, 16>;\n    var sp = 0;\n");
    for t in &g.iface.colour_outputs {
        let _ = writeln!(m, "    var out_{t} = vec4<f32>(0.0);");
    }
    if g.iface.depth_output {
        m.push_str("    var out_z = 0.0;\n");
    }
    m.push_str(&g.out);
    m.push_str("    var o: Out;\n");
    for t in &g.iface.colour_outputs {
        let _ = writeln!(m, "    o.c{t} = out_{t};");
    }
    if g.iface.depth_output {
        m.push_str("    o.z = out_z;\n");
    }
    m.push_str("    return o;\n}\n");
    Ok(Translated {
        wgsl: m,
        interface: g.iface,
    })
}

impl Gen<'_> {
    /// A vertex-style fetch in a texture clause on a resource from 128 up: the element of a uniform block chosen by an index in a
    /// register (what a dynamically indexed uniform array becomes).
    fn uniform_fetch(&mut self, f: &Fetch) {
        if f.resource < 128 {
            self.err(format!("a buffer fetch on resource {}", f.resource));
            return;
        }
        let bank = f.resource - 128;
        let entry = self.iface.banks.entry(bank).or_default();
        *entry = DYNAMIC_BANK;
        let src = self.gpr(f.src_gpr);
        let index = match f.src_sel[0] {
            0..=3 => format!("{src}.{}", swz(f.src_sel[0])),
            _ => "0.0".to_string(),
        };
        let dst = self.gpr(f.dst_gpr);
        self.line(1, "{");
        self.line(2, &format!("let e = cb{bank}[min(iu({index}), {}u)];", DYNAMIC_BANK - 1));
        for (k, &d) in f.dst_sel.iter().enumerate() {
            let value = match d {
                0..=3 => format!("e.{}", swz(d)),
                4 => "0.0".to_string(),
                5 => "1.0".to_string(),
                _ => continue,
            };
            let w = if self.predicated {
                format!("if (live) {{ {dst}.{} = {value}; }}", swz(k as u32))
            } else {
                format!("{dst}.{} = {value};", swz(k as u32))
            };
            self.line(2, &w);
        }
        self.line(1, "}");
    }

    /// A texture fetch: run for every lane, written only by the lanes that are live.
    fn fetch(&mut self, f: &Fetch) {
        if f.inst == 0 {
            self.uniform_fetch(f);
            return;
        }
        let name = match f.inst {
            0x10 => "SAMPLE",
            0x11 => "SAMPLE_L",
            0x12 => "SAMPLE_LB",
            0x13 => "SAMPLE_LZ",
            0x18 => "SAMPLE_C",
            0x19 => "SAMPLE_C_L",
            0x1b => "SAMPLE_C_LZ",
            other => {
                self.err(format!("the texture instruction {other:#x}"));
                return;
            }
        };
        let compare = name.starts_with("SAMPLE_C");
        // a lookup with a comparison is on a depth texture, whatever the binding says
        let kind = if compare {
            TexKind::Depth2d
        } else {
            (self.ctx.texture_kind)(f.resource)
        };
        self.iface.textures.insert(f.resource, kind);
        let src = self.gpr(f.src_gpr);
        let sel = |s: u32| match s {
            0..=3 => format!("{src}.{}", swz(s)),
            4 => "0.0".to_string(),
            _ => "1.0".to_string(),
        };
        let coords: Vec<String> = f.src_sel.iter().map(|&s| sel(s)).collect();
        let uv = match kind {
            TexKind::D2 | TexKind::Depth2d => format!("vec2<f32>({}, {})", coords[0], coords[1]),
            TexKind::D3 | TexKind::Cube => format!("vec3<f32>({}, {}, {})", coords[0], coords[1], coords[2]),
        };
        let call = if compare {
            self.iface.compare_samplers.insert(f.sampler);
            // the reference value of a shadow lookup is in the fourth coordinate
            format!(
                "vec4<f32>(textureSampleCompare(tex{}, cmp{}, {uv}, {}))",
                f.resource, f.sampler, coords[3]
            )
        } else {
            self.iface.samplers.insert(f.sampler);
            match name {
                "SAMPLE" => format!("textureSample(tex{}, smp{}, {uv})", f.resource, f.sampler),
                "SAMPLE_L" => format!("textureSampleLevel(tex{}, smp{}, {uv}, {})", f.resource, f.sampler, coords[3]),
                "SAMPLE_LZ" => format!("textureSampleLevel(tex{}, smp{}, {uv}, 0.0)", f.resource, f.sampler),
                "SAMPLE_LB" => format!("textureSampleBias(tex{}, smp{}, {uv}, {})", f.resource, f.sampler, coords[3]),
                _ => {
                    self.err(format!("the texture instruction {name}"));
                    return;
                }
            }
        };
        self.line(1, "{");
        self.line(2, &format!("let s = {call};"));
        let dst = self.gpr(f.dst_gpr);
        for (k, &d) in f.dst_sel.iter().enumerate() {
            let value = match d {
                0..=3 => format!("s.{}", swz(d)),
                4 => "0.0".to_string(),
                5 => "1.0".to_string(),
                _ => continue,
            };
            let w = if self.predicated {
                format!("if (live) {{ {dst}.{} = {value}; }}", swz(k as u32))
            } else {
                format!("{dst}.{} = {value};", swz(k as u32))
            };
            self.line(2, &w);
        }
        self.line(1, "}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::decode;

    fn words(list: &[u32]) -> Vec<u8> {
        list.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// `MULADD r0.x = r1.x * 0.5 + 1.0`, exported as colour 0 (x, then the constants 0 and 1).
    fn sample() -> Vec<u8> {
        words(&[
            // ALU clause at slot 2 (after the two control flow instructions), one slot
            2,
            8 << 26,
            // export colour 0 from r0 with swizzle x, 0, 1, mask (0, 4, 5, 7): end of program
            0,
            (0x28 << 23) | (1 << 21) | (7 << 9) | (5 << 6) | (4 << 3),
            // MULADD: src0 = r1.x (select 1, channel x), src1 = 0.5 (select 252), src2 = 1.0 (249); last in group; dst r0.x
            1 | (252 << 13) | (1 << 31),
            (16 << 13) | 249,
        ])
    }

    #[test]
    fn a_small_program_becomes_the_expected_arithmetic() {
        let d = decode(&sample()).unwrap();
        let ctx = Context {
            texture_kind: &|_| TexKind::D2,
        };
        let t = translate_pixel(&d, &PixelInputs { count: 2, flat: vec![] }, &ctx).unwrap();
        assert!(t.wgsl.contains("r0.x = t0;"), "{}", t.wgsl);
        assert!(t.wgsl.contains("mul_dx9(r1.x, 0.5) + 1.0"), "{}", t.wgsl);
        assert!(t.interface.colour_outputs.contains(&0));
        assert!(!t.interface.kills);
    }

    #[test]
    fn what_is_not_known_is_an_error_not_a_guess() {
        let mut p = sample();
        // an instruction number the table has no name for
        p[20..24].copy_from_slice(&(0x7ffu32 << 7).to_le_bytes());
        let d = decode(&p).unwrap();
        let ctx = Context {
            texture_kind: &|_| TexKind::D2,
        };
        assert!(translate_pixel(&d, &PixelInputs::default(), &ctx).is_err());
    }
}
