//! A synthetic frame of draws on wgpu's no-op device, for timing the CPU side of the draw path (`cargo test -p mh3u-gfx --release
//! draw_path_costs -- --nocapture`; `MH3U_BENCH_FRAMES` sets how many frames, 20 by default). The frame is shaped like the village's
//! (about 190 draws, three or four uniform blocks each, some of them read by index, and vertex and index data that do not change), so
//! the times per draw can be compared with the `[draw times]` of a run.

use crate::draw::{AttribStream, DrawIndices};
use crate::gx2::{Call, blank_ctx};
use crate::state::{AttribBuffer, State, UniformBlock};
use crate::{Display, Gx2};
use mh3u_latte::translate::DYNAMIC_BANK;
use mh3u_rt::{Guest, Gx2Backend};
use std::time::{Duration, Instant};

const DRAWS: usize = 190;
const OBJECTS: usize = 24;
const CAMERA: u32 = 0x1_0000;
const OBJECT_BLOCKS: u32 = 0x2_0000;
const SKIN_BLOCKS: u32 = 0x10_0000;
const PIXEL_BLOCKS: u32 = 0x8_0000;
const VERTICES: u32 = 0x40_0000;
const INDICES: u32 = 0x60_0000;
/// Each object's vertex data: 800 vertices of 32 bytes.
const VERTEX_COUNT: u32 = 800;
const STRIDE: u32 = 32;
/// Each draw's index list: 1200 16-bit big-endian indices.
const INDEX_COUNT: u32 = 1200;
/// The skinning palettes' size: 48 matrices of three vec4 (2304 bytes), in a bank the shader reads by index (4096 vec4 declared).
const SKIN_SIZE: u32 = 48 * 48;

fn layout(d: &wgpu::Device, bindings: &[u32]) -> wgpu::BindGroupLayout {
    let entries: Vec<wgpu::BindGroupLayoutEntry> = bindings
        .iter()
        .map(|&binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        })
        .collect();
    d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &entries,
    })
}

#[derive(Default)]
struct Times {
    uniforms: Duration,
    indices: Duration,
    streams: Duration,
    flush: Duration,
}

#[test]
fn draw_path_costs() {
    let frames: usize = std::env::var("MH3U_BENCH_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
        .max(2);
    let mut gx2 = Gx2::new(Display::noop());
    let mut memory = vec![0u8; 8 << 20];
    let g = Guest::new(memory.as_mut_ptr());
    // vertex data and index lists (the same every frame)
    for o in 0..OBJECTS as u32 {
        let bytes: Vec<u8> = (0..VERTEX_COUNT * STRIDE).map(|k| (k.wrapping_mul(31) ^ o) as u8).collect();
        g.write(VERTICES + o * VERTEX_COUNT * STRIDE, &bytes);
    }
    for d in 0..DRAWS as u32 {
        let indices: Vec<u8> = (0..INDEX_COUNT)
            .flat_map(|k| (((k * 7 + d) % VERTEX_COUNT) as u16).to_be_bytes())
            .collect();
        g.write(INDICES + d * INDEX_COUNT * 2, &indices);
    }
    let fetch = [
        AttribStream {
            location: 0,
            buffer: 0,
            offset: 0,
            format: 0x11,
            per_instance: false,
            divisor: 0,
            select: [0, 1, 2, 5],
            endian: 2,
        },
        AttribStream {
            location: 1,
            buffer: 0,
            offset: 12,
            format: 0x0d,
            per_instance: false,
            divisor: 0,
            select: [0, 1, 4, 5],
            endian: 2,
        },
        AttribStream {
            location: 2,
            buffer: 0,
            offset: 20,
            format: 0x0a,
            per_instance: false,
            divisor: 0,
            select: [0, 1, 2, 3],
            endian: 0,
        },
    ];
    let device = gx2.d.device.clone();
    let (vs3, vs2, ps1) = (layout(&device, &[0, 1, 2]), layout(&device, &[0, 1]), layout(&device, &[0]));
    let mut t = Times::default();
    for frame in 0..frames {
        // the camera and the objects' matrices move every frame; the pixel constants stay
        g.write(CAMERA, &[frame as u8; 256]);
        for o in 0..OBJECTS as u32 {
            g.write(OBJECT_BLOCKS + o * 1024, &[(frame as u32 ^ o) as u8; 1024]);
            g.write(SKIN_BLOCKS + o * 0x1_0000, &vec![(frame as u32 + o) as u8; SKIN_SIZE as usize]);
            g.write(PIXEL_BLOCKS + o * 256, &[o as u8; 256]);
        }
        let mut frame_times = Times::default();
        for d in 0..DRAWS {
            let o = (d * OBJECTS / DRAWS) as u32;
            let skinned = o.is_multiple_of(2);
            let mut st = State::default();
            st.vertex_blocks[0] = UniformBlock {
                address: CAMERA,
                size: 256,
            };
            st.vertex_blocks[1] = UniformBlock {
                address: OBJECT_BLOCKS + o * 1024,
                size: 1024,
            };
            st.vertex_blocks[2] = UniformBlock {
                address: SKIN_BLOCKS + o * 0x1_0000,
                size: SKIN_SIZE,
            };
            st.pixel_blocks[0] = UniformBlock {
                address: PIXEL_BLOCKS + o * 256,
                size: 256,
            };
            st.attrib_buffers[0] = AttribBuffer {
                address: VERTICES + o * VERTEX_COUNT * STRIDE,
                size: VERTEX_COUNT * STRIDE,
                stride: STRIDE,
            };
            let at = Instant::now();
            let vs_banks: &[(u32, u32)] = if skinned {
                &[(0, 16), (1, 64), (2, DYNAMIC_BANK)]
            } else {
                &[(0, 16), (1, 64)]
            };
            let vs_entries: Vec<_> = vs_banks
                .iter()
                .map(|&(bank, len)| (bank, gx2.uniform_buffer(&g, &st.vertex_blocks[bank as usize], len).bound()))
                .collect();
            let ps_entries = vec![(0, gx2.uniform_buffer(&g, &st.pixel_blocks[0], 16).bound())];
            let _ = gx2.bind_group(if skinned { &vs3 } else { &vs2 }, vs_entries, true);
            let _ = gx2.bind_group(&ps1, ps_entries, true);
            let after_uniforms = Instant::now();
            let DrawIndices { lowest, vertices, .. } = gx2
                .draw_indices(&g, None, 4, INDEX_COUNT, Some((4, INDICES + d as u32 * INDEX_COUNT * 2)), 0)
                .expect("indices");
            let after_indices = Instant::now();
            let _ = gx2.cached_streams(&g, &st, &fetch, lowest, vertices, 1);
            let after_streams = Instant::now();
            frame_times.uniforms += after_uniforms - at;
            frame_times.indices += after_indices - after_uniforms;
            frame_times.streams += after_streams - after_indices;
        }
        let at = Instant::now();
        gx2.flush_draws();
        frame_times.flush = at.elapsed();
        gx2.swaps += 1;
        // the first frame fills the caches
        if frame > 0 {
            t.uniforms += frame_times.uniforms;
            t.indices += frame_times.indices;
            t.streams += frame_times.streams;
            t.flush += frame_times.flush;
        }
    }
    let n = (frames - 1) as f64;
    let ms = |d: Duration| d.as_secs_f64() * 1000.0 / n;
    let us = |d: Duration| d.as_secs_f64() * 1e6 / n / DRAWS as f64;
    eprintln!("[bench] {DRAWS} draws per frame, {} frames", frames - 1);
    eprintln!(
        "[bench] uniforms and groups 0, 1: {:.3} ms per frame, {:.2} us per draw",
        ms(t.uniforms),
        us(t.uniforms)
    );
    eprintln!(
        "[bench] index list:               {:.3} ms per frame, {:.2} us per draw",
        ms(t.indices),
        us(t.indices)
    );
    eprintln!(
        "[bench] vertex streams:           {:.3} ms per frame, {:.2} us per draw",
        ms(t.streams),
        us(t.streams)
    );
    eprintln!("[bench] flush (uploads):          {:.3} ms per frame", ms(t.flush));
}

/// Playing back a display list of state calls only (unit settings, invalidations, uniform blocks), as the village's lists are mostly.
#[test]
fn display_list_replay_costs() {
    let frames: usize = std::env::var("MH3U_BENCH_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
        .max(2);
    let mut gx2 = Gx2::new(Display::noop());
    let mut memory = vec![0u8; 1 << 20];
    let g = Guest::new(memory.as_mut_ptr());
    let mut c = blank_ctx();
    c.r[1] = 0x8000;
    let list = 0x1_0000;
    let mut call = |gx2: &mut Gx2, c: &mut Ctx3, call: Call| gx2.call_id(Some(call as u32), call.name(), c, &g);
    c.r[3] = list;
    call(&mut gx2, &mut c, Call::BeginDisplayListEx);
    gx2_setup(&mut gx2, &mut c, &mut call);
    let calls = 3000u32;
    for k in 0..calls {
        let (what, a, b, d) = match k % 6 {
            0 | 1 => (Call::SetPixelTexture, 0x2_0000 + k * 0x100, k % 18, 0),
            2 | 3 => (Call::SetPixelSampler, 0x3_0000 + k * 0x10, k % 18, 0),
            4 => (Call::Invalidate, 0x2, 0x4_0000 + k, 0x40),
            _ => (Call::SetVertexUniformBlock, k % 16, 0x100, 0x5_0000 + k * 0x100),
        };
        c.r[3] = a;
        c.r[4] = b;
        c.r[5] = d;
        call(&mut gx2, &mut c, what);
    }
    call(&mut gx2, &mut c, Call::EndDisplayList);
    let at = Instant::now();
    for _ in 0..frames {
        c.r[3] = list;
        call(&mut gx2, &mut c, Call::DirectCallDisplayList);
    }
    let took = at.elapsed();
    eprintln!(
        "[bench] display list of {calls} state calls played back: {:.3} ms per list, {:.1} ns per call",
        took.as_secs_f64() * 1000.0 / frames as f64,
        took.as_secs_f64() * 1e9 / frames as f64 / f64::from(calls)
    );
}

type Ctx3 = mh3u_rt::Ctx;

/// A context state for the calls to go to.
fn gx2_setup(gx2: &mut Gx2, c: &mut Ctx3, call: &mut impl FnMut(&mut Gx2, &mut Ctx3, Call)) {
    c.r[3] = 0x6_0000;
    call(gx2, c, Call::SetupContextStateEx);
}

/// What wgpu's own bookkeeping costs as GPU buffers pile up (`cargo test -p mh3u-gfx --release live_buffers_cost -- --nocapture`): a
/// frame of 60 render passes and one submit, with few buffers alive and with as many as run 12 had (31,000 vertex streams, two buffers
/// each). Each pass's usage scope is sized by the most buffers ever alive, so the cost per pass follows that number, not the draws.
#[test]
fn live_buffers_cost() {
    let display = Display::noop();
    let (device, queue) = (&display.device, &display.queue);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let frame = || {
        let started = Instant::now();
        for _ in 0..10 {
            let mut enc = device.create_command_encoder(&Default::default());
            for _ in 0..60 {
                let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
            queue.submit([enc.finish()]);
        }
        started.elapsed() / 10
    };
    let mut buffers = Vec::new();
    let mut times = Vec::new();
    for alive in [1_000usize, 62_000] {
        while buffers.len() < alive {
            buffers.push(device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 256,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        frame();
        let t = frame();
        println!(
            "[bench] {alive} buffers alive: a frame of 60 passes and a submit, {:.3} ms",
            t.as_secs_f64() * 1e3
        );
        times.push(t);
    }
    // and after most of them are let go (the indices freed are used again, so new buffers do not push the size further)
    buffers.truncate(1_000);
    frame();
    let t = frame();
    println!("[bench] back to 1000 buffers alive: {:.3} ms", t.as_secs_f64() * 1e3);
    assert_eq!(times.len(), 2);
}
