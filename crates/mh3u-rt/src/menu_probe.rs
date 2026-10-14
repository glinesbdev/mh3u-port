//! Opt-in, read-only probe of the start menu (`MH3U_MENU_PROBE=1`; `docs/native-menu.md`, "The start menu and a new page"):
//! logs the menu's state block `S` (`menu + 0x34`) whenever it changes, and each open, from two hooks:
//!
//! - `menu_start` on `f_02152c48(menu, open, kind)`, the open path: logs `S` as it is before the game resets it;
//! - `menu_update` on `f_02154abc(menu, S)`, the start menu's update, each frame it is up.
//!
//! It only reads guest memory.

use crate::hooks::{Hook, Registry};
use std::sync::Mutex;

/// The bytes of `S` read: up to the end of the second-level list's records (`S+0x170`, 8 of 0x18 bytes).
pub const LEN: usize = 0x230;
const ROWS: usize = 8;
const RECORD: usize = 0x18;

static LAST: Mutex<Option<Snapshot>> = Mutex::new(None);

pub fn enabled() -> bool {
    std::env::var("MH3U_MENU_PROBE").is_ok_and(|v| v.trim() == "1")
}

/// What the probe logs of `S`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub state: u8,
    pub sub: u8,
    pub action: u8,
    pub tabs: (u8, i8),
    pub rows: (i16, i16),
    pub second: (i16, i8),
    /// Each tab's enabled byte (`S+0x38 + i*0x18 + 2`).
    pub tab_enabled: Vec<u8>,
    /// Each row's enabled byte and label pointer (`S+0xb0 + i*0x18`, `+2` and `+8`).
    pub row: Vec<(u8, u32)>,
    /// Each second-level record's action id and enabled byte (`S+0x170 + i*0x18`, `+0` and `+2`).
    pub second_rows: Vec<(u8, u8)>,
}

fn u16_at(s: &[u8], o: usize) -> i16 {
    i16::from_be_bytes([s[o], s[o + 1]])
}

fn u32_at(s: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([s[o], s[o + 1], s[o + 2], s[o + 3]])
}

impl Snapshot {
    /// From `LEN` bytes of `S`.
    pub fn parse(s: &[u8]) -> Option<Snapshot> {
        if s.len() < LEN {
            return None;
        }
        let tabs = (s[0x24], s[0x25] as i8);
        let rows = (u16_at(s, 0x20), u16_at(s, 0x28));
        let second = (u16_at(s, 0x22), s[0x26] as i8);
        let n = |count: i16| (count.clamp(0, ROWS as i16)) as usize;
        Some(Snapshot {
            state: s[0],
            sub: s[0x1f],
            action: s[0x1e],
            tabs,
            rows,
            second,
            tab_enabled: (0..usize::from(tabs.0).min(5)).map(|i| s[0x38 + i * RECORD + 2]).collect(),
            row: (0..n(rows.0))
                .map(|i| (s[0xb0 + i * RECORD + 2], u32_at(s, 0xb0 + i * RECORD + 8)))
                .collect(),
            second_rows: (0..n(second.0))
                .map(|i| (s[0x170 + i * RECORD], s[0x170 + i * RECORD + 2]))
                .collect(),
        })
    }

    /// One log line; `text` gives a label's text from its pointer.
    pub fn line(&self, text: &dyn Fn(u32) -> String) -> String {
        let rows: Vec<String> = self
            .row
            .iter()
            .enumerate()
            .map(|(i, &(on, label))| format!("{i}{} {label:#x} \"{}\"", if on != 0 { "" } else { " (off)" }, text(label)))
            .collect();
        let second: Vec<String> = self
            .second_rows
            .iter()
            .enumerate()
            .map(|(i, &(action, on))| format!("{i}: action {action}{}", if on != 0 { "" } else { " (off)" }))
            .collect();
        format!(
            "state {} sub {} action {} | tabs {} at {} enabled {:?} | rows {} at {}: [{}] | second {} at {}: [{}]",
            self.state,
            self.sub,
            self.action,
            self.tabs.0,
            self.tabs.1,
            self.tab_enabled,
            self.rows.0,
            self.rows.1,
            rows.join(", "),
            self.second.0,
            self.second.1,
            second.join(", "),
        )
    }
}

/// A label's text: the 8-bit string at `at` (printable ASCII kept, at most 40 bytes); empty for a null or out-of-range pointer.
fn label_text(g: &crate::guest::Guest, at: u32) -> String {
    if !(0x1000_0000..0xf000_0000).contains(&at) {
        return String::new();
    }
    let mut b = [0u8; 40];
    g.read(at, &mut b);
    b.iter()
        .take_while(|&&c| c != 0)
        .map(|&c| if c.is_ascii_graphic() || c == b' ' { c as char } else { '?' })
        .collect()
}

fn snapshot(h: &Hook, s: u32) -> Option<(Snapshot, crate::guest::Guest)> {
    let g = crate::guest::Guest::new(h.ctx.mem);
    let mut b = vec![0u8; LEN];
    g.read(s, &mut b);
    Snapshot::parse(&b).map(|x| (x, g))
}

/// Says why the probe is off when it was asked for but the build has no hooks list (`hooks.txt` in the build folder).
pub fn no_hooks() {
    if enabled() {
        eprintln!("[menu-probe] off: the build has no hooks (translate again: win-build.ps1 -Translate)");
    }
}

/// Registers the probe's callbacks when `MH3U_MENU_PROBE=1`.
pub fn register(r: &mut Registry) -> bool {
    if !enabled() {
        if let Ok(v) = std::env::var("MH3U_MENU_PROBE") {
            eprintln!("[menu-probe] off: MH3U_MENU_PROBE is '{v}', not 1");
        }
        return false;
    }
    let start = r.on(
        "menu_start",
        Box::new(|h: &mut Hook| {
            let s = h.ctx.r[3].wrapping_add(0x34);
            if let Some((snap, g)) = snapshot(h, s) {
                eprintln!(
                    "[menu-probe] open (kind {}, S {s:#x}); before the reset: {}",
                    h.ctx.r[5],
                    snap.line(&|a| label_text(&g, a))
                );
            }
            *LAST.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }),
    );
    let update = r.on(
        "menu_update",
        Box::new(|h: &mut Hook| {
            let Some((snap, g)) = snapshot(h, h.ctx.r[4]) else { return };
            let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
            if last.as_ref() != Some(&snap) {
                let closing = snap.state == 3 && last.as_ref().is_some_and(|l| l.state != 3);
                eprintln!(
                    "[menu-probe] {}{}",
                    snap.line(&|a| label_text(&g, a)),
                    if closing { " (closing)" } else { "" }
                );
                *last = Some(snap);
            }
        }),
    );
    eprintln!("[menu-probe] on (MH3U_MENU_PROBE): menu_start {start}, menu_update {update}; reads only");
    start || update
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_block_is_read_at_the_documented_offsets() {
        let mut s = vec![0u8; LEN];
        s[0] = 2;
        s[0x1f] = 2;
        s[0x1e] = 6;
        s[0x24] = 3;
        s[0x25] = 1;
        s[0x20..0x22].copy_from_slice(&6i16.to_be_bytes());
        s[0x28..0x2a].copy_from_slice(&4i16.to_be_bytes());
        s[0x22..0x24].copy_from_slice(&2i16.to_be_bytes());
        s[0x26] = 1;
        s[0x38 + 0x18 + 2] = 1;
        s[0xb0 + 2] = 1;
        s[0xb0 + 8..0xb0 + 12].copy_from_slice(&0x1234_5678u32.to_be_bytes());
        s[0x170 + 0x18] = 9;
        let snap = Snapshot::parse(&s).unwrap();
        assert_eq!((snap.state, snap.sub, snap.action), (2, 2, 6));
        assert_eq!((snap.tabs, snap.rows, snap.second), ((3, 1), (6, 4), (2, 1)));
        assert_eq!(snap.tab_enabled, vec![0, 1, 0]);
        assert_eq!(snap.row.len(), 6);
        assert_eq!(snap.row[0], (1, 0x1234_5678));
        assert_eq!(snap.second_rows, vec![(0, 0), (9, 0)]);
        let line = snap.line(&|_| "Items".into());
        assert!(line.starts_with("state 2 sub 2 action 6 | tabs 3 at 1"), "{line}");
        assert!(line.contains("0 0x12345678 \"Items\", 1 (off)"), "{line}");
    }

    #[test]
    fn odd_counts_are_clamped_and_short_blocks_refused() {
        let mut s = vec![0u8; LEN];
        s[0x20..0x22].copy_from_slice(&(-1i16).to_be_bytes());
        s[0x22..0x24].copy_from_slice(&40i16.to_be_bytes());
        s[0x24] = 200;
        let snap = Snapshot::parse(&s).unwrap();
        assert_eq!((snap.row.len(), snap.second_rows.len(), snap.tab_enabled.len()), (0, 8, 5));
        assert!(Snapshot::parse(&s[..0x100]).is_none());
    }
}
