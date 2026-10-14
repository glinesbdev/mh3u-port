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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

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
    /// Each text slot's text as last written, and whether it was written.
    page_written: [Vec<u8>; SLOTS],
    slot_set: [bool; SLOTS],
}

/// The Mods page while it is up.
#[derive(Debug, Default)]
pub struct Page {
    /// The Combo List's update has run (the game opened the page).
    opened: bool,
    /// Start menu updates since the page update last ran.
    menu_frames: u32,
    odd_logged: bool,
    /// The Mods page's own page (8 rows each).
    page: usize,
    /// The side panel object (`f_0265bfac`'s `r3`; 0 not seen yet).
    panel: u32,
    /// The wishlist piece under the cursor.
    hovered: Option<Entry>,
    /// The material ids last given to the panel.
    last_ids: Option<(u16, u16)>,
}

impl State {
    pub const fn new() -> State {
        State {
            replaced: None,
            pressing: false,
            written: false,
            page: None,
            page_written: [const { Vec::new() }; SLOTS],
            slot_set: [false; SLOTS],
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
    if !(modifier() && v.shows_row()) {
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
    menu_seen();
    if !modifier() || !v.shows_row() {
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
    let take = modifier() && v.on_row() && buttons & A != 0;
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

/// Whether the page's hooks are in the build (without them A on Mods only logs, as before).
fn page_ready() -> bool {
    PAGE_HOOKS.load(Relaxed)
}
static PAGE_HOOKS: AtomicBool = AtomicBool::new(false);

/// Whether the modifier is held: the mod key, or ZL on a gamepad.
pub fn modifier() -> bool {
    HELD.load(Relaxed) || crate::input::INPUT.pad_buttons() & crate::input::button::ZL != 0
}

/// When the start menu or the Mods page last ran (ms since `EPOCH`); 0 never.
static MENU_SEEN: AtomicU64 = AtomicU64::new(0);
static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();

fn now_ms() -> u64 {
    EPOCH.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64 + 1
}

fn menu_seen() {
    MENU_SEEN.store(now_ms(), Relaxed);
}

/// The buttons kept from the game: ZL while the start menu or the Mods page is up (it is the Mods modifier there), with the
/// opt-in on. Read by `Input::hold` (the game's `VPADRead`).
pub fn swallowed() -> u32 {
    let seen = MENU_SEEN.load(Relaxed);
    if seen != 0 && now_ms().saturating_sub(seen) < 200 {
        crate::input::button::ZL
    } else {
        0
    }
}

/// The Combo List's update (`f_02234218(menu, S)`, `combo_update`) while the Mods page is up: only up, down and B reach the
/// game; left and right turn the Mods page's own pages; the rows are emptied each frame so the row draw sets the empty-slot
/// text, which `text_set` replaces; the page's display state is set for the side panel (the hovered piece's materials).
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
    menu_seen();
    let g = crate::guest::Guest::new(h.ctx.mem);
    if g.u8(s + 1) != 0 {
        // a state the page never enters (combining, reordering): leave the game alone
        return;
    }
    let entries = ENTRIES.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let pages = page_count(entries.as_deref());
    let turn = (g.u32(s + 4) | g.u32(s + 0xc)) & 0xc;
    page.page = turn_page(page.page.min(pages - 1), pages, turn);
    g.put_u32(s + 4, g.u32(s + 4) & PAGE_BUTTONS);
    g.put_u32(s + 0xc, g.u32(s + 0xc) & !0xc);
    empty_rows(&g, s);
    // the page counter ("1/3"): the row draw shows S+0x1e8 + 1 of S+0x1e9
    g.write(s + 0x1e8, &[page.page as u8]);
    g.write(s + 0x1e9, &[pages as u8]);
    g.put_u16(s + 0x1ec, PAGE_LINES as u16);
    // the hovered piece (the side panel is set up just before it is drawn, `panel`: the update rewrites it after this hook)
    let row = (g.u16(s + 0x1ea) as usize).min(PAGE_LINES - 1);
    let hovered = entries.as_ref().and_then(|e| e.get(page.page * PAGE_LINES + row)).cloned();
    page.hovered = hovered;
}

/// The page after a left (`0x4`) or right (`0x8`) press, wrapping.
pub fn turn_page(page: usize, pages: usize, turn: u32) -> usize {
    let pages = pages.max(1);
    if turn & 0x4 != 0 {
        (page + pages - 1) % pages
    } else if turn & 0x8 != 0 {
        (page + 1) % pages
    } else {
        page % pages
    }
}

/// Pages of 8 rows over the wishlist (at least 1).
pub fn page_count(entries: Option<&[Entry]>) -> usize {
    entries.map_or(1, |e| e.len().div_ceil(PAGE_LINES).max(1))
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

/// The side panel's draw (`f_0265bfac(panel, S)`, `combo_panel`), after the update: keeps the panel object (so `text_set` can
/// tell its panes by pointer) and sets the panel's state in `S` (`S+0x230`) for the hovered piece. The update's
/// `f_02233834` -> `f_02233700`/`f_02231db8` rewrite that block every frame, so it is set here, right before the draw:
///
/// - `+0x230` = 0 and `+0x245` = 0: no combo, so `f_0265b2f0` takes its no-combo path (name `+0x2b4`, output `+0x2d0`);
/// - `+0x244` = 1: that path's status line is the red one, `+0x2e4` (`f_0228f57c`, message 0x168), which shows the status;
/// - `+0x234`/`+0x236`: two item ids for `f_0265bb24` (it returns early on 0): the hovered piece's first two materials, or the
///   last ones shown with their names and counts blanked, so no stale text stays.
fn panel(h: &mut Hook) {
    let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(page) = st.page.as_mut() else { return };
    if !page.opened {
        return;
    }
    let (panel, s) = (h.ctx.r[3], h.ctx.r[4]);
    page.panel = panel;
    let g = crate::guest::Guest::new(h.ctx.mem);
    if s == 0 || g.u8(s + 1) != 0 || g.u8(s + 0x1e) != 2 {
        return;
    }
    let (m1, m2) = match page.hovered.as_ref().map(|e| e.materials.as_slice()) {
        Some([a, b, ..]) => (a.item, b.item),
        Some([a]) => (a.item, a.item),
        _ => page.last_ids.unwrap_or(FALLBACK_IDS),
    };
    page.last_ids = Some((m1, m2));
    g.put_u32(s + 0x230, 0);
    g.write(s + 0x245, &[0]);
    g.write(s + 0x244, &[1]);
    g.put_u16(s + 0x234, m1);
    g.put_u16(s + 0x236, m2);
}

/// Item ids for the materials function when no piece has been shown yet (their names and counts are blanked).
const FALLBACK_IDS: (u16, u16) = (1, 1);

/// The panel's text panes (offsets in the panel object, `f_0265b2f0` and `f_0265bb24`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// `+0x2b4`: the output name ("??????"): the piece's name.
    Name,
    /// `+0x2d0`: the value next to the layout's "Output:" label: blank.
    Output,
    /// `+0x2e4`: the red line ("Insufficient materials.", message 0x168): the piece's status.
    Status,
    /// `+0x2cc` and `+0x2d4`: the other status lines (hidden or cleared on this path; blanked if set).
    Other,
    /// `+0x2bc`, `+0x2c4`: the materials' names; `+0x2c0`, `+0x2c8`: their counts.
    Name1,
    Name2,
    Count1,
    Count2,
}

const PANES: [(u32, Pane); 9] = [
    (0x2b4, Pane::Name),
    (0x2d0, Pane::Output),
    (0x2e4, Pane::Status),
    (0x2cc, Pane::Other),
    (0x2d4, Pane::Other),
    (0x2bc, Pane::Name1),
    (0x2c4, Pane::Name2),
    (0x2c0, Pane::Count1),
    (0x2c8, Pane::Count2),
];

/// The text for a panel pane, for the hovered entry (`None`: leave the game's).
pub fn pane_text(pane: Pane, hovered: Option<&Entry>, entries: Option<&[Entry]>) -> Option<String> {
    let Some(e) = hovered else {
        return Some(match (pane, entries) {
            (Pane::Name, _) => "Wishlist".into(),
            (Pane::Status, None) => "No wishlist data yet (F1)".into(),
            (Pane::Status, Some([])) => "The wishlist is empty (F1)".into(),
            (Pane::Status, Some(l)) => format!("{} of {} ready", l.iter().filter(|e| e.ready).count(), l.len()),
            _ => String::new(),
        });
    };
    let count = |i: usize| e.materials.get(i).map(|m| format!("{}/{}", m.have, m.need));
    Some(match pane {
        Pane::Name => e.name.clone(),
        Pane::Status => {
            let more = match e.materials.len() {
                0..=2 => String::new(),
                n => format!(" (+{} more)", n - 2),
            };
            if e.ready {
                format!("Ready to craft{more}")
            } else {
                let missing: u32 = e.materials.iter().map(|m| m.need.saturating_sub(m.have)).sum();
                format!("Need {missing} more{more}")
            }
        }
        Pane::Output | Pane::Other => String::new(),
        Pane::Count1 => count(0).unwrap_or_default(),
        Pane::Count2 => count(1).unwrap_or_default(),
        Pane::Name1 if e.materials.is_empty() => String::new(),
        Pane::Name2 if e.materials.len() < 2 => String::new(),
        Pane::Name1 | Pane::Name2 => return None,
    })
}

/// `text_set` return addresses: the page title (`f_0265f010`, the page setup) and the Combo List's order label
/// ("Default Order", `f_026636d4`).
pub const TITLE_RETURNS: [u32; 2] = [0x0265_fa48, 0x0265_fa98];
pub const ORDER_RETURNS: [u32; 3] = [0x0266_390c, 0x0266_3974, 0x0266_39d4];
/// `f_02144d18(ui, text, 0)`'s call for the help line in the Combo List's draw returns here.
pub const HELP_RETURN: u32 = 0x0266_3c50;
/// The other help-line call (`f_02144998`, taken when `S+0x18` is set) returns here.
pub const HELP_RETURN_ALT: u32 = 0x0266_3c34;

/// Text slots in the runtime's buffer, 64 bytes each from `PAGE_TEXT_AT` (0 to 7 the rows).
const SLOT_TITLE: usize = 8;
const SLOT_ORDER: usize = 9;
const SLOT_HELP: usize = 10;
const SLOT_PANE: usize = 11;
const SLOTS: usize = SLOT_PANE + PANES.len();

/// Writes `text` into a slot (when it changed) and points the call's `r4`/`r5` at it.
fn put_text(c: &mut crate::Ctx, st: &mut State, slot: usize, text: &str, length_in_r5: bool) {
    let bytes = crate::native_text::encode(text);
    let at = PAGE_TEXT_AT + slot as u32 * PAGE_TEXT_STRIDE;
    if st.page_written[slot] != bytes || !st.slot_set[slot] {
        let g = crate::guest::Guest::new(c.mem);
        g.write(at, &bytes);
        g.write(at + bytes.len() as u32, &[0]);
        st.page_written[slot] = bytes.clone();
        st.slot_set[slot] = true;
    }
    c.r[4] = at;
    if length_in_r5 {
        c.r[5] = bytes.len() as u32;
    }
}

/// `text_set` on the Mods page: the rows (the empty-slot call, row in `r24`), the title, the order label and the panel's panes.
fn page_text(h: &mut Hook) {
    let c = &mut *h.ctx;
    let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(page) = st.page.as_ref() else { return };
    if c.r[4] == 0 {
        return;
    }
    let (opened, panel, hovered, page_no) = (page.opened, page.panel, page.hovered.clone(), page.page);
    trace(c);
    if TITLE_RETURNS.contains(&c.lr) {
        put_text(c, &mut st, SLOT_TITLE, "Wishlist", true);
        return;
    }
    if !opened {
        return;
    }
    if c.lr == crate::native_text::EMPTY_SLOT_RETURN {
        let row = c.r[24] as usize;
        if row >= PAGE_LINES {
            return;
        }
        let lines = page_lines(ENTRIES.lock().unwrap_or_else(|e| e.into_inner()).as_deref(), page_no);
        put_text(c, &mut st, row, &lines[row], true);
        return;
    }
    if ORDER_RETURNS.contains(&c.lr) {
        put_text(c, &mut st, SLOT_ORDER, "Left/Right: page", true);
        return;
    }
    if panel == 0 {
        return;
    }
    let g = crate::guest::Guest::new(c.mem);
    let pane = c.r[3];
    let Some(&(_, which)) = PANES.iter().find(|(off, _)| g.u32(panel + off) == pane) else {
        return;
    };
    let entries = ENTRIES.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(text) = pane_text(which, hovered.as_ref(), entries.as_deref()) {
        let slot = SLOT_PANE + PANES.iter().position(|(off, _)| g.u32(panel + off) == pane).unwrap_or(0);
        put_text(c, &mut st, slot, &text, true);
    }
}

/// `MH3U_NATIVE_MENU_TRACE=1`: logs each text set while the Mods page is up, once per (caller, pane), with its text, to find
/// the panes still showing the Combo List's text (the button hints).
fn trace(c: &crate::Ctx) {
    static ON: OnceLock<bool> = OnceLock::new();
    static SEEN: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
    if !*ON.get_or_init(|| std::env::var("MH3U_NATIVE_MENU_TRACE").is_ok_and(|v| v.trim() == "1")) {
        return;
    }
    let key = (c.lr, c.r[3]);
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.contains(&key) || seen.len() > 400 {
        return;
    }
    seen.push(key);
    let g = crate::guest::Guest::new(c.mem);
    let text: String = g
        .cstr(c.r[4])
        .chars()
        .take(60)
        .map(|ch| if ch.is_ascii_graphic() || ch == ' ' { ch } else { '?' })
        .collect();
    eprintln!("[native-menu] trace: text_set lr {:#x} pane {:#x}: \"{text}\"", c.lr, c.r[3]);
}

/// The help line (`f_02144d18(ui, text, 0)`, `help_set`) from the Combo List's draw, on the Mods page.
fn help(h: &mut Hook) {
    let c = &mut *h.ctx;
    if !(c.lr == HELP_RETURN || c.lr == HELP_RETURN_ALT) || c.r[4] == 0 {
        return;
    }
    let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if !st.page.as_ref().is_some_and(|p| p.opened) {
        return;
    }
    put_text(c, &mut st, SLOT_HELP, "Wishlist (Left/Right: page, B: back)", false);
}

/// The Mods page's rows a page.
pub const PAGE_LINES: usize = 8;
/// Where the page's text goes in the runtime's buffer (64 bytes a slot).
pub const PAGE_TEXT_AT: u32 = crate::native_text::BUFFER + 0x200;
const PAGE_TEXT_STRIDE: u32 = 0x40;

/// A wishlist piece as the Mods page shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub ready: bool,
    pub materials: Vec<Material>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Material {
    pub item: u16,
    pub have: u32,
    pub need: u32,
}

impl Entry {
    /// The row: `Name: ready` or `Name: need N`.
    pub fn line(&self) -> String {
        if self.ready {
            format!("{}: ready", self.name)
        } else {
            let missing: u32 = self.materials.iter().map(|m| m.need.saturating_sub(m.have)).sum();
            format!("{}: need {missing}", self.name)
        }
    }
}

/// The wishlist, set by the overlay (`None`: no data yet).
static ENTRIES: Mutex<Option<Vec<Entry>>> = Mutex::new(None);

/// The wishlist for the Mods page (the overlay calls this whenever it works the status out).
pub fn set_entries(entries: Option<Vec<Entry>>) {
    *ENTRIES.lock().unwrap_or_else(|e| e.into_inner()) = entries;
}

/// Page `page`'s 8 rows: one a piece; a note on row 0 when there is nothing to list.
pub fn page_lines(entries: Option<&[Entry]>, page: usize) -> Vec<String> {
    let mut out: Vec<String> = match entries {
        None => vec!["No wishlist data yet (F1 opens the overlay)".into()],
        Some([]) => vec!["The wishlist is empty (add pieces in the F1 overlay)".into()],
        Some(l) => l.iter().skip(page * PAGE_LINES).take(PAGE_LINES).map(Entry::line).collect(),
    };
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
    let panel_hook = r.on("combo_panel", Box::new(panel));
    let help_hook = r.on("help_set", Box::new(help));
    r.on("help_set_alt", Box::new(help));
    if !(panel_hook && help_hook) {
        eprintln!(
            "[native-menu] the build lacks combo_panel ({panel_hook}) or help_set ({help_hook}): the side panel and help line stay the Combo List's; translate again"
        );
    }
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

    fn entry(name: &str, mats: &[(u16, u32, u32)]) -> Entry {
        let materials: Vec<Material> = mats.iter().map(|&(item, have, need)| Material { item, have, need }).collect();
        Entry {
            name: name.into(),
            ready: materials.iter().all(|m| m.have >= m.need),
            materials,
        }
    }

    #[test]
    fn the_page_lists_eight_pieces_a_page() {
        let p = page_lines(None, 0);
        assert_eq!(p.len(), PAGE_LINES);
        assert!(p[0].starts_with("No wishlist data"));
        assert!(page_lines(Some(&[]), 0)[0].contains("empty"));
        let many: Vec<Entry> = (0..10).map(|i| entry(&format!("piece {i}"), &[(1, 0, 2)])).collect();
        assert_eq!(page_lines(Some(&many), 0)[7], "piece 7: need 2");
        let second = page_lines(Some(&many), 1);
        assert_eq!(
            (second[0].as_str(), second[1].as_str(), second[2].as_str()),
            ("piece 8: need 2", "piece 9: need 2", "")
        );
        assert_eq!(page_count(Some(&many)), 2);
        assert_eq!(page_count(Some(&[])), 1);
        assert_eq!(page_count(None), 1);
    }

    #[test]
    fn left_and_right_turn_the_pages_and_wrap() {
        assert_eq!(turn_page(0, 3, 0x8), 1);
        assert_eq!(turn_page(2, 3, 0x8), 0);
        assert_eq!(turn_page(0, 3, 0x4), 2);
        assert_eq!(turn_page(1, 3, 0), 1);
        assert_eq!(turn_page(5, 1, 0x8), 0);
    }

    #[test]
    fn the_panel_shows_the_hovered_piece() {
        let e = entry("Bone Helm", &[(10, 1, 3), (11, 2, 2), (12, 0, 1)]);
        let t = |p| pane_text(p, Some(&e), None).unwrap_or_default();
        assert_eq!(t(Pane::Name), "Bone Helm");
        assert_eq!(t(Pane::Status), "Need 3 more (+1 more)");
        assert_eq!((t(Pane::Output), t(Pane::Other)), (String::new(), String::new()));
        assert_eq!(t(Pane::Count1), "1/3");
        assert_eq!(t(Pane::Count2), "2/2");
        assert_eq!(pane_text(Pane::Name1, Some(&e), None), None, "the game's own name");
        assert_eq!(pane_text(Pane::Name2, Some(&e), None), None);
        let one = entry("Potion", &[(1, 5, 1)]);
        assert_eq!(pane_text(Pane::Status, Some(&one), None).unwrap(), "Ready to craft");
        assert_eq!(pane_text(Pane::Name2, Some(&one), None).unwrap(), "");
        let all = [e.clone(), one.clone()];
        assert_eq!(pane_text(Pane::Status, None, Some(&all)).unwrap(), "1 of 2 ready");
        assert_eq!(pane_text(Pane::Name, None, None).unwrap(), "Wishlist");
        for p in [Pane::Name1, Pane::Name2, Pane::Count1, Pane::Count2, Pane::Output] {
            assert_eq!(pane_text(p, None, Some(&all)).unwrap(), "", "no stale materials on an empty row");
        }
    }

    #[test]
    fn the_page_text_does_not_overlap_the_other_buffers() {
        const {
            assert!(PAGE_TEXT_AT >= LABEL_AT + 0x100);
            assert!(PAGE_TEXT_AT + SLOTS as u32 * PAGE_TEXT_STRIDE < crate::native_text::BUFFER + 0x10_0000);
            assert!(crate::native_text::MOST < PAGE_TEXT_STRIDE as usize);
        }
    }

    #[test]
    fn the_label_is_in_the_runtimes_buffer_past_the_native_text() {
        assert!(LABEL_AT > crate::native_text::BUFFER + crate::native_text::MOST as u32);
        assert!(LABEL_AT + (LABEL.len() as u32) < crate::native_text::BUFFER + 0x10_0000);
    }
}
