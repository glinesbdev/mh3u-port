//! What the player is pressing: the keyboard and the mouse (reported by the window) and gamepads (read here), combined into the
//! state the game's `VPADRead` sees: one GamePad's buttons, two sticks and a touch point.
//!
//! The state lives in one place (`INPUT`) because the window, the gamepad thread and the game all reach it from different threads.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

/// The bits of `VPADStatus::hold`.
pub mod button {
    pub const A: u32 = 0x8000;
    pub const B: u32 = 0x4000;
    pub const X: u32 = 0x2000;
    pub const Y: u32 = 0x1000;
    pub const LEFT: u32 = 0x0800;
    pub const RIGHT: u32 = 0x0400;
    pub const UP: u32 = 0x0200;
    pub const DOWN: u32 = 0x0100;
    pub const ZL: u32 = 0x0080;
    pub const ZR: u32 = 0x0040;
    pub const L: u32 = 0x0020;
    pub const R: u32 = 0x0010;
    pub const PLUS: u32 = 0x0008;
    pub const MINUS: u32 = 0x0004;
    pub const HOME: u32 = 0x0002;
    pub const STICK_R: u32 = 0x0002_0000;
    pub const STICK_L: u32 = 0x0004_0000;
    // the sticks read as buttons (the menus use these)
    pub const STICK_L_LEFT: u32 = 0x4000_0000;
    pub const STICK_L_RIGHT: u32 = 0x2000_0000;
    pub const STICK_L_UP: u32 = 0x1000_0000;
    pub const STICK_L_DOWN: u32 = 0x0800_0000;
    pub const STICK_R_LEFT: u32 = 0x0400_0000;
    pub const STICK_R_RIGHT: u32 = 0x0200_0000;
    pub const STICK_R_UP: u32 = 0x0100_0000;
    pub const STICK_R_DOWN: u32 = 0x0080_0000;
}

/// The size of the GamePad's screen, which touch points are given in.
pub const TOUCH_SIZE: (f32, f32) = (1280.0, 720.0);

/// The right stick from the mouse: the mouse's speed (pixels a second, from raw motion) times `sensitivity`, eased towards with a time
/// constant of `smoothing` and so falling back to 0 the same way when the mouse stops. No dead zone is added (the game has its own).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MouseStick {
    /// Stick units per pixel a second: 0.0015 gives full deflection at about 670 pixels a second.
    pub sensitivity: f32,
    pub smoothing: std::time::Duration,
    pub invert_y: bool,
    moved: (f32, f32),
    value: (f32, f32),
    last: Option<std::time::Instant>,
}

/// Samples closer together than this give the last value (the game reads the sticks more than once a frame).
const MOUSE_MIN_STEP: std::time::Duration = std::time::Duration::from_millis(2);

impl MouseStick {
    pub const fn new() -> MouseStick {
        MouseStick {
            sensitivity: 0.0015,
            smoothing: std::time::Duration::from_millis(30),
            invert_y: false,
            moved: (0.0, 0.0),
            value: (0.0, 0.0),
            last: None,
        }
    }

    /// Raw mouse motion, in pixels (y down, as the mouse reports it).
    pub fn add(&mut self, dx: f32, dy: f32) {
        self.moved.0 += dx;
        self.moved.1 += dy;
    }

    /// The stick (x right, y up, each -1 to 1) at `now`.
    pub fn sample(&mut self, now: std::time::Instant) -> (f32, f32) {
        let Some(last) = self.last else {
            self.last = Some(now);
            self.moved = (0.0, 0.0);
            return self.value;
        };
        let dt = now.saturating_duration_since(last);
        if dt < MOUSE_MIN_STEP {
            return self.value;
        }
        self.last = Some(now);
        let secs = dt.as_secs_f32();
        let y = if self.invert_y { self.moved.1 } else { -self.moved.1 };
        let target = (
            (self.moved.0 / secs * self.sensitivity).clamp(-1.0, 1.0),
            (y / secs * self.sensitivity).clamp(-1.0, 1.0),
        );
        self.moved = (0.0, 0.0);
        let keep = if self.smoothing.is_zero() {
            0.0
        } else {
            (-secs / self.smoothing.as_secs_f32()).exp()
        };
        let ease = |v: f32, t: f32| {
            let n = t + (v - t) * keep;
            if t == 0.0 && n.abs() < 0.01 { 0.0 } else { n }
        };
        self.value = (ease(self.value.0, target.0), ease(self.value.1, target.1));
        self.value
    }
}

impl Default for MouseStick {
    fn default() -> MouseStick {
        MouseStick::new()
    }
}

#[derive(Default)]
pub struct Input {
    keyboard: AtomicU32,
    pad: AtomicU32,
    /// The sticks from the keyboard and from a gamepad: left x, left y, right x, right y (as `f32` bits), up and right positive.
    keyboard_sticks: [AtomicU32; 4],
    pad_sticks: [AtomicU32; 4],
    touched: AtomicBool,
    touch_x: AtomicU32,
    touch_y: AtomicU32,
    /// The mouse as the right stick (keyboard and mouse controls).
    pub mouse: std::sync::Mutex<MouseStick>,
}

pub static INPUT: Input = Input::new();

/// The overlay (`mh3u-gfx`): whether there is one (`MH3U_OVERLAY`), whether it is open (the game then gets no input at all), a toggle
/// asked for by the gamepad's combination, the buttons kept from the game until they are let go (the combination's), and the gamepad
/// buttons pressed while it is open (for the overlay to move with).
pub static OVERLAY_ENABLED: AtomicBool = AtomicBool::new(false);
pub static OVERLAY_OPEN: AtomicBool = AtomicBool::new(false);
pub static OVERLAY_TOGGLE: AtomicBool = AtomicBool::new(false);
static SWALLOWED: AtomicU32 = AtomicU32::new(0);
pub static OVERLAY_PRESSES: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());
/// Back (View, Select) and the right shoulder button held together open and close the overlay. The game has no use for the pair:
/// Minus opens its menu only on its own, and the combination's buttons are kept from the game until they are let go.
pub const OVERLAY_COMBO: u32 = button::MINUS | button::R;

/// Whether the game's window has the focus (the window sets it on each focus change).
pub static FOCUSED: AtomicBool = AtomicBool::new(true);
/// Whether gamepads still reach the game while the window does not have the focus. [`start_gamepads`] sets the real value:
/// on by default, off when `MH3U_BACKGROUND_GAMEPAD` is off, and off when `RECOMPLAUNCHER=1` unless that variable is on
/// (issue #33; the launcher's overlay uses the controller too). This `true` is only the placeholder until then. The keyboard's
/// counterpart, `MH3U_BACKGROUND_KEYBOARD`, is off by default and lives in the window (`mh3u-gfx`).
pub static BACKGROUND_PADS: AtomicBool = AtomicBool::new(true);

/// An on/off setting: `1`, `on`, `true` or `yes` is on, `0`, `off`, `false` or `no` is off, and anything else (or nothing) is
/// `default`.
pub fn switch_setting(value: Option<&str>, default: bool) -> bool {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("1" | "on" | "true" | "yes") => true,
        Some("0" | "off" | "false" | "no") => false,
        _ => default,
    }
}

/// Whether gamepads play while the window is in the background.
///
/// Standalone, with `setting` unset, they do. RecompLauncher sets `RECOMPLAUNCHER` to `1` and its overlay also reads the
/// controller, so the default is then off unless the player turned the setting on (`1`, `on`, `true` or `yes`). Any other
/// launcher value leaves the standalone default.
pub fn background_gamepad_on(setting: Option<&str>, recomp_launcher: Option<&str>) -> bool {
    let launcher = matches!(recomp_launcher.map(str::trim), Some("1"));
    switch_setting(setting, !launcher)
}

/// The startup line for [`background_gamepad_on`]. `setting_was_set` is whether `MH3U_BACKGROUND_GAMEPAD` was in the environment.
pub fn background_gamepad_message(on: bool, setting_was_set: bool, launcher: bool) -> &'static str {
    if on {
        "gamepads also work while the window is in the background (MH3U_BACKGROUND_GAMEPAD=0 turns that off)"
    } else if launcher && !setting_was_set {
        "gamepads only work while the window has the focus (RECOMPLAUNCHER=1; MH3U_BACKGROUND_GAMEPAD=1 turns that on)"
    } else {
        "gamepads only work while the window has the focus (MH3U_BACKGROUND_GAMEPAD=0)"
    }
}

/// Whether the gamepads' buttons and sticks reach the game: with the focus, or without it when background gamepads are on.
pub fn pads_reach_game(focused: bool, background: bool) -> bool {
    focused || background
}

fn pads_heard() -> bool {
    pads_reach_game(FOCUSED.load(Relaxed), BACKGROUND_PADS.load(Relaxed))
}

fn store(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Relaxed);
}

fn load(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Relaxed))
}

/// The buttons the sticks stand for when they are pushed far enough.
fn stick_buttons(x: f32, y: f32, left: bool) -> u32 {
    use button::*;
    let (l, r, u, d) = if left {
        (STICK_L_LEFT, STICK_L_RIGHT, STICK_L_UP, STICK_L_DOWN)
    } else {
        (STICK_R_LEFT, STICK_R_RIGHT, STICK_R_UP, STICK_R_DOWN)
    };
    let mut bits = 0;
    if x < -0.5 {
        bits |= l;
    }
    if x > 0.5 {
        bits |= r;
    }
    if y > 0.5 {
        bits |= u;
    }
    if y < -0.5 {
        bits |= d;
    }
    bits
}

impl Input {
    const fn new() -> Input {
        Input {
            keyboard: AtomicU32::new(0),
            pad: AtomicU32::new(0),
            keyboard_sticks: [const { AtomicU32::new(0) }; 4],
            pad_sticks: [const { AtomicU32::new(0) }; 4],
            touched: AtomicBool::new(false),
            touch_x: AtomicU32::new(0),
            touch_y: AtomicU32::new(0),
            mouse: std::sync::Mutex::new(MouseStick::new()),
        }
    }

    /// All the keyboard's (and mouse buttons') GamePad buttons at once.
    pub fn set_keyboard_buttons(&self, mask: u32) {
        self.keyboard.store(mask, Relaxed);
    }

    pub fn set_keyboard_button(&self, mask: u32, down: bool) {
        if down {
            self.keyboard.fetch_or(mask, Relaxed);
        } else {
            self.keyboard.fetch_and(!mask, Relaxed);
        }
    }

    pub fn set_pad_button(&self, mask: u32, down: bool) {
        if down {
            let before = self.pad.fetch_or(mask, Relaxed);
            let held = before | mask;
            // only the press that completes the combination toggles (a repeated press of a held button does not)
            if OVERLAY_ENABLED.load(Relaxed) && before & OVERLAY_COMBO != OVERLAY_COMBO && held & OVERLAY_COMBO == OVERLAY_COMBO {
                OVERLAY_TOGGLE.store(true, Relaxed);
                SWALLOWED.fetch_or(OVERLAY_COMBO, Relaxed);
            } else if OVERLAY_OPEN.load(Relaxed) {
                OVERLAY_PRESSES.lock().unwrap_or_else(|e| e.into_inner()).push(mask);
            }
        } else {
            self.pad.fetch_and(!mask, Relaxed);
            SWALLOWED.fetch_and(!mask, Relaxed);
        }
    }

    /// One of the keyboard's stick axes (0 to 3: left x, left y, right x, right y).
    pub fn set_keyboard_axis(&self, axis: usize, v: f32) {
        store(&self.keyboard_sticks[axis], v);
    }

    pub fn set_pad_axis(&self, axis: usize, v: f32) {
        store(&self.pad_sticks[axis], v);
    }

    /// Lets go of every gamepad button and centres the sticks (when the gamepads' source changes, so nothing stays held).
    pub fn release_pads(&self) {
        let held = self.pad.load(Relaxed);
        for bit in 0..32 {
            if held & (1 << bit) != 0 {
                self.set_pad_button(1 << bit, false);
            }
        }
        for axis in 0..4 {
            self.set_pad_axis(axis, 0.0);
        }
    }

    /// A touch of the GamePad's screen at (x, y) in `TOUCH_SIZE` units, or none.
    pub fn set_touch(&self, at: Option<(f32, f32)>) {
        if let Some((x, y)) = at {
            store(&self.touch_x, x);
            store(&self.touch_y, y);
        }
        self.touched.store(at.is_some(), Relaxed);
    }

    /// The sticks (left x, left y, right x, right y): a gamepad's when it is pushed, else the keyboard's.
    pub fn sticks(&self) -> [f32; 4] {
        let mut out = [0.0; 4];
        if OVERLAY_OPEN.load(Relaxed) {
            return out;
        }
        let (mx, my) = self
            .mouse
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sample(std::time::Instant::now());
        let mouse = [0.0, 0.0, mx, my];
        let heard = pads_heard();
        for (i, o) in out.iter_mut().enumerate() {
            let p = if heard { load(&self.pad_sticks[i]) } else { 0.0 };
            *o = if p.abs() > 0.05 {
                p
            } else {
                (load(&self.keyboard_sticks[i]) + mouse[i]).clamp(-1.0, 1.0)
            };
        }
        out
    }

    /// The buttons held, with the sticks' directions as buttons.
    pub fn hold(&self) -> u32 {
        if OVERLAY_OPEN.load(Relaxed) {
            return 0;
        }
        let s = self.sticks();
        let pad = if pads_heard() { self.pad.load(Relaxed) } else { 0 };
        let all = self.keyboard.load(Relaxed) | pad | stick_buttons(s[0], s[1], true) | stick_buttons(s[2], s[3], false);
        // ZL is the native Mods entry's modifier while the start menu is up (`native_menu::swallowed`; 0 with the opt-in off)
        all & !SWALLOWED.load(Relaxed) & !crate::native_menu::swallowed()
    }

    /// The gamepads' buttons only (not the keyboard's).
    pub fn pad_buttons(&self) -> u32 {
        if OVERLAY_OPEN.load(Relaxed) || !pads_heard() {
            return 0;
        }
        self.pad.load(Relaxed)
    }

    pub fn touch(&self) -> Option<(f32, f32)> {
        if OVERLAY_OPEN.load(Relaxed) {
            return None;
        }
        self.touched.load(Relaxed).then(|| (load(&self.touch_x), load(&self.touch_y)))
    }
}

/// How long the gamepad thread sleeps when there is no event: short next to a frame (the game reads the pads once a frame), long
/// enough that the thread costs nothing.
const PAD_IDLE: std::time::Duration = std::time::Duration::from_millis(4);

/// Reads gamepads on a thread of their own, as long as the program runs. MH3U_GAMEPADS=0 turns them off (no thread, no backend).
/// Which gamepad face button is which of the GamePad's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadLayout {
    /// The default: the button gilrs names East is the GamePad's A, South B, North X, West Y. With an Xbox pad on Windows this made
    /// the Xbox A the game's A (with the other layout, the user's run found the Xbox A acting as the game's B).
    Xbox,
    /// `MH3U_PAD_LAYOUT=nintendo`, the layout before: South is A, East B, West X, North Y.
    Nintendo,
}

impl PadLayout {
    /// From `MH3U_PAD_LAYOUT` (`nintendo`, or anything else or nothing for the default). `position`, the old name of what is now the
    /// default, is the default too.
    pub fn from_setting(value: Option<&str>) -> PadLayout {
        match value.map(|v| v.trim().to_ascii_lowercase()) {
            Some(v) if v == "nintendo" => PadLayout::Nintendo,
            _ => PadLayout::Xbox,
        }
    }
}

/// The GamePad button a gamepad button is, or `None` for one the GamePad does not have.
pub fn pad_button(b: gilrs::Button, layout: PadLayout) -> Option<u32> {
    use gilrs::Button;
    let nintendo = layout == PadLayout::Nintendo;
    Some(match b {
        Button::South if nintendo => button::A,
        Button::East if nintendo => button::B,
        Button::West if nintendo => button::X,
        Button::North if nintendo => button::Y,
        Button::East => button::A,
        Button::South => button::B,
        Button::North => button::X,
        Button::West => button::Y,
        Button::DPadUp => button::UP,
        Button::DPadDown => button::DOWN,
        Button::DPadLeft => button::LEFT,
        Button::DPadRight => button::RIGHT,
        Button::LeftTrigger => button::L,
        Button::RightTrigger => button::R,
        Button::LeftTrigger2 => button::ZL,
        Button::RightTrigger2 => button::ZR,
        Button::Start => button::PLUS,
        Button::Select => button::MINUS,
        Button::Mode => button::HOME,
        Button::LeftThumb => button::STICK_L,
        Button::RightThumb => button::STICK_R,
        _ => return None,
    })
}

fn face_name(mask: u32) -> &'static str {
    match mask {
        button::A => "A",
        button::B => "B",
        button::X => "X",
        _ => "Y",
    }
}

pub fn start_gamepads() {
    if std::env::var("MH3U_GAMEPADS").is_ok_and(|v| v.trim() == "0") {
        eprintln!("gamepads off (MH3U_GAMEPADS=0)");
        return;
    }
    let setting = std::env::var("MH3U_BACKGROUND_GAMEPAD").ok();
    let launcher_var = std::env::var("RECOMPLAUNCHER").ok();
    let launcher = matches!(launcher_var.as_deref().map(str::trim), Some("1"));
    let background = background_gamepad_on(setting.as_deref(), launcher_var.as_deref());
    BACKGROUND_PADS.store(background, Relaxed);
    eprintln!("{}", background_gamepad_message(background, setting.is_some(), launcher));
    let _ = std::thread::Builder::new().name("gamepads".into()).spawn(|| {
        let mut gilrs = match gilrs::Gilrs::new() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("no gamepad support: {e}");
                return;
            }
        };
        let layout = PadLayout::from_setting(std::env::var("MH3U_PAD_LAYOUT").ok().as_deref());
        eprintln!(
            "{}",
            match layout {
                PadLayout::Xbox => "gamepad buttons: xbox layout (MH3U_PAD_LAYOUT=nintendo swaps A with B and X with Y)",
                PadLayout::Nintendo => "gamepad buttons: nintendo layout (MH3U_PAD_LAYOUT=nintendo; unset it for the xbox layout)",
            }
        );
        // the first press of each face button says what it was taken as, so a swapped layout shows in the log
        let mut told = [false; 4];
        for (_, pad) in gilrs.gamepads() {
            eprintln!("gamepad: {}", pad.name());
        }
        use gilrs::{Axis, EventType};
        let trace = std::env::var_os("MH3U_TRACE_PAD").is_some();
        // The pending events are taken without waiting, and the thread sleeps a little when there are none. It does not wait in
        // gilrs's blocking call (`next_event_blocking` with a timeout), which was where a Windows profile found this thread taking
        // a whole core; a plain sleep costs nothing on any platform whatever the backend does.
        // Windows: gilrs's backend (Windows.Gaming.Input) hears nothing while the window does not have the focus, so then the
        // pads are read with XInput instead (`background::Pad`; Xbox-style pads only), and gilrs's events are dropped
        #[cfg(windows)]
        let mut xinput = background::Pad::new(layout);
        loop {
            #[cfg(windows)]
            if xinput.step(!FOCUSED.load(Relaxed) && BACKGROUND_PADS.load(Relaxed)) {
                while gilrs.next_event().is_some() {}
                std::thread::sleep(PAD_IDLE);
                continue;
            }
            let Some(event) = gilrs.next_event() else {
                std::thread::sleep(PAD_IDLE);
                continue;
            };
            if trace {
                eprintln!("[gamepad] {:?}", event.event);
            }
            match event.event {
                EventType::ButtonPressed(b, _) | EventType::ButtonReleased(b, _) => {
                    let down = matches!(event.event, EventType::ButtonPressed(..));
                    let Some(mask) = pad_button(b, layout) else { continue };
                    let face = [button::A, button::B, button::X, button::Y].iter().position(|m| *m == mask);
                    if let Some(k) = face
                        && down
                        && !told[k]
                    {
                        told[k] = true;
                        eprintln!("[gamepad] {b:?} is the GamePad's {}", face_name(mask));
                    }
                    INPUT.set_pad_button(mask, down);
                }
                EventType::Connected => eprintln!("gamepad connected: {}", gilrs.gamepad(event.id).name()),
                EventType::AxisChanged(a, v, _) => {
                    let axis = match a {
                        Axis::LeftStickX => 0,
                        Axis::LeftStickY => 1,
                        Axis::RightStickX => 2,
                        Axis::RightStickY => 3,
                        _ => continue,
                    };
                    INPUT.set_pad_axis(axis, v);
                }
                _ => {}
            }
        }
    });
}

/// The pads while the window is in the background on Windows, read with XInput: gilrs's Windows backend (Windows.Gaming.Input) only
/// hears pads while a window of the process has the focus, while XInput hears them always (but only Xbox-style pads, the first
/// one connected of the four slots).
#[cfg(windows)]
mod background {
    use super::{INPUT, PadLayout, button, pad_button};
    use gilrs::Button;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::UI::Input::XboxController::{XINPUT_STATE, XInputGetState};

    /// XInput's button bits (`XINPUT_GAMEPAD_*`) as gilrs names them, so they map to the GamePad's as gilrs's events do.
    const BUTTONS: [(u16, Button); 14] = [
        (0x0001, Button::DPadUp),
        (0x0002, Button::DPadDown),
        (0x0004, Button::DPadLeft),
        (0x0008, Button::DPadRight),
        (0x0010, Button::Start),
        (0x0020, Button::Select),
        (0x0040, Button::LeftThumb),
        (0x0080, Button::RightThumb),
        (0x0100, Button::LeftTrigger),
        (0x0200, Button::RightTrigger),
        (0x1000, Button::South),
        (0x2000, Button::East),
        (0x4000, Button::West),
        (0x8000, Button::North),
    ];
    /// XInput's own dead zones and trigger threshold (`XINPUT_GAMEPAD_LEFT_THUMB_DEADZONE` and the rest).
    const LEFT_DEADZONE: f32 = 7849.0;
    const RIGHT_DEADZONE: f32 = 8689.0;
    const TRIGGER_THRESHOLD: u8 = 30;
    /// An empty slot is slow to ask about, so without a pad the slots are looked through at most this often.
    const RESCAN: Duration = Duration::from_secs(1);

    fn axis(v: i16, deadzone: f32) -> f32 {
        let v = f32::from(v);
        if v.abs() < deadzone { 0.0 } else { (v / 32767.0).clamp(-1.0, 1.0) }
    }

    pub struct Pad {
        layout: PadLayout,
        active: bool,
        slot: Option<u32>,
        scanned: Option<Instant>,
        held: u32,
    }

    impl Pad {
        pub fn new(layout: PadLayout) -> Pad {
            Pad {
                layout,
                active: false,
                slot: None,
                scanned: None,
                held: 0,
            }
        }

        fn state(slot: u32) -> Option<XINPUT_STATE> {
            // SAFETY: XInputGetState writes one XINPUT_STATE, a plain struct for which all zeros is a valid value, to the pointer it
            // is given, which points at one here
            unsafe {
                let mut state: XINPUT_STATE = std::mem::zeroed();
                (XInputGetState(slot, &mut state) == 0).then_some(state)
            }
        }

        /// Reads the pad when `wanted` (the window in the background with background gamepads on), and says whether it did. Each
        /// change between the two sources lets go of what the other held.
        pub fn step(&mut self, wanted: bool) -> bool {
            if wanted != self.active {
                self.active = wanted;
                self.held = 0;
                INPUT.release_pads();
                eprintln!(
                    "[gamepad] {}",
                    if wanted {
                        "window in the background: reading the pad with XInput"
                    } else {
                        "window focused: back to gilrs"
                    }
                );
            }
            if !self.active {
                return false;
            }
            let state = match self.slot.and_then(Self::state) {
                Some(s) => Some(s),
                None => {
                    self.slot = None;
                    let now = Instant::now();
                    if self.scanned.is_none_or(|t| now.duration_since(t) >= RESCAN) {
                        self.scanned = Some(now);
                        self.slot = (0..4).find(|&i| Self::state(i).is_some());
                    }
                    self.slot.and_then(Self::state)
                }
            };
            let Some(state) = state else {
                if self.held != 0 {
                    self.held = 0;
                    INPUT.release_pads();
                }
                return true;
            };
            let g = state.Gamepad;
            let pressed = g.wButtons;
            let mut mask = 0;
            for (bit, b) in BUTTONS {
                if pressed & bit != 0
                    && let Some(m) = pad_button(b, self.layout)
                {
                    mask |= m;
                }
            }
            if g.bLeftTrigger > TRIGGER_THRESHOLD {
                mask |= button::ZL;
            }
            if g.bRightTrigger > TRIGGER_THRESHOLD {
                mask |= button::ZR;
            }
            let changed = mask ^ self.held;
            for bit in 0..32 {
                let m = 1 << bit;
                if changed & m != 0 {
                    INPUT.set_pad_button(m, mask & m != 0);
                }
            }
            self.held = mask;
            let sticks = [
                axis(g.sThumbLX, LEFT_DEADZONE),
                axis(g.sThumbLY, LEFT_DEADZONE),
                axis(g.sThumbRX, RIGHT_DEADZONE),
                axis(g.sThumbRY, RIGHT_DEADZONE),
            ];
            for (i, v) in sticks.into_iter().enumerate() {
                INPUT.set_pad_axis(i, v);
            }
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_gamepads_are_on_by_default_and_keep_the_pads_without_the_focus() {
        assert!(switch_setting(None, true));
        assert!(!switch_setting(Some("0"), true));
        assert!(!switch_setting(Some(" off "), true));
        assert!(switch_setting(Some("1"), false));
        assert!(switch_setting(Some("ON"), false));
        assert!(!switch_setting(Some("maybe"), false), "anything else is the default");
        assert!(pads_reach_game(true, false), "with the focus, always");
        assert!(pads_reach_game(false, true));
        assert!(!pads_reach_game(false, false), "MH3U_BACKGROUND_GAMEPAD=0 without the focus");
    }

    #[test]
    fn recomp_launcher_defaults_background_gamepads_off_unless_the_player_turned_them_on() {
        assert!(background_gamepad_on(None, None), "standalone default stays on");
        assert!(background_gamepad_on(None, Some("0")));
        assert!(background_gamepad_on(None, Some("")));
        assert!(!background_gamepad_on(None, Some("1")));
        assert!(!background_gamepad_on(None, Some(" 1 ")));
        assert!(background_gamepad_on(Some("1"), Some("1")), "an explicit on wins");
        assert!(background_gamepad_on(Some("on"), Some("1")));
        assert!(background_gamepad_on(Some(" yes "), Some("1")));
        assert!(!background_gamepad_on(Some("0"), Some("1")));
        assert!(!background_gamepad_on(Some("off"), None));
        assert!(
            background_gamepad_on(None, Some("desktop")),
            "only RECOMPLAUNCHER=1 changes the default"
        );
        assert_eq!(
            background_gamepad_message(false, false, true),
            "gamepads only work while the window has the focus (RECOMPLAUNCHER=1; MH3U_BACKGROUND_GAMEPAD=1 turns that on)"
        );
        assert!(background_gamepad_message(true, true, true).contains("MH3U_BACKGROUND_GAMEPAD=0"));
        assert!(background_gamepad_message(false, true, true).contains("MH3U_BACKGROUND_GAMEPAD=0"));
    }

    #[test]
    fn the_mouse_stick_follows_the_mouse_s_speed_and_falls_back_to_zero_when_it_stops() {
        let t0 = std::time::Instant::now();
        let ms = |n: u64| t0 + std::time::Duration::from_millis(n);
        let mut m = MouseStick::new();
        m.smoothing = std::time::Duration::ZERO;
        assert_eq!(m.sample(ms(0)), (0.0, 0.0));
        // 4 pixels right and 4 up (y down on the mouse) in 10 ms: 400 px/s, 0.6 of the way at 0.0015
        m.add(4.0, -4.0);
        let (x, y) = m.sample(ms(10));
        assert!((x - 0.6).abs() < 1e-4 && (y - 0.6).abs() < 1e-4, "{x} {y}");
        // closer than 2 ms: the same value
        m.add(100.0, 0.0);
        assert_eq!(m.sample(ms(11)), (x, y));
        // fast: at most full deflection
        assert_eq!(m.sample(ms(20)).0, 1.0);
        // stopped: zero (no smoothing)
        assert_eq!(m.sample(ms(30)), (0.0, 0.0));
        // with smoothing it eases there and decays to exactly zero
        m.smoothing = std::time::Duration::from_millis(30);
        m.add(4.0, 0.0);
        let first = m.sample(ms(40)).0;
        assert!(first > 0.0 && first < 0.6);
        let mut t = 40;
        let mut v = first;
        for _ in 0..40 {
            t += 10;
            let n = m.sample(ms(t)).0;
            assert!(n <= v);
            v = n;
        }
        assert_eq!(v, 0.0);
        // inverted: mouse up is stick down
        m.smoothing = std::time::Duration::ZERO;
        m.invert_y = true;
        m.add(0.0, -4.0);
        assert!(m.sample(ms(t + 10)).1 < 0.0);
    }

    #[test]
    fn the_overlay_combination_toggles_and_is_kept_from_the_game_and_an_open_overlay_takes_all_input() {
        // its own Input, but the overlay's flags are global: this is the only test that sets them
        let input = Input::new();
        OVERLAY_ENABLED.store(true, Relaxed);
        input.set_pad_button(button::MINUS, true);
        // Back alone reaches the game
        assert_eq!(input.hold(), button::MINUS);
        input.set_pad_button(button::R, true);
        assert!(OVERLAY_TOGGLE.swap(false, Relaxed));
        assert_eq!(input.hold(), 0, "the combination's buttons are the overlay's");
        // a press event repeated for a button already held (some backends send them) does not toggle again
        input.set_pad_button(button::R, true);
        input.set_pad_button(button::MINUS, true);
        assert!(!OVERLAY_TOGGLE.load(Relaxed));
        input.set_pad_button(button::R, false);
        assert_eq!(input.hold(), 0, "Back stays kept until it is let go");
        input.set_pad_button(button::MINUS, false);
        input.set_pad_button(button::A, true);
        assert_eq!(input.hold(), button::A);
        // open: nothing reaches the game, and presses go to the overlay
        OVERLAY_OPEN.store(true, Relaxed);
        input.set_pad_button(button::DOWN, true);
        input.set_keyboard_axis(0, 1.0);
        input.set_touch(Some((1.0, 2.0)));
        assert_eq!((input.hold(), input.sticks(), input.touch()), (0, [0.0; 4], None));
        assert_eq!(OVERLAY_PRESSES.lock().unwrap().drain(..).collect::<Vec<_>>(), [button::DOWN]);
        OVERLAY_OPEN.store(false, Relaxed);
        assert_eq!(input.hold() & button::A, button::A);
        OVERLAY_ENABLED.store(false, Relaxed);
    }

    #[test]
    fn a_pushed_stick_is_also_a_button_and_keys_and_pads_add_up() {
        let i = Input::new();
        i.set_keyboard_button(button::A, true);
        i.set_pad_button(button::B, true);
        i.set_keyboard_axis(0, -1.0);
        let hold = i.hold();
        assert_eq!(hold, button::A | button::B | button::STICK_L_LEFT);
        i.set_pad_axis(0, 0.8);
        assert_eq!(i.sticks()[0], 0.8);
        i.set_keyboard_button(button::A, false);
        assert_eq!(i.hold() & button::A, 0);
    }

    #[test]
    fn face_buttons_follow_the_layout() {
        use gilrs::Button;
        let faces = [Button::South, Button::East, Button::West, Button::North];
        let map = |layout| faces.map(|b| pad_button(b, layout));
        // the default: East A, South B, North X, West Y
        assert_eq!(
            map(PadLayout::Xbox),
            [Some(button::B), Some(button::A), Some(button::Y), Some(button::X)]
        );
        // MH3U_PAD_LAYOUT=nintendo: the layout before, A with B and X with Y swapped
        assert_eq!(
            map(PadLayout::Nintendo),
            [Some(button::A), Some(button::B), Some(button::X), Some(button::Y)]
        );
        // the rest are the same in both
        for layout in [PadLayout::Xbox, PadLayout::Nintendo] {
            assert_eq!(pad_button(Button::Start, layout), Some(button::PLUS));
            assert_eq!(pad_button(Button::LeftTrigger2, layout), Some(button::ZL));
            assert_eq!(pad_button(Button::DPadLeft, layout), Some(button::LEFT));
            assert_eq!(pad_button(Button::C, layout), None);
        }
    }

    #[test]
    fn the_layout_setting() {
        assert_eq!(PadLayout::from_setting(None), PadLayout::Xbox);
        assert_eq!(PadLayout::from_setting(Some("nintendo")), PadLayout::Nintendo);
        assert_eq!(PadLayout::from_setting(Some(" Nintendo ")), PadLayout::Nintendo);
        assert_eq!(PadLayout::from_setting(Some("position")), PadLayout::Xbox);
        assert_eq!(PadLayout::from_setting(Some("xbox")), PadLayout::Xbox);
    }
}
