//! Wii U `.rpx` executables: a big-endian ELF32 whose sections may be zlib-compressed.

use anyhow::{Context, Result, bail};

/// Section flag marking zlib compression; the stream is preceded by the u32 uncompressed size.
const SHF_RPL_ZLIB: u32 = 0x0800_0000;
const SHT_NOBITS: u32 = 8;

fn be16(d: &[u8], o: usize) -> Option<usize> {
    Some(u16::from_be_bytes(d.get(o..o.checked_add(2)?)?.try_into().ok()?) as usize)
}

fn be32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(o..o.checked_add(4)?)?.try_into().ok()?))
}

/// Return the decompressed contents of the section loaded at virtual address `addr`. Every number in the file is checked: a
/// truncated or made-up file is an error, never a panic.
pub fn section_at(rpx: &[u8], addr: u32) -> Result<Vec<u8>> {
    if rpx.len() < 0x34 || &rpx[..4] != b"\x7fELF" {
        bail!("not an ELF/RPX file");
    }
    let truncated = || "truncated section table";
    let table = be32(rpx, 0x20).context(truncated())? as usize;
    let entry_len = be16(rpx, 0x2e).context(truncated())?;
    let count = be16(rpx, 0x30).context(truncated())?;
    for i in 0..count {
        let h = i
            .checked_mul(entry_len)
            .and_then(|n| n.checked_add(table))
            .context("section table out of bounds")?;
        let header = rpx.get(h..h.saturating_add(40)).context(truncated())?;
        let field = |o: usize| be32(header, o).context(truncated());
        let (kind, flags, sh_addr, offset, size) = (field(4)?, field(8)?, field(12)?, field(16)? as usize, field(20)? as usize);
        if sh_addr != addr || kind == SHT_NOBITS {
            continue;
        }
        let raw = rpx.get(offset..offset.saturating_add(size)).context("section data out of bounds")?;
        if flags & SHF_RPL_ZLIB == 0 {
            return Ok(raw.to_vec());
        }
        let declared = be32(raw, 0).context("compressed section too short")? as usize;
        return crate::companion::inflate::inflate(&raw[4..], declared).context("inflating section");
    }
    bail!("no section at address {addr:#x}")
}
