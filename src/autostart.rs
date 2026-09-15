//! Start at login, a setting the user turns on (never on by default): an XDG
//! autostart entry, `$XDG_CONFIG_HOME/autostart/<app id>.desktop`. Full
//! desktop sessions (GNOME, KDE, XFCE, Cinnamon, MATE, `LXQt`, Budgie) start
//! the entries there at login; bare compositors (Sway, Hyprland, niri) need
//! their own autostart setup.
//!
//! The file is the setting: there's no config key to disagree with it. A
//! desktop's startup-apps tool may keep the file and turn it off with
//! `Hidden=true` (or GNOME's `X-GNOME-Autostart-enabled=false`); that counts
//! as off.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::limits::MAX_CONFIG_BYTES;

/// Passed by the entry: start in the tray with the timer running.
pub const FLAG: &str = "--autostart";

#[derive(Debug, thiserror::Error)]
pub enum AutostartError {
    #[error("there's no config directory for autostart entries")]
    NoConfigDir,
    #[error("cannot find our own executable: {0}")]
    Exe(std::io::Error),
    #[error("our executable's path can't go in a desktop entry: {0:?}")]
    UnusablePath(PathBuf),
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
}

/// Where the entry lives.
pub fn entry_path() -> Result<PathBuf, AutostartError> {
    let dirs = directories::BaseDirs::new().ok_or(AutostartError::NoConfigDir)?;
    Ok(dirs.config_dir().join("autostart").join(crate::app::DESKTOP_FILE))
}

/// Whether the app starts at login: the entry exists and isn't turned off.
pub fn is_enabled(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else { return false };
    let mut text = String::new();
    let limit = u64::try_from(MAX_CONFIG_BYTES).unwrap_or(u64::MAX);
    match file.take(limit).read_to_string(&mut text) {
        Ok(_) => !turned_off(&text),
        Err(error) => {
            log::warn!("{}: {error}", path.display());
            false
        }
    }
}

/// Turns starting at login on (writes the entry) or off (removes it).
pub fn set_enabled(path: &Path, enabled: bool) -> Result<(), AutostartError> {
    if !enabled {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(AutostartError::Io { path: path.to_owned(), source }),
        };
    }
    let exe = executable()?;
    let exe_text = exe.to_str().filter(|text| !text.contains(['\n', '\r'])).ok_or(AutostartError::UnusablePath(exe.clone()))?;
    write_atomically(path, &entry_text(exe_text))
}

/// The path to start: under an `AppImage` the running file is inside a mount
/// that changes each launch, so the `AppImage` itself.
fn executable() -> Result<PathBuf, AutostartError> {
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        return Ok(PathBuf::from(appimage));
    }
    std::env::current_exe().map_err(AutostartError::Exe)
}

/// A temporary file next to the entry, then a rename: never half-written.
fn write_atomically(path: &Path, contents: &str) -> Result<(), AutostartError> {
    let io = |source| AutostartError::Io { path: path.to_owned(), source };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let temporary = path.with_extension("desktop.tmp");
    std::fs::write(&temporary, contents).map_err(io)?;
    std::fs::rename(&temporary, path).map_err(io)
}

/// The entry that starts the app at `exe`.
fn entry_text(exe: &str) -> String {
    assert!(!exe.contains(['\n', '\r']), "a desktop entry value is one line");
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={name}\n\
         Comment=Every N minutes of work, a cat takes over the screen for a short break\n\
         Exec={exec} {FLAG}\n\
         Icon={id}\n\
         Terminal=false\n",
        name = crate::app::NAME,
        exec = exec_argument(exe),
        id = crate::app::ID,
    )
}

/// An `Exec` argument, quoted as the desktop entry spec asks: `%` doubled;
/// arguments with reserved characters in double quotes, where `"`, `` ` ``,
/// `$` and `\` get a backslash; then the file's own string escaping doubles
/// every backslash.
fn exec_argument(argument: &str) -> String {
    const RESERVED: &[char] = &[' ', '\t', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(', ')', '`'];
    let argument = argument.replace('%', "%%");
    if !argument.contains(RESERVED) {
        return argument;
    }
    let mut quoted = String::with_capacity(argument.len() + 8);
    quoted.push('"');
    for character in argument.chars() {
        match character {
            '"' | '`' | '$' => {
                quoted.push_str("\\\\");
                quoted.push(character);
            }
            // Escaped once for the quoting, and both backslashes again for
            // the file's string escaping.
            '\\' => quoted.push_str("\\\\\\\\"),
            _ => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

/// Whether a desktop's startup-apps tool turned the entry off.
fn turned_off(text: &str) -> bool {
    text.lines().map(str::trim).any(|line| line == "Hidden=true" || line == "X-GNOME-Autostart-enabled=false")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_starts_the_app_in_the_tray() {
        let text = entry_text("/usr/bin/app");
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(text.contains("\nExec=/usr/bin/app --autostart\n"));
        assert!(text.contains(&format!("\nName={}\n", crate::app::NAME)));
        assert!(text.contains(&format!("\nIcon={}\n", crate::app::ID)));
        assert!(!turned_off(&text));
    }

    #[test]
    fn exec_arguments_are_quoted_as_the_spec_asks() {
        assert_eq!(exec_argument("/usr/bin/app"), "/usr/bin/app");
        assert_eq!(exec_argument("/opt/my apps/app"), "\"/opt/my apps/app\"");
        assert_eq!(exec_argument("/home/a$b/app"), "\"/home/a\\\\$b/app\"");
        assert_eq!(exec_argument("/tmp/100%/app"), "/tmp/100%%/app");
        assert_eq!(exec_argument("/tmp/a\\b"), "\"/tmp/a\\\\\\\\b\"");
    }

    #[test]
    fn startup_tools_can_turn_the_entry_off() {
        assert!(turned_off("[Desktop Entry]\nHidden=true\n"));
        assert!(turned_off("[Desktop Entry]\n X-GNOME-Autostart-enabled=false \n"));
        assert!(!turned_off("[Desktop Entry]\nHidden=false\n"));
    }

    fn scratch_entry(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{}-autostart-{}-{name}", crate::app::DIR, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("autostart").join(crate::app::DESKTOP_FILE)
    }

    #[test]
    fn turning_it_on_and_off_writes_and_removes_the_entry() {
        let path = scratch_entry("toggle");
        assert!(!is_enabled(&path));
        set_enabled(&path, true).unwrap();
        assert!(is_enabled(&path));
        assert!(!path.with_extension("desktop.tmp").exists(), "the temporary file is renamed away");
        set_enabled(&path, false).unwrap();
        assert!(!path.exists());
        set_enabled(&path, false).unwrap();
        std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn an_entry_hidden_by_the_desktop_counts_as_off() {
        let path = scratch_entry("hidden");
        set_enabled(&path, true).unwrap();
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("Hidden=true\n");
        std::fs::write(&path, text).unwrap();
        assert!(!is_enabled(&path));
        std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
    }
}
