//! The graphics side: a window, a Vulkan (or Direct3D 12, or Metal) device through `wgpu`, and what the game's `GX2` calls become.

mod decode;
mod draw;
#[cfg(test)]
mod draw_bench;
pub mod events;
mod geometry;
mod gx2;
pub mod kbm;
pub mod overlay;
mod perf;
mod pipelines;
mod report;
mod shader_cache;
mod state;
mod texture;
mod translations;

pub use gx2::Gx2;

/// The commit this build was made from ("+changes" when the working tree differed), printed at start and written into frame dumps.
pub const BUILD: &str = env!("MH3U_BUILD_COMMIT");

/// A hash of the translator's and the graphics code's sources (see `build.rs`): what the on-disk caches are valid for.
pub const SHADER_SOURCE: &str = env!("MH3U_SHADER_SOURCE");

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
    /// The window and the overlay's input (egui-winit), when the overlay is on.
    window: Option<Arc<Window>>,
    overlay: Option<egui_winit::State>,
    /// Borderless fullscreen on the window's monitor (F11 or Alt+Enter toggles it), whether Alt is held, and whether the cursor shows.
    fullscreen: bool,
    alt: bool,
    cursor_shown: bool,
    /// Keyboard and mouse controls (none with `MH3U_KBM=off`), whether the window has the focus, and whether the cursor is grabbed.
    kbm: Option<kbm::Bindings>,
    focused: bool,
    grabbed: bool,
}

/// Whether fullscreen is asked for: `--fullscreen`, or `MH3U_FULLSCREEN=1`.
pub fn fullscreen_wanted(flag: bool, env: Option<&str>) -> bool {
    flag || matches!(env.map(str::trim), Some("1" | "on" | "true"))
}

/// Whether a key press toggles fullscreen: F11, or Enter with Alt held (not F1, the overlay's key). Such presses never reach the game.
pub fn is_fullscreen_toggle(code: winit::keyboard::KeyCode, alt: bool) -> bool {
    use winit::keyboard::KeyCode;
    code == KeyCode::F11 || (alt && matches!(code, KeyCode::Enter | KeyCode::NumpadEnter))
}

/// Where a window event goes: to egui (which hears everything but the overlay's key, so that it knows the window's size and scale
/// before it opens), to the game (not the keyboard and mouse while the overlay is open, nor the overlay's key), and whether it toggles
/// the overlay (`f1`: whether this is the F1 key, and then whether a first press: a key held down repeats, and its release also comes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub to_egui: bool,
    pub to_game: bool,
    pub toggle: bool,
}

pub fn route_event(overlay: bool, open: bool, f1: Option<bool>, input: bool) -> Route {
    match (overlay, f1) {
        (false, _) => Route {
            to_egui: false,
            to_game: true,
            toggle: false,
        },
        (true, Some(first_press)) => Route {
            to_egui: false,
            to_game: false,
            toggle: first_press,
        },
        (true, None) => Route {
            to_egui: true,
            to_game: !(open && input),
            toggle: false,
        },
    }
}

/// Whether an event is the player's keyboard or mouse (what the overlay takes while open).
fn is_input(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::KeyboardInput { .. }
            | WindowEvent::Ime(_)
            | WindowEvent::ModifiersChanged(_)
            | WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::Touch(_)
    )
}

/// The cursor shows in a window, and in fullscreen only while the overlay is open (for its mouse).
pub fn cursor_visible(fullscreen: bool, overlay_open: bool) -> bool {
    !fullscreen || overlay_open
}

fn borderless(on: bool) -> Option<winit::window::Fullscreen> {
    // `None` as the monitor: the one the window is on
    on.then_some(winit::window::Fullscreen::Borderless(None))
}

impl<F: FnOnce(Display) + Send + 'static> App<F> {
    /// Passes the keys and mouse buttons held on to the input state: the GamePad's buttons and the left stick (keyboard and mouse
    /// controls; nothing with them off).
    fn keys_changed(&self) {
        use mh3u_rt::input::INPUT;
        let (buttons, (x, y)) = self
            .kbm
            .as_ref()
            .map_or((0, (0.0, 0.0)), |b| (b.buttons(&self.keys), b.left_stick(&self.keys)));
        INPUT.set_keyboard_buttons(buttons);
        // the native menu's mod key (MH3U_NATIVE_MENU_KEY; docs/native-menu.md)
        mh3u_rt::native_menu::HELD.store(
            self.keys.contains(mh3u_rt::native_menu::key()),
            std::sync::atomic::Ordering::Relaxed,
        );
        for (i, v) in [x, y, 0.0, 0.0].into_iter().enumerate() {
            INPUT.set_keyboard_axis(i, v);
        }
    }

    /// Grabs the cursor (kept in the window and hidden) while playing with keyboard and mouse, lets it go otherwise, and shows it
    /// when it is not grabbed (except in fullscreen with the overlay closed).
    fn update_cursor(&mut self) {
        let open = mh3u_rt::input::OVERLAY_OPEN.load(std::sync::atomic::Ordering::Relaxed);
        let Some(w) = self.window.clone() else { return };
        let grab = kbm::grab(self.kbm.is_some(), self.focused, open);
        if grab != self.grabbed {
            use winit::window::CursorGrabMode;
            let done = if grab {
                w.set_cursor_grab(CursorGrabMode::Locked)
                    .or_else(|_| w.set_cursor_grab(CursorGrabMode::Confined))
            } else {
                w.set_cursor_grab(CursorGrabMode::None)
            };
            if let Err(e) = done {
                eprintln!("[window] the cursor could not be {}: {e}", if grab { "grabbed" } else { "let go" });
            }
            self.grabbed = grab;
            if !grab {
                // what the mouse held is let go
                self.keys.retain(|k| !k.starts_with("Mouse"));
                self.keys_changed();
            }
        }
        let show = !grab && cursor_visible(self.fullscreen, open);
        if show != self.cursor_shown {
            w.set_cursor_visible(show);
            self.cursor_shown = show;
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
            .with_inner_size(winit::dpi::LogicalSize::new(1280, 720))
            .with_fullscreen(borderless(self.fullscreen));
        self.size = (1280.0, 720.0);
        let made = event_loop
            .create_window(attributes)
            .context("opening the window")
            .and_then(|w| Display::new(Arc::new(w)));
        match made {
            Ok(display) => {
                if let Some(w) = display.window.clone() {
                    if overlay::enabled() {
                        self.overlay = Some(egui_winit::State::new(
                            overlay::CONTEXT.clone(),
                            egui::ViewportId::ROOT,
                            &w,
                            Some(w.scale_factor() as f32),
                            None,
                            None,
                        ));
                    }
                    self.window = Some(w);
                }
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
        if let WindowEvent::ModifiersChanged(m) = &event {
            self.alt = m.state().alt_key();
        }
        if let WindowEvent::Focused(f) = event {
            self.focused = f;
        }
        // the cursor follows the focus, fullscreen and the overlay (which the gamepad can open too, so this looks at every event)
        self.update_cursor();
        // F9 / F10: the hook-finding tools' keys (docs/hooks.md); they also reach the game, which does not use them
        if let WindowEvent::KeyboardInput { event: k, .. } = &event
            && let PhysicalKey::Code(code) = k.physical_key
            && k.state == ElementState::Pressed
            && !k.repeat
        {
            use winit::keyboard::KeyCode;
            let key = match code {
                KeyCode::F9 => Some(&mh3u_rt::mods::KEY_F9),
                KeyCode::F10 => Some(&mh3u_rt::mods::KEY_F10),
                _ => None,
            };
            if let Some(key) = key {
                let n = key.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                eprintln!("[keys] {code:?} pressed ({n})");
            }
        }
        if let WindowEvent::KeyboardInput { event: k, .. } = &event
            && let PhysicalKey::Code(code) = k.physical_key
            && is_fullscreen_toggle(code, self.alt)
        {
            if k.state == ElementState::Pressed
                && !k.repeat
                && let Some(w) = &self.window
            {
                self.fullscreen = !self.fullscreen;
                w.set_fullscreen(borderless(self.fullscreen));
                eprintln!("[window] {}", if self.fullscreen { "borderless fullscreen" } else { "windowed" });
            }
            // the surface follows the new size at the next present; the picture keeps its proportions (bars at the sides or top)
            return;
        }
        let open = mh3u_rt::input::OVERLAY_OPEN.load(std::sync::atomic::Ordering::Relaxed);
        let f1 = match &event {
            WindowEvent::KeyboardInput { event: k, .. } if k.physical_key == PhysicalKey::Code(winit::keyboard::KeyCode::F1) => {
                Some(k.state == ElementState::Pressed && !k.repeat)
            }
            _ => None,
        };
        let route = route_event(self.overlay.is_some(), open, f1, is_input(&event));
        if let (Some(state), Some(w)) = (self.overlay.as_mut(), self.window.as_ref())
            && route.to_egui
        {
            // egui hears every event but the toggle key (size, scale and focus too while closed, so they are right when it opens)
            let _ = state.on_window_event(w, &event);
            overlay::push_input(state.take_egui_input(w));
        }
        if route.toggle {
            mh3u_rt::input::OVERLAY_TOGGLE.store(true, std::sync::atomic::Ordering::Relaxed);
            // what the game held is let go, so nothing stays pressed while the overlay is open
            self.keys.clear();
            self.mouse_down = false;
            self.keys_changed();
            self.touch_changed();
        }
        if !route.to_game {
            match event {
                WindowEvent::CloseRequested => event_loop.exit(),
                WindowEvent::Resized(size) => self.size = (size.width as f32, size.height as f32),
                _ => {}
            }
            return;
        }
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
            WindowEvent::MouseInput { state, button, .. } if self.kbm.is_some() => {
                // the mouse's buttons are GamePad buttons, while the cursor is grabbed (the click that gives the window the focus
                // is not one); a release always counts
                if let Some(name) = kbm::mouse_name(button) {
                    if state == ElementState::Pressed && self.grabbed {
                        self.keys.insert(name.to_string());
                    } else if state == ElementState::Released {
                        self.keys.remove(name);
                    }
                    self.keys_changed();
                }
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

    /// The mouse's raw motion: the right stick, while the cursor is grabbed.
    fn device_event(&mut self, _: &ActiveEventLoop, _: winit::event::DeviceId, event: winit::event::DeviceEvent) {
        if let winit::event::DeviceEvent::MouseMotion { delta } = event
            && self.grabbed
        {
            mh3u_rt::input::INPUT
                .mouse
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .add(delta.0 as f32, delta.1 as f32);
        }
    }
}

/// Opens the window on this thread (the one that called `main`; borderless fullscreen with `fullscreen`), then calls `start` with the display on a new thread, where the game
/// runs. Returns when the window is closed.
/// `bindings` is the keyboard and mouse bindings' file (made with the defaults when missing); none, or `MH3U_KBM=off`, leaves the
/// keyboard and mouse without controls (the mouse is then the GamePad's touch screen).
pub fn run<F: FnOnce(Display) + Send + 'static>(fullscreen: bool, bindings: Option<std::path::PathBuf>, start: F) -> Result<()> {
    let kbm = bindings
        .filter(|_| kbm::enabled(std::env::var("MH3U_KBM").ok().as_deref()))
        .map(|path| {
            let (b, note) = kbm::load_or_create(&path);
            if let Some(note) = note {
                eprintln!("[input] {note}");
            }
            eprintln!(
                "[input] keyboard and mouse controls on ({}; MH3U_KBM=off turns them off)",
                path.display()
            );
            b.mouse_stick(&mut mh3u_rt::input::INPUT.mouse.lock().unwrap_or_else(|e| e.into_inner()));
            *overlay::CONTROLS.lock().unwrap_or_else(|e| e.into_inner()) = b.lines();
            b
        });
    let event_loop = EventLoop::new().context("starting the window system")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        start: Some(start),
        failed: None,
        keys: Default::default(),
        size: (1280.0, 720.0),
        cursor: (0.0, 0.0),
        mouse_down: false,
        window: None,
        overlay: None,
        fullscreen,
        alt: false,
        cursor_shown: true,
        kbm,
        focused: true,
        grabbed: false,
    };
    event_loop.run_app(&mut app).context("running the window")?;
    match app.failed {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(test)]
mod window_tests {
    use super::*;
    use winit::keyboard::KeyCode;

    #[test]
    fn fullscreen_is_asked_for_by_the_flag_or_the_variable_and_toggled_by_f11_or_alt_enter() {
        assert!(fullscreen_wanted(true, None));
        assert!(fullscreen_wanted(false, Some("1")));
        assert!(!fullscreen_wanted(false, Some("0")));
        assert!(!fullscreen_wanted(false, None));
        assert!(is_fullscreen_toggle(KeyCode::F11, false));
        assert!(is_fullscreen_toggle(KeyCode::Enter, true));
        assert!(is_fullscreen_toggle(KeyCode::NumpadEnter, true));
        assert!(!is_fullscreen_toggle(KeyCode::Enter, false), "Enter alone is the game's");
        assert!(!is_fullscreen_toggle(KeyCode::F1, false), "F1 is the overlay's");
        assert!(!is_fullscreen_toggle(KeyCode::F1, true));
    }

    #[test]
    fn f1_toggles_on_its_first_press_only_and_the_open_overlay_takes_the_keyboard_and_mouse() {
        let r = |open, f1, input| route_event(true, open, f1, input);
        // F1: pressed once toggles; held (repeats) and let go do nothing; never to the game or egui
        assert!(r(false, Some(true), true).toggle);
        assert!(r(true, Some(true), true).toggle);
        assert!(!r(true, Some(false), true).toggle);
        assert!(!r(false, Some(true), true).to_game && !r(false, Some(true), true).to_egui);
        // open: typing goes to egui only
        assert_eq!(
            r(true, None, true),
            Route {
                to_egui: true,
                to_game: false,
                toggle: false
            }
        );
        // closed: input goes to the game, and egui still hears it (for the size, scale and focus)
        assert_eq!(
            r(false, None, true),
            Route {
                to_egui: true,
                to_game: true,
                toggle: false
            }
        );
        // a resize while open reaches both (the game's surface follows the window)
        assert!(r(true, None, false).to_game);
        // no overlay: all to the game, F1 included
        assert_eq!(
            route_event(false, false, Some(true), true),
            Route {
                to_egui: false,
                to_game: true,
                toggle: false
            }
        );
    }

    #[test]
    fn the_cursor_hides_in_fullscreen_unless_the_overlay_is_open() {
        assert!(cursor_visible(false, false));
        assert!(!cursor_visible(true, false));
        assert!(cursor_visible(true, true));
    }
}
