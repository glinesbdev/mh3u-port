//! Explorers for the game's files, for the port's groundwork. They read the owner's own dump (`--game-dir`, or the `MH3U_GAME_DIR`
//! environment variable) and print; they change nothing.

mod build_game;
mod codegen_bench;
mod latte_check;
#[cfg(target_os = "linux")]
mod shader_scan;
mod translate;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use mh3u_dump::{Dump, expand_home};
use mh3u_formats::{arc::Arc, gmd, rpx};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Tool,
}

#[derive(Subcommand)]
enum Tool {
    /// Check that a folder holds the game, and say where its files are.
    Dump { game_dir: PathBuf },
    /// List the sections of the game's executable.
    Sections { game_dir: PathBuf },
    /// Find where the functions of the executable start (calls, code pointers in the data, the entry point) and say how many.
    Functions { game_dir: PathBuf },
    /// Decode every shader program of the game's pack and count what they use.
    LatteCheck { game_dir: PathBuf },
    /// Translate every pixel program of the pack to WGSL and validate it.
    LatteWgsl {
        game_dir: PathBuf,
        /// Print the translation of this shader of the pack.
        #[arg(long)]
        show: Option<usize>,
    },
    /// Print instructions of the code with their fields (for finding out what the code is doing; not a full disassembler).
    Disasm {
        game_dir: PathBuf,
        /// The address to start at, in hex.
        start: String,
        #[arg(default_value_t = 24)]
        count: u32,
    },
    /// Compile the C that `translate` wrote into a library, OUT/libgame.so (or .dll), using every core.
    BuildGame {
        c_dir: PathBuf,
        /// The optimisation level of the C compiler (0 builds fastest).
        #[arg(long, default_value_t = 1)]
        opt: u8,
    },
    /// Run the game's entry point from the library built by `build-game`, and list the system functions it calls.
    Run {
        game_dir: PathBuf,
        c_dir: PathBuf,
        /// Where the game's saves go.
        #[arg(long)]
        save_dir: Option<PathBuf>,
        /// Open a window and draw into it.
        #[arg(long)]
        window: bool,
        /// How many system calls to allow before stopping.
        #[arg(long, default_value_t = 100_000)]
        max_imports: u64,
    },
    /// Translate the whole executable into C files in OUT (which must not be inside a git working tree).
    Translate {
        game_dir: PathBuf,
        out: PathBuf,
        /// Functions per generated file.
        #[arg(long, default_value_t = 200)]
        per_file: usize,
    },
    /// Count the instructions of the executable's code by kind, and the ones that are not known.
    Coverage {
        game_dir: PathBuf,
        /// How many unknown words to show.
        #[arg(long, default_value_t = 20)]
        unknown: usize,
    },
    /// Start Cemu on the game and, when a scene is up, look for shader programs in its memory (finds the vertex shaders, which are in no
    /// file). Linux only. What it finds is made from the game and is refused inside a git working tree.
    ScanShaders {
        game_dir: PathBuf,
        /// Where to write the programs found (outside any repository).
        out: PathBuf,
        /// The Cemu program.
        #[arg(long, default_value = "Cemu")]
        cemu: String,
    },
    /// Count what is in shader program files (the ones `scan-shaders` writes), added up.
    ProgramCensus { files: Vec<PathBuf> },
    /// Count what is in the game's shaders (the GPU's instruction census, spike 3).
    Shaders { game_dir: PathBuf },
    /// Spike 4: translate a sample of the game's functions into Rust or C, only to time how long that takes to compile. The output is
    /// made from the game and is refused inside a git working tree.
    CodegenBench {
        game_dir: PathBuf,
        /// Where to write the generated project (outside any repository).
        out: PathBuf,
        /// `rust` or `c`.
        #[arg(long, default_value = "rust")]
        lang: String,
        /// How many functions to translate.
        #[arg(long, default_value_t = 2000)]
        functions: usize,
        /// Functions per generated file.
        #[arg(long, default_value_t = 100)]
        per_file: usize,
    },
    /// Write one section of the executable (unpacked) to a file, by name (`.text`, `.rodata`, `.data`).
    Section { game_dir: PathBuf, name: String, out: PathBuf },
    /// List what the executable imports from the system libraries, by library (the size of the runtime a port would owe).
    Imports {
        game_dir: PathBuf,
        /// Print every name, not only the counts.
        #[arg(long)]
        names: bool,
    },
    /// List the entries of an archive.
    Arcls { arc: PathBuf },
    /// Extract every entry of an archive as OUTDIR/NAME.TYPEHASH.
    Arcx { arc: PathBuf, outdir: PathBuf },
    /// Print every string of a text table, or only the given indexes.
    Gmd { file: PathBuf, ids: Vec<usize> },
    /// Find byte patterns (hex) in every decompressed archive entry under a folder.
    Arcsearch {
        dir: PathBuf,
        #[arg(required = true)]
        patterns: Vec<String>,
    },
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| path.display().to_string())
}

fn dump(game_dir: &Path) -> Result<Dump> {
    Dump::locate(&expand_home(&game_dir.to_string_lossy()))
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Tool::Dump { game_dir } => {
            let d = dump(&game_dir)?;
            println!("game:       {}", d.base.display());
            match &d.update {
                Some(u) => println!("update:     {}", u.display()),
                None => println!("update:     none found beside the game"),
            }
            println!("executable: {}", d.executable()?.display());
            Ok(())
        }
        Tool::Sections { game_dir } => sections(&dump(&game_dir)?),
        Tool::Section { game_dir, name, out } => {
            let data = read(&dump(&game_dir)?.executable()?)?;
            let all = rpx::sections(&data)?;
            let section = all
                .iter()
                .find(|s| s.name == name)
                .with_context(|| format!("no section called {name}"))?;
            std::fs::write(&out, rpx::contents(&data, section)?)?;
            println!("wrote {name}");
            Ok(())
        }
        Tool::CodegenBench {
            game_dir,
            out,
            lang,
            functions,
            per_file,
        } => {
            let data = read(&dump(&game_dir)?.executable()?)?;
            let text = rpx::section_at(&data, TEXT_ADDRESS)?;
            let words: Vec<u32> = text.as_chunks::<4>().0.iter().map(|w| u32::from_be_bytes(*w)).collect();
            let lang = match lang.as_str() {
                "rust" => codegen_bench::Lang::Rust,
                "c" => codegen_bench::Lang::C,
                other => anyhow::bail!("--lang is rust or c, not {other}"),
            };
            let r = codegen_bench::run(&words, TEXT_ADDRESS, functions, lang, &out, per_file)?;
            println!(
                "{} functions, {} instructions ({} placeholders), {} lines, {} bytes, {} files",
                r.functions, r.instructions, r.placeholders, r.lines, r.bytes, r.files
            );
            Ok(())
        }
        Tool::ScanShaders { game_dir, out, cemu } => scan_shaders(&dump(&game_dir)?, &cemu, &out),
        Tool::ProgramCensus { files } => {
            let mut total = mh3u_latte::Census::default();
            for f in &files {
                total.add(&mh3u_latte::census(&read(f)?).map_err(|e| anyhow::anyhow!("{}: {e}", f.display()))?);
            }
            print_census(&total);
            Ok(())
        }
        Tool::Shaders { game_dir } => shaders(&dump(&game_dir)?),
        Tool::Translate { game_dir, out, per_file } => {
            let data = read(&dump(&game_dir)?.executable()?)?;
            let text = rpx::section_at(&data, TEXT_ADDRESS)?;
            let mut words: Vec<u32> = text.as_chunks::<4>().0.iter().map(|w| u32::from_be_bytes(*w)).collect();
            let entry = u32::from_be_bytes(data.get(0x18..0x1c).context("short file")?.try_into()?);
            let mut pointers = Vec::new();
            for s in rpx::sections(&data)? {
                if matches!(s.name.as_str(), ".rodata" | ".data") {
                    let body = rpx::contents(&data, &s)?;
                    pointers.extend(body.as_chunks::<4>().0.iter().map(|w| u32::from_be_bytes(*w)));
                }
            }
            let imports = import_targets(&data, &text)?;
            patch_import_addresses(&data, &mut words, &imports)?;
            let end = TEXT_ADDRESS + 4 * words.len() as u32;
            pointers.extend(rpx::relocation_targets(&data, TEXT_ADDRESS, end)?);
            let r = translate::run(
                &translate::Input {
                    words: &words,
                    base: TEXT_ADDRESS,
                    pointers: &pointers,
                    entry,
                    imports: &imports.names,
                    targets: &imports.targets,
                },
                &out,
                per_file,
            )?;
            println!("{} functions, {} files, {} bytes", r.functions, r.files, r.bytes);
            Ok(())
        }
        Tool::BuildGame { c_dir, opt } => build_game::run(&c_dir, opt),
        Tool::Run {
            game_dir,
            c_dir,
            save_dir,
            window,
            max_imports,
        } => {
            let save = save_dir.unwrap_or_else(|| c_dir.join("save"));
            run_game(&dump(&game_dir)?, &c_dir, max_imports, save, window)
        }
        Tool::Disasm { game_dir, start, count } => {
            let data = read(&dump(&game_dir)?.executable()?)?;
            let text = rpx::section_at(&data, TEXT_ADDRESS)?;
            let start = u32::from_str_radix(start.trim_start_matches("0x"), 16)?;
            for k in 0..count {
                let at = start + 4 * k;
                let o = at.wrapping_sub(TEXT_ADDRESS) as usize;
                let Some(w) = text.get(o..o + 4) else { break };
                let w = u32::from_be_bytes(w.try_into()?);
                let line = match mh3u_ppc::Insn::decode(w) {
                    Some(i) => format!(
                        "{:<10} rd/rs={} ra={} rb={} simm={} uimm={:#x} bo={} bi={}",
                        i.op.name(),
                        i.rd(),
                        i.ra(),
                        i.rb(),
                        i.simm(),
                        i.uimm(),
                        i.bo(),
                        i.bi()
                    ),
                    None => "?".to_string(),
                };
                println!("{at:#x}  {w:08x}  {line}");
            }
            Ok(())
        }
        Tool::LatteWgsl { game_dir, show } => {
            let dump = dump(&game_dir)?;
            let path = dump.content("nativeCafe/shaderArc.arc").context("no shaderArc.arc in the dump")?;
            let bytes = read(&path)?;
            let arc = Arc::parse(&bytes)?;
            let entry = arc.entries.iter().find(|e| e.name == "shaderPack").context("no shaderPack")?;
            latte_check::wgsl(&arc.read(entry)?, show)
        }
        Tool::LatteCheck { game_dir } => {
            let dump = dump(&game_dir)?;
            let path = dump.content("nativeCafe/shaderArc.arc").context("no shaderArc.arc in the dump")?;
            let bytes = read(&path)?;
            let arc = Arc::parse(&bytes)?;
            let entry = arc.entries.iter().find(|e| e.name == "shaderPack").context("no shaderPack")?;
            latte_check::run(&arc.read(entry)?)
        }
        Tool::Functions { game_dir } => functions(&dump(&game_dir)?),
        Tool::Coverage { game_dir, unknown } => coverage(&dump(&game_dir)?, unknown),
        Tool::Imports { game_dir, names } => imports(&dump(&game_dir)?, names),
        Tool::Arcls { arc } => {
            for e in Arc::parse(&read(&arc)?)?.entries {
                println!("{:08x} {:>9} {:>9}  {}", e.type_hash, e.compressed_size, e.size, e.name);
            }
            Ok(())
        }
        Tool::Arcx { arc, outdir } => {
            let data = read(&arc)?;
            let arc = Arc::parse(&data)?;
            for e in &arc.entries {
                let path = outdir.join(format!("{}.{:08x}", e.name.replace('\\', "/"), e.type_hash));
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, arc.read(e)?)?;
            }
            println!("extracted {} entries", arc.entries.len());
            Ok(())
        }
        Tool::Gmd { file, ids } => {
            let strings = gmd::parse(&read(&file)?)?;
            if ids.is_empty() {
                for (i, s) in strings.iter().enumerate() {
                    println!("{i:5} {s}");
                }
            } else {
                for &i in &ids {
                    println!("{i:5} {}", strings.get(i).map_or("<out of range>", String::as_str));
                }
            }
            Ok(())
        }
        Tool::Arcsearch { dir, patterns } => arcsearch(&dir, &patterns),
    }
}

fn sections(dump: &Dump) -> Result<()> {
    let data = read(&dump.executable()?)?;
    println!(
        "{:>3} {:<22} {:<12} {:>10} {:>10} {:>10}  flags",
        "#", "name", "kind", "address", "file", "bytes"
    );
    for s in rpx::sections(&data)? {
        println!(
            "{:>3} {:<22} {:<12} {:>#10x} {:>#10x} {:>10}  {:#010x}{}",
            s.index,
            s.name,
            s.kind_name(),
            s.addr,
            s.offset,
            s.size,
            s.flags,
            if s.compressed() { " zlib" } else { "" }
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn scan_shaders(dump: &Dump, cemu: &str, out: &Path) -> Result<()> {
    use mh3u_formats::gfd;
    // the programs of the game's pack, to tell which of what is found in memory are known ones
    let path = dump.content("nativeCafe/shaderArc.arc").context("no shaderArc.arc in the dump")?;
    let data = read(&path)?;
    let arc = Arc::parse(&data)?;
    let entry = arc
        .entries
        .iter()
        .find(|e| e.name == "shaderPack")
        .context("no shaderPack in shaderArc.arc")?;
    let pack = arc.read(entry)?;
    let mut programs = Vec::new();
    for file in gfd::parse_pack(&pack)? {
        if let Some(p) = file.block(gfd::PIXEL_SHADER_PROGRAM)
            && let Ok(c) = mh3u_latte::census(p)
        {
            programs.push(p[..c.extent].to_vec());
        }
    }
    shader_scan::run(dump, cemu, out, &programs)
}

#[cfg(not(target_os = "linux"))]
fn scan_shaders(_: &Dump, _: &str, _: &Path) -> Result<()> {
    anyhow::bail!("scan-shaders reads another process's memory and works on Linux only")
}

/// The game's shader pack and what is in it.
fn shaders(dump: &Dump) -> Result<()> {
    use mh3u_formats::gfd;
    use mh3u_latte::Census;
    let path = dump.content("nativeCafe/shaderArc.arc").context("no shaderArc.arc in the dump")?;
    let data = read(&path)?;
    let arc = Arc::parse(&data)?;
    let entry = arc
        .entries
        .iter()
        .find(|e| e.name == "shaderPack")
        .context("no shaderPack in shaderArc.arc")?;
    let pack_bytes = arc.read(entry)?;
    let files = gfd::parse_pack(&pack_bytes)?;
    let mut total = Census::default();
    let (mut pixel, mut vertex, mut other) = (0, 0, 0);
    let (mut bytes, mut largest) = (0usize, 0usize);
    for (n, file) in files.iter().enumerate() {
        let program = match (file.block(gfd::PIXEL_SHADER_PROGRAM), file.block(gfd::VERTEX_SHADER_PROGRAM)) {
            (Some(p), _) => {
                pixel += 1;
                p
            }
            (None, Some(p)) => {
                vertex += 1;
                p
            }
            _ => {
                other += 1;
                continue;
            }
        };
        bytes += program.len();
        largest = largest.max(program.len());
        total.add(&mh3u_latte::census(program).map_err(|e| anyhow::anyhow!("shader {n}: {e}"))?);
    }
    println!(
        "{} shader files in the pack: {pixel} pixel, {vertex} vertex, {other} with neither",
        files.len()
    );
    println!("programs: {bytes} bytes in all, {largest} the largest");
    print_census(&total);
    Ok(())
}

fn print_census(total: &mh3u_latte::Census) {
    use mh3u_latte::{alu_name, cf_name};
    println!("\ncontrol flow ({} instructions):", total.control_flow.values().sum::<usize>());
    for (k, n) in &total.control_flow {
        let name = if *k >= 0x40 {
            format!("alu-clause (kind {:#x})", k & 0xf)
        } else {
            cf_name(*k).to_string()
        };
        println!("  {k:>#5x} {name:<24} {n:>8}");
    }
    println!(
        "\nALU: {} instructions in {} groups ({} clauses, {} slots of which {} literal constants)",
        total.alu_instructions, total.groups, total.alu_clauses, total.slots, total.literal_slots
    );
    println!("  groups by size: {:?}", total.group_sizes);
    println!(
        "  {} different two-operand operations, {} three-operand:",
        total.op2.len(),
        total.op3.len()
    );
    let mut ops: Vec<(String, usize)> = total
        .op2
        .iter()
        .map(|(k, n)| (format!("op2 {k:>3} {}", alu_name(false, *k).unwrap_or("")), *n))
        .chain(
            total
                .op3
                .iter()
                .map(|(k, n)| (format!("op3 {k:>3} {}", alu_name(true, *k).unwrap_or("")), *n)),
        )
        .collect();
    ops.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (name, n) in ops {
        println!("    {name:<24} {n:>8}");
    }
    println!(
        "\ntexture: {} clauses, {} fetches, by instruction number: {:?}",
        total.tex_clauses,
        total.tex.values().sum::<usize>(),
        total.tex
    );
}

/// Where the executable's code is loaded.
const TEXT_ADDRESS: u32 = 0x0200_0020;

fn coverage(dump: &Dump, show_unknown: usize) -> Result<()> {
    use mh3u_ppc::{Class, decode, spr, spr_name};
    use std::collections::BTreeMap;
    let data = read(&dump.executable()?)?;
    let text = rpx::section_at(&data, TEXT_ADDRESS)?;
    let words: Vec<u32> = text.as_chunks::<4>().0.iter().map(|w| u32::from_be_bytes(*w)).collect();
    let mut by_name: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_class: BTreeMap<Class, usize> = BTreeMap::new();
    let mut sprs: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown: Vec<(u32, u32)> = Vec::new();
    let mut unknown_count = 0usize;
    let mut unknown_opcodes: BTreeMap<u32, usize> = BTreeMap::new();
    let mut zero = 0usize;
    // how the branches go: direct calls and jumps, returns, and the ones through a register
    let (mut calls, mut jumps, mut conditional, mut returns, mut conditional_returns, mut indirect_jumps, mut indirect_calls) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    for (i, &word) in words.iter().enumerate() {
        let address = TEXT_ADDRESS + (i as u32) * 4;
        match decode(word) {
            Some(d) => {
                *by_name.entry(d.name).or_default() += 1;
                *by_class.entry(d.class).or_default() += 1;
                let link = word & 1 != 0;
                let always = (word >> 21) & 0x14 == 0x14;
                match (d.name, link) {
                    ("b", true) => calls += 1,
                    ("b", false) => jumps += 1,
                    ("bc", _) => conditional += 1,
                    ("bclr", false) if always => returns += 1,
                    ("bclr", _) if !always => conditional_returns += 1,
                    ("bclr", true) | ("bcctr", true) => indirect_calls += 1,
                    ("bcctr", false) => indirect_jumps += 1,
                    _ => {}
                }
                if d.name == "mfspr" || d.name == "mtspr" {
                    let n = spr(word);
                    let name = spr_name(n).map_or_else(|| format!("spr {n}"), str::to_string);
                    *sprs.entry(format!("{} {name}", d.name)).or_default() += 1;
                }
            }
            None if word == 0 => zero += 1,
            None => {
                unknown_count += 1;
                *unknown_opcodes.entry(word >> 26).or_default() += 1;
                if unknown.len() < show_unknown {
                    unknown.push((address, word));
                }
            }
        }
    }
    let total = words.len();
    let known: usize = by_class.values().sum();
    println!("code: {} bytes, {total} words from {TEXT_ADDRESS:#x}", text.len());
    println!(
        "  decoded {known} ({:.2}%), zero words {zero}, unknown {unknown_count}",
        100.0 * known as f64 / total as f64
    );
    println!("  {} different instructions are used", by_name.len());
    println!("\nby kind:");
    let mut kinds: Vec<_> = by_class.iter().collect();
    kinds.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (class, n) in kinds {
        println!("  {class:<10?} {n:>9} {:>6.2}%", 100.0 * *n as f64 / total as f64);
    }
    let count = |names: &[&str]| names.iter().map(|n| by_name.get(n).copied().unwrap_or(0)).sum::<usize>();
    println!("\nthe ones that cost a translator something:");
    println!(
        "  paired singles (ps_*, psq_*): {}",
        by_class.get(&Class::Paired).copied().unwrap_or(0)
    );
    println!("  branches to a register (bclr, bcctr): {}", count(&["bclr", "bcctr"]));
    println!("  reserved loads and stores (lwarx, stwcx.): {}", count(&["lwarx", "stwcx."]));
    println!(
        "  load/store multiple and string (lmw, stmw, lswi, lswx, stswi, stswx): {}",
        count(&["lmw", "stmw", "lswi", "lswx", "stswi", "stswx"])
    );
    println!(
        "  cache control: {}   sync/isync/eieio: {}",
        by_class.get(&Class::Cache).copied().unwrap_or(0),
        by_class.get(&Class::Sync).copied().unwrap_or(0)
    );
    println!(
        "  system (sc, traps, rfi): {}   64-bit only: {}",
        by_class.get(&Class::System).copied().unwrap_or(0),
        by_class.get(&Class::Only64).copied().unwrap_or(0)
    );
    println!(
        "  floating point status register: {}",
        count(&["mtfsb0", "mtfsb1", "mtfsf", "mtfsfi", "mffs", "mcrfs"])
    );
    println!("\nhow the branches go:");
    println!("  calls (bl): {calls}   jumps (b): {jumps}   conditional (bc, including loops): {conditional}");
    println!("  returns (blr): {returns}   conditional returns: {conditional_returns}");
    println!(
        "  indirect jumps (bctr: jump tables, tail calls): {indirect_jumps}   indirect calls (bctrl, blrl: function pointers, virtual calls): {indirect_calls}"
    );
    println!("\nspecial registers:");
    for (k, n) in &sprs {
        println!("  {k:<22} {n:>8}");
    }
    println!("\nmost used instructions:");
    let mut top: Vec<_> = by_name.iter().collect();
    top.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (name, n) in top.iter().take(25) {
        println!("  {name:<12} {n:>9}");
    }
    println!("\nleast used (down to 1):");
    for (name, n) in top.iter().rev().take(25) {
        println!("  {name:<12} {n:>9}");
    }
    if unknown_count > 0 {
        println!("\nunknown words by opcode (first six bits):");
        for (op, n) in &unknown_opcodes {
            println!("  {op:>2}: {n}");
        }
        println!("first unknown words:");
        for (address, word) in &unknown {
            println!("  {address:#010x}  {word:08x}");
        }
    }
    Ok(())
}

fn imports(dump: &Dump, names: bool) -> Result<()> {
    let data = read(&dump.executable()?)?;
    let all = rpx::imports(&data)?;
    let mut libraries: Vec<&str> = all.iter().map(|i| i.library.as_str()).collect();
    libraries.dedup();
    libraries.sort_unstable();
    libraries.dedup();
    let (mut functions, mut datas) = (0, 0);
    for lib in libraries {
        let mine: Vec<_> = all.iter().filter(|i| i.library == lib).collect();
        let f = mine.iter().filter(|i| i.kind == rpx::ImportKind::Function).count();
        println!("{lib:<16} {f:>5} functions {:>4} data", mine.len() - f);
        functions += f;
        datas += mine.len() - f;
        if names {
            for i in mine {
                println!(
                    "    {} {}",
                    if i.kind == rpx::ImportKind::Function { "fn  " } else { "data" },
                    i.name
                );
            }
        }
    }
    println!("{:<16} {functions:>5} functions {datas:>4} data", "all");
    Ok(())
}

fn hex_bytes(h: &str) -> Result<Vec<u8>> {
    let h: String = h.chars().filter(|c| !c.is_whitespace()).collect();
    anyhow::ensure!(
        !h.is_empty() && h.len().is_multiple_of(2),
        "a pattern is an even number of hex digits: {h}"
    );
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).with_context(|| format!("not hex: {h}")))
        .collect()
}

fn arcsearch(dir: &Path, patterns: &[String]) -> Result<()> {
    let pats: Vec<Vec<u8>> = patterns.iter().map(|h| hex_bytes(h)).collect::<Result<_>>()?;
    let mut stack = vec![expand_home(&dir.to_string_lossy())];
    while let Some(dir) = stack.pop() {
        for ent in std::fs::read_dir(&dir)? {
            let path = ent?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "arc") {
                let data = std::fs::read(&path)?;
                let Ok(arc) = Arc::parse(&data) else { continue };
                for e in &arc.entries {
                    let Ok(body) = arc.read(e) else { continue };
                    for (pi, pat) in pats.iter().enumerate() {
                        for (off, _) in body.windows(pat.len()).enumerate().filter(|(_, w)| *w == &pat[..]) {
                            println!("pat{pi} {} :: {} ({:08x}) @ {off:#x}", path.display(), e.name, e.type_hash);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn functions(dump: &Dump) -> Result<()> {
    let data = read(&dump.executable()?)?;
    let text = rpx::section_at(&data, TEXT_ADDRESS)?;
    let words: Vec<u32> = text.as_chunks::<4>().0.iter().map(|w| u32::from_be_bytes(*w)).collect();
    let entry = u32::from_be_bytes(data.get(0x18..0x1c).context("short file")?.try_into()?);
    let mut pointers = Vec::new();
    for s in rpx::sections(&data)? {
        if matches!(s.name.as_str(), ".rodata" | ".data") {
            let body = rpx::contents(&data, &s)?;
            pointers.extend(body.as_chunks::<4>().0.iter().map(|w| u32::from_be_bytes(*w)));
        }
    }
    let end = TEXT_ADDRESS + 4 * words.len() as u32;
    let by_relocation = rpx::relocation_targets(&data, TEXT_ADDRESS, end)?;
    println!("{} addresses in the code that the relocations name", by_relocation.len());
    pointers.extend(by_relocation);
    let calls = mh3u_recomp::discover::functions(TEXT_ADDRESS, &words, &[], entry);
    let all = mh3u_recomp::discover::functions(TEXT_ADDRESS, &words, &pointers, entry);
    println!("entry point {entry:#x}");
    println!("{} functions from the entry and direct calls", calls.len());
    println!(
        "{} with the code pointers in the data ({} more)",
        all.len(),
        all.len() - calls.len()
    );
    let starts: Vec<u32> = all.iter().copied().collect();
    let end = TEXT_ADDRESS + 4 * words.len() as u32;
    let mut sizes: Vec<u32> = starts.windows(2).map(|w| w[1] - w[0]).collect();
    sizes.push(end - starts[starts.len() - 1]);
    sizes.sort_unstable();
    println!(
        "bytes per function: median {}, 99th percentile {}, largest {}",
        sizes[sizes.len() / 2],
        sizes[sizes.len() * 99 / 100],
        sizes[sizes.len() - 1]
    );
    Ok(())
}

/// The imports of the executable as the translation and the run need them.
struct Imports {
    /// "library:function" for every import, functions and data, in the order of the symbol table; a call names its import by the index.
    names: Vec<String>,
    /// Where calls to an import go (the loader's trampolines), and the index of the import.
    targets: std::collections::BTreeMap<u32, u32>,
    /// For every import, where the loader puts it: the trampoline of a function, the variable of data (none when it is not known).
    addresses: Vec<Option<u32>>,
    /// The variables the loader fills in: where each is, the address a call through it goes to, and its name.
    data: Vec<mh3u_rt::DataImport>,
}

/// Where the runtime's own call targets begin: the address a data import that holds a function points at (no code is there).
const DATA_IMPORT_TARGETS: u32 = 0x0320_0000;

/// The relocations of `.text` say which import each call means, and the trampolines of one library are all at the same distance from
/// their symbols' own addresses, so the trampolines of the imports no call names follow from those.
fn import_targets(rpx_data: &[u8], text: &[u8]) -> Result<Imports> {
    let symbols = rpx::import_addresses(rpx_data)?;
    let names: Vec<String> = symbols.iter().map(|(i, _)| format!("{}:{}", i.library, i.name)).collect();
    let mut shift: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for s in rpx::import_sites(rpx_data)?.into_iter().filter(|s| s.kind == 10) {
        let (site, import) = (s.address, s.import);
        let at = site.wrapping_sub(TEXT_ADDRESS) as usize;
        let Some(w) = text.get(at..at + 4) else { continue };
        let w = u32::from_be_bytes(w.try_into()?);
        if w >> 26 != 18 {
            continue;
        }
        let target = site.wrapping_add((((w & 0x03ff_fffc) as i32) << 6 >> 6) as u32);
        let value = symbols
            .iter()
            .find(|(i, _)| i.library == import.library && i.name == import.name && i.kind == rpx::ImportKind::Function)
            .map(|(_, v)| *v)
            .with_context(|| format!("no symbol for the import {}:{}", import.library, import.name))?;
        let this = target.wrapping_sub(value);
        anyhow::ensure!(
            *shift.entry(import.library.clone()).or_insert(this) == this,
            "the trampolines of {} are not at a constant distance from their symbols",
            import.library
        );
    }
    let mut targets = std::collections::BTreeMap::new();
    let mut data = Vec::new();
    let mut addresses = vec![None; symbols.len()];
    for (n, (i, v)) in symbols.iter().enumerate() {
        let Some(s) = shift.get(&i.library) else { continue };
        addresses[n] = Some(v.wrapping_add(*s));
        if i.kind == rpx::ImportKind::Function {
            targets.insert(v.wrapping_add(*s), n as u32);
        } else {
            let target = DATA_IMPORT_TARGETS + 4 * n as u32;
            targets.insert(target, n as u32);
            data.push(mh3u_rt::DataImport {
                slot: v.wrapping_add(*s),
                target,
                name: format!("{}:{}", i.library, i.name),
            });
        }
    }
    Ok(Imports {
        names,
        targets,
        addresses,
        data,
    })
}

/// The code as the loader leaves it: an instruction that builds the address of an import (`lis`/`addi`, `lis`/`lwz`...) has the address's
/// halves patched in, and the file holds other values there.
fn patch_import_addresses(rpx_data: &[u8], words: &mut [u32], imports: &Imports) -> Result<()> {
    let symbols = rpx::import_addresses(rpx_data)?;
    for s in rpx::import_sites(rpx_data)? {
        if s.kind != 4 && s.kind != 6 {
            continue;
        }
        let n = symbols
            .iter()
            .position(|(i, _)| *i == s.import)
            .with_context(|| format!("no symbol for {}", s.import.name))?;
        let Some(address) = imports.addresses[n] else { continue };
        let target = address.wrapping_add(s.addend as u32);
        let Some(w) = words.get_mut((s.address.wrapping_sub(TEXT_ADDRESS) / 4) as usize) else {
            continue;
        };
        let half = if s.kind == 4 {
            target & 0xffff
        } else {
            (target.wrapping_add(0x8000) >> 16) & 0xffff
        };
        *w = (*w & 0xffff_0000) | half;
    }
    Ok(())
}

fn run_game(dump: &Dump, c_dir: &Path, max_imports: u64, save: PathBuf, window: bool) -> Result<()> {
    let data = read(&dump.executable()?)?;
    let entry = u32::from_be_bytes(data.get(0x18..0x1c).context("short file")?.try_into()?);
    let mut pieces = Vec::new();
    for s in rpx::sections(&data)? {
        // the sections that are loaded (SHF_ALLOC) into the program's own range; the import sections are placed by the loader
        if s.flags & 2 == 0 || s.addr >= 0x8000_0000 || s.addr == 0 {
            continue;
        }
        let bytes = if s.kind == 8 { Vec::new() } else { rpx::contents(&data, &s)? };
        pieces.push(mh3u_rt::Piece {
            address: s.addr,
            bytes,
            zero: if s.kind == 8 { s.size as u32 } else { 0 },
        });
    }
    let library = c_dir.join(build_game::LIBRARY);
    let text = rpx::section_at(&data, TEXT_ADDRESS)?;
    let mut content: Vec<PathBuf> = Vec::new();
    if let Some(u) = &dump.update {
        content.push(u.join("content"));
    }
    content.push(dump.base.join("content"));
    let options = mh3u_rt::Options {
        backend: None,
        roots: mh3u_rt::Roots { content, save },
        data_imports: import_targets(&data, &text)?.data,
        entry,
        max_imports,
        stack_top: 0xa010_0000,
        stack_size: 0x10_0000,
    };
    let finish = move |options: mh3u_rt::Options| -> Result<()> {
        let report = mh3u_rt::run(&library, &pieces, options)?;
        println!("{}", report.end);
        for (name, n) in &report.imports {
            println!("  {n:>8}  {name}");
        }
        Ok(())
    };
    if !window {
        return finish(options);
    }
    // the window belongs to this thread; the game runs on another one, drawing into it
    mh3u_gfx::run(move |display| {
        let mut options = options;
        options.backend = Some(Box::new(mh3u_gfx::Gx2::new(display)));
        if let Err(e) = finish(options) {
            eprintln!("{e:#}");
        }
        std::process::exit(0);
    })
}
