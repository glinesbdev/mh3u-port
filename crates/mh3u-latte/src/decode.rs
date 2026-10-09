//! Decoding a Latte shader program into its control flow, ALU instruction groups and fetches.
//!
//! The bit layouts are those of the R7xx instruction set (the word of an ALU instruction has the output modifier in bits 5 and 6 and
//! the two-operand instruction number in bits 7 to 17; the numbers are the R6xx ones) (AMD's documents, and the open drivers for the same family, which
//! number the instructions); every program of the game parses to the end with them (checked by `mh3u-tools latte-check`). Words are
//! little-endian. Nothing here knows what an instruction *does*: that is the translator's job.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Truncated(&'static str),
    Unknown(&'static str, u32),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated(what) => write!(f, "{what} runs off the end of the program"),
            Error::Unknown(what, n) => write!(f, "{what} {n:#x} is not known"),
        }
    }
}

impl std::error::Error for Error {}

fn word(program: &[u8], index: usize) -> Result<u32, Error> {
    let at = index.checked_mul(4).ok_or(Error::Truncated("a word"))?;
    program
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(Error::Truncated("a word"))
}

/// Where a source operand comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Gpr(u32),
    /// A constant of a kcache bank (`bank` 0 or 1 of the clause), `index` into the bank's window.
    Kcache {
        bank: u32,
        index: u32,
    },
    /// The constant file (`c[index]`), which the game fills with initial values and uniform registers.
    ConstFile(u32),
    Zero,
    One,
    OneInt,
    MinusOneInt,
    Half,
    /// A literal constant: the channel (x, y, z, w) of the literal slots after the group.
    Literal,
    /// The previous group's result of the vector slot with this channel.
    PreviousVector,
    PreviousScalar,
    Other(u32),
}

fn source(sel: u32) -> Source {
    match sel {
        0..=127 => Source::Gpr(sel),
        128..=159 => Source::Kcache { bank: 0, index: sel - 128 },
        160..=191 => Source::Kcache { bank: 1, index: sel - 160 },
        248 => Source::Zero,
        249 => Source::One,
        250 => Source::OneInt,
        251 => Source::MinusOneInt,
        252 => Source::Half,
        253 => Source::Literal,
        254 => Source::PreviousVector,
        255 => Source::PreviousScalar,
        256..=511 => Source::ConstFile(sel - 256),
        _ => Source::Other(sel),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Operand {
    pub source: Source,
    /// Relative addressing: the index is added to the address register.
    pub relative: bool,
    pub channel: u32,
    pub negate: bool,
    pub absolute: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AluInst {
    /// The instruction number, and whether it has three operands (the numbers of the two kinds are separate).
    pub op: u32,
    pub three: bool,
    pub src: [Operand; 3],
    pub dst_gpr: u32,
    pub dst_relative: bool,
    pub dst_channel: u32,
    /// Whether the result is written to the register (two-operand instructions can leave it out; their result is still the group's).
    pub write: bool,
    pub clamp: bool,
    /// Output modifier: 0 none, 1 times two, 2 times four, 3 divided by two.
    pub omod: u32,
    pub update_exec_mask: bool,
    pub update_pred: bool,
    pub pred_sel: u32,
    pub bank_swizzle: u32,
    /// What a relatively addressed operand or destination adds to its index: 0 to 3 a component of the address register (`AR`),
    /// 4 the loop index (`aL`).
    pub index_mode: u32,
    pub last: bool,
}

/// One group of instructions issued together (up to five: the four vector slots and the transcendental one), with the literal
/// constants that follow it.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub insts: Vec<AluInst>,
    pub literals: [u32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kcache {
    pub bank: u32,
    /// 0 off, 1 one line (16 constants) locked, 2 two lines.
    pub mode: u32,
    /// The line, in units of 16 constants.
    pub line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fetch {
    /// 0 is a vertex fetch (`VTX`), the rest are texture instructions (`SAMPLE` is 0x10 in the texture clause numbering).
    pub inst: u32,
    pub resource: u32,
    pub sampler: u32,
    pub src_gpr: u32,
    pub src_relative: bool,
    pub src_sel: [u32; 4],
    pub dst_gpr: u32,
    pub dst_relative: bool,
    pub dst_sel: [u32; 4],
    pub coord_normalized: [bool; 4],
    pub lod_bias: u32,
    pub offset: [u32; 3],
    /// For vertex fetches: the buffer, format and offset.
    pub fetch_type: u32,
    pub data_format: u32,
    pub num_format: u32,
    pub format_comp_signed: bool,
    pub srf_mode: u32,
    pub mega_fetch_count: u32,
    pub vtx_offset: u32,
    pub endian: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cf {
    /// An ALU clause: its instruction groups, the kcache windows and how it treats the predicate stack.
    Alu {
        kind: AluKind,
        kcache: [Kcache; 2],
        groups: Vec<Group>,
    },
    /// A texture clause (`vertex` when it is a vertex-fetch clause).
    Fetch {
        vertex: bool,
        fetches: Vec<Fetch>,
    },
    Export {
        /// 0 pixel colour, 1 position, 2 parameter (vertex output), others for memory writes.
        kind: u32,
        array_base: u32,
        gpr: u32,
        swizzle: [u32; 4],
        burst: u32,
        /// 39 is `EXPORT`, 40 `EXPORT_DONE`.
        inst: u32,
    },
    /// Everything else that moves control: `inst` is the instruction number (10 `JUMP`, 11 `PUSH`, 12 `PUSH_ELSE`, 13 `ELSE`, 14
    /// `POP`, 15 `POP_JUMP`, 16 `POP_PUSH`, 17 `POP_PUSH_ELSE`, 4 `LOOP_END`..., see `cf_name`).
    Flow {
        inst: u32,
        addr: u32,
        pop_count: u32,
        cond: u32,
        /// The constant the instruction uses: for a counted loop, which loop constant holds its count, start and step.
        cf_const: u32,
    },
    /// A write to a ring (`MEM_RING`, 0x26): `burst` consecutive registers from `gpr` go to the dword offset `array_base` of the
    /// ring, only the components in `mask`. The vertex shader that feeds a geometry shader writes its outputs this way, and so does the
    /// geometry shader.
    Ring {
        array_base: u32,
        gpr: u32,
        burst: u32,
        mask: u32,
    },
    /// A geometry shader emitting a vertex (0x15), emitting and cutting the strip (0x16), or cutting it (0x17).
    Emit {
        inst: u32,
    },
    /// Other writes to memory: stream out (0x20 to 0x23), scratch, reduction; not translated.
    Memory {
        inst: u32,
    },
    CallFetchShader,
    Return,
    Nop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AluKind {
    Plain,
    PushBefore,
    PopAfter,
    Pop2After,
    Continue,
    Break,
    ElseAfter,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    /// The control flow instructions, with the index they have in the program (jump addresses name these).
    pub cf: Vec<Cf>,
    /// Whether the control flow instruction ends the program (the last one).
    pub ends: Vec<bool>,
}

fn operand(sel: u32, rel: u32, chan: u32, neg: u32, abs: bool) -> Operand {
    Operand {
        source: source(sel),
        relative: rel != 0,
        channel: chan,
        negate: neg != 0,
        absolute: abs,
    }
}

fn decode_alu_clause(program: &[u8], address: usize, count: usize) -> Result<Vec<Group>, Error> {
    let mut at = address * 2;
    let end = at + count * 2;
    let mut groups = Vec::new();
    while at < end {
        let mut insts: Vec<AluInst> = Vec::new();
        let mut literal_channels = 0u32;
        let mut have_literal = false;
        loop {
            let (a0, a1) = (word(program, at)?, word(program, at + 1)?);
            at += 2;
            let three = (a1 >> 13) & 0x1f >= 8;
            let abs0 = !three && a1 & 1 != 0;
            let abs1 = !three && a1 & 2 != 0;
            let mut src = [
                operand(a0 & 0x1ff, (a0 >> 9) & 1, (a0 >> 10) & 3, (a0 >> 12) & 1, abs0),
                operand((a0 >> 13) & 0x1ff, (a0 >> 22) & 1, (a0 >> 23) & 3, (a0 >> 25) & 1, abs1),
                operand(0, 0, 0, 0, false),
            ];
            if three {
                src[2] = operand(a1 & 0x1ff, (a1 >> 9) & 1, (a1 >> 10) & 3, (a1 >> 12) & 1, false);
            }
            let used = if three { 3 } else { 2 };
            for s in &src[..used] {
                if s.source == Source::Literal {
                    have_literal = true;
                    literal_channels = literal_channels.max(s.channel + 1);
                }
            }
            let last = a0 >> 31 != 0;
            insts.push(AluInst {
                op: if three { (a1 >> 13) & 0x1f } else { (a1 >> 7) & 0x7ff },
                three,
                src,
                dst_gpr: (a1 >> 21) & 0x7f,
                dst_relative: (a1 >> 28) & 1 != 0,
                dst_channel: (a1 >> 29) & 3,
                write: three || (a1 >> 4) & 1 != 0,
                clamp: a1 >> 31 != 0,
                omod: if three { 0 } else { (a1 >> 5) & 3 },
                update_exec_mask: !three && (a1 >> 2) & 1 != 0,
                update_pred: !three && (a1 >> 3) & 1 != 0,
                pred_sel: (a0 >> 29) & 3,
                bank_swizzle: (a1 >> 18) & 7,
                index_mode: (a0 >> 26) & 7,
                last,
            });
            if last {
                break;
            }
            if insts.len() >= 5 {
                return Err(Error::Truncated("an ALU group that does not end within five instructions"));
            }
        }
        let mut literals = [0u32; 4];
        if have_literal {
            let slots = (literal_channels as usize).div_ceil(2);
            for k in 0..slots * 2 {
                let v = word(program, at + k)?;
                if let Some(slot) = literals.get_mut(k) {
                    *slot = v;
                }
            }
            at += slots * 2;
        }
        groups.push(Group { insts, literals });
    }
    if at != end {
        return Err(Error::Truncated("an ALU clause that does not end where its count says"));
    }
    Ok(groups)
}

fn decode_fetch_clause(program: &[u8], address: usize, count: usize, vertex: bool) -> Result<Vec<Fetch>, Error> {
    let mut out = Vec::new();
    for k in 0..count {
        let at = address * 2 + k * 4;
        let (w0, w1, w2) = (word(program, at)?, word(program, at + 1)?, word(program, at + 2)?);
        let inst = w0 & 0x1f;
        // the instruction number says what the fetch is, except that a vertex clause only holds vertex fetches
        let is_vertex = vertex || inst == 0;
        let sel3 = |w: u32, s: u32| [(w >> s) & 7, (w >> (s + 3)) & 7, (w >> (s + 6)) & 7, (w >> (s + 9)) & 7];
        out.push(Fetch {
            inst: if is_vertex { 0 } else { inst },
            resource: (w0 >> 8) & 0xff,
            sampler: (w2 >> 15) & 0x1f,
            src_gpr: (w0 >> 16) & 0x7f,
            src_relative: (w0 >> 23) & 1 != 0,
            src_sel: if is_vertex {
                [(w0 >> 24) & 3, 0, 0, 0]
            } else {
                [(w2 >> 20) & 7, (w2 >> 23) & 7, (w2 >> 26) & 7, (w2 >> 29) & 7]
            },
            dst_gpr: w1 & 0x7f,
            dst_relative: (w1 >> 7) & 1 != 0,
            dst_sel: sel3(w1, 9),
            coord_normalized: [(w1 >> 28) & 1 == 0, (w1 >> 29) & 1 == 0, (w1 >> 30) & 1 == 0, (w1 >> 31) & 1 == 0],
            lod_bias: (w1 >> 21) & 0x7f,
            offset: [w2 & 0x1f, (w2 >> 5) & 0x1f, (w2 >> 10) & 0x1f],
            fetch_type: (w0 >> 5) & 3,
            data_format: (w1 >> 22) & 0x3f,
            num_format: (w1 >> 28) & 3,
            format_comp_signed: (w1 >> 30) & 1 != 0,
            srf_mode: w1 >> 31,
            mega_fetch_count: (w0 >> 26) & 0x3f,
            vtx_offset: w2 & 0xffff,
            endian: (w2 >> 16) & 3,
        });
    }
    Ok(out)
}

/// Reads the control flow of a program and the clauses it names.
pub fn decode(program: &[u8]) -> Result<Decoded, Error> {
    let mut cf = Vec::new();
    let mut ends = Vec::new();
    let mut i = 0;
    loop {
        let (w0, w1) = (word(program, 2 * i)?, word(program, 2 * i + 1)?);
        i += 1;
        let inst = (w1 >> 23) & 0x7f;
        // an ALU clause has no end-of-program bit (bit 21 is part of its count)
        let end = inst < 0x40 && (w1 >> 21) & 1 != 0;
        let node = if inst >= 0x40 {
            let kind = match (w1 >> 26) & 0xf {
                8 => AluKind::Plain,
                9 => AluKind::PushBefore,
                10 => AluKind::PopAfter,
                11 => AluKind::Pop2After,
                13 => AluKind::Continue,
                14 => AluKind::Break,
                15 => AluKind::ElseAfter,
                n => return Err(Error::Unknown("an ALU clause kind", n)),
            };
            let count = ((w1 >> 18) & 0x7f) as usize + 1;
            let kcache = [
                Kcache {
                    bank: (w0 >> 22) & 0xf,
                    mode: w0 >> 30,
                    line: (w1 >> 2) & 0xff,
                },
                Kcache {
                    bank: (w0 >> 26) & 0xf,
                    mode: w1 & 3,
                    line: (w1 >> 10) & 0xff,
                },
            ];
            Cf::Alu {
                kind,
                kcache,
                groups: decode_alu_clause(program, (w0 & 0x3f_ffff) as usize, count)?,
            }
        } else {
            let pop_count = w1 & 7;
            let cond = (w1 >> 8) & 3;
            match inst {
                0 => Cf::Nop,
                1..=3 => {
                    let count = (((w1 >> 10) & 7) | (((w1 >> 19) & 1) << 3)) as usize + 1;
                    Cf::Fetch {
                        vertex: inst != 1,
                        fetches: decode_fetch_clause(program, w0 as usize, count, inst != 1)?,
                    }
                }
                4..=18 => Cf::Flow {
                    inst,
                    addr: w0,
                    pop_count,
                    cond,
                    cf_const: (w1 >> 3) & 0x1f,
                },
                0x13 => Cf::CallFetchShader,
                0x15..=0x17 => Cf::Emit { inst },
                0x26 => Cf::Ring {
                    array_base: w0 & 0x1fff,
                    gpr: (w0 >> 15) & 0x7f,
                    burst: ((w1 >> 17) & 0xf) + 1,
                    mask: (w1 >> 12) & 0xf,
                },
                0x18 | 0x20..=0x25 => Cf::Memory { inst },
                0x14 => Cf::Return,
                0x27 | 0x28 => Cf::Export {
                    kind: (w0 >> 13) & 3,
                    array_base: w0 & 0x1fff,
                    gpr: (w0 >> 15) & 0x7f,
                    swizzle: [w1 & 7, (w1 >> 3) & 7, (w1 >> 6) & 7, (w1 >> 9) & 7],
                    burst: ((w1 >> 17) & 0xf) + 1,
                    inst,
                },
                n => return Err(Error::Unknown("a control flow instruction", n)),
            }
        };
        cf.push(node);
        ends.push(end);
        if end {
            break;
        }
    }
    Ok(Decoded { cf, ends })
}

fn source_text(o: &Operand, literals: &[u32; 4]) -> String {
    let c = ['x', 'y', 'z', 'w'][(o.channel & 3) as usize];
    let base = match o.source {
        Source::Gpr(n) => format!("R{n}.{c}"),
        Source::Kcache { bank, index } => format!("KC{bank}[{index}].{c}"),
        Source::ConstFile(n) => format!("C{n}.{c}"),
        Source::Zero => "0".into(),
        Source::One => "1.0".into(),
        Source::OneInt => "1(int)".into(),
        Source::MinusOneInt => "-1(int)".into(),
        Source::Half => "0.5".into(),
        Source::Literal => format!(
            "L({:#x}={})",
            literals[(o.channel & 3) as usize],
            f32::from_bits(literals[(o.channel & 3) as usize])
        ),
        Source::PreviousVector => format!("PV.{c}"),
        Source::PreviousScalar => "PS".into(),
        Source::Other(n) => format!("sel{n}"),
    };
    let rel = if o.relative { "[AR]" } else { "" };
    let mut t = format!("{base}{rel}");
    if o.absolute {
        t = format!("|{t}|");
    }
    if o.negate {
        t = format!("-{t}");
    }
    t
}

/// A text listing of a decoded program, for reading.
pub fn listing(d: &Decoded) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (n, cf) in d.cf.iter().enumerate() {
        match cf {
            Cf::Alu { kind, kcache, groups } => {
                let _ = writeln!(
                    out,
                    "{n:3} ALU {kind:?} kcache {:?}",
                    kcache.iter().filter(|k| k.mode != 0).collect::<Vec<_>>()
                );
                for g in groups {
                    for i in &g.insts {
                        let name = if i.three { crate::ops::op3(i.op) } else { crate::ops::op2(i.op) }.map_or("?", |t| t.1);
                        let used = if i.three { 3 } else { 2 };
                        let index = ["[AR.x]", "[AR.y]", "[AR.z]", "[AR.w]", "[aL]", "[global]", "[global+AR.x]", "[?]"]
                            [(i.index_mode & 7) as usize];
                        let srcs: Vec<String> = i.src[..used]
                            .iter()
                            .map(|s| source_text(s, &g.literals).replace("[AR]", index))
                            .collect();
                        let _ = writeln!(
                            out,
                            "      {}{} {}R{}{}.{} <- {}{}{}",
                            if i.last { "" } else { "  " },
                            name,
                            if i.write { "" } else { "(nowrite) " },
                            i.dst_gpr,
                            if i.dst_relative { index } else { "" },
                            ['x', 'y', 'z', 'w'][(i.dst_channel & 3) as usize],
                            srcs.join(", "),
                            if i.omod != 0 { format!(" omod{}", i.omod) } else { String::new() },
                            if i.clamp { " clamp" } else { "" },
                        );
                    }
                }
            }
            Cf::Fetch { vertex, fetches } => {
                let _ = writeln!(out, "{n:3} {} clause", if *vertex { "VTX" } else { "TEX" });
                for f in fetches {
                    let _ = writeln!(
                        out,
                        "      fetch inst {:#x} res {} smp {} offset {} mega {} src R{}{:?} -> R{}{:?} fmt {} num {}",
                        f.inst,
                        f.resource,
                        f.sampler,
                        f.vtx_offset,
                        f.mega_fetch_count,
                        f.src_gpr,
                        f.src_sel,
                        f.dst_gpr,
                        f.dst_sel,
                        f.data_format,
                        f.num_format
                    );
                }
            }
            other => {
                let _ = writeln!(out, "{n:3} {other:?}");
            }
        }
    }
    out
}

/// How many dwords of a ring a program writes: the furthest end of its ring writes (a geometry shader's tells how many vertices it emits).
pub fn ring_extent(d: &Decoded) -> u32 {
    d.cf.iter()
        .filter_map(|c| match c {
            Cf::Ring { array_base, burst, .. } => Some(array_base + 4 * burst),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}
