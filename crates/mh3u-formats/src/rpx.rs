//! Wii U `.rpx` executables: a big-endian ELF32 whose sections may be zlib-compressed.
//!
//! [`section_at`] gives the contents of the section loaded at an address. [`sections`] lists them all (with their names and kinds) and
//! [`imports`] names every function and piece of data the executable takes from the system libraries, which is the size of the
//! runtime a port would owe.

use anyhow::{Context, Result, bail};

/// Section flag marking zlib compression; the stream is preceded by the u32 uncompressed size.
const SHF_RPL_ZLIB: u32 = 0x0800_0000;
const SHT_NOBITS: u32 = 8;
const SHT_SYMTAB: u32 = 2;
/// The kinds of section only RPL files have.
pub const SHT_RPL_EXPORTS: u32 = 0x8000_0001;
pub const SHT_RPL_IMPORTS: u32 = 0x8000_0002;
pub const SHT_RPL_CRCS: u32 = 0x8000_0003;
pub const SHT_RPL_FILEINFO: u32 = 0x8000_0004;
/// The size of one entry of the section table, and of one symbol.
const SECTION_HEADER: usize = 40;
const SYMBOL: usize = 16;

fn be16(d: &[u8], o: usize) -> Option<usize> {
    Some(u16::from_be_bytes(d.get(o..o.checked_add(2)?)?.try_into().ok()?) as usize)
}

fn be32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(o..o.checked_add(4)?)?.try_into().ok()?))
}

/// One entry of the section table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub index: usize,
    pub name: String,
    /// Where the name is in the section-name table.
    pub name_offset: usize,
    pub kind: u32,
    pub flags: u32,
    /// Where the section is loaded.
    pub addr: u32,
    /// Where its bytes are in the file, and how many (compressed, when `compressed`).
    pub offset: usize,
    pub size: usize,
    pub link: u32,
    pub info: u32,
    pub entry_size: u32,
}

impl Section {
    pub fn compressed(&self) -> bool {
        self.flags & SHF_RPL_ZLIB != 0
    }

    /// The name of the section's kind.
    pub fn kind_name(&self) -> &'static str {
        match self.kind {
            0 => "null",
            1 => "progbits",
            SHT_SYMTAB => "symtab",
            3 => "strtab",
            4 => "rela",
            SHT_NOBITS => "nobits",
            SHT_RPL_EXPORTS => "rpl-exports",
            SHT_RPL_IMPORTS => "rpl-imports",
            SHT_RPL_CRCS => "rpl-crcs",
            SHT_RPL_FILEINFO => "rpl-fileinfo",
            _ => "other",
        }
    }
}

/// The section table's place in the file.
fn table(rpx: &[u8]) -> Result<(usize, usize, usize, usize)> {
    if rpx.len() < 0x34 || &rpx[..4] != b"\x7fELF" {
        bail!("not an ELF/RPX file");
    }
    let truncated = || "truncated section table";
    let at = be32(rpx, 0x20).context(truncated())? as usize;
    let entry_len = be16(rpx, 0x2e).context(truncated())?;
    let count = be16(rpx, 0x30).context(truncated())?;
    let names = be16(rpx, 0x32).context(truncated())?;
    if entry_len < SECTION_HEADER {
        bail!("section entries of {entry_len} bytes");
    }
    Ok((at, entry_len, count, names))
}

/// The header of section `i`, without its name.
fn header(rpx: &[u8], i: usize) -> Result<Section> {
    let (table, entry_len, count, _) = table(rpx)?;
    if i >= count {
        bail!("no section {i}");
    }
    let truncated = || "truncated section table";
    let h = i
        .checked_mul(entry_len)
        .and_then(|n| n.checked_add(table))
        .context("section table out of bounds")?;
    let bytes = rpx.get(h..h.saturating_add(SECTION_HEADER)).context(truncated())?;
    let field = |o: usize| be32(bytes, o).context(truncated());
    Ok(Section {
        index: i,
        name: String::new(),
        name_offset: field(0)? as usize,
        kind: field(4)?,
        flags: field(8)?,
        addr: field(12)?,
        offset: field(16)? as usize,
        size: field(20)? as usize,
        link: field(24)?,
        info: field(28)?,
        entry_size: field(36)?,
    })
}

/// The text of a string table at `offset` of the section's contents.
fn cstr(table: &[u8], offset: usize) -> String {
    let rest = table.get(offset..).unwrap_or(&[]);
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    String::from_utf8_lossy(&rest[..end]).into_owned()
}

/// Every section, with its name. A name that cannot be read is left empty.
pub fn sections(rpx: &[u8]) -> Result<Vec<Section>> {
    let (_, _, count, names) = table(rpx)?;
    let mut all: Vec<Section> = (0..count).map(|i| header(rpx, i)).collect::<Result<_>>()?;
    let strings = all.get(names).filter(|s| s.kind != SHT_NOBITS).and_then(|s| contents(rpx, s).ok());
    if let Some(strings) = strings {
        for s in &mut all {
            s.name = cstr(&strings, s.name_offset);
        }
    }
    Ok(all)
}

/// A section's contents, decompressed if it is.
pub fn contents(rpx: &[u8], section: &Section) -> Result<Vec<u8>> {
    if section.kind == SHT_NOBITS {
        return Ok(Vec::new());
    }
    let raw = rpx
        .get(section.offset..section.offset.saturating_add(section.size))
        .context("section data out of bounds")?;
    if !section.compressed() {
        return Ok(raw.to_vec());
    }
    let declared = be32(raw, 0).context("compressed section too short")? as usize;
    crate::inflate::inflate(&raw[4..], declared).context("inflating section")
}

/// Return the decompressed contents of the section loaded at virtual address `addr`. Every number in the file is checked: a
/// truncated or made-up file is an error, never a panic.
pub fn section_at(rpx: &[u8], addr: u32) -> Result<Vec<u8>> {
    let (_, _, count, _) = table(rpx)?;
    for i in 0..count {
        let s = header(rpx, i)?;
        if s.addr != addr || s.kind == SHT_NOBITS {
            continue;
        }
        return contents(rpx, &s);
    }
    bail!("no section at address {addr:#x}")
}

/// Whether an import is a function or a piece of data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImportKind {
    Function,
    Data,
}

/// One thing the executable takes from a system library.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Import {
    /// The library, for example `coreinit` or `gx2`.
    pub library: String,
    pub name: String,
    pub kind: ImportKind,
}

/// Every function and piece of data the executable imports, by library. Found through the symbol table: a symbol that lives in a section
/// called `.fimport_<library>` is a function of that library, one in `.dimport_<library>` is data.
pub fn imports(rpx: &[u8]) -> Result<Vec<Import>> {
    let all = sections(rpx)?;
    let mut out = Vec::new();
    for symtab in all.iter().filter(|s| s.kind == SHT_SYMTAB) {
        let data = contents(rpx, symtab)?;
        let strtab = all
            .get(symtab.link as usize)
            .context("symbol table without strings")
            .and_then(|s| contents(rpx, s))?;
        for sym in data.as_chunks::<SYMBOL>().0 {
            let (name, shndx) = (be32(sym, 0).unwrap_or(0) as usize, be16(sym, 14).unwrap_or(0));
            let Some(home) = all.get(shndx) else { continue };
            // a section's own symbol is named after it (`.fimport_gx2`) and is not an import
            let (kind, library) = if let Some(l) = home.name.strip_prefix(".fimport_") {
                (ImportKind::Function, l)
            } else if let Some(l) = home.name.strip_prefix(".dimport_") {
                (ImportKind::Data, l)
            } else {
                continue;
            };
            let name = cstr(&strtab, name);
            if name.is_empty() || name.starts_with('.') {
                continue;
            }
            out.push(Import {
                library: library.to_string(),
                name,
                kind,
            });
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Every imported function with the address its symbol gives (the address its calls go to).
pub fn import_addresses(rpx: &[u8]) -> Result<Vec<(Import, u32)>> {
    let all = sections(rpx)?;
    let mut out = Vec::new();
    for symtab in all.iter().filter(|s| s.kind == SHT_SYMTAB) {
        let data = contents(rpx, symtab)?;
        let strtab = all
            .get(symtab.link as usize)
            .context("symbol table without strings")
            .and_then(|s| contents(rpx, s))?;
        for sym in data.as_chunks::<SYMBOL>().0 {
            let (name, value, shndx) = (
                be32(sym, 0).unwrap_or(0) as usize,
                be32(sym, 4).unwrap_or(0),
                be16(sym, 14).unwrap_or(0),
            );
            let Some(home) = all.get(shndx) else { continue };
            let Some(library) = home.name.strip_prefix(".fimport_") else {
                continue;
            };
            let name = cstr(&strtab, name);
            if name.is_empty() || name.starts_with('.') {
                continue;
            }
            let import = Import {
                library: library.to_string(),
                name,
                kind: ImportKind::Function,
            };
            out.push((import, value));
        }
    }
    out.sort();
    Ok(out)
}

/// The places in the code that name an import: the address of the instruction and the import (found through the relocations of
/// `.text`, whose symbols are the imports).
pub fn import_sites(rpx: &[u8]) -> Result<Vec<(u32, Import)>> {
    let all = sections(rpx)?;
    let Some(symtab) = all.iter().find(|s| s.kind == SHT_SYMTAB) else {
        return Ok(Vec::new());
    };
    let symbols = contents(rpx, symtab)?;
    let strtab = all
        .get(symtab.link as usize)
        .context("symbol table without strings")
        .and_then(|s| contents(rpx, s))?;
    let mut out = Vec::new();
    for rela in all.iter().filter(|s| s.name == ".rela.text") {
        for entry in contents(rpx, rela)?.as_chunks::<12>().0 {
            let (site, info) = (be32(entry, 0).unwrap_or(0), be32(entry, 4).unwrap_or(0));
            let Some(sym) = symbols.as_chunks::<SYMBOL>().0.get((info >> 8) as usize) else {
                continue;
            };
            let Some(home) = all.get(be16(sym, 14).unwrap_or(0)) else {
                continue;
            };
            let Some(library) = home.name.strip_prefix(".fimport_") else {
                continue;
            };
            let name = cstr(&strtab, be32(sym, 0).unwrap_or(0) as usize);
            if name.is_empty() || name.starts_with('.') {
                continue;
            }
            out.push((
                site,
                Import {
                    library: library.to_string(),
                    name,
                    kind: ImportKind::Function,
                },
            ));
        }
    }
    Ok(out)
}

/// An executable made up for the tests: an import section for the library `gx2`, one for data of `coreinit`, and a symbol table
/// naming two functions and one piece of data (and the sections' own symbols, which are not imports). Nothing in it is compressed.
#[cfg(test)]
pub(crate) fn sample_with_imports() -> Vec<u8> {
    let names: [&[u8]; 6] = [b"", b".shstrtab", b".fimport_gx2", b".dimport_coreinit", b".symtab", b".strtab"];
    let mut shstr = Vec::new();
    let at: Vec<u32> = names
        .iter()
        .map(|n| {
            let o = shstr.len() as u32;
            shstr.extend(*n);
            shstr.push(0);
            o
        })
        .collect();
    // symbols: (name, section)
    let symbols: [(&[u8], u16); 5] = [
        (b"", 0),
        (b".fimport_gx2", 2),
        (b"GX2Init", 2),
        (b"GX2Flush", 2),
        (b"OSSomething", 3),
    ];
    let mut strtab = vec![0u8];
    let mut symtab = vec![0u8; SYMBOL];
    for (name, sec) in symbols.iter().skip(1) {
        let o = strtab.len() as u32;
        strtab.extend(*name);
        strtab.push(0);
        let mut sym = vec![0u8; SYMBOL];
        sym[..4].copy_from_slice(&o.to_be_bytes());
        sym[14..16].copy_from_slice(&sec.to_be_bytes());
        symtab.extend(sym);
    }
    // section 0 null, 1 shstrtab, 2 .fimport_gx2, 3 .dimport_coreinit, 4 symtab (strings: 5), 5 strtab
    let bodies: [(u32, &[u8], u32); 6] = [
        (0, b"", 0),
        (3, &shstr, 0),
        (SHT_RPL_IMPORTS, &[0u8; 8], 0),
        (SHT_RPL_IMPORTS, &[0u8; 8], 0),
        (SHT_SYMTAB, &symtab, 5),
        (3, &strtab, 0),
    ];
    let table = 0x40usize;
    let mut data_at = table + bodies.len() * SECTION_HEADER;
    let mut d = vec![0u8; data_at];
    d[..4].copy_from_slice(b"\x7fELF");
    d[0x20..0x24].copy_from_slice(&(table as u32).to_be_bytes());
    d[0x2e..0x30].copy_from_slice(&(SECTION_HEADER as u16).to_be_bytes());
    d[0x30..0x32].copy_from_slice(&(bodies.len() as u16).to_be_bytes());
    d[0x32..0x34].copy_from_slice(&1u16.to_be_bytes());
    for (i, (kind, body, link)) in bodies.iter().enumerate() {
        let h = table + i * SECTION_HEADER;
        d[h..h + 4].copy_from_slice(&at[i].to_be_bytes());
        d[h + 4..h + 8].copy_from_slice(&kind.to_be_bytes());
        d[h + 16..h + 20].copy_from_slice(&(data_at as u32).to_be_bytes());
        d[h + 20..h + 24].copy_from_slice(&(body.len() as u32).to_be_bytes());
        d[h + 24..h + 28].copy_from_slice(&link.to_be_bytes());
        d.extend(*body);
        data_at += body.len();
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_are_listed_with_their_names_and_kinds() {
        let all = sections(&sample_with_imports()).unwrap();
        let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["", ".shstrtab", ".fimport_gx2", ".dimport_coreinit", ".symtab", ".strtab"]);
        assert_eq!(all[2].kind_name(), "rpl-imports");
        assert_eq!(all[4].kind_name(), "symtab");
        assert!(sections(b"nope").is_err());
    }

    #[test]
    fn imports_are_the_symbols_that_live_in_import_sections() {
        let found = imports(&sample_with_imports()).unwrap();
        let shown: Vec<(&str, &str, ImportKind)> = found.iter().map(|i| (i.library.as_str(), i.name.as_str(), i.kind)).collect();
        assert_eq!(
            shown,
            [
                ("coreinit", "OSSomething", ImportKind::Data),
                ("gx2", "GX2Flush", ImportKind::Function),
                ("gx2", "GX2Init", ImportKind::Function),
            ]
        );
    }
}
