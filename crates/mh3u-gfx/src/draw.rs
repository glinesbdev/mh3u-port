//! What a draw call becomes: the game's shaders translated to WGSL, its vertex data decoded on the CPU, and a render pipeline made from
//! the recorded state, cached by what makes it different.

use crate::gx2::Gx2;
use crate::shader_cache::{FrontCache, Lookup, Ranges};
use crate::state::State;
use mh3u_latte::decode::decode;
use mh3u_latte::translate::{
    Context, IDENTITY_SELECT, Interface, PixelInputs, TexKind, VertexAttribute, VertexInputs, translate_pixel, translate_vertex,
};
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

/// The debugging override of pixel shaders (`MH3U_GFX_DEBUG_PS`, see `DebugPs::parse`), for every pixel shader or, with
/// `MH3U_GFX_DEBUG_PS_ONLY` (addresses in hex, comma-separated, as in `draws.txt`), only for those.
fn debug_ps(ps: u32) -> mh3u_latte::translate::DebugPs {
    static SETTING: std::sync::OnceLock<(mh3u_latte::translate::DebugPs, Option<Vec<u32>>)> = std::sync::OnceLock::new();
    let (mode, only) = SETTING.get_or_init(|| {
        let mode = std::env::var("MH3U_GFX_DEBUG_PS").map_or(Default::default(), |v| mh3u_latte::translate::DebugPs::parse(&v));
        let only = std::env::var("MH3U_GFX_DEBUG_PS_ONLY").ok().map(|v| parse_addresses(&v));
        (mode, only)
    });
    match only {
        Some(list) if !list.contains(&ps) => Default::default(),
        _ => *mode,
    }
}

/// Whether draws get the depth bias `GX2SetPolygonOffset` asks for: `MH3U_GFX_DEPTH_BIAS=off` (or `0`) leaves it out, to see whether a
/// draw lost to the depth its own depth-only pass wrote with a larger bias.
fn depth_bias_on() -> bool {
    static SETTING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SETTING.get_or_init(|| bias_setting(std::env::var("MH3U_GFX_DEPTH_BIAS").ok().as_deref()))
}

pub(crate) fn bias_setting(v: Option<&str>) -> bool {
    !matches!(
        v.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("off" | "0" | "no" | "false")
    )
}

/// Addresses written in hex (with or without `0x`), separated by commas or spaces; what does not read as one is left out.
pub(crate) fn parse_addresses(text: &str) -> Vec<u32> {
    text.split([',', ' ', ';'])
        .filter_map(|t| {
            let t = t.trim();
            let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
            (!t.is_empty()).then(|| u32::from_str_radix(t, 16).ok()).flatten()
        })
        .collect()
}

/// A vertex shader's `SQ_VTX_SEMANTIC` table (`GX2VertexShader` +0x40 the count, +0x44 the 32 entries, the semantic id in bits 0
/// to 7): entry `i` names the attribute that register `i + 1` is loaded with. `None` when the shader has no table (count 0).
pub(crate) fn vertex_semantics(g: &Guest, vs: u32) -> Option<Vec<u32>> {
    let count = g.u32(vs + 0x40).min(32);
    (count != 0).then(|| (0..count).map(|i| g.u32(vs + 0x44 + 4 * i) & 0xff).collect())
}

/// Which register each attribute stream is loaded into. The fetch shader tags each attribute with its location as the semantic id;
/// the vertex shader's semantic table says which register has which id (the first entry that matches; an attribute it does not name
/// is not loaded). With no table, location `n` goes to register `n + 1`. The village's scenery shaders load location 2 (the UVs) into
/// R2 and location 3 (the colour) into R3, skipping location 1: read as `location + 1` the UVs were the normal and the colour the
/// UVs, which made the scenery black.
pub(crate) fn vertex_attributes(semantics: &Option<Vec<u32>>, fetch: &[AttribStream]) -> Vec<VertexAttribute> {
    fetch
        .iter()
        .enumerate()
        .filter_map(|(i, a)| {
            let gpr = match semantics {
                Some(table) => table.iter().position(|&id| id == a.location)? as u32 + 1,
                None => a.location + 1,
            };
            Some(VertexAttribute {
                gpr,
                stream: i as u32,
                per_instance: a.per_instance,
                divisor: a.divisor,
            })
        })
        .collect()
}

pub struct Compiled {
    pub module: wgpu::ShaderModule,
    pub interface: Interface,
    /// The WGSL it was made from (for the pipeline cache on disk) and a hash of it (which names it there).
    pub wgsl: Arc<str>,
    pub id: u64,
}

impl Compiled {
    pub(crate) fn new(module: wgpu::ShaderModule, interface: Interface, wgsl: String) -> Compiled {
        let id = xxhash_rust::xxh3::xxh3_64(wgsl.as_bytes());
        Compiled {
            module,
            interface,
            wgsl: wgsl.into(),
            id,
        }
    }
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
/// The front key of a vertex shader: (shader, program, size, fetch shader contents (the address of its interned slice), half z).
pub type VertexFront = (u32, u32, u32, usize, bool);
/// What else a pixel shader's translation depends on: the kinds and component selectors of the textures it reads, and the alpha test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelVariant {
    pub kinds: [TexKind; 18],
    pub selects: [u32; 18],
    /// The alpha test's comparison and the bits of its reference; (7, 0), always, when it is off.
    pub alpha: (u32, u32),
}

/// The front key of a pixel shader: (shader, program, size, hash of the words its inputs are made from, what else it depends on).
pub type PixelFront = (u32, u32, u32, u64, PixelVariant);

/// The loop constants a shader brings (`GX2LoopVar`s: the register's byte offset from the first loop constant, and its value), by
/// the loop constant's number.
fn loop_constants(g: &Guest, count: u32, list: u32) -> [Option<u32>; 32] {
    let mut out = [None; 32];
    for k in 0..count.min(32) {
        let (offset, value) = (g.u32(list + 8 * k), g.u32(list + 8 * k + 4));
        if let Some(slot) = out.get_mut((offset / 4) as usize) {
            *slot = Some(value);
        }
    }
    out
}

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
    state: crate::pipelines::PipelineState,
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
    /// The fronts of the two (see `shader_cache`): by (shader, program, size, fetch shader contents, half z), and by (shader, program,
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
    /// Comparison samplers by their `GX2CompareFunction` (15 when the game set none: less or equal).
    pub compare_samplers: HashMap<u32, wgpu::Sampler>,
    /// How many times each colour buffer (by image) has been drawn into, cleared or copied to: a copy made of it is stale when this
    /// has moved on.
    pub target_version: HashMap<u32, u64>,
    /// Colour buffers a shader reads with a comparison (a shadow map drawn as `r32f` colour), copied into depth textures, with the
    /// `target_version` they were copied at.
    pub depth_copies: HashMap<u32, (wgpu::Texture, wgpu::TextureView, u64)>,
    pub depth_copy_pipeline: Option<(wgpu::RenderPipeline, wgpu::BindGroupLayout)>,
    /// Bind groups by what they bind, with the swap they were last used in.
    pub bind_groups: HashMap<GroupKey, (wgpu::BindGroup, u32)>,
    /// The uniform blocks uploaded, by (address, vec4s, bytes read): the guest bytes of each upload of the current batch, so a draw
    /// that finds the same bytes there again reads the same upload (compared in place, nothing copied or hashed).
    pub uniforms: HashMap<(u32, u32, u32), UniformVersions>,
    /// A buffer reused for the words of a uniform block.
    pub scratch: Vec<u8>,
    /// Geometry pipelines by the hashes of their programs and what else their translation depends on, with their front.
    pub geometry: HashMap<(u32, u32, u64), Option<Arc<GeometryEntry>>>,
    pub geometry_front: FrontCache<GeometryFront, (Option<Arc<GeometryEntry>>, Fetch)>,
    /// Why draws were not drawn, and how many.
    pub skipped: HashMap<&'static str, u64>,
    pub gs_draws: HashMap<(u32, u32, u32, u32, u32), u64>,
    /// Index lists by the draw and a hash of its index bytes (see `draw_indices`), and the bytes of their buffers.
    pub indices: HashMap<IndexKey, IndexEntry>,
    pub index_bytes: u64,
    /// Decoded vertex streams kept on the GPU, by what they were decoded from (the layout and a hash of the bytes).
    pub streams: HashMap<(u64, u64), StreamEntry>,
    pub stream_bytes: u64,
    /// Shaders translated and textures decoded so far (for the area-change lines, `perf.rs`).
    pub work: crate::perf::Work,
}

/// The uploads of one uniform block in the current batch (`batch`, see `Batch::generation`), newest last: its guest bytes and where they
/// went. Older batches' entries are reused, keeping their memory.
#[derive(Default)]
pub struct UniformVersions {
    batch: u64,
    used: usize,
    versions: Vec<(Vec<u8>, Option<BufRef>)>,
    swap: u32,
}

/// The `have` bytes of a uniform block at `address` into `out`, as they are (a word cut short by the block's size is completed with
/// zeros, as the rest of the block is). The GPU reads uniform blocks as little-endian words with no swap (the block's buffer resource
/// asks for none), so the game sends them already turned around: its floats come through `DMAECopyMem` with the 8-in-32 swap, and
/// packed colours it writes as bytes (red first) are read with red in the low byte.
pub(crate) fn uniform_words(g: &Guest, address: u32, have: usize, out: &mut Vec<u8>) {
    out.clear();
    g.with_bytes(address, have, |b| out.extend_from_slice(b));
    out.resize(have.next_multiple_of(4), 0);
}

/// How many uploads of one block a batch remembers (a block rewritten more often than this between flushes is uploaded each time).
const UNIFORM_VERSIONS: usize = 8;

impl UniformVersions {
    /// The upload of this batch that holds `now` (the block's guest bytes).
    fn find(&mut self, batch: u64, now: &[u8]) -> Option<BufRef> {
        if self.batch != batch {
            self.batch = batch;
            self.used = 0;
            return None;
        }
        self.versions[..self.used]
            .iter()
            .rev()
            .find(|(raw, _)| raw.as_slice() == now)
            .and_then(|(_, r)| r.clone())
    }

    fn insert(&mut self, batch: u64, raw: &[u8], r: &BufRef) {
        if self.batch != batch {
            self.batch = batch;
            self.used = 0;
        }
        if self.used == UNIFORM_VERSIONS {
            return;
        }
        if self.used == self.versions.len() {
            self.versions.push((Vec::new(), None));
        }
        let v = &mut self.versions[self.used];
        v.0.clear();
        v.0.extend_from_slice(raw);
        v.1 = Some(r.clone());
        self.used += 1;
    }
}

/// Uniform blocks of this size or more (the banks a shader reads by index are declared 4096 vec4, 64 KiB) go to slots of their own,
/// which are kept zero past what was written, so only the block's own bytes are copied and uploaded, not the zeros after them.
const SLOT_SIZE: u64 = 64 << 10;
const SLOTS_PER_BUFFER: usize = 16;

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

/// How the integer components of an attribute become what the shader reads (the `GX2AttribFormat` flags: `SCALED` 0x800 wins over
/// `INTEGER` 0x100, as in the fetch instruction's number format).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumFormat {
    /// Normalised to 0..1 (or -1..1 when signed).
    Norm,
    /// Left as integers.
    Int,
    /// Converted to floats as they are ("integer to float").
    Scaled,
}

/// What an attribute format is made of: the number of components, their sizes in bits (a packed format has several in one word,
/// lowest bits first), whether they are floats, and how the integer ones are read. `None` for a type that is not known.
fn attribute_layout(format: u32) -> Option<(&'static [u32], bool)> {
    Some(match format & 0x3f {
        0x00 => (&[8], false),
        0x01 => (&[4, 4], false),
        0x02 => (&[16], false),
        0x03 => (&[16], true),
        0x04 => (&[8, 8], false),
        0x05 => (&[32], false),
        0x06 => (&[32], true),
        0x07 => (&[16, 16], false),
        0x08 => (&[16, 16], true),
        0x09 => (&[11, 11, 10], true),
        0x0a => (&[8, 8, 8, 8], false),
        0x0b => (&[10, 10, 10, 2], false),
        0x0c => (&[32, 32], false),
        0x0d => (&[32, 32], true),
        0x0e => (&[16, 16, 16, 16], false),
        0x0f => (&[16, 16, 16, 16], true),
        0x10 => (&[32, 32, 32], false),
        0x11 => (&[32, 32, 32], true),
        0x12 => (&[32, 32, 32, 32], false),
        0x13 => (&[32, 32, 32, 32], true),
        _ => return None,
    })
}

fn num_format(format: u32) -> NumFormat {
    if format & 0x800 != 0 {
        NumFormat::Scaled
    } else if format & 0x100 != 0 {
        NumFormat::Int
    } else {
        NumFormat::Norm
    }
}

/// A short name of an attribute format, for reports (`snorm 16x4`, `float 32x3`, `uint->float 8x4`...).
pub(crate) fn attribute_format_name(format: u32) -> String {
    let Some((sizes, float)) = attribute_layout(format) else {
        return format!("unknown {format:#x}");
    };
    let signed = format & 0x200 != 0;
    let kind = if float {
        "float"
    } else {
        match (num_format(format), signed) {
            (NumFormat::Norm, false) => "unorm",
            (NumFormat::Norm, true) => "snorm",
            (NumFormat::Int, false) => "uint",
            (NumFormat::Int, true) => "sint",
            (NumFormat::Scaled, false) => "uint->float",
            (NumFormat::Scaled, true) => "sint->float",
        }
    };
    let degamma = if format & 0x400 != 0 { " srgb" } else { "" };
    let parts: Vec<String> = sizes.iter().map(u32::to_string).collect();
    format!("{kind} {}{degamma}", parts.join("_"))
}

/// The attribute's components as raw 32-bit patterns: floats for the float, normalised and scaled formats, integers (sign-extended
/// when signed) for the integer ones. A component the format does not have reads as 0, and the fourth as 1.
///
/// `endian` is the stream's `GX2EndianSwapMode`: 0 reads the bytes as they are (little-endian); 1 swaps the bytes in every 16 bits, 2
/// in every 32 bits, and 3 (the default) by the size of the components (none for 8-bit ones, 16 for 16-bit ones, 32 for the rest),
/// which reads the big-endian values the game wrote.
fn decode_attribute(format: u32, endian: u32, bytes: &[u8]) -> [u32; 4] {
    let Some((sizes, float)) = attribute_layout(format) else {
        return [0; 4];
    };
    let signed = format & 0x200 != 0;
    let num = num_format(format);
    let int_out = !float && num == NumFormat::Int;
    let one = if int_out { 1 } else { 1.0f32.to_bits() };
    let mut out = [0, 0, 0, one];
    let total: u32 = sizes.iter().sum();
    let len = total.div_ceil(8) as usize;
    let mut buf = [0u8; 16];
    for (k, b) in buf.iter_mut().enumerate().take(len) {
        *b = bytes.get(k).copied().unwrap_or(0);
    }
    // the swap unit
    let unit = match endian {
        0 => 1,
        1 => 2,
        2 => 4,
        // the default: none for 8-bit (and 4-bit) components, 16 for 16-bit ones, 32 for the rest (the packed formats too)
        _ => match sizes[0] {
            0..=8 => 1,
            16 => 2,
            _ => 4,
        },
    };
    if unit > 1 && len >= unit {
        for chunk in buf[..len - len % unit].chunks_mut(unit) {
            chunk.reverse();
        }
    }
    let mut bit = 0usize;
    let read = |bit: usize, n: u32| -> u32 {
        // little-endian bit stream
        let mut v = 0u64;
        for k in 0..n.div_ceil(8) as usize + 1 {
            v |= u64::from(*buf.get(bit / 8 + k).unwrap_or(&0)) << (8 * k);
        }
        ((v >> (bit % 8)) & ((1u64 << n) - 1)) as u32
    };
    for (k, &n) in sizes.iter().enumerate() {
        let raw = read(bit, n);
        bit += n as usize;
        out[k] = if float {
            match n {
                32 => raw,
                16 => half_to_f32(raw as u16).to_bits(),
                11 => small_float(raw, 6).to_bits(),
                _ => small_float(raw, 5).to_bits(),
            }
        } else {
            let value = if signed && n < 32 && raw >> (n - 1) & 1 == 1 {
                (raw | !((1u32 << n) - 1)) as i32
            } else {
                raw as i32
            };
            match num {
                NumFormat::Int => value as u32,
                NumFormat::Scaled if signed => (value as f32).to_bits(),
                NumFormat::Scaled => (raw as f32).to_bits(),
                NumFormat::Norm if signed => {
                    let max = ((1u64 << (n - 1)) - 1).max(1) as f64;
                    ((f64::from(value) / max).max(-1.0) as f32).to_bits()
                }
                NumFormat::Norm => {
                    let v = (f64::from(raw) / ((1u64 << n) - 1) as f64) as f32;
                    if format & 0x400 != 0 && n == 8 && k < 3 {
                        srgb_to_linear(v)
                    } else {
                        v
                    }
                    .to_bits()
                }
            }
        };
    }
    out
}

/// An unsigned float of the packed 11-11-10 format: 5 exponent bits and `mantissa` bits.
fn small_float(bits: u32, mantissa: u32) -> f32 {
    let (e, m) = ((bits >> mantissa) & 0x1f, bits & ((1 << mantissa) - 1));
    let scale = (1u32 << mantissa) as f32;
    if e == 0 {
        m as f32 / scale * 2f32.powi(-14)
    } else if e == 31 {
        if m == 0 { f32::INFINITY } else { f32::NAN }
    } else {
        (1.0 + m as f32 / scale) * 2f32.powi(e as i32 - 15)
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
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
    attribute_layout(format).map_or(4, |(sizes, _)| sizes.iter().sum::<u32>().div_ceil(8))
}

pub(crate) fn blend_factor(f: u32) -> wgpu::BlendFactor {
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

pub(crate) fn blend_op(op: u32) -> wgpu::BlendOperation {
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

fn stencil_op(f: u32) -> wgpu::StencilOperation {
    use wgpu::StencilOperation as S;
    match f {
        1 => S::Zero,
        2 => S::Replace,
        3 => S::IncrementClamp,
        4 => S::DecrementClamp,
        5 => S::Invert,
        6 => S::IncrementWrap,
        7 => S::DecrementWrap,
        _ => S::Keep,
    }
}

/// One face's stencil settings: (comparison, fail, depth fail, pass).
pub(crate) fn stencil_face(f: [u32; 4]) -> wgpu::StencilFaceState {
    wgpu::StencilFaceState {
        compare: compare(f[0]),
        fail_op: stencil_op(f[1]),
        depth_fail_op: stencil_op(f[2]),
        pass_op: stencil_op(f[3]),
    }
}

/// How much of a uniform block the shader can read: `GX2Set*UniformBlock` gives the hardware the size in 256-byte units, rounded up
/// (`SQ_ALU_CONST_BUFFER_SIZE`, 9 bits), so a block given as 0x80 bytes still reads 16 vec4s.
pub(crate) fn readable_bytes(size: u32) -> u32 {
    (size.div_ceil(256) & 0x1ff) * 256
}

/// The program that writes a colour texture's first component as depth: one triangle over the target.
pub(crate) const DEPTH_COPY_WGSL: &str = "
@group(0) @binding(0) var src: texture_2d<f32>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}
struct DepthOut { @builtin(frag_depth) z: f32 }
@fragment fn fs(@builtin(position) at: vec4<f32>) -> DepthOut {
    var o: DepthOut;
    o.z = clamp(textureLoad(src, vec2<i32>(at.xy), 0).x, 0.0, 1.0);
    return o;
}
";

fn depth_copy_pipeline(device: &wgpu::Device) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("depth copy"),
        source: wgpu::ShaderSource::Wgsl(DEPTH_COPY_WGSL.into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("depth copy"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("depth copy"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("depth copy"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[],
        }),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, layout)
}

/// Whether the colour hardware blends into a buffer of this format: not one of 32-bit float components (the shadow pass draws its
/// r32f map with the blending of the draws before it still on, and the value it writes, z/w, is also its alpha), so it writes over.
pub(crate) fn blends(format: wgpu::TextureFormat) -> bool {
    use wgpu::TextureFormat as F;
    !matches!(format, F::R32Float | F::Rg32Float | F::Rgba32Float)
}

/// The comparison of a shadow lookup's sampler: the sampler's own (`GX2CompareFunction`), or less or equal when it has none (15).
fn compare_sampler_function(f: u32) -> wgpu::CompareFunction {
    if f > 7 { wgpu::CompareFunction::LessEqual } else { compare(f) }
}

pub(crate) fn compare(f: u32) -> wgpu::CompareFunction {
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
    // the depth formats (0x16 is the 10-11-11 float colour format, not a depth one)
    if matches!(format & 0x3f, 0x11..=0x14 | 0x1c) {
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

    /// Keeps a shader's listing and WGSL (or why it was not translated) for the frame that is written out.
    pub(crate) fn keep_shader_text(&mut self, name: String, code: &[u8], result: Result<&str, &str>) {
        if !self.diag.keeps_shaders() {
            return;
        }
        let listing = decode(code).map_or_else(|e| format!("not decoded: {e}"), |d| mh3u_latte::decode::listing(&d));
        let tail = match result {
            Ok(wgsl) => format!("--- WGSL ---\n{wgsl}"),
            Err(e) => format!("--- NOT TRANSLATED ---\n{e}\n"),
        };
        self.diag.shader_texts.insert(name, format!("--- listing ---\n{listing}\n{tail}"));
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
        // the shader's semantic table (which register each attribute goes to) is checked with its program
        let ranges = [(program, size), (vs + 0x40, 0x84), (0, 0)];
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
        let key = (
            vs,
            program,
            hash_fast(&code, 0) ^ hash_of(&(fetch, half_z, vertex_semantics(g, vs))),
        );
        if let Some(c) = self.caches.vertex.get(&key) {
            return c.clone();
        }
        self.caches.work.translated += 1;
        let result = (|| {
            let decoded = decode(&code).map_err(|e| e.to_string())?;
            let attributes = vertex_attributes(&vertex_semantics(g, vs), fetch);
            let constants = loop_constants(g, g.u32(vs + 0xf4), g.u32(vs + 0xf8));
            let loop_const = move |n: u32| constants.get(n as usize).copied().flatten();
            let mut ctx = Context::new(&|_| TexKind::D2);
            ctx.loop_const = &loop_const;
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
        self.keep_shader_text(
            format!("vs-{vs:08x}"),
            &code,
            result.as_ref().map(|t| t.wgsl.as_str()).map_err(String::as_str),
        );
        let compiled = match result {
            Ok(t) => {
                let module = self.compile("vertex shader", &t.wgsl);
                Some(Arc::new(Compiled::new(module, t.interface, t.wgsl)))
            }
            Err(e) => {
                self.diag.untranslated(format!("vertex shader {vs:#x}"), &e);
                self.report(format!("vertex shader {vs:#x} not translated: {e}"));
                None
            }
        };
        self.caches.vertex.insert(key, compiled.clone());
        compiled
    }

    fn pixel_shader(&mut self, g: &Guest, ps: u32, semantics_at: u32, variant: &PixelVariant) -> Option<Arc<Compiled>> {
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
        let front = (ps, program, size, hash_fast(&words_bytes, 0x9), *variant);
        let swaps = self.swaps;
        let (found, how) = self.caches.pixel_front.get(&front, swaps, |r| hash_ranges(g, r));
        count_lookup(&how, "pixel shader hits", "pixel shader checks", "pixel shader misses");
        if let Some(c) = found {
            return c;
        }
        let ranges = [(program, size), (0, 0), (0, 0)];
        let compiled = self.pixel_shader_slow(g, ps, semantics_at, variant);
        let hash = hash_ranges(g, &ranges);
        self.caches.pixel_front.insert(front, ranges, hash, swaps, compiled.clone());
        compiled
    }

    /// The pixel shader from its program bytes and inputs: translated once for each.
    fn pixel_shader_slow(&mut self, g: &Guest, ps: u32, semantics_at: u32, variant: &PixelVariant) -> Option<Arc<Compiled>> {
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
            hash_fast(&code, 0) ^ hash_of(&(inputs.count, &inputs.flat, &inputs.locations, variant)),
        );
        if let Some(c) = self.caches.pixel.get(&key) {
            return c.clone();
        }
        self.caches.work.translated += 1;
        let PixelVariant { kinds, selects, alpha } = *variant;
        let constants = loop_constants(g, g.u32(ps + 0xc8), g.u32(ps + 0xcc));
        let result = (|| {
            let decoded = decode(&code).map_err(|e| e.to_string())?;
            let kind = move |r: u32| kinds.get(r as usize).copied().unwrap_or(TexKind::D2);
            let select = move |r: u32| selects.get(r as usize).copied().unwrap_or(IDENTITY_SELECT);
            let loop_const = move |n: u32| constants.get(n as usize).copied().flatten();
            let mut ctx = Context::new(&kind);
            ctx.texture_select = &select;
            ctx.loop_const = &loop_const;
            ctx.alpha_test = (alpha.0 != 7).then_some((alpha.0, f32::from_bits(alpha.1)));
            ctx.debug = debug_ps(ps);
            translate_pixel(&decoded, &inputs, &ctx)
        })();
        self.keep_shader_text(
            format!("ps-{ps:08x}"),
            &code,
            result.as_ref().map(|t| t.wgsl.as_str()).map_err(String::as_str),
        );
        let compiled = match result {
            Ok(mut t) => {
                if std::env::var_os("MH3U_GFX_SOLID").is_some() {
                    // a debugging aid: every pixel drawn is red, to see where the geometry goes
                    t.wgsl = t
                        .wgsl
                        .replace("    return o;\n}", "    o.c0 = vec4<f32>(1.0, 0.0, 0.0, 1.0);\n    return o;\n}");
                }
                let module = self.compile("pixel shader", &t.wgsl);
                Some(Arc::new(Compiled::new(module, t.interface, t.wgsl)))
            }
            Err(e) => {
                self.diag.untranslated(format!("pixel shader {ps:#x}"), &e);
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
        let volume = cached.texture.dimension() == wgpu::TextureDimension::D3;
        if volume != (kind == TexKind::D3) {
            // a volume read as a flat texture or the other way round: the shader gets the stand-in
            return None;
        }
        let view = cached.texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(match kind {
                TexKind::D3 => wgpu::TextureViewDimension::D3,
                TexKind::Cube if cached.layers > 1 => wgpu::TextureViewDimension::D2Array,
                _ => wgpu::TextureViewDimension::D2,
            }),
            array_layer_count: Some(match kind {
                TexKind::D3 => 1,
                TexKind::Cube => 6.min(cached.layers),
                _ => 1,
            }),
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
                    self.diag.decoded(&h, d.laid_pitch, d.format.block_dimensions().0);
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
                        dimension: if d.volume {
                            wgpu::TextureDimension::D3
                        } else {
                            wgpu::TextureDimension::D2
                        },
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
                                depth_or_array_layers: l.depth,
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
                    self.caches.work.decoded += 1;
                    Lap::count("texture bytes decoded", u64::from(h.image_size) + u64::from(h.mip_size));
                    Some(())
                }
                Err(e) => {
                    self.diag.decode_failed(&h, &e);
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
    /// Returns how many index entries of the front caches it looked at.
    pub(crate) fn invalidate_shaders(&mut self, address: u32, size: u32) -> usize {
        self.caches.vertex_front.invalidate(address, size);
        self.caches.pixel_front.invalidate(address, size);
        self.caches.geometry_front.invalidate(address, size);
        self.caches.vertex_front.examined + self.caches.pixel_front.examined + self.caches.geometry_front.examined
    }

    /// How big each cache is, for `[gfx caches]` (a cache that only grows shows here).
    pub(crate) fn cache_sizes(&self) -> String {
        let c = &self.caches;
        let (made, being_made) = self.pipes.sizes();
        format!(
            "shader fronts: vertex {}, pixel {}, geometry {}; translations: vertex {}, pixel {}, geometry {}; pipelines: {} by shader \
             address, {} made, {} being made, {} left from the disk; fetch shaders {} distinct; bind groups {}; textures {}; vertex \
             streams {}; index lists {}; uniform blocks {}",
            c.vertex_front.len(),
            c.pixel_front.len(),
            c.geometry_front.len(),
            c.vertex.len(),
            c.pixel.len(),
            c.geometry.len(),
            c.pipelines.len(),
            made,
            being_made,
            self.pipes.prebuilding,
            self.fetch_interned.values().map(Vec::len).sum::<usize>(),
            c.bind_groups.len(),
            c.textures.len(),
            c.streams.len(),
            c.indices.len(),
            c.uniforms.len()
        )
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

    /// The depth texture a colour buffer is copied into to be read with a comparison (made once, at the colour buffer's size).
    fn depth_copy_view(&mut self, image: u32) -> wgpu::TextureView {
        if let Some((_, view, _)) = self.caches.depth_copies.get(&image) {
            return view.clone();
        }
        let (width, height) = {
            let t = &self.targets[&image].texture;
            (t.width(), t.height())
        };
        let texture = self.d.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("depth copy of a colour buffer"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        // u64::MAX: never copied yet
        self.caches.depth_copies.insert(image, (texture, view.clone(), u64::MAX));
        view
    }

    /// Copies the colour buffer's first component into its depth copy (clamped to 0..1), unless it has not changed since the last
    /// copy. Recorded in the batch's encoder, outside any pass, before the draw that reads it.
    fn refresh_depth_copy(&mut self, image: u32) {
        let version = self.caches.target_version.get(&image).copied().unwrap_or(0);
        let Some((texture, _, made)) = self.caches.depth_copies.get(&image) else {
            return;
        };
        if *made == version {
            return;
        }
        let depth_view = texture.create_view(&Default::default());
        let Some(source) = self.targets.get(&image).map(|t| t.view.clone()) else {
            return;
        };
        let device = self.d.device.clone();
        let (pipeline, layout) = self
            .caches
            .depth_copy_pipeline
            .get_or_insert_with(|| depth_copy_pipeline(&device))
            .clone();
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("depth copy"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&source),
            }],
        });
        self.end_pass();
        let enc = self
            .batch
            .enc
            .get_or_insert_with(|| device.create_command_encoder(&Default::default()));
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("depth copy"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        if let Some(entry) = self.caches.depth_copies.get_mut(&image) {
            entry.2 = version;
        }
        crate::draw::Lap::count("colour buffers copied for a comparison", 1);
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
        // the bytes each buffer is read from: one span per buffer and range of items (buffer, first item, items, bytes needed past the
        // last item's start), at most one per attribute
        let mut spans = [(0u32, 0u32, 0u32, 0u32); 16];
        let mut n_spans = 0;
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
            match spans[..n_spans].iter_mut().find(|s| (s.0, s.1, s.2) == (a.buffer, first, items)) {
                Some(s) => s.3 = s.3.max(end),
                None => {
                    spans[n_spans] = (a.buffer, first, items, end);
                    n_spans += 1;
                }
            }
        }
        let mut bytes_hashed = 0;
        for &(buffer, first, items, end) in &spans[..n_spans] {
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
            // hashed where they are (no copy), with a hash that does many bytes per cycle
            mix(g.with_bytes(b.address.wrapping_add(from as u32), len as usize, |raw| {
                xxhash_rust::xxh3::xxh3_64(raw)
            }));
            bytes_hashed += len;
        }
        Lap::count("vertex bytes hashed", bytes_hashed);
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

    /// The index buffer of a draw (vertex numbers relative to `lowest`), and the vertices it uses; `None` when it draws nothing or its
    /// primitive type is not drawn. A list made before from the same index bytes (hashed where they are) and draw is used again: the
    /// first time it is put in the batch's arena, and when it comes again it gets a buffer of its own that later frames use as it is.
    pub(crate) fn draw_indices(
        &mut self,
        g: &Guest,
        restart_index: Option<u32>,
        primitive: u32,
        count: u32,
        indexed: Option<(u32, u32)>,
        first: u32,
    ) -> Option<DrawIndices> {
        let (_, expand) = topology_of(primitive)?;
        let bytes = match indexed {
            Some((kind, address)) => g.with_bytes(address, index_bytes(kind, count), xxhash_rust::xxh3::xxh3_64),
            None => 0,
        };
        let key = IndexKey {
            indexed,
            count,
            first,
            primitive,
            restart_index,
            bytes,
        };
        let swaps = self.swaps;
        // the bytes are compared too, so a hash that happens to match is not trusted
        let same = |e: &IndexEntry| match indexed {
            Some((kind, address)) => g.with_bytes(address, index_bytes(kind, count), |b| b == e.raw.as_slice()),
            None => true,
        };
        if let Some(e) = self.caches.indices.get_mut(&key).filter(|e| same(e)) {
            e.used = swaps;
            Lap::count("index list hits", 1);
            let (count, lowest, vertices) = (e.count, e.lowest, e.vertices);
            let buffer = match &e.buffer {
                Some(b) => b.clone(),
                None => {
                    // seen again: worth a buffer of its own
                    let list = std::mem::take(&mut e.indices);
                    let b = self.d.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("index list"),
                        size: u64::from(count.max(1)) * 4,
                        usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                    let bytes: Vec<u8> = list.iter().flat_map(|i| i.to_le_bytes()).collect();
                    self.d.queue.write_buffer(&b, 0, &bytes);
                    self.caches.index_bytes += bytes.len() as u64;
                    if let Some(e) = self.caches.indices.get_mut(&key) {
                        e.buffer = Some(b.clone());
                    }
                    b
                }
            };
            return Some(DrawIndices {
                buffer: BufRef {
                    buffer,
                    offset: 0,
                    size: u64::from(count) * 4,
                },
                count,
                lowest,
                vertices,
            });
        }
        Lap::count("index list misses", 1);
        let IndexList { indices, lowest, vertices } = index_list(g, restart_index, primitive, expand, count, indexed, first)?;
        let raw = match indexed {
            Some((kind, address)) => g.with_bytes(address, index_bytes(kind, count), <[u8]>::to_vec),
            None => Vec::new(),
        };
        let index_bytes: Vec<u8> = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let buffer = self.arena_write(&index_bytes, index_bytes.len() as u64);
        let count = indices.len() as u32;
        // forget the lists not used for a while when they take too much
        if self.caches.index_bytes > (256 << 20) {
            self.caches.indices.retain(|_, e| e.used + 120 >= swaps);
            self.caches.index_bytes = self
                .caches
                .indices
                .values()
                .filter(|e| e.buffer.is_some())
                .map(|e| u64::from(e.count) * 4)
                .sum();
        }
        if self.caches.indices.len() >= INDEX_LIST_LIMIT {
            self.caches.indices.retain(|_, e| e.used + 2 >= swaps);
        }
        self.caches.indices.insert(
            key,
            IndexEntry {
                raw,
                indices,
                buffer: None,
                count,
                lowest,
                vertices,
                used: swaps,
            },
        );
        Some(DrawIndices {
            buffer,
            count,
            lowest,
            vertices,
        })
    }

    /// The upload of one uniform block: the guest's big-endian words as the shader reads them, `vec4s` of them (zeros past the block's
    /// size). The same bytes at the same place uploaded earlier in the batch are not uploaded again: the draw reads that upload (the
    /// guest bytes are compared, not hashed), which also lets it share the bind group of the draw before.
    pub(crate) fn uniform_buffer(&mut self, g: &Guest, block: &crate::state::UniformBlock, vec4s: u32) -> BufRef {
        let size = u64::from(vec4s.max(1)) * 16;
        let have = if block.address != 0 {
            (readable_bytes(block.size) as usize).min(size as usize)
        } else {
            0
        };
        let key = (block.address, vec4s, have as u32);
        let batch = self.batch.generation;
        let swaps = self.swaps;
        let memo = self.caches.uniforms.entry(key).or_default();
        memo.swap = swaps;
        if let Some(r) = g.with_bytes(block.address, have, |now| memo.find(batch, now)) {
            Lap::count("uniform uploads shared", 1);
            return r;
        }
        let mut words = std::mem::take(&mut self.caches.scratch);
        uniform_words(g, block.address, have, &mut words);
        let r = if size >= SLOT_SIZE {
            Lap::count("uniform blocks read by index", 1);
            self.slot_write(&words, size)
        } else {
            self.arena_write(&words, size)
        };
        Lap::count("uniform uploads", 1);
        Lap::count("uniform bytes uploaded", words.len() as u64);
        let memo = self.caches.uniforms.get_mut(&key).expect("made above");
        g.with_bytes(block.address, have, |now| memo.insert(batch, now, &r));
        self.caches.scratch = words;
        r
    }

    /// A slot of `size` (at most `SLOT_SIZE`) bytes holding `data` and zeros after it. Only `data`, and the part of the slot an earlier
    /// batch wrote past it, are uploaded: the rest of the slot is zero already (buffers start zeroed).
    fn slot_write(&mut self, data: &[u8], size: u64) -> BufRef {
        debug_assert!(size <= SLOT_SIZE && data.len() as u64 <= size && data.len().is_multiple_of(4));
        let k = self.batch.slots_used;
        self.batch.slots_used += 1;
        if k / SLOTS_PER_BUFFER >= self.batch.slot_buffers.len() {
            let buffer = self.d.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("uniform slots"),
                size: SLOT_SIZE * SLOTS_PER_BUFFER as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.batch.slot_buffers.push(buffer);
            self.batch.slot_marks.resize(self.batch.slot_buffers.len() * SLOTS_PER_BUFFER, 0);
        }
        let buffer = self.batch.slot_buffers[k / SLOTS_PER_BUFFER].clone();
        let offset = (k % SLOTS_PER_BUFFER) as u64 * SLOT_SIZE;
        if !data.is_empty() {
            self.d.queue.write_buffer(&buffer, offset, data);
        }
        // what an earlier upload wrote past this one's end goes back to zero
        let mark = self.batch.slot_marks[k];
        if mark > data.len() as u64 {
            let zeros = vec![0u8; (mark - data.len() as u64) as usize];
            self.d.queue.write_buffer(&buffer, offset + data.len() as u64, &zeros);
        }
        self.batch.slot_marks[k] = data.len() as u64;
        BufRef { buffer, offset, size }
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
            self.caches.uniforms.retain(|_, u| u.swap + 60 >= swaps);
            // index lists drawn once and not again are not kept long
            self.caches
                .indices
                .retain(|_, e| e.used + 120 >= swaps && (e.buffer.is_some() || e.used + 2 >= swaps));
            self.caches.index_bytes = self
                .caches
                .indices
                .values()
                .filter(|e| e.buffer.is_some())
                .map(|e| u64::from(e.count) * 4)
                .sum();
            // an invalidated texture is kept in case its bytes did not change; one not drawn since goes as it did before
            self.caches.textures.retain(|_, t| !t.dirty || t.used + 60 >= swaps);
            // shader fronts not drawn with for a while, and the least recently used past their cap
            let aged =
                self.caches.vertex_front.sweep(swaps) + self.caches.pixel_front.sweep(swaps) + self.caches.geometry_front.sweep(swaps);
            Lap::count("shader front entries aged out", aged as u64);
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
        *self.caches.target_version.entry(image).or_default() += 1;
        let (target_w, target_h, target_format) = {
            let t = &self.targets[&image];
            (t.texture.width(), t.texture.height(), t.format)
        };
        // the textures the pixel shader will look at, to know their kinds and component selectors, and the alpha test
        let mut variant = PixelVariant {
            kinds: [TexKind::D2; 18],
            selects: [IDENTITY_SELECT; 18],
            alpha: (7, 0),
        };
        for unit in 0..18 {
            let t = st.pixel_textures[unit];
            if t != 0 {
                variant.kinds[unit] = tex_kind(g.u32(t), g.u32(t + 0x14));
                variant.selects[unit] = g.u32(t + 0x84) & 0x0707_0707;
                if self.diag.report {
                    self.diag.texture(g, t);
                }
            }
        }
        if self.diag.report {
            for a in fetch.iter() {
                self.diag.attribute(a.format, a.endian);
            }
        }
        let (alpha_on, alpha_func, alpha_ref) = st.alpha_test;
        if alpha_on && alpha_func & 7 != 7 {
            variant.alpha = (alpha_func & 7, alpha_ref.to_bits());
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
                    self.diag.skipped_for(format!("geometry pipeline {:#x}", st.geometry_shader));
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
                    self.diag.skipped_for(format!("vertex shader {:#x}", st.vertex_shader));
                    *self.caches.skipped.entry("vertex shader not translated").or_default() += 1;
                    return;
                };
                vs
            }
        };
        let semantics_at = geo.as_ref().map_or(st.vertex_shader + 0x10, |x| x.semantics_at);
        let Some(ps) = self.pixel_shader(g, st.pixel_shader, semantics_at, &variant) else {
            self.diag.skipped_for(format!("pixel shader {:#x}", st.pixel_shader));
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
                let Some((topology, _)) = topology_of(primitive) else {
                    self.report(format!("the primitive type {primitive:#x} is not drawn"));
                    *self.caches.skipped.entry("primitive type").or_default() += 1;
                    return;
                };
                let Some(DrawIndices {
                    buffer: index_buffer,
                    count: index_count,
                    lowest,
                    vertices,
                }) = self.draw_indices(g, st.restart_index, primitive, count, indexed, first)
                else {
                    return;
                };
                lap.mark("index list");
                let (va, meta_buffer) = self.cached_streams(g, &st, &fetch, lowest, vertices.min(1 << 22), instances);
                lap.mark("decode streams");
                (va, meta_buffer, index_buffer, index_count, topology)
            };
        lap.mark("indices, streams, buffers");
        // the pipeline
        let depth = if st.depth_buffer != 0 {
            self.depth_target(g, st.depth_buffer)
        } else {
            None
        };
        // culling both faces leaves no triangle (lines and points are not culled)
        if st.cull_front
            && st.cull_back
            && matches!(
                topology,
                wgpu::PrimitiveTopology::TriangleList | wgpu::PrimitiveTopology::TriangleStrip
            )
        {
            *self.caches.skipped.entry("both faces culled").or_default() += 1;
            return;
        }
        let has_stencil = |d: &(u32, wgpu::TextureFormat)| d.1 == wgpu::TextureFormat::Depth24PlusStencil8;
        let use_depth = depth.filter(|d| st.depth_test || st.depth_write || st.stencil_test && has_stencil(d));
        let masks = st.stencil.masks.unwrap_or([0xff, 0xff, 0, 0xff, 0xff, 0]);
        let stencil = use_depth.filter(|d| st.stencil_test && has_stencil(d)).map(|_| {
            let back = if st.stencil.back_enabled {
                st.stencil.back
            } else {
                st.stencil.front
            };
            (st.stencil.front, back, masks[0], masks[1])
        });
        // one depth bias for both faces: the front's when front faces are offset, else the back's
        let [front_offset, front_scale, back_offset, back_scale, bias_clamp] = st.polygon_offset;
        let bias = match (st.offset_front, st.offset_back) {
            (true, _) => (front_offset.round() as i32, front_scale.to_bits(), bias_clamp.to_bits()),
            (false, true) => (back_offset.round() as i32, back_scale.to_bits(), bias_clamp.to_bits()),
            _ => (0, 0, 0),
        };
        let bias = if use_depth.is_some() && depth_bias_on() { bias } else { (0, 0, 0) };
        let key = PipelineKey {
            vs: Arc::as_ptr(&vs) as usize,
            ps: Arc::as_ptr(&ps) as usize,
            state: crate::pipelines::PipelineState {
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
                stencil,
                bias,
            },
        };
        let entry = match self.caches.pipelines.get(&key) {
            Some(e) => e.clone(),
            None => {
                // made already for these shaders' WGSL (from the disk cache, or for the same shaders at other addresses), made now, or
                // handed to a worker: then this draw is not drawn, and the ones after it are once the pipeline is there
                let Some(built) = self.pipes.get(&vs, &ps, &key.state) else {
                    *self.caches.skipped.entry("pipeline being made").or_default() += 1;
                    lap.mark("pipeline");
                    return;
                };
                let e = Arc::new(PipelineEntry {
                    pipeline: built.pipeline,
                    layouts: built.layouts,
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
        let mut depth_reads: Vec<u32> = Vec::new();
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
                // a colour buffer read with a comparison (the village's shadow map is drawn as r32f colour): a depth copy of it
                if k == TexKind::Depth2d && image != draw_image && self.targets.contains_key(&image) {
                    depth_reads.push(image);
                    return (r, self.depth_copy_view(image));
                }
                if t != 0
                    && k != TexKind::Depth2d
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
        for &unit in &ps.interface.compare_samplers {
            // the comparison the game gave the sampler (`GX2InitSamplerDepthCompare`), less or equal when it gave none
            let at = st.pixel_samplers.get(unit as usize).copied().unwrap_or(0);
            let func = if at == 0 {
                15
            } else {
                crate::texture::Sampler::read(g, at).compare.unwrap_or(15)
            };
            let sampler = self
                .caches
                .compare_samplers
                .entry(func)
                .or_insert_with(|| {
                    device.create_sampler(&wgpu::SamplerDescriptor {
                        compare: Some(compare_sampler_function(func)),
                        mag_filter: wgpu::FilterMode::Linear,
                        min_filter: wgpu::FilterMode::Linear,
                        ..Default::default()
                    })
                })
                .clone();
            tex_entries.push((32 + unit, Bound::Sampler(sampler)));
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
        for image in depth_reads {
            self.refresh_depth_copy(image);
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
        let [vx, vy, vw, vh, near, far] = st.viewport;
        // the depth range, when the target API can have it (0 to 1, near not past far)
        let (near, far) = if (0.0..=1.0).contains(&near) && (0.0..=1.0).contains(&far) && near <= far && far > 0.0 {
            (near, far)
        } else {
            (0.0, 1.0)
        };
        if vw > 0.0 && vh > 0.0 {
            let x = vx.clamp(0.0, target_w as f32);
            let y = vy.clamp(0.0, target_h as f32);
            pass.set_viewport(
                x,
                y,
                vw.min(target_w as f32 - x).max(1.0),
                vh.min(target_h as f32 - y).max(1.0),
                near,
                far,
            );
        } else {
            pass.set_viewport(0.0, 0.0, target_w as f32, target_h as f32, near, far);
        }
        let [sx, sy, sw, sh] = st.scissor;
        if sw > 0 && sh > 0 && sx < target_w && sy < target_h {
            pass.set_scissor_rect(sx, sy, sw.min(target_w - sx), sh.min(target_h - sy));
        } else {
            pass.set_scissor_rect(0, 0, target_w, target_h);
        }
        pass.set_pipeline(&entry.pipeline);
        if stencil.is_some() {
            pass.set_stencil_reference(masks[2]);
        }
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
        self.batch.generation += 1;
        self.batch.slots_used = 0;
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

/// What a draw's index list is made from: its index buffer (type and address) and a hash of the bytes there, the count, the first vertex,
/// the primitive type and the restart index.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct IndexKey {
    indexed: Option<(u32, u32)>,
    count: u32,
    first: u32,
    primitive: u32,
    restart_index: Option<u32>,
    bytes: u64,
}

/// An index list made before: the list itself until it gets a buffer of its own (the second time it is drawn).
pub(crate) struct IndexEntry {
    /// The index bytes it was made from.
    raw: Vec<u8>,
    indices: Vec<u32>,
    buffer: Option<wgpu::Buffer>,
    count: u32,
    lowest: u32,
    vertices: u32,
    used: u32,
}

/// How many index lists are remembered before the ones not drawn in the last two frames are forgotten.
const INDEX_LIST_LIMIT: usize = 65536;

/// The bytes of `count` indices of `GX2IndexType` `kind` (32-bit for 1 and 9, 16-bit otherwise).
fn index_bytes(kind: u32, count: u32) -> usize {
    count as usize * if matches!(kind, 1 | 9) { 4 } else { 2 }
}

/// A draw's index buffer on the GPU: `count` vertex numbers relative to `lowest`, which with the `vertices` after it are what the draw
/// reads.
pub(crate) struct DrawIndices {
    pub buffer: BufRef,
    pub count: u32,
    pub lowest: u32,
    pub vertices: u32,
}

/// The list a draw's primitives are made of.
pub(crate) struct IndexList {
    /// Vertex numbers relative to `lowest` (`u32::MAX` restarts a strip).
    pub indices: Vec<u32>,
    pub lowest: u32,
    /// From `lowest` to the highest vertex used.
    pub vertices: u32,
}

/// The GPU's primitive topology for a `GX2PrimitiveMode`, and whether its primitives are made into triangles first (fans, quads, quad
/// strips, rectangles).
pub(crate) fn topology_of(primitive: u32) -> Option<(wgpu::PrimitiveTopology, bool)> {
    Some(match primitive {
        1 => (wgpu::PrimitiveTopology::PointList, false),
        2 => (wgpu::PrimitiveTopology::LineList, false),
        3 => (wgpu::PrimitiveTopology::LineStrip, false),
        4 => (wgpu::PrimitiveTopology::TriangleList, false),
        6 => (wgpu::PrimitiveTopology::TriangleStrip, false),
        5 | 0x13 | 0x14 | 0x12 => (wgpu::PrimitiveTopology::TriangleList, true),
        _ => return None,
    })
}

/// The vertex numbers a draw uses, from its index buffer (`indexed`: the `GX2IndexType` and address) or counted from `first`, with
/// fans and quads made into triangles; `None` when there are none.
pub(crate) fn index_list(
    g: &Guest,
    restart_index: Option<u32>,
    primitive: u32,
    expand: bool,
    count: u32,
    indexed: Option<(u32, u32)>,
    first: u32,
) -> Option<IndexList> {
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
                    .map(|i| if Some(i) == restart_index { u32::MAX } else { i })
                    .collect(),
            )
        }
        None => None,
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
            .map(|i| if i == u32::MAX { restart_index.unwrap_or(i) } else { i })
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
        return None;
    }
    // only the vertices the draw uses are decoded: the indices are made relative to the lowest one
    let real = || indices.iter().copied().filter(|&i| i != u32::MAX);
    let lowest = real().min().unwrap_or(0);
    let highest = real().max().unwrap_or(0);
    let indices: Vec<u32> = indices.iter().map(|&i| if i == u32::MAX { i } else { i - lowest }).collect();
    Some(IndexList {
        indices,
        lowest,
        vertices: highest - lowest + 1,
    })
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
    /// Counts the batches handed to the GPU (what was uploaded in an earlier one is not where it was any more).
    pub(crate) generation: u64,
    /// The slots of the big uniform blocks (see `slot_write`), how many this batch has used, and how far each was last written.
    slot_buffers: Vec<wgpu::Buffer>,
    slots_used: usize,
    slot_marks: Vec<u64>,
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

    fn floats(v: [u32; 4]) -> [f32; 4] {
        v.map(f32::from_bits)
    }

    #[test]
    fn float_attributes_read_the_big_endian_values_with_the_default_swap() {
        let mut b = Vec::new();
        for f in [1.0f32, 2.0, -3.0] {
            b.extend_from_slice(&f.to_be_bytes());
        }
        assert_eq!(floats(decode_attribute(0x811, 3, &b)), [1.0, 2.0, -3.0, 1.0]);
        // swap 8 in 32 says the same; no swap reads the bytes as they are
        assert_eq!(floats(decode_attribute(0x811, 2, &b)), [1.0, 2.0, -3.0, 1.0]);
        let le: Vec<u8> = [1.0f32, 2.0].iter().flat_map(|f| f.to_le_bytes()).collect();
        assert_eq!(floats(decode_attribute(0x80d, 0, &le)), [1.0, 2.0, 0.0, 1.0]);
        // half floats: 1.0 and -2.0
        assert_eq!(floats(decode_attribute(0x808, 3, &[0x3c, 0x00, 0xc0, 0x00])), [1.0, -2.0, 0.0, 1.0]);
    }

    #[test]
    fn scaled_attributes_become_floats_of_their_integers() {
        // sint 16 to float: -2, 300, 7, -32768
        let b: Vec<u8> = [-2i16, 300, 7, -32768].iter().flat_map(|v| v.to_be_bytes()).collect();
        assert_eq!(floats(decode_attribute(0xa0e, 3, &b)), [-2.0, 300.0, 7.0, -32768.0]);
        // uint 8 to float
        assert_eq!(floats(decode_attribute(0x80a, 3, &[1, 2, 3, 255])), [1.0, 2.0, 3.0, 255.0]);
        // uint 16 to float (the scaled flag on a 16-bit type is not a half float)
        assert_eq!(floats(decode_attribute(0x802, 3, &[0x01, 0x00])), [256.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn normalised_and_integer_attributes() {
        assert_eq!(floats(decode_attribute(0x00a, 3, &[255, 0, 51, 255])), [1.0, 0.0, 0.2, 1.0]);
        assert_eq!(floats(decode_attribute(0x207, 3, &[0x80, 0x00, 0x7f, 0xff])), [-1.0, 1.0, 0.0, 1.0]);
        assert_eq!(
            floats(decode_attribute(0x20a, 3, &[0x81, 0x7f, 0, 0xff])),
            [-1.0, 1.0, 0.0, -1.0 / 127.0]
        );
        // integers stay integers, and the missing fourth component is the integer one
        assert_eq!(decode_attribute(0x104, 3, &[7, 200]), [7, 200, 0, 1]);
        assert_eq!(decode_attribute(0x304, 3, &[0xff, 2]), [u32::MAX, 2, 0, 1]);
        assert_eq!(decode_attribute(0x107, 3, &[0x12, 0x34, 0, 1]), [0x1234, 1, 0, 1]);
    }

    #[test]
    fn packed_attributes_are_read_from_the_lowest_bits() {
        // 10-10-10-2 signed normalised: x = 511, y = -511, z = 0, w = 1
        let word: u32 = 511 | ((-511i32 as u32 & 0x3ff) << 10) | (1 << 30);
        let v = floats(decode_attribute(0x20b, 3, &word.to_be_bytes()));
        assert_eq!(v, [1.0, -1.0, 0.0, 1.0]);
        let word: u32 = 1023 | (3 << 30);
        assert_eq!(floats(decode_attribute(0x00b, 3, &word.to_be_bytes())), [1.0, 0.0, 0.0, 1.0]);
        // 11-11-10 float: 1.0 in each (exponent 15)
        let word: u32 = (15 << 6) | ((15 << 6) << 11) | ((15 << 5) << 22);
        assert_eq!(floats(decode_attribute(0x809, 3, &word.to_be_bytes())), [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn attribute_sizes_and_names() {
        assert_eq!(attribute_size(0x811), 12);
        assert_eq!(attribute_size(0xa0e), 8);
        assert_eq!(attribute_size(0x00b), 4);
        assert_eq!(attribute_size(0x001), 1);
        assert_eq!(attribute_format_name(0xa0e), "sint->float 16_16_16_16");
        assert_eq!(attribute_format_name(0x813), "float 32_32_32_32");
        assert_eq!(attribute_format_name(0x20a), "snorm 8_8_8_8");
    }

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

    /// What a uniform upload held before the uploads were made cheaper: the whole block (`vec4s` of them) with the guest's bytes in front
    /// (as they are: the GPU reads little-endian words).
    fn uniform_as_before(g: &Guest, block: &crate::state::UniformBlock, vec4s: u32) -> Vec<u8> {
        let size = vec4s.max(1) as usize * 16;
        let mut bytes = vec![0u8; size];
        let have = (block.size as usize).min(size);
        if block.address != 0 && have > 0 {
            g.read(block.address, &mut bytes[..have]);
        }
        bytes
    }

    #[test]
    fn the_depth_bias_is_on_unless_turned_off() {
        assert!(bias_setting(None));
        assert!(bias_setting(Some("on")));
        for off in ["off", " OFF ", "0", "no", "false"] {
            assert!(!bias_setting(Some(off)), "{off}");
        }
    }

    /// Uniform blocks go up as they are: the GPU reads little-endian words. The game's floats arrive turned around already (through the
    /// DMA engine's 8-in-32 swap), and a colour it packs as bytes (red first, alpha last) is read with red in the low byte, which is
    /// where its shaders take red from (`AND_INT 0xff`, then `>> 8`, `>> 16` for green and blue): the village's shadow colour
    /// `4c 4c 4c ff` is grey, not the red `ff 4c 4c` the words turned around gave.
    #[test]
    fn uniform_blocks_go_up_as_the_little_endian_words_the_gpu_reads() {
        let mut m = vec![0u8; 0x200];
        let g = Guest::new(m.as_mut_ptr());
        g.write(0x100, &0.5f32.to_le_bytes());
        g.write(0x104, &[0x4c, 0x4c, 0x4c, 0xff]);
        let mut out = Vec::new();
        uniform_words(&g, 0x100, 8, &mut out);
        assert_eq!(f32::from_le_bytes(out[0..4].try_into().unwrap()), 0.5);
        let colour = u32::from_le_bytes(out[4..8].try_into().unwrap());
        let (r, gr, b, a) = (colour & 0xff, (colour >> 8) & 0xff, (colour >> 16) & 0xff, colour >> 24);
        assert_eq!((r, gr, b, a), (0x4c, 0x4c, 0x4c, 0xff));
    }

    #[test]
    fn uniform_words_are_what_was_uploaded_before() {
        let mut m: Vec<u8> = (0..0x4000u32).map(|k| (k * 7 + 3) as u8).collect();
        let g = Guest::new(m.as_mut_ptr());
        let mut out = Vec::new();
        for (address, size, vec4s) in [
            (0x100, 64, 4),
            (0x100, 70, 8),
            (0x203, 13, 1),
            (0x100, 1000, 4),
            (0, 64, 4),
            (0x100, 0, 2),
            (0x40, 0x2000, 4096),
        ] {
            let block = crate::state::UniformBlock { address, size };
            let full = (vec4s as usize).max(1) * 16;
            let have = if address != 0 { (size as usize).min(full) } else { 0 };
            uniform_words(&g, address, have, &mut out);
            assert!(out.len() <= full && out.len().is_multiple_of(4));
            let mut padded = out.clone();
            padded.resize(full, 0);
            assert_eq!(
                padded,
                uniform_as_before(&g, &block, vec4s),
                "block at {address:#x}, {size} bytes, {vec4s} vec4"
            );
        }
    }

    #[test]
    fn a_shadow_map_drawn_as_colour_is_copied_into_depth_once_per_change() {
        let mut gx2 = Gx2::new(crate::Display::noop());
        // the copy's program and pipeline are valid (wgpu would panic on a validation error)
        let _ = depth_copy_pipeline(&gx2.d.device);
        let image = 0xf561_e800;
        let texture = gx2.d.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 16,
                height: 16,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        gx2.targets.insert(
            image,
            crate::gx2::Target {
                texture,
                format: wgpu::TextureFormat::R32Float,
                view,
            },
        );
        let _ = gx2.depth_copy_view(image);
        assert_eq!(gx2.caches.depth_copies[&image].2, u64::MAX);
        gx2.refresh_depth_copy(image);
        assert_eq!(gx2.caches.depth_copies[&image].2, 0);
        assert_eq!(gx2.caches.depth_copies[&image].0.format(), wgpu::TextureFormat::Depth32Float);
        // drawn into again: copied again
        *gx2.caches.target_version.entry(image).or_default() += 1;
        gx2.refresh_depth_copy(image);
        assert_eq!(gx2.caches.depth_copies[&image].2, 1);
        gx2.end_pass();
    }

    fn stream(location: u32, offset: u32, format: u32) -> AttribStream {
        AttribStream {
            location,
            buffer: 0,
            offset,
            format,
            per_instance: false,
            divisor: 0,
            select: [0, 1, 2, 3],
            endian: 3,
        }
    }

    /// Draw 234 of the run-6 village frame: the fetch shader has locations 0 to 5 (position, normal, UV, colour, ...), and
    /// `vs-169eee80` reads the position in R1, the UVs in R2 (`DOT4 R4.x <- R2.x, KC0[8].x`) and the colour in R3 (`INT_TO_FLT R0.x
    /// <- R3.x`): its semantic table is 0, 2, 3.
    #[test]
    fn attributes_go_to_the_registers_the_semantic_table_names() {
        let fetch = [
            stream(0, 0, 0x811),
            stream(1, 12, 0x30a),
            stream(2, 16, 0x80d),
            stream(3, 24, 0x10a),
            stream(4, 28, 0x30a),
            stream(5, 32, 0x80d),
        ];
        let mut m = vec![0u8; 0x1000];
        let g = Guest::new(m.as_mut_ptr());
        let vs = 0x100;
        g.put_u32(vs + 0x40, 3);
        for (i, id) in [0u32, 2, 3].iter().enumerate() {
            g.put_u32(vs + 0x44 + 4 * i as u32, *id);
        }
        let table = vertex_semantics(&g, vs);
        assert_eq!(table, Some(vec![0, 2, 3]));
        let got: Vec<(u32, u32)> = vertex_attributes(&table, &fetch).iter().map(|a| (a.stream, a.gpr)).collect();
        // position R1, UV (stream 2) R2, colour (stream 3) R3; the normal and the rest are not loaded
        assert_eq!(got, [(0, 1), (2, 2), (3, 3)]);
        // no table: location + 1, as before
        g.put_u32(vs + 0x40, 0);
        let got: Vec<u32> = vertex_attributes(&vertex_semantics(&g, vs), &fetch).iter().map(|a| a.gpr).collect();
        assert_eq!(got, [1, 2, 3, 4, 5, 6]);
    }

    /// The character's skinning shader (`vs-1697a580`, fetch shader `0x1d526d50`, run 7 draw 188) reads bone set 1 from R6
    /// (`INT_TO_FLT R0.w <- R6.x`, uint8x4 location 5), sets 2 to 4 from R7, R8, R9 (uint8x4 locations 7, 8, 9) and morph deltas from
    /// R10, R11 (sint16x4 locations 10, 11): location 6 (a signed 8-bit tangent) is not in R7. As `location + 1` R7 held the tangent,
    /// read as bone indices (negative ones too), and vertices weighted to those bones were pulled toward the world origin.
    #[test]
    fn the_skinned_characters_bone_sets_skip_the_tangent() {
        let order = [0u32, 1, 4, 5, 6, 7, 8, 9, 10, 11, 2, 3];
        let fetch: Vec<AttribStream> = order.iter().map(|&l| stream(l, 0, 0x10a)).collect();
        // the table the listing implies: R1..R11 <- 0, 1, 2, 3, 4, 5, 7, 8, 9, 10, 11
        let table = Some(vec![0, 1, 2, 3, 4, 5, 7, 8, 9, 10, 11]);
        let gpr_of = |location: u32| {
            vertex_attributes(&table, &fetch)
                .iter()
                .find(|a| fetch[a.stream as usize].location == location)
                .map(|a| a.gpr)
        };
        assert_eq!(gpr_of(5), Some(6));
        assert_eq!(gpr_of(7), Some(7));
        assert_eq!(gpr_of(9), Some(9));
        assert_eq!(gpr_of(11), Some(11));
        assert_eq!(gpr_of(6), None, "the tangent is not loaded");
    }

    #[test]
    fn uniform_blocks_are_readable_to_the_next_256_bytes() {
        assert_eq!(readable_bytes(0), 0);
        assert_eq!(readable_bytes(0x80), 0x100);
        assert_eq!(readable_bytes(0x100), 0x100);
        assert_eq!(readable_bytes(0x101), 0x200);
        assert_eq!(readable_bytes(0x1_0000), 0x1_0000);
        // a block given as 8 vec4s: constant 10 is still uploaded
        let mut gx2 = Gx2::new(crate::Display::noop());
        let mut m = vec![0u8; 0x2_0000];
        let g = Guest::new(m.as_mut_ptr());
        g.write(0x1000 + 16 * 10, &0.5f32.to_le_bytes());
        let block = crate::state::UniformBlock {
            address: 0x1000,
            size: 0x80,
        };
        let _ = gx2.uniform_buffer(&g, &block, 14);
        // all 14 vec4s the shader declares were read from the guest, not the 8 the size says
        assert!(gx2.caches.uniforms.contains_key(&(0x1000, 14, 14 * 16)));
        let words = uniform_as_before(
            &g,
            &crate::state::UniformBlock {
                address: 0x1000,
                size: readable_bytes(0x80),
            },
            14,
        );
        assert_eq!(f32::from_le_bytes(words[160..164].try_into().unwrap()), 0.5);
        gx2.end_pass();
    }

    /// The scenery's vertex colour: a `uint 8_8_8_8` attribute (format 0x10a) reaches the shader as integers, which its
    /// `INT_TO_FLT` and `x 1/255` turn into the colour (not normalised first, which would make it about 1/255 of itself).
    #[test]
    fn an_integer_colour_attribute_becomes_its_colour_through_int_to_flt() {
        let raw = decode_attribute(0x10a, 3, &[255, 128, 0, 51]);
        assert_eq!(raw, [255, 128, 0, 51]);
        let colour: Vec<f32> = raw.iter().map(|&v| (v as i32) as f32 * 0.003_921_569).collect();
        assert!((colour[0] - 1.0).abs() < 1e-6 && (colour[1] - 0.502).abs() < 1e-3 && colour[2] == 0.0 && (colour[3] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn a_float_shadow_map_keeps_its_precision_and_is_written_over() {
        // the village's shadow map: 1024x1024 colour buffer of format 0x80e (r32f)
        let format = crate::gx2::texture_format(0x80e);
        assert_eq!(format, wgpu::TextureFormat::R32Float);
        assert_eq!(crate::gx2::texture_format(0x80d), wgpu::TextureFormat::Rg32Float);
        assert_eq!(crate::gx2::texture_format(0x823), wgpu::TextureFormat::Rgba32Float);
        // its blending, left on by the draws before, does not apply; 8-bit and 16-bit float buffers still blend
        assert!(!blends(format));
        assert!(blends(wgpu::TextureFormat::Rgba8Unorm));
        assert!(blends(wgpu::TextureFormat::Rgba16Float));
    }

    #[test]
    fn shadow_samplers_use_the_games_comparison() {
        assert_eq!(compare_sampler_function(15), wgpu::CompareFunction::LessEqual);
        assert_eq!(compare_sampler_function(4), wgpu::CompareFunction::Greater);
        assert_eq!(compare_sampler_function(1), wgpu::CompareFunction::Less);
        assert_eq!(parse_addresses("0x17101480, 1724c040 ;zz,0X10"), [0x1710_1480, 0x1724_c040, 0x10]);
    }

    #[test]
    fn a_uniform_block_is_uploaded_again_only_when_its_bytes_change() {
        let mut gx2 = Gx2::new(crate::Display::noop());
        let mut m = vec![0u8; 0x2_0000];
        let g = Guest::new(m.as_mut_ptr());
        let small = crate::state::UniformBlock {
            address: 0x1000,
            size: 256,
        };
        let big = crate::state::UniformBlock {
            address: 0x8000,
            size: 0x900,
        };
        g.write(0x1000, &[1; 256]);
        g.write(0x8000, &[2; 0x900]);
        let a = gx2.uniform_buffer(&g, &small, 16);
        let a_big = gx2.uniform_buffer(&g, &big, mh3u_latte::translate::DYNAMIC_BANK);
        assert_eq!(a_big.size, SLOT_SIZE, "a bank read by index is bound whole");
        let same = |x: &BufRef, y: &BufRef| x.buffer == y.buffer && x.offset == y.offset && x.size == y.size;
        assert!(same(&gx2.uniform_buffer(&g, &small, 16), &a), "same bytes, same upload");
        assert!(same(&gx2.uniform_buffer(&g, &big, mh3u_latte::translate::DYNAMIC_BANK), &a_big));
        // the arena holds what was uploaded before
        let shadow = &gx2.batch.arenas[Role::Read as usize][0].shadow;
        assert_eq!(
            shadow[a.offset as usize..(a.offset + a.size) as usize],
            uniform_as_before(&g, &small, 16)
        );
        // changed bytes are a new upload; changed back, the first one is used again
        g.write(0x1010, &[9]);
        let b = gx2.uniform_buffer(&g, &small, 16);
        assert!(!same(&b, &a));
        g.write(0x1010, &[1]);
        assert!(same(&gx2.uniform_buffer(&g, &small, 16), &a));
        // another size of the same block is another upload
        assert!(!same(&gx2.uniform_buffer(&g, &small, 8), &a));
        // after the batch went to the GPU, the bytes are uploaded again
        gx2.flush_draws();
        let c = gx2.uniform_buffer(&g, &small, 16);
        let shadow = &gx2.batch.arenas[Role::Read as usize][0].shadow;
        assert_eq!(
            shadow[c.offset as usize..(c.offset + c.size) as usize],
            uniform_as_before(&g, &small, 16)
        );
        assert_eq!(gx2.batch.slots_used, 0);
    }

    #[test]
    fn an_index_list_made_before_is_the_same_list() {
        let mut gx2 = Gx2::new(crate::Display::noop());
        let mut m = vec![0u8; 0x1_0000];
        let g = Guest::new(m.as_mut_ptr());
        let indices: Vec<u8> = [5u16, 9, 7, 0xffff, 6, 8, 12].iter().flat_map(|i| i.to_be_bytes()).collect();
        g.write(0x100, &indices);
        for (primitive, restart) in [(4, None), (6, Some(0xffff)), (5, None), (0x13, None)] {
            let list = |gx2: &mut Gx2| {
                let d = gx2.draw_indices(&g, restart, primitive, 7, Some((4, 0x100)), 2).expect("drawn");
                (d.count, d.lowest, d.vertices, d.buffer.size)
            };
            let made = index_list(
                &g,
                restart,
                primitive,
                topology_of(primitive).expect("drawn").1,
                7,
                Some((4, 0x100)),
                2,
            )
            .expect("drawn");
            let expected = (made.indices.len() as u32, made.lowest, made.vertices, made.indices.len() as u64 * 4);
            // the first time (into the arena), the second (its own buffer) and after
            for _ in 0..3 {
                assert_eq!(list(&mut gx2), expected, "primitive {primitive:#x}");
            }
        }
        // other bytes are another list
        let before = gx2.draw_indices(&g, None, 4, 7, Some((4, 0x100)), 0).expect("drawn");
        g.write(0x100, &[0, 1]);
        let after = gx2.draw_indices(&g, None, 4, 7, Some((4, 0x100)), 0).expect("drawn");
        assert_eq!((before.lowest, after.lowest), (5, 1));
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
