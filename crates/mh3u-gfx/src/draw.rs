//! What a draw call becomes: the game's shaders translated to WGSL, its vertex data decoded on the CPU, and a render pipeline made from
//! the recorded state, cached by what makes it different.

use crate::gx2::Gx2;
use crate::state::State;
use mh3u_latte::decode::decode;
use mh3u_latte::translate::{Context, Interface, PixelInputs, TexKind, VertexAttribute, VertexInputs, translate_pixel, translate_vertex};
use mh3u_rt::Guest;
use std::collections::HashMap;
use std::sync::Arc;

/// One element of a fetch shader's description (`GX2AttribStream`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AttribStream {
    pub location: u32,
    pub buffer: u32,
    pub offset: u32,
    pub format: u32,
    pub per_instance: bool,
    pub divisor: u32,
    /// How the components of the fetched value are chosen: four selectors, x first (0 to 3 a component, 4 zero, 5 one).
    pub select: [u32; 4],
    pub endian: u32,
}

impl AttribStream {
    pub fn read(g: &Guest, at: u32) -> AttribStream {
        let mask = g.u32(at + 24);
        AttribStream {
            location: g.u32(at),
            buffer: g.u32(at + 4),
            offset: g.u32(at + 8),
            format: g.u32(at + 12),
            per_instance: g.u32(at + 16) == 1,
            divisor: g.u32(at + 20),
            select: [(mask >> 24) & 7, (mask >> 16) & 7, (mask >> 8) & 7, mask & 7],
            endian: g.u32(at + 28),
        }
    }
}

pub struct Compiled {
    pub module: wgpu::ShaderModule,
    pub interface: Interface,
}

/// A pipeline and what it was made from.
/// The three translated stages of a geometry pipeline and how many vertices the geometry shader may emit.
pub type GeometryShaders = (Arc<Compiled>, Arc<Compiled>, Arc<Compiled>, u32);

pub struct CachedTexture {
    pub texture: wgpu::Texture,
    pub layers: u32,
    pub range: (u32, u32),
}

pub struct PipelineEntry {
    pub pipeline: wgpu::RenderPipeline,
}

#[derive(Default)]
pub struct DrawCaches {
    pub vertex: HashMap<(u32, u32, u64), Option<Arc<Compiled>>>,
    pub pixel: HashMap<(u32, u64), Option<Arc<Compiled>>>,
    pub pipelines: HashMap<String, Arc<PipelineEntry>>,
    pub depth: HashMap<u32, (wgpu::Texture, wgpu::TextureFormat)>,
    pub fallback: HashMap<&'static str, wgpu::Texture>,
    pub reported: std::collections::HashSet<String>,
    /// How many draws went to each colour buffer (by image address), for the log.
    pub drawn_into: HashMap<u32, u64>,
    /// Textures made from the guest's, by the address of their image and what they were read as, with the memory they cover.
    pub textures: HashMap<(u32, u32, u32, u32, u32), CachedTexture>,
    pub samplers: HashMap<String, wgpu::Sampler>,
    pub geometry: HashMap<String, Option<GeometryShaders>>,
    pub compute: HashMap<String, wgpu::ComputePipeline>,
    /// Why draws were not drawn, and how many.
    pub skipped: HashMap<&'static str, u64>,
    pub gs_draws: HashMap<(u32, u32, u32, u32, u32), u64>,
}

fn hash_bytes(b: &[u8]) -> u64 {
    // FNV-1a
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &x in b {
        h = (h ^ u64::from(x)).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The attribute's components as raw 32-bit patterns (floats for the float and normalised formats, integers as they are for the others).
fn decode_attribute(format: u32, endian: u32, bytes: &[u8]) -> [u32; 4] {
    let kind = format & 0xff;
    let class = (format >> 8) & 0xf;
    // (components, bytes per component)
    let (n, size) = match kind {
        0x00 => (1, 1),
        0x04 => (2, 1),
        0x0a => (4, 1),
        0x02 | 0x03 => (1, 2),
        0x07 | 0x08 => (2, 2),
        0x0e | 0x0f => (4, 2),
        0x05 | 0x06 => (1, 4),
        0x0c | 0x0d => (2, 4),
        0x10 | 0x11 => (3, 4),
        0x12 | 0x13 => (4, 4),
        0x0b => (4, 0),
        0x09 => (3, 0),
        _ => (0, 0),
    };
    // 0 means the data is read in the order it is stored; everything else is a big-endian guest value
    let swap = endian != 0;
    let u = |at: usize, len: usize| -> u32 {
        let mut v = 0u32;
        for k in 0..len {
            let byte = bytes.get(at + k).copied().unwrap_or(0);
            if swap {
                v = (v << 8) | u32::from(byte);
            } else {
                v |= u32::from(byte) << (8 * k);
            }
        }
        v
    };
    let mut out = [0u32; 4];
    if size == 0 {
        // packed formats
        let word = u(0, 4);
        if kind == 0x0b {
            let fields = [
                (word & 0x3ff, 1023.0),
                ((word >> 10) & 0x3ff, 1023.0),
                ((word >> 20) & 0x3ff, 1023.0),
                ((word >> 30) & 3, 3.0),
            ];
            for (k, (v, max)) in fields.iter().enumerate() {
                out[k] = match class {
                    0 => (*v as f32 / max).to_bits(),
                    _ => *v,
                };
            }
        } else if kind == 0x09 {
            let f11 = |bits: u32| -> f32 {
                let (e, m) = ((bits >> 6) & 0x1f, bits & 0x3f);
                if e == 0 {
                    m as f32 / 64.0 * 2f32.powi(-14)
                } else if e == 31 {
                    f32::INFINITY
                } else {
                    (1.0 + m as f32 / 64.0) * 2f32.powi(e as i32 - 15)
                }
            };
            let f10 = |bits: u32| -> f32 {
                let (e, m) = ((bits >> 5) & 0x1f, bits & 0x1f);
                if e == 0 {
                    m as f32 / 32.0 * 2f32.powi(-14)
                } else if e == 31 {
                    f32::INFINITY
                } else {
                    (1.0 + m as f32 / 32.0) * 2f32.powi(e as i32 - 15)
                }
            };
            out = [
                f11(word & 0x7ff).to_bits(),
                f11((word >> 11) & 0x7ff).to_bits(),
                f10(word >> 22).to_bits(),
                0,
            ];
        }
        return out;
    }
    for (k, slot) in out.iter_mut().enumerate().take(n) {
        let raw = u(k * size, size);
        *slot = match (class, size) {
            // floating point
            (8, 4) => raw,
            (8, 2) => half_to_f32(raw as u16).to_bits(),
            // unsigned normalised
            (0, 1) => (raw as f32 / 255.0).to_bits(),
            (0, 2) => (raw as f32 / 65535.0).to_bits(),
            (0, 4) => (raw as f32 / 4_294_967_295.0).to_bits(),
            // signed normalised
            (2, 1) => ((raw as u8 as i8 as f32 / 127.0).max(-1.0)).to_bits(),
            (2, 2) => ((raw as u16 as i16 as f32 / 32767.0).max(-1.0)).to_bits(),
            // scaled: integers converted to floats
            (4, _) => (raw as f32).to_bits(),
            (5, 1) => (raw as u8 as i8 as f32).to_bits(),
            (5, 2) => (raw as u16 as i16 as f32).to_bits(),
            (5, _) => (raw as i32 as f32).to_bits(),
            // integers stay integers (signed ones sign-extended)
            (3, 1) => raw as u8 as i8 as i32 as u32,
            (3, 2) => raw as u16 as i16 as i32 as u32,
            _ => raw,
        };
    }
    out
}

fn half_to_f32(h: u16) -> f32 {
    let (s, e, m) = ((h >> 15) & 1, (h >> 10) & 0x1f, u32::from(h & 0x3ff));
    let v = if e == 0 {
        m as f32 / 1024.0 * 2f32.powi(-14)
    } else if e == 31 {
        if m == 0 { f32::INFINITY } else { f32::NAN }
    } else {
        (1.0 + m as f32 / 1024.0) * 2f32.powi(i32::from(e) - 15)
    };
    if s == 1 { -v } else { v }
}

/// How many bytes one element of an attribute format takes.
fn attribute_size(format: u32) -> u32 {
    match format & 0xff {
        0x00 | 0x01 => 1,
        0x02..=0x04 => 2,
        0x05..=0x0b => 4,
        0x0c..=0x0f => 8,
        0x10 | 0x11 => 12,
        0x12 | 0x13 => 16,
        _ => 4,
    }
}

fn blend_factor(f: u32) -> wgpu::BlendFactor {
    use wgpu::BlendFactor as B;
    match f {
        0 => B::Zero,
        1 => B::One,
        2 => B::Src,
        3 => B::OneMinusSrc,
        4 => B::SrcAlpha,
        5 => B::OneMinusSrcAlpha,
        6 => B::DstAlpha,
        7 => B::OneMinusDstAlpha,
        8 => B::Dst,
        9 => B::OneMinusDst,
        10 => B::SrcAlphaSaturated,
        13 | 17 => B::Constant,
        14 | 18 => B::OneMinusConstant,
        _ => B::One,
    }
}

fn blend_op(op: u32) -> wgpu::BlendOperation {
    use wgpu::BlendOperation as O;
    match op {
        0 => O::Add,
        1 => O::Subtract,
        2 => O::Min,
        3 => O::Max,
        4 => O::ReverseSubtract,
        _ => O::Add,
    }
}

fn compare(f: u32) -> wgpu::CompareFunction {
    use wgpu::CompareFunction as C;
    match f {
        0 => C::Never,
        1 => C::Less,
        2 => C::Equal,
        3 => C::LessEqual,
        4 => C::Greater,
        5 => C::NotEqual,
        6 => C::GreaterEqual,
        _ => C::Always,
    }
}

fn depth_format(gx2: u32) -> wgpu::TextureFormat {
    match gx2 & 0x3f {
        0x11 | 0x13 => wgpu::TextureFormat::Depth24PlusStencil8,
        _ => wgpu::TextureFormat::Depth32Float,
    }
}

fn tex_kind(dim: u32, format: u32) -> TexKind {
    if matches!(format & 0x3f, 0x11 | 0x13 | 0x14) || format & 0x3f == 0x16 {
        return TexKind::Depth2d;
    }
    match dim {
        2 => TexKind::D3,
        3 => TexKind::Cube,
        _ => TexKind::D2,
    }
}

impl Gx2 {
    /// A shader module from WGSL, with errors reported (once) instead of stopping the program.
    fn compile(&mut self, label: &str, wgsl: &str) -> wgpu::ShaderModule {
        self.d.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        })
    }

    pub(crate) fn caches_report(&mut self, text: String) {
        self.report(text);
    }

    fn report(&mut self, text: String) {
        if self.caches.reported.insert(text.clone()) {
            eprintln!("[gfx] {text}");
        }
    }

    fn vertex_shader(&mut self, g: &Guest, vs: u32, fetch: &[AttribStream], half_z: bool) -> Option<Arc<Compiled>> {
        let (size, program) = (g.u32(vs + 0xd0), g.u32(vs + 0xd4));
        let mut code = vec![0u8; size.min(1 << 20) as usize];
        g.read(program, &mut code);
        let key = (vs, program, hash_bytes(&code) ^ hash_bytes(format!("{fetch:?}{half_z}").as_bytes()));
        if let Some(c) = self.caches.vertex.get(&key) {
            return c.clone();
        }
        let result = (|| {
            let decoded = decode(&code).map_err(|e| e.to_string())?;
            let attributes = fetch
                .iter()
                .enumerate()
                .map(|(i, a)| VertexAttribute {
                    gpr: a.location + 1,
                    stream: i as u32,
                    per_instance: a.per_instance,
                    divisor: a.divisor,
                })
                .collect();
            let ctx = Context {
                texture_kind: &|_| TexKind::D2,
            };
            translate_vertex(
                &decoded,
                &VertexInputs {
                    attributes,
                    half_z,
                    ring_out_vec4: 0,
                },
                &ctx,
            )
        })();
        let compiled = match result {
            Ok(t) => {
                let module = self.compile("vertex shader", &t.wgsl);
                Some(Arc::new(Compiled {
                    module,
                    interface: t.interface,
                }))
            }
            Err(e) => {
                self.report(format!("vertex shader {vs:#x} not translated: {e}"));
                None
            }
        };
        self.caches.vertex.insert(key, compiled.clone());
        compiled
    }

    fn pixel_shader(&mut self, g: &Guest, ps: u32, semantics_at: u32, kinds: &[TexKind; 18]) -> Option<(Arc<Compiled>, PixelInputs)> {
        // the inputs: pixel input i takes the vertex shader's output with the same semantic
        let count = g.u32(ps + 0x10).min(32);
        let vs_semantics: Vec<u32> = (0..40u32)
            .map(|k| (g.u32(semantics_at + 4 * (k / 4)) >> (8 * (k % 4))) & 0xff)
            .collect();
        let mut inputs = PixelInputs {
            count,
            flat: Vec::new(),
            locations: Vec::new(),
        };
        for i in 0..count {
            let cntl = g.u32(ps + 0x14 + 4 * i);
            let semantic = cntl & 0xff;
            inputs.flat.push((cntl >> 10) & 1 != 0);
            let found = vs_semantics.iter().position(|&s| s == semantic).map_or(u32::MAX, |p| p as u32);
            inputs.locations.push(found);
        }
        let (size, program) = (g.u32(ps + 0xa4), g.u32(ps + 0xa8));
        let mut code = vec![0u8; size.min(1 << 20) as usize];
        g.read(program, &mut code);
        let key = (ps, hash_bytes(&code) ^ hash_bytes(format!("{inputs:?}{kinds:?}").as_bytes()));
        if let Some(c) = self.caches.pixel.get(&key) {
            return c.clone().map(|c| (c, inputs));
        }
        let kinds = *kinds;
        let result = (|| {
            let decoded = decode(&code).map_err(|e| e.to_string())?;
            let ctx = Context {
                texture_kind: &move |r| kinds.get(r as usize).copied().unwrap_or(TexKind::D2),
            };
            translate_pixel(&decoded, &inputs, &ctx)
        })();
        let compiled = match result {
            Ok(mut t) => {
                if std::env::var_os("MH3U_GFX_SOLID").is_some() {
                    // a debugging aid: every pixel drawn is red, to see where the geometry goes
                    t.wgsl = t
                        .wgsl
                        .replace("    return o;\n}", "    o.c0 = vec4<f32>(1.0, 0.0, 0.0, 1.0);\n    return o;\n}");
                }
                let module = self.compile("pixel shader", &t.wgsl);
                Some(Arc::new(Compiled {
                    module,
                    interface: t.interface,
                }))
            }
            Err(e) => {
                self.report(format!("pixel shader {ps:#x} not translated: {e}"));
                None
            }
        };
        self.caches.pixel.insert(key, compiled.clone());
        compiled.map(|c| (c, inputs))
    }

    /// Makes the texture at `t` (a `GX2Texture`) into a GPU texture, once until the game says it changed.
    fn texture_view(&mut self, g: &Guest, t: u32, kind: TexKind) -> Option<wgpu::TextureView> {
        let h = crate::texture::Header::read(g, t);
        let key = (h.image, h.width, h.height, h.format, h.tile_mode ^ (h.dim << 8));
        if !self.caches.textures.contains_key(&key) {
            if let Some(dir) = std::env::var_os("MH3U_GFX_TEXDUMP") {
                let dir = std::path::Path::new(&dir);
                let _ = std::fs::create_dir_all(dir);
                let mut raw = vec![0u8; h.image_size.min(1 << 26) as usize];
                g.read(h.image, &mut raw);
                let mut header = vec![0u8; 0x9c];
                g.read(t, &mut header);
                let name = format!("tex-{:08x}-{}x{}-f{:x}-m{}", h.image, h.width, h.height, h.format, h.tile_mode);
                let _ = std::fs::write(dir.join(format!("{name}.image")), raw);
                let _ = std::fs::write(dir.join(format!("{name}.header")), header);
            }
            match crate::texture::decode(g, &h) {
                Ok(d) => {
                    let texture = self.d.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("game texture"),
                        size: wgpu::Extent3d {
                            width: d.width,
                            height: d.height,
                            depth_or_array_layers: d.layers,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: d.format,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });
                    let block = d.format.block_dimensions();
                    let rows = d.height.div_ceil(block.1);
                    self.d.queue.write_texture(
                        texture.as_image_copy(),
                        &d.data,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(d.bytes_per_row),
                            rows_per_image: Some(rows),
                        },
                        wgpu::Extent3d {
                            width: d.width,
                            height: d.height,
                            depth_or_array_layers: d.layers,
                        },
                    );
                    self.caches.textures.insert(
                        key,
                        CachedTexture {
                            texture,
                            layers: d.layers,
                            range: (h.image, h.image.saturating_add(h.image_size)),
                        },
                    );
                }
                Err(e) => {
                    self.report(format!(
                        "texture {t:#x} (format {:#x}, {}x{}, tile mode {}) not read: {e}",
                        h.format, h.width, h.height, h.tile_mode
                    ));
                    return None;
                }
            }
        }
        let cached = &self.caches.textures[&key];
        Some(cached.texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(match kind {
                TexKind::Cube if cached.layers == 6 => wgpu::TextureViewDimension::Cube,
                _ => wgpu::TextureViewDimension::D2,
            }),
            array_layer_count: Some(if kind == TexKind::Cube { 6.min(cached.layers) } else { 1 }),
            ..Default::default()
        }))
    }

    pub(crate) fn invalidate_textures(&mut self, address: u32, size: u32) {
        let end = address.saturating_add(size);
        self.caches.textures.retain(|_, t| t.range.1 <= address || t.range.0 >= end);
    }

    fn fallback_texture(&mut self, kind: TexKind) -> wgpu::TextureView {
        let (name, depth) = match kind {
            TexKind::D2 => ("2d", false),
            TexKind::D3 => ("3d", false),
            TexKind::Cube => ("cube", false),
            TexKind::Depth2d => ("depth", true),
        };
        if !self.caches.fallback.contains_key(name) {
            let (dimension, layers) = match kind {
                TexKind::D3 => (wgpu::TextureDimension::D3, 1),
                TexKind::Cube => (wgpu::TextureDimension::D2, 6),
                _ => (wgpu::TextureDimension::D2, 1),
            };
            let format = if depth {
                wgpu::TextureFormat::Depth32Float
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            };
            let texture = self.d.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("fallback texture"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: layers,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            if !depth {
                for layer in 0..layers {
                    self.d.queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
                            aspect: wgpu::TextureAspect::All,
                        },
                        &[255, 255, 255, 255],
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(4),
                            rows_per_image: Some(1),
                        },
                        wgpu::Extent3d {
                            width: 1,
                            height: 1,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }
            self.caches.fallback.insert(name, texture);
        }
        let texture = &self.caches.fallback[name];
        texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(match kind {
                TexKind::D3 => wgpu::TextureViewDimension::D3,
                TexKind::Cube => wgpu::TextureViewDimension::Cube,
                _ => wgpu::TextureViewDimension::D2,
            }),
            ..Default::default()
        })
    }

    fn depth_target(&mut self, g: &Guest, db: u32) -> Option<(u32, wgpu::TextureFormat)> {
        let (width, height, format, image) = (g.u32(db + 4), g.u32(db + 8), depth_format(g.u32(db + 0x14)), g.u32(db + 0x24));
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return None;
        }
        let stale = self
            .caches
            .depth
            .get(&image)
            .is_none_or(|(t, f)| t.width() != width || t.height() != height || *f != format);
        if stale {
            let texture = self.d.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("depth buffer"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.caches.depth.insert(image, (texture, format));
        }
        Some((image, format))
    }

    pub(crate) fn clear_depth(&mut self, g: &Guest, db: u32, depth: f32, stencil: u32, mode: u32) {
        let Some((image, format)) = self.depth_target(g, db) else { return };
        let view = self.caches.depth[&image].0.create_view(&Default::default());
        let has_stencil = format == wgpu::TextureFormat::Depth24PlusStencil8;
        let mut enc = self.d.device.create_command_encoder(&Default::default());
        {
            // mode: 1 clears depth, 2 stencil, 3 both
            let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear depth"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &view,
                    depth_ops: Some(wgpu::Operations {
                        load: if mode & 1 != 0 {
                            wgpu::LoadOp::Clear(depth.clamp(0.0, 1.0))
                        } else {
                            wgpu::LoadOp::Load
                        },
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: has_stencil.then_some(wgpu::Operations {
                        load: if mode & 2 != 0 {
                            wgpu::LoadOp::Clear(stencil)
                        } else {
                            wgpu::LoadOp::Load
                        },
                        store: wgpu::StoreOp::Store,
                    }),
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        self.d.queue.submit([enc.finish()]);
    }

    /// The vertex data of the draw, decoded: one stream of `vec4` per fetch shader element, laid out one after the other.
    pub(crate) fn decode_streams(
        &self,
        g: &Guest,
        st: &State,
        fetch: &[AttribStream],
        vertices: u32,
        instances: u32,
    ) -> (Vec<u8>, [[u32; 4]; 16]) {
        let mut data: Vec<u8> = Vec::new();
        let mut meta = [[0u32; 4]; 16];
        for (i, a) in fetch.iter().enumerate().take(16) {
            meta[i][0] = (data.len() / 16) as u32;
            let b = &st.attrib_buffers[(a.buffer as usize).min(15)];
            let items = if a.per_instance {
                instances.div_ceil(a.divisor.max(1))
            } else {
                vertices
            };
            let size = attribute_size(a.format);
            let mut bytes = vec![0u8; size as usize];
            for item in 0..items {
                let at = b.address.wrapping_add(a.offset).wrapping_add(item.wrapping_mul(b.stride));
                if b.address != 0 && item.wrapping_mul(b.stride).saturating_add(a.offset).saturating_add(size) <= b.size.max(1) {
                    g.read(at, &mut bytes);
                } else {
                    bytes.iter_mut().for_each(|x| *x = 0);
                }
                let v = decode_attribute(a.format, a.endian, &bytes);
                let one = 1.0f32.to_bits();
                let pick = |s: u32| match s {
                    0..=3 => v[s as usize],
                    5 => one,
                    _ => 0,
                };
                for k in 0..4 {
                    data.extend_from_slice(&pick(a.select[k]).to_le_bytes());
                }
            }
        }
        if data.is_empty() {
            data.resize(16, 0);
        }
        (data, meta)
    }

    /// The upload of one uniform block: the guest's big-endian words as the shader reads them.
    pub(crate) fn uniform_buffer_pub(&self, g: &Guest, block: &crate::state::UniformBlock, vec4s: u32) -> wgpu::Buffer {
        self.uniform_buffer(g, block, vec4s)
    }

    fn uniform_buffer(&self, g: &Guest, block: &crate::state::UniformBlock, vec4s: u32) -> wgpu::Buffer {
        let size = u64::from(vec4s.max(1)) * 16;
        let buffer = self.d.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniform block"),
            size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut bytes = vec![0u8; size as usize];
        let have = (block.size as usize).min(bytes.len());
        if block.address != 0 && have > 0 {
            g.read(block.address, &mut bytes[..have]);
            for w in bytes.as_chunks_mut::<4>().0 {
                w.reverse();
            }
        }
        self.d.queue.write_buffer(&buffer, 0, &bytes);
        buffer
    }

    pub(crate) fn draw_call(&mut self, g: &Guest, primitive: u32, count: u32, indexed: Option<(u32, u32)>, first: u32, instances: u32) {
        let st = self.contexts.entry(self.current).or_default().clone();
        if st.vertex_shader == 0 || st.pixel_shader == 0 || count == 0 {
            *self.caches.skipped.entry("no shader or no vertices").or_default() += 1;
            return;
        }
        let Some(fetch) = self.fetch_shaders.get(&st.fetch_shader).cloned() else {
            self.report(format!(
                "a draw with the fetch shader {:#x}, which was not made by GX2InitFetchShaderEx",
                st.fetch_shader
            ));
            *self.caches.skipped.entry("unknown fetch shader").or_default() += 1;
            return;
        };
        let Some(image) = self.target(g, st.colour_buffers[0]) else {
            *self.caches.skipped.entry("no colour buffer").or_default() += 1;
            return;
        };
        *self.caches.drawn_into.entry(image).or_default() += 1;
        let (target_w, target_h, target_format) = {
            let t = &self.targets[&image];
            (t.texture.width(), t.texture.height(), t.format)
        };
        // the textures the pixel shader will look at, to know their kinds
        let mut kinds = [TexKind::D2; 18];
        for (unit, kind) in kinds.iter_mut().enumerate() {
            let t = st.pixel_textures[unit];
            if t != 0 {
                *kind = tex_kind(g.u32(t), g.u32(t + 0x14));
            }
        }
        let geometry = st.geometry_shader != 0 && st.shader_mode == 2;
        let geo = if geometry {
            match self.geometry_pass(g, &st, &fetch, primitive, count, first, indexed, instances) {
                Some(x) => Some(x),
                None => {
                    *self.caches.skipped.entry("geometry pipeline not drawn").or_default() += 1;
                    return;
                }
            }
        } else {
            None
        };
        let vs = match &geo {
            Some(x) => x.copy.clone(),
            None => {
                let Some(vs) = self.vertex_shader(g, st.vertex_shader, &fetch, st.half_z) else {
                    *self.caches.skipped.entry("vertex shader not translated").or_default() += 1;
                    return;
                };
                vs
            }
        };
        let semantics_at = geo.as_ref().map_or(st.vertex_shader + 0x10, |x| x.semantics_at);
        let Some((ps, _inputs)) = self.pixel_shader(g, st.pixel_shader, semantics_at, &kinds) else {
            *self.caches.skipped.entry("pixel shader not translated").or_default() += 1;
            return;
        };
        let device = self.d.device.clone();
        let (va, meta_buffer, index_buffer, index_count, topology): (
            wgpu::Buffer,
            wgpu::Buffer,
            wgpu::Buffer,
            u32,
            wgpu::PrimitiveTopology,
        ) = if let Some(x) = &geo {
            let dummy = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("unused streams"),
                size: 16,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            });
            let dummy_meta = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("unused offsets"),
                size: 256,
                usage: wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            });
            (
                dummy,
                dummy_meta,
                x.indices.clone(),
                x.index_count,
                wgpu::PrimitiveTopology::TriangleList,
            )
        } else {
            // indices
            let index_values: Option<Vec<u32>> = match indexed {
                Some((kind, address)) => {
                    let (wide, big) = (matches!(kind, 1 | 9), matches!(kind, 4 | 9));
                    let size = if wide { 4 } else { 2 };
                    let mut bytes = vec![0u8; (count * size) as usize];
                    g.read(address, &mut bytes);
                    Some(
                        bytes
                            .chunks_exact(size as usize)
                            .map(|c| match (wide, big) {
                                (true, true) => u32::from_be_bytes([c[0], c[1], c[2], c[3]]),
                                (true, false) => u32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                                (false, true) => u32::from(u16::from_be_bytes([c[0], c[1]])),
                                (false, false) => u32::from(u16::from_le_bytes([c[0], c[1]])),
                            })
                            .collect(),
                    )
                }
                None => None,
            };
            let (topology, expand): (wgpu::PrimitiveTopology, bool) = match primitive {
                1 => (wgpu::PrimitiveTopology::PointList, false),
                2 => (wgpu::PrimitiveTopology::LineList, false),
                3 => (wgpu::PrimitiveTopology::LineStrip, false),
                4 => (wgpu::PrimitiveTopology::TriangleList, false),
                6 => (wgpu::PrimitiveTopology::TriangleStrip, false),
                5 | 0x13 | 0x14 | 0x12 => (wgpu::PrimitiveTopology::TriangleList, true),
                other => {
                    self.report(format!("the primitive type {other:#x} is not drawn"));
                    *self.caches.skipped.entry("primitive type").or_default() += 1;
                    return;
                }
            };
            // the list of vertex numbers the draw uses
            let base: Vec<u32> = match &index_values {
                Some(v) => v.iter().map(|i| i.wrapping_add(first)).collect(),
                None => (first..first + count).collect(),
            };
            let indices: Vec<u32> = if expand {
                match primitive {
                    5 => (1..base.len().saturating_sub(1))
                        .flat_map(|i| [base[0], base[i], base[i + 1]])
                        .collect(),
                    0x13 => base
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .flat_map(|q| [q[0], q[1], q[2], q[0], q[2], q[3]])
                        .collect(),
                    0x14 => (0..base.len().saturating_sub(3))
                        .step_by(2)
                        .flat_map(|i| [base[i], base[i + 1], base[i + 3], base[i], base[i + 3], base[i + 2]])
                        .collect(),
                    _ => Vec::new(),
                }
            } else {
                base
            };
            if indices.is_empty() {
                return;
            }
            let vertices = indices.iter().copied().max().map_or(0, |m| m + 1);
            let (stream_data, meta) = self.decode_streams(g, &st, &fetch, vertices.min(1 << 22), instances);
            // buffers
            let va = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vertex streams"),
                size: stream_data.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.d.queue.write_buffer(&va, 0, &stream_data);
            let meta_bytes: Vec<u8> = meta.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
            let meta_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stream offsets"),
                size: 256,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.d.queue.write_buffer(&meta_buffer, 0, &meta_bytes);
            let index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("indices"),
                size: (indices.len() * 4) as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.d
                .queue
                .write_buffer(&index_buffer, 0, &indices.iter().flat_map(|i| i.to_le_bytes()).collect::<Vec<u8>>());
            (va, meta_buffer, index_buffer, indices.len() as u32, topology)
        };
        // the pipeline
        let depth = if st.depth_buffer != 0 {
            self.depth_target(g, st.depth_buffer)
        } else {
            None
        };
        let use_depth = depth.filter(|_| st.depth_test || st.depth_write);
        let strip = matches!(
            topology,
            wgpu::PrimitiveTopology::TriangleStrip | wgpu::PrimitiveTopology::LineStrip
        );
        let key = format!(
            "{:p}{:p}{topology:?}{target_format:?}{:?}{:?}{}{}{}{}{}{}{}",
            Arc::as_ptr(&vs),
            Arc::as_ptr(&ps),
            st.blend[0],
            use_depth.map(|d| d.1),
            st.colour_control[1] & 1,
            st.target_masks & 0xf,
            st.depth_test,
            st.depth_write,
            st.depth_func,
            st.cull_front as u8 * 2 + st.cull_back as u8,
            st.front_face,
        );
        let entry = match self.caches.pipelines.get(&key) {
            Some(e) => e.clone(),
            None => {
                let blend_on = st.colour_control[1] & 1 != 0;
                let b = &st.blend[0];
                let blend = blend_on.then(|| wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: blend_factor(b.colour_src),
                        dst_factor: blend_factor(b.colour_dst),
                        operation: blend_op(b.colour_op),
                    },
                    alpha: if b.separate_alpha {
                        wgpu::BlendComponent {
                            src_factor: blend_factor(b.alpha_src),
                            dst_factor: blend_factor(b.alpha_dst),
                            operation: blend_op(b.alpha_op),
                        }
                    } else {
                        wgpu::BlendComponent {
                            src_factor: blend_factor(b.colour_src),
                            dst_factor: blend_factor(b.colour_dst),
                            operation: blend_op(b.colour_op),
                        }
                    },
                });
                let mask = wgpu::ColorWrites::from_bits_truncate(st.target_masks & 0xf);
                let colour_targets: Vec<Option<wgpu::ColorTargetState>> = ps
                    .interface
                    .colour_outputs
                    .iter()
                    .map(|_| {
                        Some(wgpu::ColorTargetState {
                            format: target_format,
                            blend,
                            write_mask: mask,
                        })
                    })
                    .collect();
                let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("game pipeline"),
                    layout: None,
                    vertex: wgpu::VertexState {
                        module: &vs.module,
                        entry_point: Some("main"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &ps.module,
                        entry_point: Some("main"),
                        compilation_options: Default::default(),
                        targets: &colour_targets,
                    }),
                    primitive: wgpu::PrimitiveState {
                        topology,
                        strip_index_format: strip.then_some(wgpu::IndexFormat::Uint32),
                        front_face: if st.front_face == 0 {
                            wgpu::FrontFace::Ccw
                        } else {
                            wgpu::FrontFace::Cw
                        },
                        cull_mode: match (st.cull_front, st.cull_back) {
                            (false, false) => None,
                            (true, false) => Some(wgpu::Face::Front),
                            (false, true) => Some(wgpu::Face::Back),
                            (true, true) => Some(wgpu::Face::Back),
                        },
                        ..Default::default()
                    },
                    depth_stencil: use_depth.map(|(_, format)| wgpu::DepthStencilState {
                        format,
                        depth_write_enabled: Some(st.depth_write),
                        depth_compare: Some(if st.depth_test {
                            compare(st.depth_func)
                        } else {
                            wgpu::CompareFunction::Always
                        }),
                        stencil: Default::default(),
                        bias: Default::default(),
                    }),
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                });
                let e = Arc::new(PipelineEntry { pipeline });
                self.caches.pipelines.insert(key, e.clone());
                e
            }
        };
        // the bindings
        let mut vs_entries = Vec::new();
        let mut ps_entries = Vec::new();
        let mut keep: Vec<wgpu::Buffer> = Vec::new();
        for (&bank, &len) in &vs.interface.banks {
            keep.push(self.uniform_buffer(g, &st.vertex_blocks[(bank as usize).min(15)], len));
        }
        for (&bank, &len) in &ps.interface.banks {
            keep.push(self.uniform_buffer(g, &st.pixel_blocks[(bank as usize).min(15)], len));
        }
        let mut at = 0;
        for &bank in vs.interface.banks.keys() {
            vs_entries.push(wgpu::BindGroupEntry {
                binding: bank,
                resource: keep[at].as_entire_binding(),
            });
            at += 1;
        }
        for &bank in ps.interface.banks.keys() {
            ps_entries.push(wgpu::BindGroupEntry {
                binding: bank,
                resource: keep[at].as_entire_binding(),
            });
            at += 1;
        }
        let group0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &entry.pipeline.get_bind_group_layout(0),
            entries: &vs_entries,
        });
        let group1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &entry.pipeline.get_bind_group_layout(1),
            entries: &ps_entries,
        });
        // textures: white stand-ins until the real ones are decoded
        let draw_image = image;
        let mut snapshots: Vec<(wgpu::Texture, wgpu::Texture)> = Vec::new();
        let views: Vec<(u32, wgpu::TextureView)> = ps
            .interface
            .textures
            .iter()
            .map(|(&r, &k)| {
                let t = st.pixel_textures.get(r as usize).copied().unwrap_or(0);
                let image = if t != 0 { g.u32(t + 0x24) } else { 0 };
                // a render target the game drew into is what a later pass samples
                if k == TexKind::D2
                    && let Some(target) = self.targets.get(&image)
                {
                    {
                        if image != draw_image {
                            return (r, target.texture.create_view(&Default::default()));
                        }
                        // the pass draws into what it samples: the shader gets a copy of how it was before
                        let copy = device.create_texture(&wgpu::TextureDescriptor {
                            label: Some("snapshot"),
                            size: wgpu::Extent3d {
                                width: target.texture.width(),
                                height: target.texture.height(),
                                depth_or_array_layers: 1,
                            },
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format: target.format,
                            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                            view_formats: &[],
                        });
                        snapshots.push((target.texture.clone(), copy.clone()));
                        return (r, copy.create_view(&Default::default()));
                    }
                }
                if k == TexKind::Depth2d
                    && let Some((texture, _)) = self.caches.depth.get(&image)
                {
                    {
                        return (
                            r,
                            texture.create_view(&wgpu::TextureViewDescriptor {
                                aspect: wgpu::TextureAspect::DepthOnly,
                                ..Default::default()
                            }),
                        );
                    }
                }
                if t != 0
                    && k != TexKind::Depth2d
                    && k != TexKind::D3
                    && let Some(view) = self.texture_view(g, t, k)
                {
                    return (r, view);
                }
                (r, self.fallback_texture(k))
            })
            .collect();
        let sampler_for = |this: &mut Gx2, unit: u32| -> wgpu::Sampler {
            let at = st.pixel_samplers.get(unit as usize).copied().unwrap_or(0);
            if at == 0 {
                return this.d.device.create_sampler(&wgpu::SamplerDescriptor {
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    ..Default::default()
                });
            }
            let d = crate::texture::Sampler::read(g, at);
            let key = format!("{d:?}");
            if let Some(s) = this.caches.samplers.get(&key) {
                return s.clone();
            }
            let filter = |linear: bool| {
                if linear {
                    wgpu::FilterMode::Linear
                } else {
                    wgpu::FilterMode::Nearest
                }
            };
            let s = this.d.device.create_sampler(&wgpu::SamplerDescriptor {
                address_mode_u: crate::texture::Sampler::address_mode(d.clamp[0]),
                address_mode_v: crate::texture::Sampler::address_mode(d.clamp[1]),
                address_mode_w: crate::texture::Sampler::address_mode(d.clamp[2]),
                mag_filter: filter(d.mag_linear),
                min_filter: filter(d.min_linear),
                mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                ..Default::default()
            });
            this.caches.samplers.insert(key, s.clone());
            s
        };
        let sampler_units: Vec<u32> = ps.interface.samplers.iter().copied().collect();
        let sampler_list: Vec<(u32, wgpu::Sampler)> = sampler_units.iter().map(|s| (16 + s, sampler_for(self, *s))).collect();
        let compare_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            compare: Some(wgpu::CompareFunction::LessEqual),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let mut tex_entries: Vec<wgpu::BindGroupEntry> = Vec::new();
        for (r, v) in &views {
            tex_entries.push(wgpu::BindGroupEntry {
                binding: *r,
                resource: wgpu::BindingResource::TextureView(v),
            });
        }
        for (binding, sampler) in &sampler_list {
            tex_entries.push(wgpu::BindGroupEntry {
                binding: *binding,
                resource: wgpu::BindingResource::Sampler(sampler),
            });
        }
        for s in &ps.interface.compare_samplers {
            tex_entries.push(wgpu::BindGroupEntry {
                binding: 32 + s,
                resource: wgpu::BindingResource::Sampler(&compare_sampler),
            });
        }
        let group2 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &entry.pipeline.get_bind_group_layout(2),
            entries: &tex_entries,
        });
        let mut group3_entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: va.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: meta_buffer.as_entire_binding(),
            },
        ];
        if let Some(x) = &geo {
            group3_entries.push(wgpu::BindGroupEntry {
                binding: 2,
                resource: x.ring_out.as_entire_binding(),
            });
        }
        let group3 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &entry.pipeline.get_bind_group_layout(3),
            entries: &group3_entries,
        });
        let colour_view = self.targets[&image].texture.create_view(&Default::default());
        let depth_view = use_depth.map(|(image, _)| self.caches.depth[&image].0.create_view(&Default::default()));
        let mut enc = device.create_command_encoder(&Default::default());
        for (from, to) in &snapshots {
            enc.copy_texture_to_texture(
                from.as_image_copy(),
                to.as_image_copy(),
                wgpu::Extent3d {
                    width: from.width(),
                    height: from.height(),
                    depth_or_array_layers: 1,
                },
            );
        }
        {
            let colour_attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = ps
                .interface
                .colour_outputs
                .iter()
                .map(|_| {
                    Some(wgpu::RenderPassColorAttachment {
                        view: &colour_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })
                })
                .collect();
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("draw"),
                color_attachments: &colour_attachments,
                depth_stencil_attachment: depth_view.as_ref().map(|v| wgpu::RenderPassDepthStencilAttachment {
                    view: v,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: use_depth
                        .filter(|d| d.1 == wgpu::TextureFormat::Depth24PlusStencil8)
                        .map(|_| wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        }),
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let [vx, vy, vw, vh, ..] = st.viewport;
            if vw > 0.0 && vh > 0.0 {
                let x = vx.clamp(0.0, target_w as f32);
                let y = vy.clamp(0.0, target_h as f32);
                pass.set_viewport(
                    x,
                    y,
                    vw.min(target_w as f32 - x).max(1.0),
                    vh.min(target_h as f32 - y).max(1.0),
                    0.0,
                    1.0,
                );
            }
            let [sx, sy, sw, sh] = st.scissor;
            if sw > 0 && sh > 0 && sx < target_w && sy < target_h {
                pass.set_scissor_rect(sx, sy, sw.min(target_w - sx), sh.min(target_h - sy));
            }
            pass.set_pipeline(&entry.pipeline);
            pass.set_bind_group(0, &group0, &[]);
            pass.set_bind_group(1, &group1, &[]);
            pass.set_bind_group(2, &group2, &[]);
            pass.set_bind_group(3, &group3, &[]);
            pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..index_count, 0, 0..if geo.is_some() { 1 } else { instances.max(1) });
        }
        self.d.queue.submit([enc.finish()]);
    }
}
