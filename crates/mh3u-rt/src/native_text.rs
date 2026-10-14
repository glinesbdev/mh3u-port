//! Opt-in mod text drawn by the game (`MH3U_NATIVE_TEXT=1`; `docs/native-menu.md`): in the item menu, the last empty pouch slot's
//! "empty" text is replaced by a line the overlay sets (the wishlist's status), so the game's own layout and font draw it.
//!
//! How: the `text_set` hook is on `f_02aff91c(pane, text, length)`, the layout's set-the-text call. Only the call that sets the
//! empty-slot text in the item row function (`f_02660a84`, returning to `0x02660e68`; the string is message 3/0x25c) is touched:
//! its `r4` and `r5` are pointed at this module's own buffer. Nothing else in the game is written. Anything unexpected (another
//! caller, an odd row or length, no text set) leaves the call as it is.

use crate::hooks::{Hook, Registry};
use std::sync::Mutex;

/// The mods' guest buffer: the last MiB below `.rodata`, at the top of the runtime's own area (`cafe.rs`, `OS_AREA`), which the
/// game never allocates from. The text goes in its first `MOST` bytes.
pub const BUFFER: u32 = 0x0ff0_0000;
/// The longest text put there (bytes, without the terminating 0).
pub const MOST: usize = 63;
/// Where `f_02660a84`'s call to set the empty-slot text returns to (update `.rpx`).
pub const EMPTY_SLOT_RETURN: u32 = 0x0266_0e68;
/// The rows the item list draws (`f_026611b8`: at most 8).
const ROWS: u32 = 8;

static TEXT: Mutex<Option<String>> = Mutex::new(None);
static STATE: Mutex<State> = Mutex::new(State::new());

/// Whether the opt-in is on (`MH3U_NATIVE_TEXT=1`).
pub fn enabled() -> bool {
    std::env::var("MH3U_NATIVE_TEXT").is_ok_and(|v| v.trim() == "1")
}

/// The line to show (`None`: leave the game's text).
pub fn set(text: Option<String>) {
    *TEXT.lock().unwrap_or_else(|e| e.into_inner()) = text;
}

/// The text as the game's strings are kept (8-bit, ending in 0): printable ASCII only, so any font has the glyphs.
pub fn encode(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| if c.is_ascii_graphic() || c == ' ' { c as u8 } else { b'?' })
        .take(MOST)
        .collect()
}

/// Which empty row is the last one: the rows come in order each frame, so the last empty row of the previous frame is used.
#[derive(Debug)]
pub struct State {
    previous_row: Option<u32>,
    this_frame: Option<u32>,
    last: Option<u32>,
    written: Vec<u8>,
}

impl State {
    pub const fn new() -> State {
        State {
            previous_row: None,
            this_frame: None,
            last: None,
            written: Vec::new(),
        }
    }

    /// An empty row's text is being set: whether this row is the one to show the mod text on.
    pub fn empty_row(&mut self, row: u32) -> bool {
        if self.previous_row.is_some_and(|p| row <= p) {
            // a new frame (or a redraw from the top)
            self.last = self.this_frame.take();
        }
        self.previous_row = Some(row);
        self.this_frame = Some(row);
        self.last == Some(row)
    }
}

impl Default for State {
    fn default() -> State {
        State::new()
    }
}

/// Registers the `text_set` callback when the opt-in is on; gives whether it did (off: nothing runs but the hook's lookup).
pub fn register(r: &mut Registry) -> bool {
    if !enabled() {
        return false;
    }
    let on = r.on("text_set", Box::new(|h: &mut Hook| swap(h)));
    if on {
        eprintln!("[native-text] on: the item menu's last empty slot shows the mod text (buffer {BUFFER:#x}; MH3U_NATIVE_TEXT)");
    } else {
        eprintln!("[native-text] MH3U_NATIVE_TEXT is set but the build has no text_set hook (translate again with mods/hooks.txt)");
    }
    on
}

fn swap(h: &mut Hook) {
    let c = &mut *h.ctx;
    if c.lr != EMPTY_SLOT_RETURN {
        return;
    }
    // the row is in r24 of the row function (callee-saved, set from its r4); the text's length is the game's
    let (row, length) = (c.r[24], c.r[5]);
    if row >= ROWS || c.r[4] == 0 || length > 256 {
        return;
    }
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if !state.empty_row(row) {
        return;
    }
    let Some(text) = TEXT.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        return;
    };
    let bytes = encode(&text);
    if bytes.is_empty() {
        return;
    }
    if state.written != bytes {
        let g = crate::guest::Guest::new(c.mem);
        g.write(BUFFER, &bytes);
        g.write(BUFFER + bytes.len() as u32, &[0]);
        eprintln!(
            "[native-text] row {row}: \"{}\" (in place of the empty-slot text)",
            String::from_utf8_lossy(&bytes)
        );
        state.written = bytes;
    }
    c.r[4] = BUFFER;
    c.r[5] = state.written.len() as u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_empty_row_of_the_previous_frame_gets_the_text() {
        let mut s = State::new();
        // frame 1: rows 3, 5 and 7 are empty; nothing is known yet
        assert!(![3, 5, 7].map(|r| s.empty_row(r)).contains(&true));
        // frame 2: the same rows; 7 was the last
        assert_eq!([3, 5, 7].map(|r| s.empty_row(r)), [false, false, true]);
        // an item went into row 7: frame 3 still uses 7 (it is not empty, so nothing shows), frame 4 uses 5
        assert_eq!([3, 5].map(|r| s.empty_row(r)), [false, false]);
        assert_eq!([3, 5].map(|r| s.empty_row(r)), [false, true]);
    }

    #[test]
    fn text_is_printable_ascii_and_bounded() {
        assert_eq!(encode("Wishlist: 2 ready"), b"Wishlist: 2 ready".to_vec());
        assert_eq!(encode("✔ é"), b"? ?".to_vec());
        assert_eq!(encode(&"x".repeat(200)).len(), MOST);
    }

    #[test]
    fn the_buffer_is_above_the_runtime_area_and_below_the_game_data() {
        const { assert!(BUFFER > 0x0e00_0000 && BUFFER + MOST as u32 + 1 < 0x1000_0000) };
    }
}
