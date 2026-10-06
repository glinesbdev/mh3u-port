//! Shared by the tests of the instruction set: a way to run instructions on QEMU's PowerPC emulator (`harness`), random register states
//! with the edge cases that matter, and the comparison of what two implementations leave behind. The tests that use it are in `tests/`:
//! the interpreter against QEMU, and the translated C against the interpreter.

pub mod harness;

use crate::harness::*;

use mh3u_ppc::interp::{Cpu, FlatMemory, Stop};

use mh3u_ppc::{Insn, Op};

pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: u32) -> u32 {
        (self.next_u64() % u64::from(n.max(1))) as u32
    }
    pub fn bit(&mut self) -> bool {
        self.next_u64() & 1 != 0
    }
    /// A 32-bit value: often one of the interesting ones.
    pub fn word(&mut self) -> u32 {
        const SPECIAL: [u32; 17] = [
            0,
            1,
            2,
            3,
            0xffff_ffff,
            0x7fff_ffff,
            0x8000_0000,
            0x8000_0001,
            0xffff,
            0x8000,
            0xffff_8000,
            0x1_0000,
            31,
            32,
            33,
            63,
            64,
        ];
        if self.below(10) < 4 {
            SPECIAL[self.below(SPECIAL.len() as u32) as usize]
        } else {
            self.next_u64() as u32
        }
    }
    /// A double, often an interesting one.
    pub fn double(&mut self) -> u64 {
        const SPECIAL: [f64; 14] = [
            0.0,
            -0.0,
            1.0,
            -1.0,
            0.5,
            2.0,
            3.5,
            -123.456,
            1e10,
            1e-10,
            1e300,
            -1e-300,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];
        match self.below(10) {
            0..=4 => SPECIAL[self.below(SPECIAL.len() as u32) as usize].to_bits(),
            5 => 0x7ff8_0000_0000_0000,                                     // a quiet NaN
            6 => f64::from_bits(1 + u64::from(self.below(1000))).to_bits(), // a denormal
            _ => self.next_u64(),
        }
    }
    /// A double that is exactly a single (what a single-precision operation is defined for).
    pub fn single(&mut self) -> u64 {
        const SPECIAL: [f32; 12] = [
            0.0,
            -0.0,
            1.0,
            -1.0,
            0.5,
            2.0,
            std::f32::consts::PI,
            -123.456,
            1e10,
            1e-10,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        let v = match self.below(10) {
            0..=5 => SPECIAL[self.below(SPECIAL.len() as u32) as usize],
            6 => f32::from_bits(1 + self.below(1000)),
            7 => f32::MAX,
            _ => {
                let f = f32::from_bits(self.next_u64() as u32);
                if f.is_nan() { 1.5 } else { f }
            }
        };
        f64::from(v).to_bits()
    }
}

/// Find the opcode and extended opcode that decode to `op`.
pub fn template(op: Op) -> u32 {
    for opcode in 0..64u32 {
        for ext in 0..1024u32 {
            let w = opcode << 26 | ext << 1;
            if Insn::decode(w).is_some_and(|i| i.op == op) {
                return w;
            }
        }
    }
    panic!("no word decodes to {op:?}");
}

use Op::*;

pub fn has_rc(op: Op) -> bool {
    matches!(
        op,
        Add | Addc
            | Adde
            | Addze
            | Addme
            | Subf
            | Subfc
            | Subfe
            | Subfze
            | Subfme
            | Neg
            | Mullw
            | Mulhw
            | Mulhwu
            | Divw
            | Divwu
            | Extsb
            | Extsh
            | Cntlzw
            | And
            | Or
            | Xor
            | Nand
            | Nor
            | Eqv
            | Andc
            | Orc
            | Slw
            | Srw
            | Sraw
            | Srawi
            | Rlwimi
            | Rlwinm
            | Rlwnm
    )
}

pub fn has_oe(op: Op) -> bool {
    matches!(
        op,
        Add | Addc | Adde | Addze | Addme | Subf | Subfc | Subfe | Subfze | Subfme | Neg | Mullw | Divw | Divwu
    )
}

pub fn is_memory(op: Op) -> bool {
    matches!(
        op.class(),
        mh3u_ppc::Class::Load | mh3u_ppc::Class::Store | mh3u_ppc::Class::FloatMem
    )
}

pub fn is_indexed(op: Op) -> bool {
    matches!(
        op,
        Lbzx | Lbzux
            | Lhzx
            | Lhzux
            | Lhax
            | Lhaux
            | Lwzx
            | Lwzux
            | Lhbrx
            | Lwbrx
            | Lwarx
            | Stbx
            | Stbux
            | Sthx
            | Sthux
            | Stwx
            | Stwux
            | Sthbrx
            | Stwbrx
            | Stwcx
            | Lfsx
            | Lfsux
            | Lfdx
            | Lfdux
            | Stfsx
            | Stfsux
            | Stfdx
            | Stfdux
            | Stfiwx
            | Lswx
            | Stswx
            | Dcbz
            | Dcbf
            | Dcbst
            | Dcbt
            | Dcbtst
            | Dcbi
            | Icbi
    )
}

pub fn is_update(op: Op) -> bool {
    matches!(
        op,
        Lbzu | Lbzux
            | Lhzu
            | Lhzux
            | Lhau
            | Lhaux
            | Lwzu
            | Lwzux
            | Stbu
            | Stbux
            | Sthu
            | Sthux
            | Stwu
            | Stwux
            | Lfsu
            | Lfsux
            | Lfdu
            | Lfdux
            | Stfsu
            | Stfsux
            | Stfdu
            | Stfdux
    )
}

pub fn is_single_float(op: Op) -> bool {
    matches!(
        op,
        Fadds | Fsubs | Fmuls | Fdivs | Fmadds | Fmsubs | Fnmadds | Fnmsubs | Fsqrts | Fres
    )
}

pub fn is_float_op(op: Op) -> bool {
    op.class() == mh3u_ppc::Class::Float
}

/// Which of the operations are not run against QEMU, and why.
pub fn skipped(op: Op) -> Option<&'static str> {
    Some(match op {
        Only64 => "64-bit instructions are not on this processor",
        Mfmsr | Mtmsr | Mfsr | Mtsr | Mfsrin | Mtsrin | Rfi | Tlbie | Tlbia | Tlbsync | Dcbi => "privileged",
        Eciwx | Ecowx => "external control is not modelled",
        Sc | Tw | Twi => "traps are checked in the unit tests, since they stop the program",
        Mftb => "the time base changes by itself",
        Mffs | Mtfsf | Mtfsfi | Mtfsb0 | Mtfsb1 | Mcrfs => "the floating point status register is not modelled",
        Lmw => "it overwrites r30 and r31, which the harness needs",
        DcbzL => "it is a paired-single-mode instruction",
        Fres | Frsqrte => "the hardware gives an estimate, not the exact reciprocal (tested for closeness)",
        Fsqrt | Fsqrts => "not on the 750 (illegal there)",
        _ if op.class() == mh3u_ppc::Class::Paired => "QEMU has no paired singles",
        _ => return None,
    })
}

pub struct Case {
    pub word: Vec<u32>,
    pub st: St,
}

/// A random word of `op` with valid fields, and a random state it works in, for case number `i` of the run.
pub fn build(op: Op, tpl: u32, rng: &mut Rng, i: usize) -> Case {
    let mut w = tpl;
    let reg = |rng: &mut Rng| rng.below(30);
    let (mut rd, mut ra, mut rb) = (reg(rng), reg(rng), reg(rng));
    let float = is_float_op(op) || matches!(op.class(), mh3u_ppc::Class::FloatMem);
    if is_memory(op) || matches!(op, Dcbz | Dcbf | Dcbst | Dcbt | Dcbtst | Icbi | Lswi | Lswx | Stswi | Stswx) {
        ra = 1 + rng.below(28);
        if is_indexed(op) {
            while rb == ra {
                rb = reg(rng);
            }
        }
        if is_update(op) && matches!(op.class(), mh3u_ppc::Class::Load) {
            while rd == ra {
                rd = reg(rng);
            }
        }
        if matches!(op, Lswi | Lswx) {
            rd = rng.below(20);
            ra = 27;
            rb = 28;
        }
        if matches!(op, Stswi | Stswx) {
            rd = rng.below(20);
            ra = 27;
            rb = 28;
        }
        if matches!(op, Stmw) {
            rd = 4 + rng.below(20);
        }
    }
    let imm = (rng.below(65) as i32 - 32) as u32 & 0xffff;
    let mut st = St::default();
    for r in 0..30 {
        st.gpr[r] = rng.word();
    }
    for f in 0..32 {
        st.fpr[f] = if is_single_float(op) || matches!(op, Stfs | Stfsu | Stfsx | Stfsux) {
            rng.single()
        } else {
            rng.double()
        };
    }
    st.cr = rng.next_u64() as u32;
    st.xer = (rng.next_u64() as u32) & 0xe000_0000;
    st.lr = rng.word() & !3;
    st.ctr = rng.word();
    for b in st.scratch.iter_mut() {
        *b = rng.next_u64() as u8;
    }
    let opcode = tpl >> 26;
    match opcode {
        31 | 63 | 59 | 19 | 4 => {
            w |= rd << 21 | ra << 16 | rb << 11;
            if matches!(opcode, 59 | 63) && !matches!(op, Fcmpu | Fcmpo | Fmr | Fneg | Fabs | Fnabs | Frsp | Fctiw | Fctiwz) {
                // the A-form float operations: which of the four registers they use
                let uses_c = matches!(
                    op,
                    Fmul | Fmuls | Fmadd | Fmadds | Fmsub | Fmsubs | Fnmadd | Fnmadds | Fnmsub | Fnmsubs | Fsel
                );
                let uses_a = !matches!(op, Fres | Frsqrte | Fsqrt | Fsqrts);
                let uses_b = !matches!(op, Fmul | Fmuls);
                w &= !(0x1f << 16 | 0x1f << 11);
                if uses_a {
                    w |= ra << 16;
                }
                if uses_b {
                    w |= rb << 11;
                }
                if uses_c {
                    w |= rng.below(30) << 6;
                }
            }
            if matches!(
                op,
                Neg | Addze
                    | Subfze
                    | Addme
                    | Subfme
                    | Extsb
                    | Extsh
                    | Cntlzw
                    | Fres
                    | Frsqrte
                    | Frsp
                    | Fctiw
                    | Fctiwz
                    | Fmr
                    | Fneg
                    | Fabs
                    | Fnabs
                    | Fsqrt
                    | Fsqrts
            ) {
                w &= !(0x1f << 11);
                if matches!(
                    op,
                    Fres | Frsqrte | Frsp | Fctiw | Fctiwz | Fmr | Fneg | Fabs | Fnabs | Fsqrt | Fsqrts
                ) {
                    w |= rb << 11;
                    w &= !(0x1f << 16);
                }
            }
            if has_oe(op) && rng.bit() {
                w |= 1 << 10;
            }
            if has_rc(op) && rng.bit() {
                w |= 1;
            }
            match op {
                Cmp | Cmpl | Fcmpu | Fcmpo => {
                    w &= !(0x1f << 21 | 0x1f << 16 | 0x1f << 11);
                    w |= rng.below(8) << 23 | ra << 16 | rb << 11;
                }
                Mtcrf => {
                    w &= !(0x1f << 16 | 0x1f << 11);
                    w |= rng.below(256) << 12;
                }
                Mfcr | Mcrxr => {
                    w &= !(0x1f << 11 | 0x1f << 16);
                    if op == Mcrxr {
                        w &= !(0x1f << 21);
                        w |= rng.below(8) << 23;
                    }
                }
                Mcrf => {
                    w &= !(0x1f << 11 | 0x1f << 16 | 0x1f << 21);
                    w |= rng.below(8) << 23 | rng.below(8) << 18;
                }
                Mftb => {
                    let spr = [268u32, 269][rng.below(2) as usize];
                    w &= !(0x3ff << 11);
                    w |= (((spr & 31) << 5) | (spr >> 5)) << 11;
                }
                Mfspr | Mtspr => {
                    let spr = [1u32, 8, 9][rng.below(3) as usize];
                    w &= !(0x3ff << 11);
                    w |= (((spr & 31) << 5) | (spr >> 5)) << 11;
                }
                Srawi => {}
                Stwcx => w |= 1,
                Sync | Eieio | Isync => w = tpl,
                Dcbz | Dcbf | Dcbst | Dcbt | Dcbtst | Icbi => w &= !(0x1f << 21),
                _ => {}
            }
            if matches!(op, Fres | Frsqrte) {
                // the harness compares closeness for these elsewhere
            }
            if matches!(op, Lswi | Stswi) {
                w &= !(0x1f << 11);
                w |= ((1 + rng.below(32)) % 32) << 11;
            }
        }
        20 | 21 | 23 => {
            w |= rd << 21 | ra << 16 | rng.below(32) << 11 | rng.below(32) << 6 | rng.below(32) << 1;
            if rng.bit() {
                w |= 1;
            }
        }
        10 | 11 => w |= rng.below(8) << 23 | ra << 16 | if opcode == 10 { rng.next_u64() as u32 & 0xffff } else { imm },
        _ => {
            w |= rd << 21 | ra << 16;
            w |= if is_memory(op) || matches!(op, Stmw) || opcode >= 32 {
                imm
            } else {
                rng.next_u64() as u32 & 0xffff
            };
        }
    }
    // state constraints
    let scratch = scratch_addr(i);
    let mem_op = is_memory(op) || matches!(op, Dcbz | Dcbf | Dcbst | Dcbt | Dcbtst | Icbi | Lswi | Lswx | Stswi | Stswx | Stmw);
    if mem_op {
        let ra_i = ((w >> 16) & 31) as usize;
        let rb_i = ((w >> 11) & 31) as usize;
        let base = scratch + if matches!(op, Stmw) { 8 } else { 96 };
        if is_indexed(op) {
            st.gpr[ra_i] = base & !31;
            st.gpr[rb_i] = rng.below(40);
            if matches!(
                op,
                Lwarx | Stwcx | Lwbrx | Stwbrx | Lwzx | Lwzux | Stwx | Stwux | Lfsx | Lfsux | Stfsx | Stfsux | Stfiwx
            ) {
                st.gpr[rb_i] &= !3;
            }
        } else if matches!(op, Lswi | Stswi) {
            st.gpr[ra_i] = base;
        } else {
            let disp = i32::from(w as i16);
            st.gpr[ra_i] = (base as i32).wrapping_sub(disp) as u32;
            if matches!(op, Stmw) {
                st.gpr[ra_i] = base.wrapping_sub(disp as u32);
            }
        }
        if matches!(op, Lswx | Stswx) {
            st.xer = (st.xer & !0x7f) | rng.below(33);
        }
    }
    if matches!(op, Divw | Divwu) {
        let rb_i = ((w >> 11) & 31) as usize;
        let ra_i = ((w >> 16) & 31) as usize;
        if st.gpr[rb_i] == 0 {
            st.gpr[rb_i] = 1 + rng.below(100);
        }
        if op == Divw && st.gpr[ra_i] == 0x8000_0000 && st.gpr[rb_i] == 0xffff_ffff {
            st.gpr[rb_i] = 2;
        }
        // a register that is both operands holds a value that is not zero
        if ra_i == rb_i && st.gpr[ra_i] == 0 {
            st.gpr[ra_i] = 3;
        }
    }
    let _ = float;
    Case { word: vec![w], st }
}

/// Run a case on the interpreter.
pub fn interpret(words: &[u32], st: &St, i: usize) -> Result<(St, usize), Stop> {
    let mut cpu = Cpu {
        gpr: st.gpr,
        fpr: st.fpr,
        cr: st.cr,
        xer: st.xer,
        lr: st.lr,
        ctr: st.ctr,
        pc: test_address(),
        ..Cpu::default()
    };
    // the harness's own registers
    cpu.gpr[30] = 0;
    cpu.gpr[31] = IN_BASE + (i * STATE) as u32;
    let mut mem = FlatMemory::new(scratch_addr(i), SCRATCH_LEN);
    mem.data.copy_from_slice(&st.scratch);
    let end = test_address() + 4 * words.len() as u32;
    let mut steps = 0;
    while cpu.pc >= test_address() && cpu.pc < end && steps < 64 {
        let word = words[((cpu.pc - test_address()) / 4) as usize];
        let insn = Insn::decode(word).expect("a word of the test decodes");
        cpu.execute(&mut mem, insn)?;
        steps += 1;
    }
    let mut out = St {
        gpr: cpu.gpr,
        fpr: cpu.fpr,
        cr: cpu.cr,
        xer: cpu.xer,
        lr: cpu.lr,
        ctr: cpu.ctr,
        ..St::default()
    };
    out.scratch.copy_from_slice(&mem.data);
    Ok((out, mem.outside))
}

/// What differs between two states, ignoring what the harness uses.
pub fn differences(op: Op, qemu: &St, ours: &St) -> Vec<String> {
    let mut d = Vec::new();
    for r in 0..30 {
        if qemu.gpr[r] != ours.gpr[r] {
            d.push(format!("r{r}: qemu {:#010x}, interpreter {:#010x}", qemu.gpr[r], ours.gpr[r]));
        }
    }
    for f in 0..32 {
        let (a, b) = (qemu.fpr[f], ours.fpr[f]);
        let same = if matches!(op, Fctiw | Fctiwz) {
            a as u32 == b as u32
        } else {
            a == b
        };
        if !same {
            d.push(format!("f{f}: qemu {a:#018x}, interpreter {b:#018x}"));
        }
    }
    if qemu.cr != ours.cr {
        d.push(format!("cr: qemu {:#010x}, interpreter {:#010x}", qemu.cr, ours.cr));
    }
    if qemu.xer & 0xe000_007f != ours.xer & 0xe000_007f {
        d.push(format!("xer: qemu {:#010x}, interpreter {:#010x}", qemu.xer, ours.xer));
    }
    if qemu.lr != ours.lr {
        d.push(format!("lr: qemu {:#010x}, interpreter {:#010x}", qemu.lr, ours.lr));
    }
    if qemu.ctr != ours.ctr {
        d.push(format!("ctr: qemu {:#010x}, interpreter {:#010x}", qemu.ctr, ours.ctr));
    }
    if qemu.scratch != ours.scratch {
        let at = qemu.scratch.iter().zip(&ours.scratch).position(|(a, b)| a != b).unwrap_or(0);
        d.push(format!(
            "memory at +{at}: qemu {:02x?}, interpreter {:02x?}",
            &qemu.scratch[at..at + 8.min(256 - at)],
            &ours.scratch[at..at + 8.min(256 - at)]
        ));
    }
    d
}

pub const VARIANTS: usize = 8;

pub const CASES: usize = 24;

pub fn skipped_reason(op: Op) -> Option<&'static str> {
    skipped(op)
}

/// The state constraints that depend on the registers a word names (the word was made with another state).
pub fn fix_state_for_word(op: Op, word: u32, st: &mut St, i: usize, rng: &mut Rng) {
    let ra = ((word >> 16) & 31) as usize;
    let rb = ((word >> 11) & 31) as usize;
    let scratch = scratch_addr(i);
    let mem_op = is_memory(op) || matches!(op, Dcbz | Dcbf | Dcbst | Dcbt | Dcbtst | Icbi | Lswi | Lswx | Stswi | Stswx | Stmw);
    if mem_op {
        let base = scratch + if matches!(op, Stmw) { 8 } else { 96 };
        if is_indexed(op) {
            st.gpr[ra] = base & !31;
            st.gpr[rb] = rng.below(40);
            if matches!(
                op,
                Lwarx | Stwcx | Lwbrx | Stwbrx | Lwzx | Lwzux | Stwx | Stwux | Lfsx | Lfsux | Stfsx | Stfsux | Stfiwx
            ) {
                st.gpr[rb] &= !3;
            }
        } else if matches!(op, Lswi | Stswi) {
            st.gpr[ra] = base;
        } else {
            st.gpr[ra] = (base as i32).wrapping_sub(i32::from(word as i16)) as u32;
        }
        if matches!(op, Lswx | Stswx) {
            st.xer = (st.xer & !0x7f) | rng.below(33);
        }
    }
    if matches!(op, Divw | Divwu) {
        if st.gpr[rb] == 0 {
            st.gpr[rb] = 1 + rng.below(100);
        }
        if op == Divw && st.gpr[ra] == 0x8000_0000 && st.gpr[rb] == 0xffff_ffff {
            st.gpr[rb] = 2;
        }
        if ra == rb && st.gpr[ra] == 0 {
            st.gpr[ra] = 3;
        }
        if ra == rb && op == Divw && st.gpr[ra] == 0x8000_0000 {
            st.gpr[ra] = 3;
        }
    }
}

// ---- sequences: branches, and the reservation pair

/// `addi rd, rd, 1`: the marker a branch skips or does not.
pub fn marker(rd: u32) -> u32 {
    0x3800_0001 | rd << 21 | rd << 16
}

pub fn run_sequence(name: &str, words: &[u32], states: &[St], note: &dyn Fn(&St) -> String) -> Vec<String> {
    let run = run_qemu(words, states, name).expect("qemu ran");
    let mut failures = Vec::new();
    for (i, st) in states.iter().enumerate() {
        match interpret(words, st, i) {
            Ok((ours, outside)) => {
                let mut d = differences(Add, &run.after[i], &ours);
                if outside > 0 {
                    d.push(format!("the interpreter touched memory outside the scratch area {outside} times"));
                }
                if !d.is_empty() {
                    failures.push(format!("{name} {words:08x?} case {i}: {}\n    start: {}", d.join("; "), note(st)));
                    break;
                }
            }
            Err(stop) => {
                failures.push(format!("{name} {words:08x?}: interpreter stopped with {stop:?}"));
                break;
            }
        }
    }
    failures
}
