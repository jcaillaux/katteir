//! User configuration (CLAUDE.md §5), stored as TOML in
//! `$XDG_CONFIG_HOME/catnap/config.toml` or the platform equivalent.
//!
//! Parsing, sanitising and serialising are pure and tested without the disk;
//! only `config_path`, `load_file` and `save_file` do I/O.

use std::io::ErrorKind;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::limits;

/// The bundled cat used when no clips are configured.
pub const DEFAULT_CAT: &str = "placeholder";

const HEADER: &str = "# catnap configuration. Edit freely: values out of range are clamped when\n\
                      # catnap loads this file. Clip paths must be absolute.\n\n";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub timer: TimerConfig,
    pub cat: CatConfig,
    pub display: DisplayConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TimerConfig {
    pub work_minutes: u32,
    /// 0 turns the warning off.
    pub warn_before_secs: u32,
    /// The break can't be dismissed before this.
    pub min_break_secs: u32,
}

impl Default for TimerConfig {
    fn default() -> Self {
        Self { work_minutes: 25, warn_before_secs: 60, min_break_secs: 30 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CatConfig {
    /// A bundled cat, `assets/cats/<name>`; used unless both clips are set.
    pub name: String,
    /// Own clips: stacked-alpha AV1 in IVF files, absolute paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_clip: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_clip: Option<PathBuf>,
}

impl Default for CatConfig {
    fn default() -> Self {
        Self { name: DEFAULT_CAT.to_owned(), entry_clip: None, loop_clip: None }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DisplayMode {
    #[default]
    Fullscreen,
    Overlay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DisplayConfig {
    pub mode: DisplayMode,
    pub dismiss_hold_secs: u32,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self { mode: DisplayMode::Fullscreen, dismiss_hold_secs: 5 }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file is {len} bytes, over the {max}-byte limit")]
    TooLarge { len: u64, max: usize },
    #[error("config file is not a valid catnap config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("cannot write the config as TOML: {0}")]
    Serialise(#[from] toml::ser::Error),
    #[error("no config directory is known for this user")]
    NoConfigDir,
    #[error("config file I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Name of a field that `Config::sanitise` changed, for logging.
pub type Adjusted = &'static str;

impl Config {
    /// Clamps numbers to `limits` and drops values that can't be used.
    /// Returns the fields it changed.
    pub fn sanitise(&mut self) -> Vec<Adjusted> {
        let mut adjusted = Vec::new();
        clamp_field(&mut self.timer.work_minutes, &limits::WORK_MINUTES, "timer.work_minutes", &mut adjusted);
        clamp_field(&mut self.timer.warn_before_secs, &limits::WARN_BEFORE_SECS, "timer.warn_before_secs", &mut adjusted);
        clamp_field(&mut self.timer.min_break_secs, &limits::MIN_BREAK_SECS, "timer.min_break_secs", &mut adjusted);
        clamp_field(&mut self.display.dismiss_hold_secs, &limits::DISMISS_HOLD_SECS, "display.dismiss_hold_secs", &mut adjusted);
        if !is_valid_cat_name(&self.cat.name) {
            DEFAULT_CAT.clone_into(&mut self.cat.name);
            adjusted.push("cat.name");
        }
        sanitise_clip(&mut self.cat.entry_clip, "cat.entry_clip", &mut adjusted);
        sanitise_clip(&mut self.cat.loop_clip, "cat.loop_clip", &mut adjusted);
        debug_assert!(self.is_sane());
        adjusted
    }

    /// True when every value is within its limits.
    pub fn is_sane(&self) -> bool {
        let clip_ok = |clip: &Option<PathBuf>| clip.as_deref().is_none_or(is_usable_clip_path);
        limits::WORK_MINUTES.contains(&self.timer.work_minutes)
            && limits::WARN_BEFORE_SECS.contains(&self.timer.warn_before_secs)
            && limits::MIN_BREAK_SECS.contains(&self.timer.min_break_secs)
            && limits::DISMISS_HOLD_SECS.contains(&self.display.dismiss_hold_secs)
            && is_valid_cat_name(&self.cat.name)
            && clip_ok(&self.cat.entry_clip)
            && clip_ok(&self.cat.loop_clip)
    }

    /// The configured clips, only when both are set.
    #[allow(dead_code)] // Used by the cat window (M1).
    pub fn clips(&self) -> Option<(&Path, &Path)> {
        Some((self.cat.entry_clip.as_deref()?, self.cat.loop_clip.as_deref()?))
    }

    /// Exactly one clip is set: it can't be used on its own.
    pub fn has_lone_clip(&self) -> bool {
        self.cat.entry_clip.is_some() != self.cat.loop_clip.is_some()
    }
}

fn clamp_field(value: &mut u32, range: &RangeInclusive<u32>, name: Adjusted, adjusted: &mut Vec<Adjusted>) {
    let clamped = (*value).clamp(*range.start(), *range.end());
    if clamped != *value {
        *value = clamped;
        adjusted.push(name);
    }
    debug_assert!(range.contains(value));
}

fn is_valid_cat_name(name: &str) -> bool {
    (1..=limits::MAX_CAT_NAME_BYTES).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Absolute and not absurdly long. Whether the file exists is checked when
/// it's used, not here: this stays pure.
pub fn is_usable_clip_path(path: &Path) -> bool {
    path.is_absolute() && path.as_os_str().len() <= limits::MAX_PATH_BYTES
}

fn sanitise_clip(clip: &mut Option<PathBuf>, name: Adjusted, adjusted: &mut Vec<Adjusted>) {
    if clip.as_deref().is_some_and(|path| !is_usable_clip_path(path)) {
        *clip = None;
        adjusted.push(name);
    }
}

/// Parses and sanitises a config file's contents.
pub fn from_toml(text: &str) -> Result<(Config, Vec<Adjusted>), ConfigError> {
    if text.len() > limits::MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge { len: text.len() as u64, max: limits::MAX_CONFIG_BYTES });
    }
    let mut config: Config = toml::from_str(text)?;
    let adjusted = config.sanitise();
    Ok((config, adjusted))
}

/// Serialises a sanitised config, with a short header comment.
pub fn to_toml(config: &Config) -> Result<String, ConfigError> {
    assert!(config.is_sane(), "only sanitised configs are written");
    let body = toml::to_string_pretty(config)?;
    Ok(format!("{HEADER}{body}"))
}

/// `$XDG_CONFIG_HOME/catnap/config.toml`, or the platform equivalent.
pub fn config_path() -> Result<PathBuf, ConfigError> {
    let dirs = directories::ProjectDirs::from("", "", "catnap").ok_or(ConfigError::NoConfigDir)?;
    Ok(dirs.config_dir().join("config.toml"))
}

/// Loads the config file. A missing file gives the defaults.
pub fn load_file(path: &Path) -> Result<(Config, Vec<Adjusted>), ConfigError> {
    let len = match std::fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok((Config::default(), Vec::new())),
        Err(error) => return Err(error.into()),
    };
    if len > limits::MAX_CONFIG_BYTES as u64 {
        return Err(ConfigError::TooLarge { len, max: limits::MAX_CONFIG_BYTES });
    }
    from_toml(&std::fs::read_to_string(path)?)
}

/// Writes the config atomically: a temporary file next to it, then a rename.
pub fn save_file(path: &Path, config: &Config) -> Result<(), ConfigError> {
    let text = to_toml(config)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension("toml.tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
[timer]
work_minutes = 50
warn_before_secs = 30
min_break_secs = 120

[cat]
name = "ginger"
entry_clip = "/clips/entry.ivf"
loop_clip = "/clips/loop.ivf"

[display]
mode = "overlay"
dismiss_hold_secs = 3
"#;

    #[test]
    fn empty_file_gives_defaults() {
        let (config, adjusted) = from_toml("").expect("empty is valid");
        assert_eq!(config, Config::default());
        assert!(adjusted.is_empty());
    }

    #[test]
    fn defaults_round_trip() {
        let text = to_toml(&Config::default()).expect("serialises");
        assert!(text.starts_with("# catnap configuration"));
        let (config, adjusted) = from_toml(&text).expect("parses");
        assert_eq!(config, Config::default());
        assert!(adjusted.is_empty());
    }

    #[test]
    fn full_config_round_trips() {
        let (config, adjusted) = from_toml(FULL).expect("parses");
        assert!(adjusted.is_empty());
        assert_eq!(config.timer.work_minutes, 50);
        assert_eq!(config.display.mode, DisplayMode::Overlay);
        assert_eq!(config.clips(), Some((Path::new("/clips/entry.ivf"), Path::new("/clips/loop.ivf"))));
        let (again, _) = from_toml(&to_toml(&config).expect("serialises")).expect("parses");
        assert_eq!(again, config);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let (config, _) = from_toml("[timer]\nwork_minutes = 40\n").expect("parses");
        assert_eq!(config.timer.work_minutes, 40);
        assert_eq!(config.timer.min_break_secs, TimerConfig::default().min_break_secs);
        assert_eq!(config.cat, CatConfig::default());
    }

    #[test]
    fn out_of_range_values_are_clamped_and_reported() {
        let text = "[timer]\nwork_minutes = 0\nwarn_before_secs = 9999\n[display]\ndismiss_hold_secs = 0\n";
        let (config, adjusted) = from_toml(text).expect("parses");
        assert_eq!(config.timer.work_minutes, *limits::WORK_MINUTES.start());
        assert_eq!(config.timer.warn_before_secs, *limits::WARN_BEFORE_SECS.end());
        assert_eq!(config.display.dismiss_hold_secs, *limits::DISMISS_HOLD_SECS.start());
        assert_eq!(adjusted, ["timer.work_minutes", "timer.warn_before_secs", "display.dismiss_hold_secs"]);
    }

    #[test]
    fn unsafe_cat_name_is_reset() {
        let (config, adjusted) = from_toml("[cat]\nname = \"../../etc\"\n").expect("parses");
        assert_eq!(config.cat.name, DEFAULT_CAT);
        assert_eq!(adjusted, ["cat.name"]);
    }

    #[test]
    fn relative_clip_paths_are_dropped() {
        let (config, adjusted) = from_toml("[cat]\nentry_clip = \"clips/entry.ivf\"\n").expect("parses");
        assert_eq!(config.cat.entry_clip, None);
        assert_eq!(adjusted, ["cat.entry_clip"]);
    }

    #[test]
    fn clips_need_both_paths() {
        let (config, _) = from_toml("[cat]\nentry_clip = \"/clips/entry.ivf\"\n").expect("parses");
        assert_eq!(config.clips(), None);
        assert!(config.has_lone_clip());
        let (both, _) = from_toml(FULL).expect("parses");
        assert!(!both.has_lone_clip());
        assert!(!Config::default().has_lone_clip(), "no clips at all is fine");
    }

    #[test]
    fn missing_clip_files_are_kept() {
        // Only the path's form is checked here: a file that doesn't exist (yet)
        // is saved, and the settings window warns about it instead.
        let (config, adjusted) = from_toml(FULL).expect("parses");
        assert!(!Path::new("/clips/entry.ivf").exists());
        assert!(adjusted.is_empty());
        assert_eq!(config.cat.entry_clip.as_deref(), Some(Path::new("/clips/entry.ivf")));
    }

    #[test]
    fn unknown_fields_and_bad_values_are_errors() {
        assert!(matches!(from_toml("[timer]\nwork_hours = 1\n"), Err(ConfigError::Parse(_))));
        assert!(matches!(from_toml("[display]\nmode = \"window\"\n"), Err(ConfigError::Parse(_))));
        assert!(matches!(from_toml("[timer]\nwork_minutes = -5\n"), Err(ConfigError::Parse(_))));
    }

    #[test]
    fn oversized_text_is_rejected() {
        let text = "#".repeat(limits::MAX_CONFIG_BYTES + 1);
        assert!(matches!(from_toml(&text), Err(ConfigError::TooLarge { .. })));
    }

    // The only tests that touch the disk: the thin file layer on top of the
    // pure functions above. Each uses its own directory under the temp dir.

    #[test]
    fn save_then_load_round_trips_on_disk() {
        let dir = std::env::temp_dir().join(format!("catnap-config-test-{}", std::process::id()));
        let path = dir.join("nested").join("config.toml");
        let (config, _) = from_toml(FULL).expect("parses");
        save_file(&path, &config).expect("saves, creating the directories");
        assert!(!path.with_extension("toml.tmp").exists(), "the temporary file is renamed away");
        let loaded = load_file(&path);
        std::fs::remove_dir_all(&dir).expect("cleanup");
        let (loaded, adjusted) = loaded.expect("loads");
        assert_eq!(loaded, config);
        assert!(adjusted.is_empty());
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = std::env::temp_dir().join(format!("catnap-config-missing-{}", std::process::id()));
        let (config, adjusted) = load_file(&dir.join("config.toml")).expect("a missing file is fine");
        assert_eq!(config, Config::default());
        assert!(adjusted.is_empty());
    }
}
