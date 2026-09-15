//! One-time move from catnap, the app's working name until 2026-09-15: the
//! config folder and the start-at-login entry take the new names. It runs at
//! startup, before the config is loaded. Whatever it can't move is logged
//! and left in place. Remove it once no catnap build is left in use.

use std::path::Path;

use crate::app;
use crate::autostart::{self, AutostartError};

/// The old names: the only place they're still written.
const OLD_DIR: &str = "catnap";
const OLD_DESKTOP_FILE: &str = "catnap.desktop";

pub fn from_catnap() {
    let old = directories::ProjectDirs::from("", "", OLD_DIR);
    let new = directories::ProjectDirs::from("", "", app::DIR);
    if let (Some(old), Some(new)) = (old, new) {
        let (old, new) = (old.config_dir(), new.config_dir());
        match move_folder(old, new) {
            Ok(true) => log::info!("moved {} to {}", old.display(), new.display()),
            Ok(false) => {}
            Err(error) => log::warn!("cannot move {} to {}: {error}", old.display(), new.display()),
        }
    }
    let (Some(base), Ok(new_entry)) = (directories::BaseDirs::new(), autostart::entry_path()) else { return };
    let old_entry = base.config_dir().join("autostart").join(OLD_DESKTOP_FILE);
    match move_autostart(&old_entry, &new_entry) {
        Ok(true) => log::info!("replaced {} with {}", old_entry.display(), new_entry.display()),
        Ok(false) => {}
        Err(error) => log::warn!("cannot replace {}: {error}", old_entry.display()),
    }
}

/// Renames `old` to `new` when only `old` exists. True if it moved.
fn move_folder(old: &Path, new: &Path) -> std::io::Result<bool> {
    assert_ne!(old, new);
    if new.exists() || !old.is_dir() {
        return Ok(false);
    }
    if let Some(parent) = new.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(old, new)?;
    debug_assert!(new.is_dir() && !old.exists());
    Ok(true)
}

/// Replaces the old start-at-login entry: with a new one if it was on, with
/// none if a desktop's startup tool had turned it off. The new entry starts
/// this executable: the old one pointed at a binary that's gone. True if
/// there was an old entry.
fn move_autostart(old: &Path, new: &Path) -> Result<bool, AutostartError> {
    assert_ne!(old, new);
    if !old.exists() {
        return Ok(false);
    }
    if autostart::is_enabled(old) {
        autostart::set_enabled(new, true)?;
    }
    autostart::set_enabled(old, false)?;
    debug_assert!(!old.exists());
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{}-migrate-{}-{name}", app::DIR, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_old_folder_moves_when_there_is_no_new_one() {
        let dir = scratch("folder");
        let (old, new) = (dir.join(OLD_DIR), dir.join("new").join(app::DIR));
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("config.toml"), "kept").unwrap();
        assert!(move_folder(&old, &new).unwrap());
        assert_eq!(std::fs::read_to_string(new.join("config.toml")).unwrap(), "kept");
        assert!(!old.exists());
        assert!(!move_folder(&old, &new).unwrap(), "nothing is left to move");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_existing_new_folder_is_left_alone() {
        let dir = scratch("both");
        let (old, new) = (dir.join(OLD_DIR), dir.join(app::DIR));
        std::fs::create_dir(&old).unwrap();
        std::fs::create_dir(&new).unwrap();
        assert!(!move_folder(&old, &new).unwrap());
        assert!(old.exists() && new.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_start_at_login_entry_that_was_on_is_rewritten() {
        let dir = scratch("autostart-on");
        let (old, new) = (dir.join(OLD_DESKTOP_FILE), dir.join(app::DESKTOP_FILE));
        std::fs::write(&old, "[Desktop Entry]\nExec=/gone/catnap --autostart\n").unwrap();
        assert!(move_autostart(&old, &new).unwrap());
        assert!(!old.exists());
        assert!(autostart::is_enabled(&new));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_start_at_login_entry_that_was_off_is_dropped() {
        let dir = scratch("autostart-off");
        let (old, new) = (dir.join(OLD_DESKTOP_FILE), dir.join(app::DESKTOP_FILE));
        std::fs::write(&old, "[Desktop Entry]\nExec=/gone/catnap --autostart\nHidden=true\n").unwrap();
        assert!(move_autostart(&old, &new).unwrap());
        assert!(!old.exists() && !new.exists());
        assert!(!move_autostart(&old, &new).unwrap(), "nothing is left to replace");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
