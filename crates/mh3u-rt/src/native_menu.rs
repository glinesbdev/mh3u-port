//! Opt-in "Mods" entry in the start menu (`MH3U_NATIVE_MENU=1`; `docs/native-menu.md`, "The Mods entry"): while the mod key is
//! held (default Right Shift, `MH3U_NATIVE_MENU_KEY`), the Hunter tab's Gestures row (tab 0, row 4) reads "Mods", and A on it
//! logs `[native-menu] Mods selected` instead of opening Gestures (the menu stays up). Without the key, or with the opt-in off,
//! the game is unchanged.
//!
//! How (all on the start menu's state block `S`, `menu + 0x34`):
//!
//! - **The label:** `f_0215249c` refills the rows' label pointers (`S+0xb0 + i*0x18 + 8`) every frame and `f_02152874(menu, S,
//!   cursor)` runs right after it (`menu_rows`; only its 3 calls from the update count). On that hook, with the key held and the Hunter tab up, row 4's pointer is set to
//!   "Mods" in the runtime's buffer; the row draw (`f_026615a8`) reads the pointer each frame. When the key is let go the game's
//!   own pointer is written back (the game would refill it on its next frame anyway).
//! - **A:** on `f_02154abc(menu, S)` (`menu_update`), with the key held, the tab list up (`S+0x1f` = 0), tab 0, row 4 and A in the
//!   buttons (`S+4` bit `0x10`), the A bit is cleared before the game reads it, so its if/else chain never reaches Gestures.
//!
//! The only writes: the label text in the runtime's own buffer, row 4's label pointer, and the A bit of that frame's buttons.

use crate::hooks::{Hook, Registry};
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// Where the label goes: past the native text's (`native_text::BUFFER`, 64 bytes), in the runtime's own MiB.
pub const LABEL_AT: u32 = crate::native_text::BUFFER + 0x100;
pub const LABEL: &[u8] = b"Mods";
/// The Hunter tab and its Gestures row.
pub const TAB: u8 = 0;
pub const ROW: u16 = 4;
const RECORD: u32 = 0x18;
/// `f_02152874` has some 60 callers (other menus share the row helpers); only its calls from the start menu's update count:
/// `0x02154b1c`, `0x0215500c` and `0x02155048` (Ghidra xrefs), returning 4 bytes on.
pub const ROWS_RETURNS: [u32; 3] = [0x0215_4b20, 0x0215_5010, 0x0215_504c];
const A: u32 = 0x10;
/// The default mod key (winit's name for it): unbound in the default keyboard controls (`docs/input.md`).
pub const DEFAULT_KEY: &str = "ShiftRight";

/// Whether the mod key is down (set by the window, `mh3u-gfx`).
pub static HELD: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<State> = Mutex::new(State::new());

pub fn enabled() -> bool {
    std::env::var("MH3U_NATIVE_MENU").is_ok_and(|v| v.trim() == "1")
}

/// The mod key's name, as winit names keys (`ShiftRight`, `AltLeft`, `KeyM`...): `MH3U_NATIVE_MENU_KEY` or the default.
pub fn key() -> &'static str {
    static KEY: OnceLock<String> = OnceLock::new();
    KEY.get_or_init(|| {
        std::env::var("MH3U_NATIVE_MENU_KEY")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_KEY.to_string())
    })
}

/// What was changed, to put back.
#[derive(Debug, Default)]
pub struct State {
    /// The row's label field and the game's pointer that was in it.
    replaced: Option<(u32, u32)>,
    /// A was being kept from Gestures last frame (to log once per press).
    pressing: bool,
    /// Whether the label text is in the buffer.
    written: bool,
}

impl State {
    pub const fn new() -> State {
        State {
            replaced: None,
            pressing: false,
            written: false,
        }
    }
}

/// What the menu's state says about the Mods row this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View {
    pub sub: u8,
    pub tab: u8,
    pub tabs: u8,
    pub rows: i16,
    pub row: i16,
}

impl View {
    /// The Hunter tab is up with the row on it (the label applies).
    pub fn shows_row(&self) -> bool {
        self.sub == 0 && self.tab == TAB && self.tabs > TAB && self.rows > ROW as i16
    }
    /// The cursor is on the row (A applies).
    pub fn on_row(&self) -> bool {
        self.shows_row() && self.row == ROW as i16
    }
}

fn view(g: &crate::guest::Guest, s: u32) -> View {
    View {
        sub: g.u8(s + 0x1f),
        tab: g.u8(s + 0x25),
        tabs: g.u8(s + 0x24),
        rows: g.u16(s + 0x20) as i16,
        row: g.u16(s + 0x28) as i16,
    }
}

/// Puts the game's label back if it was replaced.
fn restore(g: &crate::guest::Guest, st: &mut State) {
    if let Some((field, original)) = st.replaced.take() {
        if g.u32(field) == LABEL_AT {
            g.put_u32(field, original);
        }
        eprintln!("[native-menu] Gestures label restored");
    }
}

/// Sets or puts back row 4's label for this frame.
fn label(g: &crate::guest::Guest, s: u32, st: &mut State) {
    let v = view(g, s);
    let field = s + 0xb0 + u32::from(ROW) * RECORD + 8;
    if !(HELD.load(Relaxed) && v.shows_row()) {
        restore(g, st);
        return;
    }
    if !st.written {
        g.write(LABEL_AT, LABEL);
        g.write(LABEL_AT + LABEL.len() as u32, &[0]);
        st.written = true;
    }
    let current = g.u32(field);
    if current == LABEL_AT {
        return;
    }
    if current == 0 {
        // nothing to stand in for: the row is not set up
        return;
    }
    if st.replaced.is_none_or(|(f, _)| f != field) {
        eprintln!(
            "[native-menu] Gestures row reads \"Mods\" (was {current:#x} \"{}\")",
            g.cstr(current).chars().take(40).collect::<String>()
        );
    }
    st.replaced = Some((field, current));
    g.put_u32(field, LABEL_AT);
}

fn update(h: &mut Hook) {
    let s = h.ctx.r[4];
    let g = crate::guest::Guest::new(h.ctx.mem);
    let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let v = view(&g, s);
    if !HELD.load(Relaxed) || !v.shows_row() {
        restore(&g, &mut st);
    }
    let buttons = g.u32(s + 4);
    let take = HELD.load(Relaxed) && v.on_row() && buttons & A != 0;
    if take {
        g.put_u32(s + 4, buttons & !A);
        if !st.pressing {
            eprintln!("[native-menu] Mods selected");
        }
    }
    st.pressing = take;
}

/// Registers the callbacks when `MH3U_NATIVE_MENU=1`.
pub fn register(r: &mut Registry) -> bool {
    if !enabled() {
        return false;
    }
    let rows = r.on(
        "menu_rows",
        Box::new(|h: &mut Hook| {
            if !ROWS_RETURNS.contains(&h.ctx.lr) {
                return;
            }
            let s = h.ctx.r[4];
            let g = crate::guest::Guest::new(h.ctx.mem);
            label(&g, s, &mut STATE.lock().unwrap_or_else(|e| e.into_inner()));
        }),
    );
    let upd = r.on("menu_update", Box::new(update));
    if rows && upd {
        eprintln!(
            "[native-menu] on (MH3U_NATIVE_MENU): hold {} in the start menu's Hunter tab for the Mods row (Gestures)",
            key()
        );
    } else {
        eprintln!("[native-menu] MH3U_NATIVE_MENU is set but the build lacks hooks (menu_rows {rows}, menu_update {upd}): translate again");
    }
    rows && upd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_applies_only_on_the_hunter_tab_list() {
        let v = View {
            sub: 0,
            tab: 0,
            tabs: 2,
            rows: 6,
            row: 4,
        };
        assert!(v.shows_row() && v.on_row());
        assert!(!View { row: 3, ..v }.on_row());
        assert!(View { row: 3, ..v }.shows_row());
        assert!(!View { tab: 1, ..v }.shows_row());
        assert!(!View { sub: 2, ..v }.shows_row(), "not in a second-level list");
        assert!(!View { rows: 4, ..v }.shows_row(), "a mode with fewer rows");
    }

    #[test]
    fn the_label_is_in_the_runtimes_buffer_past_the_native_text() {
        assert!(LABEL_AT > crate::native_text::BUFFER + crate::native_text::MOST as u32);
        assert!(LABEL_AT + (LABEL.len() as u32) < crate::native_text::BUFFER + 0x10_0000);
    }
}
