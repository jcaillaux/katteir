//! Cat video (CLAUDE.md §5): stacked-alpha AV1 clips in IVF files, decoded
//! by dav1d on a worker thread (`decode`) and drawn through an OpenGL texture
//! (`gl`). Ported from `spikes/av1-video`.

pub mod decode;
pub mod gl;
pub mod ivf;

use std::path::Path;
use std::sync::Arc;

use crate::limits;

/// A clip's bytes: embedded in the binary, or read from a file once and
/// shared (dav1d takes each frame as an owned, `'static` value).
#[derive(Clone)]
enum ClipBytes {
    Static(&'static [u8]),
    Shared(Arc<[u8]>),
}

impl AsRef<[u8]> for ClipBytes {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Static(bytes) => bytes,
            Self::Shared(bytes) => bytes,
        }
    }
}

/// One frame's bytes, handed to dav1d without copying.
pub struct FrameData {
    bytes: ClipBytes,
    span: ivf::FrameSpan,
}

impl AsRef<[u8]> for FrameData {
    fn as_ref(&self) -> &[u8] {
        &self.bytes.as_ref()[self.span.offset..self.span.offset + self.span.len]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipInfo {
    pub width_px: u16,
    /// Height of the picture: half the stacked frame (colour over alpha).
    pub picture_height_px: u16,
    pub frames: usize,
    pub fps: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ClipError {
    #[error("cannot read the clip: {0}")]
    Io(#[from] std::io::Error),
    #[error("clip is {len} bytes, over the {max}-byte limit")]
    TooLarge { len: u64, max: u64 },
    #[error("{0}")]
    Ivf(#[from] ivf::IvfError),
    #[error("frame rate {den}/{num} isn't a whole number of frames per second")]
    FpsNotWhole { den: u32, num: u32 },
    #[error("{fps} fps is over the {max} fps limit")]
    FpsTooHigh { fps: u32, max: u32 },
}

/// A parsed clip, cheap to clone (the bytes and index are shared).
#[derive(Clone)]
pub struct Clip {
    bytes: ClipBytes,
    index: Arc<ivf::IvfIndex>,
    fps: u32,
}

impl Clip {
    pub fn from_static(bytes: &'static [u8]) -> Result<Self, ClipError> {
        Self::new(ClipBytes::Static(bytes))
    }

    /// Reads a clip file (bounded by `limits::MAX_CLIP_BYTES`) and checks it.
    pub fn load(path: &Path) -> Result<Self, ClipError> {
        let len = std::fs::metadata(path)?.len();
        if len > limits::MAX_CLIP_BYTES {
            return Err(ClipError::TooLarge { len, max: limits::MAX_CLIP_BYTES });
        }
        Self::new(ClipBytes::Shared(Arc::from(std::fs::read(path)?)))
    }

    fn new(bytes: ClipBytes) -> Result<Self, ClipError> {
        let index = ivf::parse(bytes.as_ref())?;
        let (den, num) = (index.timebase_den, index.timebase_num);
        if !den.is_multiple_of(num) {
            return Err(ClipError::FpsNotWhole { den, num });
        }
        let fps = den / num;
        if fps > limits::MAX_CLIP_FPS {
            return Err(ClipError::FpsTooHigh { fps, max: limits::MAX_CLIP_FPS });
        }
        assert!(fps > 0 && index.height_px.is_multiple_of(4), "ivf::parse checks the timebase and height");
        Ok(Self { bytes, index: Arc::new(index), fps })
    }

    pub fn info(&self) -> ClipInfo {
        ClipInfo {
            width_px: self.index.width_px,
            picture_height_px: self.index.height_px / 2,
            frames: self.index.frames.len(),
            fps: self.fps,
        }
    }

    /// Whole frames per second.
    pub fn fps(&self) -> u32 {
        self.fps
    }

    /// Size of the stacked frame (colour over alpha), in pixels.
    pub fn stacked_size_px(&self) -> (u32, u32) {
        (u32::from(self.index.width_px), u32::from(self.index.height_px))
    }

    pub fn frame_count(&self) -> usize {
        self.index.frames.len()
    }

    pub fn frame_data(&self, frame_index: usize) -> FrameData {
        let span = self.index.frames[frame_index];
        FrameData { bytes: self.bytes.clone(), span }
    }
}

/// Reads a clip file and checks it's a usable stacked-alpha AV1 IVF.
pub fn probe_clip(path: &Path) -> Result<ClipInfo, ClipError> {
    Clip::load(path).map(|clip| clip.info())
}
