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
}

pub static INPUT: Input = Input::new();

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
        }
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
            self.pad.fetch_or(mask, Relaxed);
        } else {
            self.pad.fetch_and(!mask, Relaxed);
        }
    }

    /// One of the keyboard's stick axes (0 to 3: left x, left y, right x, right y).
    pub fn set_keyboard_axis(&self, axis: usize, v: f32) {
        store(&self.keyboard_sticks[axis], v);
    }

    pub fn set_pad_axis(&self, axis: usize, v: f32) {
        store(&self.pad_sticks[axis], v);
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
        for (i, o) in out.iter_mut().enumerate() {
            let p = load(&self.pad_sticks[i]);
            *o = if p.abs() > 0.05 { p } else { load(&self.keyboard_sticks[i]) };
        }
        out
    }

    /// The buttons held, with the sticks' directions as buttons.
    pub fn hold(&self) -> u32 {
        let s = self.sticks();
        self.keyboard.load(Relaxed) | self.pad.load(Relaxed) | stick_buttons(s[0], s[1], true) | stick_buttons(s[2], s[3], false)
    }

    pub fn touch(&self) -> Option<(f32, f32)> {
        self.touched.load(Relaxed).then(|| (load(&self.touch_x), load(&self.touch_y)))
    }
}

/// The GamePad button a keyboard key stands for, by the key's position (so it does not depend on the layout).
pub fn key_button(code: &str) -> Option<u32> {
    use button::*;
    Some(match code {
        "Enter" => A,
        "Backspace" => B,
        "KeyX" => X,
        "KeyC" => Y,
        "KeyI" => UP,
        "KeyK" => DOWN,
        "KeyJ" => LEFT,
        "KeyL" => RIGHT,
        "KeyQ" => L,
        "KeyE" => R,
        "Digit1" => ZL,
        "Digit3" => ZR,
        "KeyP" => PLUS,
        "KeyO" => MINUS,
        "Home" => HOME,
        "ShiftLeft" => STICK_L,
        "ShiftRight" => STICK_R,
        _ => return None,
    })
}

/// The stick axis a keyboard key pushes: (axis, direction). WASD is the left stick and the arrows the right one.
pub fn key_axis(code: &str) -> Option<(usize, f32)> {
    Some(match code {
        "KeyA" => (0, -1.0),
        "KeyD" => (0, 1.0),
        "KeyW" => (1, 1.0),
        "KeyS" => (1, -1.0),
        "ArrowLeft" => (2, -1.0),
        "ArrowRight" => (2, 1.0),
        "ArrowUp" => (3, 1.0),
        "ArrowDown" => (3, -1.0),
        _ => return None,
    })
}

/// How long the gamepad thread sleeps when there is no event: short next to a frame (the game reads the pads once a frame), long
/// enough that the thread costs nothing.
const PAD_IDLE: std::time::Duration = std::time::Duration::from_millis(4);

/// Reads gamepads on a thread of their own, as long as the program runs. MH3U_GAMEPADS=0 turns them off (no thread, no backend).
pub fn start_gamepads() {
    if std::env::var("MH3U_GAMEPADS").is_ok_and(|v| v.trim() == "0") {
        eprintln!("gamepads off (MH3U_GAMEPADS=0)");
        return;
    }
    let _ = std::thread::Builder::new().name("gamepads".into()).spawn(|| {
        let mut gilrs = match gilrs::Gilrs::new() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("no gamepad support: {e}");
                return;
            }
        };
        let by_position = std::env::var("MH3U_PAD_LAYOUT").is_ok_and(|v| v == "position");
        for (_, pad) in gilrs.gamepads() {
            eprintln!("gamepad: {}", pad.name());
        }
        use gilrs::{Axis, Button, EventType};
        let trace = std::env::var_os("MH3U_TRACE_PAD").is_some();
        // The pending events are taken without waiting, and the thread sleeps a little when there are none. It does not wait in
        // gilrs's blocking call (`next_event_blocking` with a timeout), which was where a Windows profile found this thread taking
        // a whole core; a plain sleep costs nothing on any platform whatever the backend does.
        loop {
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
                    // by default the buttons go by their labels (the Xbox A is the GamePad's A); MH3U_PAD_LAYOUT=position puts
                    // them where the GamePad's are (the right-hand one is A)
                    let mask = match b {
                        Button::East if by_position => button::A,
                        Button::South if by_position => button::B,
                        Button::North if by_position => button::X,
                        Button::West if by_position => button::Y,
                        Button::South => button::A,
                        Button::East => button::B,
                        Button::West => button::X,
                        Button::North => button::Y,
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
                        _ => continue,
                    };
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
