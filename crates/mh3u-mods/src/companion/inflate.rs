//! Decompressing zlib data from files the program did not make (game archives and executables).
//!
//! A file can say any size it likes for what it holds, so the size is not trusted: memory is not set aside for more than [`MAX_INFLATED`],
//! and decompressing stops when the data grows past the size the file declared. (A tiny stream that unpacks to gigabytes would otherwise
//! use up the memory.)

use anyhow::{Result, bail};
use flate2::read::ZlibDecoder;
use std::io::Read;

/// The most one compressed entry or section may unpack to. The game's biggest is its code, about 15 MB.
pub const MAX_INFLATED: usize = 512 << 20;

/// Decompress `compressed`, which the file says unpacks to `declared` bytes.
pub fn inflate(compressed: &[u8], declared: usize) -> Result<Vec<u8>> {
    inflate_within(compressed, declared, MAX_INFLATED)
}

fn inflate_within(compressed: &[u8], declared: usize, max: usize) -> Result<Vec<u8>> {
    if declared > max {
        bail!("it says it unpacks to {declared} bytes, more than the {max} allowed");
    }
    let mut out = Vec::with_capacity(declared);
    // one byte past the declared size is enough to tell that the data is bigger than it said
    ZlibDecoder::new(compressed).take(declared as u64 + 1).read_to_end(&mut out)?;
    if out.len() > declared {
        bail!("it unpacks to more than the {declared} bytes it says");
    }
    Ok(out)
}
