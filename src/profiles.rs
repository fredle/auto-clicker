//! Saved profiles: a named configuration with an optional hotkey of its own.
//! Stored in %LOCALAPPDATA%\AutoClicker\profiles.json.

use crate::engine::Config;
use crate::keys::Hotkey;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub config: Config,
    #[serde(default)]
    pub hotkey: Option<Hotkey>,
}

fn path() -> std::path::PathBuf {
    crate::settings::data_dir().join("profiles.json")
}

pub fn load() -> Vec<Profile> {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(profiles: &[Profile]) {
    let _ = std::fs::create_dir_all(crate::settings::data_dir());
    if let Ok(json) = serde_json::to_string_pretty(profiles) {
        let _ = std::fs::write(path(), json);
    }
}

/// Add or replace (by name, ignoring case) a profile. Returns false when
/// that would exceed `max` profiles.
pub fn upsert(profiles: &mut Vec<Profile>, name: &str, config: &Config, max: usize) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    if let Some(p) = profiles.iter_mut().find(|p| p.name.eq_ignore_ascii_case(name)) {
        p.config = config.clone();
        return true;
    }
    if profiles.len() >= max {
        return false;
    }
    profiles.push(Profile { name: name.to_string(), config: config.clone(), hotkey: None });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_adds_replaces_and_respects_the_limit() {
        let mut list = Vec::new();
        let a = Config { interval_ms: 5, ..Config::default() };
        assert!(upsert(&mut list, "  Farm ", &a, 1));
        assert_eq!(list[0].name, "Farm");
        // Same name replaces even when at the limit.
        let b = Config { interval_ms: 9, ..Config::default() };
        assert!(upsert(&mut list, "farm", &b, 1));
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].config.interval_ms, 9);
        // A second name is over the limit.
        assert!(!upsert(&mut list, "Other", &a, 1));
        assert!(!upsert(&mut list, "   ", &a, 5));
        assert!(upsert(&mut list, "Other", &a, 2));
    }

    #[test]
    fn profiles_round_trip_through_json() {
        let list = vec![Profile {
            name: "x".into(),
            config: Config::default(),
            hotkey: Some(Hotkey { mods: 2, vk: 0x70 }),
        }];
        let json = serde_json::to_string(&list).unwrap();
        assert_eq!(serde_json::from_str::<Vec<Profile>>(&json).unwrap(), list);
    }
}
