//! The Wii U's GPU ("Latte", an AMD R7xx design): reading the shader programs the game hands to GX2.
//!
//! A program is a list of control-flow instructions (8 bytes each) that point at *clauses*: runs of ALU instruction groups (up to five
//! operations issued together, with literal constants after the group) and of texture fetches (16 bytes each). Words are little-endian.
//! This reads the structure and counts what is in it ([`census`]); it does not run or translate anything. The format is as the
//! R6xx/R7xx instruction set documents give it, and every program of the game's pack parses to the end with it (see `docs/spikes.md`).

pub mod decode;
pub mod ops;
pub mod translate;

use std::collections::BTreeMap;

/// The control-flow instruction kinds a pixel shader of the game uses, by their number (bits 23 to 29 of the second word).
pub fn cf_name(inst: u32) -> &'static str {
    match inst {
        0x00 => "nop",
        0x01 => "tex-clause",
        0x02 => "vtx-clause",
        0x03 => "vtx-tc-clause",
        0x04 => "loop-start",
        0x05 => "loop-end",
        0x0a => "jump",
        0x0b => "push",
        0x0c => "push-else",
        0x0d => "else",
        0x0e => "pop",
        0x0f => "pop-jump",
        0x10 => "pop-push",
        0x13 => "call-fs",
        0x06 => "loop-start-dx10",
        0x08 => "loop-continue",
        0x09 => "loop-break",
        0x11 => "pop-push-else",
        0x12 => "call",
        0x14 => "return",
        0x28 => "export-done",
        0x27 => "export",
        _ => "other",
    }
}

/// The ALU operations whose numbers are certain; the rest are counted by number.
pub fn alu_name(op3: bool, op: u32) -> Option<&'static str> {
    if op3 {
        return match op {
            16 => Some("MULADD"),
            17 => Some("MULADD_M2"),
            18 => Some("MULADD_M4"),
            20 => Some("MULADD_IEEE"),
            28 => Some("CNDE_INT"),
            _ => None,
        };
    }
    match op {
        0 => Some("ADD"),
        1 => Some("MUL"),
        2 => Some("MUL_IEEE"),
        3 => Some("MAX"),
        4 => Some("MIN"),
        5 => Some("MAX_DX10"),
        25 => Some("MOV"),
        26 => Some("NOP"),
        80 => Some("DOT4"),
        81 => Some("DOT4_IEEE"),
        _ => None,
    }
}

/// What a program holds.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Census {
    pub programs: usize,
    /// How many bytes the program takes: its control flow and the furthest end of any clause it names (for one program; added up for
    /// many it is meaningless).
    pub extent: usize,
    /// Control-flow instructions by kind number.
    pub control_flow: BTreeMap<u32, usize>,
    pub alu_clauses: usize,
    pub tex_clauses: usize,
    /// Exports of results (`export` and `export-done`): position and parameters of a vertex shader, colours of a pixel shader.
    pub exports: usize,
    /// ALU instructions, and the groups (up to five issued together) they come in.
    pub alu_instructions: usize,
    pub groups: usize,
    /// Groups by the number of instructions in them (1 to 5).
    pub group_sizes: BTreeMap<usize, usize>,
    /// 64-bit slots of the ALU clauses: instructions and the literal constants after groups.
    pub slots: usize,
    pub literal_slots: usize,
    /// Two-operand and three-operand ALU operations by number.
    pub op2: BTreeMap<u32, usize>,
    pub op3: BTreeMap<u32, usize>,
    /// Texture fetch instructions by number.
    pub tex: BTreeMap<u32, usize>,
}

impl Census {
    pub fn add(&mut self, other: &Census) {
        self.programs += other.programs;
        self.extent += other.extent;
        self.exports += other.exports;
        self.alu_clauses += other.alu_clauses;
        self.tex_clauses += other.tex_clauses;
        self.alu_instructions += other.alu_instructions;
        self.groups += other.groups;
        self.slots += other.slots;
        self.literal_slots += other.literal_slots;
        for (dst, src) in [
            (&mut self.control_flow, &other.control_flow),
            (&mut self.op2, &other.op2),
            (&mut self.op3, &other.op3),
            (&mut self.tex, &other.tex),
        ] {
            for (k, n) in src {
                *dst.entry(*k).or_default() += n;
            }
        }
        for (k, n) in &other.group_sizes {
            *self.group_sizes.entry(*k).or_default() += n;
        }
    }
}

fn word(program: &[u8], index: usize) -> Option<u32> {
    let at = index.checked_mul(4)?;
    Some(u32::from_le_bytes(program.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

/// Walk a program: its control flow, then every clause it names, and count what is there. An error says what did not add up (a clause
/// that does not fit the program, a group that does not end, a program with no end), so a wrong reading of the format cannot pass.
pub fn census(program: &[u8]) -> Result<Census, String> {
    let mut out = Census {
        programs: 1,
        ..Census::default()
    };
    // (is an ALU clause, address in 64-bit units, how many slots or fetches)
    let mut clauses: Vec<(bool, usize, usize)> = Vec::new();
    let mut i = 0;
    loop {
        let (Some(w0), Some(w1)) = (word(program, 2 * i), word(program, 2 * i + 1)) else {
            return Err("control flow runs off the end of the program".to_string());
        };
        i += 1;
        let inst = (w1 >> 23) & 0x7f;
        if inst >= 0x40 {
            // an ALU clause: the instruction number is in bits 26 to 29 and the count of slots in bits 18 to 24
            *out.control_flow.entry(0x40 | ((w1 >> 26) & 0xf)).or_default() += 1;
            clauses.push((true, (w0 & 0x3f_ffff) as usize, ((w1 >> 18) & 0x7f) as usize + 1));
            out.alu_clauses += 1;
            continue;
        }
        if !(inst <= 0x1a || (0x27..=0x30).contains(&inst)) {
            return Err(format!("control flow instruction {inst:#x} is none of the known ones"));
        }
        *out.control_flow.entry(inst).or_default() += 1;
        if matches!(inst, 0x27 | 0x28) {
            out.exports += 1;
        }
        if matches!(inst, 1..=3) {
            let count = (((w1 >> 10) & 7) | (((w1 >> 19) & 1) << 3)) as usize + 1;
            clauses.push((false, w0 as usize, count));
            out.tex_clauses += 1;
        }
        if (w1 >> 21) & 1 != 0 {
            break; // the end of the program
        }
    }
    out.extent = i * 8;
    for (is_alu, address, count) in clauses {
        let mut at = address * 2; // in words
        if !is_alu {
            if at + count * 4 > program.len() / 4 {
                return Err("a texture clause does not fit the program".to_string());
            }
            for k in 0..count {
                *out.tex.entry(word(program, at + k * 4).unwrap_or(0) & 0x1f).or_default() += 1;
            }
            out.extent = out.extent.max((at + count * 4) * 4);
            continue;
        }
        let mut used = 0;
        while used < count {
            let mut size = 0;
            let mut literal_channels = None::<u32>;
            loop {
                let (Some(a0), Some(a1)) = (word(program, at), word(program, at + 1)) else {
                    return Err("an ALU group runs off the end of the program".to_string());
                };
                at += 2;
                used += 1;
                size += 1;
                let op3 = (a1 >> 13) & 0x1f >= 8;
                let mut sources = vec![(a0 & 0x1ff, (a0 >> 10) & 3), ((a0 >> 13) & 0x1ff, (a0 >> 23) & 3)];
                if op3 {
                    sources.push((a1 & 0x1ff, (a1 >> 10) & 3));
                    *out.op3.entry((a1 >> 13) & 0x1f).or_default() += 1;
                } else {
                    *out.op2.entry((a1 >> 7) & 0x7ff).or_default() += 1;
                }
                for (select, channel) in sources {
                    if select == 253 {
                        literal_channels = Some(literal_channels.map_or(channel, |m| m.max(channel)));
                    }
                }
                out.alu_instructions += 1;
                if a0 >> 31 != 0 {
                    break;
                }
                if size > 5 {
                    return Err("an ALU group of more than five instructions".to_string());
                }
            }
            // the literal constants follow the group, two to a slot
            let literal = literal_channels.map_or(0, |m| (m as usize + 2) / 2);
            at += literal * 2;
            used += literal;
            out.literal_slots += literal;
            out.groups += 1;
            *out.group_sizes.entry(size).or_default() += 1;
        }
        if used != count {
            return Err(format!("an ALU clause of {count} slots holds {used}"));
        }
        out.extent = out.extent.max(at * 4);
        out.slots += count;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[u32]) -> Vec<u8> {
        list.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// Control flow: an ALU clause of three slots at slot 2, then export-done and end. They hold a two-operand group of one
    /// instruction and a multiply-add group of one instruction with a literal (and the slot of constants after it).
    fn sample() -> Vec<u8> {
        words(&[
            // ALU clause: address 2 (after two control-flow slots), 3 slots (two instructions and one literal slot)
            2,
            (8 << 26) | (2 << 18),
            // export done, end of program
            0,
            (0x28 << 23) | (1 << 21),
            // MOV r0.x, r1.x (last in its group)
            1 << 16 | 1 << 31,
            25 << 7,
            // MULADD with a literal source (select 253, channel y), last in its group
            (253 << 13) | (1 << 10) | 1 << 31,
            16 << 13,
            // the literal slot: two constants
            0x3f80_0000,
            0x4000_0000,
        ])
    }

    #[test]
    fn a_made_up_program_is_counted_the_way_it_is_built() {
        let c = census(&sample()).unwrap();
        assert_eq!((c.alu_clauses, c.alu_instructions, c.groups), (1, 2, 2));
        assert_eq!(c.slots, 3);
        assert_eq!(c.extent, sample().len(), "the program is its control flow and its one clause");
        assert_eq!(c.literal_slots, 1, "a literal in channel y takes one slot of two constants");
        assert_eq!(c.op2.get(&25), Some(&1), "MOV");
        assert_eq!(c.op3.get(&16), Some(&1), "MULADD");
        assert_eq!(c.control_flow.get(&0x28), Some(&1));
        assert_eq!(alu_name(false, 25), Some("MOV"));
    }

    #[test]
    fn a_program_that_does_not_add_up_is_an_error() {
        assert!(census(&[]).is_err());
        // the clause claims more slots than the program has
        let mut bad = sample();
        bad[4..8].copy_from_slice(&((8u32 << 26) | (9 << 18)).to_le_bytes());
        assert!(census(&bad).is_err());
        // control flow that never ends
        assert!(census(&words(&[0, 0, 0, 0])).is_err());
        // a control flow instruction that is none of the known ones
        assert!(census(&words(&[0, 0x35 << 23, 0, 1 << 21])).is_err());
    }
}
