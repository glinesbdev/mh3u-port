//! The graphics API (`gx2`). For now only what the game needs to size its buffers: nothing is drawn yet.
//!
//! The surface sizes below are an approximation of the console's tiling rules (pitch rounded to 64 elements, height to 8, a 0x1000
//! alignment), enough for the game to allocate; they must be replaced by the exact address-library rules before textures are read.

use crate::cafe::SimpleHandler;
use crate::guest::Guest;

pub fn find(name: &str) -> Option<SimpleHandler> {
    Some(match name {
        "gx2:GX2CalcSurfaceSizeAndAlignment" => |_, g, c| calc_surface(g, c.r[3]),
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

/// `GX2CalcSurfaceSizeAndAlignment(surface)`: fills in the image size, mip size, alignment, pitch and the mip offsets.
fn calc_surface(g: &Guest, s: u32) {
    let (dim, width, height, depth, mips, format) = (
        g.u32(s),
        g.u32(s + 4),
        g.u32(s + 8),
        g.u32(s + 0xc),
        g.u32(s + 0x10),
        g.u32(s + 0x14),
    );
    let (bytes, block) = element(format);
    let (bw, bh) = if block {
        (width.div_ceil(4), height.div_ceil(4))
    } else {
        (width, height)
    };
    // a cube map or an array has `depth` layers; a 3D texture has `depth` slices that shrink with the level
    let layers = if dim == 2 || dim == 6 || dim == 7 { depth.max(1) } else { 1 };
    let pitch = align(bw.max(1), 64);
    let mut offset = 0;
    let mut mip_offsets = [0u32; 13];
    let mut base = 0;
    for level in 0..mips.clamp(1, 14) {
        let (w, h) = (align((bw >> level).max(1), 64), align((bh >> level).max(1), 8));
        let size = w * h * bytes * layers;
        if level == 0 {
            base = align(size, 0x1000);
            offset = base;
        } else {
            mip_offsets[level as usize - 1] = offset;
            offset += align(size, 0x200);
        }
    }
    g.put_u32(s + 0x20, base);
    g.put_u32(s + 0x28, if mips > 1 { offset - base } else { 0 });
    g.put_u32(s + 0x38, 0x1000);
    g.put_u32(s + 0x3c, pitch);
    for (i, o) in mip_offsets.iter().enumerate() {
        g.put_u32(s + 0x40 + 4 * i as u32, *o);
    }
}
