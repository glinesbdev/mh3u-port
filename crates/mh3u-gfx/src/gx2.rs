//! The game's `GX2` calls, over `wgpu`. For now: colour buffers as textures, clears, and showing the TV picture in the window with the
//! console's frame pacing. Draws, shaders and textures are the next part.

use crate::Display;
use crate::state::{State, arg, farg};
use mh3u_rt::{Ctx, Guest, Gx2Backend};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The calls this backend handles, as a compact number: recorded display lists keep this, and calls are dispatched by it, not by name.
macro_rules! calls {
    ($($variant:ident = $name:literal,)*) => {
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub(crate) enum Call {
            $($variant,)*
        }

        impl Call {
                        pub(crate) const ALL: &'static [Call] = &[$(Call::$variant,)*];

            pub(crate) fn name(self) -> &'static str {
                match self {
                    $(Call::$variant => $name,)*
                }
            }

            pub(crate) fn from_name(name: &str) -> Option<Call> {
                match name {
                    $($name => Some(Call::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

calls! {
    SetupContextStateEx = "gx2:GX2SetupContextStateEx",
    SetContextState = "gx2:GX2SetContextState",
    BeginDisplayListEx = "gx2:GX2BeginDisplayListEx",
    EndDisplayList = "gx2:GX2EndDisplayList",
    DirectCallDisplayList = "gx2:GX2DirectCallDisplayList",
    SetVertexShader = "gx2:GX2SetVertexShader",
    SetPixelShader = "gx2:GX2SetPixelShader",
    SetPrimitiveRestartIndex = "gx2:GX2SetPrimitiveRestartIndex",
    SetGeometryShader = "gx2:GX2SetGeometryShader",
    SetFetchShader = "gx2:GX2SetFetchShader",
    SetAttribBuffer = "gx2:GX2SetAttribBuffer",
    SetVertexUniformBlock = "gx2:GX2SetVertexUniformBlock",
    SetPixelUniformBlock = "gx2:GX2SetPixelUniformBlock",
    SetGeometryUniformBlock = "gx2:GX2SetGeometryUniformBlock",
    SetPixelTexture = "gx2:GX2SetPixelTexture",
    SetPixelSampler = "gx2:GX2SetPixelSampler",
    SetColorBuffer = "gx2:GX2SetColorBuffer",
    SetDepthBuffer = "gx2:GX2SetDepthBuffer",
    SetViewport = "gx2:GX2SetViewport",
    SetScissor = "gx2:GX2SetScissor",
    SetBlendControl = "gx2:GX2SetBlendControl",
    SetColorControl = "gx2:GX2SetColorControl",
    SetTargetChannelMasks = "gx2:GX2SetTargetChannelMasks",
    SetDepthOnlyControl = "gx2:GX2SetDepthOnlyControl",
    SetDepthStencilControl = "gx2:GX2SetDepthStencilControl",
    SetCullOnlyControl = "gx2:GX2SetCullOnlyControl",
    SetPolygonControl = "gx2:GX2SetPolygonControl",
    SetAlphaTest = "gx2:GX2SetAlphaTest",
    SetStencilMask = "gx2:GX2SetStencilMask",
    SetPolygonOffset = "gx2:GX2SetPolygonOffset",
    SetRasterizerClipControlEx = "gx2:GX2SetRasterizerClipControlEx",
    SetShaderModeEx = "gx2:GX2SetShaderModeEx",
    CalcFetchShaderSizeEx = "gx2:GX2CalcFetchShaderSizeEx",
    InitFetchShaderEx = "gx2:GX2InitFetchShaderEx",
    ClearDepthStencilEx = "gx2:GX2ClearDepthStencilEx",
    Invalidate = "gx2:GX2Invalidate",
    InitSampler = "gx2:GX2InitSampler",
    InitSamplerClamping = "gx2:GX2InitSamplerClamping",
    InitSamplerXYFilter = "gx2:GX2InitSamplerXYFilter",
    InitSamplerZMFilter = "gx2:GX2InitSamplerZMFilter",
    InitSamplerLOD = "gx2:GX2InitSamplerLOD",
    InitSamplerDepthCompare = "gx2:GX2InitSamplerDepthCompare",
    CopySurface = "gx2:GX2CopySurface",
    DrawEx = "gx2:GX2DrawEx",
    DrawIndexedEx = "gx2:GX2DrawIndexedEx",
    ClearColor = "gx2:GX2ClearColor",
    CopyColorBufferToScanBuffer = "gx2:GX2CopyColorBufferToScanBuffer",
    SwapScanBuffers = "gx2:GX2SwapScanBuffers",
    WaitForVsync = "gx2:GX2WaitForVsync",
    GetSwapStatus = "gx2:GX2GetSwapStatus",
    SetSwapInterval = "gx2:GX2SetSwapInterval",
    GetSwapInterval = "gx2:GX2GetSwapInterval",
}

const FRAME: Duration = Duration::from_nanos(16_683_333);

/// The console's TV scan target and its GamePad one.
const TV: u32 = 1;

pub(crate) struct Target {
    pub(crate) texture: wgpu::Texture,
    pub(crate) format: wgpu::TextureFormat,
    /// The whole texture as one view, made with it (draws into it and samples of it use this).
    pub(crate) view: wgpu::TextureView,
}

pub struct Gx2 {
    /// The mod overlay, drawn over the picture (`use_overlay`).
    overlay: Option<crate::overlay::Overlay>,
    pub(crate) batch: crate::draw::Batch,
    pub(crate) d: Display,
    pub(crate) config: wgpu::SurfaceConfiguration,
    blit: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// The colour buffers the game has used, by the guest address of their image.
    pub(crate) targets: HashMap<u32, Target>,
    /// What the TV shows: a copy of the colour buffer given to `GX2CopyColorBufferToScanBuffer`.
    tv: Option<wgpu::Texture>,
    pub(crate) swaps: u32,
    interval: u32,
    /// The state of each context the game has made, by the guest address of its `GX2ContextState`, and the one in use.
    pub(crate) contexts: HashMap<u32, State>,
    pub(crate) current: u32,
    /// Display lists: recorded calls by the address of their buffer, and the list being recorded.
    lists: HashMap<u32, std::sync::Arc<[Recorded]>>,
    recording: Option<Recording>,
    pub(crate) draws: u64,
    dumped: std::collections::HashSet<u32>,
    null_draws: u64,
    real_draws: u64,
    pub(crate) log: bool,
    next_vsync: Instant,
    pub(crate) caches: crate::draw::DrawCaches,
    pub(crate) last_tv_source: u32,
    save_every: u32,
    /// The fetch shaders by address: shared slices, one for each distinct description (`fetch_interned`), so the address of the slice
    /// names the contents in the shader caches' keys. The game makes some fetch shaders again every frame; with a new slice each time,
    /// every draw with them missed the caches and left an entry behind (run 11: the field slowing from 56 to 31 FPS).
    pub(crate) fetch_shaders: HashMap<u32, std::sync::Arc<[crate::draw::AttribStream]>>,
    /// Every distinct fetch shader description made, by a hash of it.
    pub(crate) fetch_interned: HashMap<u64, Vec<std::sync::Arc<[crate::draw::AttribStream]>>>,
    /// With MH3U_FPS set: when the frame rate was last printed, and the swap count then.
    fps: Option<(Instant, u32)>,
    /// MH3U_GFX_REPORT and MH3U_GFX_DUMP_FRAME.
    pub(crate) diag: crate::report::Diagnostics,
    /// The render pipelines, their workers and the disk cache (`pipelines.rs`).
    pub(crate) pipes: crate::pipelines::Pipelines,
    /// Frame times, cache sizes and area changes for the log (`perf.rs`).
    pub(crate) perf: crate::perf::Perf,
    /// With timing on: when the graphics call running now started (the swap moves it to its own end of the frame, see `swap_point`).
    call_started: Option<Instant>,
    /// With MH3U_GFX_TIMES: each non-draw call's time and count in this frame, by `Call` (for `[gfx slow frame]`; an array, not the
    /// shared step table, as there are thousands of these calls a frame).
    call_frame: Vec<(std::time::Duration, u64)>,
    /// How long the last call took, for the runtime's `[gfx times]` (see `Gx2Backend::times_calls`).
    last_call: std::time::Duration,
}

/// The texture format of a `GX2SurfaceFormat` of a colour buffer.
pub(crate) fn texture_format(gx2: u32) -> wgpu::TextureFormat {
    match gx2 {
        0x41a => wgpu::TextureFormat::Rgba8UnormSrgb,
        0x19 => wgpu::TextureFormat::Rgb10a2Unorm,
        0x816 | 0x810 => wgpu::TextureFormat::Rgba16Float,
        0x1 => wgpu::TextureFormat::R8Unorm,
        0x7 => wgpu::TextureFormat::Rg8Unorm,
        // the 32-bit float formats keep their precision: the village's shadow map is z/w in an r32f colour buffer, and in 8 bits
        // its steps (1/255) outgrow the game's shadow bias (about 0.0115) and the lit side of a figure shadows itself in bands
        0x80e => wgpu::TextureFormat::R32Float,
        0x80d => wgpu::TextureFormat::Rg32Float,
        0x823 => wgpu::TextureFormat::Rgba32Float,
        _ => wgpu::TextureFormat::Rgba8Unorm,
    }
}

const BLIT: &str = "
@group(0) @binding(0) var picture: texture_2d<f32>;
@group(0) @binding(1) var picture_sampler: sampler;
struct Out { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: Out;
    o.position = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(p.x, 1.0 - p.y);
    return o;
}
@fragment fn fs(in: Out) -> @location(0) vec4<f32> { return textureSample(picture, picture_sampler, in.uv); }
";

impl Gx2 {
    /// Draws the mod overlay (unless `MH3U_OVERLAY=off`), with the game's data from the dump's base folder, the wishlist in `dir`, and
    /// the hunters' names from the game's save folder (to find the live save by).
    pub fn use_overlay(&mut self, dump_base: std::path::PathBuf, dir: std::path::PathBuf, save_dir: std::path::PathBuf) {
        if crate::overlay::enabled() && self.d.window.is_some() {
            self.overlay = Some(crate::overlay::Overlay::new(dump_base, dir, save_dir));
        }
    }

    pub fn new(d: Display) -> Gx2 {
        eprintln!("[gfx] build {}", crate::BUILD);
        let pipes = crate::pipelines::Pipelines::new(d.device.clone());
        let size = d
            .window
            .as_ref()
            .map_or(winit::dpi::PhysicalSize::new(1280, 720), |w| w.inner_size());
        let caps = d.surface.as_ref().map(|s| s.get_capabilities(&d.adapter));
        // the game's colours are already in the display's encoding, so a surface that does not encode them again
        let format = caps.as_ref().map_or(wgpu::TextureFormat::Bgra8Unorm, |caps| {
            caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0])
        });
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.as_ref().map_or(wgpu::CompositeAlphaMode::Auto, |caps| caps.alpha_modes[0]),
            view_formats: vec![],
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        if let Some(surface) = &d.surface {
            surface.configure(&d.device, &config);
        }
        let module = d.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT.into()),
        });
        let layout = d.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = d.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let blit = d.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(format.into())],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = d.device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Gx2 {
            overlay: None,
            batch: Default::default(),
            d,
            config,
            blit,
            layout,
            sampler,
            targets: HashMap::new(),
            tv: None,
            swaps: 0,
            interval: 1,
            contexts: HashMap::new(),
            current: 0,
            lists: HashMap::new(),
            recording: None,
            draws: 0,
            dumped: Default::default(),
            null_draws: 0,
            real_draws: 0,
            log: std::env::var_os("MH3U_GFX_LOG").is_some(),
            next_vsync: Instant::now(),
            caches: crate::draw::DrawCaches {
                // the tests translate as much as they ask for in a frame
                budget: if cfg!(test) {
                    None
                } else {
                    crate::draw::shader_budget(std::env::var("MH3U_GFX_SHADER_BUDGET_MS").ok().as_deref())
                },
                // the tests decode on the render thread unless they turn the workers on
                async_decode: !cfg!(test) && crate::decode::async_decode(std::env::var("MH3U_GFX_ASYNC_DECODE").ok().as_deref()),
                decode_budget: crate::decode::decode_budget(std::env::var("MH3U_GFX_DECODE_BUDGET_MS").ok().as_deref()),
                ..Default::default()
            },
            last_tv_source: 0,
            save_every: std::env::var("MH3U_GFX_SAVE_EVERY")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(120),
            fetch_shaders: HashMap::new(),
            fetch_interned: HashMap::new(),
            fps: std::env::var_os("MH3U_FPS").map(|_| (Instant::now(), 0)),
            diag: crate::report::Diagnostics::from_env(),
            pipes,
            perf: crate::perf::Perf::from_env(),
            call_started: None,
            last_call: std::time::Duration::ZERO,
            call_frame: vec![Default::default(); Call::ALL.len()],
        }
    }

    /// The texture of the colour buffer at `cb` (a `GX2ColorBuffer`, which begins with its `GX2Surface`), made when first seen.
    pub(crate) fn target(&mut self, g: &Guest, cb: u32) -> Option<u32> {
        let (width, height, format, image) = (g.u32(cb + 4), g.u32(cb + 8), g.u32(cb + 0x14), g.u32(cb + 0x24));
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return None;
        }
        let format = texture_format(format);
        let stale = self
            .targets
            .get(&image)
            .is_none_or(|t| t.texture.width() != width || t.texture.height() != height || t.format != format);
        if stale {
            let texture = self.d.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("colour buffer"),
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
            self.targets.insert(image, Target { texture, format, view });
        }
        Some(image)
    }

    /// `GX2CopySurface`: the contents of one surface into another (both are known to this backend as render targets or not at all).
    fn copy_surface(&mut self, g: &Guest, source: u32, destination: u32) {
        self.flush_draws();
        if self.log && self.swaps < 400 {
            eprintln!(
                "[gfx] copy surface {source:#x} (image {:#x}, {}x{}, format {:#x}) to {destination:#x} (image {:#x}, {}x{}, format {:#x})",
                g.u32(source + 0x24),
                g.u32(source + 4),
                g.u32(source + 8),
                g.u32(source + 0x14),
                g.u32(destination + 0x24),
                g.u32(destination + 4),
                g.u32(destination + 8),
                g.u32(destination + 0x14)
            );
        }
        let (Some(from), Some(to)) = (self.target(g, source), self.target(g, destination)) else {
            return;
        };
        if from == to {
            return;
        }
        let (a, b) = (&self.targets[&from].texture, &self.targets[&to].texture);
        if a.width() != b.width() || a.height() != b.height() || a.format() != b.format() {
            self.caches_report(format!(
                "a copy between surfaces of different sizes or formats ({}x{} {:?} to {}x{} {:?})",
                a.width(),
                a.height(),
                a.format(),
                b.width(),
                b.height(),
                b.format()
            ));
            return;
        }
        *self.caches.target_version.entry(to).or_default() += 1;
        let (a, b) = (&self.targets[&from].texture, &self.targets[&to].texture);
        let mut enc = self.d.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_texture(
            a.as_image_copy(),
            b.as_image_copy(),
            wgpu::Extent3d {
                width: a.width(),
                height: a.height(),
                depth_or_array_layers: 1,
            },
        );
        self.d.queue.submit([enc.finish()]);
    }

    fn clear(&mut self, g: &Guest, c: &Ctx) {
        self.flush_draws();
        let Some(image) = self.target(g, c.r[3]) else { return };
        *self.caches.target_version.entry(image).or_default() += 1;
        let f = |n: usize| f64::from(f64::from_bits(c.f[n]) as f32);
        let view = self.targets[&image].view.clone();
        let mut enc = self.d.device.create_command_encoder(&Default::default());
        {
            let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f(1),
                            g: f(2),
                            b: f(3),
                            a: f(4),
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        self.d.queue.submit([enc.finish()]);
    }

    fn copy_to_scan_buffer(&mut self, g: &Guest, c: &Ctx) {
        self.flush_draws();
        if c.r[4] != TV {
            return;
        }
        let Some(image) = self.target(g, c.r[3]) else { return };
        self.last_tv_source = image;
        let source = &self.targets[&image].texture;
        let (w, h) = (source.width(), source.height());
        let keep = self
            .tv
            .as_ref()
            .is_some_and(|t| t.width() == w && t.height() == h && t.format() == source.format());
        if !keep {
            self.tv = Some(self.d.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("tv"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: source.format(),
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            }));
        }
        let tv = self.tv.as_ref().expect("made above");
        let mut enc = self.d.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_texture(
            source.as_image_copy(),
            tv.as_image_copy(),
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.d.queue.submit([enc.finish()]);
    }

    /// Writes the TV picture as a PPM file (for looking at what the game drew without the window): MH3U_GFX_SAVE=<folder> saves one
    /// every 120 swaps.
    fn save_tv(&self, folder: &std::path::Path) {
        let Some((w, h, rgba)) = self.read_tv() else { return };
        let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
        for px in rgba.as_chunks::<4>().0 {
            ppm.extend_from_slice(&px[..3]);
        }
        let _ = std::fs::create_dir_all(folder);
        let _ = std::fs::write(folder.join(format!("tv-{:06}.ppm", self.swaps)), ppm);
    }

    /// The TV picture (or the colour buffer MH3U_GFX_SAVE_IMAGE names) as 8-bit RGBA, read back from the GPU.
    fn read_tv(&self) -> Option<(u32, u32, Vec<u8>)> {
        // MH3U_GFX_SAVE_IMAGE=<hex address of a colour buffer's image> saves that buffer instead of the TV picture
        let wanted = std::env::var("MH3U_GFX_SAVE_IMAGE")
            .ok()
            .and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok());
        let tv = match wanted {
            Some(image) => self.targets.get(&image).map(|t| &t.texture),
            None => self.tv.as_ref(),
        };
        let tv = tv?;
        if tv.format() != wgpu::TextureFormat::Rgba8Unorm {
            return None;
        }
        let (w, h) = (tv.width(), tv.height());
        let row = (w * 4).div_ceil(256) * 256;
        let buffer = self.d.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(row * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.d.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            tv.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.d.queue.submit([enc.finish()]);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.d.device.poll(wgpu::PollType::wait_indefinitely());
        let data = slice.get_mapped_range().ok()?;
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            let at = (y * row) as usize;
            rgba.extend_from_slice(&data[at..at + (w * 4) as usize]);
        }
        Some((w, h, rgba))
    }

    /// Shows the TV picture, scaled to the window with the picture's proportions kept.
    fn present(&mut self) {
        self.flush_draws();
        let (Some(window), Some(surface)) = (&self.d.window, &self.d.surface) else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        if size.width != self.config.width || size.height != self.config.height {
            self.config.width = size.width;
            self.config.height = size.height;
            surface.configure(&self.d.device, &self.config);
        }
        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                surface.configure(&self.d.device, &self.config);
                return;
            }
            _ => return,
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = self.d.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(tv) = &self.tv {
                let (tw, th) = (tv.width() as f32, tv.height() as f32);
                let scale = (size.width as f32 / tw).min(size.height as f32 / th);
                let (w, h) = (tw * scale, th * scale);
                pass.set_viewport((size.width as f32 - w) / 2.0, (size.height as f32 - h) / 2.0, w, h, 0.0, 1.0);
                let group = self.d.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&tv.create_view(&Default::default())),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                    ],
                });
                pass.set_pipeline(&self.blit);
                pass.set_bind_group(0, &group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        if let Some(o) = self.overlay.as_mut() {
            o.frame(
                &self.d.device,
                &self.d.queue,
                &mut enc,
                &view,
                self.config.format,
                (size.width, size.height),
            );
        }
        self.d.queue.submit([enc.finish()]);
        self.d.queue.present(frame);
    }

    /// Waits for the next vertical blank: the console's display refreshes 60 times a second and a swap takes effect at one.
    fn wait_for_vsync(&mut self) {
        let now = Instant::now();
        if self.next_vsync > now {
            std::thread::sleep(self.next_vsync - now);
        }
        // a game that fell behind does not owe the frames it missed
        self.next_vsync = self.next_vsync.max(now) + FRAME;
    }
}

/// A call kept in a display list: which call, the argument registers (r3..r10, f1..f8) and the stack arguments.
/// Fixed-size, so recording and replaying a list allocates nothing per call.
#[derive(Clone, Copy)]
struct Recorded {
    call: Call,
    r: [u32; 8],
    f: [u64; 8],
    stack: [u32; 8],
}

impl Recorded {
    fn new(call: Call, c: &Ctx, g: &Guest) -> Recorded {
        let mut r = [0; 8];
        r.copy_from_slice(&c.r[3..11]);
        let mut f = [0; 8];
        f.copy_from_slice(&c.f[1..9]);
        Recorded {
            call,
            r,
            f,
            stack: std::array::from_fn(|k| g.u32(c.r[1].wrapping_add(8 + 4 * k as u32))),
        }
    }

    /// Puts the recorded arguments back where the call reads them (only these registers are read by `exec`).
    fn load(&self, c: &mut Ctx) {
        c.r[3..11].copy_from_slice(&self.r);
        c.f[1..9].copy_from_slice(&self.f);
    }
}

/// A display list being recorded.
struct Recording {
    address: u32,
    calls: Vec<Recorded>,
    /// The calls made into it, the ones left out as redundant too (the size the game is told counts them, as before).
    made: u32,
    seen: Seen,
}

/// The texture and sampler each pixel unit was last set to in the list being recorded, since its last change of context: setting a unit
/// to what it already is changes nothing, so such a call is not recorded (the game sets every unit for every draw).
#[derive(Default)]
struct Seen {
    textures: [Option<u32>; 18],
    samplers: [Option<u32>; 18],
}

impl Seen {
    /// Whether the call (with r3, r4) would change nothing when played back right after the calls recorded so far.
    fn redundant(&mut self, call: Call, r3: u32, r4: u32) -> bool {
        let slots = match call {
            // the state the units are in is another context's (or a fresh one) from here on
            Call::SetupContextStateEx | Call::SetContextState => {
                *self = Seen::default();
                return false;
            }
            Call::SetPixelTexture => &mut self.textures,
            Call::SetPixelSampler => &mut self.samplers,
            _ => return false,
        };
        let Some(slot) = slots.get_mut(r4 as usize) else { return false };
        if *slot == Some(r3) {
            return true;
        }
        *slot = Some(r3);
        false
    }
}

/// The calls that are never part of a display list: the ones about the display, not the GPU's command stream, and the ones that only fill
/// in an object in the CPU's memory (`GX2Calc*`, `GX2Init*`). The console runs those at once even while a list is being made; recorded,
/// they would run only when the list is played, reading their inputs (a fetch shader's attribute array, often on the caller's stack) long
/// after the game has reused that memory, and giving no result (a size) to the caller now.
fn is_immediate(call: Call) -> bool {
    matches!(
        call,
        Call::BeginDisplayListEx
            | Call::EndDisplayList
            | Call::DirectCallDisplayList
            | Call::SwapScanBuffers
            | Call::WaitForVsync
            | Call::GetSwapStatus
            | Call::SetSwapInterval
            | Call::GetSwapInterval
            | Call::CalcFetchShaderSizeEx
            | Call::InitFetchShaderEx
            | Call::InitSampler
            | Call::InitSamplerClamping
            | Call::InitSamplerXYFilter
            | Call::InitSamplerZMFilter
            | Call::InitSamplerLOD
            | Call::InitSamplerDepthCompare
    )
}

/// `GX2InitFetchShaderEx(fetchShader, buffer, count, attribs, ...)`: no machine code is made; the attribute streams are read now (the array
/// is the caller's and may be gone after the call) and kept for the draws. Only the `GX2FetchShader` itself (0x20 bytes) is written.
pub(crate) fn init_fetch_shader(
    g: &Guest,
    fs: u32,
    buffer: u32,
    count: u32,
    attribs: u32,
) -> (u32, std::sync::Arc<[crate::draw::AttribStream]>) {
    let streams: Vec<_> = (0..count.min(32))
        .map(|i| crate::draw::AttribStream::read(g, attribs + 32 * i))
        .collect();
    g.fill(fs, 0, 0x20);
    g.put_u32(fs + 8, (32 + 16 * count).div_ceil(16) * 16);
    g.put_u32(fs + 0xc, buffer);
    g.put_u32(fs + 0x10, count);
    (fs, streams.into())
}

impl Gx2 {
    /// Keeps the pipelines made under `root` (`<out>/cache` for a run; MH3U_GFX_CACHE_DIR puts it elsewhere, MH3U_GFX_PIPELINE_CACHE=off
    /// turns it off), and starts making again, on the workers, those a run before made (see `pipelines.rs`).
    pub fn use_cache_dir(&mut self, root: &std::path::Path) {
        let off =
            |name: &str| std::env::var(name).is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "off" | "no" | "false"));
        let root = std::env::var_os("MH3U_GFX_CACHE_DIR").map_or_else(|| root.to_path_buf(), std::path::PathBuf::from);
        if off("MH3U_GFX_SHADER_CACHE") {
            eprintln!("[gfx] shader cache off (MH3U_GFX_SHADER_CACHE)");
        } else {
            match self.caches.translations.open(&root) {
                Ok((dir, n)) => eprintln!(
                    "[gfx] shader cache {}: {n} translations kept; at most {} of translating a frame (MH3U_GFX_SHADER_BUDGET_MS)",
                    dir.display(),
                    self.caches
                        .budget
                        .map_or("no limit".to_string(), |b| format!("{} ms", b.as_millis())),
                ),
                Err(e) => eprintln!("[gfx] shader cache under {} not opened: {e}", root.display()),
            }
        }
        if off("MH3U_GFX_PIPELINE_CACHE") {
            eprintln!("[gfx] pipeline cache off (MH3U_GFX_PIPELINE_CACHE)");
            return;
        }
        let info = self.d.adapter.get_info();
        match self.pipes.open_disk(&root, &info) {
            Ok((dir, n)) => eprintln!(
                "[gfx] pipeline cache {}: {n} pipelines to make again; pipelines made {}",
                dir.display(),
                if self.pipes.asynchronous {
                    "on worker threads (MH3U_GFX_ASYNC_PIPELINES=off to wait for them)"
                } else {
                    "with the draw waiting (MH3U_GFX_ASYNC_PIPELINES is off)"
                }
            ),
            Err(e) => eprintln!("[gfx] pipeline cache under {} not opened: {e}", root.display()),
        }
    }

    /// The shared slice for a fetch shader description: the one made before for the same attributes, or this one.
    pub(crate) fn intern_fetch(
        &mut self,
        streams: std::sync::Arc<[crate::draw::AttribStream]>,
    ) -> std::sync::Arc<[crate::draw::AttribStream]> {
        let same = self.fetch_interned.entry(crate::draw::hash_of(&streams)).or_default();
        if let Some(s) = same.iter().find(|s| s[..] == streams[..]) {
            crate::draw::Lap::count("fetch shaders made again", 1);
            return s.clone();
        }
        same.push(streams.clone());
        streams
    }

    fn state(&mut self) -> &mut State {
        self.contexts.entry(self.current).or_default()
    }

    /// Runs one call (or plays back a recorded one: `replay` is its stack).
    fn exec(&mut self, call: Call, c: &mut Ctx, g: &Guest, replay: Option<&[u32]>) {
        if false {
            eprintln!("call {}{}", call.name(), if replay.is_some() { " (replayed)" } else { "" });
        }
        let a = |n: usize| arg(c, g, replay, n);
        match call {
            Call::SetupContextStateEx => {
                self.contexts.insert(c.r[3], State::default());
                self.current = c.r[3];
            }
            Call::SetContextState => self.current = c.r[3],
            Call::SetVertexShader => {
                let v = a(0);
                if false {
                    eprintln!("  vertex shader {v:#x}");
                }
                self.state().vertex_shader = v;
            }
            Call::SetPrimitiveRestartIndex => {
                let v = a(0);
                self.state().restart_index = Some(v);
            }
            Call::SetPixelShader => {
                let v = a(0);
                if false {
                    eprintln!("  pixel shader {v:#x}");
                }
                self.state().pixel_shader = v;
            }
            Call::SetGeometryShader => {
                let v = a(0);
                self.state().geometry_shader = v;
            }
            Call::SetFetchShader => {
                let v = a(0);
                self.state().fetch_shader = v;
            }
            Call::SetAttribBuffer => {
                let (i, size, stride, address) = (a(0), a(1), a(2), a(3));
                if let Some(b) = self.state().attrib_buffers.get_mut(i as usize) {
                    *b = crate::state::AttribBuffer { address, size, stride };
                }
            }
            Call::SetVertexUniformBlock | Call::SetPixelUniformBlock | Call::SetGeometryUniformBlock => {
                let (i, size, address) = (a(0), a(1), a(2));
                let st = self.state();
                let blocks = if call == Call::SetVertexUniformBlock {
                    &mut st.vertex_blocks
                } else if call == Call::SetGeometryUniformBlock {
                    &mut st.geometry_blocks
                } else {
                    &mut st.pixel_blocks
                };
                if let Some(b) = blocks.get_mut(i as usize) {
                    *b = crate::state::UniformBlock { address, size };
                }
            }
            Call::SetPixelTexture => {
                let (t, unit) = (a(0), a(1));
                if let Some(slot) = self.state().pixel_textures.get_mut(unit as usize) {
                    *slot = t;
                }
            }
            Call::SetPixelSampler => {
                let (s, unit) = (a(0), a(1));
                if let Some(slot) = self.state().pixel_samplers.get_mut(unit as usize) {
                    *slot = s;
                }
            }
            Call::SetColorBuffer => {
                let (cb, target) = (a(0), a(1));
                if let Some(slot) = self.state().colour_buffers.get_mut(target as usize) {
                    *slot = cb;
                }
            }
            Call::SetDepthBuffer => {
                let v = a(0);
                self.state().depth_buffer = v;
            }
            Call::SetViewport => {
                let v = [farg(c, 0), farg(c, 1), farg(c, 2), farg(c, 3), farg(c, 4), farg(c, 5)];
                self.state().viewport = v;
            }
            Call::SetScissor => {
                let v = [a(0), a(1), a(2), a(3)];
                self.state().scissor = v;
            }
            Call::SetBlendControl => {
                let t = a(0) as usize;
                let b = crate::state::Blend {
                    colour_src: a(1),
                    colour_dst: a(2),
                    colour_op: a(3),
                    separate_alpha: a(4) != 0,
                    alpha_src: a(5),
                    alpha_dst: a(6),
                    alpha_op: a(7),
                };
                if let Some(slot) = self.state().blend.get_mut(t) {
                    *slot = b;
                }
            }
            Call::SetColorControl => {
                let v = [a(0), a(1), a(2), a(3)];
                self.state().colour_control = v;
            }
            Call::SetTargetChannelMasks => {
                let mut m = 0;
                for k in 0..8 {
                    m |= (a(k) & 0xf) << (4 * k);
                }
                self.state().target_masks = m;
            }
            Call::SetDepthOnlyControl => {
                let (t, w, f) = (a(0) != 0, a(1) != 0, a(2));
                let st = self.state();
                (st.depth_test, st.depth_write, st.depth_func, st.stencil_test) = (t, w, f, false);
            }
            Call::SetDepthStencilControl => {
                let (t, w, f, s) = (a(0) != 0, a(1) != 0, a(2), a(3) != 0);
                // front: comparison, pass, depth fail, fail; then the same for back faces (the arguments' order)
                let (back_enabled, front, back) = (a(4) != 0, [a(5), a(8), a(7), a(6)], [a(9), a(12), a(11), a(10)]);
                let st = self.state();
                (st.depth_test, st.depth_write, st.depth_func, st.stencil_test) = (t, w, f, s);
                (st.stencil.back_enabled, st.stencil.front, st.stencil.back) = (back_enabled, front, back);
            }
            Call::SetStencilMask => {
                let masks = std::array::from_fn(|k| a(k) & 0xff);
                self.state().stencil.masks = Some(masks);
            }
            Call::SetPolygonOffset => {
                let offset = std::array::from_fn(|k| farg(c, k));
                self.state().polygon_offset = offset;
            }
            Call::SetCullOnlyControl => {
                // GX2 writes the whole of PA_SU_SC_MODE_CNTL here, as GX2SetPolygonControl with no polygon modes and no polygon
                // offset: a draw after it has no depth bias until GX2SetPolygonControl turns it on again
                let (face, front, back) = (a(0), a(1) != 0, a(2) != 0);
                let st = self.state();
                (st.front_face, st.cull_front, st.cull_back) = (face, front, back);
                (st.offset_front, st.offset_back) = (false, false);
            }
            Call::SetPolygonControl => {
                let (face, front, back) = (a(0), a(1) != 0, a(2) != 0);
                let (offset_front, offset_back) = (a(6) != 0, a(7) != 0);
                let st = self.state();
                (st.front_face, st.cull_front, st.cull_back) = (face, front, back);
                (st.offset_front, st.offset_back) = (offset_front, offset_back);
            }
            Call::SetAlphaTest => {
                let (e, f, r) = (a(0) != 0, a(1), farg(c, 0));
                self.state().alpha_test = (e, f, r);
            }
            Call::SetShaderModeEx => {
                let mode = a(0);
                self.state().shader_mode = mode;
            }
            Call::SetRasterizerClipControlEx => {
                let half = a(2) != 0;
                self.state().half_z = half;
            }
            // the size of the code the fetch shader would be: sixteen bytes for each attribute and a little control flow
            Call::CalcFetchShaderSizeEx => c.r[3] = (32 + 16 * c.r[3]).div_ceil(16) * 16,
            // (fetch shader, buffer, attribute count, attributes, type, tessellation): nothing of machine code is made, the attributes
            // are kept and the draw reads them
            Call::InitFetchShaderEx => {
                let (fs, streams) = init_fetch_shader(g, c.r[3], c.r[4], c.r[5], c.r[6]);
                let streams = self.intern_fetch(streams);
                self.fetch_shaders.insert(fs, streams);
            }
            Call::CopySurface => self.copy_surface(g, c.r[3], c.r[6]),
            // (mode, address, size): the CPU wrote memory the GPU may have cached; a texture or a shader program there is read again
            // next time
            Call::Invalidate => {
                let (mode, address, size) = (a(0), a(1), a(2));
                let started = self.perf.on.then(Instant::now);
                if mode & 2 != 0 || mode & 0x40 != 0 {
                    self.invalidate_textures(address, size);
                }
                let examined = crate::shader_cache::invalidates_shaders(mode).then(|| self.invalidate_shaders(address, size));
                if let Some(t) = started {
                    self.perf.invalidate(t.elapsed(), examined);
                }
            }
            Call::InitSampler => {
                let (s, clamp, filter) = (a(0), a(1) & 15, a(2) & 15);
                g.put_u32(s, clamp | clamp << 4 | clamp << 8 | filter << 12 | filter << 16);
                g.put_u32(s + 4, 0xf | (13 * 16) << 18);
                g.put_u32(s + 8, 0);
            }
            Call::InitSamplerClamping => {
                let (s, x, y, z) = (a(0), a(1) & 15, a(2) & 15, a(3) & 15);
                let w = g.u32(s) & !0xfff;
                g.put_u32(s, w | x | y << 4 | z << 8);
            }
            Call::InitSamplerXYFilter => {
                let (s, mag, min) = (a(0), a(1) & 15, a(2) & 15);
                let w = g.u32(s) & !(0xff << 12);
                g.put_u32(s, w | mag << 12 | min << 16);
            }
            Call::InitSamplerZMFilter => {
                let (s, z, mip) = (a(0), a(1) & 15, a(2) & 15);
                let w = g.u32(s) & !(0xff << 20);
                g.put_u32(s, w | z << 20 | mip << 24);
            }
            Call::InitSamplerLOD => {
                let s = a(0);
                let q = |v: f32| (v.clamp(0.0, 63.0) * 16.0) as u32 & 0x3ff;
                let w = g.u32(s + 4) & 0xff;
                g.put_u32(s + 4, w | q(farg(c, 0)) << 8 | q(farg(c, 1)) << 18);
                g.put_u32(s + 8, farg(c, 2).to_bits());
            }
            Call::InitSamplerDepthCompare => {
                let (s, func) = (a(0), a(1) & 15);
                let w = g.u32(s + 4) & !0xf;
                g.put_u32(s + 4, w | func);
            }
            Call::ClearDepthStencilEx => self.clear_depth(g, c.r[3], farg(c, 0), c.r[4], c.r[5]),
            Call::DrawEx => self.draw(g, a(0), a(1), None, a(2), a(3)),
            Call::DrawIndexedEx => self.draw(g, a(0), a(1), Some((a(2), a(3))), a(4), a(5)),
            Call::ClearColor => self.clear(g, c),
            Call::CopyColorBufferToScanBuffer => self.copy_to_scan_buffer(g, c),
            Call::SwapScanBuffers => {
                self.present();
                self.swaps += 1;
                mh3u_rt::FRAMES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.prune();
                self.pipes.collect();
                if self.perf.on {
                    self.swap_point();
                    let (pipes, work) = (self.pipes.counts, self.caches.work);
                    // taken out for the call, so the caches can be measured while it runs
                    let mut perf = std::mem::replace(&mut self.perf, crate::perf::Perf::idle());
                    let lines = perf.swap(self.swaps - 1, pipes, work, || self.cache_sizes());
                    let (ms, gfx) = perf.last;
                    self.perf = perf;
                    for line in lines {
                        eprintln!("{line}");
                    }
                    if crate::draw::Lap::enabled() {
                        let mut laps = crate::draw::Lap::take_frame();
                        let calls = std::mem::replace(&mut self.call_frame, vec![Default::default(); Call::ALL.len()]);
                        if ms >= slow_frame_ms() {
                            laps.times.extend(
                                Call::ALL
                                    .iter()
                                    .zip(calls)
                                    .filter(|(_, (_, n))| *n > 0)
                                    .map(|(call, (d, n))| (call.name(), d, n)),
                            );
                            eprintln!("{}", crate::draw::slow_frame_text(self.swaps - 1, ms, gfx, &laps));
                        }
                    }
                }
                self.diag.maybe_print(&self.caches.skipped);
                if self.diag.swapped(self.swaps) {
                    // the frame written out is on the TV now
                    if let (Some((w, h, rgba)), Some(dir)) = (self.read_tv(), self.diag.finished_dir.take()) {
                        let _ = std::fs::write(dir.join("tv.png"), crate::report::png(w, h, &rgba));
                    }
                }
                if let Some((since, at)) = self.fps {
                    // the frame rate, swaps per second, every five seconds (for comparing builds)
                    let secs = since.elapsed().as_secs_f64();
                    if secs >= 5.0 {
                        eprintln!("[fps] {:.1}", f64::from(self.swaps - at) / secs);
                        self.fps = Some((Instant::now(), self.swaps));
                    }
                }
                if let Some(dir) = std::env::var_os("MH3U_GFX_SAVE").filter(|_| self.swaps.is_multiple_of(self.save_every)) {
                    self.save_tv(std::path::Path::new(&dir));
                }
                if self.log && self.swaps.is_multiple_of(120) {
                    let mut gs: Vec<_> = self.caches.gs_draws.iter().collect();
                    gs.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
                    for ((g_, vs, prim, count, inst), n) in gs.iter().take(6) {
                        eprintln!("[gfx] gs draws: gs {g_:#x} vs {vs:#x} prim {prim} count {count} instances {inst}: {n}");
                    }
                    eprintln!(
                        "[gfx] draws into {:x?}, last copy to the TV from {:#x}, skipped {:?}",
                        self.caches.drawn_into, self.last_tv_source, self.caches.skipped
                    );
                    eprintln!(
                        "[gfx] {} swaps, {} draws ({} with no shader, {} with)",
                        self.swaps, self.draws, self.null_draws, self.real_draws
                    );
                }
            }
            Call::WaitForVsync => self.wait_for_vsync(),
            // (swap count, flip count, last flip time, last vsync time): every swap has flipped by now
            Call::GetSwapStatus => {
                for (p, v) in [(c.r[3], self.swaps), (c.r[4], self.swaps)] {
                    if p != 0 {
                        g.put_u32(p, v);
                    }
                }
                c.r[3] = 0;
            }
            Call::SetSwapInterval => self.interval = c.r[3],
            Call::GetSwapInterval => c.r[3] = self.interval,
            Call::BeginDisplayListEx | Call::EndDisplayList | Call::DirectCallDisplayList => {}
        }
    }

    /// Writes the shader's structure and program to the folder named by MH3U_GFX_DUMP (once per shader), for looking at offline.
    fn dump_shader(&mut self, g: &Guest, address: u32, vertex: bool) {
        // read once: this runs for every draw
        static DIR: std::sync::OnceLock<Option<std::ffi::OsString>> = std::sync::OnceLock::new();
        let Some(dir) = DIR.get_or_init(|| std::env::var_os("MH3U_GFX_DUMP")).as_ref() else {
            return;
        };
        if address == 0 || !self.dumped.insert(address) {
            return;
        }
        let (struct_len, size_at, program_at) = if vertex { (0x140u32, 0xd0, 0xd4) } else { (0xe8, 0xa4, 0xa8) };
        let (size, program) = (g.u32(address + size_at), g.u32(address + program_at));
        let mut header = vec![0u8; struct_len as usize];
        g.read(address, &mut header);
        let mut code = vec![0u8; size.min(1 << 20) as usize];
        g.read(program, &mut code);
        let dir = std::path::Path::new(&dir);
        let kind = if vertex { "vs" } else { "ps" };
        let _ = std::fs::create_dir_all(dir);
        // the tables the shader header points at, with their names
        let mut text = String::new();
        let table = |count_at: u32, ptr_at: u32, entry: u32, text: &mut String, what: &str| {
            let (n, p) = (g.u32(address + count_at), g.u32(address + ptr_at));
            for i in 0..n.min(64) {
                let e = p + i * entry;
                let words: Vec<String> = (0..entry / 4).map(|k| format!("{:#x}", g.u32(e + 4 * k))).collect();
                let name = g.cstr(g.u32(e));
                text.push_str(&format!("{what} {i}: {name:?} {}\n", words.join(" ")));
            }
        };
        if vertex {
            table(0xdc, 0xe0, 12, &mut text, "uniform block");
            table(0xe4, 0xe8, 20, &mut text, "uniform var");
            table(0xfc, 0x100, 12, &mut text, "sampler");
            table(0x104, 0x108, 16, &mut text, "attrib");
        } else {
            table(0xb0, 0xb4, 12, &mut text, "uniform block");
            table(0xb8, 0xbc, 20, &mut text, "uniform var");
            table(0xd0, 0xd4, 12, &mut text, "sampler");
        }
        let _ = std::fs::write(dir.join(format!("{kind}-{address:08x}.txt")), text);
        let _ = std::fs::write(dir.join(format!("{kind}-{address:08x}.struct")), header);
        let _ = std::fs::write(dir.join(format!("{kind}-{address:08x}.code")), code);
    }

    fn draw(&mut self, g: &Guest, primitive: u32, count: u32, indexed: Option<(u32, u32)>, offset: u32, instances: u32) {
        self.draws += 1;
        if !self.diag.per_draw() {
            self.draw_call(g, primitive, count, indexed, offset, instances);
        } else {
            self.diag.draw();
            let before: u64 = self.caches.skipped.values().sum();
            let skipped_before = self.diag.dumping.then(|| self.caches.skipped.clone());
            self.draw_call(g, primitive, count, indexed, offset, instances);
            if let Some(then) = skipped_before {
                let outcome = if self.caches.skipped.values().sum::<u64>() == before {
                    "drawn".to_string()
                } else {
                    let why = self
                        .caches
                        .skipped
                        .iter()
                        .find(|(k, v)| then.get(*k).copied().unwrap_or(0) != **v)
                        .map_or("skipped", |(k, _)| *k);
                    format!("SKIPPED ({why})")
                };
                let st = self.contexts.entry(self.current).or_default().clone();
                let shaders = [
                    (
                        format!("vs-{:08x}", st.vertex_shader),
                        self.diag.shader_texts.get(&format!("vs-{:08x}", st.vertex_shader)).cloned(),
                    ),
                    (
                        format!("ps-{:08x}", st.pixel_shader),
                        self.diag.shader_texts.get(&format!("ps-{:08x}", st.pixel_shader)).cloned(),
                    ),
                ];
                let fetch = self.fetch_shaders.get(&st.fetch_shader).cloned();
                self.diag.dump_draw(
                    g,
                    &st,
                    (primitive, count, indexed, offset, instances),
                    &outcome,
                    shaders,
                    fetch.as_deref().unwrap_or(&[]),
                );
            }
        }
        {
            let (vs, ps) = {
                let st = self.contexts.entry(self.current).or_default();
                (st.vertex_shader, st.pixel_shader)
            };
            self.dump_shader(g, vs, true);
            self.dump_shader(g, ps, false);
        }
        if !self.log {
            return;
        }
        let st = self.contexts.entry(self.current).or_default().clone();
        if st.vertex_shader == 0 && st.pixel_shader == 0 {
            self.null_draws += 1;
            return;
        }
        self.real_draws += 1;
        if self.real_draws > 30 {
            return;
        }
        let (vs, ps) = (st.vertex_shader, st.pixel_shader);
        let shader = |p: u32, size_at: u32, prog_at: u32| (g.u32(p + size_at), g.u32(p + prog_at));
        eprintln!(
            "draw {}: prim {primitive} count {count} indexed {indexed:?} offset {offset} instances {instances}",
            self.draws
        );
        if vs != 0 {
            eprintln!("  vs {vs:#x} (size, program) {:x?}", shader(vs, 0xd0, 0xd4));
        }
        if ps != 0 {
            eprintln!("  ps {ps:#x} (size, program) {:x?}", shader(ps, 0xa4, 0xa8));
        }
        eprintln!("  fetch shader {:#x}", st.fetch_shader);
        for (i, b) in st.attrib_buffers.iter().enumerate().filter(|(_, b)| b.address != 0) {
            eprintln!("  attrib buffer {i}: {:#x} size {:#x} stride {}", b.address, b.size, b.stride);
        }
        for (i, b) in st.vertex_blocks.iter().enumerate().filter(|(_, b)| b.address != 0) {
            eprintln!("  vertex block {i}: {:#x} size {:#x}", b.address, b.size);
        }
        for (i, b) in st.pixel_blocks.iter().enumerate().filter(|(_, b)| b.address != 0) {
            eprintln!("  pixel block {i}: {:#x} size {:#x}", b.address, b.size);
        }
        for (i, &t) in st.pixel_textures.iter().enumerate().filter(|(_, t)| **t != 0) {
            eprintln!(
                "  texture {i}: {t:#x} dim {} {}x{}x{} mips {} format {:#x} tile {} image {:#x}",
                g.u32(t),
                g.u32(t + 4),
                g.u32(t + 8),
                g.u32(t + 0xc),
                g.u32(t + 0x10),
                g.u32(t + 0x14),
                g.u32(t + 0x30),
                g.u32(t + 0x24)
            );
        }
        for (i, &cb) in st.colour_buffers.iter().enumerate().filter(|(_, c)| **c != 0) {
            eprintln!(
                "  colour buffer {i}: {cb:#x} {}x{} format {:#x}",
                g.u32(cb + 4),
                g.u32(cb + 8),
                g.u32(cb + 0x14)
            );
        }
        eprintln!(
            "  depth {} {} func {} cull {} {} viewport {:?} scissor {:?}",
            st.depth_test, st.depth_write, st.depth_func, st.cull_front, st.cull_back, st.viewport, st.scissor
        );
    }
}

/// A frame at least this long prints `[gfx slow frame]` (`MH3U_GFX_SLOW_MS`, 25 by default).
fn slow_frame_ms() -> f64 {
    static MS: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *MS.get_or_init(|| {
        std::env::var("MH3U_GFX_SLOW_MS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(25.0)
    })
}

impl Gx2Backend for Gx2 {
    fn implements(&self, name: &str) -> bool {
        let known = Call::from_name(name).is_some();
        if !known && name.starts_with("gx2:") {
            self.diag.stub(name.trim_start_matches("gx2:"));
        }
        known
    }

    fn resolve(&self, name: &str) -> Option<u32> {
        Call::from_name(name).map(|c| c as u32)
    }

    fn call_id(&mut self, id: Option<u32>, name: &str, c: &mut Ctx, g: &Guest) {
        // one clock reading at each end of a call, for both the frame's time in graphics calls (the area-change and slow-frame lines)
        // and the call's own line in `[gfx slow frame]`
        let began = self.perf.on.then(Instant::now);
        self.call_started = began;
        let call = id.and_then(|id| Call::ALL.get(id as usize)).copied();
        match call {
            Some(call) => self.run(call, c, g),
            None => self.call(name, c, g),
        }
        if let (Some(began), Some(t)) = (began, self.call_started.take()) {
            let now = Instant::now();
            // since the call began, or since the swap's frame ended inside it
            let d = now - t;
            self.perf.frame_gfx += d;
            if let Some(call) = call {
                self.add_call_time(call, d);
            }
            self.last_call = now - began;
        }
    }

    fn times_calls(&self) -> bool {
        self.perf.on
    }

    fn last_call_time(&self) -> std::time::Duration {
        self.last_call
    }

    fn call(&mut self, name: &str, c: &mut Ctx, g: &Guest) {
        if let Some(call) = Call::from_name(name) {
            self.run(call, c, g);
        }
    }
}

impl Gx2 {
    /// With `MH3U_GFX_TIMES` (not `=calls`), a call's time goes to the frame's `[gfx slow frame]` line, except the draws (which have
    /// their own steps) and the display list ones.
    fn add_call_time(&mut self, call: Call, d: std::time::Duration) {
        if crate::draw::Lap::enabled()
            && !matches!(
                call,
                Call::DirectCallDisplayList | Call::DrawEx | Call::DrawIndexedEx | Call::BeginDisplayListEx | Call::EndDisplayList
            )
        {
            let e = &mut self.call_frame[call as usize];
            e.0 += d;
            e.1 += 1;
        }
    }

    /// Inside the swap, where a frame ends for the frame-time lines: the swap's time so far (presenting, and letting go of what the
    /// caches no longer use) counts in the frame it ends, and the rest of the call in the next one.
    fn swap_point(&mut self) {
        if let Some(t) = self.call_started {
            let now = Instant::now();
            let d = now - t;
            self.perf.frame_gfx += d;
            self.add_call_time(Call::SwapScanBuffers, d);
            self.call_started = Some(now);
        }
    }

    /// A call from the game: the display list ones here, the others recorded into the list being made or run now.
    fn run(&mut self, call: Call, c: &mut Ctx, g: &Guest) {
        match call {
            Call::BeginDisplayListEx => {
                self.recording = Some(Recording {
                    address: c.r[3],
                    calls: Vec::new(),
                    made: 0,
                    seen: Seen::default(),
                });
            }
            Call::EndDisplayList => {
                if let Some(r) = self.recording.take() {
                    c.r[3] = 0x20 + 0x10 * r.made;
                    crate::draw::Lap::count("display list calls left out", u64::from(r.made) - r.calls.len() as u64);
                    self.lists.insert(r.address, r.calls.into());
                }
            }
            Call::DirectCallDisplayList => {
                // replayed by reference (a cheap shared handle, so a list that re-records itself is still safe); one scratch context for the whole list
                let Some(calls) = self.lists.get(&c.r[3]).cloned() else { return };
                crate::draw::Lap::count("display lists replayed", 1);
                crate::draw::Lap::count("display list calls replayed", calls.len() as u64);
                let mut replay = c.clone();
                for rec in calls.iter() {
                    rec.load(&mut replay);
                    self.exec(rec.call, &mut replay, g, Some(&rec.stack));
                }
            }
            _ => {
                if let Some(r) = self.recording.as_mut().filter(|_| !is_immediate(call)) {
                    r.made += 1;
                    if !r.seen.redundant(call, c.r[3], c.r[4]) {
                        r.calls.push(Recorded::new(call, c, g));
                    }
                } else {
                    self.exec(call, c, g, None);
                }
            }
        }
    }
}

/// A context with every register zero (for the tests).
#[cfg(test)]
pub(crate) fn blank_ctx() -> Ctx {
    Ctx {
        r: [0; 32],
        f: [0; 32],
        ps1: [0; 32],
        cr: 0,
        xer: 0,
        lr: 0,
        ctr: 0,
        fpscr: 0,
        gqr: [0; 8],
        reservation: 0,
        has_reservation: 0,
        tb: 0,
        mem: std::ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_swaps_time_so_far_counts_in_the_frame_it_ends() {
        // run 18: a frame's own present and cache pruning showed up in the next frame's graphics time (an 11.3 ms frame with 13.6 ms
        // in graphics calls), and a 26 ms frame with 12 ms
        let mut gx2 = super::Gx2::new(crate::Display::noop());
        let started = std::time::Instant::now() - std::time::Duration::from_millis(10);
        gx2.call_started = Some(started);
        gx2.swap_point();
        assert!(gx2.perf.frame_gfx >= std::time::Duration::from_millis(10));
        let reset = gx2.call_started.expect("still in the call");
        assert!(
            reset > started && reset.elapsed() < std::time::Duration::from_millis(5),
            "the rest goes to the next frame"
        );
        // with timing off nothing is measured
        let mut off = super::Gx2::new(crate::Display::noop());
        off.swap_point();
        assert_eq!(off.perf.frame_gfx, std::time::Duration::ZERO);
    }

    use super::*;

    #[test]
    fn call_names_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for &call in Call::ALL {
            assert!(call.name().starts_with("gx2:GX2"), "{call:?}");
            assert!(seen.insert(call.name()), "duplicate name {}", call.name());
            assert_eq!(Call::from_name(call.name()), Some(call));
        }
        assert_eq!(Call::ALL.len(), 52);
        assert_eq!(Call::from_name("gx2:GX2Nothing"), None);
        assert_eq!(Call::from_name("coreinit:GX2DrawEx"), None);
    }

    #[test]
    fn resolved_numbers_name_the_same_call() {
        let gx2_names: Vec<&str> = Call::ALL.iter().map(|c| c.name()).collect();
        for (i, name) in gx2_names.iter().enumerate() {
            let id = Call::from_name(name).map(|c| c as u32);
            assert_eq!(id, Some(i as u32));
            assert_eq!(Call::ALL[i].name(), *name);
        }
    }

    /// The village's broken scenery (run 4, frame 4800): its fetch shaders were made while the frame's display list was being recorded, so
    /// `GX2InitFetchShaderEx` was recorded too and read the attribute array a frame later, from reused memory (draws.txt showed one
    /// `sint 16_16_16_16` stream and then all-zero streams, all at location 0). CPU-only calls must run when they are made.
    #[test]
    fn object_setup_calls_are_never_recorded() {
        for &call in Call::ALL {
            let name = call.name();
            if name.starts_with("gx2:GX2Init") || name.starts_with("gx2:GX2Calc") {
                assert!(is_immediate(call), "{name} would be recorded into a display list");
            }
        }
        // the GPU's commands are still recorded
        for call in [
            Call::SetFetchShader,
            Call::SetAttribBuffer,
            Call::DrawIndexedEx,
            Call::SetPixelSampler,
            Call::Invalidate,
        ] {
            assert!(!is_immediate(call), "{}", call.name());
        }
    }

    #[test]
    fn a_fetch_shader_made_while_a_list_is_recorded_is_made_at_once() {
        let mut gx2 = Gx2::new(crate::Display::noop());
        let mut m = vec![0u8; 0x1_0000];
        let g = Guest::new(m.as_mut_ptr());
        let mut c = blank_ctx();
        c.r[1] = 0x8000;
        c.r[3] = 0x4000;
        gx2.run(Call::BeginDisplayListEx, &mut c, &g);
        c.r[3] = 1;
        gx2.run(Call::CalcFetchShaderSizeEx, &mut c, &g);
        assert_eq!(c.r[3], 48, "the size is the caller's now");
        for (k, v) in [0u32, 0, 0, 0x811, 0, 0, 0x0001_0203, 3].iter().enumerate() {
            g.put_u32(0x200 + 4 * k as u32, *v);
        }
        (c.r[3], c.r[4], c.r[5], c.r[6]) = (0x100, 0x400, 1, 0x200);
        gx2.run(Call::InitFetchShaderEx, &mut c, &g);
        // the caller's array is gone before the list is played
        g.fill(0x200, 0, 32);
        c.r[3] = 0x100;
        gx2.run(Call::SetFetchShader, &mut c, &g);
        gx2.run(Call::EndDisplayList, &mut c, &g);
        let streams = gx2.fetch_shaders.get(&0x100).expect("made while recording");
        assert_eq!(streams[0].format, 0x811);
        c.r[3] = 0x4000;
        gx2.run(Call::DirectCallDisplayList, &mut c, &g);
        assert_eq!(
            gx2.fetch_shaders[&0x100][0].format, 0x811,
            "playing the list does not make it again"
        );
        assert_eq!(gx2.state().fetch_shader, 0x100);
    }

    /// A fetch shader made again with the same attributes (the game does this every frame for some) is the same shared slice, so the
    /// shader caches keyed by it keep hitting; other attributes get their own.
    #[test]
    fn a_fetch_shader_made_again_is_the_same_slice() {
        let mut gx2 = Gx2::new(crate::Display::noop());
        let mut m = vec![0u8; 0x1_0000];
        let g = Guest::new(m.as_mut_ptr());
        let mut c = blank_ctx();
        let make = |gx2: &mut Gx2, c: &mut Ctx, fs: u32, format: u32| {
            for (k, v) in [0u32, 0, 0, format, 0, 0, 0x0001_0203, 3].iter().enumerate() {
                g.put_u32(0x200 + 4 * k as u32, *v);
            }
            (c.r[3], c.r[4], c.r[5], c.r[6]) = (fs, 0x400, 1, 0x200);
            gx2.run(Call::InitFetchShaderEx, c, &g);
            gx2.fetch_shaders[&fs].clone()
        };
        let first = make(&mut gx2, &mut c, 0x100, 0x811);
        let again = make(&mut gx2, &mut c, 0x100, 0x811);
        let elsewhere = make(&mut gx2, &mut c, 0x140, 0x811);
        let other = make(&mut gx2, &mut c, 0x100, 0x80d);
        assert!(std::sync::Arc::ptr_eq(&first, &again));
        assert!(std::sync::Arc::ptr_eq(&first, &elsewhere), "the contents decide, not the address");
        assert!(!std::sync::Arc::ptr_eq(&first, &other));
        assert_eq!(gx2.fetch_interned.values().map(Vec::len).sum::<usize>(), 2);
    }

    /// The shadow pass turns the polygon offset on with `GX2SetPolygonControl`; the scene's materials set their culling with
    /// `GX2SetCullOnlyControl`, which turns it off again (it writes the whole register). Kept on, every later draw had the bias of the
    /// last `GX2SetPolygonOffset`, which differed between a depth-only pass and its colour pass.
    #[test]
    fn cull_only_control_turns_the_polygon_offset_off() {
        let mut gx2 = Gx2::new(crate::Display::noop());
        let mut m = vec![0u8; 0x1_0000];
        let g = Guest::new(m.as_mut_ptr());
        let mut c = blank_ctx();
        c.r[1] = 0x8000;
        // front face CCW, cull back, no polygon modes, front offset on, back off
        c.r[3..12].copy_from_slice(&[0, 0, 1, 0, 0, 0, 1, 0, 0]);
        gx2.run(Call::SetPolygonControl, &mut c, &g);
        c.f[1..6].copy_from_slice(&[(-192.0f64).to_bits(), 0, (-192.0f64).to_bits(), 0, 0]);
        gx2.run(Call::SetPolygonOffset, &mut c, &g);
        assert!(gx2.state().offset_front && !gx2.state().offset_back);
        c.r[3..6].copy_from_slice(&[0, 0, 1]);
        gx2.run(Call::SetCullOnlyControl, &mut c, &g);
        let st = gx2.state();
        assert!(!st.offset_front && !st.offset_back, "no bias after GX2SetCullOnlyControl");
        assert!(st.cull_back && !st.cull_front);
        assert_eq!(
            st.polygon_offset[0], -192.0,
            "the offset itself is kept for the next GX2SetPolygonControl"
        );
    }

    #[test]
    fn fetch_shader_streams_are_read_when_made() {
        let mut m = vec![0u8; 0x1000];
        let g = Guest::new(m.as_mut_ptr());
        // two streams at 0x200: float3 position at location 0, uint8x4 at location 2, offset 12
        let words = [[0, 0, 0, 0x811, 0, 0, 0x0001_0203, 3], [2, 0, 12, 0x10a, 0, 0, 0x0001_0203, 3]];
        for (i, w) in words.iter().enumerate() {
            for (k, v) in w.iter().enumerate() {
                g.put_u32(0x200 + 32 * i as u32 + 4 * k as u32, *v);
            }
        }
        // the word after the 0x20-byte GX2FetchShader is someone else's
        g.put_u32(0x120, 0xdead_beef);
        let (fs, streams) = init_fetch_shader(&g, 0x100, 0x400, 2, 0x200);
        // the caller's array goes away (as a stack frame does)
        g.fill(0x200, 0, 64);
        assert_eq!(fs, 0x100);
        assert_eq!(streams.len(), 2);
        assert_eq!((streams[0].location, streams[0].offset, streams[0].format), (0, 0, 0x811));
        assert_eq!((streams[1].location, streams[1].offset, streams[1].format), (2, 12, 0x10a));
        assert_eq!(streams[1].select, [0, 1, 2, 3]);
        assert_eq!((g.u32(0x10c), g.u32(0x110)), (0x400, 2));
        assert_eq!(g.u32(0x120), 0xdead_beef);
    }

    #[test]
    fn immediate_calls_are_the_display_and_setup_ones() {
        let immediate: Vec<&str> = Call::ALL.iter().filter(|c| is_immediate(**c)).map(|c| c.name()).collect();
        assert_eq!(
            immediate,
            [
                "gx2:GX2BeginDisplayListEx",
                "gx2:GX2EndDisplayList",
                "gx2:GX2DirectCallDisplayList",
                "gx2:GX2CalcFetchShaderSizeEx",
                "gx2:GX2InitFetchShaderEx",
                "gx2:GX2InitSampler",
                "gx2:GX2InitSamplerClamping",
                "gx2:GX2InitSamplerXYFilter",
                "gx2:GX2InitSamplerZMFilter",
                "gx2:GX2InitSamplerLOD",
                "gx2:GX2InitSamplerDepthCompare",
                "gx2:GX2SwapScanBuffers",
                "gx2:GX2WaitForVsync",
                "gx2:GX2GetSwapStatus",
                "gx2:GX2SetSwapInterval",
                "gx2:GX2GetSwapInterval",
            ]
            .iter()
            .copied()
            .filter(|n| immediate.contains(n))
            .collect::<Vec<_>>()
        );
        assert_eq!(immediate.len(), 16);
    }

    #[test]
    fn redundant_unit_settings_are_left_out() {
        let mut s = Seen::default();
        assert!(!s.redundant(Call::SetPixelTexture, 0x100, 0));
        assert!(s.redundant(Call::SetPixelTexture, 0x100, 0));
        // another unit, another texture, or a sampler of the same unit are changes
        assert!(!s.redundant(Call::SetPixelTexture, 0x100, 1));
        assert!(!s.redundant(Call::SetPixelTexture, 0x200, 0));
        assert!(!s.redundant(Call::SetPixelSampler, 0x200, 0));
        assert!(s.redundant(Call::SetPixelSampler, 0x200, 0));
        assert!(s.redundant(Call::SetPixelTexture, 0x200, 0));
        // other calls in between change nothing about the units
        assert!(!s.redundant(Call::DrawEx, 0, 0));
        assert!(!s.redundant(Call::SetVertexShader, 0x100, 0));
        assert!(s.redundant(Call::SetPixelTexture, 0x200, 0));
        // a change of context forgets everything
        assert!(!s.redundant(Call::SetContextState, 0x5000, 0));
        assert!(!s.redundant(Call::SetPixelTexture, 0x200, 0));
        assert!(!s.redundant(Call::SetupContextStateEx, 0x5000, 0));
        assert!(!s.redundant(Call::SetPixelSampler, 0x200, 0));
        // units past the last are never left out (the call does nothing either way)
        assert!(!s.redundant(Call::SetPixelTexture, 0x100, 18));
        assert!(!s.redundant(Call::SetPixelTexture, 0x100, 18));
    }

    #[test]
    fn recorded_arguments_come_back() {
        let mut c = blank_ctx();
        for k in 0..32 {
            c.r[k] = 100 + k as u32;
            c.f[k] = 1000 + k as u64;
        }
        let mut rec = Recorded {
            call: Call::DrawEx,
            r: [0; 8],
            f: [0; 8],
            stack: [0; 8],
        };
        rec.r.copy_from_slice(&c.r[3..11]);
        rec.f.copy_from_slice(&c.f[1..9]);
        let mut out = blank_ctx();
        rec.load(&mut out);
        assert_eq!(out.r[3..11], c.r[3..11]);
        assert_eq!(out.f[1..9], c.f[1..9]);
    }
}
