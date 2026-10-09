//! The guest's memory as the host sees it: big-endian reads and writes at guest addresses.

/// A watched range (set from MH3U_WATCH): host-side writes into it are reported, as the translated code's stores are in a `--watch` build.
pub static WATCH: (std::sync::atomic::AtomicU32, std::sync::atomic::AtomicU32) =
    (std::sync::atomic::AtomicU32::new(1), std::sync::atomic::AtomicU32::new(0));

fn watched(address: u32, len: u32, what: &str) {
    use std::sync::atomic::Ordering::Relaxed;
    let (lo, hi) = (WATCH.0.load(Relaxed), WATCH.1.load(Relaxed));
    if address < hi && address.saturating_add(len) > lo {
        eprintln!("[watch-host] {what}: {len} bytes at {address:#x}");
    }
}

/// A view of the guest's address space (a reserved range of 4 GiB with guest address 0 at `base`).
#[derive(Clone, Copy)]
pub struct Guest {
    base: *mut u8,
}

impl Guest {
    pub fn new(base: *mut u8) -> Self {
        Guest { base }
    }

    fn at(&self, address: u32) -> *mut u8 {
        // SAFETY: the range is 4 GiB and `address` is 32 bits
        unsafe { self.base.add(address as usize) }
    }

    /// Copies `out.len()` bytes from the guest. The caller keeps `address + len` below 4 GiB.
    pub fn read(&self, address: u32, out: &mut [u8]) {
        debug_assert!(u64::from(address) + out.len() as u64 <= 1 << 32);
        // SAFETY: inside the reserved range (checked above in debug builds, by the caller otherwise); `out` is a host buffer
        unsafe { std::ptr::copy_nonoverlapping(self.at(address), out.as_mut_ptr(), out.len()) }
    }

    /// Calls `f` with the `len` bytes at `address` in place, without copying them (for hashing or comparing guest memory). The caller
    /// keeps `address + len` below 4 GiB. Nothing writes guest memory meanwhile: only the thread holding the baton runs the game's code,
    /// and it is the one in here.
    pub fn with_bytes<R>(&self, address: u32, len: usize, f: impl FnOnce(&[u8]) -> R) -> R {
        debug_assert!(u64::from(address) + len as u64 <= 1 << 32);
        // SAFETY: inside the reserved range (as for `read`), and not written while `f` runs (above); the slice does not outlive `f`
        f(unsafe { std::slice::from_raw_parts(self.at(address), len) })
    }

    pub fn write(&self, address: u32, bytes: &[u8]) {
        watched(address, bytes.len() as u32, "write");
        debug_assert!(u64::from(address) + bytes.len() as u64 <= 1 << 32);
        // SAFETY: as for `read`
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.at(address), bytes.len()) }
    }

    pub fn fill(&self, address: u32, value: u8, len: u32) {
        watched(address, len, "fill");
        debug_assert!(u64::from(address) + u64::from(len) <= 1 << 32);
        // SAFETY: as for `read`
        unsafe { std::ptr::write_bytes(self.at(address), value, len as usize) }
    }

    /// Moves bytes inside the guest (the ranges may overlap).
    pub fn copy(&self, from: u32, to: u32, len: u32) {
        watched(to, len, "copy");
        debug_assert!(u64::from(from) + u64::from(len) <= 1 << 32 && u64::from(to) + u64::from(len) <= 1 << 32);
        // SAFETY: as for `read`; `copy` allows overlap
        unsafe { std::ptr::copy(self.at(from), self.at(to), len as usize) }
    }

    pub fn u8(&self, address: u32) -> u8 {
        let mut b = [0];
        self.read(address, &mut b);
        b[0]
    }

    pub fn u16(&self, address: u32) -> u16 {
        let mut b = [0; 2];
        self.read(address, &mut b);
        u16::from_be_bytes(b)
    }

    pub fn put_u16(&self, address: u32, v: u16) {
        watched(address, 2, "put_u16");
        self.write(address, &v.to_be_bytes());
    }

    pub fn u32(&self, address: u32) -> u32 {
        let mut b = [0; 4];
        self.read(address, &mut b);
        u32::from_be_bytes(b)
    }

    pub fn put_u32(&self, address: u32, v: u32) {
        watched(address, 4, "put_u32");
        self.write(address, &v.to_be_bytes());
    }

    /// The zero-terminated string at `address` (at most 4096 bytes).
    pub fn cstr(&self, address: u32) -> String {
        let mut out = Vec::new();
        for k in 0..4096u32 {
            let b = self.u8(address.wrapping_add(k));
            if b == 0 {
                break;
            }
            out.push(b);
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}
