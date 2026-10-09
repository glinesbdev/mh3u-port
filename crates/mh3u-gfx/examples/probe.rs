//! Opens the window, says which GPU and API it got, shows a colour for a few seconds and exits.
fn main() -> anyhow::Result<()> {
    mh3u_gfx::run(|d| {
        let info = d.adapter.get_info();
        println!("adapter: {} ({:?}, {:?})", info.name, info.backend, info.device_type);
        let (window, surface) = (d.window.as_ref().expect("a window"), d.surface.as_ref().expect("a surface"));
        let size = window.inner_size();
        let caps = surface.get_capabilities(&d.adapter);
        let format = caps.formats[0];
        println!("surface {}x{} {:?} modes {:?}", size.width, size.height, format, caps.present_modes);
        surface.configure(
            &d.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                width: size.width.max(1),
                height: size.height.max(1),
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode: caps.alpha_modes[0],
                view_formats: vec![],
                color_space: wgpu::SurfaceColorSpace::Auto,
            },
        );
        for i in 0..180 {
            let frame = surface.get_current_texture();
            let wgpu::CurrentSurfaceTexture::Success(frame) = frame else {
                println!("no frame: {:?}", "x");
                break;
            };
            let view = frame.texture.create_view(&Default::default());
            let mut enc = d.device.create_command_encoder(&Default::default());
            {
                let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: f64::from(i) / 180.0,
                                g: 0.2,
                                b: 0.4,
                                a: 1.0,
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
            d.queue.submit([enc.finish()]);
            d.queue.present(frame);
        }
        println!("done");
        std::process::exit(0);
    })
}
