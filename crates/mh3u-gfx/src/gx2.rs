//! The game's `GX2` calls, over `wgpu`. For now: colour buffers as textures, clears, and showing the TV picture in the window with the
//! console's frame pacing. Draws, shaders and textures are the next part.

use crate::Display;
use mh3u_rt::{Ctx, Guest, Gx2Backend};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The names this backend handles.
const CALLS: &[&str] = &[
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

struct Target {
    texture: wgpu::Texture,
    format: wgpu::TextureFormat,
}

pub struct Gx2 {
    d: Display,
    config: wgpu::SurfaceConfiguration,
    blit: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// The colour buffers the game has used, by the guest address of their image.
    targets: HashMap<u32, Target>,
    /// What the TV shows: a copy of the colour buffer given to `GX2CopyColorBufferToScanBuffer`.
    tv: Option<wgpu::Texture>,
    swaps: u32,
    interval: u32,
    next_vsync: Instant,
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
            d,
            config,
            blit,
            layout,
            sampler,
            targets: HashMap::new(),
            tv: None,
            swaps: 0,
            interval: 1,
            next_vsync: Instant::now(),
        }
    }

    /// The texture of the colour buffer at `cb` (a `GX2ColorBuffer`, which begins with its `GX2Surface`), made when first seen.
    fn target(&mut self, g: &Guest, cb: u32) -> Option<u32> {
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

    fn clear(&mut self, g: &Guest, c: &Ctx) {
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
        if c.r[4] != TV {
            return;
        }
        let Some(image) = self.target(g, c.r[3]) else { return };
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
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
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

    /// Shows the TV picture, scaled to the window with the picture's proportions kept.
    fn present(&mut self) {
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

impl Gx2Backend for Gx2 {
    fn implements(&self, name: &str) -> bool {
        CALLS.contains(&name)
    }

    fn call(&mut self, name: &str, c: &mut Ctx, g: &Guest) {
        match name {
            "gx2:GX2ClearColor" => self.clear(g, c),
            "gx2:GX2CopyColorBufferToScanBuffer" => self.copy_to_scan_buffer(g, c),
            "gx2:GX2SwapScanBuffers" => {
                self.present();
                self.swaps += 1;
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
}
