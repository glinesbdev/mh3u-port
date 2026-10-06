//! The graphics API (`gx2`). For now only what the game needs to size its buffers: nothing is drawn yet.
//!
//! The surface sizes follow the console's tiling rules (see `mh3u_formats::tiling`), as `GX2CalcSurfaceSizeAndAlignment` does.

use crate::cafe::SimpleHandler;
use crate::guest::Guest;
use mh3u_formats::tiling::{self, TileMode};

pub fn find(name: &str) -> Option<SimpleHandler> {
    Some(match name {
        "gx2:GX2CalcSurfaceSizeAndAlignment" => |_, g, c| calc_surface(g, c.r[3]),
        // the GPU generation the shader files are checked against (Latte is 2)
        "gx2:GX2TempGetGPUVersion" => |_, _, c| c.r[3] = 2,
        "gx2:GX2CalcTVSize" => |_, g, c| {
            // (render mode, format, buffering mode, *size, *scale needed)
            let (w, h) = match c.r[3] {
                1 | 2 => (854, 480),
                3 => (1280, 720),
                4 => (1280, 720),
                _ => (1920, 1080),
            };
            write_size(g, c.r[6], c.r[7], w, h, c.r[4], c.r[5]);
        },
        "gx2:GX2CalcDRCSize" => |_, g, c| write_size(g, c.r[6], c.r[7], 854, 480, c.r[4], c.r[5]),
        _ => return None,
    })
}

/// Bytes per element (per 4x4 block for the compressed formats) and whether the format is block compressed.
fn element(format: u32) -> (u32, bool) {
    match format & 0x3f {
        0x01..=0x04 => (1, false),
        0x05..=0x0c => (2, false),
        0x0d..=0x1a => (4, false),
        0x1b..=0x1e => (8, false),
        0x1f..=0x23 => (16, false),
        0x31 | 0x34 => (8, true),
        0x32 | 0x33 | 0x35 => (16, true),
        _ => (4, false),
    }
}

fn align(v: u32, to: u32) -> u32 {
    v.div_ceil(to) * to
}

/// The size of a display buffer set: the bytes the game must allocate, and whether the picture needs scaling (it does not here).
fn write_size(g: &Guest, size: u32, scale: u32, w: u32, h: u32, format: u32, buffering: u32) {
    let (bpp, _) = element(format);
    let one = align(align(w, 64) * align(h, 8) * bpp, 0x1000);
    if size != 0 {
        g.put_u32(size, one * buffering.max(1));
    }
    if scale != 0 {
        g.put_u32(scale, 0);
    }
}

/// The number of mip levels a surface can have (its largest side's bits).
fn level_count(dim: u32, w: u32, h: u32, d: u32, asked: u32) -> u32 {
    if asked <= 1 {
        return 1;
    }
    let bits = |v: u32| 32 - v.leading_zeros();
    let mut levels = bits(w).max(bits(h));
    if dim == 2 {
        levels = levels.max(bits(d));
    }
    levels
}

/// `GX2CalcSurfaceSizeAndAlignment(surface)`: resolves the tile mode and fills in the image size, mip size, alignment, pitch,
/// swizzle and the mip offsets, by the address library's rules for each level (the order of the steps is the console library's).
fn calc_surface(g: &Guest, s: u32) {
    let (dim, width, height, depth, asked, format, aa, usage) = (
        g.u32(s),
        g.u32(s + 4),
        g.u32(s + 8),
        g.u32(s + 0xc),
        g.u32(s + 0x10),
        g.u32(s + 0x14),
        g.u32(s + 0x18),
        g.u32(s + 0x1c),
    );
    let (bytes, compressed) = element(format);
    let is_depth = usage & 4 != 0;
    let is_color = usage & 2 != 0;
    let mut mode = g.u32(s + 0x30);
    let mut changed = false;
    if mode == 0 {
        if dim != 0 || aa != 0 || is_depth {
            mode = if dim != 2 || is_color {
                TileMode::TwoDThin1
            } else {
                TileMode::TwoDThick
            } as u32;
            changed = true;
        } else {
            mode = TileMode::LinearAligned as u32;
        }
    }
    let levels = asked.max(1).min(level_count(dim, width, height, depth, asked));
    let mut swizzle = g.u32(s + 0x34) & 0xff00ffff;
    let tiled = |m: u32| m >= TileMode::TwoDThin1 as u32 && m != 0x10;
    if tiled(mode) {
        swizzle |= 0xd0000;
    }
    let mut last = mode;
    let (mut prev_size, mut offset0) = (0u32, 0u32);
    let mut offsets = [0u32; 14];
    let (mut image_size, mut alignment, mut pitch) = (0, 0, 0);
    for level in 0..levels {
        let ask = |mode: u32| {
            let mip = |v: u32| (v >> level).max(1);
            let slices = match dim {
                0 => 1,
                2 => mip(depth),
                3 => depth.max(6),
                4 | 5 | 7 => depth,
                _ => 1,
            };
            tiling::surface_info(&tiling::Request {
                mode: TileMode::from_number(mode).unwrap_or(TileMode::LinearAligned),
                bits_per_element: bytes * 8,
                compressed,
                width: mip(width),
                height: if dim == 0 || dim == 4 { 1 } else { mip(height) },
                slices: slices.max(1),
                samples: 1 << aa,
                level,
                cube: dim == 3,
                depth: is_depth,
                display: usage & 8 != 0,
            })
        };
        let mut info = ask(mode);
        if level > 0 {
            let mut pad = 0;
            if tiled(last) && (info.mode as u32) < TileMode::TwoDThin1 as u32 {
                swizzle = (level << 16) | (swizzle & 0xff00ffff);
                last = info.mode as u32;
                if level > 1 {
                    pad = swizzle & 0xffff;
                }
            }
            let align = info.base_align.max(1);
            pad += (align - prev_size % align) % align;
            if level == 1 {
                offset0 = pad + prev_size;
            } else {
                offsets[level as usize - 1] = pad + prev_size + offsets[level as usize - 2];
            }
        } else {
            if changed && mode != info.mode as u32 {
                mode = info.mode as u32;
                info = ask(mode);
                if !tiled(mode) {
                    swizzle &= 0xff00ffff;
                }
                last = mode;
            }
            if changed && width < info.pitch_align && height < info.height_align {
                mode = if mode == TileMode::TwoDThick as u32 {
                    TileMode::OneDThick
                } else {
                    TileMode::OneDThin1
                } as u32;
                info = ask(mode);
                swizzle &= 0xff00ffff;
                last = mode;
            }
            image_size = info.size as u32;
            alignment = info.base_align;
            pitch = info.pitch;
        }
        prev_size = info.size as u32;
    }
    let mip_size = if levels <= 1 { 0 } else { prev_size + offsets[levels as usize - 2] };
    offsets[0] = offset0;
    g.put_u32(s + 0x20, image_size);
    g.put_u32(s + 0x28, mip_size);
    g.put_u32(s + 0x30, mode);
    g.put_u32(s + 0x34, swizzle);
    g.put_u32(s + 0x38, alignment);
    g.put_u32(s + 0x3c, pitch);
    g.put_u32(s + 0x10, levels);
    for (i, o) in offsets.iter().take(13).enumerate() {
        g.put_u32(s + 0x40 + 4 * i as u32, *o);
    }
}
