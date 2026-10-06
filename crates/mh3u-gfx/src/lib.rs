//! The graphics side: a window, a Vulkan (or Direct3D 12, or Metal) device through `wgpu`, and what the game's `GX2` calls become.

mod gx2;

pub use gx2::Gx2;

use anyhow::{Context, Result};
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// What a window and its GPU are: the device, the queue and the surface the game's frames are shown on.
pub struct Display {
    pub window: Arc<Window>,
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface: wgpu::Surface<'static>,
}

impl Display {
    fn new(window: Arc<Window>) -> Result<Display> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN | wgpu::Backends::DX12 | wgpu::Backends::METAL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance.create_surface(window.clone()).context("creating the window's surface")?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            ..Default::default()
        }))
        .context("no graphics adapter")?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).context("opening the graphics device")?;
        Ok(Display {
            window,
            instance,
            adapter,
            device,
            queue,
            surface,
        })
    }
}

struct App<F: FnOnce(Display) + Send + 'static> {
    start: Option<F>,
    failed: Option<anyhow::Error>,
}

impl<F: FnOnce(Display) + Send + 'static> ApplicationHandler for App<F> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let Some(start) = self.start.take() else { return };
        let attributes = Window::default_attributes()
            .with_title("Monster Hunter 3 Ultimate")
            .with_inner_size(winit::dpi::LogicalSize::new(1280, 720));
        let made = event_loop
            .create_window(attributes)
            .context("opening the window")
            .and_then(|w| Display::new(Arc::new(w)));
        match made {
            Ok(display) => {
                std::thread::spawn(move || start(display));
            }
            Err(e) => {
                self.failed = Some(e);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            event_loop.exit();
        }
    }
}

/// Opens the window on this thread (the one that called `main`), then calls `start` with the display on a new thread, where the game
/// runs. Returns when the window is closed.
pub fn run<F: FnOnce(Display) + Send + 'static>(start: F) -> Result<()> {
    let event_loop = EventLoop::new().context("starting the window system")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        start: Some(start),
        failed: None,
    };
    event_loop.run_app(&mut app).context("running the window")?;
    match app.failed {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
