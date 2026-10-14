//! Opt-in "Mods" entry in the start menu (`MH3U_NATIVE_MENU=1`; `docs/native-menu.md`, "The Mods entry"): while the mod key is
//! held (default Right Shift, `MH3U_NATIVE_MENU_KEY`), the Hunter tab's Gestures row (tab 0, row 4) reads "Mods", and A on it
//! opens the Mods page instead of Gestures: the game's own Combo List page, with its rows replaced by the wishlist (a title and
//! one line a piece) and only up, down and B let through. Without the key, or with the opt-in off, the game is unchanged.
//!
//! How (all on the start menu's state block `S`, `menu + 0x34`):
//!
//! - **The label:** `f_0215249c` refills the rows' label pointers (`S+0xb0 + i*0x18 + 8`) every frame and `f_02152874(menu, S,
//!   cursor)` runs right after it (`menu_rows`; only its 3 calls from the update count). On that hook, with the key held and the Hunter tab up, row 4's pointer is set to
//!   "Mods" in the runtime's buffer; the row draw (`f_026615a8`) reads the pointer each frame. When the key is let go the game's
//!   own pointer is written back (the game would refill it on its next frame anyway).
//! - **A:** on `f_02154abc(menu, S)` (`menu_update`), with the key held, the tab list up (`S+0x1f` = 0), tab 0, row 4 and A in the
//!   buttons (`S+4` bit `0x10`), the cursor (`S+0x28`) is moved to row 2 before the game reads it, so the game's own A path opens
//!   the Combo List (`f_02152a88`). The cursor is put back on row 4 when the start menu runs again.
//! - **The page:** on the Combo List's update (`f_02234218`, `combo_update`) the buttons are cut down to up, down and B, and the
//!   page's rows in `S` are emptied (`empty_rows`), so the row draw (`f_026611b8` -> `f_02660a84`, the item page's) sets each
//!   row's empty-slot text; `text_set` at that call (`0x02660e68`, row in `r24`) points it at the Mods line for the row.
//!
//! The only writes: text in the runtime's own buffer, row 4's label pointer, the cursor, the buttons of the frame, and the
//! Combo List's display rows in `S` (rebuilt by the game each time the page opens). No save data.

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
/// The row whose page the Mods page borrows: Combo List (tab 0, row 2; `f_02152a88`).
pub const PAGE_ROW: u16 = 2;
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
#[derive(Debug)]
pub struct State {
    /// The row's label field and the game's pointer that was in it.
    replaced: Option<(u32, u32)>,
    /// A was being kept from Gestures last frame (to log once per press).
    pressing: bool,
    /// Whether the label text is in the buffer.
    written: bool,
    /// The Mods page, from A on the Mods row until the start menu runs again.
    page: Option<Page>,
    /// Each page row's text as last written.
    page_written: [Vec<u8>; PAGE_LINES],
}

/// The Mods page while it is up.
#[derive(Debug, Default)]
pub struct Page {
    /// The Combo List's update has run (the game opened the page).
    opened: bool,
    /// Start menu updates since the page update last ran.
    menu_frames: u32,
    odd_logged: bool,
}

impl State {
    pub const fn new() -> State {
        State {
            replaced: None,
            pressing: false,
            written: false,
            page: None,
            page_written: [const { Vec::new() }; PAGE_LINES],
        }
    }
}

impl Default for State {
    fn default() -> State {
        State::new()
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
    // the start menu runs again: the Mods page is closed (or the game did not open it)
    if let Some(page) = st.page.as_mut() {
        page.menu_frames += 1;
        if page.menu_frames > 2 {
            let opened = page.opened;
            st.page = None;
            if g.u16(s + 0x28) == PAGE_ROW {
                g.put_u16(s + 0x28, ROW);
            }
            eprintln!(
                "[native-menu] Mods page {}",
                if opened {
                    "closed"
                } else {
                    "not opened (the Combo List row is off)"
                }
            );
        }
    }
    let buttons = g.u32(s + 4);
    let take = HELD.load(Relaxed) && v.on_row() && buttons & A != 0;
    if take && !st.pressing && st.page.is_none() && page_ready() {
        eprintln!("[native-menu] Mods selected: opening the Mods page (the Combo List page, its rows replaced)");
        // the game's own A path then opens row 2, the Combo List (`f_02152a88`); the page update is caught below
        g.put_u16(s + 0x28, PAGE_ROW);
        st.page = Some(Page::default());
    } else if take {
        if !st.pressing {
            eprintln!("[native-menu] Mods selected (no page: the build lacks its hooks)");
        }
        g.put_u32(s + 4, buttons & !A);
    }
    st.pressing = take;
}

/// The Combo List's update (`f_02234218(menu, S)`, `combo_update`) while the Mods page is up: only up, down and B reach the
/// game, and its rows are emptied each frame so the row draw sets the empty-slot text, which `text_set` replaces.
/// Whether the page's hooks are in the build (without them A on Mods only logs, as before).
fn page_ready() -> bool {
    PAGE_HOOKS.load(Relaxed)
}
static PAGE_HOOKS: AtomicBool = AtomicBool::new(false);

fn page_update(h: &mut Hook) {
    let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(page) = st.page.as_mut() else { return };
    let (menu, s) = (h.ctx.r[3], h.ctx.r[4]);
    if s != menu.wrapping_add(0x34) {
        if !page.odd_logged {
            eprintln!("[native-menu] combo_update: r4 {s:#x} is not menu + 0x34 ({menu:#x}); the Mods page leaves this call alone");
            page.odd_logged = true;
        }
        return;
    }
    page.opened = true;
    page.menu_frames = 0;
    let g = crate::guest::Guest::new(h.ctx.mem);
    if g.u8(s + 1) != 0 {
        // a state the page never enters (combining, reordering): leave the game alone
        return;
    }
    g.put_u32(s + 4, g.u32(s + 4) & PAGE_BUTTONS);
    g.put_u32(s + 0xc, g.u32(s + 0xc) & !0xc);
    empty_rows(&g, s);
}

/// The buttons the Mods page lets through: up and down (`0x3`) and B (`0x20`).
pub const PAGE_BUTTONS: u32 = 0x23;

/// The Combo List's rows as "nothing there": the same values the game writes for a row past the end (`f_022338e8`) and for no
/// selected combo (`f_02233700`). This is the page's display state in `S`, rebuilt by the game when the page opens; no save data.
fn empty_rows(g: &crate::guest::Guest, s: u32) {
    for i in 0..8 {
        g.write(s + 0x26c + i, &[0xff]);
        g.write(s + 0x274 + i, &[0]);
        g.put_u32(s + 0x27c + i * 4, 0);
    }
    g.put_u32(s + 0x230, 0);
    g.put_u16(s + 0x234, 0);
    g.put_u16(s + 0x236, 0);
    g.put_u16(s + 0x238, 0xffff);
    g.put_u16(s + 0x23a, 0xffff);
    g.write(s + 0x245, &[0]);
    g.write(s + 0x24b, &[0]);
}

/// The row draw's call setting an empty row's text (`native_text::EMPTY_SLOT_RETURN`, row in `r24`): on the Mods page, the
/// row's own line.
fn page_text(h: &mut Hook) {
    let c = &mut *h.ctx;
    if c.lr != crate::native_text::EMPTY_SLOT_RETURN {
        return;
    }
    let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if !st.page.as_ref().is_some_and(|p| p.opened) {
        return;
    }
    let row = c.r[24];
    if row >= PAGE_LINES as u32 || c.r[4] == 0 {
        return;
    }
    let lines = page_lines(&LINES.lock().unwrap_or_else(|e| e.into_inner()));
    let bytes = crate::native_text::encode(&lines[row as usize]);
    let at = PAGE_TEXT_AT + row * PAGE_TEXT_STRIDE;
    let g = crate::guest::Guest::new(c.mem);
    if st.page_written[row as usize] != bytes {
        g.write(at, &bytes);
        g.write(at + bytes.len() as u32, &[0]);
        st.page_written[row as usize] = bytes.clone();
    }
    c.r[4] = at;
    c.r[5] = bytes.len() as u32;
}

/// The Mods page's rows.
pub const PAGE_LINES: usize = 8;
/// Where the rows' text goes in the runtime's buffer (64 bytes each).
pub const PAGE_TEXT_AT: u32 = crate::native_text::BUFFER + 0x200;
const PAGE_TEXT_STRIDE: u32 = 0x40;

/// The wishlist's lines, set by the overlay (`None`: no data yet).
static LINES: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// The wishlist's lines for the Mods page, one a piece (the overlay calls this whenever it works the status out).
pub fn set_lines(lines: Option<Vec<String>>) {
    *LINES.lock().unwrap_or_else(|e| e.into_inner()) = lines;
}

/// The page's 8 rows: a title, then the lines (the last row says how many more there are when they do not fit).
pub fn page_lines(lines: &Option<Vec<String>>) -> Vec<String> {
    let mut out = vec!["Mods: Wishlist".to_string()];
    match lines {
        None => out.push("No wishlist data yet (F1 opens the overlay)".into()),
        Some(l) if l.is_empty() => out.push("The wishlist is empty (add pieces in the F1 overlay)".into()),
        Some(l) => {
            let room = PAGE_LINES - 1;
            if l.len() <= room {
                out.extend(l.iter().cloned());
            } else {
                out.extend(l[..room - 1].iter().cloned());
                out.push(format!("...and {} more (F1)", l.len() - (room - 1)));
            }
        }
    }
    out.resize(PAGE_LINES, String::new());
    out
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
    let page = r.on("combo_update", Box::new(page_update));
    let text = r.on("text_set", Box::new(page_text));
    PAGE_HOOKS.store(page && text, Relaxed);
    if !(page && text) {
        eprintln!(
            "[native-menu] the build lacks combo_update ({page}) or text_set ({text}): A on Mods would open the plain Combo List; translate again"
        );
    }
    if rows && upd {
        eprintln!(
            "[native-menu] on (MH3U_NATIVE_MENU): hold {} in the start menu's Hunter tab for the Mods row (Gestures)",
            key()
        );
    } else {
        eprintln!("[native-menu] MH3U_NATIVE_MENU is set but the build lacks hooks (menu_rows {rows}, menu_update {upd}): translate again");
    }
    rows && upd && page && text
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
    fn the_page_has_a_title_and_one_line_a_piece() {
        let p = page_lines(&None);
        assert_eq!(p.len(), PAGE_LINES);
        assert_eq!(p[0], "Mods: Wishlist");
        assert!(p[1].starts_with("No wishlist data"));
        assert!(page_lines(&Some(vec![]))[1].contains("empty"));
        let few = page_lines(&Some(vec!["Bone Helm: ready".into()]));
        assert_eq!((few[1].as_str(), few[2].as_str()), ("Bone Helm: ready", ""));
        let many: Vec<String> = (0..10).map(|i| format!("piece {i}")).collect();
        let p = page_lines(&Some(many));
        assert_eq!(p[6], "piece 5");
        assert_eq!(p[7], "...and 4 more (F1)");
    }

    #[test]
    fn the_page_text_does_not_overlap_the_other_buffers() {
        const {
            assert!(PAGE_TEXT_AT >= LABEL_AT + 0x100);
            assert!(PAGE_TEXT_AT + PAGE_LINES as u32 * PAGE_TEXT_STRIDE < crate::native_text::BUFFER + 0x10_0000);
            assert!(crate::native_text::MOST < PAGE_TEXT_STRIDE as usize);
        }
    }

    #[test]
    fn the_label_is_in_the_runtimes_buffer_past_the_native_text() {
        assert!(LABEL_AT > crate::native_text::BUFFER + crate::native_text::MOST as u32);
        assert!(LABEL_AT + (LABEL.len() as u32) < crate::native_text::BUFFER + 0x10_0000);
    }
}
