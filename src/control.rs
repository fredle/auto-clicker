//! State shared between the window and the hotkey/tray thread. Hotkeys and
//! the tray work through this directly (not through the UI loop), so they
//! still work while the window is minimised.

use crate::engine::{Config, Runner};
use crate::keys::Hotkey;
use crate::profiles::Profile;
use crate::winsys::{self, Event};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Hotkey id of the global start/stop key; profiles use `PROFILE_BASE + index`.
pub const GLOBAL_ID: i32 = 1;
pub const PROFILE_BASE: i32 = 100;

pub struct Shared {
    pub runner: Runner,
    /// The configuration a hotkey will start. The window keeps this in sync.
    pub config: Mutex<Config>,
    pub profiles: Mutex<Vec<Profile>>,
    /// Set when a hotkey replaced `config`, so the window reloads it.
    pub external_change: AtomicBool,
    pub licensed: AtomicBool,
    /// Why the last hotkey/tray start was refused.
    pub error: Mutex<Option<String>>,
}

impl Shared {
    pub fn start_or_stop(&self) {
        if self.runner.is_running() {
            self.runner.stop();
            return;
        }
        let config = self.config.lock().unwrap().clone();
        match crate::engine::validate(&config, self.licensed.load(Ordering::SeqCst)) {
            Ok(()) => {
                *self.error.lock().unwrap() = None;
                self.runner.start(config);
            }
            Err(e) => *self.error.lock().unwrap() = Some(e),
        }
    }

    pub fn handle(&self, event: Event) {
        match event {
            Event::Hotkey(GLOBAL_ID) | Event::TrayToggle => self.start_or_stop(),
            Event::Hotkey(id) if id >= PROFILE_BASE => {
                let profile = self.profiles.lock().unwrap().get((id - PROFILE_BASE) as usize).cloned();
                if let Some(p) = profile {
                    if self.runner.is_running() {
                        self.runner.stop();
                        return;
                    }
                    *self.config.lock().unwrap() = p.config;
                    self.external_change.store(true, Ordering::SeqCst);
                    self.start_or_stop();
                }
            }
            Event::Hotkey(_) => {}
            Event::TrayShow => winsys::show_main_window(),
            Event::TrayQuit => {
                self.runner.stop_and_wait();
                std::process::exit(0);
            }
        }
    }
}

/// The hotkeys to register: the global one plus every profile that has one.
pub fn hotkey_list(global: Hotkey, profiles: &[Profile]) -> Vec<(i32, Hotkey)> {
    let mut list = vec![(GLOBAL_ID, global)];
    for (i, p) in profiles.iter().enumerate() {
        if let Some(hk) = p.hotkey {
            list.push((PROFILE_BASE + i as i32, hk));
        }
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Button, Input};
    use std::sync::Arc;

    struct Quiet;
    impl Input for Quiet {
        fn cursor(&self) -> (i32, i32) {
            (0, 0)
        }
        fn move_to(&self, _: i32, _: i32) {}
        fn click(&self, _: Button, _: bool) {}
        fn key_down(&self, _: u32) {}
        fn key_up(&self, _: u32) {}
    }

    fn shared() -> Shared {
        Shared {
            runner: Runner::new(Arc::new(Quiet), Arc::new(|_| {})),
            config: Mutex::new(Config::default()),
            profiles: Mutex::new(Vec::new()),
            external_change: AtomicBool::new(false),
            licensed: AtomicBool::new(false),
            error: Mutex::new(None),
        }
    }

    #[test]
    fn hotkey_list_numbers_profiles_by_position() {
        let hk = |vk| Some(Hotkey { mods: 2, vk });
        let p = |name: &str, hotkey| Profile { name: name.into(), config: Config::default(), hotkey };
        let list = hotkey_list(
            Hotkey { mods: 0, vk: 0x75 },
            &[p("a", None), p("b", hk(0x70)), p("c", hk(0x71))],
        );
        assert_eq!(list.len(), 3);
        assert_eq!(list[1].0, PROFILE_BASE + 1);
        assert_eq!(list[2].0, PROFILE_BASE + 2);
    }

    #[test]
    fn global_hotkey_toggles_and_reports_invalid_configs() {
        let s = shared();
        s.config.lock().unwrap().interval_ms = 5;
        s.handle(Event::Hotkey(GLOBAL_ID));
        assert!(s.runner.is_running());
        s.handle(Event::Hotkey(GLOBAL_ID));
        s.runner.stop_and_wait();
        assert!(!s.runner.is_running());

        s.config.lock().unwrap().click_enabled = false;
        s.handle(Event::TrayToggle);
        assert!(!s.runner.is_running());
        assert!(s.error.lock().unwrap().is_some());
    }

    #[test]
    fn profile_hotkey_loads_the_profile_then_starts() {
        let s = shared();
        let cfg = Config { interval_ms: 7, ..Config::default() };
        s.profiles.lock().unwrap().push(Profile {
            name: "p".into(),
            config: cfg,
            hotkey: Some(Hotkey { mods: 2, vk: 0x70 }),
        });
        s.handle(Event::Hotkey(PROFILE_BASE));
        assert_eq!(s.config.lock().unwrap().interval_ms, 7);
        assert!(s.external_change.load(Ordering::SeqCst));
        assert!(s.runner.is_running());
        s.runner.stop_and_wait();
        // An id with no profile does nothing.
        s.handle(Event::Hotkey(PROFILE_BASE + 5));
        assert!(!s.runner.is_running());
    }
}
