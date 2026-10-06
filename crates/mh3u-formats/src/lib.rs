//! The game's files as bytes in, typed values out: the executable (`.rpx`), the archives (`.arc`), the text tables (`.gmd`) and the GX2 shader files (`.gfd`). All
//! values are big-endian. Every number in a file is checked: a damaged or made-up file is an error, never a panic or an unbounded
//! allocation (`hostile_tests` feeds them such files).

pub mod arc;
pub mod gfd;
pub mod gmd;
#[cfg(test)]
mod hostile_tests;
pub mod inflate;
pub mod rpx;
pub mod tiling;
