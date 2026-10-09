//! Where a texel is in the Wii U GPU's tiled surfaces.
//!
//! The GPU stores textures and render targets in tiles: micro tiles of 8 x 8 texels, arranged in macro tiles that spread over the
//! memory pipes and banks. This gives the address of a texel in a surface from its coordinates, by the rules of AMD's R6xx/R7xx
//! address library (published under the MIT licence: Copyright 2014 Advanced Micro Devices, Inc.), which the console's graphics
//! library used to make the layout; this is a re-statement of the part that maps coordinates to addresses, with the console's
//! configuration (2 pipes, 4 banks, 256-byte pipe interleave, 2 KiB rows). Block-compressed formats count their 4 x 4 blocks as
//! the elements, with the element size in `bits_per_element`.

/// The memory configuration of the console's GPU.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub pipes: u32,
    pub banks: u32,
    pub pipe_interleave_bytes: u32,
    pub row_size: u32,
    pub swap_size: u32,
    pub split_size: u32,
}

/// What the console uses (its GPU is an R7xx with two pipes and four banks).
pub const LATTE: Config = Config {
    pipes: 2,
    banks: 4,
    pipe_interleave_bytes: 256,
    row_size: 2048,
    swap_size: 256,
    split_size: 2048,
};

/// How a surface is laid out (the `GX2TileMode` values are these numbers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileMode {
    LinearGeneral,
    LinearAligned,
    OneDThin1,
    OneDThick,
    TwoDThin1,
    TwoDThin2,
    TwoDThin4,
    TwoDThick,
    TwoBThin1,
    TwoBThin2,
    TwoBThin4,
    TwoBThick,
    ThreeDThin1,
    ThreeDThick,
    ThreeBThin1,
    ThreeBThick,
}

impl TileMode {
    /// From the number the game's surfaces carry (0 is "default", which GX2 resolves to a linear layout for tiny surfaces and a
    /// 2D-tiled one otherwise; the surfaces the game makes carry an explicit mode).
    pub fn from_number(n: u32) -> Option<TileMode> {
        use TileMode::*;
        Some(match n {
            0 => LinearGeneral,
            1 | 0x10 => LinearAligned,
            2 => OneDThin1,
            3 => OneDThick,
            4 => TwoDThin1,
            5 => TwoDThin2,
            6 => TwoDThin4,
            7 => TwoDThick,
            8 => TwoBThin1,
            9 => TwoBThin2,
            10 => TwoBThin4,
            11 => TwoBThick,
            12 => ThreeDThin1,
            13 => ThreeDThick,
            14 => ThreeBThin1,
            15 => ThreeBThick,
            _ => return None,
        })
    }

    fn thickness(self) -> u64 {
        use TileMode::*;
        match self {
            OneDThick | TwoDThick | TwoBThick | ThreeDThick | ThreeBThick => 4,
            _ => 1,
        }
    }

    fn is_thick_macro(self) -> bool {
        use TileMode::*;
        matches!(self, TwoDThick | TwoBThick | ThreeDThick | ThreeBThick)
    }

    fn is_bank_swapped(self) -> bool {
        use TileMode::*;
        matches!(self, TwoBThin1 | TwoBThin2 | TwoBThin4 | TwoBThick | ThreeBThin1 | ThreeBThick)
    }

    /// How many macro-tile heights tall a macro tile is, relative to the plain shape.
    fn aspect(self) -> u64 {
        use TileMode::*;
        match self {
            TwoDThin2 | TwoBThin2 => 2,
            TwoDThin4 | TwoBThin4 => 4,
            _ => 1,
        }
    }

    fn rotation(self, c: &Config) -> u64 {
        use TileMode::*;
        match self {
            TwoDThin1 | TwoDThin2 | TwoDThin4 | TwoDThick | TwoBThin1 | TwoBThin2 | TwoBThin4 | TwoBThick => {
                u64::from(c.pipes) * (u64::from(c.banks >> 1) - 1)
            }
            ThreeDThin1 | ThreeDThick | ThreeBThin1 | ThreeBThick => {
                if c.pipes >= 4 {
                    u64::from(c.pipes >> 1) - 1
                } else {
                    1
                }
            }
            _ => 0,
        }
    }
}

fn bit(v: u64, n: u32) -> u64 {
    (v >> n) & 1
}

/// The index of a texel inside its 8 x 8 micro tile (`z` matters for thick tiles; `depth` for the tile type of depth surfaces).
fn pixel_index(x: u64, y: u64, z: u64, bits: u32, mode: TileMode, depth: bool) -> u64 {
    let (x0, x1, x2) = (bit(x, 0), bit(x, 1), bit(x, 2));
    let (y0, y1, y2) = (bit(y, 0), bit(y, 1), bit(y, 2));
    let (z0, z1, z2) = (bit(z, 0), bit(z, 1), bit(z, 2));
    let thickness = mode.thickness();
    // The library has a third order for "thick" tiling, but the console's surfaces are always displayable (colour) or
    // non-displayable (depth) ones, thick or not: a thick tile keeps the 2D order and adds the slice bits on top.
    let (b0, b1, b2, b3, b4, b5) = if depth {
        (x0, y0, x1, y1, x2, y2)
    } else {
        match bits {
            8 => (x0, x1, x2, y1, y0, y2),
            16 => (x0, x1, x2, y0, y1, y2),
            64 => (x0, y0, x1, x2, y1, y2),
            128 => (y0, x0, x1, x2, y1, y2),
            _ => (x0, x1, y0, x2, y1, y2),
        }
    };
    let (b6, b7) = if thickness > 1 { (z0, z1) } else { (0, 0) };
    let b8 = if thickness == 8 { z2 } else { 0 };
    b0 | b1 << 1 | b2 << 2 | b3 << 3 | b4 << 4 | b5 << 5 | b6 << 6 | b7 << 7 | b8 << 8
}

fn pipe_of(x: u64, y: u64, c: &Config) -> u64 {
    let (x3, x4, x5) = (bit(x, 3), bit(x, 4), bit(x, 5));
    let (y3, y4, y5) = (bit(y, 3), bit(y, 4), bit(y, 5));
    match c.pipes {
        2 => y3 ^ x3,
        4 => (y3 ^ x4) | (y4 ^ x3) << 1,
        8 => (y3 ^ x5) | (y4 ^ x5 ^ x4) << 1 | (y5 ^ x3) << 2,
        _ => 0,
    }
}

fn bank_of(x: u64, y: u64, c: &Config) -> u64 {
    let (tx, ty) = (x / u64::from(c.banks), y / u64::from(c.pipes));
    let (x3, x4, x5) = (bit(x, 3), bit(x, 4), bit(x, 5));
    let (ty3, ty4, ty5) = (bit(ty, 3), bit(ty, 4), bit(ty, 5));
    match c.banks {
        4 => (ty4 ^ x3) | (ty3 ^ x4) << 1,
        8 => (ty5 ^ x3 ^ if c.pipes == 8 { bit(tx, 3) } else { 0 }) | (ty5 ^ ty4 ^ x4) << 1 | (ty3 ^ x5) << 2,
        _ => 0,
    }
}

/// Everything the address of a texel depends on.
#[derive(Debug, Clone, Copy)]
pub struct Surface {
    pub mode: TileMode,
    /// Bits in one element (a texel, or a 4 x 4 block of a compressed format).
    pub bits_per_element: u32,
    /// The pitch and height in elements, as the surface was laid out (padded).
    pub pitch: u32,
    pub height: u32,
    pub samples: u32,
    pub depth: bool,
    pub pipe_swizzle: u32,
    pub bank_swizzle: u32,
}

/// The byte offset of an element at (`x`, `y`) in slice `slice` of the surface (the bits of an element that is not a whole number
/// of bytes are not tracked).
pub fn address(s: &Surface, x: u32, y: u32, slice: u32, sample: u32) -> u64 {
    let c = &LATTE;
    let (x, y, slice) = (u64::from(x), u64::from(y), u64::from(slice));
    let bpp = u64::from(s.bits_per_element);
    let (pitch, height) = (u64::from(s.pitch), u64::from(s.height));
    let samples = u64::from(s.samples.max(1));
    let tile_bytes = |bits: u64| bits.div_ceil(8);
    match s.mode {
        TileMode::LinearGeneral | TileMode::LinearAligned => {
            let slice_size = pitch * height;
            (slice_size * (slice + u64::from(sample)) + y * pitch + x) * bpp / 8
        }
        TileMode::OneDThin1 | TileMode::OneDThick => {
            let thickness = s.mode.thickness();
            let micro_bytes = tile_bytes(64 * thickness * bpp);
            let (ix, iy, iz) = (x / 8, y / 8, slice / thickness);
            let micro_offset = micro_bytes * (ix + iy * (pitch / 8));
            let slice_bytes = tile_bytes(pitch * height * thickness * bpp);
            let pixel = pixel_index(x, y, slice, s.bits_per_element, s.mode, s.depth);
            (bpp * pixel) / 8 + micro_offset + iz * slice_bytes
        }
        _ => {
            let (pipes, banks) = (u64::from(c.pipes), u64::from(c.banks));
            let group_bits = u64::from(c.pipe_interleave_bytes.ilog2());
            let (pipe_bits, bank_bits) = (u64::from(c.pipes.ilog2()), u64::from(c.banks.ilog2()));
            let thickness = s.mode.thickness();
            let micro_bits = 64 * thickness * bpp * samples;
            let pixel = pixel_index(x, y, slice, s.bits_per_element, s.mode, s.depth);
            let sample_offset = u64::from(sample) * (micro_bits / samples);
            let elem_offset = bpp * pixel + sample_offset;
            let elem_offset = elem_offset / 8;
            let mut pipe = pipe_of(x, y, c);
            let mut bank = bank_of(x, y, c);
            let mut bank_pipe = pipe + pipes * bank;
            let rotation = s.mode.rotation(c);
            let swizzle = u64::from(s.pipe_swizzle) + pipes * u64::from(s.bank_swizzle);
            let slice_in = if s.mode.is_thick_macro() { slice / 4 } else { slice };
            bank_pipe ^= (swizzle + slice_in * rotation) % (pipes * banks);
            bank_pipe %= pipes * banks;
            pipe = bank_pipe % pipes;
            bank = bank_pipe / pipes;
            let slice_bytes = tile_bytes(pitch * height * thickness * bpp * samples);
            let slice_offset = slice_bytes * (slice / thickness);
            let (mut macro_pitch, mut macro_height) = (8 * banks, 8 * pipes);
            let aspect = s.mode.aspect();
            macro_pitch /= aspect;
            macro_height *= aspect;
            let macro_tiles_per_row = pitch / macro_pitch;
            let macro_bytes = tile_bytes(samples * thickness * bpp * macro_height * macro_pitch);
            let (mx, my) = (x / macro_pitch, y / macro_height);
            let macro_offset = macro_bytes * (mx + macro_tiles_per_row * my);
            if s.mode.is_bank_swapped() {
                let swap_width = bank_swapped_width(s.mode, s.bits_per_element, samples, pitch, c);
                if let Some(index) = (macro_pitch * mx).checked_div(swap_width) {
                    const ORDER: [u64; 8] = [0, 1, 3, 2, 6, 7, 5, 4];
                    bank ^= ORDER[(index & (banks - 1)) as usize];
                }
            }
            let group_mask = (1u64 << group_bits) - 1;
            let total = elem_offset + ((macro_offset + slice_offset) >> (bank_bits + pipe_bits));
            let high = (total & !group_mask) << (bank_bits + pipe_bits);
            let low = total & group_mask;
            (bank << (pipe_bits + group_bits)) | (pipe << group_bits) | low | high
        }
    }
}

/// How wide a bank-swapped surface is swapped (in elements).
fn bank_swapped_width(mode: TileMode, bits: u32, samples: u64, pitch: u64, c: &Config) -> u64 {
    let bpp = u64::from(bits);
    let (banks, pipes) = (u64::from(c.banks), u64::from(c.pipes));
    let (swap_size, row_size, split_size, group_size) = (
        u64::from(c.swap_size),
        u64::from(c.row_size),
        u64::from(c.split_size),
        u64::from(c.pipe_interleave_bytes),
    );
    let bytes_per_sample = 8 * bpp;
    let samples_per_tile = split_size / bytes_per_sample;
    let slices_per_tile = samples.checked_div(samples_per_tile).map_or(1, |n| n.max(1));
    let samples = if mode.is_thick_macro() { 4 } else { samples };
    let bytes_per_tile_slice = samples * bytes_per_sample / slices_per_tile;
    if !mode.is_bank_swapped() {
        return 0;
    }
    let factor = mode.aspect();
    let swap_tiles = ((swap_size >> 1) / bpp).max(1);
    let swap_width = swap_tiles * 8 * banks;
    let height_bytes = samples * factor * pipes * bpp / slices_per_tile;
    let swap_max = pipes * banks * row_size / height_bytes;
    let swap_min = group_size * 8 * banks / bytes_per_tile_slice;
    let mut width = swap_max.min(swap_min.max(swap_width));
    while width >= 2 * pitch && width > 0 {
        width >>= 1;
    }
    width
}

/// The pipe and bank swizzle of a surface from its `swizzle` field (bits 8 to 10 hold the pipe and bank).
pub fn swizzles(swizzle_field: u32) -> (u32, u32) {
    let base256 = (swizzle_field >> 8) & 0xff;
    let group = LATTE.pipe_interleave_bytes >> 8;
    (
        (base256 / group) & (LATTE.pipes - 1),
        (base256 / group / LATTE.pipes) & (LATTE.banks - 1),
    )
}

/// What the surface calculation is asked (the console's `GX2CalcSurfaceSizeAndAlignment` asks this for each mip level).
#[derive(Debug, Clone, Copy)]
pub struct Request {
    pub mode: TileMode,
    /// Bits in one element (a texel, or a 4 x 4 block of a compressed format).
    pub bits_per_element: u32,
    /// Whether the elements are 4 x 4 blocks (the size is then in blocks).
    pub compressed: bool,
    /// The size of this mip level, in texels.
    pub width: u32,
    pub height: u32,
    pub slices: u32,
    pub samples: u32,
    pub level: u32,
    pub cube: bool,
    pub depth: bool,
    pub display: bool,
}

/// What it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub mode: TileMode,
    pub pitch: u32,
    pub height: u32,
    pub slices: u32,
    pub size: u64,
    pub base_align: u32,
    pub pitch_align: u32,
    pub height_align: u32,
}

fn next_pow2(v: u32) -> u32 {
    v.max(1).next_power_of_two()
}

fn align_up(v: u32, a: u32) -> u32 {
    if a.is_power_of_two() {
        (v + a - 1) & !(a - 1)
    } else {
        v.div_ceil(a) * a
    }
}

fn non_bank_swapped(m: TileMode) -> TileMode {
    use TileMode::*;
    match m {
        TwoBThin1 => TwoDThin1,
        TwoBThin2 => TwoDThin2,
        TwoBThin4 => TwoDThin4,
        TwoBThick => TwoDThick,
        ThreeBThin1 => ThreeDThin1,
        ThreeBThick => ThreeDThick,
        m => m,
    }
}

fn degrade(m: TileMode, samples: u32, slices: u32, depth: bool, c: &Config) -> TileMode {
    use TileMode::*;
    let (group, split) = (c.pipe_interleave_bytes, c.split_size);
    match m {
        OneDThick if samples == 2 || samples == 4 => TwoDThick,
        OneDThick if samples > 1 || depth => OneDThin1,
        TwoDThin2 if 2 * group > split => TwoDThin1,
        TwoDThin4 if 4 * group > split => TwoDThin2,
        TwoDThick if samples > 1 || slices > 1 || depth => TwoDThin1,
        TwoBThin2 if 2 * group > split => TwoBThin1,
        TwoBThin4 if 4 * group > split => TwoBThin2,
        TwoBThick if samples > 1 || slices > 1 || depth => TwoBThin1,
        ThreeDThick if samples > 1 || slices > 1 || depth => ThreeDThin1,
        ThreeBThick if samples > 1 || slices > 1 || depth => ThreeBThin1,
        m => m,
    }
}

fn tile_slices(m: TileMode, bpp: u32, samples: u32, c: &Config) -> u32 {
    let bytes_per_sample = (bpp * 64).div_ceil(8);
    let samples = if m.thickness() > 1 { 4 } else { samples };
    c.split_size
        .checked_div(bytes_per_sample)
        .and_then(|per_tile| samples.checked_div(per_tile))
        .map_or(1, |n| n.max(1))
}

/// The layout a mip level really gets: small levels of 2D-tiled surfaces fall back to 1D tiling.
fn level_mode(base: TileMode, r: &Request, c: &Config) -> TileMode {
    use TileMode::*;
    let bpp = r.bits_per_element;
    let step = |m: TileMode| {
        let slices = tile_slices(m, bpp, r.samples, c);
        let mut m = degrade(m, r.samples, slices, r.depth, c);
        if m.rotation(c).is_multiple_of(u64::from(c.pipes)) {
            m = match m {
                ThreeDThin1 => TwoDThin1,
                ThreeDThick => TwoDThick,
                ThreeBThin1 => TwoBThin1,
                ThreeBThick => TwoBThick,
                m => m,
            };
        }
        m
    };
    let mut m = step(base);
    if r.level == 0 {
        return m;
    }
    let bpp = if matches!(bpp, 96 | 48 | 24) { bpp / 3 } else { bpp };
    let (width, height, slices) = (next_pow2(r.width), next_pow2(r.height), next_pow2(r.slices));
    m = non_bank_swapped(m);
    let micro_bytes = (r.samples * bpp * m.thickness() as u32 * 64).div_ceil(8);
    let width_factor = if micro_bytes <= c.pipe_interleave_bytes {
        c.pipe_interleave_bytes / micro_bytes
    } else {
        1
    };
    let (mut macro_w, mut macro_h) = (8 * c.banks, 8 * c.pipes);
    match m {
        TwoDThin1 | ThreeDThin1 => {
            if width < width_factor * macro_w || height < macro_h {
                m = OneDThin1;
            }
        }
        TwoDThin2 => {
            macro_w >>= 1;
            macro_h *= 2;
            if width < width_factor * macro_w || height < macro_h {
                m = OneDThin1;
            }
        }
        TwoDThin4 => {
            macro_w >>= 2;
            macro_h *= 4;
            if width < width_factor * macro_w || height < macro_h {
                m = OneDThin1;
            }
        }
        TwoDThick | ThreeDThick if width < width_factor * macro_w || height < macro_h => m = OneDThick,
        _ => {}
    }
    if slices < 4 {
        m = match m {
            OneDThick => OneDThin1,
            TwoDThick => TwoDThin1,
            ThreeDThick => ThreeDThin1,
            m => m,
        };
    }
    step(m)
}

struct Dims<'a> {
    pitch: &'a mut u32,
    height: &'a mut u32,
    slices: &'a mut u32,
}

fn pad(mode: TileMode, mut pad_dims: u32, d: Dims, pitch_align: u32, height_align: u32, slice_align: u32, cube: bool) {
    let thickness = mode.thickness() as u32;
    if pad_dims == 0 {
        pad_dims = 3;
    }
    *d.pitch = align_up(*d.pitch, pitch_align);
    if pad_dims > 1 {
        *d.height = align_up(*d.height, height_align);
    }
    if pad_dims > 2 || thickness > 1 {
        if cube {
            *d.slices = next_pow2(*d.slices);
        }
        if thickness > 1 {
            *d.slices = align_up(*d.slices, slice_align);
        }
    }
}

fn adjust_pitch(display: bool, pitch_align: &mut u32) {
    if display {
        *pitch_align = align_up(*pitch_align, 32);
    }
}

/// (base alignment, pitch alignment, height alignment)
fn macro_alignments(m: TileMode, bpp: u32, samples: u32, c: &Config) -> (u32, u32, u32) {
    let aspect = m.aspect() as u32;
    let thickness = m.thickness() as u32;
    let bpp = if matches!(bpp, 96 | 48 | 24) { bpp / 3 } else { bpp };
    let bpp = if bpp == 3 { 1 } else { bpp };
    let macro_w = 8 * c.banks / aspect;
    let macro_h = aspect * 8 * c.pipes;
    let pitch_align = macro_w.max(macro_w * (c.pipe_interleave_bytes / bpp / (8 * thickness) / samples));
    let height_align = macro_h;
    // (the R6xx parts double this for single-sampled surfaces and want the base on an even number of macro tiles; the console's
    // chip is an R7xx, which does neither)
    let macro_bytes = samples * (bpp * macro_h * macro_w).div_ceil(8);
    let mut base = if thickness == 1 {
        macro_bytes.max((samples * height_align * bpp * pitch_align).div_ceil(8))
    } else {
        c.pipe_interleave_bytes.max((4 * height_align * bpp * pitch_align).div_ceil(8))
    };
    let micro_bytes = (thickness * samples * bpp * 64).div_ceil(8);
    let per_micro = if micro_bytes >= c.split_size {
        micro_bytes / c.split_size
    } else {
        1
    };
    base /= per_micro;
    (base, pitch_align, height_align)
}

fn size_of(height: u32, pitch: u32, slices: u32, bits: u32, samples: u32) -> u64 {
    (u64::from(height) * u64::from(pitch) * u64::from(slices) * u64::from(bits) * u64::from(samples)).div_ceil(8)
}

/// How big a surface level is and how it must be aligned: the part of AMD's library that `GX2CalcSurfaceSizeAndAlignment`
/// uses, for a request that is one mip level of one surface.
pub fn surface_info(r: &Request) -> Info {
    use TileMode::*;
    let c = &LATTE;
    let samples = r.samples.max(1);
    let (mut width, mut height) = (r.width.max(1), r.height.max(1));
    if r.compressed {
        // the base is padded to whole blocks, the smaller levels to a power of two first
        if r.level == 0 {
            width = align_up(width, 4);
            height = align_up(height, 4);
        } else {
            width = next_pow2(width);
            height = next_pow2(height);
        }
        (width, height) = (width.div_ceil(4), height.div_ceil(4));
    }
    let bpp = r.bits_per_element;
    let base_mode = r.mode;
    let mut mode = level_mode(base_mode, &Request { width, height, ..*r }, c);
    let mut pad_dims = if r.cube && r.level == 0 { 2 } else { 0 };
    let (mut pitch, mut hgt, mut slices) = (width, height, r.slices.max(1));
    let mip = r.level > 0;
    if mip {
        pitch = next_pow2(pitch);
        hgt = next_pow2(hgt);
        if r.cube {
            pad_dims = if slices <= 1 { 2 } else { 0 };
        } else {
            slices = next_pow2(slices);
        }
    }
    match mode {
        LinearGeneral | LinearAligned => {
            let (base_align, mut pitch_align) = match base_mode {
                LinearGeneral => (1, if bpp != 1 { 1 } else { 8 }),
                LinearAligned => (c.pipe_interleave_bytes, 64.max(8 * c.pipe_interleave_bytes / bpp)),
                _ => (1, 1),
            };
            adjust_pitch(r.display, &mut pitch_align);
            pad(
                mode,
                pad_dims,
                Dims {
                    pitch: &mut pitch,
                    height: &mut hgt,
                    slices: &mut slices,
                },
                pitch_align,
                1,
                1,
                r.cube,
            );
            let size = size_of(hgt, pitch, slices, bpp, samples);
            Info {
                mode,
                pitch,
                height: hgt,
                slices,
                size,
                base_align,
                pitch_align,
                height_align: 1,
            }
        }
        OneDThin1 | OneDThick => micro(r, mode, pad_dims, pitch, hgt, slices),
        _ => {
            if mip && mode == TwoDThick && slices < 4 {
                mode = TwoDThin1;
            }
            if mode != base_mode && mip && base_mode.is_thick_macro() && !mode.is_thick_macro() {
                let (_, pa, ha) = macro_alignments(base_mode, bpp, samples, c);
                let factor = ((c.pipe_interleave_bytes >> 3) / bpp).max(1);
                if pitch < pa * factor || hgt < ha {
                    return micro(r, OneDThin1, pad_dims, pitch, hgt, slices);
                }
            }
            let thickness = mode.thickness() as u32;
            let (base_align, mut pitch_align, height_align) = macro_alignments(mode, bpp, samples, c);
            let swapped = bank_swapped_width(mode, bpp, u64::from(samples), u64::from(pitch), c) as u32;
            pitch_align = pitch_align.max(swapped);
            // (no dual pitch alignment: the library does that for R6xx parts only)
            pad(
                mode,
                pad_dims,
                Dims {
                    pitch: &mut pitch,
                    height: &mut hgt,
                    slices: &mut slices,
                },
                pitch_align,
                height_align,
                thickness,
                r.cube,
            );
            let size = size_of(hgt, pitch, slices, bpp, samples);
            Info {
                mode,
                pitch,
                height: hgt,
                slices,
                size,
                base_align,
                pitch_align,
                height_align,
            }
        }
    }
}

fn micro(r: &Request, mut mode: TileMode, mut pad_dims: u32, mut pitch: u32, mut hgt: u32, mut slices: u32) -> Info {
    let c = &LATTE;
    let samples = r.samples.max(1);
    if r.level > 0 {
        if r.cube {
            pad_dims = if slices <= 1 { 2 } else { 0 };
        }
        if mode == TileMode::OneDThick && slices < 4 {
            mode = TileMode::OneDThin1;
        }
    }
    let thickness = mode.thickness() as u32;
    let bpp = if matches!(r.bits_per_element, 96 | 48 | 24) {
        r.bits_per_element / 3
    } else {
        r.bits_per_element
    };
    let mut pitch_align = 8.max(c.pipe_interleave_bytes / bpp / samples / thickness);
    adjust_pitch(r.display, &mut pitch_align);
    pad(
        mode,
        pad_dims,
        Dims {
            pitch: &mut pitch,
            height: &mut hgt,
            slices: &mut slices,
        },
        pitch_align,
        8,
        thickness,
        r.cube,
    );
    let size = size_of(hgt, pitch, slices, r.bits_per_element, samples);
    Info {
        mode,
        pitch,
        height: hgt,
        slices,
        size,
        base_align: c.pipe_interleave_bytes,
        pitch_align,
        height_align: 8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn surface(mode: TileMode, bits: u32, pitch: u32, height: u32) -> Surface {
        Surface {
            mode,
            bits_per_element: bits,
            pitch,
            height,
            samples: 1,
            depth: false,
            pipe_swizzle: 0,
            bank_swizzle: 0,
        }
    }

    #[test]
    fn a_linear_surface_is_row_by_row() {
        let s = surface(TileMode::LinearAligned, 32, 64, 16);
        assert_eq!(address(&s, 0, 0, 0, 0), 0);
        assert_eq!(address(&s, 5, 0, 0, 0), 20);
        assert_eq!(address(&s, 0, 3, 0, 0), 3 * 64 * 4);
    }

    #[test]
    fn every_tiled_layout_puts_each_texel_in_its_own_place_inside_the_surface() {
        for mode in [
            TileMode::OneDThin1,
            TileMode::TwoDThin1,
            TileMode::TwoBThin1,
            TileMode::TwoDThin2,
            TileMode::TwoBThin4,
        ] {
            for bits in [8, 16, 32, 64, 128] {
                // a surface of 128 x 64 elements (a whole number of the largest macro tiles)
                let s = surface(mode, bits, 128, 64);
                let size = 128u64 * 64 * u64::from(bits) / 8;
                let mut seen = HashSet::new();
                for y in 0..64 {
                    for x in 0..128 {
                        let a = address(&s, x, y, 0, 0);
                        assert!(a + u64::from(bits) / 8 <= size, "{mode:?} {bits}: ({x},{y}) at {a} of {size}");
                        assert!(seen.insert(a), "{mode:?} {bits}: two texels at {a}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_first_texels_of_a_micro_tile_follow_the_bit_order_of_the_format() {
        // 32 bits: x0, x1, y0, x2, y1, y2
        let s = surface(TileMode::OneDThin1, 32, 8, 8);
        let at = |x, y| address(&s, x, y, 0, 0) / 4;
        assert_eq!((at(1, 0), at(2, 0), at(0, 1), at(4, 0), at(0, 2), at(0, 4)), (1, 2, 4, 8, 16, 32));
    }

    fn request(mode: TileMode, bits: u32, w: u32, h: u32, level: u32) -> Request {
        Request {
            mode,
            bits_per_element: bits,
            compressed: false,
            width: w,
            height: h,
            slices: 1,
            samples: 1,
            level,
            cube: false,
            depth: false,
            display: false,
        }
    }

    #[test]
    fn a_wide_color_buffer_keeps_its_size_when_it_fills_whole_macro_tiles() {
        let i = surface_info(&request(TileMode::TwoDThin1, 32, 1280, 720, 0));
        assert_eq!((i.pitch, i.height, i.size), (1280, 720, 1280 * 720 * 4));
    }

    #[test]
    fn small_levels_of_a_tiled_surface_fall_back_to_1d_tiling() {
        let big = surface_info(&request(TileMode::TwoDThin1, 32, 256, 256, 0));
        assert_eq!(big.mode, TileMode::TwoDThin1);
        let small = surface_info(&request(TileMode::TwoDThin1, 32, 16, 16, 4));
        assert_eq!(small.mode, TileMode::OneDThin1);
        assert_eq!((small.pitch, small.height), (16, 16));
    }

    #[test]
    fn a_compressed_surface_is_sized_in_blocks() {
        let r = Request {
            compressed: true,
            ..request(TileMode::OneDThin1, 64, 64, 64, 0)
        };
        let i = surface_info(&r);
        assert_eq!(
            (i.pitch, i.height, i.size),
            (16u32.max(i.pitch_align), 16, u64::from(i.pitch) * 16 * 8)
        );
    }
}

/// Checks against vectors computed with AMD's address library (see `tiling_vectors.txt` for how they were made).
#[cfg(test)]
mod reference {
    use super::*;

    /// FNV-1a over the little-endian bytes of every address, in slice, row, column order.
    fn hash(addresses: impl Iterator<Item = u64>) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for a in addresses {
            for b in a.to_le_bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        }
        h
    }

    /// Compares every line of a vector file; returns the lines that differ.
    pub(super) fn mismatches(text: &str) -> Vec<String> {
        let mut bad = Vec::new();
        for line in text.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
            let f: Vec<&str> = line.split_whitespace().collect();
            let n = |i: usize| f[i].parse::<u64>().unwrap();
            let (mode, bits, format) = (
                n(0) as u32,
                n(1) as u32,
                u32::from_str_radix(f[2].trim_start_matches("0x"), 16).unwrap(),
            );
            let (w, h, slices, cube, level, base256) = (n(3) as u32, n(4) as u32, n(5) as u32, n(6) == 1, n(7) as u32, n(8) as u32);
            let want = (
                n(9) as u32,
                n(10) as u32,
                n(11) as u32,
                n(13),
                n(14) as u32,
                u64::from_str_radix(f[15], 16).unwrap(),
            );
            let compressed = format >= 0x31;
            let info = surface_info(&Request {
                mode: TileMode::from_number(mode).unwrap(),
                bits_per_element: bits,
                compressed,
                width: w,
                height: h,
                slices,
                samples: 1,
                level,
                cube,
                depth: false,
                display: false,
            });
            let (pipe_swizzle, bank_swizzle) = swizzles(base256 << 8);
            let s = Surface {
                mode: info.mode,
                bits_per_element: bits,
                pitch: info.pitch,
                height: info.height,
                samples: 1,
                depth: false,
                pipe_swizzle,
                bank_swizzle,
            };
            let (ew, eh) = if compressed { (w.div_ceil(4), h.div_ceil(4)) } else { (w, h) };
            let addresses = (0..slices.min(4))
                .flat_map(|z| (0..eh).flat_map(move |y| (0..ew).map(move |x| (x, y, z))))
                .map(|(x, y, z)| address(&s, x, y, z, 0));
            let got = (
                info.mode as u32,
                info.pitch,
                info.height,
                info.size,
                info.base_align,
                hash(addresses),
            );
            if got != want {
                bad.push(format!(
                    "{line}\n    got mode {} pitch {} height {} size {} align {} hash {:016x}",
                    got.0, got.1, got.2, got.3, got.4, got.5
                ));
            }
        }
        bad
    }

    #[test]
    fn layouts_and_addresses_match_the_address_library() {
        let bad = mismatches(include_str!("tiling_vectors.txt"));
        assert!(bad.is_empty(), "{} differ:\n{}", bad.len(), bad.join("\n"));
    }

    /// The full set, when a file is given (`MH3U_TILING_VECTORS`).
    #[test]
    fn layouts_match_an_extra_vector_file() {
        if let Ok(path) = std::env::var("MH3U_TILING_VECTORS") {
            let bad = mismatches(&std::fs::read_to_string(path).unwrap());
            assert!(
                bad.is_empty(),
                "{} differ:\n{}",
                bad.len(),
                bad.iter().take(100000).cloned().collect::<Vec<_>>().join("\n")
            );
        }
    }
}
