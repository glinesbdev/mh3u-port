//! Checks that the decoder reads every shader program of the game's pack, and counts what the programs use.

use anyhow::Result;
use mh3u_formats::gfd;
use mh3u_latte::decode::{Cf, Source, decode};
use mh3u_latte::ops;
use std::collections::BTreeMap;

/// Translates every pixel program of the pack to WGSL and has naga check each one.
pub fn wgsl(pack: &[u8], show: Option<usize>) -> Result<()> {
    use mh3u_latte::translate::{Context, PixelInputs, TexKind, translate_pixel};
    let files = gfd::parse_pack(pack)?;
    let ctx = Context::new(&|_| TexKind::D2);
    let inputs = PixelInputs {
        count: 8,
        flat: vec![false; 8],
        locations: Vec::new(),
    };
    let (mut ok, mut untranslated, mut invalid) = (0, BTreeMap::<String, usize>::new(), 0);
    let mut first_invalid = None;
    let mut seen = std::collections::BTreeSet::new();
    for (n, file) in files.iter().enumerate() {
        let Some(program) = file.block(gfd::PIXEL_SHADER_PROGRAM) else {
            continue;
        };
        if !seen.insert(program.to_vec()) {
            continue;
        }
        let d = decode(program).map_err(|e| anyhow::anyhow!("shader {n}: {e}"))?;
        match translate_pixel(&d, &inputs, &ctx) {
            Err(e) => {
                for part in e.split("; ") {
                    *untranslated.entry(part.to_string()).or_default() += 1;
                }
            }
            Ok(t) => {
                if show == Some(n) {
                    println!("{}", t.wgsl);
                }
                match naga::front::wgsl::parse_str(&t.wgsl)
                    .map_err(|e| e.emit_to_string(&t.wgsl))
                    .and_then(|m| {
                        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
                            .validate(&m)
                            .map(|_| ())
                            .map_err(|e| format!("{e:?}"))
                    }) {
                    Ok(()) => ok += 1,
                    Err(e) => {
                        invalid += 1;
                        if first_invalid.is_none() {
                            first_invalid = Some((n, e, t.wgsl));
                        }
                    }
                }
            }
        }
    }
    println!("{ok} different pixel programs translated and valid, {invalid} translated but not valid");
    println!("not translated, by reason:");
    for (why, n) in &untranslated {
        println!("  {n:>5}  {why}");
    }
    if let Some((n, e, w)) = first_invalid {
        println!(
            "\nfirst invalid: shader {n}\n{e}\n{}",
            w.lines().take(80).collect::<Vec<_>>().join("\n")
        );
    }
    Ok(())
}

pub fn run(pack: &[u8]) -> Result<()> {
    let files = gfd::parse_pack(pack)?;
    let mut op2: BTreeMap<u32, usize> = BTreeMap::new();
    let mut op3: BTreeMap<u32, usize> = BTreeMap::new();
    let mut sources: BTreeMap<String, usize> = BTreeMap::new();
    let mut banks: BTreeMap<u32, usize> = BTreeMap::new();
    let mut flow: BTreeMap<u32, usize> = BTreeMap::new();
    let mut vfetch: BTreeMap<String, usize> = BTreeMap::new();
    let mut omod = 0;
    let mut pred_sel = 0;
    let mut relative = 0;
    let mut programs = 0;
    for (n, file) in files.iter().enumerate() {
        let Some(program) = file
            .block(gfd::PIXEL_SHADER_PROGRAM)
            .or_else(|| file.block(gfd::VERTEX_SHADER_PROGRAM))
        else {
            continue;
        };
        programs += 1;
        let d = decode(program).map_err(|e| anyhow::anyhow!("shader {n}: {e}"))?;
        for cf in &d.cf {
            match cf {
                Cf::Alu { kcache, groups, .. } => {
                    for k in kcache.iter().filter(|k| k.mode != 0) {
                        *banks.entry(k.bank).or_default() += 1;
                    }
                    for g in groups {
                        for i in &g.insts {
                            *if i.three { &mut op3 } else { &mut op2 }.entry(i.op).or_default() += 1;
                            omod += usize::from(i.omod != 0);
                            pred_sel += usize::from(i.pred_sel != 0);
                            for s in &i.src[..if i.three { 3 } else { 2 }] {
                                relative += usize::from(s.relative);
                                let name = match s.source {
                                    Source::Gpr(_) => "gpr".to_string(),
                                    Source::Kcache { bank, .. } => format!("kcache{bank}"),
                                    Source::ConstFile(_) => "constfile".to_string(),
                                    other => format!("{other:?}"),
                                };
                                *sources.entry(name).or_default() += 1;
                            }
                        }
                    }
                }
                Cf::Flow { inst, .. } => *flow.entry(*inst).or_default() += 1,
                Cf::Fetch { fetches, .. } => {
                    for f in fetches.iter().filter(|f| f.inst == 0) {
                        *vfetch
                            .entry(format!(
                                "res {} type {} fmt {} num {} signed {} srf {} mega {} off {} dst {:?} src {:?}",
                                f.resource,
                                f.fetch_type,
                                f.data_format,
                                f.num_format,
                                f.format_comp_signed,
                                f.srf_mode,
                                f.mega_fetch_count,
                                f.vtx_offset,
                                f.dst_sel,
                                f.src_sel
                            ))
                            .or_default() += 1;
                    }
                }
                _ => {}
            }
        }
    }
    println!("{programs} programs decoded");
    println!("two-operand instructions:");
    for (k, n) in &op2 {
        println!("  {k:#05x} {:>8}  {}", n, ops::op2(*k).map_or("UNKNOWN", |o| o.1));
    }
    println!("three-operand instructions:");
    for (k, n) in &op3 {
        println!("  {k:#05x} {:>8}  {}", n, ops::op3(*k).map_or("UNKNOWN", |o| o.1));
    }
    println!("sources: {sources:?}");
    println!("kcache banks used: {banks:?}");
    println!("flow instructions: {flow:?}");
    for (k, n) in &vfetch {
        println!("vfetch {n:>5}  {k}");
    }
    println!("with an output modifier: {omod}, predicated: {pred_sel}, relative sources: {relative}");
    Ok(())
}
