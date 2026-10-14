//! The runtime: the guest's memory, the translated game as a library, and the system functions the game calls.
//!
//! This is the one crate that uses `unsafe`, and only for two things: the guest's address space (a reserved 4 GiB range the translated
//! code indexes with 32-bit guest addresses) and the calls into the translated library. Everything the game is made of comes from the
//! owner's own files at run time; nothing of it is in this repository.

mod axfx;
mod cafe;
mod fs;
mod game;
mod guest;
mod gx2;
pub mod hooks;

/// A zeroed context with no guest memory, for tests of hook callbacks elsewhere.
pub fn new_test_ctx() -> Ctx {
    game::new_ctx(std::ptr::null_mut())
}
pub mod input;
mod kernel;
mod memory;
pub mod menu_probe;
pub mod mods;
pub mod native_text;
mod sync;

pub use fs::Roots;
pub use game::{DataImport, Options, Report, run};
pub use guest::Guest;

/// The registers of a core, laid out as the translated C's `Ctx` (`ppc.h`); the library's size for it is checked when it is loaded.
#[repr(C)]
#[derive(Clone)]
pub struct Ctx {
    pub r: [u32; 32],
    pub f: [u64; 32],
    pub ps1: [u64; 32],
    pub cr: u32,
    pub xer: u32,
    pub lr: u32,
    pub ctr: u32,
    pub fpscr: u32,
    pub gqr: [u32; 8],
    pub reservation: u32,
    pub has_reservation: u32,
    pub tb: u64,
    /// Where guest address 0 is in the host's memory.
    pub mem: *mut u8,
}

/// What draws for the game: the runtime hands it every `gx2` call it has no code of its own for, with the registers and the memory.
/// A call that blocks (waiting for the next frame) blocks here, with the other guest threads waiting for it.
pub trait Gx2Backend: Send {
    /// Whether the backend does something for this import ("gx2:GX2SwapScanBuffers"); the others stay stubs.
    fn implements(&self, name: &str) -> bool;
    fn call(&mut self, name: &str, c: &mut Ctx, g: &Guest);
    /// A number for an import the backend implements, asked once when the game is loaded; calls then come to `call_id` with it, so a
    /// backend need not match names on every call. None (the default) keeps them coming by name.
    fn resolve(&self, _name: &str) -> Option<u32> {
        None
    }
    fn call_id(&mut self, _id: Option<u32>, name: &str, c: &mut Ctx, g: &Guest) {
        self.call(name, c, g);
    }
    /// Whether the backend times each call itself (then `last_call_time` says how long the last one took, and the runtime's
    /// MH3U_GFX_TIMES report uses that instead of reading the clock around the call a second time).
    fn times_calls(&self) -> bool {
        false
    }
    fn last_call_time(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }
}

/// Frames shown so far: the graphics backend counts its swaps here, so the timing reports (MH3U_GFX_TIMES) can give per-frame averages.
pub static FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// MH3U_TIMES_WINDOW: how many seconds each timing report covers (10 when it is not set or not a number).
pub fn times_window() -> std::time::Duration {
    static WINDOW: std::sync::OnceLock<std::time::Duration> = std::sync::OnceLock::new();
    *WINDOW.get_or_init(|| {
        let secs = std::env::var("MH3U_TIMES_WINDOW").ok().and_then(|v| v.trim().parse::<f64>().ok());
        std::time::Duration::from_secs_f64(secs.filter(|s| s.is_finite() && *s > 0.0).unwrap_or(10.0))
    })
}

/// The frames shown since `since` (a value of [`FRAMES`]), at least one so averages can be divided by it.
pub fn frames_since(since: u64) -> u64 {
    FRAMES.load(std::sync::atomic::Ordering::Relaxed).saturating_sub(since).max(1)
}

/// A piece of the executable to put in the guest's memory, or to clear.
pub struct Piece {
    pub address: u32,
    /// What to write (empty with `zero` for a section that is only cleared memory).
    pub bytes: Vec<u8>,
    /// How many bytes of zeros to put after `bytes`.
    pub zero: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_context_has_the_size_the_c_gives_it() {
        // 32 * 4 + 3 * 32 * 8 ... counted by hand against ppc.h: 128 + 256 + 256 + 20 + 32 + 8 + 8 (+ padding) + 8
        assert_eq!(std::mem::size_of::<Ctx>(), 720);
        assert_eq!(std::mem::offset_of!(Ctx, mem), 712);
    }
}
