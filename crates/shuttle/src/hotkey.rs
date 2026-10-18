//! The kill hotkey: a `ctrl+alt+cmd+escape` string as a virtual key code and modifier mask, macOS Carbon or Windows `RegisterHotKey`.

const CMD: u32 = 1 << 8;
const SHIFT: u32 = 1 << 9;
const OPTION: u32 = 1 << 11;
const CONTROL: u32 = 1 << 12;

const MOD_ALT: u32 = 0x1;
const MOD_CONTROL: u32 = 0x2;
const MOD_SHIFT: u32 = 0x4;
const MOD_WIN: u32 = 0x8;

const KEYS: [(&str, u32, u32); 53] = [
    ("escape", 53, 0x1B),
    ("esc", 53, 0x1B),
    ("space", 49, 0x20),
    ("return", 36, 0x0D),
    ("tab", 48, 0x09),
    ("a", 0, 0x41),
    ("s", 1, 0x53),
    ("d", 2, 0x44),
    ("f", 3, 0x46),
    ("h", 4, 0x48),
    ("g", 5, 0x47),
    ("z", 6, 0x5A),
    ("x", 7, 0x58),
    ("c", 8, 0x43),
    ("v", 9, 0x56),
    ("b", 11, 0x42),
    ("q", 12, 0x51),
    ("w", 13, 0x57),
    ("e", 14, 0x45),
    ("r", 15, 0x52),
    ("y", 16, 0x59),
    ("t", 17, 0x54),
    ("1", 18, 0x31),
    ("2", 19, 0x32),
    ("3", 20, 0x33),
    ("4", 21, 0x34),
    ("6", 22, 0x36),
    ("5", 23, 0x35),
    ("9", 25, 0x39),
    ("7", 26, 0x37),
    ("8", 28, 0x38),
    ("0", 29, 0x30),
    ("o", 31, 0x4F),
    ("u", 32, 0x55),
    ("i", 34, 0x49),
    ("p", 35, 0x50),
    ("l", 37, 0x4C),
    ("j", 38, 0x4A),
    ("k", 40, 0x4B),
    ("n", 45, 0x4E),
    ("m", 46, 0x4D),
    ("f1", 122, 0x70),
    ("f2", 120, 0x71),
    ("f3", 99, 0x72),
    ("f4", 118, 0x73),
    ("f5", 96, 0x74),
    ("f6", 97, 0x75),
    ("f7", 98, 0x76),
    ("f8", 100, 0x77),
    ("f9", 101, 0x78),
    ("f10", 109, 0x79),
    ("f11", 103, 0x7A),
    ("f12", 111, 0x7B),
];

/// A key combination as a virtual key code and a modifier mask of this OS.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Hotkey {
    pub code: u32,
    pub modifiers: u32,
}

/// Reads `ctrl+alt+cmd+escape`: modifiers, then one key; a hotkey without a modifier would take that key from every app.
pub fn parse(spec: &str) -> Result<Hotkey, String> {
    parse_for(spec, cfg!(windows))
}

fn parse_for(spec: &str, windows: bool) -> Result<Hotkey, String> {
    let lower = spec.to_lowercase();
    let mut parts = lower.split('+').map(str::trim);
    let key = parts.next_back().unwrap_or_default();
    let mut modifiers = 0;
    for m in parts {
        let (mac, win) = match m {
            "cmd" | "command" | "meta" | "win" | "super" => (CMD, MOD_WIN),
            "shift" => (SHIFT, MOD_SHIFT),
            "alt" | "option" | "opt" => (OPTION, MOD_ALT),
            "ctrl" | "control" => (CONTROL, MOD_CONTROL),
            other => return Err(format!("kill_hotkey {spec}: unknown modifier {other}")),
        };
        modifiers |= if windows { win } else { mac };
    }
    if modifiers == 0 {
        return Err(format!("kill_hotkey {spec}: needs a modifier"));
    }
    let code = KEYS
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|&(_, mac, win)| if windows { win } else { mac })
        .ok_or_else(|| format!("kill_hotkey {spec}: unknown key {key}"))?;
    Ok(Hotkey { code, modifiers })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_and_spelled_out_combinations_parse_and_a_bare_or_unknown_key_does_not() {
        assert_eq!(
            parse_for("ctrl+alt+cmd+escape", false),
            Ok(Hotkey {
                code: 53,
                modifiers: CONTROL | OPTION | CMD
            })
        );
        assert_eq!(
            parse_for("Control + Option + K", false),
            Ok(Hotkey {
                code: 40,
                modifiers: CONTROL | OPTION
            })
        );
        assert!(
            parse_for("escape", false)
                .unwrap_err()
                .contains("needs a modifier")
        );
        assert!(
            parse_for("ctrl+hyper+k", false)
                .unwrap_err()
                .contains("hyper")
        );
        assert!(
            parse_for("ctrl+pause", false)
                .unwrap_err()
                .contains("pause")
        );
    }

    #[test]
    fn windows_combinations_parse_to_virtual_keys_and_register_hotkey_modifiers() {
        assert_eq!(
            parse_for("ctrl+alt+shift+escape", true),
            Ok(Hotkey {
                code: 0x1B,
                modifiers: MOD_CONTROL | MOD_ALT | MOD_SHIFT
            })
        );
        assert_eq!(
            parse_for("Win + F12", true),
            Ok(Hotkey {
                code: 0x7B,
                modifiers: MOD_WIN
            })
        );
        assert_eq!(
            parse_for("ctrl+shift+k", true),
            Ok(Hotkey {
                code: 0x4B,
                modifiers: MOD_CONTROL | MOD_SHIFT
            })
        );
        assert!(parse_for("escape", true).is_err());
    }
}
