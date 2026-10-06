//! A reference interpreter for the Wii U's PowerPC (one core, no interrupts, no address translation). It is the *meaning* of each
//! instruction in plain Rust, small enough to read against the architecture book, and is what the translator's C is tested against.
//! (Both are also tested against QEMU's PowerPC emulator, an independent implementation; see `tests/qemu.rs`.)
//!
//! Not modelled: the floating point status register's exception bits (the game's code never reads it: see `docs/spikes.md`), the
//! hardware's estimate tables for `fres` and `frsqrte` (the exact reciprocal is used: within the hardware's stated error), and paired
//! singles, which are in `paired.rs`.

use crate::insn::{Insn, Op};

/// What an instruction stopped the program for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// `sc`.
    SystemCall,
    /// A trap instruction whose condition held.
    Trap(u32),
    /// An instruction this does not model (privileged, 64-bit, external control).
    Unsupported(u32),
    /// A move to or from a special register this does not know.
    UnknownSpr(u32),
}

/// Guest memory, big-endian.
pub trait Memory {
    fn read8(&mut self, addr: u32) -> u8;
    fn write8(&mut self, addr: u32, v: u8);
    fn read16(&mut self, addr: u32) -> u16 {
        u16::from_be_bytes([self.read8(addr), self.read8(addr.wrapping_add(1))])
    }
    fn read32(&mut self, addr: u32) -> u32 {
        u32::from(self.read16(addr)) << 16 | u32::from(self.read16(addr.wrapping_add(2)))
    }
    fn read64(&mut self, addr: u32) -> u64 {
        u64::from(self.read32(addr)) << 32 | u64::from(self.read32(addr.wrapping_add(4)))
    }
    fn write16(&mut self, addr: u32, v: u16) {
        let b = v.to_be_bytes();
        self.write8(addr, b[0]);
        self.write8(addr.wrapping_add(1), b[1]);
    }
    fn write32(&mut self, addr: u32, v: u32) {
        self.write16(addr, (v >> 16) as u16);
        self.write16(addr.wrapping_add(2), v as u16);
    }
    fn write64(&mut self, addr: u32, v: u64) {
        self.write32(addr, (v >> 32) as u32);
        self.write32(addr.wrapping_add(4), v as u32);
    }
}

/// A flat piece of memory at a base address; reads outside it give zero and writes outside it are dropped (and counted).
#[derive(Debug, Clone)]
pub struct FlatMemory {
    pub base: u32,
    pub data: Vec<u8>,
    pub outside: usize,
}

impl FlatMemory {
    pub fn new(base: u32, len: usize) -> FlatMemory {
        FlatMemory {
            base,
            data: vec![0; len],
            outside: 0,
        }
    }
}

impl Memory for FlatMemory {
    fn read8(&mut self, addr: u32) -> u8 {
        match addr.checked_sub(self.base).and_then(|o| self.data.get(o as usize)) {
            Some(b) => *b,
            None => {
                self.outside += 1;
                0
            }
        }
    }
    fn write8(&mut self, addr: u32, v: u8) {
        match addr.checked_sub(self.base).and_then(|o| self.data.get_mut(o as usize)) {
            Some(b) => *b = v,
            None => self.outside += 1,
        }
    }
}

/// The condition register bits and the carry/overflow bits of XER, as masks.
pub const XER_SO: u32 = 1 << 31;
pub const XER_OV: u32 = 1 << 30;
pub const XER_CA: u32 = 1 << 29;

/// The state of one core.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Cpu {
    pub gpr: [u32; 32],
    /// The floating point registers' bits (the first of a pair, for paired singles).
    pub fpr: [u64; 32],
    /// The second of each pair (paired singles only).
    pub ps1: [u64; 32],
    /// The condition register, field 0 in the top four bits.
    pub cr: u32,
    pub xer: u32,
    pub lr: u32,
    pub ctr: u32,
    pub fpscr: u32,
    pub gqr: [u32; 8],
    pub pc: u32,
    /// The address of a reservation made by `lwarx`.
    pub reservation: Option<u32>,
    /// The time base, read by `mftb`.
    pub tb: u64,
}

/// The mask `rlwinm` and friends use: ones from bit `mb` to bit `me` (bit 0 the most significant), wrapping if `mb > me`.
pub fn rotate_mask(mb: u32, me: u32) -> u32 {
    let begin = 0xffff_ffffu32 >> mb;
    let end = 0xffff_ffffu32 << (31 - me);
    if mb <= me { begin & end } else { begin | end }
}

/// `a + b + carry_in`: the result, the carry out and whether it overflowed as signed numbers.
fn add_with_carry(a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
    let wide = u64::from(a) + u64::from(b) + u64::from(carry_in);
    let result = wide as u32;
    let overflow = ((a ^ result) & (b ^ result)) >> 31 != 0;
    (result, wide >> 32 != 0, overflow)
}

/// A NaN made by an invalid operation is the positive quiet NaN on this processor (other machines make a negative one).
fn canonical(result: f64, inputs: &[f64]) -> f64 {
    if result.is_nan() {
        // an input NaN is passed on, made quiet; the first one wins
        if let Some(n) = inputs.iter().find(|x| x.is_nan()) {
            return f64::from_bits(n.to_bits() | 0x0008_0000_0000_0000);
        }
        return f64::from_bits(0x7ff8_0000_0000_0000);
    }
    result
}

fn canonical32(result: f32, inputs: &[f32]) -> f32 {
    if result.is_nan() {
        if let Some(n) = inputs.iter().find(|x| x.is_nan()) {
            return f32::from_bits(n.to_bits() | 0x0040_0000);
        }
        return f32::from_bits(0x7fc0_0000);
    }
    result
}

impl Cpu {
    fn cr_bit(&self, bit: u32) -> bool {
        (self.cr >> (31 - bit)) & 1 != 0
    }

    fn set_cr_bit(&mut self, bit: u32, value: bool) {
        let mask = 1u32 << (31 - bit);
        if value {
            self.cr |= mask;
        } else {
            self.cr &= !mask;
        }
    }

    /// Set field `n` of the condition register to the four bits `v`.
    fn set_cr_field(&mut self, n: usize, v: u32) {
        let shift = 28 - 4 * n as u32;
        self.cr = (self.cr & !(0xf << shift)) | ((v & 0xf) << shift);
    }

    fn cr_field(&self, n: usize) -> u32 {
        (self.cr >> (28 - 4 * n as u32)) & 0xf
    }

    /// Field 0 from a result (the `Rc` bit), with the summary overflow bit of XER.
    fn record(&mut self, result: u32) {
        let so = u32::from(self.xer & XER_SO != 0);
        let v = if (result as i32) < 0 {
            8
        } else if result == 0 {
            2
        } else {
            4
        };
        self.set_cr_field(0, v | so);
    }

    fn set_ov(&mut self, overflow: bool) {
        if overflow {
            self.xer |= XER_OV | XER_SO;
        } else {
            self.xer &= !XER_OV;
        }
    }

    fn set_ca(&mut self, carry: bool) {
        if carry {
            self.xer |= XER_CA;
        } else {
            self.xer &= !XER_CA;
        }
    }

    fn ca(&self) -> bool {
        self.xer & XER_CA != 0
    }

    /// The value of register `ra` for an address or `addi`: zero when the field is 0.
    fn ra_or_zero(&self, ra: usize) -> u32 {
        if ra == 0 { 0 } else { self.gpr[ra] }
    }

    fn compare(&mut self, field: usize, less: bool, greater: bool) {
        let so = u32::from(self.xer & XER_SO != 0);
        let v = if less {
            8
        } else if greater {
            4
        } else {
            2
        };
        self.set_cr_field(field, v | so);
    }

    fn f(&self, n: usize) -> f64 {
        f64::from_bits(self.fpr[n])
    }

    fn set_f(&mut self, n: usize, v: f64) {
        self.fpr[n] = v.to_bits();
    }

    /// The special register read by `mfspr`.
    fn read_spr(&self, n: u32) -> Result<u32, Stop> {
        Ok(match n {
            1 => self.xer,
            8 => self.lr,
            9 => self.ctr,
            912..=919 => self.gqr[(n - 912) as usize],
            _ => return Err(Stop::UnknownSpr(n)),
        })
    }

    fn write_spr(&mut self, n: u32, v: u32) -> Result<(), Stop> {
        match n {
            1 => self.xer = v & (XER_SO | XER_OV | XER_CA | 0x7f),
            8 => self.lr = v,
            9 => self.ctr = v,
            912..=919 => self.gqr[(n - 912) as usize] = v,
            _ => return Err(Stop::UnknownSpr(n)),
        }
        Ok(())
    }

    /// Run the instruction `insn`, which is at `self.pc`; afterwards `pc` is where the program goes next.
    pub fn execute<M: Memory>(&mut self, mem: &mut M, insn: Insn) -> Result<(), Stop> {
        let next = self.pc.wrapping_add(4);
        self.pc = next;
        let (rd, ra, rb) = (insn.rd(), insn.ra(), insn.rb());
        let rc = insn.rc();
        let w = insn.word;
        use Op::*;
        match insn.op {
            // ---- integer arithmetic
            Addi => self.gpr[rd] = self.ra_or_zero(ra).wrapping_add(insn.simm() as u32),
            Addis => self.gpr[rd] = self.ra_or_zero(ra).wrapping_add((insn.simm() as u32) << 16),
            Mulli => self.gpr[rd] = (self.gpr[ra] as i32).wrapping_mul(insn.simm()) as u32,
            Subfic => {
                let (r, c, _) = add_with_carry(!self.gpr[ra], insn.simm() as u32, true);
                self.gpr[rd] = r;
                self.set_ca(c);
            }
            Addic | AddicDot => {
                let (r, c, _) = add_with_carry(self.gpr[ra], insn.simm() as u32, false);
                self.gpr[rd] = r;
                self.set_ca(c);
                if insn.op == AddicDot {
                    self.record(r);
                }
            }
            Add | Addc | Adde | Addze | Addme | Subf | Subfc | Subfe | Subfze | Subfme | Neg => {
                let a = self.gpr[ra];
                let b = self.gpr[rb];
                let ca = self.ca();
                let (result, carry, overflow, sets_ca) = match insn.op {
                    Add => {
                        let (r, c, o) = add_with_carry(a, b, false);
                        (r, c, o, false)
                    }
                    Addc => {
                        let (r, c, o) = add_with_carry(a, b, false);
                        (r, c, o, true)
                    }
                    Adde => {
                        let (r, c, o) = add_with_carry(a, b, ca);
                        (r, c, o, true)
                    }
                    Addze => {
                        let (r, c, o) = add_with_carry(a, 0, ca);
                        (r, c, o, true)
                    }
                    Addme => {
                        let (r, c, o) = add_with_carry(a, 0xffff_ffff, ca);
                        (r, c, o, true)
                    }
                    Subf => {
                        let (r, c, o) = add_with_carry(!a, b, true);
                        (r, c, o, false)
                    }
                    Subfc => {
                        let (r, c, o) = add_with_carry(!a, b, true);
                        (r, c, o, true)
                    }
                    Subfe => {
                        let (r, c, o) = add_with_carry(!a, b, ca);
                        (r, c, o, true)
                    }
                    Subfze => {
                        let (r, c, o) = add_with_carry(!a, 0, ca);
                        (r, c, o, true)
                    }
                    Subfme => {
                        let (r, c, o) = add_with_carry(!a, 0xffff_ffff, ca);
                        (r, c, o, true)
                    }
                    _ => {
                        // neg
                        let (r, c, o) = add_with_carry(!a, 0, true);
                        (r, c, o, false)
                    }
                };
                self.gpr[rd] = result;
                if sets_ca {
                    self.set_ca(carry);
                }
                if insn.oe() {
                    self.set_ov(overflow);
                }
                if rc {
                    self.record(result);
                }
            }
            Mullw => {
                let p = i64::from(self.gpr[ra] as i32) * i64::from(self.gpr[rb] as i32);
                self.gpr[rd] = p as u32;
                if insn.oe() {
                    self.set_ov(p != i64::from(p as i32));
                }
                if rc {
                    self.record(p as u32);
                }
            }
            Mulhw => {
                let p = i64::from(self.gpr[ra] as i32) * i64::from(self.gpr[rb] as i32);
                self.gpr[rd] = (p >> 32) as u32;
                if rc {
                    self.record(self.gpr[rd]);
                }
            }
            Mulhwu => {
                let p = u64::from(self.gpr[ra]) * u64::from(self.gpr[rb]);
                self.gpr[rd] = (p >> 32) as u32;
                if rc {
                    self.record(self.gpr[rd]);
                }
            }
            Divw => {
                let (a, b) = (self.gpr[ra] as i32, self.gpr[rb] as i32);
                let undefined = b == 0 || (a == i32::MIN && b == -1);
                // the result of an undefined division is not defined by the architecture; this gives what the 750 gives (all ones for a
                // negative dividend, else zero)
                self.gpr[rd] = if undefined {
                    if a < 0 { 0xffff_ffff } else { 0 }
                } else {
                    (a / b) as u32
                };
                if insn.oe() {
                    self.set_ov(undefined);
                }
                if rc {
                    self.record(self.gpr[rd]);
                }
            }
            Divwu => {
                let (a, b) = (self.gpr[ra], self.gpr[rb]);
                self.gpr[rd] = a.checked_div(b).unwrap_or(0);
                if insn.oe() {
                    self.set_ov(b == 0);
                }
                if rc {
                    self.record(self.gpr[rd]);
                }
            }
            Extsb => {
                self.gpr[ra] = self.gpr[rd] as u8 as i8 as i32 as u32;
                if rc {
                    self.record(self.gpr[ra]);
                }
            }
            Extsh => {
                self.gpr[ra] = self.gpr[rd] as u16 as i16 as i32 as u32;
                if rc {
                    self.record(self.gpr[ra]);
                }
            }
            Cntlzw => {
                self.gpr[ra] = self.gpr[rd].leading_zeros();
                if rc {
                    self.record(self.gpr[ra]);
                }
            }
            // ---- compares
            Cmpi => {
                let (a, b) = (self.gpr[ra] as i32, insn.simm());
                self.compare(insn.crfd(), a < b, a > b);
            }
            Cmpli => {
                let (a, b) = (self.gpr[ra], insn.uimm());
                self.compare(insn.crfd(), a < b, a > b);
            }
            Cmp => {
                let (a, b) = (self.gpr[ra] as i32, self.gpr[rb] as i32);
                self.compare(insn.crfd(), a < b, a > b);
            }
            Cmpl => {
                let (a, b) = (self.gpr[ra], self.gpr[rb]);
                self.compare(insn.crfd(), a < b, a > b);
            }
            // ---- logic and shifts (the destination is rA, the source rS)
            Ori | Oris | Xori | Xoris | AndiDot | AndisDot => {
                let s = self.gpr[rd];
                let imm = insn.uimm();
                let r = match insn.op {
                    Ori => s | imm,
                    Oris => s | (imm << 16),
                    Xori => s ^ imm,
                    Xoris => s ^ (imm << 16),
                    AndiDot => s & imm,
                    _ => s & (imm << 16),
                };
                self.gpr[ra] = r;
                if matches!(insn.op, AndiDot | AndisDot) {
                    self.record(r);
                }
            }
            And | Or | Xor | Nand | Nor | Eqv | Andc | Orc => {
                let (s, b) = (self.gpr[rd], self.gpr[rb]);
                let r = match insn.op {
                    And => s & b,
                    Or => s | b,
                    Xor => s ^ b,
                    Nand => !(s & b),
                    Nor => !(s | b),
                    Eqv => !(s ^ b),
                    Andc => s & !b,
                    _ => s | !b,
                };
                self.gpr[ra] = r;
                if rc {
                    self.record(r);
                }
            }
            Slw | Srw => {
                let (s, n) = (self.gpr[rd], self.gpr[rb] & 0x3f);
                let r = if n >= 32 {
                    0
                } else if insn.op == Slw {
                    s << n
                } else {
                    s >> n
                };
                self.gpr[ra] = r;
                if rc {
                    self.record(r);
                }
            }
            Sraw | Srawi => {
                let s = self.gpr[rd] as i32;
                let n = if insn.op == Sraw { self.gpr[rb] & 0x3f } else { insn.sh() };
                let (r, carry) = if n >= 32 {
                    (if s < 0 { -1 } else { 0 }, s < 0)
                } else {
                    (s >> n, s < 0 && n > 0 && (s as u32) & ((1u32 << n) - 1) != 0)
                };
                self.gpr[ra] = r as u32;
                self.set_ca(carry);
                if rc {
                    self.record(r as u32);
                }
            }
            // ---- rotates
            Rlwinm | Rlwnm | Rlwimi => {
                let s = self.gpr[rd];
                let n = if insn.op == Rlwnm { self.gpr[rb] & 31 } else { insn.sh() };
                let m = rotate_mask(insn.mb(), insn.me());
                let rotated = s.rotate_left(n);
                let r = if insn.op == Rlwimi {
                    (rotated & m) | (self.gpr[ra] & !m)
                } else {
                    rotated & m
                };
                self.gpr[ra] = r;
                if rc {
                    self.record(r);
                }
            }
            // ---- branches
            B => {
                let target = if insn.aa() {
                    insn.li() as u32
                } else {
                    next.wrapping_sub(4).wrapping_add(insn.li() as u32)
                };
                if insn.lk() {
                    self.lr = next;
                }
                self.pc = target;
            }
            Bc | Bclr | Bcctr => {
                let (bo, bi) = (insn.bo(), insn.bi());
                if bo & 4 == 0 && insn.op != Bcctr {
                    self.ctr = self.ctr.wrapping_sub(1);
                }
                let ctr_ok = bo & 4 != 0 || ((self.ctr != 0) != (bo & 2 != 0));
                let cond_ok = bo & 16 != 0 || (self.cr_bit(bi) == (bo & 8 != 0));
                let target = match insn.op {
                    Bc => {
                        if insn.aa() {
                            insn.bd() as u32
                        } else {
                            next.wrapping_sub(4).wrapping_add(insn.bd() as u32)
                        }
                    }
                    Bclr => self.lr & !3,
                    _ => self.ctr & !3,
                };
                if insn.lk() {
                    self.lr = next;
                }
                if ctr_ok && cond_ok {
                    self.pc = target;
                }
            }
            // ---- condition register
            Mcrf => {
                let v = self.cr_field(insn.crfs());
                self.set_cr_field(insn.crfd(), v);
            }
            Crand | Crandc | Creqv | Crnand | Crnor | Cror | Crorc | Crxor => {
                let (a, b) = (self.cr_bit(ra as u32), self.cr_bit(rb as u32));
                let r = match insn.op {
                    Crand => a & b,
                    Crandc => a & !b,
                    Creqv => !(a ^ b),
                    Crnand => !(a & b),
                    Crnor => !(a | b),
                    Cror => a | b,
                    Crorc => a | !b,
                    _ => a ^ b,
                };
                self.set_cr_bit(rd as u32, r);
            }
            Mfcr => self.gpr[rd] = self.cr,
            Mtcrf => {
                let crm = insn.crm();
                let mut mask = 0u32;
                for i in 0..8 {
                    if crm & (0x80 >> i) != 0 {
                        mask |= 0xf << (28 - 4 * i);
                    }
                }
                self.cr = (self.cr & !mask) | (self.gpr[rd] & mask);
            }
            Mcrxr => {
                let v = self.xer >> 28;
                self.set_cr_field(insn.crfd(), v);
                self.xer &= 0x0fff_ffff;
            }
            // ---- special registers
            Mfspr => self.gpr[rd] = self.read_spr(insn.spr())?,
            Mtspr => self.write_spr(insn.spr(), self.gpr[rd])?,
            Mftb => {
                self.gpr[rd] = match insn.spr() {
                    268 => self.tb as u32,
                    269 => (self.tb >> 32) as u32,
                    n => return Err(Stop::UnknownSpr(n)),
                }
            }
            // ---- integer loads
            Lbz | Lbzu | Lbzx | Lbzux | Lhz | Lhzu | Lhzx | Lhzux | Lha | Lhau | Lhax | Lhaux | Lwz | Lwzu | Lwzx | Lwzux | Lhbrx
            | Lwbrx | Lwarx => {
                let indexed = matches!(
                    insn.op,
                    Lbzx | Lbzux | Lhzx | Lhzux | Lhax | Lhaux | Lwzx | Lwzux | Lhbrx | Lwbrx | Lwarx
                );
                let update = matches!(insn.op, Lbzu | Lbzux | Lhzu | Lhzux | Lhau | Lhaux | Lwzu | Lwzux);
                let ea = if indexed {
                    self.ra_or_zero(ra).wrapping_add(self.gpr[rb])
                } else {
                    self.ra_or_zero(ra).wrapping_add(insn.simm() as u32)
                };
                let value = match insn.op {
                    Lbz | Lbzu | Lbzx | Lbzux => u32::from(mem.read8(ea)),
                    Lhz | Lhzu | Lhzx | Lhzux => u32::from(mem.read16(ea)),
                    Lha | Lhau | Lhax | Lhaux => mem.read16(ea) as i16 as i32 as u32,
                    Lhbrx => u32::from(mem.read16(ea).swap_bytes()),
                    Lwbrx => mem.read32(ea).swap_bytes(),
                    Lwarx => {
                        self.reservation = Some(ea);
                        mem.read32(ea)
                    }
                    _ => mem.read32(ea),
                };
                self.gpr[rd] = value;
                if update {
                    self.gpr[ra] = ea;
                }
            }
            Lmw => {
                let mut ea = self.ra_or_zero(ra).wrapping_add(insn.simm() as u32);
                for r in rd..32 {
                    self.gpr[r] = mem.read32(ea);
                    ea = ea.wrapping_add(4);
                }
            }
            Lswi | Lswx => {
                let ea = if insn.op == Lswi {
                    self.ra_or_zero(ra)
                } else {
                    self.ra_or_zero(ra).wrapping_add(self.gpr[rb])
                };
                let n = if insn.op == Lswi { insn.nb() } else { self.xer & 0x7f };
                for k in 0..n {
                    let r = (rd + (k / 4) as usize) % 32;
                    if k % 4 == 0 {
                        self.gpr[r] = 0;
                    }
                    self.gpr[r] |= u32::from(mem.read8(ea.wrapping_add(k))) << (24 - 8 * (k % 4));
                }
            }
            // ---- integer stores
            Stb | Stbu | Stbx | Stbux | Sth | Sthu | Sthx | Sthux | Stw | Stwu | Stwx | Stwux | Sthbrx | Stwbrx | Stwcx => {
                let indexed = matches!(insn.op, Stbx | Stbux | Sthx | Sthux | Stwx | Stwux | Sthbrx | Stwbrx | Stwcx);
                let update = matches!(insn.op, Stbu | Stbux | Sthu | Sthux | Stwu | Stwux);
                let ea = if indexed {
                    self.ra_or_zero(ra).wrapping_add(self.gpr[rb])
                } else {
                    self.ra_or_zero(ra).wrapping_add(insn.simm() as u32)
                };
                let s = self.gpr[rd];
                match insn.op {
                    Stb | Stbu | Stbx | Stbux => mem.write8(ea, s as u8),
                    Sth | Sthu | Sthx | Sthux => mem.write16(ea, s as u16),
                    Sthbrx => mem.write16(ea, (s as u16).swap_bytes()),
                    Stwbrx => mem.write32(ea, s.swap_bytes()),
                    Stwcx => {
                        let ok = self.reservation == Some(ea);
                        if ok {
                            mem.write32(ea, s);
                        }
                        self.reservation = None;
                        let so = u32::from(self.xer & XER_SO != 0);
                        self.set_cr_field(0, if ok { 2 } else { 0 } | so);
                    }
                    _ => mem.write32(ea, s),
                }
                if update {
                    self.gpr[ra] = ea;
                }
            }
            Stmw => {
                let mut ea = self.ra_or_zero(ra).wrapping_add(insn.simm() as u32);
                for r in rd..32 {
                    mem.write32(ea, self.gpr[r]);
                    ea = ea.wrapping_add(4);
                }
            }
            Stswi | Stswx => {
                let ea = if insn.op == Stswi {
                    self.ra_or_zero(ra)
                } else {
                    self.ra_or_zero(ra).wrapping_add(self.gpr[rb])
                };
                let n = if insn.op == Stswi { insn.nb() } else { self.xer & 0x7f };
                for k in 0..n {
                    let r = (rd + (k / 4) as usize) % 32;
                    mem.write8(ea.wrapping_add(k), (self.gpr[r] >> (24 - 8 * (k % 4))) as u8);
                }
            }
            // ---- floating point loads and stores
            Lfs | Lfsu | Lfsx | Lfsux | Lfd | Lfdu | Lfdx | Lfdux => {
                let indexed = matches!(insn.op, Lfsx | Lfsux | Lfdx | Lfdux);
                let update = matches!(insn.op, Lfsu | Lfsux | Lfdu | Lfdux);
                let ea = if indexed {
                    self.ra_or_zero(ra).wrapping_add(self.gpr[rb])
                } else {
                    self.ra_or_zero(ra).wrapping_add(insn.simm() as u32)
                };
                if matches!(insn.op, Lfs | Lfsu | Lfsx | Lfsux) {
                    let bits = mem.read32(ea);
                    let single = f32::from_bits(bits);
                    // a single is widened exactly; a NaN keeps its payload and is made quiet
                    self.fpr[rd] = if single.is_nan() {
                        widen_nan(bits)
                    } else {
                        f64::from(single).to_bits()
                    };
                    // in paired-single mode a load of a single fills both halves
                    self.ps1[rd] = self.fpr[rd];
                } else {
                    self.fpr[rd] = mem.read64(ea);
                }
                if update {
                    self.gpr[ra] = ea;
                }
            }
            Stfs | Stfsu | Stfsx | Stfsux | Stfd | Stfdu | Stfdx | Stfdux | Stfiwx => {
                let indexed = matches!(insn.op, Stfsx | Stfsux | Stfdx | Stfdux | Stfiwx);
                let update = matches!(insn.op, Stfsu | Stfsux | Stfdu | Stfdux);
                let ea = if indexed {
                    self.ra_or_zero(ra).wrapping_add(self.gpr[rb])
                } else {
                    self.ra_or_zero(ra).wrapping_add(insn.simm() as u32)
                };
                match insn.op {
                    Stfs | Stfsu | Stfsx | Stfsux => mem.write32(ea, narrow(self.fpr[rd])),
                    Stfiwx => mem.write32(ea, self.fpr[rd] as u32),
                    _ => mem.write64(ea, self.fpr[rd]),
                }
                if update {
                    self.gpr[ra] = ea;
                }
            }
            // ---- cache and synchronisation
            Dcbz => {
                let ea = self.ra_or_zero(ra).wrapping_add(self.gpr[rb]) & !31;
                for i in 0..32 {
                    mem.write8(ea.wrapping_add(i), 0);
                }
            }
            Dcbf | Dcbi | Dcbst | Dcbt | Dcbtst | Icbi | Sync | Isync | Eieio => {}
            // ---- traps and the system
            Sc => return Err(Stop::SystemCall),
            Tw | Twi => {
                let to = rd as u32;
                let a = self.gpr[ra];
                let b = if insn.op == Twi { insn.simm() as u32 } else { self.gpr[rb] };
                let hit = (to & 16 != 0 && (a as i32) < (b as i32))
                    || (to & 8 != 0 && (a as i32) > (b as i32))
                    || (to & 4 != 0 && a == b)
                    || (to & 2 != 0 && a < b)
                    || (to & 1 != 0 && a > b);
                if hit {
                    return Err(Stop::Trap(w));
                }
            }
            op if op.class() == crate::Class::Float || op.class() == crate::Class::Special => self.float(insn)?,
            _ => return Err(Stop::Unsupported(w)),
        }
        Ok(())
    }

    /// Floating point arithmetic and the floating point status register moves.
    fn float(&mut self, insn: Insn) -> Result<(), Stop> {
        use Op::*;
        let (rd, ra, rb, rc) = (insn.rd(), insn.ra(), insn.rb(), insn.rc_f());
        let (a, b, c) = (self.f(ra), self.f(rb), self.f(rc));
        let single = |x: f64| x as f32;
        match insn.op {
            Fadd => self.set_f(rd, canonical(a + b, &[a, b])),
            Fsub => self.set_f(rd, canonical(a - b, &[a, b])),
            Fmul => self.set_f(rd, canonical(a * c, &[a, c])),
            Fdiv => self.set_f(rd, canonical(a / b, &[a, b])),
            Fmadd => self.set_f(rd, canonical(a.mul_add(c, b), &[a, c, b])),
            Fmsub => self.set_f(rd, canonical(a.mul_add(c, -b), &[a, c, b])),
            Fnmadd => {
                let r = canonical(a.mul_add(c, b), &[a, c, b]);
                self.set_f(rd, if r.is_nan() { r } else { -r });
            }
            Fnmsub => {
                let r = canonical(a.mul_add(c, -b), &[a, c, b]);
                self.set_f(rd, if r.is_nan() { r } else { -r });
            }
            Fadds | Fsubs | Fmuls | Fdivs | Fmadds | Fmsubs | Fnmadds | Fnmsubs => {
                let (x, y, z) = (single(a), single(b), single(c));
                let r = match insn.op {
                    Fadds => x + y,
                    Fsubs => x - y,
                    Fmuls => x * z,
                    Fdivs => x / y,
                    Fmadds | Fnmadds => x.mul_add(z, y),
                    _ => x.mul_add(z, -y),
                };
                let r = canonical32(r, &[x, y, z]);
                let r = if matches!(insn.op, Fnmadds | Fnmsubs) && !r.is_nan() {
                    -r
                } else {
                    r
                };
                self.set_f(rd, f64::from(r));
            }
            Fsqrt => self.set_f(rd, canonical(b.sqrt(), &[b])),
            Fsqrts => self.set_f(rd, f64::from(canonical32(single(b).sqrt(), &[single(b)]))),
            Fres => self.set_f(rd, f64::from(canonical32(1.0 / single(b), &[single(b)]))),
            Frsqrte => self.set_f(rd, canonical(1.0 / b.sqrt(), &[b])),
            Fsel => self.set_f(rd, if a >= 0.0 { c } else { b }),
            Frsp => self.set_f(rd, f64::from(canonical32(single(b), &[single(b)]))),
            Fctiwz => self.fpr[rd] = u64::from(round_to_i32(b, true) as u32),
            Fctiw => self.fpr[rd] = u64::from(round_to_i32(b, false) as u32),
            Fcmpu | Fcmpo => {
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
            Fmr => self.fpr[rd] = self.fpr[rb],
            Fneg => self.fpr[rd] = self.fpr[rb] ^ (1 << 63),
            Fabs => self.fpr[rd] = self.fpr[rb] & !(1 << 63),
            Fnabs => self.fpr[rd] = self.fpr[rb] | (1 << 63),
            Mffs => self.fpr[rd] = 0xfff8_0000_0000_0000 | u64::from(self.fpscr),
            Mtfsf => {
                let fm = (insn.word >> 17) & 0xff;
                let mut mask = 0u32;
                for i in 0..8 {
                    if fm & (0x80 >> i) != 0 {
                        mask |= 0xf << (28 - 4 * i);
                    }
                }
                self.fpscr = (self.fpscr & !mask) | (self.fpr[rb] as u32 & mask);
            }
            Mtfsfi => {
                let n = insn.crfd();
                let imm = (insn.word >> 12) & 0xf;
                let shift = 28 - 4 * n as u32;
                self.fpscr = (self.fpscr & !(0xf << shift)) | (imm << shift);
            }
            Mtfsb0 | Mtfsb1 => {
                let bit = 1u32 << (31 - rd as u32);
                if insn.op == Mtfsb1 {
                    self.fpscr |= bit;
                } else {
                    self.fpscr &= !bit;
                }
            }
            Mcrfs => {
                let shift = 28 - 4 * insn.crfs() as u32;
                let v = (self.fpscr >> shift) & 0xf;
                self.set_cr_field(insn.crfd(), v);
                self.fpscr &= !(0xf << shift);
            }
            _ => return Err(Stop::Unsupported(insn.word)),
        }
        Ok(())
    }
}

/// A single NaN's bits widened to a double's, as `lfs` does: the fields are moved, nothing is quieted.
fn widen_nan(bits: u32) -> u64 {
    let sign = u64::from(bits >> 31) << 63;
    let payload = u64::from(bits & 0x007f_ffff) << 29;
    sign | 0x7ff0_0000_0000_0000 | payload
}

/// A double's bits narrowed to a single's, as `stfs` does: rounded to nearest even; a NaN keeps the top of its payload.
fn narrow(bits: u64) -> u32 {
    let d = f64::from_bits(bits);
    if d.is_nan() {
        let sign = ((bits >> 63) as u32) << 31;
        sign | 0x7f80_0000 | ((bits >> 29) as u32 & 0x007f_ffff)
    } else {
        (d as f32).to_bits()
    }
}

/// `fctiw`/`fctiwz`: to a 32-bit integer, saturating; a NaN gives the smallest number.
fn round_to_i32(x: f64, truncate: bool) -> i32 {
    if x.is_nan() {
        return i32::MIN;
    }
    let r = if truncate { x.trunc() } else { x.round_ties_even() };
    if r >= 2147483648.0 {
        i32::MAX
    } else if r < -2147483648.0 {
        i32::MIN
    } else {
        r as i32
    }
}
