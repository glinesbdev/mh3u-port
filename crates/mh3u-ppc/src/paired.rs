//! The paired singles: a Gekko/Broadway/Espresso extension in which a floating point register holds two single-precision values (`ps0`,
//! the register as the ordinary instructions see it, and `ps1`), and instructions work on both at once. `psq_l` and `psq_st` load and
//! store the pair, optionally as small integers scaled by a power of two (the "quantization" the GQR registers describe).
//!
//! QEMU has none of this, so there is no second implementation to compare with: the meaning is from the architecture manual and the
//! tests below are values worked out by hand from it. (The translated C is compared with this, which catches slips in either.)

use crate::insn::{Insn, Op};
use crate::interp::{Cpu, Memory, Stop};

/// A quantization register's load or store half: the type and the scale (a signed six-bit power of two).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quant {
    pub kind: u32,
    pub scale: i32,
}

impl Quant {
    /// From a GQR for loading: the type is in bits 16 to 18, the scale in bits 24 to 29.
    pub fn load(gqr: u32) -> Quant {
        Quant {
            kind: (gqr >> 16) & 7,
            scale: ((((gqr >> 24) & 0x3f) << 26) as i32) >> 26,
        }
    }

    /// For storing: the type is in bits 0 to 2, the scale in bits 8 to 13.
    pub fn store(gqr: u32) -> Quant {
        Quant {
            kind: gqr & 7,
            scale: ((((gqr >> 8) & 0x3f) << 26) as i32) >> 26,
        }
    }

    /// How many bytes one value takes.
    pub fn size(self) -> u32 {
        match self.kind {
            4 | 6 => 1,
            5 | 7 => 2,
            _ => 4,
        }
    }
}

fn pow2(n: i32) -> f32 {
    2f32.powi(n)
}

/// A NaN made by an invalid operation is the positive quiet NaN; an operand NaN is passed on, quiet, the first one winning.
fn canonical32(result: f32, inputs: &[f32]) -> f32 {
    if result.is_nan() {
        if let Some(n) = inputs.iter().find(|x| x.is_nan()) {
            return f32::from_bits(n.to_bits() | 0x0040_0000);
        }
        return f32::from_bits(0x7fc0_0000);
    }
    result
}

fn s(bits: u64) -> f32 {
    f64::from_bits(bits) as f32
}

fn d(v: f32) -> u64 {
    f64::from(v).to_bits()
}

impl Cpu {
    fn half(&self, reg: usize, i: usize) -> f32 {
        s(if i == 0 { self.fpr[reg] } else { self.ps1[reg] })
    }

    fn set_half(&mut self, reg: usize, i: usize, v: f32) {
        if i == 0 {
            self.fpr[reg] = d(v);
        } else {
            self.ps1[reg] = d(v);
        }
    }

    /// Load the pair (or just the first value, the second being 1.0) the way a quantization says.
    fn quantized_load<M: Memory>(&mut self, mem: &mut M, ea: u32, w: bool, gqr: usize) -> (u64, u64) {
        let q = Quant::load(self.gqr[gqr]);
        let read = |mem: &mut M, at: u32| -> u64 {
            match q.kind {
                4 => d(f32::from(mem.read8(at)) * pow2(-q.scale)),
                5 => d(f32::from(mem.read16(at)) * pow2(-q.scale)),
                6 => d(f32::from(mem.read8(at) as i8) * pow2(-q.scale)),
                7 => d(f32::from(mem.read16(at) as i16) * pow2(-q.scale)),
                _ => {
                    let bits = mem.read32(at);
                    let single = f32::from_bits(bits);
                    if single.is_nan() {
                        // the fields are moved, nothing is made quiet
                        (u64::from(bits >> 31) << 63) | 0x7ff0_0000_0000_0000 | (u64::from(bits & 0x007f_ffff) << 29)
                    } else {
                        f64::from(single).to_bits()
                    }
                }
            }
        };
        let first = read(mem, ea);
        let second = if w {
            1.0f64.to_bits()
        } else {
            read(mem, ea.wrapping_add(q.size()))
        };
        (first, second)
    }

    /// Store the pair (or just the first value) the way a quantization says.
    fn quantized_store<M: Memory>(&mut self, mem: &mut M, ea: u32, w: bool, gqr: usize, first: u64, second: u64) {
        let q = Quant::store(self.gqr[gqr]);
        let put = |mem: &mut M, at: u32, bits: u64| {
            let x = f64::from_bits(bits) as f32;
            let scaled = x * pow2(q.scale);
            // to an integer by truncation, a NaN as the smallest number (as the hardware's conversion gives), then held to the range
            let int = if scaled.is_nan() { i32::MIN } else { scaled as i32 };
            match q.kind {
                4 => mem.write8(at, int.clamp(0, 255) as u8),
                5 => mem.write16(at, int.clamp(0, 65535) as u16),
                6 => mem.write8(at, int.clamp(-128, 127) as i8 as u8),
                7 => mem.write16(at, int.clamp(-32768, 32767) as i16 as u16),
                _ => {
                    let single = if f64::from_bits(bits).is_nan() {
                        ((bits >> 63) as u32) << 31 | 0x7f80_0000 | ((bits >> 29) as u32 & 0x007f_ffff)
                    } else {
                        (f64::from_bits(bits) as f32).to_bits()
                    };
                    mem.write32(at, single);
                }
            }
        };
        put(mem, ea, first);
        if !w {
            put(mem, ea.wrapping_add(q.size()), second);
        }
    }

    /// The paired-single instructions. `pc` has already been moved on.
    pub(crate) fn paired<M: Memory>(&mut self, mem: &mut M, insn: Insn) -> Result<(), Stop> {
        use Op::*;
        let (rd, ra, rb, rc) = (insn.rd(), insn.ra(), insn.rb(), insn.rc_f());
        match insn.op {
            PsAdd | PsSub | PsMul | PsDiv | PsMadd | PsMsub | PsNmadd | PsNmsub => {
                for i in 0..2 {
                    let (a, b, c) = (self.half(ra, i), self.half(rb, i), self.half(rc, i));
                    let (r, used): (f32, Vec<f32>) = match insn.op {
                        PsAdd => (a + b, vec![a, b]),
                        PsSub => (a - b, vec![a, b]),
                        PsMul => (a * c, vec![a, c]),
                        PsDiv => (a / b, vec![a, b]),
                        PsMadd | PsNmadd => (a.mul_add(c, b), vec![a, c, b]),
                        _ => (a.mul_add(c, -b), vec![a, c, b]),
                    };
                    let r = canonical32(r, &used);
                    let r = if matches!(insn.op, PsNmadd | PsNmsub) && !r.is_nan() {
                        -r
                    } else {
                        r
                    };
                    self.set_half(rd, i, r);
                }
            }
            PsMuls0 | PsMuls1 | PsMadds0 | PsMadds1 => {
                let which = usize::from(matches!(insn.op, PsMuls1 | PsMadds1));
                let c = self.half(rc, which);
                let (a0, a1) = (self.half(ra, 0), self.half(ra, 1));
                let (b0, b1) = (self.half(rb, 0), self.half(rb, 1));
                for (i, (a, b)) in [(a0, b0), (a1, b1)].into_iter().enumerate() {
                    let (r, used): (f32, Vec<f32>) = if matches!(insn.op, PsMuls0 | PsMuls1) {
                        (a * c, vec![a, c])
                    } else {
                        (a.mul_add(c, b), vec![a, c, b])
                    };
                    self.set_half(rd, i, canonical32(r, &used));
                }
            }
            PsSum0 | PsSum1 => {
                let (a0, b1, c0, c1) = (self.half(ra, 0), self.half(rb, 1), self.half(rc, 0), self.half(rc, 1));
                let sum = canonical32(a0 + b1, &[a0, b1]);
                if insn.op == PsSum0 {
                    self.set_half(rd, 0, sum);
                    self.set_half(rd, 1, c1);
                } else {
                    self.set_half(rd, 0, c0);
                    self.set_half(rd, 1, sum);
                }
            }
            PsSel => {
                for i in 0..2 {
                    let (a, b, c) = (self.half(ra, i), self.half(rb, i), self.half(rc, i));
                    self.set_half(rd, i, if a >= 0.0 { c } else { b });
                }
            }
            PsRes | PsRsqrte => {
                for i in 0..2 {
                    let b = self.half(rb, i);
                    let r = if insn.op == PsRes { 1.0 / b } else { 1.0 / b.sqrt() };
                    self.set_half(rd, i, canonical32(r, &[b]));
                }
            }
            PsNeg | PsMr | PsNabs | PsAbs => {
                let (b0, b1) = (self.fpr[rb], self.ps1[rb]);
                let f = |bits: u64| match insn.op {
                    PsNeg => bits ^ (1 << 63),
                    PsNabs => bits | (1 << 63),
                    PsAbs => bits & !(1 << 63),
                    _ => bits,
                };
                self.fpr[rd] = f(b0);
                self.ps1[rd] = f(b1);
            }
            PsMerge00 | PsMerge01 | PsMerge10 | PsMerge11 => {
                let (a0, a1, b0, b1) = (self.fpr[ra], self.ps1[ra], self.fpr[rb], self.ps1[rb]);
                let (x, y) = match insn.op {
                    PsMerge00 => (a0, b0),
                    PsMerge01 => (a0, b1),
                    PsMerge10 => (a1, b0),
                    _ => (a1, b1),
                };
                self.fpr[rd] = x;
                self.ps1[rd] = y;
            }
            PsCmpu0 | PsCmpo0 | PsCmpu1 | PsCmpo1 => {
                let i = usize::from(matches!(insn.op, PsCmpu1 | PsCmpo1));
                let (a, b) = (self.half(ra, i), self.half(rb, i));
                let v = if a.is_nan() || b.is_nan() {
                    1
                } else if a < b {
                    8
                } else if a > b {
                    4
                } else {
                    2
                };
                self.set_cr_field(insn.crfd(), v);
            }
            PsqL | PsqLu | PsqLx | PsqLux => {
                let indexed = matches!(insn.op, PsqLx | PsqLux);
                let ea = if indexed {
                    self.ra_or_zero(ra).wrapping_add(self.gpr[rb])
                } else {
                    self.ra_or_zero(ra).wrapping_add(insn.psq_d() as u32)
                };
                let (w, i) = if indexed {
                    (insn.psqx_w(), insn.psqx_i())
                } else {
                    (insn.psq_w(), insn.psq_i())
                };
                let (first, second) = self.quantized_load(mem, ea, w, i);
                self.fpr[rd] = first;
                self.ps1[rd] = second;
                if matches!(insn.op, PsqLu | PsqLux) {
                    self.gpr[ra] = ea;
                }
            }
            PsqSt | PsqStu | PsqStx | PsqStux => {
                let indexed = matches!(insn.op, PsqStx | PsqStux);
                let ea = if indexed {
                    self.ra_or_zero(ra).wrapping_add(self.gpr[rb])
                } else {
                    self.ra_or_zero(ra).wrapping_add(insn.psq_d() as u32)
                };
                let (w, i) = if indexed {
                    (insn.psqx_w(), insn.psqx_i())
                } else {
                    (insn.psq_w(), insn.psq_i())
                };
                let (first, second) = (self.fpr[rd], self.ps1[rd]);
                self.quantized_store(mem, ea, w, i, first, second);
                if matches!(insn.op, PsqStu | PsqStux) {
                    self.gpr[ra] = ea;
                }
            }
            _ => return Err(Stop::Unsupported(insn.word)),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::FlatMemory;

    fn run(cpu: &mut Cpu, mem: &mut FlatMemory, word: u32) {
        cpu.execute(mem, Insn::decode(word).expect("a word that decodes")).expect("it runs");
    }

    /// ps_<op> frD=3, frA=1, frB=2, frC=4 as a word.
    fn a_form(ext: u32, c: u32) -> u32 {
        4 << 26 | 3 << 21 | 1 << 16 | 2 << 11 | c << 6 | ext << 1
    }

    fn pair(cpu: &mut Cpu, reg: usize, x: f32, y: f32) {
        cpu.fpr[reg] = f64::from(x).to_bits();
        cpu.ps1[reg] = f64::from(y).to_bits();
    }

    fn get(cpu: &Cpu, reg: usize) -> (f32, f32) {
        (f64::from_bits(cpu.fpr[reg]) as f32, f64::from_bits(cpu.ps1[reg]) as f32)
    }

    #[test]
    fn arithmetic_works_on_both_halves_and_the_sum_and_scalar_forms_mix_them() {
        let mut cpu = Cpu::default();
        let mut mem = FlatMemory::new(0, 16);
        pair(&mut cpu, 1, 1.5, -2.0);
        pair(&mut cpu, 2, 0.25, 8.0);
        pair(&mut cpu, 4, 3.0, 10.0);
        run(&mut cpu, &mut mem, a_form(21, 0)); // ps_add
        assert_eq!(get(&cpu, 3), (1.75, 6.0));
        run(&mut cpu, &mut mem, a_form(20, 0)); // ps_sub
        assert_eq!(get(&cpu, 3), (1.25, -10.0));
        run(&mut cpu, &mut mem, a_form(25, 4)); // ps_mul A*C
        assert_eq!(get(&cpu, 3), (4.5, -20.0));
        run(&mut cpu, &mut mem, a_form(29, 4)); // ps_madd A*C+B
        assert_eq!(get(&cpu, 3), (4.75, -12.0));
        run(&mut cpu, &mut mem, a_form(28, 4)); // ps_msub A*C-B
        assert_eq!(get(&cpu, 3), (4.25, -28.0));
        run(&mut cpu, &mut mem, a_form(31, 4)); // ps_nmadd -(A*C+B)
        assert_eq!(get(&cpu, 3), (-4.75, 12.0));
        run(&mut cpu, &mut mem, a_form(30, 4)); // ps_nmsub -(A*C-B)
        assert_eq!(get(&cpu, 3), (-4.25, 28.0));
        run(&mut cpu, &mut mem, a_form(10, 4)); // ps_sum0: ps0 = A.ps0 + B.ps1, ps1 = C.ps1
        assert_eq!(get(&cpu, 3), (9.5, 10.0));
        run(&mut cpu, &mut mem, a_form(11, 4)); // ps_sum1: ps0 = C.ps0, ps1 = A.ps0 + B.ps1
        assert_eq!(get(&cpu, 3), (3.0, 9.5));
        run(&mut cpu, &mut mem, a_form(12, 4)); // ps_muls0: A * C.ps0
        assert_eq!(get(&cpu, 3), (4.5, -6.0));
        run(&mut cpu, &mut mem, a_form(13, 4)); // ps_muls1: A * C.ps1
        assert_eq!(get(&cpu, 3), (15.0, -20.0));
        run(&mut cpu, &mut mem, a_form(14, 4)); // ps_madds0: A * C.ps0 + B
        assert_eq!(get(&cpu, 3), (4.75, 2.0));
        run(&mut cpu, &mut mem, a_form(15, 4)); // ps_madds1: A * C.ps1 + B
        assert_eq!(get(&cpu, 3), (15.25, -12.0));
        run(&mut cpu, &mut mem, a_form(23, 4)); // ps_sel: A >= 0 ? C : B
        assert_eq!(get(&cpu, 3), (3.0, 8.0));
    }

    #[test]
    fn merge_and_the_sign_moves_pick_and_change_halves() {
        let mut cpu = Cpu::default();
        let mut mem = FlatMemory::new(0, 16);
        pair(&mut cpu, 1, 1.0, 2.0);
        pair(&mut cpu, 2, 3.0, 4.0);
        // X-form: opcode 4, ext in bits 1..10
        let x = |ext: u32| 4 << 26 | 3 << 21 | 1 << 16 | 2 << 11 | ext << 1;
        run(&mut cpu, &mut mem, x(528)); // merge00
        assert_eq!(get(&cpu, 3), (1.0, 3.0));
        run(&mut cpu, &mut mem, x(560)); // merge01
        assert_eq!(get(&cpu, 3), (1.0, 4.0));
        run(&mut cpu, &mut mem, x(592)); // merge10
        assert_eq!(get(&cpu, 3), (2.0, 3.0));
        run(&mut cpu, &mut mem, x(624)); // merge11
        assert_eq!(get(&cpu, 3), (2.0, 4.0));
        run(&mut cpu, &mut mem, x(40)); // ps_neg of B
        assert_eq!(get(&cpu, 3), (-3.0, -4.0));
        run(&mut cpu, &mut mem, x(264)); // ps_abs of B
        assert_eq!(get(&cpu, 3), (3.0, 4.0));
        run(&mut cpu, &mut mem, x(136)); // ps_nabs of B
        assert_eq!(get(&cpu, 3), (-3.0, -4.0));
        run(&mut cpu, &mut mem, x(72)); // ps_mr of B
        assert_eq!(get(&cpu, 3), (3.0, 4.0));
        // ps_cmpu0 cr2, A, B: 1.0 < 3.0 sets "less"
        run(&mut cpu, &mut mem, 4 << 26 | 2 << 23 | 1 << 16 | 2 << 11);
        assert_eq!(cpu.cr_field_for_test(2), 8);
    }

    #[test]
    fn a_pair_of_floats_loads_and_stores_as_they_are_and_as_one_with_w() {
        let mut cpu = Cpu::default();
        let mut mem = FlatMemory::new(0x1000, 64);
        mem.write32(0x1000, 1.5f32.to_bits());
        mem.write32(0x1004, (-2.25f32).to_bits());
        cpu.gpr[3] = 0x1000;
        // psq_l f1, 0(r3), W=0, I=0 (GQR0 is zero: float)
        run(&mut cpu, &mut mem, 56 << 26 | 1 << 21 | 3 << 16);
        assert_eq!(get(&cpu, 1), (1.5, -2.25));
        // W=1 loads one value and gives 1.0 for the second
        run(&mut cpu, &mut mem, 56 << 26 | 1 << 21 | 3 << 16 | 1 << 15);
        assert_eq!(get(&cpu, 1), (1.5, 1.0));
        // psq_st f1, 8(r3), W=0
        pair(&mut cpu, 1, 7.0, 8.5);
        run(&mut cpu, &mut mem, 60 << 26 | 1 << 21 | 3 << 16 | 8);
        assert_eq!((mem.read32(0x1008), mem.read32(0x100c)), (7.0f32.to_bits(), 8.5f32.to_bits()));
    }

    #[test]
    fn integer_types_are_scaled_by_a_power_of_two_and_clamped_when_stored() {
        let mut cpu = Cpu::default();
        let mut mem = FlatMemory::new(0x1000, 64);
        cpu.gpr[3] = 0x1000;
        // GQR2: load and store u8, scale 0; GQR3: s16 with a scale of 4
        cpu.gqr[2] = 0x0004_0004;
        let gqr3 = (4u32 << 24) | (7 << 16) | (4 << 8) | 7;
        cpu.gqr[3] = gqr3;
        mem.write8(0x1000, 200);
        mem.write8(0x1001, 7);
        // psq_l f1, 0(r3), W=0, I=2: two bytes become 200.0 and 7.0
        run(&mut cpu, &mut mem, 56 << 26 | 1 << 21 | 3 << 16 | 2 << 12);
        assert_eq!(get(&cpu, 1), (200.0, 7.0));
        // psq_st f1 with I=2 stores them back, clamped: 300.0 -> 255, -5.0 -> 0
        pair(&mut cpu, 1, 300.0, -5.0);
        run(&mut cpu, &mut mem, 60 << 26 | 1 << 21 | 3 << 16 | 2 << 12 | 16);
        assert_eq!((mem.read8(0x1010), mem.read8(0x1011)), (255, 0));
        // s16 with scale 4 when loading divides by 2^4: 0x0140 (= 320) -> 20.0; when storing multiplies: 1.5 -> 24
        mem.write16(0x1020, 0x0140);
        mem.write16(0x1022, 0xfff0); // -16 -> -1.0
        run(&mut cpu, &mut mem, 56 << 26 | 1 << 21 | 3 << 16 | 3 << 12 | 0x20);
        assert_eq!(get(&cpu, 1), (20.0, -1.0));
        pair(&mut cpu, 1, 1.5, -1.0);
        run(&mut cpu, &mut mem, 60 << 26 | 1 << 21 | 3 << 16 | 3 << 12 | 0x30);
        assert_eq!((mem.read16(0x1030), mem.read16(0x1032)), (24, 0xfff0));
    }

    #[test]
    fn the_update_and_indexed_forms_move_the_base_and_take_w_and_i_from_their_own_fields() {
        let mut cpu = Cpu::default();
        let mut mem = FlatMemory::new(0x1000, 64);
        mem.write32(0x1008, 2.0f32.to_bits());
        mem.write32(0x100c, 3.0f32.to_bits());
        cpu.gpr[3] = 0x1000;
        cpu.gpr[4] = 8;
        // psq_lux f1, r3, r4 (opcode 4, ext 38), W=0, I=0
        run(&mut cpu, &mut mem, 4 << 26 | 1 << 21 | 3 << 16 | 4 << 11 | 38 << 1);
        assert_eq!(get(&cpu, 1), (2.0, 3.0));
        assert_eq!(cpu.gpr[3], 0x1008, "the base is updated");
        // psq_lu with a displacement
        cpu.gpr[3] = 0x1000;
        run(&mut cpu, &mut mem, 57 << 26 | 1 << 21 | 3 << 16 | 8);
        assert_eq!((get(&cpu, 1), cpu.gpr[3]), ((2.0, 3.0), 0x1008));
    }
}
