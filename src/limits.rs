//! Fixed limits (CLAUDE.md §4). Config values are clamped to these on load,
//! and file inputs are rejected past them.

use std::ops::RangeInclusive;

/// Most cats catnap will list.
#[allow(dead_code)] // Used by the cat registry (M3).
pub const MAX_CATS: usize = 32;
/// Most frames in one clip (20 s at 30 fps).
pub const MAX_FRAMES_PER_CLIP: usize = 600;
/// Highest clip frame rate.
pub const MAX_CLIP_FPS: u32 = 30;
/// A single AV1 temporal unit bigger than this is certainly corrupt.
pub const MAX_CLIP_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Largest clip file catnap will read.
pub const MAX_CLIP_BYTES: u64 = 64 * 1024 * 1024;

pub const WORK_MINUTES: RangeInclusive<u32> = 1..=180;
pub const WARN_BEFORE_SECS: RangeInclusive<u32> = 0..=300;
/// Break length: 10 s to an hour.
pub const BREAK_SECS: RangeInclusive<u32> = 10..=3600;
pub const DISMISS_HOLD_SECS: RangeInclusive<u32> = 1..=30;

/// Cat names are directory names under `assets/cats/`.
pub const MAX_CAT_NAME_BYTES: usize = 64;
pub const MAX_PATH_BYTES: usize = 4096;
/// Largest config file catnap will read.
pub const MAX_CONFIG_BYTES: usize = 64 * 1024;
