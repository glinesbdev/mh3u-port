//! A decoded PowerPC instruction: which operation it is and the fields of its word, named as the architecture books name them.

use crate::Class;
use crate::Class::*;

macro_rules! ops {
    ($($v:ident = $name:literal, $class:ident;)*) => {
        /// One operation of the instruction set (the name is the assembler's, with a trailing `.` where the word is an `andi.` kind of
        /// instruction that always records; `Rc` forms are `rc` on the instruction and not separate operations).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Op { $($v),* }

        impl Op {
            pub fn name(self) -> &'static str { match self { $(Op::$v => $name),* } }
            pub fn class(self) -> Class { match self { $(Op::$v => $class),* } }
            /// Every operation, for tests.
            pub const ALL: &'static [Op] = &[$(Op::$v),*];
        }
    };
}

ops! {
    // integer arithmetic
    Mulli = "mulli", Integer; Subfic = "subfic", Integer; Addic = "addic", Integer; AddicDot = "addic.", Integer;
    Addi = "addi", Integer; Addis = "addis", Integer; Add = "add", Integer; Subf = "subf", Integer; Neg = "neg", Integer;
    Mullw = "mullw", Integer; Mulhw = "mulhw", Integer; Mulhwu = "mulhwu", Integer; Divw = "divw", Integer; Divwu = "divwu", Integer;
    Addc = "addc", Integer; Subfc = "subfc", Integer; Adde = "adde", Integer; Subfe = "subfe", Integer; Addze = "addze", Integer;
    Subfze = "subfze", Integer; Addme = "addme", Integer; Subfme = "subfme", Integer; Extsb = "extsb", Integer;
    Extsh = "extsh", Integer; Cntlzw = "cntlzw", Integer;
    // compares
    Cmpi = "cmpi", Compare; Cmpli = "cmpli", Compare; Cmp = "cmp", Compare; Cmpl = "cmpl", Compare;
    // logic and shifts
    Ori = "ori", Logical; Oris = "oris", Logical; Xori = "xori", Logical; Xoris = "xoris", Logical;
    AndiDot = "andi.", Logical; AndisDot = "andis.", Logical; And = "and", Logical; Or = "or", Logical; Xor = "xor", Logical;
    Nand = "nand", Logical; Nor = "nor", Logical; Eqv = "eqv", Logical; Andc = "andc", Logical; Orc = "orc", Logical;
    Slw = "slw", Logical; Srw = "srw", Logical; Sraw = "sraw", Logical; Srawi = "srawi", Logical;
    // rotates
    Rlwimi = "rlwimi", Rotate; Rlwinm = "rlwinm", Rotate; Rlwnm = "rlwnm", Rotate;
    // branches
    B = "b", Branch; Bc = "bc", Branch; Bclr = "bclr", Branch; Bcctr = "bcctr", Branch;
    // condition register
    Mcrf = "mcrf", CondReg; Crand = "crand", CondReg; Crandc = "crandc", CondReg; Creqv = "creqv", CondReg;
    Crnand = "crnand", CondReg; Crnor = "crnor", CondReg; Cror = "cror", CondReg; Crorc = "crorc", CondReg;
    Crxor = "crxor", CondReg; Mfcr = "mfcr", CondReg; Mtcrf = "mtcrf", CondReg; Mcrxr = "mcrxr", CondReg;
    // integer loads
    Lbz = "lbz", Load; Lbzu = "lbzu", Load; Lbzx = "lbzx", Load; Lbzux = "lbzux", Load;
    Lhz = "lhz", Load; Lhzu = "lhzu", Load; Lhzx = "lhzx", Load; Lhzux = "lhzux", Load;
    Lha = "lha", Load; Lhau = "lhau", Load; Lhax = "lhax", Load; Lhaux = "lhaux", Load;
    Lwz = "lwz", Load; Lwzu = "lwzu", Load; Lwzx = "lwzx", Load; Lwzux = "lwzux", Load;
    Lhbrx = "lhbrx", Load; Lwbrx = "lwbrx", Load; Lmw = "lmw", Load; Lswi = "lswi", Load; Lswx = "lswx", Load;
    Lwarx = "lwarx", Load; Eciwx = "eciwx", Load;
    // integer stores
    Stb = "stb", Store; Stbu = "stbu", Store; Stbx = "stbx", Store; Stbux = "stbux", Store;
    Sth = "sth", Store; Sthu = "sthu", Store; Sthx = "sthx", Store; Sthux = "sthux", Store;
    Stw = "stw", Store; Stwu = "stwu", Store; Stwx = "stwx", Store; Stwux = "stwux", Store;
    Sthbrx = "sthbrx", Store; Stwbrx = "stwbrx", Store; Stmw = "stmw", Store; Stswi = "stswi", Store; Stswx = "stswx", Store;
    Stwcx = "stwcx.", Store; Ecowx = "ecowx", Store;
    // floating point loads and stores
    Lfs = "lfs", FloatMem; Lfsu = "lfsu", FloatMem; Lfsx = "lfsx", FloatMem; Lfsux = "lfsux", FloatMem;
    Lfd = "lfd", FloatMem; Lfdu = "lfdu", FloatMem; Lfdx = "lfdx", FloatMem; Lfdux = "lfdux", FloatMem;
    Stfs = "stfs", FloatMem; Stfsu = "stfsu", FloatMem; Stfsx = "stfsx", FloatMem; Stfsux = "stfsux", FloatMem;
    Stfd = "stfd", FloatMem; Stfdu = "stfdu", FloatMem; Stfdx = "stfdx", FloatMem; Stfdux = "stfdux", FloatMem;
    Stfiwx = "stfiwx", FloatMem;
    // floating point arithmetic
    Fadd = "fadd", Float; Fadds = "fadds", Float; Fsub = "fsub", Float; Fsubs = "fsubs", Float;
    Fmul = "fmul", Float; Fmuls = "fmuls", Float; Fdiv = "fdiv", Float; Fdivs = "fdivs", Float;
    Fmadd = "fmadd", Float; Fmadds = "fmadds", Float; Fmsub = "fmsub", Float; Fmsubs = "fmsubs", Float;
    Fnmadd = "fnmadd", Float; Fnmadds = "fnmadds", Float; Fnmsub = "fnmsub", Float; Fnmsubs = "fnmsubs", Float;
    Fsel = "fsel", Float; Fres = "fres", Float; Frsqrte = "frsqrte", Float; Fsqrt = "fsqrt", Float; Fsqrts = "fsqrts", Float;
    Fctiw = "fctiw", Float; Fctiwz = "fctiwz", Float; Frsp = "frsp", Float; Fcmpu = "fcmpu", Float; Fcmpo = "fcmpo", Float;
    Fmr = "fmr", Float; Fneg = "fneg", Float; Fabs = "fabs", Float; Fnabs = "fnabs", Float;
    // special registers and the floating point status register
    Mfspr = "mfspr", Special; Mtspr = "mtspr", Special; Mftb = "mftb", Special; Mfmsr = "mfmsr", Special; Mtmsr = "mtmsr", Special;
    Mfsr = "mfsr", Special; Mtsr = "mtsr", Special; Mfsrin = "mfsrin", Special; Mtsrin = "mtsrin", Special;
    Mffs = "mffs", Special; Mtfsf = "mtfsf", Special; Mtfsfi = "mtfsfi", Special; Mtfsb0 = "mtfsb0", Special;
    Mtfsb1 = "mtfsb1", Special; Mcrfs = "mcrfs", Special;
    // paired singles
    PsAdd = "ps_add", Paired; PsSub = "ps_sub", Paired; PsMul = "ps_mul", Paired; PsDiv = "ps_div", Paired;
    PsMadd = "ps_madd", Paired; PsMsub = "ps_msub", Paired; PsNmadd = "ps_nmadd", Paired; PsNmsub = "ps_nmsub", Paired;
    PsSum0 = "ps_sum0", Paired; PsSum1 = "ps_sum1", Paired; PsMuls0 = "ps_muls0", Paired; PsMuls1 = "ps_muls1", Paired;
    PsMadds0 = "ps_madds0", Paired; PsMadds1 = "ps_madds1", Paired; PsSel = "ps_sel", Paired; PsRes = "ps_res", Paired;
    PsRsqrte = "ps_rsqrte", Paired; PsCmpu0 = "ps_cmpu0", Paired; PsCmpo0 = "ps_cmpo0", Paired; PsCmpu1 = "ps_cmpu1", Paired;
    PsCmpo1 = "ps_cmpo1", Paired; PsNeg = "ps_neg", Paired; PsMr = "ps_mr", Paired; PsNabs = "ps_nabs", Paired;
    PsAbs = "ps_abs", Paired; PsMerge00 = "ps_merge00", Paired; PsMerge01 = "ps_merge01", Paired;
    PsMerge10 = "ps_merge10", Paired; PsMerge11 = "ps_merge11", Paired;
    PsqL = "psq_l", Paired; PsqLu = "psq_lu", Paired; PsqSt = "psq_st", Paired; PsqStu = "psq_stu", Paired;
    PsqLx = "psq_lx", Paired; PsqStx = "psq_stx", Paired; PsqLux = "psq_lux", Paired; PsqStux = "psq_stux", Paired;
    // cache control
    Dcbf = "dcbf", Cache; Dcbi = "dcbi", Cache; Dcbst = "dcbst", Cache; Dcbt = "dcbt", Cache; Dcbtst = "dcbtst", Cache;
    Dcbz = "dcbz", Cache; DcbzL = "dcbz_l", Cache; Icbi = "icbi", Cache;
    // synchronisation
    Sync = "sync", Sync; Isync = "isync", Sync; Eieio = "eieio", Sync;
    // system
    Sc = "sc", System; Tw = "tw", System; Twi = "twi", System; Rfi = "rfi", System; Tlbie = "tlbie", System;
    Tlbia = "tlbia", System; Tlbsync = "tlbsync", System;
    // 64-bit only
    Only64 = "64-bit-only", Only64;
}

/// A decoded instruction word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Insn {
    pub word: u32,
    pub op: Op,
}

fn bits(word: u32, from: u32, to: u32) -> u32 {
    (word >> (31 - to)) & ((1u32 << (to - from + 1)) - 1)
}

impl Insn {
    pub fn decode(word: u32) -> Option<Insn> {
        decode_op(word).map(|op| Insn { word, op })
    }

    /// The destination or source register of the first operand (`rD`, `rS`, `frD`, `frS`, `crbD`, `BO`, `TO`).
    pub fn rd(&self) -> usize {
        ((self.word >> 21) & 31) as usize
    }
    pub fn ra(&self) -> usize {
        ((self.word >> 16) & 31) as usize
    }
    pub fn rb(&self) -> usize {
        ((self.word >> 11) & 31) as usize
    }
    /// The third floating point operand (`frC`).
    pub fn rc_f(&self) -> usize {
        ((self.word >> 6) & 31) as usize
    }
    /// The record bit: the result sets condition register field 0 (field 1 for floating point).
    pub fn rc(&self) -> bool {
        self.word & 1 != 0
    }
    /// The overflow-enable bit of the arithmetic forms.
    pub fn oe(&self) -> bool {
        (self.word >> 10) & 1 != 0
    }
    pub fn lk(&self) -> bool {
        self.word & 1 != 0
    }
    pub fn aa(&self) -> bool {
        (self.word >> 1) & 1 != 0
    }
    pub fn simm(&self) -> i32 {
        i32::from(self.word as i16)
    }
    pub fn uimm(&self) -> u32 {
        self.word & 0xffff
    }
    /// The displacement of an unconditional branch, in bytes.
    pub fn li(&self) -> i32 {
        ((self.word & 0x03ff_fffc) as i32) << 6 >> 6
    }
    /// The displacement of a conditional branch, in bytes.
    pub fn bd(&self) -> i32 {
        i32::from((self.word & 0xfffc) as u16 as i16)
    }
    pub fn bo(&self) -> u32 {
        (self.word >> 21) & 31
    }
    pub fn bi(&self) -> u32 {
        (self.word >> 16) & 31
    }
    pub fn sh(&self) -> u32 {
        (self.word >> 11) & 31
    }
    pub fn mb(&self) -> u32 {
        (self.word >> 6) & 31
    }
    pub fn me(&self) -> u32 {
        (self.word >> 1) & 31
    }
    /// The destination field of a compare or a condition register move.
    pub fn crfd(&self) -> usize {
        ((self.word >> 23) & 7) as usize
    }
    pub fn crfs(&self) -> usize {
        ((self.word >> 18) & 7) as usize
    }
    /// The field mask of `mtcrf`.
    pub fn crm(&self) -> u32 {
        (self.word >> 12) & 0xff
    }
    /// Whether a compare is 64-bit (always false on this processor).
    pub fn l(&self) -> bool {
        (self.word >> 21) & 1 != 0
    }
    pub fn spr(&self) -> u32 {
        crate::spr(self.word)
    }
    /// The number of bytes of a string instruction's immediate form (0 means 32).
    pub fn nb(&self) -> u32 {
        let n = (self.word >> 11) & 31;
        if n == 0 { 32 } else { n }
    }
    /// The quantization register and the "single" flag of a paired-single load or store with a displacement.
    pub fn psq_w(&self) -> bool {
        (self.word >> 15) & 1 != 0
    }
    pub fn psq_i(&self) -> usize {
        ((self.word >> 12) & 7) as usize
    }
    /// The 12-bit signed displacement of `psq_l` and `psq_st`.
    pub fn psq_d(&self) -> i32 {
        ((self.word & 0xfff) as i32) << 20 >> 20
    }
    /// The same of the indexed forms (`psq_lx`...).
    pub fn psqx_w(&self) -> bool {
        (self.word >> 10) & 1 != 0
    }
    pub fn psqx_i(&self) -> usize {
        ((self.word >> 7) & 7) as usize
    }
}

fn decode_op(word: u32) -> Option<Op> {
    use Op::*;
    let opcode = word >> 26;
    Some(match opcode {
        3 => Twi,
        4 => return paired(word),
        7 => Mulli,
        8 => Subfic,
        10 => Cmpli,
        11 => Cmpi,
        12 => Addic,
        13 => AddicDot,
        14 => Addi,
        15 => Addis,
        16 => Bc,
        17 => Sc,
        18 => B,
        19 => return group19(word),
        20 => Rlwimi,
        21 => Rlwinm,
        23 => Rlwnm,
        24 => Ori,
        25 => Oris,
        26 => Xori,
        27 => Xoris,
        28 => AndiDot,
        29 => AndisDot,
        30 | 58 | 62 => Only64,
        31 => return group31(word),
        32 => Lwz,
        33 => Lwzu,
        34 => Lbz,
        35 => Lbzu,
        36 => Stw,
        37 => Stwu,
        38 => Stb,
        39 => Stbu,
        40 => Lhz,
        41 => Lhzu,
        42 => Lha,
        43 => Lhau,
        44 => Sth,
        45 => Sthu,
        46 => Lmw,
        47 => Stmw,
        48 => Lfs,
        49 => Lfsu,
        50 => Lfd,
        51 => Lfdu,
        52 => Stfs,
        53 => Stfsu,
        54 => Stfd,
        55 => Stfdu,
        56 => PsqL,
        57 => PsqLu,
        59 => return single_float(word),
        60 => PsqSt,
        61 => PsqStu,
        63 => return double_float(word),
        _ => return None,
    })
}

fn group19(word: u32) -> Option<Op> {
    use Op::*;
    Some(match bits(word, 21, 30) {
        0 => Mcrf,
        16 => Bclr,
        33 => Crnor,
        50 => Rfi,
        129 => Crandc,
        150 => Isync,
        193 => Crxor,
        225 => Crnand,
        257 => Crand,
        289 => Creqv,
        417 => Crorc,
        449 => Cror,
        528 => Bcctr,
        _ => return None,
    })
}

fn group31(word: u32) -> Option<Op> {
    use Op::*;
    let ext = bits(word, 21, 30);
    // the arithmetic forms have an overflow-enable bit (bit 21), which is not part of which instruction it is
    Some(match ext & 0x1ff {
        8 => Subfc,
        10 => Addc,
        136 => Subfe,
        138 => Adde,
        200 => Subfze,
        202 => Addze,
        232 => Subfme,
        234 => Addme,
        235 => Mullw,
        266 => Add,
        459 => Divwu,
        491 => Divw,
        40 => Subf,
        104 => Neg,
        11 => Mulhwu,
        75 => Mulhw,
        _ => match ext {
            0 => Cmp,
            4 => Tw,
            19 => Mfcr,
            20 => Lwarx,
            23 => Lwzx,
            24 => Slw,
            26 => Cntlzw,
            28 => And,
            32 => Cmpl,
            54 => Dcbst,
            55 => Lwzux,
            60 => Andc,
            83 => Mfmsr,
            86 => Dcbf,
            87 => Lbzx,
            119 => Lbzux,
            124 => Nor,
            144 => Mtcrf,
            146 => Mtmsr,
            150 => Stwcx,
            151 => Stwx,
            183 => Stwux,
            210 => Mtsr,
            215 => Stbx,
            242 => Mtsrin,
            246 => Dcbtst,
            247 => Stbux,
            278 => Dcbt,
            279 => Lhzx,
            284 => Eqv,
            306 => Tlbie,
            310 => Eciwx,
            311 => Lhzux,
            316 => Xor,
            339 => Mfspr,
            343 => Lhax,
            370 => Tlbia,
            371 => Mftb,
            375 => Lhaux,
            407 => Sthx,
            412 => Orc,
            438 => Ecowx,
            439 => Sthux,
            444 => Or,
            467 => Mtspr,
            470 => Dcbi,
            476 => Nand,
            512 => Mcrxr,
            533 => Lswx,
            534 => Lwbrx,
            535 => Lfsx,
            536 => Srw,
            566 => Tlbsync,
            567 => Lfsux,
            595 => Mfsr,
            597 => Lswi,
            598 => Sync,
            599 => Lfdx,
            631 => Lfdux,
            659 => Mfsrin,
            661 => Stswx,
            662 => Stwbrx,
            663 => Stfsx,
            695 => Stfsux,
            725 => Stswi,
            727 => Stfdx,
            759 => Stfdux,
            790 => Lhbrx,
            792 => Sraw,
            824 => Srawi,
            854 => Eieio,
            918 => Sthbrx,
            922 => Extsh,
            954 => Extsb,
            982 => Icbi,
            983 => Stfiwx,
            1014 => Dcbz,
            9 | 21 | 27 | 53 | 58 | 73 | 84 | 149 | 181 | 214 => Only64,
            _ => return None,
        },
    })
}

fn paired(word: u32) -> Option<Op> {
    use Op::*;
    Some(match bits(word, 26, 30) {
        10 => PsSum0,
        11 => PsSum1,
        12 => PsMuls0,
        13 => PsMuls1,
        14 => PsMadds0,
        15 => PsMadds1,
        18 => PsDiv,
        20 => PsSub,
        21 => PsAdd,
        23 => PsSel,
        24 => PsRes,
        25 => PsMul,
        26 => PsRsqrte,
        28 => PsMsub,
        29 => PsMadd,
        30 => PsNmsub,
        31 => PsNmadd,
        _ => match bits(word, 21, 30) {
            0 => PsCmpu0,
            32 => PsCmpo0,
            40 => PsNeg,
            64 => PsCmpu1,
            72 => PsMr,
            96 => PsCmpo1,
            136 => PsNabs,
            264 => PsAbs,
            528 => PsMerge00,
            560 => PsMerge01,
            592 => PsMerge10,
            624 => PsMerge11,
            1014 => DcbzL,
            _ => match bits(word, 25, 30) {
                6 => PsqLx,
                7 => PsqStx,
                38 => PsqLux,
                39 => PsqStux,
                _ => return None,
            },
        },
    })
}

fn single_float(word: u32) -> Option<Op> {
    use Op::*;
    Some(match bits(word, 26, 30) {
        18 => Fdivs,
        20 => Fsubs,
        21 => Fadds,
        22 => Fsqrts,
        24 => Fres,
        25 => Fmuls,
        28 => Fmsubs,
        29 => Fmadds,
        30 => Fnmsubs,
        31 => Fnmadds,
        _ => return None,
    })
}

fn double_float(word: u32) -> Option<Op> {
    use Op::*;
    match bits(word, 26, 30) {
        18 => return Some(Fdiv),
        20 => return Some(Fsub),
        21 => return Some(Fadd),
        22 => return Some(Fsqrt),
        23 => return Some(Fsel),
        25 => return Some(Fmul),
        26 => return Some(Frsqrte),
        28 => return Some(Fmsub),
        29 => return Some(Fmadd),
        30 => return Some(Fnmsub),
        31 => return Some(Fnmadd),
        _ => {}
    }
    Some(match bits(word, 21, 30) {
        0 => Fcmpu,
        12 => Frsp,
        14 => Fctiw,
        15 => Fctiwz,
        32 => Fcmpo,
        38 => Mtfsb1,
        40 => Fneg,
        64 => Mcrfs,
        70 => Mtfsb0,
        72 => Fmr,
        134 => Mtfsfi,
        136 => Fnabs,
        264 => Fabs,
        583 => Mffs,
        711 => Mtfsf,
        _ => return None,
    })
}
