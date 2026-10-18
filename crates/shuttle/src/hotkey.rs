//! The kill hotkey: a `ctrl+alt+cmd+escape` string as a macOS virtual key code and Carbon modifier mask.

const CMD: u32 = 1 << 8;
const SHIFT: u32 = 1 << 9;
const OPTION: u32 = 1 << 11;
const CONTROL: u32 = 1 << 12;

const KEYS: [(&str, u32); 53] = [
    ("escape", 53),
    ("esc", 53),
    ("space", 49),
    ("return", 36),
    ("tab", 48),
    ("a", 0),
    ("s", 1),
    ("d", 2),
    ("f", 3),
    ("h", 4),
    ("g", 5),
    ("z", 6),
    ("x", 7),
    ("c", 8),
    ("v", 9),
    ("b", 11),
    ("q", 12),
    ("w", 13),
    ("e", 14),
    ("r", 15),
    ("y", 16),
    ("t", 17),
    ("1", 18),
    ("2", 19),
    ("3", 20),
    ("4", 21),
    ("6", 22),
    ("5", 23),
    ("9", 25),
    ("7", 26),
    ("8", 28),
    ("0", 29),
    ("o", 31),
    ("u", 32),
    ("i", 34),
    ("p", 35),
    ("l", 37),
    ("j", 38),
    ("k", 40),
    ("n", 45),
    ("m", 46),
    ("f1", 122),
    ("f2", 120),
    ("f3", 99),
    ("f4", 118),
    ("f5", 96),
    ("f6", 97),
    ("f7", 98),
    ("f8", 100),
    ("f9", 101),
    ("f10", 109),
    ("f11", 103),
    ("f12", 111),
];

/// A key combination as a macOS virtual key code and a Carbon modifier mask.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Hotkey {
    pub code: u32,
    pub modifiers: u32,
}

/// Reads `ctrl+alt+cmd+escape`: modifiers, then one key; a hotkey without a modifier would take that key from every app.
pub fn parse(spec: &str) -> Result<Hotkey, String> {
    let lower = spec.to_lowercase();
    let mut parts: Vec<&str> = lower.split('+').map(str::trim).collect();
    let key = parts.pop().unwrap_or_default();
    let mut modifiers = 0;
    for m in parts {
        modifiers |= match m {
            "cmd" | "command" | "meta" => CMD,
            "shift" => SHIFT,
            "alt" | "option" | "opt" => OPTION,
            "ctrl" | "control" => CONTROL,
            other => return Err(format!("kill_hotkey {spec}: unknown modifier {other}")),
        };
    }
    if modifiers == 0 {
        return Err(format!("kill_hotkey {spec}: needs a modifier"));
    }
    let code = KEYS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, c)| *c)
        .ok_or_else(|| format!("kill_hotkey {spec}: unknown key {key}"))?;
    Ok(Hotkey { code, modifiers })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_and_spelled_out_combinations_parse_and_a_bare_or_unknown_key_does_not() {
        assert_eq!(
            parse("ctrl+alt+cmd+escape"),
            Ok(Hotkey {
                code: 53,
                modifiers: CONTROL | OPTION | CMD
            })
        );
        assert_eq!(
            parse("Control + Option + K"),
            Ok(Hotkey {
                code: 40,
                modifiers: CONTROL | OPTION
            })
        );
        assert!(parse("escape").unwrap_err().contains("needs a modifier"));
        assert!(parse("ctrl+hyper+k").unwrap_err().contains("hyper"));
        assert!(parse("ctrl+pause").unwrap_err().contains("pause"));
    }
}
