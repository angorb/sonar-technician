//! Key-chord parsing ("Ctrl+Shift+F17") into Win32 modifier bits and a
//! virtual-key code. Pure, so it's tested on any platform;
//! `platform::hotkeys` registers the result with `RegisterHotKey`.

use std::fmt;

/// Win32 `MOD_*` values, so `Chord::mods` can go straight to `RegisterHotKey`.
pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;
pub const MOD_SHIFT: u32 = 0x0004;
pub const MOD_WIN: u32 = 0x0008;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub mods: u32,
    pub vk: u32,
}

/// Named keys (besides F1–F24, A–Z, 0–9, NumPad0–9) and their VK codes.
const NAMED_KEYS: &[(&str, u32)] = &[
    ("Space", 0x20),
    ("Enter", 0x0D),
    ("Tab", 0x09),
    ("Esc", 0x1B),
    ("Backspace", 0x08),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("Up", 0x26),
    ("Down", 0x28),
    ("Left", 0x25),
    ("Right", 0x27),
    ("Pause", 0x13),
    ("ScrollLock", 0x91),
    ("PrintScreen", 0x2C),
    ("VolumeMute", 0xAD),
    ("VolumeDown", 0xAE),
    ("VolumeUp", 0xAF),
    ("MediaNext", 0xB0),
    ("MediaPrev", 0xB1),
    ("MediaStop", 0xB2),
    ("MediaPlayPause", 0xB3),
];

fn key_vk(name: &str) -> Option<u32> {
    let upper = name.to_ascii_uppercase();
    if let Some(n) = upper.strip_prefix('F').and_then(|n| n.parse::<u32>().ok())
        && (1..=24).contains(&n)
    {
        return Some(0x70 + n - 1);
    }
    if let Some(n) = upper
        .strip_prefix("NUMPAD")
        .and_then(|n| n.parse::<u32>().ok())
        && n <= 9
    {
        return Some(0x60 + n);
    }
    if upper.len() == 1 {
        let c = upper.as_bytes()[0];
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(u32::from(c));
        }
    }
    NAMED_KEYS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, vk)| *vk)
}

fn key_name(vk: u32) -> String {
    match vk {
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        0x60..=0x69 => format!("NumPad{}", vk - 0x60),
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(vk).map(String::from).unwrap_or_default(),
        _ => NAMED_KEYS
            .iter()
            .find(|(_, v)| *v == vk)
            .map(|(n, _)| (*n).to_owned())
            .unwrap_or_else(|| format!("VK{vk:#04X}")),
    }
}

impl std::str::FromStr for Chord {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut mods = 0;
        let mut vk = None;
        for part in s.split('+').map(str::trim) {
            if part.is_empty() {
                return Err(format!("`{s}`: empty key name"));
            }
            let m = match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => Some(MOD_CONTROL),
                "alt" => Some(MOD_ALT),
                "shift" => Some(MOD_SHIFT),
                "win" | "super" => Some(MOD_WIN),
                _ => None,
            };
            if let Some(m) = m {
                if mods & m != 0 {
                    return Err(format!("`{s}`: `{part}` given twice"));
                }
                mods |= m;
                continue;
            }
            if vk.is_some() {
                return Err(format!("`{s}`: more than one non-modifier key"));
            }
            vk = Some(key_vk(part).ok_or_else(|| {
                format!(
                    "`{s}`: unknown key `{part}` (try F13–F24, A–Z, 0–9, PageUp, VolumeUp, ...)"
                )
            })?);
        }
        let vk = vk.ok_or_else(|| format!("`{s}`: needs a key, not just modifiers"))?;
        Ok(Chord { mods, vk })
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (bit, name) in [
            (MOD_CONTROL, "Ctrl"),
            (MOD_ALT, "Alt"),
            (MOD_SHIFT, "Shift"),
            (MOD_WIN, "Win"),
        ] {
            if self.mods & bit != 0 {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(&key_name(self.vk))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Chord {
        s.parse().unwrap()
    }

    #[test]
    fn function_keys() {
        assert_eq!(p("F13"), Chord { mods: 0, vk: 0x7C });
        assert_eq!(p("f17").vk, 0x80);
        assert_eq!(p("F24").vk, 0x87);
        assert!("F25".parse::<Chord>().is_err());
        assert!("F0".parse::<Chord>().is_err());
    }

    #[test]
    fn modifiers() {
        let c = p("ctrl + Shift+F19");
        assert_eq!(c.mods, MOD_CONTROL | MOD_SHIFT);
        assert_eq!(c.vk, 0x82);
        assert_eq!(p("Alt+Win+A").mods, MOD_ALT | MOD_WIN);
    }

    #[test]
    fn other_keys() {
        assert_eq!(p("A").vk, 0x41);
        assert_eq!(p("7").vk, 0x37);
        assert_eq!(p("NumPad3").vk, 0x63);
        assert_eq!(p("pageup").vk, 0x21);
        assert_eq!(p("VolumeUp").vk, 0xAF);
    }

    #[test]
    fn errors() {
        for bad in [
            "",
            "Ctrl+",
            "Ctrl+Shift",
            "F17+F18",
            "Ctrl+Ctrl+F1",
            "Hyper+F1",
            "AB",
        ] {
            assert!(bad.parse::<Chord>().is_err(), "{bad}");
        }
    }

    #[test]
    fn display_is_normalised() {
        assert_eq!(p("shift+ctrl+f17").to_string(), "Ctrl+Shift+F17");
        assert_eq!(p("alt+numpad0").to_string(), "Alt+NumPad0");
        assert_eq!(p("Win+volumeup").to_string(), "Win+VolumeUp");
        assert_eq!(p("Ctrl+q").to_string(), "Ctrl+Q");
    }
}
