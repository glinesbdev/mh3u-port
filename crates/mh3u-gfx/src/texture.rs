//! Textures: the game's `GX2Texture`s read out of the guest's memory, untiled and converted to what the GPU API takes, and the
//! samplers the game describes with `GX2InitSampler*`.

use mh3u_formats::tiling::{self, Surface, TileMode};
use mh3u_rt::Guest;

/// A texture as its header says (a `GX2Texture` begins with a `GX2Surface`); the mip count and the component selectors are read for
/// the parts not done yet.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Header {
    pub dim: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub mips: u32,
    pub format: u32,
    pub image: u32,
    pub image_size: u32,
    pub tile_mode: u32,
    pub swizzle: u32,
    pub pitch: u32,
    /// Where the smaller levels are: the pointer, its size and the offsets of levels 2 and up.
    pub mip_ptr: u32,
    pub mip_size: u32,
    pub mip_offsets: [u32; 13],
    /// How the components are chosen: four selectors for red, green, blue and alpha (0 to 3 a component, 4 zero, 5 one).
    pub select: [u32; 4],
}

impl Header {
    pub fn read(g: &Guest, t: u32) -> Header {
        let comp = g.u32(t + 0x84);
        Header {
            dim: g.u32(t),
            width: g.u32(t + 4),
            height: g.u32(t + 8),
            depth: g.u32(t + 0xc),
            mips: g.u32(t + 0x10),
            format: g.u32(t + 0x14),
            image: g.u32(t + 0x24),
            image_size: g.u32(t + 0x20),
            tile_mode: g.u32(t + 0x30),
            swizzle: g.u32(t + 0x34),
            pitch: g.u32(t + 0x3c),
            mip_ptr: g.u32(t + 0x2c),
            mip_size: g.u32(t + 0x28),
            mip_offsets: std::array::from_fn(|i| g.u32(t + 0x40 + 4 * i as u32)),
            select: [(comp >> 24) & 7, (comp >> 16) & 7, (comp >> 8) & 7, comp & 7],
        }
    }
}

/// What a texture becomes on the GPU: its size and format, and each level's bytes (every layer, one after the other).
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    /// Layers of an array or cube, or slices of a volume (`volume`).
    pub layers: u32,
    pub volume: bool,
    pub format: wgpu::TextureFormat,
    /// The base level's pitch in elements as the layout rules give it (what is used; when the header's own differs, the report
    /// says so).
    pub laid_pitch: u32,
    pub levels: Vec<Level>,
}

pub struct Level {
    pub width: u32,
    pub height: u32,
    /// Its layers (slices for a volume, which halve with the levels).
    pub depth: u32,
    pub bytes_per_row: u32,
    pub data: Vec<u8>,
}

/// How the elements of a format are read.
struct Info {
    /// Bits in an element, and the width and height of the block an element covers.
    bits: u32,
    block: u32,
    format: wgpu::TextureFormat,
    /// Turns the guest's bytes of one element into the bytes of the texel(s) of the output.
    convert: fn(&[u8], &mut Vec<u8>),
}

// The GPU reads texture memory little-endian and GX2 asks for no swap for any texture format (the endian field of its format table
// is 0 for all of them), so a 16-bit texel is a little-endian 16-bit word with its first component in the lowest bits, the way
// Cemu's decoders read them; block-compressed data and 32-bit floats go up as they are.

fn copy(e: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(e);
}

fn le16(e: &[u8]) -> u32 {
    u32::from(u16::from_le_bytes([e[0], e[1]]))
}

/// A field of `bits` bits widened to 8.
fn widen(v: u32, bits: u32) -> u8 {
    (v * 255 / ((1 << bits) - 1)) as u8
}

fn r5g6b5(e: &[u8], out: &mut Vec<u8>) {
    let v = le16(e);
    out.extend_from_slice(&[widen(v & 31, 5), widen((v >> 5) & 63, 6), widen((v >> 11) & 31, 5), 255]);
}

/// Red in the lowest bits, alpha in the top one.
fn r5g5b5a1(e: &[u8], out: &mut Vec<u8>) {
    let v = le16(e);
    out.extend_from_slice(&[
        widen(v & 31, 5),
        widen((v >> 5) & 31, 5),
        widen((v >> 10) & 31, 5),
        widen(v >> 15, 1),
    ]);
}

/// Alpha in the lowest bit, red in the top five.
fn a1b5g5r5(e: &[u8], out: &mut Vec<u8>) {
    let v = le16(e);
    out.extend_from_slice(&[
        widen((v >> 11) & 31, 5),
        widen((v >> 6) & 31, 5),
        widen((v >> 1) & 31, 5),
        widen(v & 1, 1),
    ]);
}

fn r4g4b4a4(e: &[u8], out: &mut Vec<u8>) {
    let v = le16(e);
    let n = |s: u32| widen((v >> s) & 15, 4);
    out.extend_from_slice(&[n(0), n(4), n(8), n(12)]);
}

fn r4g4(e: &[u8], out: &mut Vec<u8>) {
    let v = u32::from(e[0]);
    out.extend_from_slice(&[widen(v & 15, 4), widen(v >> 4, 4)]);
}

fn f16_bits(f: f32) -> u16 {
    let b = f.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32 - 127 + 15;
    let man = b & 0x7f_ffff;
    if exp <= 0 {
        return sign;
    }
    if exp >= 31 {
        return sign | 0x7c00;
    }
    sign | ((exp as u16) << 10) | (man >> 13) as u16
}

/// 16-bit normalised components to half floats (the 16-bit normalised formats need a device feature).
fn unorm16_to_half(e: &[u8], out: &mut Vec<u8>) {
    for w in e.as_chunks::<2>().0 {
        let v = f32::from(u16::from_le_bytes(*w)) / 65535.0;
        out.extend_from_slice(&f16_bits(v).to_le_bytes());
    }
}

fn snorm16_to_half(e: &[u8], out: &mut Vec<u8>) {
    for w in e.as_chunks::<2>().0 {
        let v = (f32::from(i16::from_le_bytes(*w)) / 32767.0).max(-1.0);
        out.extend_from_slice(&f16_bits(v).to_le_bytes());
    }
}

/// Alpha in the lowest two bits, red in the top ten (by the name, lowest first, as the other formats are named); what the
/// target format wants is red lowest.
fn a2b10g10r10(e: &[u8], out: &mut Vec<u8>) {
    let v = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
    let (a, b, g, r) = (v & 3, (v >> 2) & 0x3ff, (v >> 12) & 0x3ff, (v >> 22) & 0x3ff);
    out.extend_from_slice(&(r | g << 10 | b << 20 | a << 30).to_le_bytes());
}

/// How well the decoder knows a texture format: by a reference (`Exact`), by the name only (`Approximate`, e.g. the component
/// order of a format no reference decodes), or not at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    Exact,
    Approximate,
    Unsupported,
}

/// A short name of a texture format and how well it is decoded, for reports.
pub fn format_support(format: u32) -> (String, Support) {
    let hw = format & 0x3f;
    let kind = match format >> 8 {
        0 => "unorm",
        1 => "uint",
        2 => "snorm",
        3 => "sint",
        4 => "srgb",
        8 => "float",
        _ => "?",
    };
    let name = match hw {
        0x01 => "r8",
        0x02 => "r4g4",
        0x05 => "r16",
        0x06 => "r16f",
        0x07 => "r8g8",
        0x08 => "r5g6b5",
        0x0a => "r5g5b5a1",
        0x0b => "r4g4b4a4",
        0x0c => "a1b5g5r5",
        0x0d => "r32",
        0x0e => "r32f",
        0x0f => "r16g16",
        0x10 => "r16g16f",
        0x11 => "r24x8/d24s8",
        0x16 => "r11g11b10f",
        0x19 => "r10g10b10a2",
        0x1a => "r8g8b8a8",
        0x1b => "a2b10g10r10",
        0x1c => "d32s8",
        0x1d => "r32g32",
        0x1e => "r32g32f",
        0x1f => "r16g16b16a16",
        0x20 => "r16g16b16a16f",
        0x22 => "r32g32b32a32",
        0x23 => "r32g32b32a32f",
        0x31 => "bc1",
        0x32 => "bc2",
        0x33 => "bc3",
        0x34 => "bc4",
        0x35 => "bc5",
        _ => "?",
    };
    let support = match info(format) {
        None => Support::Unsupported,
        // the integer kinds are read as normalised ones (a shader samples them as floats); names only for these two
        Some(_) if matches!(format >> 8, 1 | 3) || matches!(hw, 0x1b | 0x0c | 0x02) => Support::Approximate,
        Some(_) => Support::Exact,
    };
    (format!("{kind} {name} ({format:#x})"), support)
}

fn info(format: u32) -> Option<Info> {
    use wgpu::TextureFormat as F;
    let (hw, kind) = (format & 0x3f, format >> 8);
    let srgb = kind == 4;
    let snorm = kind == 2;
    let rgba8 = |convert: fn(&[u8], &mut Vec<u8>)| Info {
        bits: 16,
        block: 1,
        format: F::Rgba8Unorm,
        convert,
    };
    let plain = |bits: u32, format: F| Info {
        bits,
        block: 1,
        format,
        convert: copy,
    };
    let norm16 = |bits: u32, format: F| Info {
        bits,
        block: 1,
        format,
        convert: if snorm { snorm16_to_half } else { unorm16_to_half },
    };
    let bc = |bits: u32, format: F| Info {
        bits,
        block: 4,
        format,
        convert: copy,
    };
    Some(match hw {
        0x01 => plain(8, if snorm { F::R8Snorm } else { F::R8Unorm }),
        0x02 => Info {
            bits: 8,
            block: 1,
            format: F::Rg8Unorm,
            convert: r4g4,
        },
        0x07 => plain(16, if snorm { F::Rg8Snorm } else { F::Rg8Unorm }),
        0x1a => plain(
            32,
            if srgb {
                F::Rgba8UnormSrgb
            } else if snorm {
                F::Rgba8Snorm
            } else {
                F::Rgba8Unorm
            },
        ),
        0x08 => rgba8(r5g6b5),
        0x0a => rgba8(r5g5b5a1),
        0x0c => rgba8(a1b5g5r5),
        0x0b => rgba8(r4g4b4a4),
        0x05 => norm16(16, F::R16Float),
        0x0f => norm16(32, F::Rg16Float),
        0x1f => norm16(64, F::Rgba16Float),
        0x06 => plain(16, F::R16Float),
        0x10 => plain(32, F::Rg16Float),
        0x20 => plain(64, F::Rgba16Float),
        0x0e => plain(32, F::R32Float),
        0x1e => plain(64, F::Rg32Float),
        0x23 => plain(128, F::Rgba32Float),
        0x16 => plain(32, F::Rg11b10Ufloat),
        0x19 => plain(32, F::Rgb10a2Unorm),
        0x1b => Info {
            bits: 32,
            block: 1,
            format: F::Rgb10a2Unorm,
            convert: a2b10g10r10,
        },
        0x31 => bc(64, if srgb { F::Bc1RgbaUnormSrgb } else { F::Bc1RgbaUnorm }),
        0x32 => bc(128, if srgb { F::Bc2RgbaUnormSrgb } else { F::Bc2RgbaUnorm }),
        0x33 => bc(128, if srgb { F::Bc3RgbaUnormSrgb } else { F::Bc3RgbaUnorm }),
        0x34 => bc(64, if snorm { F::Bc4RSnorm } else { F::Bc4RUnorm }),
        0x35 => bc(128, if snorm { F::Bc5RgSnorm } else { F::Bc5RgUnorm }),
        _ => return None,
    })
}

/// Reads a texture, every level, from the guest's memory.
/// A level's laid-out place: where it starts and its size in bytes, by the same rules the game's own size calculation used (the
/// address library's).
struct LevelPlace {
    base: u32,
    laid: tiling::Info,
    w: u32,
    h: u32,
    slices: u32,
}

/// What `decode` needs to know before reading: the format, the tile mode and every level's place.
struct Plan {
    info: Info,
    volume: bool,
    layers: u32,
    height: u32,
    places: Vec<LevelPlace>,
}

fn plan(h: &Header) -> Result<Plan, String> {
    let Some(info) = info(h.format) else {
        return Err(format!("the texture format {:#x}", h.format));
    };
    let Some(mode) = TileMode::from_number(h.tile_mode) else {
        return Err(format!("the tile mode {}", h.tile_mode));
    };
    if h.width == 0 || h.height.max(1) > 16384 || h.width > 16384 || h.image == 0 || h.depth > 2048 {
        return Err("an empty or huge texture".into());
    }
    let volume = h.dim == 2;
    let layers = match h.dim {
        3 => 6,
        2 | 4..=7 => h.depth.max(1),
        _ => 1,
    };
    let height = if h.dim == 0 { 1 } else { h.height.max(1) };
    let block = info.block;
    let max_levels = 32 - h.width.max(height).max(if volume { layers } else { 1 }).leading_zeros();
    let count = h.mips.clamp(1, max_levels);
    let mut places = Vec::new();
    for level in 0..count {
        let (w, hgt) = ((h.width >> level).max(1), (height >> level).max(1));
        // a volume's slices halve with the levels; an array's layers do not
        let slices = if volume { (layers >> level).max(1) } else { layers };
        let laid = tiling::surface_info(&tiling::Request {
            mode,
            bits_per_element: info.bits,
            compressed: block > 1,
            width: w,
            height: hgt,
            slices,
            samples: 1,
            level,
            cube: h.dim == 3,
            depth: false,
            display: false,
        });
        let base = match level {
            0 => h.image,
            1 => h.mip_ptr,
            n => h.mip_ptr.wrapping_add(h.mip_offsets[n as usize - 1]),
        };
        if base == 0 {
            break;
        }
        places.push(LevelPlace {
            base,
            laid,
            w,
            h: hgt,
            slices,
        });
    }
    Ok(Plan {
        info,
        volume,
        layers,
        height,
        places,
    })
}

/// The guest ranges a decode of `h` reads, for a [`crate::decode::Snapshot`]: the image and the smaller levels, each at least as big
/// as the header says (what the source hash covers) and as the levels laid out in it (at most 256 MiB each).
pub fn sources(h: &Header) -> Vec<(u32, usize)> {
    let cap = |n: u64| n.min(1 << 28) as usize;
    let mut image = u64::from(h.image_size);
    let mut mips = u64::from(h.mip_size);
    if let Ok(p) = plan(h) {
        for (n, l) in p.places.iter().enumerate() {
            match n {
                0 => image = image.max(l.laid.size),
                _ => mips = mips.max(u64::from(l.base.wrapping_sub(h.mip_ptr)) + l.laid.size),
            }
        }
    }
    let mut out = vec![(h.image, cap(image))];
    if h.mip_ptr != 0 {
        out.push((h.mip_ptr, cap(mips)));
    }
    out
}

/// Levels at least this big (decoded bytes) are split between threads.
const PARALLEL_BYTES: usize = 256 << 10;

/// Decodes the texture `h` describes from `m` (a snapshot of `sources(h)`), splitting each big level's rows between up to `threads`
/// threads.
pub fn decode<M: crate::decode::Memory + Sync>(m: &M, h: &Header, threads: usize) -> Result<Decoded, String> {
    let p = plan(h)?;
    let info = &p.info;
    let block = info.block;
    let (pipe_swizzle, bank_swizzle) = tiling::swizzles(h.swizzle);
    let elem_bytes = (info.bits / 8) as usize;
    let mut levels = Vec::new();
    let laid_pitch = p.places.first().map_or(0, |l| l.laid.pitch);
    for l in &p.places {
        let (bw, bh) = (l.w.div_ceil(block), l.h.div_ceil(block));
        let surface = Surface {
            mode: l.laid.mode,
            bits_per_element: info.bits,
            pitch: l.laid.pitch,
            height: l.laid.height,
            samples: 1,
            depth: false,
            pipe_swizzle,
            bank_swizzle,
        };
        // rows (layer, y) in order; each part of them is decoded on its own and the parts joined
        let rows = (l.slices * bh) as usize;
        let rows_of = |from: usize, to: usize| {
            let mut data = Vec::with_capacity((to - from) * bw as usize * elem_bytes);
            let mut element = vec![0u8; elem_bytes];
            for r in from..to {
                let (layer, y) = (r as u32 / bh, r as u32 % bh);
                for x in 0..bw {
                    let at = l.base.wrapping_add(tiling::address(&surface, x, y, layer, 0) as u32);
                    m.read(at, &mut element);
                    (info.convert)(&element, &mut data);
                }
            }
            data
        };
        let bytes = rows * bw as usize * elem_bytes;
        let parts = if threads > 1 && bytes >= PARALLEL_BYTES && rows > 1 {
            threads.min(rows)
        } else {
            1
        };
        let data = if parts == 1 {
            rows_of(0, rows)
        } else {
            let per = rows.div_ceil(parts);
            std::thread::scope(|scope| {
                let handles: Vec<_> = (0..parts)
                    .map(|k| {
                        let (from, to) = ((k * per).min(rows), ((k + 1) * per).min(rows));
                        scope.spawn(move || rows_of(from, to))
                    })
                    .collect();
                let mut data = Vec::new();
                for part in handles {
                    data.extend(part.join().unwrap_or_default());
                }
                data
            })
        };
        let texel_bytes = data.len() / (bw * bh * l.slices).max(1) as usize;
        levels.push(Level {
            width: bw * block,
            height: bh * block,
            depth: l.slices,
            bytes_per_row: bw * texel_bytes as u32,
            data,
        });
    }
    Ok(Decoded {
        width: h.width.next_multiple_of(block),
        height: p.height.next_multiple_of(block),
        layers: p.layers,
        volume: p.volume,
        format: info.format,
        laid_pitch,
        levels,
    })
}

/// Decodes the texture at `h` straight from guest memory (copied first), on this thread and its helpers.
pub fn decode_guest(g: &Guest, h: &Header) -> Result<Decoded, String> {
    let snapshot = crate::decode::Snapshot::take(g, &sources(h));
    decode(&snapshot, h, crate::decode::texture_threads())
}

/// A sampler's description, as the game's `GX2InitSampler*` calls put it into the three words of a `GX2Sampler` (this backend's own
/// encoding; the game never reads them).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sampler {
    pub clamp: [u32; 3],
    pub mag_linear: bool,
    pub min_linear: bool,
    pub mip_linear: bool,
    pub mip: bool,
    pub compare: Option<u32>,
    pub min_lod: f32,
    pub max_lod: f32,
    pub lod_bias: f32,
}

impl Sampler {
    pub fn read(g: &Guest, at: u32) -> Sampler {
        let (w0, w1, w2) = (g.u32(at), g.u32(at + 4), g.u32(at + 8));
        Sampler {
            clamp: [w0 & 15, (w0 >> 4) & 15, (w0 >> 8) & 15],
            mag_linear: (w0 >> 12) & 15 != 0,
            min_linear: (w0 >> 16) & 15 != 0,
            mip: (w0 >> 24) & 15 != 0,
            mip_linear: (w0 >> 24) & 15 == 2,
            compare: (w1 & 15 != 15).then_some(w1 & 15),
            min_lod: ((w1 >> 8) & 0x3ff) as f32 / 16.0,
            max_lod: ((w1 >> 18) & 0x3ff) as f32 / 16.0,
            lod_bias: f32::from_bits(w2),
        }
    }

    pub fn address_mode(clamp: u32) -> wgpu::AddressMode {
        match clamp {
            0 => wgpu::AddressMode::Repeat,
            1 => wgpu::AddressMode::MirrorRepeat,
            _ => wgpu::AddressMode::ClampToEdge,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 512x512 RGBA8 texture tiled 2D (mode 4) with three levels, in a 4 MiB guest filled with a pattern.
    fn big_texture(m: &mut [u8]) -> Header {
        for (k, b) in m.iter_mut().enumerate() {
            *b = (k.wrapping_mul(2_654_435_761) >> 13) as u8;
        }
        Header {
            dim: 1,
            width: 512,
            height: 512,
            depth: 1,
            mips: 3,
            format: 0x1a,
            image: 0x10_0000,
            image_size: 0,
            tile_mode: 4,
            swizzle: 0,
            pitch: 512,
            mip_ptr: 0x20_0000,
            mip_size: 0,
            mip_offsets: [0, 0x4_0000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            select: [0, 1, 2, 3],
        }
    }

    #[test]
    fn a_texture_decoded_on_several_threads_is_the_one_decoded_on_one() {
        let mut m = vec![0u8; 0x40_0000];
        let h = big_texture(&mut m);
        let g = Guest::new(m.as_mut_ptr());
        let snapshot = crate::decode::Snapshot::take(&g, &sources(&h));
        let one = decode(&snapshot, &h, 1).expect("decoded");
        let many = decode(&snapshot, &h, 4).expect("decoded");
        assert_eq!(one.levels.len(), 3);
        assert_eq!(one.levels[0].data.len(), 512 * 512 * 4);
        for (a, b) in one.levels.iter().zip(&many.levels) {
            assert!(a.data == b.data && a.bytes_per_row == b.bytes_per_row);
        }
        // and the copy held everything the decode read
        assert_eq!(snapshot.outside(), 0);
        // straight from guest memory, as the render thread does it, the same again
        assert!(decode_guest(&g, &h).expect("decoded").levels[2].data == one.levels[2].data);
    }

    #[test]
    fn the_bytes_copied_for_a_decode_cover_the_levels_as_laid_out() {
        let mut m = vec![0u8; 0x40_0000];
        let h = big_texture(&mut m);
        // the header says nothing of the sizes: they come from the layout
        let s = sources(&h);
        assert_eq!(s[0].0, 0x10_0000);
        assert!(s[0].1 >= 512 * 512 * 4);
        assert_eq!(s[1].0, 0x20_0000);
        assert!(s[1].1 >= 0x4_0000 + 128 * 128 * 4);
        // the header's sizes, when bigger, are what is copied (the source hash covers them)
        let bigger = Header {
            image_size: 0x18_0000,
            ..h
        };
        assert_eq!(sources(&bigger)[0].1, 0x18_0000);
        // no smaller levels: one range
        assert_eq!(
            sources(&Header {
                mip_ptr: 0,
                mips: 1,
                ..big_texture(&mut m)
            })
            .len(),
            1
        );
    }

    fn run(f: fn(&[u8], &mut Vec<u8>), bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        f(bytes, &mut out);
        out
    }

    #[test]
    fn sixteen_bit_texels_are_little_endian_with_the_first_component_lowest() {
        // pure red in 5-6-5 is 0x001f: bytes 1f 00
        assert_eq!(run(r5g6b5, &[0x1f, 0x00]), [255, 0, 0, 255]);
        assert_eq!(run(r5g6b5, &[0x00, 0xf8]), [0, 0, 255, 255]);
        assert_eq!(run(r5g6b5, &[0xe0, 0x07]), [0, 255, 0, 255]);
        // 5-5-5-1: red lowest, alpha the top bit
        assert_eq!(run(r5g5b5a1, &[0x1f, 0x80]), [255, 0, 0, 255]);
        assert_eq!(run(r5g5b5a1, &[0x00, 0x7c]), [0, 0, 255, 0]);
        // 1-5-5-5: alpha lowest, red the top five
        assert_eq!(run(a1b5g5r5, &[0x01, 0xf8]), [255, 0, 0, 255]);
        assert_eq!(run(a1b5g5r5, &[0x3e, 0x00]), [0, 0, 255, 0]);
        // 4-4-4-4: red the low nibble of the first byte
        assert_eq!(run(r4g4b4a4, &[0x0f, 0xf0]), [255, 0, 0, 255]);
        assert_eq!(run(r4g4, &[0xf0]), [0, 255]);
    }

    #[test]
    fn sixteen_bit_normalised_texels_become_half_floats() {
        assert_eq!(run(unorm16_to_half, &[0xff, 0xff]), f16_bits(1.0).to_le_bytes());
        assert_eq!(run(snorm16_to_half, &[0x01, 0x80]), f16_bits(-1.0).to_le_bytes());
        assert_eq!(run(snorm16_to_half, &[0xff, 0x7f]), f16_bits(1.0).to_le_bytes());
    }

    #[test]
    fn ten_bit_texels_end_with_red_lowest() {
        // a2b10g10r10 with red 1023 only: red is the top ten bits
        let v: u32 = 1023 << 22;
        assert_eq!(run(a2b10g10r10, &v.to_le_bytes()), 1023u32.to_le_bytes());
    }

    #[test]
    fn every_known_format_names_itself_and_says_how_well_it_is_read() {
        assert_eq!(format_support(0x1a).1, Support::Exact);
        assert_eq!(format_support(0x431).0, "srgb bc1 (0x431)");
        assert_eq!(format_support(0x1b).1, Support::Approximate);
        assert_eq!(format_support(0x11).1, Support::Unsupported);
        assert_eq!(format_support(0x816).1, Support::Exact);
    }
}
