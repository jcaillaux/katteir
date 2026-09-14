//! Cat sources (CLAUDE.md §5). M1 has the bundled placeholder plus the clips
//! from the config; the registry of bundled cats comes with M3.

use crate::config::Config;
use crate::video::{Clip, ClipError, decode};

/// The bundled placeholder: a flat ginger blob made by tools/placeholder.sh
/// (CC0, see assets/cats/placeholder/cat.toml).
const PLACEHOLDER_ENTRY: &[u8] = include_bytes!("../assets/cats/placeholder/entry.ivf");
const PLACEHOLDER_SLEEP: &[u8] = include_bytes!("../assets/cats/placeholder/sleep.ivf");

/// The two clips of one break: the entry once, then the loop.
#[derive(Clone)]
pub struct CatClips {
    pub entry: Clip,
    pub looped: Clip,
}

#[derive(Debug, thiserror::Error)]
pub enum CatError {
    #[error("{which} clip: {source}")]
    Clip { which: &'static str, source: ClipError },
    #[error("the entry and loop clips have different sizes ({entry:?} and {looped:?})")]
    SizeMismatch { entry: (u32, u32), looped: (u32, u32) },
    #[error("the clips' frame rates ({entry} and {looped} fps) can't play back to back")]
    RateMismatch { entry: u32, looped: u32 },
}

pub fn placeholder() -> Result<CatClips, CatError> {
    let entry = Clip::from_static(PLACEHOLDER_ENTRY).map_err(|source| CatError::Clip { which: "placeholder entry", source })?;
    let looped = Clip::from_static(PLACEHOLDER_SLEEP).map_err(|source| CatError::Clip { which: "placeholder sleep", source })?;
    pair(entry, looped)
}

/// The clips from the config, when both are set. Loading reads both files.
pub fn configured(config: &Config) -> Option<Result<CatClips, CatError>> {
    let (entry_path, loop_path) = config.clips()?;
    Some(load_pair(entry_path, loop_path))
}

fn load_pair(entry_path: &std::path::Path, loop_path: &std::path::Path) -> Result<CatClips, CatError> {
    let entry = Clip::load(entry_path).map_err(|source| CatError::Clip { which: "entry", source })?;
    let looped = Clip::load(loop_path).map_err(|source| CatError::Clip { which: "loop", source })?;
    pair(entry, looped)
}

/// Checks two clips can play back to back: same size, compatible rates.
pub fn pair(entry: Clip, looped: Clip) -> Result<CatClips, CatError> {
    if entry.stacked_size_px() != looped.stacked_size_px() {
        return Err(CatError::SizeMismatch { entry: entry.stacked_size_px(), looped: looped.stacked_size_px() });
    }
    if !decode::rates_compatible(&entry, &looped) {
        return Err(CatError::RateMismatch { entry: entry.fps(), looped: looped.fps() });
    }
    Ok(CatClips { entry, looped })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal IVF (one tiny frame) with the given size and rate, leaked
    /// so it can be a `&'static` clip like the embedded ones.
    fn ivf(width_px: u16, height_px: u16, fps: u32) -> &'static [u8] {
        let mut file = Vec::new();
        file.extend_from_slice(b"DKIF");
        file.extend_from_slice(&0u16.to_le_bytes());
        file.extend_from_slice(&32u16.to_le_bytes());
        file.extend_from_slice(b"AV01");
        file.extend_from_slice(&width_px.to_le_bytes());
        file.extend_from_slice(&height_px.to_le_bytes());
        file.extend_from_slice(&fps.to_le_bytes());
        file.extend_from_slice(&1u32.to_le_bytes());
        file.extend_from_slice(&[0; 8]);
        file.extend_from_slice(&1u32.to_le_bytes());
        file.extend_from_slice(&0u64.to_le_bytes());
        file.push(0);
        Box::leak(file.into_boxed_slice())
    }

    fn clip(width_px: u16, height_px: u16, fps: u32) -> Clip {
        Clip::from_static(ivf(width_px, height_px, fps)).expect("valid test clip")
    }

    #[test]
    fn placeholder_is_a_valid_pair() {
        let clips = placeholder().expect("the embedded placeholder is valid");
        assert_eq!(clips.entry.stacked_size_px(), (640, 720));
        assert_eq!((clips.entry.fps(), clips.looped.fps()), (15, 15));
        assert_eq!((clips.entry.frame_count(), clips.looped.frame_count()), (30, 60));
    }

    #[test]
    fn different_sizes_are_rejected() {
        let error = pair(clip(640, 720, 15), clip(1280, 1440, 15)).err().expect("mismatch");
        assert!(matches!(error, CatError::SizeMismatch { .. }));
    }

    #[test]
    fn rates_must_divide_the_faster_one() {
        assert!(pair(clip(640, 720, 30), clip(640, 720, 15)).is_ok());
        let error = pair(clip(640, 720, 30), clip(640, 720, 24)).err().expect("mismatch");
        assert!(matches!(error, CatError::RateMismatch { entry: 30, looped: 24 }));
    }
}
