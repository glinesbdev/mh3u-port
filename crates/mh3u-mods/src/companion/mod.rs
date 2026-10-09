//! Copied from the companion app, [glinesbdev/mh3u-companion](https://github.com/glinesbdev/mh3u-companion) (`crates/mh3u-core`,
//! commit 563aa22b9eaf7e761018815acea9ad84de72869d), which is public domain like this project: the archive, text, executable,
//! recipe and save formats it worked out. Only what the wishlist needs; the tests stayed there (they need its save snapshots).
//!
//! Its crate is not a dependency because it does not build for Windows (its `/proc` reader is Unix only, and not optional). Changes
//! here: the test modules and `Save::reached_rank` (which needs its drop tables) removed, paths made relative to this module.

pub mod arc;
pub mod gmd;
pub mod inflate;
pub mod recipes;
pub mod rpx;
pub mod save;
