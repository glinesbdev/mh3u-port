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
    /// For a vertex shader: whether it writes the position, and which parameters (outputs for the pixel shader).
    pub position: bool,
    pub params: BTreeSet<u32>,
    /// For a geometry pipeline's shaders: the furthest dword written to a ring (the geometry shader's tells how many vertices it emits).
    pub ring_dwords: u32,
    /// Whether the shader reads the geometry shader's output ring.
    pub reads_ring: bool,
}

#[derive(Debug)]
pub struct Translated {
    pub wgsl: String,
    pub interface: Interface,
}

/// What kind of texture a resource is bound to (it decides how many coordinates are used and the WGSL type).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TexKind {
    D2,
    D3,
    Cube,
    /// A depth texture sampled with a comparison.
    Depth2d,
}

/// The resource a geometry pipeline's rings are read through.
pub const RING_RESOURCE: u32 = 159;

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
    /// The location of the vertex shader output each input comes from (input `i` by default).
    pub locations: Vec<u32>,
}

/// What a translation needs to know besides the program.
pub struct Context<'a> {
    pub texture_kind: &'a dyn Fn(u32) -> TexKind,
    /// The texture's component selectors as `GX2Texture` has them (`compSel`: 3 bits each for red at bit 24, green 16, blue 8,
    /// alpha 0; 0 to 3 a component, 4 zero, 5 one). `IDENTITY_SELECT` leaves the components as they are.
    pub texture_select: &'a dyn Fn(u32) -> u32,
    /// The value of a loop constant (`SQ_LOOP_CONST`, what a counted loop reads: count in bits 0 to 11, start 12 to 23, step 24 to 31).
    pub loop_const: &'a dyn Fn(u32) -> Option<u32>,
    /// The alpha test (a pixel shader only): the comparison (`GX2CompareFunction`, 0 never to 7 always) and the reference value. A
    /// pixel whose first colour output fails it is discarded.
    pub alpha_test: Option<(u32, f32)>,
    /// A debugging override of what a pixel shader puts out (`MH3U_GFX_DEBUG_PS`); `DebugPs::Off` translates the program as it is.
    pub debug: DebugPs,
}

/// What a pixel shader puts out instead of its first colour, to tell apart where a wrong picture comes from: the geometry (`White`),
/// the blending of a transparent output (`Alpha1`), the first texture's coordinates (`Uv`) or texels (`Tex`), and the interpolated
/// inputs from the vertex shader (`Input`). All but `Off` and `Alpha1` give an opaque colour and drop the alpha test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DebugPs {
    #[default]
    Off,
    White,
    /// The program's colour with alpha 1 (no alpha test).
    Alpha1,
    /// The colour of the program's first ordinary texture lookup, as read (magenta when it has none).
    Tex,
    /// The fractional part of that lookup's first two coordinates as red and green (magenta when it has none).
    Uv,
    /// The pixel shader's input register `n` (0 is the first interpolated value, R0), as it arrives; with `Signed` its components
    /// are mapped from -1..1 to 0..1 (for normals).
    Input {
        n: u32,
        signed: bool,
    },
}

impl DebugPs {
    /// The value of `MH3U_GFX_DEBUG_PS`: `white`, `alpha1`, `tex` (or `tex0`), `uv`, `inputN` and `normalN` (input N, signed), or
    /// `normals` (input 1, signed). Anything else is `Off`.
    pub fn parse(text: &str) -> DebugPs {
        let t = text.trim().to_ascii_lowercase();
        match t.as_str() {
            "white" => DebugPs::White,
            "alpha1" | "opaque" => DebugPs::Alpha1,
            "tex" | "tex0" => DebugPs::Tex,
            "uv" => DebugPs::Uv,
            "normals" => DebugPs::Input { n: 1, signed: true },
            _ => {
                if let Some(n) = t.strip_prefix("input").and_then(|n| n.parse().ok()) {
                    DebugPs::Input { n, signed: false }
                } else if let Some(n) = t.strip_prefix("normal").and_then(|n| n.parse().ok()) {
                    DebugPs::Input { n, signed: true }
                } else {
                    DebugPs::Off
                }
            }
        }
    }
}

/// The component selectors that change nothing.
pub const IDENTITY_SELECT: u32 = 0x0001_0203;

fn identity_select(_: u32) -> u32 {
    IDENTITY_SELECT
}

fn no_loop_const(_: u32) -> Option<u32> {
    None
}

impl<'a> Context<'a> {
    /// A context with the components as they are, no loop constants and no alpha test.
    pub fn new(texture_kind: &'a dyn Fn(u32) -> TexKind) -> Self {
        Context {
            texture_kind,
            texture_select: &identity_select,
            loop_const: &no_loop_const,
            alpha_test: None,
            debug: DebugPs::Off,
        }
    }
}

/// The most registers a program has (the hardware's limit).
const GPR_FILE: u32 = 128;
/// The most times a loop with no count runs before it is cut short (a program that never leaves its loop would hang the GPU).
const LOOP_GUARD: u32 = 4096;

/// A loop being translated.
struct LoopFrame {
    n: u32,
    /// The step added to the loop index each time round (`LOOP_START` only).
    step: Option<i32>,
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
    stage: Stage,
    ring: RingConfig,
    /// For a copy shader: the size of the geometry shader's output item in `vec4`s (its ring fetches use it).
    ring_out_vec4: u32,
    /// Whether the registers are an array (the program indexes them).
    gpr_array: bool,
    /// Whether the program uses the address register or the loop index.
    uses_ar: bool,
    uses_al: bool,
    /// The index mode of the instruction whose operands are being read.
    index_mode: u32,
    loops: Vec<LoopFrame>,
    next_loop: u32,
    next_clause: u32,
    /// Whether the first ordinary texture lookup has been kept for `DebugPs::Tex`/`Uv`.
    debug_tex: bool,
}

const HEADER: &str = "diagnostic(off, derivative_uniformity);
fn fi(x: i32) -> f32 { return bitcast<f32>(x); }
fn fu(x: u32) -> f32 { return bitcast<f32>(x); }
fn ii(x: f32) -> i32 { return bitcast<i32>(x); }
fn iu(x: f32) -> u32 { return bitcast<u32>(x); }
fn cube_face(v: vec3<f32>) -> vec4<f32> {
    let a = abs(v);
    if (a.x >= a.y && a.x >= a.z) {
        if (v.x >= 0.0) { return vec4<f32>(-v.y, -v.z, 2.0 * a.x, 0.0); }
        return vec4<f32>(-v.y, v.z, 2.0 * a.x, 1.0);
    }
    if (a.y >= a.z) {
        if (v.y >= 0.0) { return vec4<f32>(v.z, v.x, 2.0 * a.y, 2.0); }
        return vec4<f32>(-v.z, v.x, 2.0 * a.y, 3.0);
    }
    if (v.z >= 0.0) { return vec4<f32>(-v.y, v.x, 2.0 * a.z, 4.0); }
    return vec4<f32>(-v.y, -v.x, 2.0 * a.z, 5.0);
}
fn swap8in16(v: vec4<f32>) -> vec4<f32> {
    let u = bitcast<vec4<u32>>(v);
    return bitcast<vec4<f32>>(((u & vec4<u32>(0x00ff00ffu)) << vec4<u32>(8u)) | ((u >> vec4<u32>(8u)) & vec4<u32>(0x00ff00ffu)));
}
fn swap8in32(v: vec4<f32>) -> vec4<f32> {
    let u = swap8in16(v);
    let w = bitcast<vec4<u32>>(u);
    return bitcast<vec4<f32>>((w << vec4<u32>(16u)) | (w >> vec4<u32>(16u)));
}
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
        if self.gpr_array { format!("r[{n}]") } else { format!("r{n}") }
    }

    /// A register picked by its number plus an index (the program's registers are an array then).
    fn gpr_indexed(&mut self, n: u32, index: &str) -> String {
        self.max_gpr = self.max_gpr.max(n + 1);
        format!("r[clamp({n} + {index}, 0, gpr_last)]")
    }

    /// What a relatively addressed operand or destination adds to its index, by the instruction's index mode.
    fn index_value(&mut self, mode: u32) -> String {
        match mode {
            0..=3 => {
                self.uses_ar = true;
                format!("ar.{}", swz(mode))
            }
            4 => {
                self.uses_al = true;
                "al".into()
            }
            other => {
                self.err(format!("the index mode {other}"));
                "0".into()
            }
        }
    }

    /// The expression for one operand's value (before source modifiers).
    fn operand_value(&mut self, o: &Operand, literals: &[u32; 4]) -> String {
        let index = if o.relative {
            if matches!(o.source, Source::Gpr(_) | Source::Kcache { .. } | Source::ConstFile(_)) {
                Some(self.index_value(self.index_mode))
            } else {
                self.err("a relatively addressed operand that is not a register or constant".into());
                None
            }
        } else {
            None
        };
        let c = swz(o.channel);
        let base = match o.source {
            Source::Gpr(n) => match &index {
                Some(i) => format!("{}.{c}", self.gpr_indexed(n, i)),
                None => format!("{}.{c}", self.gpr(n)),
            },
            Source::Kcache { bank, index: at } => {
                let k = self.kcache[bank as usize];
                let at = k.line * 16 + at;
                let entry = self.iface.banks.entry(k.bank).or_default();
                match &index {
                    // the program indexes the block: it may read any of it
                    Some(i) => {
                        *entry = DYNAMIC_BANK;
                        format!("cb{}[clamp({at} + {i}, 0, {})].{c}", k.bank, DYNAMIC_BANK - 1)
                    }
                    None => {
                        *entry = (*entry).max(at + 1);
                        format!("cb{}[{at}].{c}", k.bank)
                    }
                }
            }
            Source::ConstFile(n) => match &index {
                Some(i) => {
                    self.iface.const_file = 256;
                    format!("cfile[clamp({n} + {i}, 0, 255)].{c}")
                }
                None => {
                    self.iface.const_file = self.iface.const_file.max(n + 1);
                    format!("cfile[{n}].{c}")
                }
            },
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
        let mut cube_done = false;
        for (k, i) in g.insts.iter().enumerate() {
            self.index_mode = i.index_mode;
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
            let expr = if name == "CUBE" {
                // the four slots hold (z, z, y, x) and (y, x, z, z); the result of slot n is component n of
                // (t, s, twice the major axis, the face)
                if !cube_done {
                    cube_done = true;
                    let mut a = |n: usize| match slot_of.iter().position(|&s| s == n) {
                        Some(j) => {
                            self.index_mode = g.insts[j].index_mode;
                            self.operand_value(&g.insts[j].src[0], &g.literals)
                        }
                        None => "0.0".to_string(),
                    };
                    let (x, y, z) = (a(3), a(2), a(0));
                    let v = format!("vec3<f32>({x}, {y}, {z})");
                    self.line(indent, &format!("let cube = cube_face({v});"));
                }
                format!("cube.{}", swz(slot as u32))
            } else if reduction && ops::op2(i.op).is_some_and(|t| t.3 == Slots::Reduction) {
                if reduction_done {
                    // every slot of the group gets the same result
                    results.push((slot, "red".to_string()));
                    continue;
                }
                reduction_done = true;
                let mut pairs = Vec::new();
                for (kk, j) in g.insts.iter().enumerate() {
                    if ops::op2(j.op).is_some_and(|t| t.1 == name) {
                        self.index_mode = j.index_mode;
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
            self.index_mode = i.index_mode;
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
            let Some(&(slot, n)) = names.get(k) else { continue };
            let name = if i.three { ops::op3(i.op) } else { ops::op2(i.op) }.map_or("", |t| t.1);
            if matches!(name, "MOVA" | "MOVA_FLOOR" | "MOVA_INT") {
                // the address register's component is the slot's; the register written is undefined, so it is left alone
                self.uses_ar = true;
                let c = if slot < 4 { slot as u32 } else { i.dst_channel };
                self.line(indent, &format!("ar.{} = ii(t{n});", swz(c)));
                continue;
            }
            if i.write && !self.is_predicate_only(i) {
                let r = if i.dst_relative {
                    let index = self.index_value(i.index_mode);
                    self.gpr_indexed(i.dst_gpr, &index)
                } else {
                    self.gpr(i.dst_gpr)
                };
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
                    self.index_mode = i.index_mode;
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
                self.index_mode = i.index_mode;
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
            // into the address register (the caller writes it there), as an integer from -256 to 255
            "MOVA" => format!("fi(clamp(i32(floor({a} + 0.5)), -256, 255))"),
            "MOVA_FLOOR" => format!("fi(clamp(i32(floor({a})), -256, 255))"),
            "MOVA_INT" => format!("fi(clamp(ii({a}), -256, 255))"),
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Vertex,
    Pixel,
    /// The vertex shader of a pipeline with a geometry shader: it runs as a compute shader and writes its outputs to the ring.
    Es,
    /// The geometry shader, as a compute shader that reads the ring, writes vertices to another ring and a list of triangles.
    Gs,
}

/// How the rings between the stages of a geometry pipeline are laid out, in `vec4`s.
#[derive(Debug, Clone, Copy, Default)]
pub struct RingConfig {
    /// The vertex shader's output item (what the geometry shader reads for a vertex).
    pub es_vec4: u32,
    /// The geometry shader's output item (what the copy shader reads for a vertex).
    pub gs_vec4: u32,
    /// How many vertices one run of the geometry shader may emit.
    pub max_vertices: u32,
}

/// A vertex attribute as the vertex shader sees it: which register the fetch shader fills, which stream of decoded attribute data it
/// reads, and whether it advances per instance (every `divisor` instances) instead of per vertex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VertexAttribute {
    pub gpr: u32,
    pub stream: u32,
    pub per_instance: bool,
    pub divisor: u32,
}

#[derive(Debug, Clone, Default)]
pub struct VertexInputs {
    pub attributes: Vec<VertexAttribute>,
    /// Whether clip-space depth is already 0 to 1 (else it is -1 to 1 and is remapped).
    pub half_z: bool,
    /// For a copy shader (the vertex shader of a geometry pipeline): the size of the geometry shader's output item in `vec4`s, which
    /// its fetches from the ring use. Zero for an ordinary vertex shader.
    pub ring_out_vec4: u32,
}

/// Translates a pixel shader.
pub fn translate_pixel(decoded: &Decoded, inputs: &PixelInputs, ctx: &Context) -> Result<Translated, String> {
    translate(Stage::Pixel, decoded, Some(inputs), None, None, ctx)
}

/// Translates a vertex shader (its fetch shader is replaced by `inputs`).
pub fn translate_vertex(decoded: &Decoded, inputs: &VertexInputs, ctx: &Context) -> Result<Translated, String> {
    translate(Stage::Vertex, decoded, None, Some(inputs), None, ctx)
}

/// Translates the vertex shader of a geometry pipeline into a compute shader that writes the ring.
pub fn translate_es(decoded: &Decoded, inputs: &VertexInputs, ring: RingConfig, ctx: &Context) -> Result<Translated, String> {
    translate(Stage::Es, decoded, None, Some(inputs), Some(ring), ctx)
}

/// Translates a geometry shader into a compute shader.
pub fn translate_gs(decoded: &Decoded, ring: RingConfig, ctx: &Context) -> Result<Translated, String> {
    translate(Stage::Gs, decoded, None, None, Some(ring), ctx)
}

fn translate(
    stage: Stage,
    decoded: &Decoded,
    pixel: Option<&PixelInputs>,
    vertex: Option<&VertexInputs>,
    ring: Option<RingConfig>,
    ctx: &Context,
) -> Result<Translated, String> {
    let default_pixel = PixelInputs::default();
    let inputs = pixel.unwrap_or(&default_pixel);
    let mut g = Gen {
        out: String::new(),
        iface: Interface::default(),
        ctx,
        kcache: [Kcache { bank: 0, mode: 0, line: 0 }; 2],
        predicated: false,
        max_gpr: inputs.count.max(1),
        errors: Vec::new(),
        stage,
        ring: ring.unwrap_or_default(),
        ring_out_vec4: vertex.map_or(0, |v| v.ring_out_vec4),
        gpr_array: false,
        uses_ar: false,
        uses_al: false,
        index_mode: 0,
        loops: Vec::new(),
        next_loop: 0,
        next_clause: 0,
        debug_tex: false,
    };
    // a program that picks registers by an index has them as an array (only those: separate variables are faster)
    g.gpr_array = decoded.cf.iter().any(|c| match c {
        Cf::Alu { groups, .. } => groups.iter().flat_map(|gr| &gr.insts).any(|i| {
            let used = if i.three { 3 } else { 2 };
            i.dst_relative && i.write || i.src[..used].iter().any(|o| o.relative && matches!(o.source, Source::Gpr(_)))
        }),
        Cf::Fetch { fetches, .. } => fetches.iter().any(|f| f.src_relative || f.dst_relative),
        _ => false,
    });
    if let Some(v) = vertex {
        for a in &v.attributes {
            g.max_gpr = g.max_gpr.max(a.gpr + 1);
        }
    }
    g.predicated = decoded.cf.iter().any(|c| match c {
        Cf::Flow { inst, .. } => matches!(inst, 4..=17),
        Cf::Alu { kind, .. } => *kind != AluKind::Plain,
        _ => false,
    });
    for cf in &decoded.cf {
        match cf {
            Cf::Alu { kind, kcache, groups } => {
                g.kcache = *kcache;
                if matches!(kind, AluKind::PushBefore) {
                    g.line(1, "stack[sp] = live; sp = sp + 1;");
                }
                // a clause that breaks out of (or continues) its loop: the lanes it turns off leave the loop (or wait for the next time
                // round); the lanes already off stay as they are
                let clause = g.next_clause;
                if matches!(kind, AluKind::Break | AluKind::Continue) {
                    g.next_clause += 1;
                    if g.loops.is_empty() {
                        g.err(format!("the ALU clause kind {kind:?} outside a loop"));
                    }
                    g.line(1, &format!("let was_live{clause} = live;"));
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
                    AluKind::Break => g.line(1, &format!("if (was_live{clause} && !live) {{ break; }}")),
                    AluKind::Continue => g.line(1, &format!("if (was_live{clause} && !live) {{ live = true; continue; }}")),
                    AluKind::ElseAfter => g.line(1, "live = stack[sp - 1] && !live;"),
                }
            }
            Cf::Fetch { fetches, .. } => {
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
                for b in 0..*burst {
                    let (target, sel, reg) = (*array_base + b, *swizzle, *gpr + b);
                    let r = g.gpr(reg);
                    let comp = |s: u32, k: usize, old: &str| match s {
                        0..=3 => format!("{r}.{}", swz(s)),
                        4 => "0.0".to_string(),
                        5 => "1.0".to_string(),
                        _ => format!("{old}.{}", swz(k as u32)),
                    };
                    let guard = |text: String| if g.predicated { format!("if (live) {{ {text} }}") } else { text };
                    match (stage, *kind) {
                        (Stage::Pixel, 0) if target == 61 => {
                            g.iface.depth_output = true;
                            let line = guard(format!("out_z = {};", comp(sel[0], 0, "vec4<f32>(out_z)")));
                            g.line(1, &line);
                        }
                        (Stage::Pixel, 0) if target < 8 => {
                            g.iface.colour_outputs.insert(target);
                            let old = format!("out_{target}");
                            let v = format!(
                                "vec4<f32>({}, {}, {}, {})",
                                comp(sel[0], 0, &old),
                                comp(sel[1], 1, &old),
                                comp(sel[2], 2, &old),
                                comp(sel[3], 3, &old)
                            );
                            let line = guard(format!("out_{target} = {v};"));
                            g.line(1, &line);
                        }
                        (Stage::Vertex, 1) if target == 60 => {
                            g.iface.position = true;
                            let v = format!(
                                "vec4<f32>({}, {}, {}, {})",
                                comp(sel[0], 0, "out_pos"),
                                comp(sel[1], 1, "out_pos"),
                                comp(sel[2], 2, "out_pos"),
                                comp(sel[3], 3, "out_pos")
                            );
                            let line = guard(format!("out_pos = {v};"));
                            g.line(1, &line);
                        }
                        (Stage::Vertex, 2) => {
                            g.iface.params.insert(target);
                            let old = format!("out_p{target}");
                            let v = format!(
                                "vec4<f32>({}, {}, {}, {})",
                                comp(sel[0], 0, &old),
                                comp(sel[1], 1, &old),
                                comp(sel[2], 2, &old),
                                comp(sel[3], 3, &old)
                            );
                            let line = guard(format!("out_p{target} = {v};"));
                            g.line(1, &line);
                        }
                        _ => g.err(format!("an export of kind {kind} to {target}")),
                    }
                }
            }
            Cf::Flow {
                inst,
                pop_count,
                cond,
                cf_const,
                ..
            } => match inst {
                // LOOP_START (counted, sets the loop index), LOOP_START_DX10 (runs until it breaks), LOOP_START_NO_AL (counted)
                4 | 6 | 7 => {
                    let n = g.next_loop;
                    g.next_loop += 1;
                    let mut limit = LOOP_GUARD;
                    let mut step = None;
                    if *inst != 6 {
                        match (g.ctx.loop_const)(*cf_const) {
                            Some(c) => {
                                limit = c & 0xfff;
                                if *inst == 4 {
                                    step = Some(i32::from((c >> 24) as u8 as i8));
                                    g.uses_al = true;
                                    g.line(1, &format!("let al_saved{n} = al; al = {};", (c >> 12) & 0xff));
                                }
                            }
                            None => g.err(format!("a counted loop on loop constant {cf_const}, which is not set")),
                        }
                    }
                    if *cond != 0 {
                        g.err(format!("a loop with condition {cond}"));
                    }
                    g.line(
                        1,
                        &format!("stack[sp] = live; sp = sp + 1; let loop_sp{n} = sp; var trips{n} = 0u;"),
                    );
                    g.line(1, "loop {");
                    g.line(1, &format!("sp = loop_sp{n}; if (!live || trips{n} >= {limit}u) {{ break; }}"));
                    g.loops.push(LoopFrame { n, step });
                }
                // LOOP_END: the stack entry its start pushed is popped (its pop count does not apply)
                5 => match g.loops.pop() {
                    Some(LoopFrame { n, step }) => {
                        let al = step.map_or(String::new(), |s| format!(" al = al + {s};"));
                        g.line(1, &format!("continuing {{ trips{n} = trips{n} + 1u;{al} }}"));
                        g.line(1, "}");
                        g.line(1, &format!("sp = loop_sp{n} - 1; live = stack[sp];"));
                        if step.is_some() {
                            g.line(1, &format!("al = al_saved{n};"));
                        }
                    }
                    None => g.err("a loop end with no loop".into()),
                },
                // LOOP_CONTINUE and LOOP_BREAK: the lanes that are live go round again (live) or leave the loop
                8 | 9 => {
                    if g.loops.is_empty() {
                        g.err(format!("the control flow instruction {inst:#x} outside a loop"));
                    }
                    if *cond != 0 {
                        g.err(format!("the control flow instruction {inst:#x} with condition {cond}"));
                    }
                    g.line(
                        1,
                        if *inst == 8 {
                            "if (live) { continue; }"
                        } else {
                            "if (live) { break; }"
                        },
                    );
                }
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
            Cf::Nop | Cf::Return => {}
            Cf::Ring {
                array_base,
                gpr,
                burst,
                mask,
            } => {
                if !matches!(stage, Stage::Es | Stage::Gs) {
                    g.err("a write to a ring outside a geometry pipeline".into());
                    continue;
                }
                g.iface.ring_dwords = g.iface.ring_dwords.max(array_base + 4 * burst);
                for b in 0..*burst {
                    let r = g.gpr(gpr + b);
                    let element = if stage == Stage::Es {
                        format!("vid * {}u + {}u", g.ring.es_vec4, array_base / 4 + b)
                    } else {
                        format!("base_out + {}u", array_base / 4 + b)
                    };
                    let target = if stage == Stage::Es { "ring_in" } else { "ring_out" };
                    for (k, c) in ['x', 'y', 'z', 'w'].iter().enumerate() {
                        if mask & (1 << k) != 0 {
                            let text = format!("{target}[{element}].{c} = {r}.{c};");
                            let line = if g.predicated { format!("if (live) {{ {text} }}") } else { text };
                            g.line(1, &line);
                        }
                    }
                }
            }
            Cf::Emit { inst } => {
                if stage != Stage::Gs {
                    g.err("a vertex emission outside a geometry shader".into());
                    continue;
                }
                let mut text = String::new();
                if matches!(inst, 0x15 | 0x16) {
                    text.push_str(
                        "{ let n = emitted; if (strip >= 2u) { let at = (base_idx + n) * 3u; if ((strip & 1u) == 0u) { gs_idx[at] = base_idx + n - 2u; gs_idx[at + 1u] = base_idx + n - 1u; } else { gs_idx[at] = base_idx + n - 1u; gs_idx[at + 1u] = base_idx + n - 2u; } gs_idx[at + 2u] = base_idx + n; } strip = strip + 1u; emitted = emitted + 1u; }",
                    );
                }
                if matches!(inst, 0x16 | 0x17) {
                    text.push_str(" strip = 0u;");
                }
                let line = if g.predicated { format!("if (live) {{ {text} }}") } else { text };
                g.line(1, &line);
            }
            Cf::Memory { inst } => g.err(format!("the memory or geometry instruction {inst:#x}")),
            Cf::CallFetchShader => {
                if stage == Stage::Pixel {
                    g.err("a call to a fetch shader in a pixel shader".into());
                }
            }
        }
    }
    if !g.loops.is_empty() {
        g.err("a loop with no end".into());
    }
    if !g.errors.is_empty() {
        return Err(g.errors.join("; "));
    }
    // the address register and the loop index, before the body that uses them
    let mut prefix = String::new();
    if g.gpr_array {
        // an index past the registers the program names reads the last of them (the hardware gives such a program no more)
        let _ = writeln!(prefix, "    const gpr_last = {};", g.max_gpr.min(GPR_FILE) - 1);
    }
    if g.uses_ar {
        prefix.push_str("    var ar = vec4<i32>(0);\n");
    }
    if g.uses_al {
        prefix.push_str("    var al = 0;\n");
    }
    g.out = prefix + &g.out;
    if stage == Stage::Pixel && alpha_test_line(&g).is_some() {
        g.iface.kills = true;
    }
    // assemble the module
    let mut m = String::from(HEADER);
    let consts_group = if stage == Stage::Pixel { 1 } else { 0 };
    for (bank, len) in &g.iface.banks {
        let _ = writeln!(
            m,
            "@group({consts_group}) @binding({bank}) var<uniform> cb{bank}: array<vec4<f32>, {}>;",
            (*len).max(1)
        );
    }
    if g.iface.const_file > 0 {
        let _ = writeln!(
            m,
            "@group({consts_group}) @binding(15) var<uniform> cfile: array<vec4<f32>, {}>;",
            g.iface.const_file
        );
    }
    for (r, kind) in &g.iface.textures {
        let ty = match kind {
            TexKind::D2 => "texture_2d<f32>",
            TexKind::D3 => "texture_3d<f32>",
            TexKind::Cube => "texture_2d_array<f32>",
            TexKind::Depth2d => "texture_depth_2d",
        };
        let _ = writeln!(m, "@group(2) @binding({r}) var tex{r}: {ty};");
    }
    for s in &g.iface.samplers {
        let _ = writeln!(m, "@group(2) @binding({}) var smp{s}: sampler;", 16 + s);
    }
    for s in &g.iface.compare_samplers {
        let _ = writeln!(m, "@group(2) @binding({}) var cmp{s}: sampler_comparison;", 32 + s);
    }
    match stage {
        Stage::Pixel => assemble_pixel(&mut m, &g, inputs),
        Stage::Vertex => assemble_vertex(&mut m, &g, vertex.expect("a vertex shader has its inputs")),
        Stage::Es => assemble_es(&mut m, &g, vertex.expect("a vertex shader has its inputs")),
        Stage::Gs => assemble_gs(&mut m, &g),
    }
    Ok(Translated {
        wgsl: m,
        interface: g.iface,
    })
}

/// Declares register `n` with its first value: a variable of its own, or an element of the array of registers.
fn declare_gpr(m: &mut String, g: &Gen, n: u32, init: &str) {
    if g.gpr_array {
        if n == 0 {
            let _ = writeln!(m, "    var r: array<vec4<f32>, {}>;", g.max_gpr.min(GPR_FILE));
        }
        let _ = writeln!(m, "    r[{n}] = {init};");
    } else {
        let _ = writeln!(m, "    var r{n}: vec4<f32> = {init};");
    }
}

/// The alpha test as a line of the pixel shader: a pixel whose first colour's alpha fails the comparison with the reference is not
/// drawn. `None` when there is no test (or it always passes, or there is no first colour).
fn alpha_test_line(g: &Gen) -> Option<String> {
    if g.ctx.debug != DebugPs::Off {
        return None;
    }
    let (func, reference) = g.ctx.alpha_test?;
    if !g.iface.colour_outputs.contains(&0) {
        return None;
    }
    let op = match func & 7 {
        0 => return Some("    discard;\n".into()),
        1 => "<",
        2 => "==",
        3 => "<=",
        4 => ">",
        5 => "!=",
        6 => ">=",
        _ => return None,
    };
    Some(format!(
        "    if (!(out_0.w {op} {})) {{ discard; }}\n",
        literal(reference.to_bits())
    ))
}

/// The line that replaces the first colour for `DebugPs` (after the program, before the alpha test).
fn debug_line(g: &Gen, inputs: &PixelInputs) -> Option<String> {
    if !g.iface.colour_outputs.contains(&0) {
        return None;
    }
    Some(match g.ctx.debug {
        DebugPs::Off => return None,
        DebugPs::White => "    out_0 = vec4<f32>(1.0);\n".into(),
        DebugPs::Alpha1 => "    out_0.w = 1.0;\n".into(),
        DebugPs::Tex => "    out_0 = vec4<f32>(debug_tex.xyz, 1.0);\n".into(),
        DebugPs::Uv => "    out_0 = select(vec4<f32>(1.0, 0.0, 1.0, 1.0), vec4<f32>(fract(debug_uv), 0.0, 1.0), debug_have);\n".into(),
        DebugPs::Input { n, signed } => {
            let fed = n < inputs.count && inputs.locations.get(n as usize).copied().unwrap_or(n) != u32::MAX;
            if !fed {
                // no such input: magenta
                "    out_0 = vec4<f32>(1.0, 0.0, 1.0, 1.0);\n".into()
            } else if signed {
                format!("    out_0 = vec4<f32>(in.i{n}.xyz * 0.5 + 0.5, 1.0);\n")
            } else {
                format!("    out_0 = vec4<f32>(in.i{n}.xyz, 1.0);\n")
            }
        }
    })
}

fn assemble_pixel(m: &mut String, g: &Gen, inputs: &PixelInputs) {
    m.push_str("struct In {\n");
    for i in 0..inputs.count {
        let flat = if inputs.flat.get(i as usize).copied().unwrap_or(false) {
            " @interpolate(flat)"
        } else {
            ""
        };
        let location = inputs.locations.get(i as usize).copied().unwrap_or(i);
        // an input no vertex output feeds (the location is `u32::MAX`) reads as zero
        if location != u32::MAX {
            let _ = writeln!(m, "    @location({location}){flat} i{i}: vec4<f32>,");
        }
    }
    m.push_str("    @builtin(position) frag: vec4<f32>,\n}\n");
    m.push_str("struct Out {\n");
    for t in &g.iface.colour_outputs {
        let _ = writeln!(m, "    @location({t}) c{t}: vec4<f32>,");
    }
    if g.iface.depth_output {
        m.push_str("    @builtin(frag_depth) z: f32,\n");
    }
    if g.iface.colour_outputs.is_empty() && !g.iface.depth_output {
        m.push_str("    @location(0) unused: vec4<f32>,\n");
    }
    m.push_str("}\n");
    m.push_str("@fragment fn main(in: In) -> Out {\n");
    for n in 0..g.max_gpr {
        let fed = inputs.locations.get(n as usize).copied().unwrap_or(n) != u32::MAX;
        let init = if n < inputs.count && fed {
            format!("in.i{n}")
        } else {
            "vec4<f32>(0.0)".to_string()
        };
        declare_gpr(m, g, n, &init);
    }
    m.push_str("    var pv = vec4<f32>(0.0);\n    var ps = 0.0;\n    var live = true;\n    var stack: array<bool, 16>;\n    var sp = 0;\n");
    if matches!(g.ctx.debug, DebugPs::Tex | DebugPs::Uv) {
        m.push_str("    var debug_tex = vec4<f32>(1.0, 0.0, 1.0, 1.0);\n    var debug_uv = vec2<f32>(0.0);\n    var debug_have = false;\n");
    }
    for t in &g.iface.colour_outputs {
        let _ = writeln!(m, "    var out_{t} = vec4<f32>(0.0);");
    }
    if g.iface.depth_output {
        m.push_str("    var out_z = 0.0;\n");
    }
    m.push_str(&g.out);
    if let Some(line) = debug_line(g, inputs) {
        m.push_str(&line);
    }
    if let Some(test) = alpha_test_line(g) {
        m.push_str(&test);
    }
    m.push_str("    var o: Out;\n");
    for t in &g.iface.colour_outputs {
        let _ = writeln!(m, "    o.c{t} = out_{t};");
    }
    if g.iface.depth_output {
        m.push_str("    o.z = out_z;\n");
    }
    m.push_str("    return o;\n}\n");
}

/// The position a vertex shader hands on, invariant: the game draws a mesh's depth first with one pixel shader and its colour
/// after, testing less-or-equal against that depth, and without `@invariant` the GPU's compiler may work out the same position
/// differently for the two pipelines (outputs one pixel shader does not read are dropped, and multiply-adds fused or not), so the
/// colour pass loses to its own depth in bands.
pub const VOUT_POSITION: &str = "struct VOut {\n    @invariant @builtin(position) pos: vec4<f32>,\n";

fn assemble_vertex(m: &mut String, g: &Gen, inputs: &VertexInputs) {
    // the decoded attribute streams: one storage buffer of vec4 and, per stream, where it starts
    m.push_str(
        "@group(3) @binding(0) var<storage, read> va: array<vec4<f32>>;\n@group(3) @binding(1) var<uniform> vmeta: array<vec4<u32>, 16>;\n",
    );
    if g.iface.reads_ring {
        m.push_str("@group(3) @binding(2) var<storage, read> ring_out: array<vec4<f32>>;\n");
    }
    m.push_str(VOUT_POSITION);
    for p in &g.iface.params {
        let _ = writeln!(m, "    @location({p}) p{p}: vec4<f32>,");
    }
    m.push_str("}\n@vertex fn main(@builtin(vertex_index) vid: u32, @builtin(instance_index) iid: u32) -> VOut {\n");
    // the bindings are used even when the shader reads none of them, so the pipeline layout is always the same
    m.push_str("    let touched = vmeta[0].x + u32(arrayLength(&va) > 0u);\n");
    if g.iface.reads_ring {
        m.push_str("    let touched_ring = arrayLength(&ring_out);\n");
    }
    for n in 0..g.max_gpr {
        let init = if n == 0 {
            "vec4<f32>(fu(vid), 0.0, 0.0, fu(iid))".to_string()
        } else if let Some(a) = inputs.attributes.iter().find(|a| a.gpr == n) {
            let index = if a.per_instance {
                format!("(iid / {}u)", a.divisor.max(1))
            } else {
                "vid".to_string()
            };
            format!("va[vmeta[{}].x + {index}]", a.stream)
        } else {
            "vec4<f32>(0.0)".to_string()
        };
        declare_gpr(m, g, n, &init);
    }
    m.push_str("    var pv = vec4<f32>(0.0);\n    var ps = 0.0;\n    var live = true;\n    var stack: array<bool, 16>;\n    var sp = 0;\n");
    m.push_str("    var out_pos = vec4<f32>(0.0);\n");
    for p in &g.iface.params {
        let _ = writeln!(m, "    var out_p{p} = vec4<f32>(0.0);");
    }
    m.push_str(&g.out);
    m.push_str("    var o: VOut;\n");
    if inputs.half_z {
        m.push_str("    o.pos = out_pos;\n");
    } else {
        // the depth range of clip space is -w to w; the target API's is 0 to w
        m.push_str("    o.pos = vec4<f32>(out_pos.x, out_pos.y, (out_pos.z + out_pos.w) * 0.5, out_pos.w);\n");
    }
    for p in &g.iface.params {
        let _ = writeln!(m, "    o.p{p} = out_p{p};");
    }
    m.push_str("    return o;\n}\n");
}

impl Gen<'_> {
    /// A vertex-style fetch in a texture clause on a resource from 128 up: the element of a uniform block chosen by an index in a
    /// register (what a dynamically indexed uniform array becomes).
    fn uniform_fetch(&mut self, f: &Fetch) {
        let src = self.fetch_gpr(f.src_gpr, f.src_relative);
        let index = match f.src_sel[0] {
            0..=3 => format!("{src}.{}", swz(f.src_sel[0])),
            _ => "0.0".to_string(),
        };
        let element = if f.resource == RING_RESOURCE {
            // a read of a ring: the geometry shader reads what the vertex shader wrote (its address is in dwords, the offset in bytes);
            // the copy shader reads what the geometry shader wrote (its address is the vertex number)
            match self.stage {
                Stage::Gs => format!("ring_in[(iu({index}) * 4u + {}u) / 16u]", f.vtx_offset),
                Stage::Vertex if self.ring_out_vec4 > 0 => {
                    self.iface.reads_ring = true;
                    format!("ring_out[iu({index}) * {}u + {}u]", self.ring_out_vec4, f.vtx_offset / 16)
                }
                _ => {
                    self.err("a ring fetch outside a geometry pipeline".into());
                    return;
                }
            }
        } else if f.resource < 128 {
            self.err(format!("a buffer fetch on resource {}", f.resource));
            return;
        } else {
            let bank = f.resource - 128;
            let entry = self.iface.banks.entry(bank).or_default();
            *entry = DYNAMIC_BANK;
            let element = format!("cb{bank}[min(iu({index}), {}u)]", DYNAMIC_BANK - 1);
            // the block is read as little-endian words; the fetch may ask for the bytes to be turned around
            match f.endian {
                1 => format!("swap8in16({element})"),
                2 => format!("swap8in32({element})"),
                _ => element,
            }
        };
        let dst = self.fetch_gpr(f.dst_gpr, f.dst_relative);
        self.line(1, "{");
        self.line(2, &format!("let e = {element};"));
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

    /// The register of a fetch: relative ones add the loop index.
    fn fetch_gpr(&mut self, n: u32, relative: bool) -> String {
        if relative {
            self.uses_al = true;
            self.gpr_indexed(n, "al")
        } else {
            self.gpr(n)
        }
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
        let src = self.fetch_gpr(f.src_gpr, f.src_relative);
        let sel = |s: u32| match s {
            0..=3 => format!("{src}.{}", swz(s)),
            4 => "0.0".to_string(),
            _ => "1.0".to_string(),
        };
        let coords: Vec<String> = f.src_sel.iter().map(|&s| sel(s)).collect();
        let uv = match kind {
            TexKind::D2 | TexKind::Depth2d => format!("vec2<f32>({}, {})", coords[0], coords[1]),
            TexKind::D3 => format!("vec3<f32>({}, {}, {})", coords[0], coords[1], coords[2]),
            // a cube map is read as six layers: the coordinates are the face's own (the CUBE instruction made them)
            TexKind::Cube => format!("vec2<f32>({}, {}), i32(floor({} + 0.5))", coords[0], coords[1], coords[2]),
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
        // the texture's component selectors, as the hardware applies them to what it reads
        let select = if compare || kind == TexKind::Depth2d {
            IDENTITY_SELECT
        } else {
            (self.ctx.texture_select)(f.resource)
        };
        if select == IDENTITY_SELECT {
            self.line(2, &format!("let s = {call};"));
        } else {
            self.line(2, &format!("let raw = {call};"));
            let pick = |shift: u32| match (select >> shift) & 7 {
                c @ 0..=3 => format!("raw.{}", swz(c)),
                4 => "0.0".to_string(),
                _ => "1.0".to_string(),
            };
            let v = format!("vec4<f32>({}, {}, {}, {})", pick(24), pick(16), pick(8), pick(0));
            self.line(2, &format!("let s = {v};"));
        }
        if !compare && !self.debug_tex && matches!(self.ctx.debug, DebugPs::Tex | DebugPs::Uv) {
            self.debug_tex = true;
            self.line(
                2,
                &format!(
                    "debug_tex = s; debug_uv = vec2<f32>({}, {}); debug_have = true;",
                    coords[0], coords[1]
                ),
            );
        }
        let dst = self.fetch_gpr(f.dst_gpr, f.dst_relative);
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

fn assemble_es(m: &mut String, g: &Gen, inputs: &VertexInputs) {
    m.push_str(
        "@group(1) @binding(0) var<storage, read> va: array<vec4<f32>>;\n@group(1) @binding(1) var<uniform> vmeta: array<vec4<u32>, 16>;\n",
    );
    m.push_str("@group(1) @binding(2) var<storage, read_write> ring_in: array<vec4<f32>>;\n@group(1) @binding(5) var<uniform> params: vec4<u32>;\n");
    m.push_str("@compute @workgroup_size(64) fn main(@builtin(global_invocation_id) gid: vec3<u32>) {\n    let vid = gid.x;\n");
    m.push_str("    if (vid >= params.x + 0u * (vmeta[0].x + u32(arrayLength(&va) > 0u))) { return; }\n    let iid = 0u;\n");
    for n in 0..g.max_gpr {
        let init = if n == 0 {
            "vec4<f32>(fu(vid), 0.0, 0.0, fu(iid))".to_string()
        } else if let Some(a) = inputs.attributes.iter().find(|a| a.gpr == n) {
            let index = if a.per_instance {
                format!("(iid / {}u)", a.divisor.max(1))
            } else {
                "vid".to_string()
            };
            format!("va[vmeta[{}].x + {index}]", a.stream)
        } else {
            "vec4<f32>(0.0)".to_string()
        };
        declare_gpr(m, g, n, &init);
    }
    m.push_str("    var pv = vec4<f32>(0.0);\n    var ps = 0.0;\n    var live = true;\n    var stack: array<bool, 16>;\n    var sp = 0;\n");
    m.push_str(&g.out);
    m.push_str("}\n");
}

fn assemble_gs(m: &mut String, g: &Gen) {
    m.push_str("@group(1) @binding(2) var<storage, read_write> ring_in: array<vec4<f32>>;\n@group(1) @binding(3) var<storage, read_write> ring_out: array<vec4<f32>>;\n");
    m.push_str(
        "@group(1) @binding(4) var<storage, read_write> gs_idx: array<u32>;\n@group(1) @binding(5) var<uniform> params: vec4<u32>;\n",
    );
    let _ = writeln!(
        m,
        "@compute @workgroup_size(64) fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{\n    let p = gid.x;\n    if (p >= params.x + 0u * (u32(arrayLength(&ring_in) > 0u) + u32(arrayLength(&ring_out) > 0u) + u32(arrayLength(&gs_idx) > 0u))) {{ return; }}"
    );
    let _ = writeln!(
        m,
        "    let base_out = p * {}u;\n    let base_idx = p * {}u;\n    var emitted = 0u;\n    var strip = 0u;",
        g.ring.max_vertices * g.ring.gs_vec4,
        g.ring.max_vertices
    );
    for n in 0..g.max_gpr {
        // the first register holds where the input vertex's item is in the ring, in dwords
        let init = if n == 0 {
            format!(
                "vec4<f32>(fu((p + params.y) * {}u), 0.0, 0.0, fu(p + params.y))",
                g.ring.es_vec4 * 4
            )
        } else {
            "vec4<f32>(0.0)".to_string()
        };
        declare_gpr(m, g, n, &init);
    }
    m.push_str("    var pv = vec4<f32>(0.0);\n    var ps = 0.0;\n    var live = true;\n    var stack: array<bool, 16>;\n    var sp = 0;\n");
    m.push_str(&g.out);
    m.push_str("}\n");
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
        let ctx = Context::new(&|_| TexKind::D2);
        let t = translate_pixel(
            &d,
            &PixelInputs {
                count: 2,
                flat: vec![],
                locations: vec![],
            },
            &ctx,
        )
        .unwrap();
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
        let ctx = Context::new(&|_| TexKind::D2);
        assert!(translate_pixel(&d, &PixelInputs::default(), &ctx).is_err());
    }

    #[test]
    fn the_vertex_position_is_invariant() {
        assert!(VOUT_POSITION.contains("@invariant @builtin(position)"));
        valid(&format!(
            "{VOUT_POSITION}}}\n@vertex fn main() -> VOut {{\n    var o: VOut;\n    o.pos = vec4<f32>(0.0, 0.0, 0.0, 1.0);\n    return o;\n}}\n"
        ));
    }

    /// Parses and validates a module the way the GPU backend will.
    fn valid(wgsl: &str) {
        let module = naga::front::wgsl::parse_str(wgsl).unwrap_or_else(|e| panic!("{}\n{wgsl}", e.emit_to_string(wgsl)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}\n{wgsl}"));
    }

    /// A loop that runs until its clause breaks, adding a register picked by the address register each time round:
    ///
    /// ```text
    /// 0 LOOP_START_DX10
    /// 1 ALU_BREAK: MOVA_INT AR.x <- R1.x; ADD R0.x <- R0.x + R1[AR.x].y; PRED_SETGE R0.x >= 1.0 (turns off the lanes that are done)
    /// 2 LOOP_END
    /// 3 EXPORT_DONE colour 0 <- R0
    /// ```
    fn looping(loop_inst: u32, cf_const: u32) -> Vec<u8> {
        words(&[
            3,
            (loop_inst << 23) | (cf_const << 3),
            4,
            (14 << 26) | (2 << 18),
            1,
            5 << 23,
            0,
            (0x28 << 23) | (1 << 21) | (3 << 9) | (2 << 6) | (1 << 3),
            // MOVA_INT: src0 r1.x, dst channel x
            1 | (1 << 31),
            0x18 << 7,
            // ADD r0.x = r0.x + r[1 + AR.x].y (src1 relative, index mode AR.x)
            (1 << 13) | (1 << 22) | (1 << 23) | (1 << 31),
            (1 << 4),
            // PRED_SETGE r0.x >= 1.0, updating the execution mask and the predicate
            (249 << 13) | (1 << 31),
            (0x22 << 7) | (1 << 2) | (1 << 3),
        ])
    }

    #[test]
    fn a_loop_that_breaks_with_registers_picked_by_the_address_register() {
        let d = decode(&looping(6, 0)).unwrap();
        let ctx = Context::new(&|_| TexKind::D2);
        let inputs = PixelInputs {
            count: 2,
            flat: vec![],
            locations: vec![],
        };
        let t = translate_pixel(&d, &inputs, &ctx).unwrap();
        valid(&t.wgsl);
        assert!(t.wgsl.contains("loop {"), "{}", t.wgsl);
        if std::env::var_os("SHOW_WGSL").is_some() {
            println!("{}", t.wgsl);
        }
        assert!(t.wgsl.contains("ar.x = ii(t0);"), "{}", t.wgsl);
        assert!(t.wgsl.contains("r[clamp(1 + ar.x, 0, gpr_last)].y"), "{}", t.wgsl);
        assert!(t.wgsl.contains("if (was_live0 && !live) { break; }"), "{}", t.wgsl);
        assert!(t.wgsl.contains(&format!("trips0 >= {LOOP_GUARD}u")), "{}", t.wgsl);
    }

    #[test]
    fn a_counted_loop_reads_its_constant_and_steps_the_loop_index() {
        let d = decode(&looping(4, 3)).unwrap();
        // without the constant it is an error, not a guess
        assert!(translate_pixel(&d, &PixelInputs::default(), &Context::new(&|_| TexKind::D2)).is_err());
        // count 5, start 2, step -1
        let constant = |n: u32| (n == 3).then_some(5 | (2 << 12) | (0xff << 24));
        let mut ctx = Context::new(&|_| TexKind::D2);
        ctx.loop_const = &constant;
        let t = translate_pixel(&d, &PixelInputs::default(), &ctx).unwrap();
        valid(&t.wgsl);
        assert!(t.wgsl.contains("al = 2;"), "{}", t.wgsl);
        assert!(t.wgsl.contains("trips0 >= 5u"), "{}", t.wgsl);
        assert!(t.wgsl.contains("al = al + -1;"), "{}", t.wgsl);
    }

    /// A pixel shader that reads texture 1 at r1.xy and exports what it read as colour 0.
    fn textured() -> Vec<u8> {
        words(&[
            // TEX clause at slot 2 (word 4), one fetch
            2,
            1 << 23,
            // export colour 0 from r0 (x, y, z, w): end of program
            0,
            (0x28 << 23) | (1 << 21) | (3 << 9) | (2 << 6) | (1 << 3),
            // SAMPLE, resource 1, from r1; to r0.xyzw; sampler 1, coordinates r1.x, r1.y, 0, 0
            0x10 | (1 << 8) | (1 << 16),
            (1 << 12) | (2 << 15) | (3 << 18),
            (1 << 15) | (1 << 23) | (4 << 26) | (4 << 29),
            0,
        ])
    }

    #[test]
    fn debug_overrides_replace_the_first_colour() {
        let d = decode(&textured()).unwrap();
        let inputs = PixelInputs {
            count: 2,
            flat: vec![],
            locations: vec![],
        };
        let mut ctx = Context::new(&|_| TexKind::D2);
        ctx.alpha_test = Some((4, 0.0));
        let plain = translate_pixel(&d, &inputs, &ctx).unwrap();
        valid(&plain.wgsl);
        assert!(
            plain.wgsl.contains("textureSample(tex1, smp1, vec2<f32>(r1.x, r1.y))"),
            "{}",
            plain.wgsl
        );
        assert!(!plain.wgsl.contains("debug"));
        for (mode, wanted) in [
            ("white", "out_0 = vec4<f32>(1.0);"),
            ("alpha1", "out_0.w = 1.0;"),
            ("tex0", "out_0 = vec4<f32>(debug_tex.xyz, 1.0);"),
            ("uv", "fract(debug_uv)"),
            ("input1", "out_0 = vec4<f32>(in.i1.xyz, 1.0);"),
            ("normals", "in.i1.xyz * 0.5 + 0.5"),
            ("input7", "out_0 = vec4<f32>(1.0, 0.0, 1.0, 1.0);"),
        ] {
            ctx.debug = DebugPs::parse(mode);
            assert_ne!(ctx.debug, DebugPs::Off, "{mode}");
            let t = translate_pixel(&d, &inputs, &ctx).unwrap();
            valid(&t.wgsl);
            assert!(t.wgsl.contains(wanted), "{mode}: {}", t.wgsl);
            // the override comes after the program and no alpha test can hide what it shows
            assert!(!t.wgsl.contains("discard"), "{mode}: {}", t.wgsl);
        }
        ctx.debug = DebugPs::Tex;
        let t = translate_pixel(&d, &inputs, &ctx).unwrap();
        assert!(t.wgsl.contains("debug_tex = s; debug_uv = vec2<f32>(r1.x, r1.y);"), "{}", t.wgsl);
        assert_eq!(DebugPs::parse("nonsense"), DebugPs::Off);
        assert_eq!(DebugPs::parse(" TEX "), DebugPs::Tex);
        assert_eq!(DebugPs::parse("normal3"), DebugPs::Input { n: 3, signed: true });
    }

    /// A pixel shader that reads element r1.x of uniform block 1 (resource 129) into r0 with a vertex-style fetch whose bytes are turned
    /// around as `endian` says, and exports r0 as colour 0.
    fn indexed_uniform(endian: u32) -> Vec<u8> {
        words(&[
            2,
            1 << 23,
            0,
            (0x28 << 23) | (1 << 21) | (3 << 9) | (2 << 6) | (1 << 3),
            (129 << 8) | (1 << 16),
            (1 << 12) | (2 << 15) | (3 << 18),
            endian << 16,
            0,
        ])
    }

    #[test]
    fn an_indexed_uniform_fetch_turns_the_bytes_around_only_when_asked() {
        let ctx = Context::new(&|_| TexKind::D2);
        let inputs = PixelInputs {
            count: 2,
            flat: vec![],
            locations: vec![],
        };
        for (endian, wanted) in [
            (0, "let e = cb1[min(iu(r1.x)"),
            (2, "let e = swap8in32(cb1[min(iu(r1.x)"),
            (1, "swap8in16(cb1["),
        ] {
            let d = decode(&indexed_uniform(endian)).unwrap();
            let t = translate_pixel(&d, &inputs, &ctx).unwrap();
            valid(&t.wgsl);
            assert!(t.wgsl.contains(wanted), "endian {endian}: {}", t.wgsl);
        }
    }

    #[test]
    fn the_alpha_test_goes_into_the_pixel_shader() {
        let d = decode(&sample()).unwrap();
        let mut ctx = Context::new(&|_| TexKind::D2);
        ctx.alpha_test = Some((6, 0.5));
        let t = translate_pixel(&d, &PixelInputs::default(), &ctx).unwrap();
        valid(&t.wgsl);
        assert!(t.wgsl.contains("if (!(out_0.w >= (0.5))) { discard; }"), "{}", t.wgsl);
        assert!(t.interface.kills);
        // always: no test
        ctx.alpha_test = Some((7, 0.5));
        let t = translate_pixel(&d, &PixelInputs::default(), &ctx).unwrap();
        assert!(!t.wgsl.contains("discard"));
    }
}
