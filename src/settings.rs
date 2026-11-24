//! Small persisted preferences in %LOCALAPPDATA%\AutoClicker\settings.json.
//! Add app-specific fields here, always with `#[serde(default)]` so older
//! settings files keep loading.

use crate::engine::Config;
use crate::keys::{DEFAULT_HOTKEY, Hotkey};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Settings {
    /// Check for updates in the background and install them silently.
    #[serde(default = "default_true")]
    pub auto_update: bool,
    /// Unix seconds of the last background update check, to throttle to once
    /// a day.
    #[serde(default)]
    pub last_update_check: u64,
    /// Global start/stop hotkey.
    #[serde(default = "default_hotkey")]
    pub hotkey: Hotkey,
    #[serde(default)]
    pub always_on_top: bool,
    /// What the main window was last set to.
    #[serde(default)]
    pub config: Config,
    /// Licence key as pasted by the user (only used once a paid tier exists).
    #[serde(default)]
    pub license_key: String,
}

fn default_true() -> bool {
    true
}

fn default_hotkey() -> Hotkey {
    DEFAULT_HOTKEY
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_update: true,
            last_update_check: 0,
            hotkey: DEFAULT_HOTKEY,
            always_on_top: false,
            config: Config::default(),
            license_key: String::new(),
        }
    }
}

pub fn data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into()))
        .join("AutoClicker")
}

fn settings_path() -> std::path::PathBuf {
    data_dir().join("settings.json")
}

pub fn load() -> Settings {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(settings: &Settings) {
    let _ = std::fs::create_dir_all(data_dir());
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(settings_path(), json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_files_keep_loading() {
        let s: Settings = serde_json::from_str(r#"{"auto_update": false}"#).unwrap();
        assert!(!s.auto_update);
        assert_eq!(s.hotkey, DEFAULT_HOTKEY);
        assert!(!s.always_on_top);
        assert_eq!(s.config, Config::default());
    }
}
