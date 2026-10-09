//! The game's `GX2` calls, over `wgpu`. For now: colour buffers as textures, clears, and showing the TV picture in the window with the
//! console's frame pacing. Draws, shaders and textures are the next part.

use crate::Display;
use crate::state::{State, arg, farg};
use mh3u_rt::{Ctx, Guest, Gx2Backend};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The names this backend handles.
const CALLS: &[&str] = &[
    "gx2:GX2SetupContextStateEx",
    "gx2:GX2SetContextState",
    "gx2:GX2BeginDisplayListEx",
    "gx2:GX2EndDisplayList",
    "gx2:GX2DirectCallDisplayList",
    "gx2:GX2SetVertexShader",
    "gx2:GX2SetPixelShader",
    "gx2:GX2SetPrimitiveRestartIndex",
    "gx2:GX2SetGeometryShader",
    "gx2:GX2SetFetchShader",
    "gx2:GX2SetAttribBuffer",
    "gx2:GX2SetVertexUniformBlock",
    "gx2:GX2SetPixelUniformBlock",
    "gx2:GX2SetGeometryUniformBlock",
    "gx2:GX2SetPixelTexture",
    "gx2:GX2SetPixelSampler",
    "gx2:GX2SetColorBuffer",
    "gx2:GX2SetDepthBuffer",
    "gx2:GX2SetViewport",
    "gx2:GX2SetScissor",
    "gx2:GX2SetBlendControl",
    "gx2:GX2SetColorControl",
    "gx2:GX2SetTargetChannelMasks",
    "gx2:GX2SetDepthOnlyControl",
    "gx2:GX2SetDepthStencilControl",
    "gx2:GX2SetCullOnlyControl",
    "gx2:GX2SetPolygonControl",
    "gx2:GX2SetAlphaTest",
    "gx2:GX2SetRasterizerClipControlEx",
    "gx2:GX2SetShaderModeEx",
    "gx2:GX2CalcFetchShaderSizeEx",
    "gx2:GX2InitFetchShaderEx",
    "gx2:GX2ClearDepthStencilEx",
    "gx2:GX2Invalidate",
    "gx2:GX2InitSampler",
    "gx2:GX2InitSamplerClamping",
    "gx2:GX2InitSamplerXYFilter",
    "gx2:GX2InitSamplerZMFilter",
    "gx2:GX2InitSamplerLOD",
    "gx2:GX2InitSamplerDepthCompare",
    "gx2:GX2CopySurface",
    "gx2:GX2DrawEx",
    "gx2:GX2DrawIndexedEx",
    "gx2:GX2ClearColor",
    "gx2:GX2CopyColorBufferToScanBuffer",
    "gx2:GX2SwapScanBuffers",
    "gx2:GX2WaitForVsync",
    "gx2:GX2GetSwapStatus",
    "gx2:GX2SetSwapInterval",
    "gx2:GX2GetSwapInterval",
];

const FRAME: Duration = Duration::from_nanos(16_683_333);

/// The console's TV scan target and its GamePad one.
const TV: u32 = 1;

pub(crate) struct Target {
    pub(crate) texture: wgpu::Texture,
    pub(crate) format: wgpu::TextureFormat,
}

pub struct Gx2 {
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
    lists: HashMap<u32, Vec<Recorded>>,
    recording: Option<(u32, Vec<Recorded>)>,
    pub(crate) draws: u64,
    dumped: std::collections::HashSet<u32>,
    null_draws: u64,
    real_draws: u64,
    pub(crate) log: bool,
    next_vsync: Instant,
    pub(crate) caches: crate::draw::DrawCaches,
    pub(crate) last_tv_source: u32,
    save_every: u32,
    pub(crate) fetch_shaders: HashMap<u32, Vec<crate::draw::AttribStream>>,
    /// With MH3U_FPS set: when the frame rate was last printed, and the swap count then.
    fps: Option<(Instant, u32)>,
}

/// The texture format of a `GX2SurfaceFormat` of a colour buffer.
fn texture_format(gx2: u32) -> wgpu::TextureFormat {
    match gx2 {
        0x41a => wgpu::TextureFormat::Rgba8UnormSrgb,
        0x19 => wgpu::TextureFormat::Rgb10a2Unorm,
        0x816 | 0x810 => wgpu::TextureFormat::Rgba16Float,
        0x1 => wgpu::TextureFormat::R8Unorm,
        0x7 => wgpu::TextureFormat::Rg8Unorm,
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
    pub fn new(d: Display) -> Gx2 {
        let size = d.window.inner_size();
        let caps = d.surface.get_capabilities(&d.adapter);
        // the game's colours are already in the display's encoding, so a surface that does not encode them again
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        d.surface.configure(&d.device, &config);
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
            caches: Default::default(),
            last_tv_source: 0,
            save_every: std::env::var("MH3U_GFX_SAVE_EVERY")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(120),
            fetch_shaders: HashMap::new(),
            fps: std::env::var_os("MH3U_FPS").map(|_| (Instant::now(), 0)),
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
            self.targets.insert(image, Target { texture, format });
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
        let f = |n: usize| f64::from(f64::from_bits(c.f[n]) as f32);
        let view = self.targets[&image].texture.create_view(&Default::default());
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
        // MH3U_GFX_SAVE_IMAGE=<hex address of a colour buffer's image> saves that buffer instead of the TV picture
        let wanted = std::env::var("MH3U_GFX_SAVE_IMAGE")
            .ok()
            .and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok());
        let tv = match wanted {
            Some(image) => self.targets.get(&image).map(|t| &t.texture),
            None => self.tv.as_ref(),
        };
        let Some(tv) = tv else { return };
        if tv.format() != wgpu::TextureFormat::Rgba8Unorm {
            return;
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
        let Ok(data) = slice.get_mapped_range() else { return };
        let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
        for y in 0..h {
            for x in 0..w {
                let at = (y * row + x * 4) as usize;
                ppm.extend_from_slice(&data[at..at + 3]);
            }
        }
        let _ = std::fs::create_dir_all(folder);
        let _ = std::fs::write(folder.join(format!("tv-{:06}.ppm", self.swaps)), ppm);
    }

    /// Shows the TV picture, scaled to the window with the picture's proportions kept.
    fn present(&mut self) {
        self.flush_draws();
        let size = self.d.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        if size.width != self.config.width || size.height != self.config.height {
            self.config.width = size.width;
            self.config.height = size.height;
            self.d.surface.configure(&self.d.device, &self.config);
        }
        let frame = match self.d.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.d.surface.configure(&self.d.device, &self.config);
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

/// A call kept in a display list: its name, the argument registers and the stack arguments.
#[derive(Clone)]
struct Recorded {
    name: String,
    r: [u32; 32],
    f: [u64; 32],
    stack: Vec<u32>,
}

/// The calls that are never part of a display list (they are about the display, not the GPU's command stream).
fn is_immediate(name: &str) -> bool {
    matches!(
        name,
        "gx2:GX2BeginDisplayListEx"
            | "gx2:GX2EndDisplayList"
            | "gx2:GX2DirectCallDisplayList"
            | "gx2:GX2SwapScanBuffers"
            | "gx2:GX2WaitForVsync"
            | "gx2:GX2GetSwapStatus"
            | "gx2:GX2SetSwapInterval"
            | "gx2:GX2GetSwapInterval"
    )
}

impl Gx2 {
    fn state(&mut self) -> &mut State {
        self.contexts.entry(self.current).or_default()
    }

    /// Runs one call (or plays back a recorded one: `replay` is its stack).
    fn exec(&mut self, name: &str, c: &mut Ctx, g: &Guest, replay: Option<&[u32]>) {
        if false {
            eprintln!("call {name}{}", if replay.is_some() { " (replayed)" } else { "" });
        }
        let a = |n: usize| arg(c, g, replay, n);
        match name {
            "gx2:GX2SetupContextStateEx" => {
                self.contexts.insert(c.r[3], State::default());
                self.current = c.r[3];
            }
            "gx2:GX2SetContextState" => self.current = c.r[3],
            "gx2:GX2SetVertexShader" => {
                let v = a(0);
                if false {
                    eprintln!("  vertex shader {v:#x}");
                }
                self.state().vertex_shader = v;
            }
            "gx2:GX2SetPrimitiveRestartIndex" => {
                let v = a(0);
                self.state().restart_index = Some(v);
            }
            "gx2:GX2SetPixelShader" => {
                let v = a(0);
                if false {
                    eprintln!("  pixel shader {v:#x}");
                }
                self.state().pixel_shader = v;
            }
            "gx2:GX2SetGeometryShader" => {
                let v = a(0);
                self.state().geometry_shader = v;
            }
            "gx2:GX2SetFetchShader" => {
                let v = a(0);
                self.state().fetch_shader = v;
            }
            "gx2:GX2SetAttribBuffer" => {
                let (i, size, stride, address) = (a(0), a(1), a(2), a(3));
                if let Some(b) = self.state().attrib_buffers.get_mut(i as usize) {
                    *b = crate::state::AttribBuffer { address, size, stride };
                }
            }
            "gx2:GX2SetVertexUniformBlock" | "gx2:GX2SetPixelUniformBlock" | "gx2:GX2SetGeometryUniformBlock" => {
                let (i, size, address) = (a(0), a(1), a(2));
                let st = self.state();
                let blocks = if name.contains("Vertex") {
                    &mut st.vertex_blocks
                } else if name.contains("Geometry") {
                    &mut st.geometry_blocks
                } else {
                    &mut st.pixel_blocks
                };
                if let Some(b) = blocks.get_mut(i as usize) {
                    *b = crate::state::UniformBlock { address, size };
                }
            }
            "gx2:GX2SetPixelTexture" => {
                let (t, unit) = (a(0), a(1));
                if let Some(slot) = self.state().pixel_textures.get_mut(unit as usize) {
                    *slot = t;
                }
            }
            "gx2:GX2SetPixelSampler" => {
                let (s, unit) = (a(0), a(1));
                if let Some(slot) = self.state().pixel_samplers.get_mut(unit as usize) {
                    *slot = s;
                }
            }
            "gx2:GX2SetColorBuffer" => {
                let (cb, target) = (a(0), a(1));
                if let Some(slot) = self.state().colour_buffers.get_mut(target as usize) {
                    *slot = cb;
                }
            }
            "gx2:GX2SetDepthBuffer" => {
                let v = a(0);
                self.state().depth_buffer = v;
            }
            "gx2:GX2SetViewport" => {
                let v = [farg(c, 0), farg(c, 1), farg(c, 2), farg(c, 3), farg(c, 4), farg(c, 5)];
                self.state().viewport = v;
            }
            "gx2:GX2SetScissor" => {
                let v = [a(0), a(1), a(2), a(3)];
                self.state().scissor = v;
            }
            "gx2:GX2SetBlendControl" => {
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
            "gx2:GX2SetColorControl" => {
                let v = [a(0), a(1), a(2), a(3)];
                self.state().colour_control = v;
            }
            "gx2:GX2SetTargetChannelMasks" => {
                let mut m = 0;
                for k in 0..8 {
                    m |= (a(k) & 0xf) << (4 * k);
                }
                self.state().target_masks = m;
            }
            "gx2:GX2SetDepthOnlyControl" => {
                let (t, w, f) = (a(0) != 0, a(1) != 0, a(2));
                let st = self.state();
                (st.depth_test, st.depth_write, st.depth_func, st.stencil_test) = (t, w, f, false);
            }
            "gx2:GX2SetDepthStencilControl" => {
                let (t, w, f, s) = (a(0) != 0, a(1) != 0, a(2), a(3) != 0);
                let st = self.state();
                (st.depth_test, st.depth_write, st.depth_func, st.stencil_test) = (t, w, f, s);
            }
            "gx2:GX2SetCullOnlyControl" => {
                let (face, front, back) = (a(0), a(1) != 0, a(2) != 0);
                let st = self.state();
                (st.front_face, st.cull_front, st.cull_back) = (face, front, back);
            }
            "gx2:GX2SetPolygonControl" => {
                let (face, front, back) = (a(0), a(1) != 0, a(2) != 0);
                let st = self.state();
                (st.front_face, st.cull_front, st.cull_back) = (face, front, back);
            }
            "gx2:GX2SetAlphaTest" => {
                let (e, f, r) = (a(0) != 0, a(1), farg(c, 0));
                self.state().alpha_test = (e, f, r);
            }
            "gx2:GX2SetShaderModeEx" => {
                let mode = a(0);
                self.state().shader_mode = mode;
            }
            "gx2:GX2SetRasterizerClipControlEx" => {
                let half = a(2) != 0;
                self.state().half_z = half;
            }
            // the size of the code the fetch shader would be: sixteen bytes for each attribute and a little control flow
            "gx2:GX2CalcFetchShaderSizeEx" => c.r[3] = (32 + 16 * c.r[3]).div_ceil(16) * 16,
            // (fetch shader, buffer, attribute count, attributes, type, tessellation): nothing of machine code is made, the attributes
            // are kept and the draw reads them
            "gx2:GX2InitFetchShaderEx" => {
                let (fs, buffer, count, attribs) = (c.r[3], c.r[4], c.r[5], c.r[6]);
                let streams: Vec<_> = (0..count.min(32))
                    .map(|i| crate::draw::AttribStream::read(g, attribs + 32 * i))
                    .collect();
                g.fill(fs, 0, 0x28);
                g.put_u32(fs + 8, (32 + 16 * count).div_ceil(16) * 16);
                g.put_u32(fs + 0xc, buffer);
                g.put_u32(fs + 0x10, count);
                self.fetch_shaders.insert(fs, streams);
            }
            "gx2:GX2CopySurface" => self.copy_surface(g, c.r[3], c.r[6]),
            // (mode, address, size): the CPU wrote memory the GPU may have cached; a texture there is read again next time
            "gx2:GX2Invalidate" => {
                let (mode, address, size) = (a(0), a(1), a(2));
                if mode & 2 != 0 || mode & 0x40 != 0 {
                    self.invalidate_textures(address, size);
                }
            }
            "gx2:GX2InitSampler" => {
                let (s, clamp, filter) = (a(0), a(1) & 15, a(2) & 15);
                g.put_u32(s, clamp | clamp << 4 | clamp << 8 | filter << 12 | filter << 16);
                g.put_u32(s + 4, 0xf | (13 * 16) << 18);
                g.put_u32(s + 8, 0);
            }
            "gx2:GX2InitSamplerClamping" => {
                let (s, x, y, z) = (a(0), a(1) & 15, a(2) & 15, a(3) & 15);
                let w = g.u32(s) & !0xfff;
                g.put_u32(s, w | x | y << 4 | z << 8);
            }
            "gx2:GX2InitSamplerXYFilter" => {
                let (s, mag, min) = (a(0), a(1) & 15, a(2) & 15);
                let w = g.u32(s) & !(0xff << 12);
                g.put_u32(s, w | mag << 12 | min << 16);
            }
            "gx2:GX2InitSamplerZMFilter" => {
                let (s, z, mip) = (a(0), a(1) & 15, a(2) & 15);
                let w = g.u32(s) & !(0xff << 20);
                g.put_u32(s, w | z << 20 | mip << 24);
            }
            "gx2:GX2InitSamplerLOD" => {
                let s = a(0);
                let q = |v: f32| (v.clamp(0.0, 63.0) * 16.0) as u32 & 0x3ff;
                let w = g.u32(s + 4) & 0xff;
                g.put_u32(s + 4, w | q(farg(c, 0)) << 8 | q(farg(c, 1)) << 18);
                g.put_u32(s + 8, farg(c, 2).to_bits());
            }
            "gx2:GX2InitSamplerDepthCompare" => {
                let (s, func) = (a(0), a(1) & 15);
                let w = g.u32(s + 4) & !0xf;
                g.put_u32(s + 4, w | func);
            }
            "gx2:GX2ClearDepthStencilEx" => self.clear_depth(g, c.r[3], farg(c, 0), c.r[4], c.r[5]),
            "gx2:GX2DrawEx" => self.draw(g, a(0), a(1), None, a(2), a(3)),
            "gx2:GX2DrawIndexedEx" => self.draw(g, a(0), a(1), Some((a(2), a(3))), a(4), a(5)),
            "gx2:GX2ClearColor" => self.clear(g, c),
            "gx2:GX2CopyColorBufferToScanBuffer" => self.copy_to_scan_buffer(g, c),
            "gx2:GX2SwapScanBuffers" => {
                self.present();
                self.swaps += 1;
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
            "gx2:GX2WaitForVsync" => self.wait_for_vsync(),
            // (swap count, flip count, last flip time, last vsync time): every swap has flipped by now
            "gx2:GX2GetSwapStatus" => {
                for (p, v) in [(c.r[3], self.swaps), (c.r[4], self.swaps)] {
                    if p != 0 {
                        g.put_u32(p, v);
                    }
                }
                c.r[3] = 0;
            }
            "gx2:GX2SetSwapInterval" => self.interval = c.r[3],
            "gx2:GX2GetSwapInterval" => c.r[3] = self.interval,
            _ => {}
        }
    }

    /// Writes the shader's structure and program to the folder named by MH3U_GFX_DUMP (once per shader), for looking at offline.
    fn dump_shader(&mut self, g: &Guest, address: u32, vertex: bool) {
        let Some(dir) = std::env::var_os("MH3U_GFX_DUMP") else { return };
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
        self.draw_call(g, primitive, count, indexed, offset, instances);
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

impl Gx2Backend for Gx2 {
    fn implements(&self, name: &str) -> bool {
        CALLS.contains(&name)
    }

    fn call(&mut self, name: &str, c: &mut Ctx, g: &Guest) {
        match name {
            "gx2:GX2BeginDisplayListEx" => self.recording = Some((c.r[3], Vec::new())),
            "gx2:GX2EndDisplayList" => {
                if let Some((address, calls)) = self.recording.take() {
                    c.r[3] = 0x20 + 0x10 * calls.len() as u32;
                    self.lists.insert(address, calls);
                }
            }
            "gx2:GX2DirectCallDisplayList" => {
                let calls = self.lists.get(&c.r[3]).cloned().unwrap_or_default();
                for call in calls {
                    let mut replay = c.clone();
                    replay.r = call.r;
                    replay.f = call.f;
                    self.exec(&call.name, &mut replay, g, Some(&call.stack));
                }
            }
            _ => {
                if let Some((_, calls)) = self.recording.as_mut().filter(|_| !is_immediate(name)) {
                    let stack = (0..8).map(|k| g.u32(c.r[1].wrapping_add(8 + 4 * k))).collect();
                    calls.push(Recorded {
                        name: name.to_string(),
                        r: c.r,
                        f: c.f,
                        stack,
                    });
                } else {
                    self.exec(name, c, g, None);
                }
            }
        }
    }
}
