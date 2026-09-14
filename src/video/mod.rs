//! Cat video (CLAUDE.md §5). In M0 this only checks clip files, for the
//! settings window; decoding and the GL path arrive in M1 from
//! `spikes/av1-video`.

pub mod ivf;

use std::path::Path;

use crate::limits;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipInfo {
    pub width_px: u16,
    /// Height of the picture: half the stacked frame (colour over alpha).
    pub picture_height_px: u16,
    pub frames: usize,
    pub fps: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum ClipError {
    #[error("cannot read the clip: {0}")]
    Io(#[from] std::io::Error),
    #[error("clip is {len} bytes, over the {max}-byte limit")]
    TooLarge { len: u64, max: u64 },
    #[error("{0}")]
    Ivf(#[from] ivf::IvfError),
    #[error("{fps:.1} fps is over the {max} fps limit")]
    FpsTooHigh { fps: f64, max: u32 },
}

/// Reads a clip file and checks it's a stacked-alpha AV1 IVF within limits.
pub fn probe_clip(path: &Path) -> Result<ClipInfo, ClipError> {
    let len = std::fs::metadata(path)?.len();
    if len > limits::MAX_CLIP_BYTES {
        return Err(ClipError::TooLarge { len, max: limits::MAX_CLIP_BYTES });
    }
    let index = ivf::parse(&std::fs::read(path)?)?;
    let fps = f64::from(index.timebase_den) / f64::from(index.timebase_num);
    if fps > f64::from(limits::MAX_CLIP_FPS) {
        return Err(ClipError::FpsTooHigh { fps, max: limits::MAX_CLIP_FPS });
    }
    assert!(index.height_px.is_multiple_of(4), "ivf::parse guarantees a stacked height");
    Ok(ClipInfo {
        width_px: index.width_px,
        picture_height_px: index.height_px / 2,
        frames: index.frames.len(),
        fps,
    })
}
