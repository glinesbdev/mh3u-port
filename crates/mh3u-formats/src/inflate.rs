//! Decompressing zlib data from files the program did not make (game archives and executables).
//!
//! A file can say any size it likes for what it holds, so the size is not trusted: memory is not set aside for more than [`MAX_INFLATED`],
//! and decompressing stops when the data grows past the size the file declared. (A tiny stream that unpacks to gigabytes would otherwise
//! use up the memory.) A stream that unpacks to less than it says is still returned, because callers already rely on that, but the
//! mismatch is printed: a length that was read wrong must not pass in silence.

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
    if out.len() != declared {
        // Shorter than declared stays a success: the executable reader, and the test below, rely on it.
        // Printing it is what stops a bad length (an ARC size read 8× too big) from hiding.
        let got = out.len();
        eprintln!("it unpacks to {got} bytes, not the {declared} it says");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = ZlibEncoder::new(Vec::new(), Compression::best());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn data_unpacks_to_the_size_it_says() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        assert_eq!(inflate(&zlib(&data), data.len()).unwrap(), data);
        // a stream that unpacks to less than it says is still returned, as it always was, and a warning is printed
        assert_eq!(inflate(&zlib(&data), data.len() + 10).unwrap(), data);
    }

    #[test]
    fn a_stream_bigger_than_it_says_is_refused_without_unpacking_it_all() {
        // 64 MiB of zeros is a few dozen KB compressed; the file says 1 KiB
        let bomb = zlib(&vec![0u8; 64 << 20]);
        assert!(bomb.len() < 200_000);
        let err = inflate(&bomb, 1024).unwrap_err().to_string();
        assert!(err.contains("more than the 1024 bytes"), "{err}");
    }

    #[test]
    fn a_size_beyond_the_limit_is_refused_before_any_memory_is_set_aside() {
        let err = inflate_within(&zlib(b"x"), 10_000, 1000).unwrap_err().to_string();
        assert!(err.contains("more than the 1000 allowed"), "{err}");
        assert!(inflate(&zlib(b"x"), u32::MAX as usize).is_err(), "4 GiB is over the limit");
        // garbage is an error, not a panic
        assert!(inflate(b"not zlib at all", 100).is_err());
        assert!(inflate(&[], 0).is_err());
    }
}
