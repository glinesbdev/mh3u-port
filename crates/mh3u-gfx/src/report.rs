//! What the graphics side tells about itself, for finding what is drawn wrong without a debugger:
//!
//! - **MH3U_GFX_REPORT**: every report window (MH3U_TIMES_WINDOW seconds, ten by default) a summary on standard error, each line
//!   starting `[gfx report]`: the draws skipped and why, the shaders not translated and why, the texture formats, tile modes and kinds
//!   the draws read (with how well each format is decoded), the vertex attribute formats, and the `gx2` calls the game imports that
//!   do nothing here.
//! - **MH3U_GFX_DUMP_FRAME=<n>[,<n>...]**: frame `n` (counting swaps from 0) is written to `frame-<n>` in a folder (MH3U_GFX_DUMP_DIR,
//!   `mh3u-frames` by default): `draws.txt` with every draw's state, `shaders/` with each shader's listing and WGSL, `textures/` with
//!   each texture the frame reads as a PNG, and the TV picture as `tv.png`. Small enough to zip and send.

use crate::state::State;
use crate::texture::{Header, Support, format_support};
use mh3u_rt::Guest;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

/// One texture format, tile mode and kind, as the draws in a window read it.
#[derive(Default)]
struct TextureSeen {
    binds: u64,
    textures: HashSet<u32>,
    largest: (u32, u32),
    mips: (u32, u32),
    selects: BTreeSet<u32>,
}

#[derive(Default)]
pub(crate) struct Diagnostics {
    pub report: bool,
    since: Option<(Instant, u64)>,
    draws: u64,
    skipped_then: HashMap<&'static str, u64>,
    /// Shaders not translated: the reason, and the draws skipped for it in this window.
    untranslated: BTreeMap<String, (String, u64)>,
    textures: BTreeMap<(u32, u32, u32), TextureSeen>,
    seen_textures: HashSet<u32>,
    /// Textures that could not be decoded (header text, reason), and header pitches that differ from the computed one.
    failures: BTreeMap<String, String>,
    pitches: BTreeMap<(u32, u32), String>,
    attributes: BTreeMap<(u32, u32), u64>,
    /// The `gx2` imports the backend leaves as stubs (asked about when the game is loaded).
    stubs: Mutex<BTreeSet<String>>,
    stubs_told: bool,
    /// The frames to write out, where, and the folder of the one being written.
    dump_frames: Vec<u32>,
    dump_base: PathBuf,
    pub dump_dir: PathBuf,
    /// Whether the frame being drawn is the one written out.
    pub dumping: bool,
    draw_index: u32,
    draws_text: String,
    /// Each shader's listing and WGSL (the latest translation), by its name (`vs-<address>`, `ps-<address>`); kept only when a frame
    /// is to be written.
    pub shader_texts: HashMap<String, String>,
    written_shaders: HashSet<String>,
    written_textures: HashSet<u32>,
    /// The folder of the frame just finished (its TV picture is written there).
    pub finished_dir: Option<PathBuf>,
}

impl Diagnostics {
    pub fn from_env() -> Diagnostics {
        let dump_frames = std::env::var("MH3U_GFX_DUMP_FRAME")
            .map(|v| v.split(',').filter_map(|n| n.trim().parse().ok()).collect())
            .unwrap_or_default();
        let dump_base = std::env::var_os("MH3U_GFX_DUMP_DIR").map_or_else(|| PathBuf::from("mh3u-frames"), PathBuf::from);
        Diagnostics {
            report: std::env::var_os("MH3U_GFX_REPORT").is_some(),
            dump_frames,
            dump_base,
            ..Default::default()
        }
    }

    /// Whether shader texts are kept (for the frame to be written).
    pub fn keeps_shaders(&self) -> bool {
        !self.dump_frames.is_empty()
    }

    /// Whether anything is gathered per draw.
    pub fn per_draw(&self) -> bool {
        self.report || self.dumping
    }

    pub fn stub(&self, name: &str) {
        if let Ok(mut s) = self.stubs.lock() {
            s.insert(name.to_string());
        }
    }

    pub fn untranslated(&mut self, what: String, reason: &str) {
        self.untranslated.entry(what).or_insert_with(|| (reason.to_string(), 0));
    }

    pub fn skipped_for(&mut self, what: String) {
        if self.report {
            self.untranslated
                .entry(what)
                .or_insert_with(|| ("(reason told earlier)".into(), 0))
                .1 += 1;
        }
    }

    pub fn draw(&mut self) {
        self.draws += 1;
    }

    pub fn texture(&mut self, g: &Guest, t: u32) {
        let h = Header::read(g, t);
        let e = self.textures.entry((h.format, h.tile_mode, h.dim)).or_default();
        e.binds += 1;
        if self.seen_textures.insert(t) {
            e.textures.insert(h.image);
            if h.width * h.height > e.largest.0 * e.largest.1 {
                e.largest = (h.width, h.height);
            }
            let mips = h.mips.max(1);
            e.mips = if e.mips == (0, 0) {
                (mips, mips)
            } else {
                (e.mips.0.min(mips), e.mips.1.max(mips))
            };
            e.selects.insert(g.u32(t + 0x84) & 0x0707_0707);
        }
    }

    pub fn attribute(&mut self, format: u32, endian: u32) {
        *self.attributes.entry((format, endian)).or_default() += 1;
    }

    pub fn decode_failed(&mut self, h: &Header, reason: &str) {
        if self.report {
            self.failures.entry(header_text(h)).or_insert_with(|| reason.to_string());
        }
    }

    pub fn decoded(&mut self, h: &Header, laid_pitch: u32, block: u32) {
        if self.report && h.pitch != laid_pitch && h.pitch != laid_pitch * block && self.pitches.len() < 64 {
            self.pitches
                .entry((h.format, h.tile_mode))
                .or_insert_with(|| format!("{}: header pitch {}, laid out with {laid_pitch} elements", header_text(h), h.pitch));
        }
    }

    /// Prints the window's report when the window is over (called at every swap).
    pub fn maybe_print(&mut self, skipped: &HashMap<&'static str, u64>) {
        if !self.report {
            return;
        }
        let now = Instant::now();
        let (since, frames_at) = *self
            .since
            .get_or_insert_with(|| (now, mh3u_rt::FRAMES.load(std::sync::atomic::Ordering::Relaxed)));
        let wall = now.duration_since(since);
        if wall < mh3u_rt::times_window() {
            return;
        }
        let frames = mh3u_rt::frames_since(frames_at);
        let mut out = String::new();
        let mut skips: Vec<(&str, u64)> = skipped
            .iter()
            .map(|(k, v)| (*k, v - self.skipped_then.get(k).copied().unwrap_or(0)))
            .filter(|(_, n)| *n > 0)
            .collect();
        skips.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        let skipped_total: u64 = skips.iter().map(|(_, n)| n).sum();
        let _ = writeln!(
            out,
            "{:.1} s, {frames} frames, {} draws ({:.0} per frame), {skipped_total} skipped",
            wall.as_secs_f64(),
            self.draws,
            self.draws as f64 / frames as f64
        );
        for (why, n) in &skips {
            let _ = writeln!(out, "  skipped {n:>8}  {why}");
        }
        for (what, (why, n)) in &self.untranslated {
            let _ = writeln!(out, "  not translated: {what} ({n} draws skipped): {why}");
        }
        let _ = writeln!(
            out,
            "  textures read (format, tile mode, kind: binds, distinct images, largest, levels, selectors):"
        );
        let mut textures: Vec<_> = self.textures.iter().collect();
        textures.sort_by_key(|(_, s)| std::cmp::Reverse(s.binds));
        for ((format, tile, dim), s) in textures {
            let (name, support) = format_support(*format);
            let flag = match support {
                Support::Exact => "",
                Support::Approximate => "  APPROXIMATE",
                Support::Unsupported => "  NOT DECODED",
            };
            let selects: Vec<String> = s.selects.iter().map(|&v| select_text(v)).collect();
            let _ = writeln!(
                out,
                "    {name} {} {}: {} binds, {} images, {}x{}, levels {}-{}, {}{flag}",
                tile_text(*tile),
                dim_text(*dim),
                s.binds,
                s.textures.len(),
                s.largest.0,
                s.largest.1,
                s.mips.0,
                s.mips.1,
                selects.join(" ")
            );
        }
        for (what, why) in &self.failures {
            let _ = writeln!(out, "  texture not decoded: {what}: {why}");
        }
        for text in self.pitches.values() {
            let _ = writeln!(out, "  pitch differs: {text}");
        }
        let _ = writeln!(out, "  vertex attribute formats (draws that read them):");
        let mut attributes: Vec<_> = self.attributes.iter().collect();
        attributes.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for ((format, endian), n) in attributes {
            let swap = ["no swap", "8in16", "8in32", "default swap"][(*endian & 3) as usize];
            let _ = writeln!(
                out,
                "    {:>8}  {} ({format:#x}), {swap}",
                n,
                crate::draw::attribute_format_name(*format)
            );
        }
        if !self.stubs_told {
            self.stubs_told = true;
            let stubs = self.stubs.lock().map(|s| s.iter().cloned().collect::<Vec<_>>()).unwrap_or_default();
            let _ = writeln!(
                out,
                "  gx2 calls the game imports that do nothing here ({}): {}",
                stubs.len(),
                stubs.join(", ")
            );
        }
        for line in out.lines() {
            eprintln!("[gfx report] {line}");
        }
        self.since = None;
        self.draws = 0;
        self.skipped_then = skipped.clone();
        for v in self.untranslated.values_mut() {
            v.1 = 0;
        }
        self.textures.clear();
        self.seen_textures.clear();
        self.failures.clear();
        self.attributes.clear();
    }

    /// At a swap: starts or ends the frame that is written out. `swaps` is the count after this swap.
    pub fn swapped(&mut self, swaps: u32) -> bool {
        if self.dump_frames.is_empty() {
            return false;
        }
        let was = self.dumping;
        let finished = was.then(|| self.dump_dir.clone());
        if let Some(dir) = &finished {
            let _ = std::fs::write(dir.join("draws.txt"), &self.draws_text);
            eprintln!(
                "[gfx] wrote a frame: {} draws, {} shaders, {} textures, in {}",
                self.draw_index,
                self.written_shaders.len(),
                self.written_textures.len(),
                dir.display()
            );
        }
        self.dumping = self.dump_frames.contains(&swaps);
        if self.dumping {
            let frame = swaps;
            self.dump_dir = self.dump_base.join(format!("frame-{frame}"));
            self.written_shaders.clear();
            self.written_textures.clear();
            let _ = std::fs::create_dir_all(self.dump_dir.join("shaders"));
            let _ = std::fs::create_dir_all(self.dump_dir.join("textures"));
            self.draws_text.clear();
            self.draw_index = 0;
            eprintln!("[gfx] writing frame {frame} to {}", self.dump_dir.display());
        }
        if let Some(dir) = finished {
            // the TV picture goes with the frame just finished
            self.finished_dir = Some(dir);
            return true;
        }
        false
    }

    /// Writes one draw of the frame: its state, its shaders (once) and its textures (once).
    #[allow(clippy::too_many_arguments)]
    pub fn dump_draw(
        &mut self,
        g: &Guest,
        st: &State,
        call: (u32, u32, Option<(u32, u32)>, u32, u32),
        outcome: &str,
        shaders: [(String, Option<String>); 2],
        attributes: &[crate::draw::AttribStream],
    ) {
        let (primitive, count, indexed, offset, instances) = call;
        let n = self.draw_index;
        self.draw_index += 1;
        let mut t = String::new();
        let _ = writeln!(t, "draw {n}: {outcome}");
        let _ = writeln!(
            t,
            "  primitive {primitive:#x}, count {count}, {}, first {offset}, instances {instances}",
            match indexed {
                Some((at, kind)) => format!("indexed (indices at {at:#x}, type {kind})"),
                None => "not indexed".into(),
            }
        );
        let _ = writeln!(
            t,
            "  shaders: vertex {:#x}, pixel {:#x}, geometry {:#x} (mode {}), fetch {:#x}",
            st.vertex_shader, st.pixel_shader, st.geometry_shader, st.shader_mode, st.fetch_shader
        );
        for (name, text) in shaders {
            if let Some(text) = text
                && self.written_shaders.insert(name.clone())
            {
                let _ = std::fs::write(self.dump_dir.join("shaders").join(format!("{name}.txt")), text);
            }
        }
        let cb = st.colour_buffers[0];
        let surface = |s: u32| {
            if s == 0 {
                "none".to_string()
            } else {
                format!(
                    "{}x{} format {:#x} tile {} image {:#x}",
                    g.u32(s + 4),
                    g.u32(s + 8),
                    g.u32(s + 0x14),
                    g.u32(s + 0x30),
                    g.u32(s + 0x24)
                )
            }
        };
        let _ = writeln!(t, "  colour buffer 0: {}; depth buffer: {}", surface(cb), surface(st.depth_buffer));
        let _ = writeln!(t, "  viewport {:?}, scissor {:?}, half z {}", st.viewport, st.scissor, st.half_z);
        let _ = writeln!(
            t,
            "  depth test {} write {} func {}; stencil {} {:?}; cull front {} back {}, front face {}; polygon offset {:?} (front {}, back {})",
            st.depth_test,
            st.depth_write,
            st.depth_func,
            st.stencil_test,
            st.stencil,
            st.cull_front,
            st.cull_back,
            st.front_face,
            st.polygon_offset,
            st.offset_front,
            st.offset_back
        );
        let _ = writeln!(
            t,
            "  blend on {} {:?}; channel masks {:#x}; alpha test {:?}",
            st.colour_control[1] & 1,
            st.blend[0],
            st.target_masks,
            st.alpha_test
        );
        for a in attributes {
            let _ = writeln!(
                t,
                "  attribute location {} buffer {} offset {} {} ({:#x}) endian {} select {:?}{}",
                a.location,
                a.buffer,
                a.offset,
                crate::draw::attribute_format_name(a.format),
                a.format,
                a.endian,
                a.select,
                if a.per_instance {
                    format!(" per instance / {}", a.divisor)
                } else {
                    String::new()
                }
            );
        }
        for (unit, &tex) in st.pixel_textures.iter().enumerate() {
            if tex == 0 {
                continue;
            }
            let h = Header::read(g, tex);
            let file = self.write_texture(g, &h);
            let _ = writeln!(
                t,
                "  texture {unit}: {}, selectors {}, sampler {:#x} -> {file}",
                header_text(&h),
                select_text(g.u32(tex + 0x84) & 0x0707_0707),
                st.pixel_samplers[unit]
            );
        }
        self.draws_text.push_str(&t);
    }

    fn write_texture(&mut self, g: &Guest, h: &Header) -> String {
        let name = format!("tex-{:08x}-{}x{}-f{:x}-t{}", h.image, h.width, h.height, h.format, h.tile_mode);
        if !self.written_textures.insert(h.image) {
            return format!("textures/{name}.png");
        }
        match crate::texture::decode(g, h) {
            Ok(d) => match d.levels.first().and_then(|l| to_rgba(d.format, l.width, l.height, &l.data)) {
                Some(rgba) => {
                    let w = d.levels[0].width;
                    let hgt = d.levels[0].height;
                    let _ = std::fs::write(self.dump_dir.join("textures").join(format!("{name}.png")), png(w, hgt, &rgba));
                    format!("textures/{name}.png")
                }
                None => format!("(not shown: {:?})", d.format),
            },
            Err(e) => format!("(not decoded: {e})"),
        }
    }
}

fn header_text(h: &Header) -> String {
    let (name, _) = format_support(h.format);
    format!(
        "{name} {}x{}x{} {} {} levels {} swizzle {:#x} pitch {}",
        h.width,
        h.height,
        h.depth,
        dim_text(h.dim),
        tile_text(h.tile_mode),
        h.mips,
        h.swizzle,
        h.pitch
    )
}

fn tile_text(t: u32) -> String {
    let name = match t {
        0 => "default",
        1 => "linear aligned",
        2 => "1D thin",
        3 => "1D thick",
        4 => "2D thin1",
        5 => "2D thin2",
        6 => "2D thin4",
        7 => "2D thick",
        8 => "2B thin1",
        9 => "2B thin2",
        10 => "2B thin4",
        11 => "2B thick",
        12 => "3D thin1",
        13 => "3D thick",
        14 => "3B thin1",
        15 => "3B thick",
        16 => "linear special",
        _ => "?",
    };
    format!("tile {t} ({name})")
}

fn dim_text(d: u32) -> &'static str {
    match d {
        0 => "1D",
        1 => "2D",
        2 => "3D",
        3 => "cube",
        4 => "1D array",
        5 => "2D array",
        6 => "2D MSAA",
        7 => "2D MSAA array",
        _ => "?",
    }
}

/// `compSel` as four letters (r, g, b, a, 0, 1).
fn select_text(v: u32) -> String {
    [24, 16, 8, 0]
        .iter()
        .map(|s| ['r', 'g', 'b', 'a', '0', '1', '?', '?'][((v >> s) & 7) as usize])
        .collect()
}

fn half(bits: u16) -> f32 {
    let (sign, exp, man) = ((bits >> 15) as u32, ((bits >> 10) & 0x1f) as u32, (bits & 0x3ff) as u32);
    let mag = match exp {
        0 => man as f32 / 1024.0 / 16384.0,
        31 => f32::INFINITY,
        e => (1.0 + man as f32 / 1024.0) * 2f32.powi(e as i32 - 15),
    };
    if sign != 0 { -mag } else { mag }
}

fn unit(f: f32) -> u8 {
    (f.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// A level's texels as 8-bit RGBA, for looking at; None for the formats not shown.
pub(crate) fn to_rgba(format: wgpu::TextureFormat, w: u32, h: u32, data: &[u8]) -> Option<Vec<u8>> {
    use wgpu::TextureFormat as F;
    let n = (w * h) as usize;
    let mut out = Vec::with_capacity(n * 4);
    let per = |bytes: usize, f: &dyn Fn(&[u8], &mut Vec<u8>)| -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(n * 4);
        for e in data.chunks_exact(bytes).take(n) {
            f(e, &mut out);
        }
        (out.len() == n * 4).then_some(out)
    };
    match format {
        F::Rgba8Unorm | F::Rgba8UnormSrgb => return (data.len() >= n * 4).then(|| data[..n * 4].to_vec()),
        F::R8Unorm => return per(1, &|e, o| o.extend_from_slice(&[e[0], e[0], e[0], 255])),
        F::Rg8Unorm => return per(2, &|e, o| o.extend_from_slice(&[e[0], e[1], 0, 255])),
        F::R16Float => {
            return per(2, &|e, o| {
                let v = unit(half(u16::from_le_bytes([e[0], e[1]])));
                o.extend_from_slice(&[v, v, v, 255]);
            });
        }
        F::Rg16Float => {
            return per(4, &|e, o| {
                let c = |k: usize| unit(half(u16::from_le_bytes([e[2 * k], e[2 * k + 1]])));
                o.extend_from_slice(&[c(0), c(1), 0, 255]);
            });
        }
        F::Rgba16Float => {
            return per(8, &|e, o| {
                for k in 0..4 {
                    o.push(unit(half(u16::from_le_bytes([e[2 * k], e[2 * k + 1]]))));
                }
            });
        }
        F::R32Float => {
            return per(4, &|e, o| {
                let v = unit(f32::from_le_bytes([e[0], e[1], e[2], e[3]]));
                o.extend_from_slice(&[v, v, v, 255]);
            });
        }
        F::Rgba32Float => {
            return per(16, &|e, o| {
                for k in 0..4 {
                    o.push(unit(f32::from_le_bytes([e[4 * k], e[4 * k + 1], e[4 * k + 2], e[4 * k + 3]])));
                }
            });
        }
        F::Rgb10a2Unorm => {
            return per(4, &|e, o| {
                let v = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
                let c = |s: u32| ((v >> s) & 0x3ff) as f32 / 1023.0;
                o.extend_from_slice(&[unit(c(0)), unit(c(10)), unit(c(20)), unit((v >> 30) as f32 / 3.0)]);
            });
        }
        _ => {}
    }
    let (block_bytes, kind) = match format {
        F::Bc1RgbaUnorm | F::Bc1RgbaUnormSrgb => (8, 1),
        F::Bc2RgbaUnorm | F::Bc2RgbaUnormSrgb => (16, 2),
        F::Bc3RgbaUnorm | F::Bc3RgbaUnormSrgb => (16, 3),
        F::Bc4RUnorm | F::Bc4RSnorm => (8, 4),
        F::Bc5RgUnorm | F::Bc5RgSnorm => (16, 5),
        _ => return None,
    };
    let signed = matches!(format, F::Bc4RSnorm | F::Bc5RgSnorm);
    let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
    if data.len() < (bw * bh) as usize * block_bytes {
        return None;
    }
    out.resize(n * 4, 255);
    for by in 0..bh {
        for bx in 0..bw {
            let b = &data[((by * bw + bx) as usize) * block_bytes..][..block_bytes];
            let mut texels = [[0u8, 0, 0, 255]; 16];
            match kind {
                1 => colour_block(b, &mut texels, true),
                2 => {
                    colour_block(&b[8..], &mut texels, false);
                    for (k, t) in texels.iter_mut().enumerate() {
                        let a = (u64::from_le_bytes(b[..8].try_into().unwrap_or_default()) >> (4 * k)) & 0xf;
                        t[3] = (a * 17) as u8;
                    }
                }
                3 => {
                    colour_block(&b[8..], &mut texels, false);
                    let a = channel_block(&b[..8], false);
                    for (t, v) in texels.iter_mut().zip(a) {
                        t[3] = v;
                    }
                }
                4 => {
                    let r = channel_block(b, signed);
                    for (t, v) in texels.iter_mut().zip(r) {
                        *t = [v, v, v, 255];
                    }
                }
                _ => {
                    let (r, g) = (channel_block(&b[..8], signed), channel_block(&b[8..], signed));
                    for (k, t) in texels.iter_mut().enumerate() {
                        *t = [r[k], g[k], 0, 255];
                    }
                }
            }
            for (k, t) in texels.iter().enumerate() {
                let (x, y) = (bx * 4 + (k as u32 & 3), by * 4 + (k as u32 >> 2));
                if x < w && y < h {
                    let at = ((y * w + x) * 4) as usize;
                    out[at..at + 4].copy_from_slice(t);
                }
            }
        }
    }
    Some(out)
}

fn colour_block(b: &[u8], texels: &mut [[u8; 4]; 16], punch_through: bool) {
    let c0 = u16::from_le_bytes([b[0], b[1]]);
    let c1 = u16::from_le_bytes([b[2], b[3]]);
    let rgb = |c: u16| {
        let (r, g, bl) = (u32::from(c >> 11), u32::from((c >> 5) & 0x3f), u32::from(c & 0x1f));
        [(r * 255 / 31) as i32, (g * 255 / 63) as i32, (bl * 255 / 31) as i32]
    };
    let (a, z) = (rgb(c0), rgb(c1));
    let mix = |p: i32, q: i32, d: i32| std::array::from_fn::<i32, 3, _>(|k| (a[k] * p + z[k] * q) / d);
    let palette: [[i32; 3]; 4] = if c0 > c1 || !punch_through {
        [a, z, mix(2, 1, 3), mix(1, 2, 3)]
    } else {
        [a, z, mix(1, 1, 2), [0, 0, 0]]
    };
    let bits = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    for (k, t) in texels.iter_mut().enumerate() {
        let i = ((bits >> (2 * k)) & 3) as usize;
        let p = palette[i];
        *t = [
            p[0] as u8,
            p[1] as u8,
            p[2] as u8,
            if punch_through && c0 <= c1 && i == 3 { 0 } else { 255 },
        ];
    }
}

/// A BC4-style block (also BC3's alpha and BC5's channels), as 8-bit values (a signed block is shown with 0 as mid-grey).
fn channel_block(b: &[u8], signed: bool) -> [u8; 16] {
    let (e0, e1) = if signed {
        (f32::from(b[0] as i8).max(-127.0) / 127.0, f32::from(b[1] as i8).max(-127.0) / 127.0)
    } else {
        (f32::from(b[0]) / 255.0, f32::from(b[1]) / 255.0)
    };
    let mut palette = [0f32; 8];
    palette[0] = e0;
    palette[1] = e1;
    if e0 > e1 {
        for k in 1..7 {
            palette[k + 1] = (e0 * (7 - k) as f32 + e1 * k as f32) / 7.0;
        }
    } else {
        for k in 1..5 {
            palette[k + 1] = (e0 * (5 - k) as f32 + e1 * k as f32) / 5.0;
        }
        palette[6] = if signed { -1.0 } else { 0.0 };
        palette[7] = 1.0;
    }
    let mut bits = 0u64;
    for (k, &v) in b[2..8].iter().enumerate() {
        bits |= u64::from(v) << (8 * k);
    }
    std::array::from_fn(|k| {
        let v = palette[((bits >> (3 * k)) & 7) as usize];
        unit(if signed { v * 0.5 + 0.5 } else { v })
    })
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// An 8-bit RGBA image as a PNG file (stored, not compressed: zip the folder).
pub(crate) fn png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((w * 4 + 1) as usize * h as usize);
    for row in rgba.chunks_exact((w * 4) as usize).take(h as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    // zlib with stored blocks
    let mut z = vec![0x78, 0x01];
    let chunks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (k, c) in chunks.iter().enumerate() {
        z.push(u8::from(k + 1 == chunks.len()));
        let len = c.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(c);
    }
    if chunks.is_empty() {
        z.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut chunk = |kind: &[u8; 4], body: &[u8]| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(body);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc32(&c).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_png_has_the_signature_and_the_right_checksums() {
        let p = png(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]);
        assert_eq!(&p[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        // the CRC of "IEND" with no body
        assert_eq!(&p[p.len() - 4..], &0xae42_6082u32.to_be_bytes());
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        if let Some(path) = std::env::var_os("MH3U_TEST_PNG") {
            let rgba: Vec<u8> = (0..300 * 300)
                .flat_map(|i: u32| [(i % 256) as u8, (i / 300) as u8, 128, 255])
                .collect();
            let _ = std::fs::write(path, png(300, 300, &rgba));
        }
    }

    #[test]
    fn a_bc1_block_of_one_colour_decodes_to_that_colour() {
        // c0 = c1 = pure red (0xf800), all indices 0
        let block = [0x00, 0xf8, 0x00, 0xf8, 0, 0, 0, 0];
        let rgba = to_rgba(wgpu::TextureFormat::Bc1RgbaUnorm, 4, 4, &block).unwrap();
        assert_eq!(&rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(&rgba[60..], &[255, 0, 0, 255]);
    }

    #[test]
    fn a_bc4_block_interpolates_between_its_ends() {
        // e0 = 255, e1 = 0, texel 0 index 0 (255), texel 1 index 1 (0), texel 2 index 2 (6/7 of 255)
        let block = [255, 0, 0b10_001_000, 0b0000_0000, 0, 0, 0, 0];
        let rgba = to_rgba(wgpu::TextureFormat::Bc4RUnorm, 4, 4, &block).unwrap();
        assert_eq!(rgba[0], 255);
        assert_eq!(rgba[4], 0);
        assert_eq!(rgba[8], 219);
        assert_eq!(select_text(0x0001_0203), "rgba");
        assert_eq!(select_text(0x0004_0405), "r001");
    }
}
