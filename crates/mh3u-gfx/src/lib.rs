//! The graphics side: a window, a Vulkan (or Direct3D 12, or Metal) device through `wgpu`, and what the game's `GX2` calls become.

mod draw;
#[cfg(test)]
mod draw_bench;
mod geometry;
mod gx2;
mod report;
mod shader_cache;
mod state;
mod texture;

pub use gx2::Gx2;

use anyhow::{Context, Result};
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// What a window and its GPU are: the device, the queue and the surface the game's frames are shown on. The tests' displays have no
/// window and no surface (a device that does nothing, for timing the work on the CPU).
pub struct Display {
    pub window: Option<Arc<Window>>,
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface: Option<wgpu::Surface<'static>>,
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
        // the game's shaders read many buffers, use compressed textures and filter 32-bit float ones: ask for what the adapter has
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: adapter.limits(),
            required_features: adapter.features() & (wgpu::Features::TEXTURE_COMPRESSION_BC | wgpu::Features::FLOAT32_FILTERABLE),
            ..Default::default()
        }))
        .context("opening the graphics device")?;
        // a validation error in one draw is reported, and the game goes on
        device.on_uncaptured_error(Arc::new(|e| eprintln!("[gfx] {e}")));
        Ok(Display {
            window: Some(window),
            instance,
            adapter,
            device,
            queue,
            surface: Some(surface),
        })
    }

    /// A display with wgpu's no-op backend and no window: everything is validated, nothing is drawn (for the tests).
    #[cfg(test)]
    pub(crate) fn noop() -> Display {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::NOOP,
            backend_options: wgpu::BackendOptions {
                // the limits every adapter has (as strict as a real device about alignments and counts)
                noop: wgpu::NoopBackendOptions {
                    limits: Some(wgpu::Limits::default()),
                    ..wgpu::NoopBackendOptions::enabled()
                },
                ..Default::default()
            },
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).expect("the no-op adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: adapter.limits(),
            required_features: adapter.features() & (wgpu::Features::TEXTURE_COMPRESSION_BC | wgpu::Features::FLOAT32_FILTERABLE),
            ..Default::default()
        }))
        .expect("the no-op device");
        Display {
            window: None,
            instance,
            adapter,
            device,
            queue,
            surface: None,
        }
    }
}

struct App<F: FnOnce(Display) + Send + 'static> {
    start: Option<F>,
    failed: Option<anyhow::Error>,
    /// The keys held (by position, named as `winit` does), the window's size and where the mouse is: what the game's input needs.
    keys: std::collections::HashSet<String>,
    size: (f32, f32),
    cursor: (f32, f32),
    mouse_down: bool,
}

impl<F: FnOnce(Display) + Send + 'static> App<F> {
    /// Passes the keys on to the input state: buttons, and the sticks' axes (two keys of one axis cancel).
    fn keys_changed(&self) {
        use mh3u_rt::input::{INPUT, key_axis, key_button};
        for code in [
            "Enter",
            "Backspace",
            "KeyX",
            "KeyC",
            "KeyI",
            "KeyK",
            "KeyJ",
            "KeyL",
            "KeyQ",
            "KeyE",
            "Digit1",
            "Digit3",
            "KeyP",
            "KeyO",
            "Home",
            "ShiftLeft",
            "ShiftRight",
        ] {
            if let Some(mask) = key_button(code) {
                INPUT.set_keyboard_button(mask, self.keys.contains(code));
            }
        }
        let mut axes = [0.0f32; 4];
        for code in &self.keys {
            if let Some((axis, v)) = key_axis(code) {
                axes[axis] += v;
            }
        }
        for (i, v) in axes.iter().enumerate() {
            INPUT.set_keyboard_axis(i, *v);
        }
    }

    fn touch_changed(&self) {
        let (w, h) = self.size;
        let at = (self.mouse_down && w > 0.0 && h > 0.0).then(|| {
            let (tx, ty) = mh3u_rt::input::TOUCH_SIZE;
            ((self.cursor.0 / w * tx).clamp(0.0, tx), (self.cursor.1 / h * ty).clamp(0.0, ty))
        });
        mh3u_rt::input::INPUT.set_touch(at);
    }
}

impl<F: FnOnce(Display) + Send + 'static> ApplicationHandler for App<F> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let Some(start) = self.start.take() else { return };
        let attributes = Window::default_attributes()
            .with_title("Monster Hunter 3 Ultimate")
            .with_inner_size(winit::dpi::LogicalSize::new(1280, 720));
        self.size = (1280.0, 720.0);
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
        use winit::event::{ElementState, MouseButton};
        use winit::keyboard::PhysicalKey;
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    let name = format!("{code:?}");
                    if event.state == ElementState::Pressed {
                        self.keys.insert(name);
                    } else {
                        self.keys.remove(&name);
                    }
                    self.keys_changed();
                }
            }
            WindowEvent::Focused(false) => {
                self.keys.clear();
                self.mouse_down = false;
                self.keys_changed();
                self.touch_changed();
            }
            WindowEvent::Resized(size) => self.size = (size.width as f32, size.height as f32),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as f32, position.y as f32);
                self.touch_changed();
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                self.mouse_down = state == ElementState::Pressed;
                self.touch_changed();
            }
            _ => {}
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
        keys: Default::default(),
        size: (1280.0, 720.0),
        cursor: (0.0, 0.0),
        mouse_down: false,
    };
    event_loop.run_app(&mut app).context("running the window")?;
    match app.failed {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
