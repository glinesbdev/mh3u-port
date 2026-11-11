//! MT Framework `.arc` archives as shipped on Wii U (big-endian, zlib-compressed entries).
//!
//! Layout: 12-byte header (`\0CRA`, u16 version, u16 count, 4 pad bytes) followed by
//! `count` 80-byte entries: name[64], type hash, compressed size, decompressed size, offset.
//!
//! Bits 31..29 of either size word are flags, not length ([`SIZE_MASK`] clears them). The
//! compressed length is the low 29 bits. The decompressed length is different: shift the word
//! right by 3, then apply that same mask (`(field >> 3) & (SIZE_MASK >> 3)`). The length
//! therefore lives in bits 28..3. The low 3 bits are flags/quality (MH3U stores 2) and are
//! not part of the length.

use anyhow::{Context, Result, bail};

const MAGIC: &[u8; 4] = b"\0CRA";
const HEADER_LEN: usize = 12;
const ENTRY_LEN: usize = 80;
const NAME_LEN: usize = 64;
/// Clears bits 31..29, which are flags on both size words.
const SIZE_MASK: u32 = 0x1fff_ffff;
/// Decompressed length starts at bit 3. Bits 2..0 are flags/quality, not length.
const DECOMPRESSED_SIZE_SHIFT: u32 = 3;

/// Byte length from a decompressed-size word: high flags cleared after the shift.
fn decompressed_size(field: u32) -> u32 {
    (field >> DECOMPRESSED_SIZE_SHIFT) & (SIZE_MASK >> DECOMPRESSED_SIZE_SHIFT)
}

#[derive(Debug, Clone)]
pub struct ArcEntry {
    /// Path inside the archive, with `\` separators and no extension.
    pub name: String,
    /// Hash identifying the resource type (stands in for a file extension).
    pub type_hash: u32,
    /// Compressed length in bytes (low 29 bits of the on-disk word).
    pub compressed_size: u32,
    /// Decompressed length in bytes (bits 28..3 of the on-disk word).
    pub size: u32,
    pub offset: u32,
}

pub struct Arc<'a> {
    data: &'a [u8],
    pub entries: Vec<ArcEntry>,
}

fn be32(d: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

impl<'a> Arc<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Arc<'a>> {
        if data.len() < HEADER_LEN || &data[..4] != MAGIC {
            bail!("not a big-endian ARC archive");
        }
        let count = u16::from_be_bytes([data[6], data[7]]) as usize;
        if data.len() < HEADER_LEN + count * ENTRY_LEN {
            bail!("truncated entry table ({count} entries)");
        }
        let entries = (0..count)
            .map(|i| {
                let o = HEADER_LEN + i * ENTRY_LEN;
                let raw = &data[o..o + NAME_LEN];
                let end = raw.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
                ArcEntry {
                    name: String::from_utf8_lossy(&raw[..end]).into_owned(),
                    type_hash: be32(data, o + NAME_LEN),
                    compressed_size: be32(data, o + NAME_LEN + 4) & SIZE_MASK,
                    size: decompressed_size(be32(data, o + NAME_LEN + 8)),
                    offset: be32(data, o + NAME_LEN + 12),
                }
            })
            .collect();
        Ok(Arc { data, entries })
    }

    /// Decompress one entry's contents.
    pub fn read(&self, e: &ArcEntry) -> Result<Vec<u8>> {
        let start = e.offset as usize;
        let end = start.saturating_add(e.compressed_size as usize);
        let raw = self
            .data
            .get(start..end)
            .with_context(|| format!("{}: data out of bounds", e.name))?;
        if e.compressed_size == e.size {
            return Ok(raw.to_vec()); // stored uncompressed
        }
        crate::companion::inflate::inflate(raw, e.size as usize).with_context(|| format!("{}: inflate failed", e.name))
    }
}
