//! Keyboard and mouse controls: keys and mouse buttons onto the GamePad's buttons, WASD onto the left stick, and the mouse onto the
//! right stick (`mh3u_rt::input::MouseStick`). The bindings are kept in a JSON file the player can edit, made with the defaults the
//! first time; `MH3U_KBM=off` turns keyboard and mouse controls off (the mouse is then the GamePad's touch screen, as before).
//!
//! Keys are named by position as `winit` names them (`KeyW`, `Space`, `ShiftLeft`, `Digit1`...), so they do not depend on the
//! keyboard's layout; mouse buttons are `MouseLeft`, `MouseRight`, `MouseMiddle`, `MouseBack` and `MouseForward`.

use mh3u_rt::input::button;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

/// Keys the window keeps for itself: the overlay's and fullscreen's (Alt+Enter, so Enter is not bound either).
pub const RESERVED: [&str; 4] = ["F1", "F11", "Enter", "NumpadEnter"];

/// The GamePad's buttons by the names the file uses.
pub fn button_mask(name: &str) -> Option<u32> {
    use button::*;
    Some(match name {
        "A" => A,
        "B" => B,
        "X" => X,
        "Y" => Y,
        "L" => L,
        "R" => R,
        "ZL" => ZL,
        "ZR" => ZR,
        "Plus" => PLUS,
        "Minus" => MINUS,
        "Home" => HOME,
        "Up" => UP,
        "Down" => DOWN,
        "Left" => LEFT,
        "Right" => RIGHT,
        "L3" => STICK_L,
        "R3" => STICK_R,
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// GamePad button -> the keys and mouse buttons that press it.
    pub buttons: BTreeMap<String, Vec<String>>,
    pub move_up: String,
    pub move_down: String,
    pub move_left: String,
    pub move_right: String,
    /// Held, the left stick goes only `walk_scale` of the way.
    pub walk: String,
    pub walk_scale: f32,
    /// The right stick from the mouse: stick units per pixel a second, the easing time in milliseconds, and up for down.
    pub mouse_sensitivity: f32,
    pub mouse_smoothing_ms: f32,
    pub invert_y: bool,
}

impl Default for Config {
    fn default() -> Config {
        let b = |list: &[(&str, &[&str])]| {
            list.iter()
                .map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect()))
                .collect()
        };
        Config {
            buttons: b(&[
                ("X", &["MouseLeft"]),
                ("A", &["MouseRight"]),
                ("B", &["Space"]),
                ("Y", &["KeyE"]),
                ("R", &["ShiftLeft"]),
                ("L", &["KeyQ"]),
                ("ZR", &["KeyR"]),
                ("ZL", &["KeyF"]),
                ("Plus", &["Escape"]),
                ("Minus", &["Tab"]),
                ("Up", &["ArrowUp", "Digit1"]),
                ("Down", &["ArrowDown", "Digit2"]),
                ("Left", &["ArrowLeft", "Digit3"]),
                ("Right", &["ArrowRight", "Digit4"]),
                ("L3", &["KeyC"]),
                ("R3", &["MouseMiddle"]),
                ("Home", &["Home"]),
            ]),
            move_up: "KeyW".into(),
            move_down: "KeyS".into(),
            move_left: "KeyA".into(),
            move_right: "KeyD".into(),
            walk: "ControlLeft".into(),
            walk_scale: 0.5,
            mouse_sensitivity: 0.0015,
            mouse_smoothing_ms: 30.0,
            invert_y: false,
        }
    }
}

/// The bindings ready for use.
#[derive(Debug, Clone)]
pub struct Bindings {
    pub inputs: HashMap<String, u32>,
    moves: [String; 4],
    walk: String,
    walk_scale: f32,
    pub config: Config,
}

impl Bindings {
    /// The bindings from a config, or what is wrong with it.
    pub fn new(config: Config) -> Result<Bindings, String> {
        let mut inputs: HashMap<String, u32> = HashMap::new();
        for (name, keys) in &config.buttons {
            let mask = button_mask(name).ok_or_else(|| format!("no GamePad button is called {name:?}"))?;
            for k in keys {
                if RESERVED.contains(&k.as_str()) {
                    return Err(format!(
                        "{k} is the window's (the overlay and fullscreen); bind {name} to another key"
                    ));
                }
                *inputs.entry(k.clone()).or_default() |= mask;
            }
        }
        let moves = [
            config.move_up.clone(),
            config.move_down.clone(),
            config.move_left.clone(),
            config.move_right.clone(),
        ];
        if let Some(k) = moves.iter().chain([&config.walk]).find(|k| RESERVED.contains(&k.as_str())) {
            return Err(format!("{k} is the window's (the overlay and fullscreen)"));
        }
        Ok(Bindings {
            inputs,
            moves,
            walk: config.walk.clone(),
            walk_scale: config.walk_scale.clamp(0.0, 1.0),
            config,
        })
    }

    /// The GamePad buttons the held keys and mouse buttons press.
    pub fn buttons(&self, held: &HashSet<String>) -> u32 {
        held.iter().filter_map(|k| self.inputs.get(k)).fold(0, |a, m| a | m)
    }

    /// The left stick from the movement keys (x right, y up): a diagonal is as far as a straight push, and the walk key halves it.
    pub fn left_stick(&self, held: &HashSet<String>) -> (f32, f32) {
        let on = |k: &String| f32::from(u8::from(held.contains(k)));
        let (x, y) = (on(&self.moves[3]) - on(&self.moves[2]), on(&self.moves[0]) - on(&self.moves[1]));
        let len = (x * x + y * y).sqrt();
        if len == 0.0 {
            return (0.0, 0.0);
        }
        let scale = if held.contains(&self.walk) { self.walk_scale } else { 1.0 };
        (x / len * scale, y / len * scale)
    }

    /// The mouse stick's settings, from the config.
    pub fn mouse_stick(&self, m: &mut mh3u_rt::input::MouseStick) {
        m.sensitivity = self.config.mouse_sensitivity.max(0.0);
        m.smoothing = std::time::Duration::from_secs_f32(self.config.mouse_smoothing_ms.clamp(0.0, 1000.0) / 1000.0);
        m.invert_y = self.config.invert_y;
    }

    /// The bindings as lines for the overlay: (what, keys).
    pub fn lines(&self) -> Vec<(String, String)> {
        let c = &self.config;
        let mut out = vec![
            (
                "Move".to_string(),
                format!("{} {} {} {} (walk: {})", c.move_up, c.move_left, c.move_down, c.move_right, c.walk),
            ),
            (
                "Camera".to_string(),
                format!("mouse{}", if c.invert_y { " (up and down inverted)" } else { "" }),
            ),
        ];
        out.extend(c.buttons.iter().map(|(b, keys)| (b.clone(), keys.join(", "))));
        out
    }
}

/// Whether keyboard and mouse controls are on (`MH3U_KBM=off` turns them off).
pub fn enabled(value: Option<&str>) -> bool {
    !matches!(value.map(str::trim), Some("off" | "0"))
}

/// The bindings in `path`, made there with the defaults when there is no file. A file that cannot be read gives the defaults, and
/// says why (the file is left as it is, for the player to mend).
pub fn load_or_create(path: &Path) -> (Bindings, Option<String>) {
    let defaults = || Bindings::new(Config::default()).expect("the defaults are valid");
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Config>(&text)
            .map_err(|e| e.to_string())
            .and_then(Bindings::new)
        {
            Ok(b) => (b, None),
            Err(e) => (defaults(), Some(format!("{}: {e}; using the defaults", path.display()))),
        },
        Err(_) => {
            let made = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(path, serde_json::to_string_pretty(&Config::default()).unwrap_or_default()));
            let note = match made {
                Ok(()) => format!("made {} with the default bindings", path.display()),
                Err(e) => format!("could not write {}: {e}; using the defaults", path.display()),
            };
            (defaults(), Some(note))
        }
    }
}

/// The cursor is grabbed (kept in the window, hidden) while playing with keyboard and mouse: the window has the focus and the overlay
/// is closed.
pub fn grab(kbm: bool, focused: bool, overlay_open: bool) -> bool {
    kbm && focused && !overlay_open
}

/// The name a mouse button is bound by.
pub fn mouse_name(b: winit::event::MouseButton) -> Option<&'static str> {
    use winit::event::MouseButton;
    Some(match b {
        MouseButton::Left => "MouseLeft",
        MouseButton::Right => "MouseRight",
        MouseButton::Middle => "MouseMiddle",
        MouseButton::Back => "MouseBack",
        MouseButton::Forward => "MouseForward",
        MouseButton::Other(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(keys: &[&str]) -> HashSet<String> {
        keys.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn keys_and_mouse_buttons_press_their_gamepad_buttons() {
        let b = Bindings::new(Config::default()).unwrap();
        assert_eq!(b.buttons(&held(&["MouseLeft"])), button::X);
        assert_eq!(b.buttons(&held(&["MouseRight", "Space"])), button::A | button::B);
        assert_eq!(b.buttons(&held(&["Digit1", "ArrowUp"])), button::UP);
        assert_eq!(b.buttons(&held(&["MouseMiddle", "KeyC"])), button::STICK_R | button::STICK_L);
        assert_eq!(
            b.buttons(&held(&["KeyW", "F1"])),
            0,
            "movement and the window's keys press no button"
        );
        assert!(RESERVED.iter().all(|k| !b.inputs.contains_key(*k)));
    }

    #[test]
    fn wasd_diagonals_are_as_far_as_straight_pushes_and_walking_halves_them() {
        let b = Bindings::new(Config::default()).unwrap();
        assert_eq!(b.left_stick(&held(&["KeyW"])), (0.0, 1.0));
        assert_eq!(b.left_stick(&held(&["KeyA"])), (-1.0, 0.0));
        let (x, y) = b.left_stick(&held(&["KeyW", "KeyD"]));
        assert!((x * x + y * y - 1.0).abs() < 1e-6 && x > 0.0 && y > 0.0);
        assert_eq!(b.left_stick(&held(&["KeyW", "KeyS"])), (0.0, 0.0), "opposites cancel");
        let (x, y) = b.left_stick(&held(&["KeyS", "ControlLeft"]));
        assert_eq!((x, y), (0.0, -0.5));
    }

    #[test]
    fn a_bad_config_is_refused_and_the_defaults_are_made_on_first_run() {
        let mut c = Config::default();
        c.buttons.insert("Jump".into(), vec!["KeyJ".into()]);
        assert!(Bindings::new(c).unwrap_err().contains("Jump"));
        let mut c = Config::default();
        c.buttons.insert("A".into(), vec!["F1".into()]);
        assert!(Bindings::new(c).unwrap_err().contains("F1"));

        let dir = std::env::temp_dir().join(format!("mh3u-kbm-{}", std::process::id()));
        let path = dir.join("input/bindings.json");
        let (b, note) = load_or_create(&path);
        assert!(note.unwrap().contains("made"));
        assert_eq!(b.config, Config::default());
        // an edit is read back; fields left out keep their defaults
        std::fs::write(&path, r#"{ "invert_y": true, "buttons": { "A": ["KeyZ"] } }"#).unwrap();
        let (b, note) = load_or_create(&path);
        assert!(note.is_none());
        assert!(b.config.invert_y);
        assert_eq!(b.buttons(&held(&["KeyZ"])), button::A);
        assert_eq!(b.buttons(&held(&["MouseRight"])), 0, "the buttons map is the file's");
        std::fs::write(&path, "{ nope").unwrap();
        let (b, note) = load_or_create(&path);
        assert!(note.unwrap().contains("defaults"));
        assert_eq!(b.config, Config::default());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{ nope",
            "a broken file is left for the player to mend"
        );
        std::fs::remove_dir_all(dir).unwrap();
        assert!(enabled(None) && !enabled(Some("off")));
    }

    #[test]
    fn the_cursor_is_grabbed_only_while_playing_with_the_focus() {
        assert!(grab(true, true, false));
        assert!(!grab(true, false, false), "not without the focus");
        assert!(!grab(true, true, true), "not with the overlay open");
        assert!(!grab(false, true, false), "not with keyboard and mouse off");
    }
}
