//! The software keyboard (`swkbd.rpl`), answered by the host: what the game needs to take a name it is typed on the PC.
//!
//! The game's keyboard wrapper (`FUN_0307dc18`, update `.rpx`) finds the `Swkbd*__3Rpl*` functions with `OSDynLoad_FindExport`;
//! the runtime gives each a stub address (`game::unknown_call`), which comes here. What the game does with them:
//!
//! - **Creating** (`FUN_0293af30`, on a worker): `SwkbdCreate`, then `SwkbdInitLearnDic` once per controller. A 0 from
//!   `InitLearnDic` (as every stub returned before) breaks the loop and the keyboard is never made, so the character screen's
//!   name field waited for good. Both return 1 here.
//! - **Showing** (`FUN_0293b9ac`): only when `SwkbdGetStateInputForm` is 0 (hidden), then `SwkbdAppearInputForm`.
//! - **Each frame** (`FUN_0293c0b0`): with the state 2 (shown), `SwkbdIsDecideOkButton` or `SwkbdIsDecideCancelButton` true
//!   calls the game's receiver with the text from `SwkbdGetInputFormString` (UTF-16, ending in 0).
//!
//! The text comes from `MH3U_SWKBD_TEXT` when set (taken at once), else from the overlay's keyboard window (`submit`).

use std::sync::Mutex;

/// Where the typed text is put for the game (UTF-16BE, ending in 0): in the mods' buffer (`native_text::BUFFER`), past the
/// native menu's texts.
pub const TEXT_AT: u32 = crate::native_text::BUFFER + 0x8000;
/// The most characters taken (the hunter's name field holds 10).
pub const MOST: usize = 10;

/// The input form's states, as `SwkbdGetStateInputForm` gives them.
pub const HIDDEN: u32 = 0;
pub const SHOWN: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Keyboard {
    shown: bool,
    /// What the player decided: `Some(Some(text))` OK, `Some(None)` cancel, `None` not yet.
    decided: Option<Option<String>>,
    /// The text last decided, for `GetInputFormString`.
    text: String,
}

static KEYBOARD: Mutex<Keyboard> = Mutex::new(Keyboard {
    shown: false,
    decided: None,
    text: String::new(),
});

/// The text as the game takes it: at most `MOST` characters, no control characters.
pub fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(MOST).collect()
}

/// UTF-16BE with a terminating 0.
pub fn utf16be(text: &str) -> Vec<u8> {
    text.encode_utf16().chain([0]).flat_map(u16::to_be_bytes).collect()
}

impl Keyboard {
    /// Answers a call of the keyboard function `name` (mangled; `None`: not one of these). `write(address, bytes)` puts the text
    /// in guest memory.
    pub fn call(&mut self, name: &str, env_text: Option<&str>, write: impl FnOnce(u32, &[u8])) -> Option<u32> {
        let base = name.split("__").next()?;
        if !base.starts_with("Swkbd") {
            return None;
        }
        Some(match base {
            "SwkbdCreate" | "SwkbdInitLearnDic" => 1,
            "SwkbdAppearInputForm" | "SwkbdAppearKeyboard" => {
                self.shown = true;
                self.decided = env_text.map(|t| Some(clean(t)));
                eprintln!(
                    "[swkbd] the game asks for text ({})",
                    if env_text.is_some() {
                        "MH3U_SWKBD_TEXT"
                    } else {
                        "type it in the overlay's keyboard window"
                    }
                );
                1
            }
            "SwkbdGetStateInputForm" | "SwkbdGetStateKeyboard" => {
                if self.shown {
                    SHOWN
                } else {
                    HIDDEN
                }
            }
            "SwkbdIsDecideOkButton" => match &self.decided {
                Some(Some(t)) if self.shown => {
                    self.text = t.clone();
                    eprintln!("[swkbd] OK: {t:?}");
                    self.decided = None;
                    1
                }
                _ => 0,
            },
            "SwkbdIsDecideCancelButton" => match &self.decided {
                Some(None) if self.shown => {
                    eprintln!("[swkbd] cancelled");
                    self.decided = None;
                    1
                }
                _ => 0,
            },
            "SwkbdGetInputFormString" => {
                write(TEXT_AT, &utf16be(&self.text));
                TEXT_AT
            }
            "SwkbdDisappearInputForm" | "SwkbdDisappearKeyboard" => {
                self.shown = false;
                self.decided = None;
                1
            }
            _ => 0,
        })
    }
}

/// A call from the game (see `Keyboard::call`).
pub fn call(name: &str, write: impl FnOnce(u32, &[u8])) -> Option<u32> {
    let env = std::env::var("MH3U_SWKBD_TEXT").ok();
    KEYBOARD.lock().unwrap_or_else(|e| e.into_inner()).call(name, env.as_deref(), write)
}

/// The game is waiting for text and nothing is decided yet (the overlay shows its keyboard window).
pub fn waiting() -> bool {
    let k = KEYBOARD.lock().unwrap_or_else(|e| e.into_inner());
    k.shown && k.decided.is_none()
}

/// The player's answer: `Some(text)` for OK, `None` to cancel.
pub fn submit(text: Option<&str>) {
    let mut k = KEYBOARD.lock().unwrap_or_else(|e| e.into_inner());
    if k.shown {
        k.decided = Some(text.map(clean));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(k: &mut Keyboard, name: &str, env: Option<&str>) -> (u32, Vec<u8>) {
        let mut out = Vec::new();
        let r = k
            .call(name, env, |at, b| {
                assert_eq!(at, TEXT_AT);
                out = b.to_vec();
            })
            .unwrap();
        (r, out)
    }

    #[test]
    fn creating_works_so_the_name_field_can_go_on() {
        let mut k = Keyboard::default();
        assert_eq!(call(&mut k, "SwkbdCreate__3RplFPUcQ3_2nn5swkbd10RegionTypeUiP8FSClient", None).0, 1);
        assert_eq!(
            call(&mut k, "SwkbdInitLearnDic__3RplFPv", None).0,
            1,
            "a 0 here stopped the keyboard being made"
        );
        assert_eq!(k.call("OSGetTime", None, |_, _| ()), None);
    }

    #[test]
    fn a_name_typed_is_given_on_ok() {
        let mut k = Keyboard::default();
        assert_eq!(call(&mut k, "SwkbdGetStateInputForm__3RplFv", None).0, HIDDEN);
        call(&mut k, "SwkbdAppearInputForm__3RplFRCQ3_2nn5swkbd9AppearArg", None);
        assert_eq!(call(&mut k, "SwkbdGetStateInputForm__3RplFv", None).0, SHOWN);
        assert_eq!(call(&mut k, "SwkbdIsDecideOkButton__3RplFPb", None).0, 0, "nothing typed yet");
        k.decided = Some(Some(clean("Shamus the Bold")));
        assert_eq!(call(&mut k, "SwkbdIsDecideOkButton__3RplFPb", None).0, 1);
        assert_eq!(call(&mut k, "SwkbdIsDecideOkButton__3RplFPb", None).0, 0, "once");
        let (at, bytes) = call(&mut k, "SwkbdGetInputFormString__3RplFv", None);
        assert_eq!(at, TEXT_AT);
        assert_eq!(bytes, utf16be("Shamus the"));
        call(&mut k, "SwkbdDisappearInputForm__3RplFv", None);
        assert_eq!(call(&mut k, "SwkbdGetStateInputForm__3RplFv", None).0, HIDDEN);
    }

    #[test]
    fn the_env_text_is_taken_at_once_and_cancel_works() {
        let mut k = Keyboard::default();
        call(&mut k, "SwkbdAppearInputForm__3RplFRCQ3_2nn5swkbd9AppearArg", Some("Ann"));
        assert_eq!(call(&mut k, "SwkbdIsDecideOkButton__3RplFPb", None).0, 1);
        assert_eq!(
            call(&mut k, "SwkbdGetInputFormString__3RplFv", None).1,
            vec![0, b'A', 0, b'n', 0, b'n', 0, 0]
        );
        call(&mut k, "SwkbdDisappearInputForm__3RplFv", None);
        call(&mut k, "SwkbdAppearInputForm__3RplFRCQ3_2nn5swkbd9AppearArg", None);
        k.decided = Some(None);
        assert_eq!(call(&mut k, "SwkbdIsDecideOkButton__3RplFPb", None).0, 0);
        assert_eq!(call(&mut k, "SwkbdIsDecideCancelButton__3RplFPb", None).0, 1);
    }

    #[test]
    fn the_text_fits_its_place() {
        assert_eq!(clean("a\nb"), "ab");
        assert_eq!(clean("ハンターハンターハンター").chars().count(), MOST);
        const { assert!(TEXT_AT >= crate::native_menu::DESC_AT + crate::native_menu::DESC_MOST as u32) };
        const { assert!(TEXT_AT + 2 * (MOST as u32 + 1) < crate::native_text::BUFFER + 0x10_0000) };
    }
}
