//! The guest's 4 GiB address space in the host's memory: reserved in one piece, with guest address 0 at its start, and zero wherever
//! the game has not written.
//!
//! On Linux (and other Unix systems) this is one anonymous mapping: the kernel gives a page only when it is first touched, so the
//! 4 GiB cost nothing up front. Windows charges a committed mapping against the commit limit (memory plus page file) in full when it
//! is made, so there the range is only *reserved*, and a vectored exception handler commits 64 KiB at a time when the game or the
//! runtime first touches it (an access violation inside the range, which is then retried). Both give zeroed pages on first use.

/// Whether the host memory at `p` (inside the guest's range) has been touched already: reading it then costs nothing. On Windows an
/// untouched page would be committed by the read, so a scan of memory (`mods`) skips those; elsewhere every page reads as cheaply.
pub fn touched(p: *const u8) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, VirtualQuery};
        // SAFETY: a plain C structure of numbers and pointers, for which all zeros is a valid value
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: VirtualQuery only reads the address space's bookkeeping, and `info` is ours
        let n = unsafe { VirtualQuery(p.cast(), &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
        n != 0 && info.State == MEM_COMMIT
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        true
    }
}

/// The size of the guest's address space.
pub const SIZE: usize = 1 << 32;

#[cfg(unix)]
pub struct GuestMemory {
    map: memmap2::MmapMut,
}

#[cfg(unix)]
impl GuestMemory {
    pub fn reserve() -> anyhow::Result<GuestMemory> {
        use anyhow::Context;
        let map = memmap2::MmapOptions::new()
            .len(SIZE)
            .map_anon()
            .context("reserving the guest's address space")?;
        Ok(GuestMemory { map })
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.map.as_mut_ptr()
    }
}

#[cfg(windows)]
pub use windows::GuestMemory;

#[cfg(windows)]
mod windows {
    use super::SIZE;
    use anyhow::{Result, bail};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use windows_sys::Win32::Foundation::EXCEPTION_ACCESS_VIOLATION;
    use windows_sys::Win32::System::Diagnostics::Debug::{AddVectoredExceptionHandler, EXCEPTION_POINTERS, RemoveVectoredExceptionHandler};
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_NOACCESS, PAGE_READWRITE, VirtualAlloc, VirtualFree,
    };

    /// How much is committed at once when the guest touches a page that is not committed yet.
    const CHUNK: usize = 64 << 10;
    const EXCEPTION_CONTINUE_EXECUTION: i32 = -1;
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

    /// The starts of the reserved ranges that exist (0 for a free place), for the handler to recognise them.
    static RANGES: [AtomicUsize; 4] = [const { AtomicUsize::new(0) }; 4];

    pub struct GuestMemory {
        base: *mut u8,
        slot: usize,
        handler: *mut core::ffi::c_void,
    }

    // SAFETY: the reservation is plain memory; who may touch it when is the kernel's baton's business, as on Linux
    unsafe impl Send for GuestMemory {}

    /// Commits the chunk around a faulting address when it is inside a guest range; anything else goes on to the next handler.
    unsafe extern "system" fn commit_on_touch(info: *mut EXCEPTION_POINTERS) -> i32 {
        // SAFETY: the system passes a valid record for the exception being handled
        let record = unsafe { &*(*info).ExceptionRecord };
        if record.ExceptionCode != EXCEPTION_ACCESS_VIOLATION || record.NumberParameters < 2 {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let address = record.ExceptionInformation[1];
        for range in &RANGES {
            let base = range.load(Ordering::Acquire);
            if base != 0 && (base..base + SIZE).contains(&address) {
                let chunk = address & !(CHUNK - 1);
                // SAFETY: the chunk is inside a range this module reserved and has not released
                let ok = unsafe { VirtualAlloc(chunk as *const _, CHUNK, MEM_COMMIT, PAGE_READWRITE) };
                return if ok.is_null() {
                    EXCEPTION_CONTINUE_SEARCH
                } else {
                    EXCEPTION_CONTINUE_EXECUTION
                };
            }
        }
        EXCEPTION_CONTINUE_SEARCH
    }

    impl GuestMemory {
        pub fn reserve() -> Result<GuestMemory> {
            // SAFETY: a fresh reservation anywhere the system likes
            let base = unsafe { VirtualAlloc(std::ptr::null(), SIZE, MEM_RESERVE, PAGE_NOACCESS) } as *mut u8;
            if base.is_null() {
                bail!(
                    "reserving the guest's address space (4 GiB) failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            let Some(slot) = RANGES
                .iter()
                .position(|r| r.compare_exchange(0, base as usize, Ordering::AcqRel, Ordering::Acquire).is_ok())
            else {
                // SAFETY: the reservation was just made and nothing uses it
                unsafe { VirtualFree(base.cast(), 0, MEM_RELEASE) };
                bail!("too many guest address spaces at once");
            };
            // SAFETY: the handler only commits memory inside the ranges above; first in the chain, so it runs before any other
            let handler = unsafe { AddVectoredExceptionHandler(1, Some(commit_on_touch)) };
            if handler.is_null() {
                RANGES[slot].store(0, Ordering::Release);
                // SAFETY: as above
                unsafe { VirtualFree(base.cast(), 0, MEM_RELEASE) };
                bail!("installing the handler that commits guest memory failed");
            }
            Ok(GuestMemory { base, slot, handler })
        }

        pub fn as_mut_ptr(&mut self) -> *mut u8 {
            self.base
        }
    }

    impl Drop for GuestMemory {
        fn drop(&mut self) {
            RANGES[self.slot].store(0, Ordering::Release);
            // SAFETY: the handler and the reservation are ours, and nothing runs guest code once the memory is dropped
            unsafe {
                RemoveVectoredExceptionHandler(self.handler);
                VirtualFree(self.base.cast(), 0, MEM_RELEASE);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anywhere in the range reads as zero and keeps what is written, at the start, in the middle and at the very end.
    #[test]
    fn the_whole_range_is_usable_and_starts_as_zeros() {
        let mut m = GuestMemory::reserve().expect("reserved");
        let g = crate::Guest::new(m.as_mut_ptr());
        for a in [0u32, 0x1080_0000, 0x7fff_fffc, 0xf400_0000, 0xffff_fffc] {
            assert_eq!(g.u32(a), 0, "{a:#x} reads as zero");
            g.put_u32(a, a ^ 0x5a5a_5a5a);
            assert_eq!(g.u32(a), a ^ 0x5a5a_5a5a);
        }
    }
}
