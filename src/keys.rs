//! Key names, virtual-key codes and global hotkey values. Pure data and
//! helpers, no Windows calls, so everything here is unit tested.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

pub const MOD_ALT: u32 = 1;
pub const MOD_CONTROL: u32 = 2;
pub const MOD_SHIFT: u32 = 4;
pub const MOD_WIN: u32 = 8;

pub const VK_F6: u32 = 0x75;
pub const VK_SPACE: u32 = 0x20;
const VK_PAUSE: u32 = 0x13;
const VK_SCROLL: u32 = 0x91;

/// A global hotkey: modifier bits (`MOD_*`) plus a virtual-key code.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u32,
}

pub const DEFAULT_HOTKEY: Hotkey = Hotkey { mods: 0, vk: VK_F6 };

impl Hotkey {
    pub fn label(&self) -> String {
        let mut s = String::new();
        for (bit, name) in [
            (MOD_CONTROL, "Ctrl"),
            (MOD_ALT, "Alt"),
            (MOD_SHIFT, "Shift"),
            (MOD_WIN, "Win"),
        ] {
            if self.mods & bit != 0 {
                s.push_str(name);
                s.push('+');
            }
        }
        s.push_str(&key_name(self.vk));
        s
    }

    /// A bare letter or digit as a global hotkey would stop that key working
    /// everywhere, so those need Ctrl, Alt or Win. Function keys, Pause and
    /// Scroll Lock are fine on their own.
    pub fn is_acceptable(&self) -> bool {
        let function_key = (0x70..=0x87).contains(&self.vk);
        function_key
            || self.vk == VK_PAUSE
            || self.vk == VK_SCROLL
            || self.mods & (MOD_CONTROL | MOD_ALT | MOD_WIN) != 0
    }
}

/// Every key the app can press or bind: name and virtual-key code.
pub fn key_table() -> &'static [(String, u32)] {
    static TABLE: OnceLock<Vec<(String, u32)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t: Vec<(String, u32)> = Vec::new();
        for c in 'A'..='Z' {
            t.push((c.to_string(), c as u32));
        }
        for c in '0'..='9' {
            t.push((c.to_string(), c as u32));
        }
        for n in 1..=24u32 {
            t.push((format!("F{n}"), 0x6F + n));
        }
        for n in 0..=9u32 {
            t.push((format!("Numpad {n}"), 0x60 + n));
        }
        for (name, vk) in [
            ("Space", VK_SPACE),
            ("Enter", 0x0D),
            ("Tab", 0x09),
            ("Escape", 0x1B),
            ("Backspace", 0x08),
            ("Up", 0x26),
            ("Down", 0x28),
            ("Left", 0x25),
            ("Right", 0x27),
            ("Home", 0x24),
            ("End", 0x23),
            ("Page Up", 0x21),
            ("Page Down", 0x22),
            ("Insert", 0x2D),
            ("Delete", 0x2E),
            ("Pause", VK_PAUSE),
            ("Scroll Lock", VK_SCROLL),
            ("Left Shift", 0xA0),
            ("Left Ctrl", 0xA2),
            ("Left Alt", 0xA4),
            (";", 0xBA),
            ("=", 0xBB),
            (",", 0xBC),
            ("-", 0xBD),
            (".", 0xBE),
            ("/", 0xBF),
            ("`", 0xC0),
            ("[", 0xDB),
            ("\\", 0xDC),
            ("]", 0xDD),
            ("'", 0xDE),
        ] {
            t.push((name.to_string(), vk));
        }
        t
    })
}

pub fn key_name(vk: u32) -> String {
    key_table()
        .iter()
        .find(|(_, v)| *v == vk)
        .map(|(n, _)| n.clone())
        .unwrap_or_else(|| format!("Key 0x{vk:02X}"))
}

/// Keys that need the "extended" flag when sent as scan codes.
pub fn is_extended(vk: u32) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2D | 0x2E)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_list_modifiers_in_a_fixed_order() {
        assert_eq!(DEFAULT_HOTKEY.label(), "F6");
        let h = Hotkey { mods: MOD_SHIFT | MOD_CONTROL, vk: 'K' as u32 };
        assert_eq!(h.label(), "Ctrl+Shift+K");
        let h = Hotkey { mods: MOD_ALT | MOD_WIN, vk: 0x7B };
        assert_eq!(h.label(), "Alt+Win+F12");
    }

    #[test]
    fn bare_typing_keys_are_rejected_as_hotkeys() {
        assert!(DEFAULT_HOTKEY.is_acceptable());
        assert!(Hotkey { mods: 0, vk: VK_PAUSE }.is_acceptable());
        assert!(Hotkey { mods: MOD_CONTROL, vk: 'A' as u32 }.is_acceptable());
        assert!(!Hotkey { mods: 0, vk: 'A' as u32 }.is_acceptable());
        assert!(!Hotkey { mods: MOD_SHIFT, vk: 'A' as u32 }.is_acceptable());
        assert!(!Hotkey { mods: 0, vk: VK_SPACE }.is_acceptable());
    }

    #[test]
    fn key_table_has_unique_codes_and_round_trips() {
        let table = key_table();
        for (i, (name, vk)) in table.iter().enumerate() {
            assert_eq!(key_name(*vk), *name);
            assert!(table[i + 1..].iter().all(|(_, other)| other != vk), "{name}");
        }
        assert_eq!(key_name(0x70), "F1");
        assert_eq!(key_name(0x87), "F24");
        assert_eq!(key_name(0xFF), "Key 0xFF");
    }

    #[test]
    fn arrows_and_navigation_are_extended() {
        assert!(is_extended(0x25));
        assert!(is_extended(0x2E));
        assert!(!is_extended('A' as u32));
    }
}
