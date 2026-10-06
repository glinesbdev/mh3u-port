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
            select: [(comp >> 24) & 7, (comp >> 16) & 7, (comp >> 8) & 7, comp & 7],
        }
    }
}

/// What a texture becomes on the GPU: its size, format and the bytes of level 0 (every layer, one after the other).
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub layers: u32,
    pub format: wgpu::TextureFormat,
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

fn copy(e: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(e);
}

/// Swaps the bytes inside every 16-bit word (the way the console stores compressed blocks).
fn swap16(e: &[u8], out: &mut Vec<u8>) {
    for w in e.as_chunks::<2>().0 {
        out.extend_from_slice(&[w[1], w[0]]);
    }
}

/// Reverses the bytes of every 32-bit word.
fn swap32(e: &[u8], out: &mut Vec<u8>) {
    for w in e.as_chunks::<4>().0 {
        out.extend_from_slice(&[w[3], w[2], w[1], w[0]]);
    }
}

fn r5g6b5(e: &[u8], out: &mut Vec<u8>) {
    let v = u16::from_be_bytes([e[0], e[1]]);
    let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
    out.extend_from_slice(&[(r * 255 / 31) as u8, (g * 255 / 63) as u8, (b * 255 / 31) as u8, 255]);
}

fn a1b5g5r5(e: &[u8], out: &mut Vec<u8>) {
    let v = u16::from_be_bytes([e[0], e[1]]);
    let (a, b, g, r) = ((v >> 15) & 1, (v >> 10) & 31, (v >> 5) & 31, v & 31);
    out.extend_from_slice(&[(r * 255 / 31) as u8, (g * 255 / 31) as u8, (b * 255 / 31) as u8, (a * 255) as u8]);
}

fn r5g5b5a1(e: &[u8], out: &mut Vec<u8>) {
    let v = u16::from_be_bytes([e[0], e[1]]);
    let (r, g, b, a) = ((v >> 11) & 31, (v >> 6) & 31, (v >> 1) & 31, v & 1);
    out.extend_from_slice(&[(r * 255 / 31) as u8, (g * 255 / 31) as u8, (b * 255 / 31) as u8, (a * 255) as u8]);
}

fn r4g4b4a4(e: &[u8], out: &mut Vec<u8>) {
    let v = u16::from_be_bytes([e[0], e[1]]);
    let n = |s: u32| (((v >> s) & 15) * 17) as u8;
    out.extend_from_slice(&[n(12), n(8), n(4), n(0)]);
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

fn unorm16_to_half(e: &[u8], out: &mut Vec<u8>) {
    for w in e.as_chunks::<2>().0 {
        let v = f32::from(u16::from_be_bytes([w[0], w[1]])) / 65535.0;
        out.extend_from_slice(&f16_bits(v).to_le_bytes());
    }
}

fn a2b10g10r10(e: &[u8], out: &mut Vec<u8>) {
    // the console's 2-10-10-10: alpha in the top two bits, then red, green and blue
    let v = u32::from_be_bytes([e[0], e[1], e[2], e[3]]);
    let (a, r, g, b) = ((v >> 30) & 3, (v >> 20) & 0x3ff, (v >> 10) & 0x3ff, v & 0x3ff);
    out.extend_from_slice(&(r | g << 10 | b << 20 | a << 30).to_le_bytes());
}

fn info(format: u32) -> Option<Info> {
    use wgpu::TextureFormat as F;
    let (hw, kind) = (format & 0x3f, format >> 8);
    let srgb = kind == 4;
    let snorm = kind == 2;
    Some(match hw {
        0x01 => Info {
            bits: 8,
            block: 1,
            format: if snorm { F::R8Snorm } else { F::R8Unorm },
            convert: copy,
        },
        0x07 => Info {
            bits: 16,
            block: 1,
            format: if snorm { F::Rg8Snorm } else { F::Rg8Unorm },
            convert: copy,
        },
        0x1a => Info {
            bits: 32,
            block: 1,
            format: if srgb {
                F::Rgba8UnormSrgb
            } else if snorm {
                F::Rgba8Snorm
            } else {
                F::Rgba8Unorm
            },
            convert: copy,
        },
        0x08 => Info {
            bits: 16,
            block: 1,
            format: F::Rgba8Unorm,
            convert: r5g6b5,
        },
        0x0a => Info {
            bits: 16,
            block: 1,
            format: F::Rgba8Unorm,
            convert: a1b5g5r5,
        },
        0x0c => Info {
            bits: 16,
            block: 1,
            format: F::Rgba8Unorm,
            convert: r5g5b5a1,
        },
        0x0b => Info {
            bits: 16,
            block: 1,
            format: F::Rgba8Unorm,
            convert: r4g4b4a4,
        },
        0x05 => Info {
            bits: 16,
            block: 1,
            format: F::R16Float,
            convert: unorm16_to_half,
        },
        0x06 => Info {
            bits: 16,
            block: 1,
            format: F::R16Float,
            convert: swap16,
        },
        0x0f => Info {
            bits: 32,
            block: 1,
            format: F::Rg16Float,
            convert: unorm16_to_half,
        },
        0x10 => Info {
            bits: 32,
            block: 1,
            format: F::Rg16Float,
            convert: swap16,
        },
        0x1f => Info {
            bits: 64,
            block: 1,
            format: F::Rgba16Float,
            convert: unorm16_to_half,
        },
        0x20 => Info {
            bits: 64,
            block: 1,
            format: F::Rgba16Float,
            convert: swap16,
        },
        0x0e => Info {
            bits: 32,
            block: 1,
            format: F::R32Float,
            convert: swap32,
        },
        0x1e => Info {
            bits: 64,
            block: 1,
            format: F::Rg32Float,
            convert: swap32,
        },
        0x23 => Info {
            bits: 128,
            block: 1,
            format: F::Rgba32Float,
            convert: swap32,
        },
        0x19 => Info {
            bits: 32,
            block: 1,
            format: F::Rgb10a2Unorm,
            convert: a2b10g10r10,
        },
        0x31 => Info {
            bits: 64,
            block: 4,
            format: if srgb { F::Bc1RgbaUnormSrgb } else { F::Bc1RgbaUnorm },
            convert: copy,
        },
        0x32 => Info {
            bits: 128,
            block: 4,
            format: if srgb { F::Bc2RgbaUnormSrgb } else { F::Bc2RgbaUnorm },
            convert: copy,
        },
        0x33 => Info {
            bits: 128,
            block: 4,
            format: if srgb { F::Bc3RgbaUnormSrgb } else { F::Bc3RgbaUnorm },
            convert: copy,
        },
        0x34 => Info {
            bits: 64,
            block: 4,
            format: if snorm { F::Bc4RSnorm } else { F::Bc4RUnorm },
            convert: copy,
        },
        0x35 => Info {
            bits: 128,
            block: 4,
            format: if snorm { F::Bc5RgSnorm } else { F::Bc5RgUnorm },
            convert: copy,
        },
        _ => return None,
    })
}

/// Reads level 0 of a texture from the guest's memory.
pub fn decode(g: &Guest, h: &Header) -> Result<Decoded, String> {
    let Some(info) = info(h.format) else {
        return Err(format!("the texture format {:#x}", h.format));
    };
    let Some(mode) = TileMode::from_number(h.tile_mode) else {
        return Err(format!("the tile mode {}", h.tile_mode));
    };
    if h.width == 0 || h.height.max(1) > 16384 || h.width > 16384 || h.image == 0 {
        return Err("an empty or huge texture".into());
    }
    let layers = match h.dim {
        3 => 6,
        4..=7 => h.depth.max(1),
        2 => return Err("a 3D texture".into()),
        _ => 1,
    };
    let height = if h.dim == 0 { 1 } else { h.height.max(1) };
    let block = info.block;
    let (bw, bh) = (h.width.div_ceil(block), height.div_ceil(block));
    let elem_bytes = (info.bits / 8) as usize;
    // the pitch in the header counts texels; the layout works in elements
    let pitch = (h.pitch.max(h.width)).div_ceil(block);
    let (pipe_swizzle, bank_swizzle) = tiling::swizzles(h.swizzle);
    // the size as laid out, by the same rules the game's own size calculation used
    let laid = tiling::surface_info(&tiling::Request {
        mode,
        bits_per_element: info.bits,
        compressed: block > 1,
        width: h.width,
        height,
        slices: if h.dim == 3 { layers } else { 1 },
        samples: 1,
        level: 0,
        cube: h.dim == 3,
        depth: false,
        display: false,
    });
    let surface = Surface {
        mode: laid.mode,
        bits_per_element: info.bits,
        pitch: pitch.max(laid.pitch),
        height: laid.height,
        samples: 1,
        depth: false,
        pipe_swizzle,
        bank_swizzle,
    };
    let mut data = Vec::new();
    let mut element = vec![0u8; elem_bytes];
    let probe = u64::from(h.image_size.max(1));
    let mut out_of_range = 0u32;
    for layer in 0..layers {
        for y in 0..bh {
            for x in 0..bw {
                let offset = tiling::address(&surface, x, y, layer, 0);
                if offset + elem_bytes as u64 > probe * u64::from(layers.max(1)) + 4096 {
                    out_of_range += 1;
                }
                g.read(h.image.wrapping_add(offset as u32), &mut element);
                (info.convert)(&element, &mut data);
            }
        }
    }
    let _ = out_of_range;
    let texel_bytes = data.len() / (bw * bh * layers) as usize;
    Ok(Decoded {
        width: h.width.next_multiple_of(block),
        height: height.next_multiple_of(block),
        layers,
        format: info.format,
        bytes_per_row: bw * texel_bytes as u32,
        data,
    })
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
