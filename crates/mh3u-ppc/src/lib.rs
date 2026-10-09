//! The Wii U's processor: decoding PowerPC instructions (the "Espresso" cores, a 32-bit PowerPC 750 with paired singles).
//!
//! Every instruction is one big-endian 32-bit word. [`decode`] says which instruction a word is and what kind of thing it does,
//! and nothing more; it does not run or translate anything. It exists to count what the game's code needs (see `docs/spikes.md`) and is
//! the base the translator would be built on. The instruction set is as the PowerPC architecture book and the Gekko/Broadway manuals give
//! it; where a word is none of those, [`decode`] says so rather than guessing.

/// What kind of thing an instruction does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// add, subtract, multiply, divide, negate, extend, count leading zeros.
    Integer,
    /// compare integers.
    Compare,
    /// and, or, xor, shifts and the like.
    Logical,
    /// rotate and mask.
    Rotate,
    /// branches, calls and returns.
    Branch,
    /// condition register logic and moves.
    CondReg,
    /// integer loads (bytes, halves, words, multiples, strings, reserved).
    Load,
    /// integer stores.
    Store,
    /// floating point loads and stores.
    FloatMem,
    /// floating point arithmetic and moves.
    Float,
    /// the paired-single instructions (two singles in one register), and their loads and stores.
    Paired,
    /// moves to and from special registers, the time base, the machine state.
    Special,
    /// cache control.
    Cache,
    /// synchronisation and the like.
    Sync,
    /// system call, trap, return from interrupt.
    System,
    /// instructions the architecture has for 64-bit processors only.
    Only64,
}

pub mod fused;
pub mod insn;
pub mod interp;
pub mod paired;
pub use insn::{Insn, Op};

/// One decoded instruction: its name and kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decoded {
    pub name: &'static str,
    pub class: Class,
}

/// Decode one instruction word, or `None` if it is none of the instructions of this processor.
pub fn decode(word: u32) -> Option<Decoded> {
    Insn::decode(word).map(|i| Decoded {
        name: i.op.name(),
        class: i.op.class(),
    })
}

/// Which special register a `mfspr` or `mtspr` word names (the halves of the number are swapped in the encoding).
pub fn spr(word: u32) -> u32 {
    let field = (word >> 11) & 0x3ff;
    ((field & 0x1f) << 5) | (field >> 5)
}

/// A name for the special registers the game's code uses, `None` for ones with no usual name here.
pub fn spr_name(number: u32) -> Option<&'static str> {
    Some(match number {
        1 => "XER",
        8 => "LR",
        9 => "CTR",
        18 => "DSISR",
        19 => "DAR",
        22 => "DEC",
        26 => "SRR0",
        27 => "SRR1",
        268 => "TBL",
        269 => "TBU",
        272..=275 => "SPRG",
        528..=543 => "BAT",
        912..=919 => "GQR",
        920 => "HID2",
        921 => "WPAR",
        922 => "DMA_U",
        923 => "DMA_L",
        936..=939 => "UMMCR/UPMC",
        952..=959 => "MMCR/PMC/SIA",
        1008 => "HID0",
        1009 => "HID1",
        1010 => "IABR",
        1013 => "DABR",
        1017 => "L2CR",
        1019 => "ICTC",
        1020 => "THRM",
        _ => return None,
    })
}

/// A readable line for an instruction word at `addr`: the mnemonic and operands in the usual assembler order. Meant for looking at code
/// (it covers the forms the game uses; unusual ones fall back to the raw fields).
pub fn format(addr: u32, word: u32) -> String {
    use insn::{Insn, Op};
    let Some(i) = Insn::decode(word) else {
        return format!(".long {word:#010x}");
    };
    let n = i.op.name();
    let (rd, ra, rb) = (i.rd(), i.ra(), i.rb());
    let dot = if i.rc() && !matches!(i.op, Op::B | Op::Bc | Op::Bclr | Op::Bcctr) {
        "."
    } else {
        ""
    };
    let target = |off: i32, absolute: bool| if absolute { off as u32 } else { addr.wrapping_add(off as u32) };
    match i.op {
        Op::B => format!(
            "{}{} {:#x}",
            if i.lk() { "bl" } else { "b" },
            if i.aa() { "a" } else { "" },
            target(i.li(), i.aa())
        ),
        Op::Bc => {
            let l = if i.lk() { "l" } else { "" };
            format!("bc{l} {},{},{:#x}", i.bo(), i.bi(), target(i.bd(), i.aa()))
        }
        Op::Bclr if i.bo() == 20 => {
            if i.lk() {
                "blrl".into()
            } else {
                "blr".into()
            }
        }
        Op::Bcctr if i.bo() == 20 => {
            if i.lk() {
                "bctrl".into()
            } else {
                "bctr".into()
            }
        }
        Op::Bclr | Op::Bcctr => format!("{n}{} {},{}", if i.lk() { "l" } else { "" }, i.bo(), i.bi()),
        Op::Addi if ra == 0 => format!("li r{rd}, {}", i.simm()),
        Op::Addis if ra == 0 => format!("lis r{rd}, {:#x}", i.simm() as u16),
        Op::Addi | Op::Addis | Op::Addic | Op::AddicDot | Op::Subfic | Op::Mulli => format!("{n} r{rd}, r{ra}, {}", i.simm()),
        Op::Ori | Op::Oris | Op::Xori | Op::Xoris | Op::AndiDot | Op::AndisDot => format!("{n} r{ra}, r{rd}, {:#x}", i.uimm()),
        Op::Cmpi => format!("cmpwi cr{}, r{ra}, {}", i.crfd(), i.simm()),
        Op::Cmpli => format!("cmplwi cr{}, r{ra}, {:#x}", i.crfd(), i.uimm()),
        Op::Cmp => format!("cmpw cr{}, r{ra}, r{rb}", i.crfd()),
        Op::Cmpl => format!("cmplw cr{}, r{ra}, r{rb}", i.crfd()),
        Op::Rlwinm | Op::Rlwimi => format!("{n}{dot} r{ra}, r{rd}, {}, {}, {}", i.sh(), i.mb(), i.me()),
        Op::Rlwnm => format!("{n}{dot} r{ra}, r{rd}, r{rb}, {}, {}", i.mb(), i.me()),
        Op::Mfspr => format!("mfspr r{rd}, {}", spr_name(spr(word)).map_or(spr(word).to_string(), str::to_string)),
        Op::Mtspr => format!("mtspr {}, r{rd}", spr_name(spr(word)).map_or(spr(word).to_string(), str::to_string)),
        _ => match i.op.class() {
            Class::Load | Class::Store | Class::FloatMem if (word >> 26) != 31 && (word >> 26) != 4 && (word >> 26) < 56 => {
                let reg = if i.op.class() == Class::FloatMem { 'f' } else { 'r' };
                format!("{n} {reg}{rd}, {}(r{ra})", i.simm())
            }
            Class::Load | Class::Store | Class::FloatMem => {
                let reg = if i.op.class() == Class::FloatMem { 'f' } else { 'r' };
                format!("{n} {reg}{rd}, r{ra}, r{rb}")
            }
            // the logical and shift forms put their result in rA and take rS from the first field
            Class::Logical => format!("{n}{dot} r{ra}, r{rd}, r{rb}"),
            Class::Integer if matches!(i.op, Op::Extsb | Op::Extsh | Op::Cntlzw) => format!("{n}{dot} r{ra}, r{rd}"),
            Class::Integer => format!("{n}{dot} r{rd}, r{ra}, r{rb}"),
            _ => format!("{n}{dot} {rd}, {ra}, {rb}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(word: u32) -> &'static str {
        decode(word).map_or("?", |d| d.name)
    }

    #[test]
    fn well_known_words_decode_to_the_instructions_they_are() {
        assert_eq!(name(0x4e80_0020), "bclr", "blr");
        assert_eq!(name(0x7c08_02a6), "mfspr", "mflr r0");
        assert_eq!(name(0x7c08_03a6), "mtspr", "mtlr r0");
        assert_eq!(name(0x9421_ffe0), "stwu", "stwu r1,-32(r1)");
        assert_eq!(name(0x3860_0001), "addi", "li r3,1");
        assert_eq!(name(0x6000_0000), "ori", "nop");
        assert_eq!(name(0x8003_0000), "lwz");
        assert_eq!(name(0xc003_0000), "lfs");
        assert_eq!(name(0x4800_0001), "b", "bl");
        assert_eq!(name(0x4e80_0420), "bcctr", "bctr");
        assert_eq!(name(0x7c00_04ac), "sync");
        assert_eq!(name(0x7c00_0378), "or", "mr r0,r0");
        assert_eq!(name(0x5460_063e), "rlwinm");
        assert_eq!(name(0x7c00_0214), "add");
        assert_eq!(name(0x7c00_0a14), "add", "addo is the same instruction with the overflow bit");
        assert_eq!(name(0xec00_182a), "fadds");
        assert_eq!(name(0xfc00_0050), "fneg");
        assert_eq!(name(0x1000_0420), "ps_merge00", "paired singles");
        assert_eq!(name(0x1000_002a), "ps_add", "paired singles, the five-bit forms");
        assert_eq!(name(0xe000_0000), "psq_l");
    }

    #[test]
    fn words_that_are_no_instruction_are_not_guessed() {
        assert_eq!(decode(0x0000_0000), None, "zero is not an instruction");
        assert_eq!(decode(0x0400_0000), None, "opcode 1");
        assert_eq!(decode(0x7c00_0002), None, "opcode 31 with an extended opcode nobody has");
    }

    #[test]
    fn the_special_register_number_has_its_halves_swapped_back() {
        assert_eq!(spr(0x7c08_02a6), 8, "LR");
        assert_eq!(spr(0x7c09_03a6), 9, "CTR");
        assert_eq!(spr_name(8), Some("LR"));
        assert_eq!(spr_name(912), Some("GQR"));
        // GQR0 is spr 912, which is written with its halves swapped
        assert_eq!(spr(0x7c10_e3a6), 912);
        assert_eq!(spr_name(spr(0x7c10_e3a6)), Some("GQR"));
    }
}
