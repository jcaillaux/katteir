//! Minimal IVF container reader.
//!
//! IVF layout (all integers little-endian):
//! - 32-byte file header: "DKIF", version u16, header size u16, fourcc [u8; 4],
//!   width u16, height u16, timebase denominator u32, timebase numerator u32,
//!   frame count u32, unused u32.
//! - Then per frame: 12-byte header (size u32, pts u64) followed by `size` bytes.
//!
//! The reader only builds an index of byte spans; it never copies frame data.

use std::fmt;

pub const FILE_HEADER_BYTES: usize = 32;
pub const FRAME_HEADER_BYTES: usize = 12;
/// Same limit as catnap's `limits.rs` will have: max frames per clip.
pub const MAX_FRAMES: usize = 600;
/// A single AV1 temporal unit bigger than this is certainly corrupt for our clips.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSpan {
    pub offset: usize,
    pub len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IvfIndex {
    pub width_px: u16,
    pub height_px: u16,
    pub timebase_den: u32,
    pub timebase_num: u32,
    pub frames: Vec<FrameSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IvfError {
    TooShort,
    BadSignature,
    BadHeaderSize(u16),
    NotAv1([u8; 4]),
    BadDimensions { width_px: u16, height_px: u16 },
    BadTimebase { den: u32, num: u32 },
    TruncatedFrame { index: usize },
    FrameTooLarge { index: usize, len: usize },
    TooManyFrames,
    NoFrames,
}

impl fmt::Display for IvfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid IVF file: {self:?}")
    }
}

impl std::error::Error for IvfError {}

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

pub fn parse(bytes: &[u8]) -> Result<IvfIndex, IvfError> {
    if bytes.len() < FILE_HEADER_BYTES {
        return Err(IvfError::TooShort);
    }
    if &bytes[0..4] != b"DKIF" {
        return Err(IvfError::BadSignature);
    }
    let header_bytes = read_u16(bytes, 6);
    if usize::from(header_bytes) != FILE_HEADER_BYTES {
        return Err(IvfError::BadHeaderSize(header_bytes));
    }
    let fourcc = [bytes[8], bytes[9], bytes[10], bytes[11]];
    if &fourcc != b"AV01" {
        return Err(IvfError::NotAv1(fourcc));
    }
    let width_px = read_u16(bytes, 12);
    let height_px = read_u16(bytes, 14);
    // Stacked alpha: colour on top, alpha below, so the height must split evenly.
    if width_px == 0 || height_px == 0 || !height_px.is_multiple_of(4) {
        return Err(IvfError::BadDimensions { width_px, height_px });
    }
    let den = read_u32(bytes, 16);
    let num = read_u32(bytes, 20);
    if den == 0 || num == 0 {
        return Err(IvfError::BadTimebase { den, num });
    }
    let frames = parse_frames(bytes)?;
    assert!(!frames.is_empty() && frames.len() <= MAX_FRAMES);
    Ok(IvfIndex { width_px, height_px, timebase_den: den, timebase_num: num, frames })
}

fn parse_frames(bytes: &[u8]) -> Result<Vec<FrameSpan>, IvfError> {
    let mut frames = Vec::with_capacity(MAX_FRAMES);
    let mut cursor = FILE_HEADER_BYTES;
    // Bounded: at most MAX_FRAMES + 1 iterations.
    for index in 0..=MAX_FRAMES {
        if cursor == bytes.len() {
            break;
        }
        if index == MAX_FRAMES {
            return Err(IvfError::TooManyFrames);
        }
        if bytes.len() - cursor < FRAME_HEADER_BYTES {
            return Err(IvfError::TruncatedFrame { index });
        }
        let len = read_u32(bytes, cursor) as usize;
        if len == 0 || len > MAX_FRAME_BYTES {
            return Err(IvfError::FrameTooLarge { index, len });
        }
        let offset = cursor + FRAME_HEADER_BYTES;
        if bytes.len() - offset < len {
            return Err(IvfError::TruncatedFrame { index });
        }
        frames.push(FrameSpan { offset, len });
        cursor = offset + len;
    }
    if frames.is_empty() {
        return Err(IvfError::NoFrames);
    }
    debug_assert!(frames.windows(2).all(|w| w[0].offset + w[0].len < w[1].offset));
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(width_px: u16, height_px: u16) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(b"DKIF");
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(&32u16.to_le_bytes());
        h.extend_from_slice(b"AV01");
        h.extend_from_slice(&width_px.to_le_bytes());
        h.extend_from_slice(&height_px.to_le_bytes());
        h.extend_from_slice(&30u32.to_le_bytes());
        h.extend_from_slice(&1u32.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(h.len(), FILE_HEADER_BYTES);
        h
    }

    fn push_frame(file: &mut Vec<u8>, payload: &[u8], pts: u64) {
        file.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        file.extend_from_slice(&pts.to_le_bytes());
        file.extend_from_slice(payload);
    }

    #[test]
    fn indexes_frames_without_copying() {
        let mut file = header(1280, 1440);
        push_frame(&mut file, &[1, 2, 3], 0);
        push_frame(&mut file, &[4, 5], 1);
        let index = parse(&file).expect("valid file");
        assert_eq!(index.frames.len(), 2);
        let first = index.frames[0];
        assert_eq!(&file[first.offset..first.offset + first.len], &[1, 2, 3]);
        let second = index.frames[1];
        assert_eq!(&file[second.offset..second.offset + second.len], &[4, 5]);
        assert_eq!((index.timebase_den, index.timebase_num), (30, 1));
    }

    #[test]
    fn rejects_wrong_signature_and_codec() {
        let mut file = header(1280, 1440);
        file[0] = b'X';
        assert_eq!(parse(&file), Err(IvfError::BadSignature));
        let mut file = header(1280, 1440);
        file[8..12].copy_from_slice(b"VP90");
        assert_eq!(parse(&file), Err(IvfError::NotAv1(*b"VP90")));
    }

    #[test]
    fn rejects_truncated_frame() {
        let mut file = header(1280, 1440);
        push_frame(&mut file, &[1, 2, 3, 4], 0);
        file.truncate(file.len() - 1);
        assert_eq!(parse(&file), Err(IvfError::TruncatedFrame { index: 0 }));
    }

    #[test]
    fn rejects_empty_and_odd_height() {
        assert_eq!(parse(&header(1280, 1440)), Err(IvfError::NoFrames));
        let mut file = header(1280, 1442);
        push_frame(&mut file, &[1], 0);
        assert_eq!(
            parse(&file),
            Err(IvfError::BadDimensions { width_px: 1280, height_px: 1442 })
        );
    }

    #[test]
    fn rejects_more_than_max_frames() {
        let mut file = header(16, 16);
        for pts in 0..=MAX_FRAMES as u64 {
            push_frame(&mut file, &[0], pts);
        }
        assert_eq!(parse(&file), Err(IvfError::TooManyFrames));
    }
}
