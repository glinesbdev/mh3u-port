//! The runtime: the guest's memory, the translated game as a library, and the system functions the game calls.
//!
//! This is the one crate that uses `unsafe`, and only for two things: the guest's address space (a reserved 4 GiB range the translated
//! code indexes with 32-bit guest addresses) and the calls into the translated library. Everything the game is made of comes from the
//! owner's own files at run time; nothing of it is in this repository.

mod cafe;
mod game;
mod guest;
mod kernel;
mod sync;

pub use game::{DataImport, Options, Report, run};

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
