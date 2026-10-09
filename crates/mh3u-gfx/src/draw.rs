//! What a draw call becomes: the game's shaders translated to WGSL, its vertex data decoded on the CPU, and a render pipeline made from
//! the recorded state, cached by what makes it different.

use crate::gx2::Gx2;
use crate::shader_cache::{FrontCache, Lookup, Ranges};
use crate::state::State;
use mh3u_latte::decode::decode;
use mh3u_latte::translate::{Context, Interface, PixelInputs, TexKind, VertexAttribute, VertexInputs, translate_pixel, translate_vertex};
use mh3u_rt::Guest;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
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

/// A geometry pipeline: the three translated stages, how many vertices the geometry shader may emit, and the compute pipelines of the
/// first two stages.
pub struct GeometryEntry {
    pub es: Arc<Compiled>,
    pub gs: Arc<Compiled>,
    pub copy: Arc<Compiled>,
    pub max_vertices: u32,
    pub es_pipeline: wgpu::ComputePipeline,
    pub gs_pipeline: wgpu::ComputePipeline,
    /// The layouts of their bind groups 0 and 1, asked for once.
    pub es_layouts: [wgpu::BindGroupLayout; 2],
    pub gs_layouts: [wgpu::BindGroupLayout; 2],
}

/// A fetch shader's streams, shared (see `Gx2::fetch_shaders`).
pub type Fetch = Arc<[AttribStream]>;
/// The front key of a vertex shader: (shader, program, size, fetch shader version, half z).
pub type VertexFront = (u32, u32, u32, usize, bool);
/// The front key of a pixel shader: (shader, program, size, hash of the words its inputs are made from, texture kinds).
pub type PixelFront = (u32, u32, u32, u64, [u8; 18]);

/// The front key of a geometry pipeline: the vertex and geometry shaders, their three programs (start, size), the fetch shader version,
/// half z, and the ring item sizes (es and gs vec4s), everything its translation depends on besides the program bytes.
pub type GeometryFront = (u32, u32, [(u32, u32); 3], usize, bool, u32, u32);

pub struct CachedTexture {
    pub texture: wgpu::Texture,
    pub layers: u32,
    pub range: (u32, u32),
    pub mips: (u32, u32),
    /// The views the shaders read it through, made once: as a 2D texture, and as a cube (see `texture_view`).
    pub views: [Option<wgpu::TextureView>; 2],
    /// What it was decoded from: a hash of the header's words and of the bytes of its image and smaller levels.
    pub hash: u64,
    /// `GX2Invalidate` covered its memory since it was decoded or checked: at its next use it is hashed again, and decoded again
    /// only if the hash differs.
    pub dirty: bool,
    /// The swap it was last drawn with (an invalidated texture not drawn for a second is forgotten, see `prune`).
    pub used: u32,
}

/// The words of a `GX2Texture` header that decoding reads (`texture::Header::read` reads up to 0x88).
const TEXTURE_HEADER_BYTES: usize = 0x88;

/// A hash of what a texture is decoded from: its header's words and the bytes of its image and of its smaller levels.
fn texture_source_hash(g: &Guest, t: u32, h: &crate::texture::Header) -> u64 {
    let mut buf = vec![0u8; TEXTURE_HEADER_BYTES];
    g.read(t, &mut buf);
    let mut hash = hash_fast(&buf, 0x7e);
    for (at, size) in [(h.image, h.image_size), (h.mip_ptr, h.mip_size)] {
        if at == 0 || size == 0 {
            continue;
        }
        buf.resize(size.min(1 << 28) as usize, 0);
        g.read(at, &mut buf);
        hash = hash_fast(&buf, hash);
    }
    hash
}

pub struct PipelineEntry {
    pub pipeline: wgpu::RenderPipeline,
    /// The layouts of its four bind groups, asked for once (the same objects the pipeline was made with).
    pub layouts: [wgpu::BindGroupLayout; 4],
    /// The shaders the pipeline's key names by address, kept alive so the address is not reused while the key is.
    _shaders: (Arc<Compiled>, Arc<Compiled>),
}

/// What makes a render pipeline different. The shaders are named by the address of their translation (kept alive by the entry).
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct PipelineKey {
    vs: usize,
    ps: usize,
    topology: wgpu::PrimitiveTopology,
    target_format: wgpu::TextureFormat,
    blend: crate::state::Blend,
    depth_format: Option<wgpu::TextureFormat>,
    blend_on: u32,
    masks: u32,
    depth_test: bool,
    depth_write: bool,
    depth_func: u32,
    cull: u8,
    front_face: u32,
}

/// A depth buffer the game draws with, and its views (made once): the whole of it for drawing, its depth for sampling.
pub struct DepthTarget {
    pub texture: wgpu::Texture,
    pub format: wgpu::TextureFormat,
    pub view: wgpu::TextureView,
    pub sample_view: wgpu::TextureView,
}

/// One resource of a bind group as a cache key: wgpu handles compare and hash by identity, so equal keys mean the same objects.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum Bound {
    Buffer(wgpu::Buffer, u64, u64),
    View(wgpu::TextureView),
    Sampler(wgpu::Sampler),
}

/// A bind group's layout and what is bound at each binding.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct GroupKey {
    layout: wgpu::BindGroupLayout,
    entries: Vec<(u32, Bound)>,
}

/// How many bind groups are kept before the cache is emptied (it is also pruned of the ones not used for a second, see `prune`).
const BIND_GROUP_LIMIT: usize = 16384;

#[derive(Default)]
pub struct DrawCaches {
    pub vertex: HashMap<(u32, u32, u64), Option<Arc<Compiled>>>,
    pub pixel: HashMap<(u32, u64), Option<Arc<Compiled>>>,
    /// The fronts of the two (see `shader_cache`): by (shader, program, size, fetch shader version, half z), and by (shader, program,
    /// size, hash of the inputs' words, texture kinds).
    pub vertex_front: FrontCache<VertexFront, (Option<Arc<Compiled>>, Fetch)>,
    pub pixel_front: FrontCache<PixelFront, Option<Arc<Compiled>>>,
    pub pipelines: HashMap<PipelineKey, Arc<PipelineEntry>>,
    pub depth: HashMap<u32, DepthTarget>,
    pub fallback: HashMap<&'static str, (wgpu::Texture, wgpu::TextureView)>,
    pub reported: std::collections::HashSet<String>,
    /// How many draws went to each colour buffer (by image address), for the log.
    pub drawn_into: HashMap<u32, u64>,
    /// Textures made from the guest's, by the address of their image and what they were read as, with the memory they cover.
    pub textures: HashMap<(u32, u32, u32, u32, u32), CachedTexture>,
    /// Samplers by the three words of the `GX2Sampler` they were made from, and the two that are not the game's.
    pub samplers: HashMap<[u32; 3], wgpu::Sampler>,
    pub default_sampler: Option<wgpu::Sampler>,
    pub compare_sampler: Option<wgpu::Sampler>,
    /// Bind groups by what they bind, with the swap they were last used in.
    pub bind_groups: HashMap<GroupKey, (wgpu::BindGroup, u32)>,
    /// The uniform blocks uploaded in this batch, by (address, vec4s, hash of the contents): a draw that reads the same contents again
    /// reads the same bytes (they are compared, not only hashed). Emptied with the arenas.
    pub uniforms: HashMap<(u32, u32, u64), BufRef>,
    /// A buffer reused for reading uniform blocks.
    pub scratch: Vec<u8>,
    /// Geometry pipelines by the hashes of their programs and what else their translation depends on, with their front.
    pub geometry: HashMap<(u32, u32, u64), Option<Arc<GeometryEntry>>>,
    pub geometry_front: FrontCache<GeometryFront, (Option<Arc<GeometryEntry>>, Fetch)>,
    /// Why draws were not drawn, and how many.
    pub skipped: HashMap<&'static str, u64>,
    pub gs_draws: HashMap<(u32, u32, u32, u32, u32), u64>,
    /// Decoded vertex streams kept on the GPU, by what they were decoded from (the layout and a hash of the bytes).
    pub streams: HashMap<(u64, u64), StreamEntry>,
    pub stream_bytes: u64,
}

pub struct StreamEntry {
    buffer: wgpu::Buffer,
    /// Where each stream starts, as the shaders read it (a small buffer of its own, so a draw's bind group 3 is the same every frame).
    meta: wgpu::Buffer,
    size: u64,
    used: u32,
}

/// A quick hash of bytes (eight at a time), for telling whether vertex data is what it was.
pub(crate) fn hash_fast(b: &[u8], seed: u64) -> u64 {
    let mut h = seed ^ (b.len() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let (words, rest) = b.as_chunks::<8>();
    for w in words {
        h = (h ^ u64::from_le_bytes(*w)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        h ^= h >> 29;
    }
    for &x in rest {
        h = (h ^ u64::from(x)).wrapping_mul(0x0100_0000_01b3);
    }
    h ^ (h >> 32)
}

/// A hash of the guest memory in `ranges` (the programs a cached translation was made from).
pub(crate) fn hash_ranges(g: &Guest, ranges: &Ranges) -> u64 {
    let mut h = 0x51u64;
    let mut buf = Vec::new();
    for &(at, len) in ranges.iter().filter(|r| r.1 != 0) {
        buf.resize(len as usize, 0);
        g.read(at, &mut buf);
        h = hash_fast(&buf, h);
    }
    h
}

/// A hash of anything hashable, for the parts of a shader key that are not bytes.
pub(crate) fn hash_of(v: &impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// Counts a shader cache lookup in the timing report.
pub(crate) fn count_lookup(how: &Lookup, hit: &'static str, checked: &'static str, miss: &'static str) {
    Lap::count(
        match how {
            Lookup::Hit => hit,
            Lookup::Checked => checked,
            Lookup::Miss => miss,
        },
        1,
    );
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

    fn vertex_shader(&mut self, g: &Guest, vs: u32, fetch: &Arc<[AttribStream]>, half_z: bool) -> Option<Arc<Compiled>> {
        let (size, program) = (g.u32(vs + 0xd0), g.u32(vs + 0xd4));
        let size = size.min(1 << 20);
        let front = (vs, program, size, Arc::as_ptr(fetch).cast::<u8>() as usize, half_z);
        let swaps = self.swaps;
        let (found, how) = self.caches.vertex_front.get(&front, swaps, |r| hash_ranges(g, r));
        count_lookup(&how, "vertex shader hits", "vertex shader checks", "vertex shader misses");
        if let Some((c, _)) = found {
            return c;
        }
        let ranges = [(program, size), (0, 0), (0, 0)];
        let compiled = self.vertex_shader_slow(g, vs, program, size, fetch, half_z);
        let hash = hash_ranges(g, &ranges);
        self.caches
            .vertex_front
            .insert(front, ranges, hash, swaps, (compiled.clone(), fetch.clone()));
        compiled
    }

    /// The vertex shader from its program bytes: translated once for each program, fetch shader and depth range.
    fn vertex_shader_slow(
        &mut self,
        g: &Guest,
        vs: u32,
        program: u32,
        size: u32,
        fetch: &[AttribStream],
        half_z: bool,
    ) -> Option<Arc<Compiled>> {
        let mut code = vec![0u8; size as usize];
        g.read(program, &mut code);
        let key = (vs, program, hash_fast(&code, 0) ^ hash_of(&(fetch, half_z)));
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

    fn pixel_shader(&mut self, g: &Guest, ps: u32, semantics_at: u32, kinds: &[TexKind; 18]) -> Option<Arc<Compiled>> {
        // what the translation depends on besides the program: the words the inputs are made from, and the texture kinds
        let count = g.u32(ps + 0x10).min(32);
        let mut words = [0u32; 1 + 32 + 10];
        words[0] = count;
        for i in 0..count {
            words[1 + i as usize] = g.u32(ps + 0x14 + 4 * i);
        }
        for k in 0..10 {
            words[33 + k as usize] = g.u32(semantics_at + 4 * k);
        }
        let words_bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let (size, program) = (g.u32(ps + 0xa4), g.u32(ps + 0xa8));
        let size = size.min(1 << 20);
        let front = (ps, program, size, hash_fast(&words_bytes, 0x9), kinds.map(|k| k as u8));
        let swaps = self.swaps;
        let (found, how) = self.caches.pixel_front.get(&front, swaps, |r| hash_ranges(g, r));
        count_lookup(&how, "pixel shader hits", "pixel shader checks", "pixel shader misses");
        if let Some(c) = found {
            return c;
        }
        let ranges = [(program, size), (0, 0), (0, 0)];
        let compiled = self.pixel_shader_slow(g, ps, semantics_at, kinds);
        let hash = hash_ranges(g, &ranges);
        self.caches.pixel_front.insert(front, ranges, hash, swaps, compiled.clone());
        compiled
    }

    /// The pixel shader from its program bytes and inputs: translated once for each.
    fn pixel_shader_slow(&mut self, g: &Guest, ps: u32, semantics_at: u32, kinds: &[TexKind; 18]) -> Option<Arc<Compiled>> {
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
        let key = (
            ps,
            hash_fast(&code, 0) ^ hash_of(&(inputs.count, &inputs.flat, &inputs.locations, kinds.map(|k| k as u8))),
        );
        if let Some(c) = self.caches.pixel.get(&key) {
            return c.clone();
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
        compiled
    }

    /// Makes the texture at `t` (a `GX2Texture`) into a GPU texture, once until the game says it changed (and it did: an invalidated
    /// texture whose header and bytes hash the same is kept).
    ///
    /// The path taken for a texture already made reads the five header words of the key and does one look-up.
    fn texture_view(&mut self, g: &Guest, t: u32, kind: TexKind) -> Option<wgpu::TextureView> {
        // the key: image, width, height, format, tile mode and dimension (as `texture::Header::read` reads them)
        let key = (
            g.u32(t + 0x24),
            g.u32(t + 4),
            g.u32(t + 8),
            g.u32(t + 0x14),
            g.u32(t + 0x30) ^ (g.u32(t) << 8),
        );
        let found = match self.caches.textures.get_mut(&key) {
            Some(cached) if cached.dirty => {
                let h = crate::texture::Header::read(g, t);
                if texture_source_hash(g, t, &h) == cached.hash {
                    Lap::count("texture invalidations of unchanged textures", 1);
                    cached.dirty = false;
                    cached.used = self.swaps;
                    true
                } else {
                    self.caches.textures.remove(&key);
                    false
                }
            }
            Some(cached) => {
                cached.used = self.swaps;
                true
            }
            None => false,
        };
        if !found {
            self.decode_texture(g, t, key)?;
        }
        let cached = self.caches.textures.get_mut(&key)?;
        let slot = &mut cached.views[usize::from(kind == TexKind::Cube)];
        if let Some(view) = slot {
            return Some(view.clone());
        }
        let view = cached.texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(match kind {
                TexKind::Cube if cached.layers > 1 => wgpu::TextureViewDimension::D2Array,
                _ => wgpu::TextureViewDimension::D2,
            }),
            array_layer_count: Some(if kind == TexKind::Cube { 6.min(cached.layers) } else { 1 }),
            ..Default::default()
        });
        *slot = Some(view.clone());
        Some(view)
    }

    /// Decodes the texture at `t` and keeps it under `key`; None if it cannot be read.
    #[inline(never)]
    fn decode_texture(&mut self, g: &Guest, t: u32, key: (u32, u32, u32, u32, u32)) -> Option<()> {
        static TEXDUMP: std::sync::OnceLock<Option<std::ffi::OsString>> = std::sync::OnceLock::new();
        let texdump = TEXDUMP.get_or_init(|| std::env::var_os("MH3U_GFX_TEXDUMP")).as_ref();
        let mut lap = Lap::new();
        let h = crate::texture::Header::read(g, t);
        {
            if let Some(dir) = texdump {
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
            let decoded = crate::texture::decode(g, &h);
            lap.mark("texture decode (in texture views)");
            match decoded {
                Ok(d) => {
                    if let Some(dir) = texdump {
                        let name = format!(
                            "tex-{:08x}-{}x{}-f{:x}-m{}.decoded",
                            h.image, d.width, d.height, h.format, h.tile_mode
                        );
                        let _ = std::fs::write(std::path::Path::new(&dir).join(name), &d.levels[0].data);
                    }
                    let texture = self.d.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("game texture"),
                        size: wgpu::Extent3d {
                            width: d.width,
                            height: d.height,
                            depth_or_array_layers: d.layers,
                        },
                        mip_level_count: d.levels.len() as u32,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: d.format,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });
                    let block = d.format.block_dimensions();
                    for (n, l) in d.levels.iter().enumerate() {
                        self.d.queue.write_texture(
                            wgpu::TexelCopyTextureInfo {
                                mip_level: n as u32,
                                ..texture.as_image_copy()
                            },
                            &l.data,
                            wgpu::TexelCopyBufferLayout {
                                offset: 0,
                                bytes_per_row: Some(l.bytes_per_row),
                                rows_per_image: Some(l.height.div_ceil(block.1)),
                            },
                            wgpu::Extent3d {
                                width: l.width,
                                height: l.height,
                                depth_or_array_layers: d.layers,
                            },
                        );
                    }
                    self.caches.textures.insert(
                        key,
                        CachedTexture {
                            texture,
                            layers: d.layers,
                            range: (h.image, h.image.saturating_add(h.image_size)),
                            mips: (h.mip_ptr, h.mip_ptr.saturating_add(h.mip_size)),
                            views: [None, None],
                            hash: texture_source_hash(g, t, &h),
                            dirty: false,
                            used: self.swaps,
                        },
                    );
                    lap.mark("texture upload and hash (in texture views)");
                    Lap::count("texture decodes", 1);
                    Lap::count("texture bytes decoded", u64::from(h.image_size) + u64::from(h.mip_size));
                    Some(())
                }
                Err(e) => {
                    self.report(format!(
                        "texture {t:#x} (format {:#x}, {}x{}, tile mode {}) not read: {e}",
                        h.format, h.width, h.height, h.tile_mode
                    ));
                    None
                }
            }
        }
    }

    /// `GX2Invalidate` of shader memory: the translations made from programs there are looked up by their bytes again.
    pub(crate) fn invalidate_shaders(&mut self, address: u32, size: u32) {
        self.caches.vertex_front.invalidate(address, size);
        self.caches.pixel_front.invalidate(address, size);
        self.caches.geometry_front.invalidate(address, size);
    }

    /// `GX2Invalidate` of texture memory: the textures it overlaps are checked against their bytes at their next use.
    pub(crate) fn invalidate_textures(&mut self, address: u32, size: u32) {
        let end = address.saturating_add(size);
        let mut marked = 0;
        for t in self.caches.textures.values_mut() {
            if !((t.range.1 <= address || t.range.0 >= end) && (t.mips.1 <= address || t.mips.0 >= end)) {
                t.dirty = true;
                marked += 1;
            }
        }
        Lap::count("textures invalidated", marked);
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
            let view = texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(match kind {
                    TexKind::D3 => wgpu::TextureViewDimension::D3,
                    TexKind::Cube => wgpu::TextureViewDimension::D2Array,
                    _ => wgpu::TextureViewDimension::D2,
                }),
                ..Default::default()
            });
            self.caches.fallback.insert(name, (texture, view));
        }
        self.caches.fallback[name].1.clone()
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
            .is_none_or(|d| d.texture.width() != width || d.texture.height() != height || d.format != format);
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
            let view = texture.create_view(&Default::default());
            let sample_view = texture.create_view(&wgpu::TextureViewDescriptor {
                aspect: wgpu::TextureAspect::DepthOnly,
                ..Default::default()
            });
            self.caches.depth.insert(
                image,
                DepthTarget {
                    texture,
                    format,
                    view,
                    sample_view,
                },
            );
        }
        Some((image, format))
    }

    pub(crate) fn clear_depth(&mut self, g: &Guest, db: u32, depth: f32, stencil: u32, mode: u32) {
        self.flush_draws();
        let Some((image, format)) = self.depth_target(g, db) else { return };
        let view = self.caches.depth[&image].view.clone();
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
        start: u32,
        vertices: u32,
        instances: u32,
    ) -> (Vec<u8>, [[u32; 4]; 16]) {
        let mut data: Vec<u8> = Vec::new();
        let mut meta = [[0u32; 4]; 16];
        for (i, a) in fetch.iter().enumerate().take(16) {
            meta[i][0] = (data.len() / 16) as u32;
            let b = &st.attrib_buffers[(a.buffer as usize).min(15)];
            let (first_item, items) = if a.per_instance {
                (0, instances.div_ceil(a.divisor.max(1)))
            } else {
                (start, vertices)
            };
            let size = attribute_size(a.format);
            let mut bytes = vec![0u8; size as usize];
            for item in first_item..first_item + items {
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

    /// The decoded streams of a draw on the GPU: made again only when the layout or the vertex bytes are not what they were (the bytes
    /// are hashed, so a game that changes its vertex data without telling the GPU is still followed).
    pub(crate) fn cached_streams(
        &mut self,
        g: &Guest,
        st: &State,
        fetch: &[AttribStream],
        start: u32,
        vertices: u32,
        instances: u32,
    ) -> (BufRef, BufRef) {
        let mut lap = Lap::new();
        let (mut h1, mut h2) = (0x1234_5678_9abc_def0u64, 0x0fed_cba9_8765_4321u64);
        let mut mix = |v: u64| {
            h1 = (h1 ^ v).wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(23);
            h2 = (h2.wrapping_add(v)).wrapping_mul(0xc2b2_ae3d_27d4_eb4f).rotate_left(31);
        };
        mix(u64::from(start));
        mix(u64::from(vertices));
        mix(u64::from(instances));
        // the bytes each buffer is read from: one span per buffer and range of items
        let mut spans: Vec<(u32, u32, u32, u32)> = Vec::new(); // (buffer, first item, items, bytes needed past the last item's start)
        for a in fetch.iter().take(16) {
            for v in [
                a.location,
                a.buffer,
                a.offset,
                a.format,
                a.endian,
                a.divisor,
                u32::from(a.per_instance),
            ]
            .into_iter()
            .chain(a.select)
            {
                mix(u64::from(v));
            }
            let (first, items) = if a.per_instance {
                (0, instances.div_ceil(a.divisor.max(1)))
            } else {
                (start, vertices)
            };
            let end = a.offset + attribute_size(a.format);
            match spans.iter_mut().find(|s| (s.0, s.1, s.2) == (a.buffer, first, items)) {
                Some(s) => s.3 = s.3.max(end),
                None => spans.push((a.buffer, first, items, end)),
            }
        }
        let mut raw = Vec::new();
        for &(buffer, first, items, end) in &spans {
            let b = &st.attrib_buffers[(buffer as usize).min(15)];
            mix(u64::from(b.address != 0));
            mix(u64::from(b.stride));
            mix(u64::from(b.size));
            if b.address == 0 || items == 0 {
                continue;
            }
            let from = u64::from(first) * u64::from(b.stride);
            let len = (u64::from(items) - 1) * u64::from(b.stride) + u64::from(end);
            let len = len.min(u64::from(b.size).saturating_sub(from)).min(1 << 28);
            raw.resize(len as usize, 0);
            g.read(b.address.wrapping_add(from as u32), &mut raw);
            mix(hash_fast(&raw, 1));
        }
        let key = (h1, h2);
        let swaps = self.swaps;
        lap.mark("stream hash");
        if let Some(e) = self.caches.streams.get_mut(&key) {
            lap.mark("stream hit");
            e.used = swaps;
            let (buffer, meta) = (e.buffer.clone(), e.meta.clone());
            let size = e.size;
            return (
                BufRef { buffer, offset: 0, size },
                BufRef {
                    buffer: meta,
                    offset: 0,
                    size: 256,
                },
            );
        }
        let (data, meta) = self.decode_streams(g, st, fetch, start, vertices, instances);
        lap.mark("stream miss: decode");
        if std::env::var_os("MH3U_TRACE_STREAMS").is_some() {
            eprintln!(
                "[streams] miss: start {start} vertices {vertices} instances {instances} attributes {} bytes {}",
                fetch.len(),
                data.len()
            );
        }
        let size = data.len() as u64;
        let buffer = self.d.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex streams"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.d.queue.write_buffer(&buffer, 0, &data);
        let meta_buffer = self.d.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex stream starts"),
            size: 256,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let meta_bytes: Vec<u8> = meta.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
        self.d.queue.write_buffer(&meta_buffer, 0, &meta_bytes);
        // forget what has not been used for a while, or what is too much
        self.caches.stream_bytes += size;
        if self.caches.stream_bytes > (1 << 30) {
            let before = self.caches.stream_bytes;
            self.caches.streams.retain(|_, e| e.used + 120 >= swaps);
            self.caches.stream_bytes = self.caches.streams.values().map(|e| e.size).sum::<u64>() + size;
            if self.caches.stream_bytes > before / 2 && self.caches.stream_bytes > (1 << 30) {
                self.caches.streams.clear();
                self.caches.stream_bytes = size;
            }
        }
        self.caches.streams.insert(
            key,
            StreamEntry {
                buffer: buffer.clone(),
                meta: meta_buffer.clone(),
                size,
                used: swaps,
            },
        );
        (
            BufRef { buffer, offset: 0, size },
            BufRef {
                buffer: meta_buffer,
                offset: 0,
                size: 256,
            },
        )
    }

    /// The upload of one uniform block: the guest's big-endian words as the shader reads them.
    /// The same contents uploaded earlier in the batch are not uploaded again: the draw gets the same bytes (compared, not trusted to the
    /// hash), which also lets it share the bind group of the draw before.
    pub(crate) fn uniform_buffer(&mut self, g: &Guest, block: &crate::state::UniformBlock, vec4s: u32) -> BufRef {
        let size = u64::from(vec4s.max(1)) * 16;
        let mut bytes = std::mem::take(&mut self.caches.scratch);
        bytes.clear();
        bytes.resize(size as usize, 0);
        let have = (block.size as usize).min(bytes.len());
        if block.address != 0 && have > 0 {
            g.read(block.address, &mut bytes[..have]);
            for w in bytes.as_chunks_mut::<4>().0 {
                w.reverse();
            }
        }
        let key = (block.address, vec4s, hash_fast(&bytes, 0x75));
        let earlier = self.caches.uniforms.get(&key).filter(|r| self.arena_holds(r, &bytes)).cloned();
        let r = match earlier {
            Some(r) => {
                Lap::count("uniform uploads shared", 1);
                r
            }
            None => {
                Lap::count("uniform uploads", 1);
                let r = self.arena_write(&bytes, size);
                self.caches.uniforms.insert(key, r.clone());
                r
            }
        };
        self.caches.scratch = bytes;
        r
    }

    /// A bind group with these resources: the one made before for the same layout and resources, or a new one (which is kept unless
    /// `keep` is false, for resources made for this draw only).
    pub(crate) fn bind_group(&mut self, layout: &wgpu::BindGroupLayout, entries: Vec<(u32, Bound)>, keep: bool) -> wgpu::BindGroup {
        let key = GroupKey {
            layout: layout.clone(),
            entries,
        };
        if keep && let Some(e) = self.caches.bind_groups.get_mut(&key) {
            e.1 = self.swaps;
            Lap::count("bind group hits", 1);
            return e.0.clone();
        }
        let list: Vec<wgpu::BindGroupEntry> = key
            .entries
            .iter()
            .map(|(binding, r)| wgpu::BindGroupEntry {
                binding: *binding,
                resource: match r {
                    Bound::Buffer(buffer, offset, size) => wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer,
                        offset: *offset,
                        size: std::num::NonZeroU64::new(*size),
                    }),
                    Bound::View(v) => wgpu::BindingResource::TextureView(v),
                    Bound::Sampler(s) => wgpu::BindingResource::Sampler(s),
                },
            })
            .collect();
        let group = self.d.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout,
            entries: &list,
        });
        if keep {
            Lap::count("bind group misses", 1);
            if self.caches.bind_groups.len() >= BIND_GROUP_LIMIT {
                self.caches.bind_groups.clear();
            }
            self.caches.bind_groups.insert(key, (group.clone(), self.swaps));
        } else {
            Lap::count("bind groups not kept", 1);
        }
        group
    }

    /// Once a second of frames: forgets the bind groups not used in that time (they keep their buffers and views alive), and the
    /// invalidated textures not drawn in that time.
    pub(crate) fn prune(&mut self) {
        let swaps = self.swaps;
        if swaps.is_multiple_of(60) {
            self.caches.bind_groups.retain(|_, e| e.1 + 60 >= swaps);
            // an invalidated texture is kept in case its bytes did not change; one not drawn since goes as it did before
            self.caches.textures.retain(|_, t| !t.dirty || t.used + 60 >= swaps);
        }
    }

    pub(crate) fn draw_call(&mut self, g: &Guest, primitive: u32, count: u32, indexed: Option<(u32, u32)>, first: u32, instances: u32) {
        let mut lap = Lap::new();
        let st = self.contexts.entry(self.current).or_default().clone();
        lap.mark("state clone");
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
        lap.mark("target and kinds");
        let geometry = st.geometry_shader != 0 && st.shader_mode == 2;
        // only the draws with a geometry shader have this pass, and only they are timed as it
        let geo = if geometry {
            let geo = self.geometry_pass(g, &st, &fetch, primitive, count, first, indexed, instances);
            lap.mark("geometry pass");
            match geo {
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
        let Some(ps) = self.pixel_shader(g, st.pixel_shader, semantics_at, &kinds) else {
            *self.caches.skipped.entry("pixel shader not translated").or_default() += 1;
            return;
        };
        lap.mark("shaders");
        let device = self.d.device.clone();
        let (va, meta_buffer, index_buffer, index_count, topology): (BufRef, BufRef, BufRef, u32, wgpu::PrimitiveTopology) =
            if let Some(x) = &geo {
                let dummy = self.arena_alloc(Role::Read, 16);
                let dummy_meta = self.arena_alloc(Role::Read, 256);
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
                                // the restart index of a strip is `u32::MAX` from here on (what the GPU API takes)
                                .map(|i| if Some(i) == st.restart_index { u32::MAX } else { i })
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
                    Some(v) => v.iter().map(|&i| if i == u32::MAX { i } else { i.wrapping_add(first) }).collect(),
                    None => (first..first + count).collect(),
                };
                // only strips restart; elsewhere the restart value is an ordinary vertex number
                let base: Vec<u32> = if matches!(primitive, 3 | 6) {
                    base
                } else {
                    base.into_iter()
                        .map(|i| if i == u32::MAX { st.restart_index.unwrap_or(i) } else { i })
                        .collect()
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
                lap.mark("index list");
                // only the vertices the draw uses are decoded: the indices are made relative to the lowest one
                let real = || indices.iter().copied().filter(|&i| i != u32::MAX);
                let lowest = real().min().unwrap_or(0);
                let highest = real().max().unwrap_or(0);
                let indices: Vec<u32> = indices.iter().map(|&i| if i == u32::MAX { i } else { i - lowest }).collect();
                let vertices = highest - lowest + 1;
                let (va, meta_buffer) = self.cached_streams(g, &st, &fetch, lowest, vertices.min(1 << 22), instances);
                lap.mark("decode streams");
                // buffers
                let index_bytes: Vec<u8> = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
                let index_buffer = self.arena_write(&index_bytes, index_bytes.len() as u64);
                (va, meta_buffer, index_buffer, indices.len() as u32, topology)
            };
        lap.mark("indices, streams, buffers");
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
        let key = PipelineKey {
            vs: Arc::as_ptr(&vs) as usize,
            ps: Arc::as_ptr(&ps) as usize,
            topology,
            target_format,
            blend: st.blend[0].clone(),
            depth_format: use_depth.map(|d| d.1),
            blend_on: st.colour_control[1] & 1,
            masks: st.target_masks & 0xf,
            depth_test: st.depth_test,
            depth_write: st.depth_write,
            depth_func: st.depth_func,
            cull: st.cull_front as u8 * 2 + st.cull_back as u8,
            front_face: st.front_face,
        };
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
                let layouts = std::array::from_fn(|n| pipeline.get_bind_group_layout(n as u32));
                let e = Arc::new(PipelineEntry {
                    pipeline,
                    layouts,
                    _shaders: (vs.clone(), ps.clone()),
                });
                self.caches.pipelines.insert(key, e.clone());
                e
            }
        };
        lap.mark("pipeline");
        // the bindings: the uniform blocks (uploaded again only when their contents are new in this batch) and the bind groups (made
        // again only when what they bind is new)
        let mut vs_entries = Vec::with_capacity(vs.interface.banks.len());
        for (&bank, &len) in &vs.interface.banks {
            let r = self.uniform_buffer(g, &st.vertex_blocks[(bank as usize).min(15)], len);
            vs_entries.push((bank, r.bound()));
        }
        let mut ps_entries = Vec::with_capacity(ps.interface.banks.len());
        for (&bank, &len) in &ps.interface.banks {
            let r = self.uniform_buffer(g, &st.pixel_blocks[(bank as usize).min(15)], len);
            ps_entries.push((bank, r.bound()));
        }
        let group0 = self.bind_group(&entry.layouts[0], vs_entries, true);
        let group1 = self.bind_group(&entry.layouts[1], ps_entries, true);
        lap.mark("uniforms and groups 0, 1");
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
                            return (r, target.view.clone());
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
                    && let Some(depth) = self.caches.depth.get(&image)
                {
                    return (r, depth.sample_view.clone());
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
                return this
                    .caches
                    .default_sampler
                    .get_or_insert_with(|| {
                        this.d.device.create_sampler(&wgpu::SamplerDescriptor {
                            mag_filter: wgpu::FilterMode::Linear,
                            min_filter: wgpu::FilterMode::Linear,
                            ..Default::default()
                        })
                    })
                    .clone();
            }
            // the sampler is made from these three words only
            let key = [g.u32(at), g.u32(at + 4), g.u32(at + 8)];
            if let Some(s) = this.caches.samplers.get(&key) {
                return s.clone();
            }
            let d = crate::texture::Sampler::read(g, at);
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
                mipmap_filter: if d.mip_linear {
                    wgpu::MipmapFilterMode::Linear
                } else {
                    wgpu::MipmapFilterMode::Nearest
                },
                lod_min_clamp: d.min_lod,
                lod_max_clamp: d.max_lod.max(d.min_lod),
                ..Default::default()
            });
            this.caches.samplers.insert(key, s.clone());
            s
        };
        lap.mark("texture views");
        let mut tex_entries: Vec<(u32, Bound)> = views.into_iter().map(|(r, v)| (r, Bound::View(v))).collect();
        for &unit in &ps.interface.samplers {
            tex_entries.push((16 + unit, Bound::Sampler(sampler_for(self, unit))));
        }
        if !ps.interface.compare_samplers.is_empty() {
            let compare_sampler = self
                .caches
                .compare_sampler
                .get_or_insert_with(|| {
                    device.create_sampler(&wgpu::SamplerDescriptor {
                        compare: Some(wgpu::CompareFunction::LessEqual),
                        mag_filter: wgpu::FilterMode::Linear,
                        min_filter: wgpu::FilterMode::Linear,
                        ..Default::default()
                    })
                })
                .clone();
            for s in &ps.interface.compare_samplers {
                tex_entries.push((32 + s, Bound::Sampler(compare_sampler.clone())));
            }
        }
        lap.mark("samplers");
        // a snapshot is made for this draw only: its bind group is not kept
        let group2 = self.bind_group(&entry.layouts[2], tex_entries, snapshots.is_empty());
        lap.mark("group 2");
        let mut group3_entries = vec![(0, va.bound()), (1, meta_buffer.bound())];
        if let Some(x) = &geo {
            group3_entries.push((2, x.ring_out.bound()));
        }
        let group3 = self.bind_group(&entry.layouts[3], group3_entries, true);
        lap.mark("group 3");
        // the draws are recorded into one command buffer and one render pass for as long as they go to the same targets; the pass
        // and the buffer end at the next thing that is not such a draw (see `flush_draws`)
        let key: PassKey = (image, use_depth.map(|d| d.0), ps.interface.colour_outputs.len());
        if !snapshots.is_empty() {
            self.end_pass();
            let enc = self
                .batch
                .enc
                .get_or_insert_with(|| device.create_command_encoder(&Default::default()));
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
        }
        if self.batch.key != Some(key) {
            self.end_pass();
            let colour_view = self.targets[&image].view.clone();
            let depth_view = use_depth.map(|(image, _)| self.caches.depth[&image].view.clone());
            let enc = self
                .batch
                .enc
                .get_or_insert_with(|| device.create_command_encoder(&Default::default()));
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
            let pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("draws"),
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
            self.batch.pass = Some(pass.forget_lifetime());
            self.batch.key = Some(key);
        }
        let Some(pass) = self.batch.pass.as_mut() else { return };
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
        } else {
            pass.set_viewport(0.0, 0.0, target_w as f32, target_h as f32, 0.0, 1.0);
        }
        let [sx, sy, sw, sh] = st.scissor;
        if sw > 0 && sh > 0 && sx < target_w && sy < target_h {
            pass.set_scissor_rect(sx, sy, sw.min(target_w - sx), sh.min(target_h - sy));
        } else {
            pass.set_scissor_rect(0, 0, target_w, target_h);
        }
        pass.set_pipeline(&entry.pipeline);
        pass.set_bind_group(0, &group0, &[]);
        pass.set_bind_group(1, &group1, &[]);
        pass.set_bind_group(2, &group2, &[]);
        pass.set_bind_group(3, &group3, &[]);
        pass.set_index_buffer(index_buffer.slice(), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..index_count, 0, 0..if geo.is_some() { 1 } else { instances.max(1) });
        lap.mark("pass");
    }

    /// A part of an arena of at least `size` bytes (contents not defined).
    pub(crate) fn arena_alloc(&mut self, role: Role, size: u64) -> BufRef {
        self.arena_place(role, size).0
    }

    /// The same, with which arena of the role it is in (none for a buffer too big to share).
    fn arena_place(&mut self, role: Role, size: u64) -> (BufRef, Option<usize>) {
        let limits = self.d.device.limits();
        let align = u64::from(
            limits
                .min_uniform_buffer_offset_alignment
                .max(limits.min_storage_buffer_offset_alignment),
        )
        .max(16);
        let size = size.max(16).next_multiple_of(4);
        let usage = wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::UNIFORM
            | wgpu::BufferUsages::INDEX
            | wgpu::BufferUsages::COPY_DST
            | wgpu::BufferUsages::COPY_SRC;
        if size > ARENA_SIZE / 4 {
            // too big to share
            let buffer = self.d.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("big buffer"),
                size,
                usage,
                mapped_at_creation: false,
            });
            return (BufRef { buffer, offset: 0, size }, None);
        }
        for (i, a) in self.batch.arenas[role as usize].iter_mut().enumerate() {
            let at = a.used.next_multiple_of(align);
            if at + size <= a.size {
                a.used = at + size;
                return (
                    BufRef {
                        buffer: a.buffer.clone(),
                        offset: at,
                        size,
                    },
                    Some(i),
                );
            }
        }
        let buffer = self.d.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("arena"),
            size: ARENA_SIZE,
            usage,
            mapped_at_creation: false,
        });
        let arenas = &mut self.batch.arenas[role as usize];
        arenas.push(Arena {
            buffer: buffer.clone(),
            size: ARENA_SIZE,
            used: size,
            shadow: Vec::new(),
        });
        (BufRef { buffer, offset: 0, size }, Some(arenas.len() - 1))
    }

    /// The same, holding `data` (padded with zeros to `size` when that is more). The bytes go to the arena's copy in memory, and the
    /// arena is uploaded in one write when the batch is handed to the GPU (`flush_draws`), not one write per buffer.
    pub(crate) fn arena_write(&mut self, data: &[u8], size: u64) -> BufRef {
        let (r, at) = self.arena_place(Role::Read, size.max(data.len() as u64));
        match at {
            Some(i) => {
                let a = &mut self.batch.arenas[Role::Read as usize][i];
                let (from, end) = (r.offset as usize, (r.offset + r.size) as usize);
                if a.shadow.len() < end {
                    a.shadow.resize(end, 0);
                }
                a.shadow[from..from + data.len()].copy_from_slice(data);
                a.shadow[from + data.len()..end].fill(0);
            }
            None => {
                if !data.is_empty() {
                    self.d.queue.write_buffer(&r.buffer, r.offset, data);
                }
            }
        }
        r
    }

    /// Whether `r` (from `arena_write` in this batch) holds `bytes`.
    fn arena_holds(&self, r: &BufRef, bytes: &[u8]) -> bool {
        let from = r.offset as usize;
        self.batch.arenas[Role::Read as usize]
            .iter()
            .find(|a| a.buffer == r.buffer)
            .and_then(|a| a.shadow.get(from..from + bytes.len()))
            .is_some_and(|held| held == bytes)
    }

    /// Ends the render pass the draws are recorded into (the command buffer stays open).
    pub(crate) fn end_pass(&mut self) {
        self.batch.pass = None;
        self.batch.key = None;
    }

    /// Hands what has been drawn so far to the GPU. Everything that reads or writes a target another way, and the end of the frame,
    /// calls this first.
    pub(crate) fn flush_draws(&mut self) {
        self.end_pass();
        // what the draws read, written into the arenas' copies, goes up first, one write per arena (the queue keeps writes before the
        // commands submitted after them)
        for a in &mut self.batch.arenas[Role::Read as usize] {
            let used = a.used as usize;
            if used > 0 {
                if a.shadow.len() < used {
                    a.shadow.resize(used, 0);
                }
                self.d.queue.write_buffer(&a.buffer, 0, &a.shadow[..used]);
            }
        }
        self.caches.uniforms.clear();
        let mut work = Vec::new();
        if let Some(pre) = self.batch.pre.take() {
            work.push(pre.finish());
        }
        if let Some(enc) = self.batch.enc.take() {
            work.push(enc.finish());
        }
        if !work.is_empty() {
            self.d.queue.submit(work);
        }
        for a in self.batch.arenas.iter_mut().flatten() {
            a.used = 0;
        }
    }
}

/// MH3U_GFX_TIMES: how long the parts of a draw take, and how often the caches hit, added up and reported every report window
/// (MH3U_TIMES_WINDOW seconds, ten by default) as totals and as per-frame averages (steady-state numbers to compare builds with).
pub(crate) struct Lap {
    on: bool,
    at: std::time::Instant,
}

#[derive(Default)]
struct LapTable {
    /// When the window began, and the frame count then.
    since: Option<(std::time::Instant, u64)>,
    times: std::collections::BTreeMap<&'static str, (std::time::Duration, u64)>,
    counts: std::collections::BTreeMap<&'static str, u64>,
}

static LAPS: std::sync::Mutex<LapTable> = std::sync::Mutex::new(LapTable {
    since: None,
    times: std::collections::BTreeMap::new(),
    counts: std::collections::BTreeMap::new(),
});

impl Lap {
    pub(crate) fn enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("MH3U_GFX_TIMES").is_some())
    }

    pub(crate) fn new() -> Lap {
        let on = Lap::enabled();
        Lap {
            on,
            at: std::time::Instant::now(),
        }
    }

    /// Adds the time since the last mark (or since `new`) to `what`.
    pub(crate) fn mark(&mut self, what: &'static str) {
        if !self.on {
            return;
        }
        let now = std::time::Instant::now();
        let mut l = LAPS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let e = l.times.entry(what).or_default();
        e.0 += now - self.at;
        e.1 += 1;
        self.at = now;
        Lap::maybe_print(&mut l, now);
    }

    /// Counts `n` of `what` (cache hits and misses, calls), reported with the times.
    pub(crate) fn count(what: &'static str, n: u64) {
        if !Lap::enabled() {
            return;
        }
        let mut l = LAPS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *l.counts.entry(what).or_default() += n;
    }

    fn maybe_print(l: &mut LapTable, now: std::time::Instant) {
        let (since, frames_at) = *l
            .since
            .get_or_insert_with(|| (now, mh3u_rt::FRAMES.load(std::sync::atomic::Ordering::Relaxed)));
        let wall = now.duration_since(since);
        if wall < mh3u_rt::times_window() {
            return;
        }
        let frames = mh3u_rt::frames_since(frames_at);
        eprintln!(
            "[draw times] {:.1} s, {frames} frames ({:.1} fps); total s, count, ms per frame, count per frame",
            wall.as_secs_f64(),
            frames as f64 / wall.as_secs_f64()
        );
        for (name, (d, n)) in &l.times {
            eprintln!(
                "[draw times] {:>8.3} s {n:>8} {:>8.3} ms {:>8.1}  {name}",
                d.as_secs_f64(),
                d.as_secs_f64() * 1000.0 / frames as f64,
                *n as f64 / frames as f64
            );
        }
        for (name, n) in &l.counts {
            eprintln!("[draw counts] {n:>8} {:>8.1} per frame  {name}", *n as f64 / frames as f64);
        }
        l.times.clear();
        l.counts.clear();
        l.since = None;
    }
}

/// The draws recorded and not yet submitted, and the memory they read from.
#[derive(Default)]
pub(crate) struct Batch {
    enc: Option<wgpu::CommandEncoder>,
    pass: Option<wgpu::RenderPass<'static>>,
    key: Option<PassKey>,
    /// The compute work of the geometry pipelines, which comes before the draws that read what it makes.
    pub(crate) pre: Option<wgpu::CommandEncoder>,
    arenas: [Vec<Arena>; 3],
}

/// A big buffer that the small buffers of a batch of draws are cut from (one allocation instead of several per draw). Its parts are not
/// used again until the batch has been handed to the GPU, whose queue keeps the order of the writes and the commands.
struct Arena {
    buffer: wgpu::Buffer,
    size: u64,
    used: u64,
    /// For the arenas the shaders read: what is written into the buffer, kept here until the batch goes to the GPU.
    shadow: Vec<u8>,
}

const ARENA_SIZE: u64 = 256 << 20;

/// What a part of an arena is for. A buffer is used one way only inside one pass (read-only, or written), so what a compute pass reads
/// and what it writes come from different buffers.
#[derive(Clone, Copy)]
pub(crate) enum Role {
    /// Read by the shaders: uniform blocks, vertex streams, parameters, indices.
    Read = 0,
    /// The ring the vertex stage writes and the geometry stage reads.
    RingIn = 1,
    /// What the geometry stage writes and the draw reads.
    Out = 2,
}

/// A part of a buffer.
#[derive(Clone)]
pub(crate) struct BufRef {
    pub buffer: wgpu::Buffer,
    pub offset: u64,
    pub size: u64,
}

impl BufRef {
    pub(crate) fn bound(&self) -> Bound {
        Bound::Buffer(self.buffer.clone(), self.offset, self.size)
    }

    pub(crate) fn slice(&self) -> wgpu::BufferSlice<'_> {
        self.buffer.slice(self.offset..self.offset + self.size)
    }
}

/// What a render pass draws into: the colour target, the depth target, and the number of colour outputs.
type PassKey = (u32, Option<u32>, usize);

#[cfg(test)]
mod tests {
    use super::*;

    /// A texture header at 0x100 with its image at 0x1000 (0x100 bytes) and its smaller levels at 0x2000 (0x40 bytes), in a buffer
    /// that stands for guest memory.
    fn memory() -> Vec<u8> {
        let mut m = vec![0u8; 0x3000];
        let mut put = |at: usize, v: u32| m[at..at + 4].copy_from_slice(&v.to_be_bytes());
        put(0x100 + 0x24, 0x1000);
        put(0x100 + 0x20, 0x100);
        put(0x100 + 0x2c, 0x2000);
        put(0x100 + 0x28, 0x40);
        put(0x100 + 4, 16);
        put(0x100 + 8, 16);
        m
    }

    fn hash(m: &mut [u8]) -> u64 {
        let g = Guest::new(m.as_mut_ptr());
        let h = crate::texture::Header::read(&g, 0x100);
        texture_source_hash(&g, 0x100, &h)
    }

    #[test]
    fn a_texture_is_decoded_again_only_when_what_it_is_made_from_changed() {
        let mut m = memory();
        let first = hash(&mut m);
        assert_eq!(hash(&mut m), first, "same bytes, same hash");
        // memory around it is not part of it
        m[0x0fff] ^= 1;
        m[0x1100] ^= 1;
        m[0x2040] ^= 1;
        assert_eq!(hash(&mut m), first);
        for at in [0x1000, 0x10ff, 0x2000, 0x203f, 0x100 + 0x34, 0x100 + 0x84] {
            let mut changed = memory();
            changed[at] ^= 0x10;
            assert_ne!(hash(&mut changed), first, "a change at {at:#x}");
        }
    }
}
