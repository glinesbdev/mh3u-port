//! MT Framework `.gmd` message files (big-endian `\0DMG`): a header followed by a table of
//! NUL-separated UTF-8 strings. Item/equipment IDs index directly into that table.
//!
//! Header fields used (all u32 big-endian): 0x18 string count, 0x20 string-section byte length.
//! The string section is the last `section_len` bytes of the file.

use anyhow::{Result, bail};

const MAGIC: &[u8; 4] = b"\0DMG";
const COUNT_OFFSET: usize = 0x18;
const SECTION_LEN_OFFSET: usize = 0x20;

fn be32(d: &[u8], o: usize) -> usize {
    u32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]) as usize
}

/// Parse a GMD file into its list of strings, in file order.
pub fn parse(data: &[u8]) -> Result<Vec<String>> {
    if data.len() < SECTION_LEN_OFFSET + 4 || &data[..4] != MAGIC {
        bail!("not a big-endian GMD file");
    }
    let count = be32(data, COUNT_OFFSET);
    let section_len = be32(data, SECTION_LEN_OFFSET);
    if section_len > data.len() {
        bail!("string section ({section_len} bytes) exceeds file size {}", data.len());
    }
    let section = &data[data.len() - section_len..];
    let strings: Vec<String> = section
        .split(|&b| b == 0)
        .take(count)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    if strings.len() != count {
        bail!("expected {count} strings, found {}", strings.len());
    }
    Ok(strings)
}
