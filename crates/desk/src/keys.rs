//! The desk key vocabulary per OS: combinations such as `ctrl+shift+s` in the names cua-driver takes.

use serde_json::{Value, json};

const SHIFTED: &str = "!@#$%^&*()_+:\"<>?~{}|";
const BASE: &str = "1234567890-=;',./`[]\\";
const KEYS: [&str; 20] = [
    "return",
    "tab",
    "escape",
    "space",
    "backspace",
    "delete",
    "insert",
    "home",
    "end",
    "pageup",
    "pagedown",
    "up",
    "down",
    "left",
    "right",
    "shift",
    "ctrl",
    "alt",
    "capslock",
    "numlock",
];
const LOGO: [&str; 11] = [
    "cmd", "command", "meta", "win", "windows", "winleft", "winright", "super", "super_l",
    "super_r", "mod4",
];
const ALIASES: [(&str, &str); 60] = [
    ("control", "ctrl"),
    ("primary", "ctrl"),
    ("ctl", "ctrl"),
    ("control_l", "ctrl"),
    ("control_r", "ctrl"),
    ("ctrlleft", "ctrl"),
    ("ctrlright", "ctrl"),
    ("shift_l", "shift"),
    ("shift_r", "shift"),
    ("shiftleft", "shift"),
    ("shiftright", "shift"),
    ("alt_l", "alt"),
    ("alt_r", "alt"),
    ("altleft", "alt"),
    ("altright", "alt"),
    ("option", "alt"),
    ("opt", "alt"),
    ("mod1", "alt"),
    ("enter", "return"),
    ("kp_enter", "return"),
    ("numpadenter", "return"),
    ("esc", "escape"),
    ("del", "delete"),
    ("forwarddelete", "delete"),
    ("fwddelete", "delete"),
    ("deleteforward", "delete"),
    ("ins", "insert"),
    ("prior", "pageup"),
    ("pgup", "pageup"),
    ("next", "pagedown"),
    ("pgdn", "pagedown"),
    ("pgdown", "pagedown"),
    ("pagedn", "pagedown"),
    ("arrowup", "up"),
    ("arrowdown", "down"),
    ("arrowleft", "left"),
    ("arrowright", "right"),
    ("uparrow", "up"),
    ("downarrow", "down"),
    ("leftarrow", "left"),
    ("rightarrow", "right"),
    ("spacebar", "space"),
    ("bksp", "backspace"),
    ("plus", "+"),
    ("minus", "-"),
    ("dash", "-"),
    ("equal", "="),
    ("equals", "="),
    ("asterisk", "*"),
    ("star", "*"),
    ("slash", "/"),
    ("backslash", "\\"),
    ("period", "."),
    ("dot", "."),
    ("comma", ","),
    ("semicolon", ";"),
    ("colon", ":"),
    ("underscore", "_"),
    ("parenleft", "("),
    ("parenright", ")"),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Os {
    Windows,
    Mac,
    Linux,
}

impl Os {
    fn logo(self) -> &'static str {
        match self {
            Self::Windows => "win",
            Self::Mac => "cmd",
            Self::Linux => "super",
        }
    }
}

/// The driver's key names of `combo`: `Ctrl+Shift+S` is `["ctrl", "shift", "s"]`; a name no driver knows fails.
pub fn key_names(combo: &str, os: Os) -> Result<Vec<String>, String> {
    let mut parts: Vec<&str> = combo.split('+').collect();
    if parts.ends_with(&["", ""]) {
        parts.truncate(parts.len() - 2);
        parts.push("+");
    }
    parts.iter().map(|k| key_name(k, os, combo)).collect()
}

pub fn uia_combo(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    key_names(text, Os::Windows).ok().map(|k| k.join("+"))
}

/// A macOS menu shortcut from AXMenuItemCmdChar and AXMenuItemCmdModifiers as `cmd+shift+s`.
pub fn ax_combo(cmd_char: &str, modifiers: i64) -> Option<String> {
    let key = cmd_char.trim();
    if key.chars().count() != 1 || key.chars().any(|c| ('\u{f700}'..='\u{f8ff}').contains(&c)) {
        return None;
    }
    let mut combo: Vec<&str> = Vec::new();
    for (bit, name) in [(4, "ctrl"), (2, "alt"), (1, "shift")] {
        if modifiers & bit != 0 {
            combo.push(name);
        }
    }
    if modifiers & 8 == 0 {
        combo.push("cmd");
    }
    let key = key.to_lowercase();
    combo.push(&key);
    Some(combo.join("+"))
}

/// The US-layout keys that type `c`, shift first for a shifted character; none for a character Windows types as Unicode.
pub fn keystroke(c: char) -> Option<Vec<String>> {
    match c {
        'a'..='z' | '0'..='9' => Some(vec![c.to_string()]),
        ' ' => Some(vec!["space".to_owned()]),
        'A'..='Z' => Some(vec!["shift".to_owned(), c.to_ascii_lowercase().to_string()]),
        _ => {
            if let Some(i) = SHIFTED.find(c) {
                return BASE[i..]
                    .chars()
                    .next()
                    .map(|k| vec!["shift".to_owned(), k.to_string()]);
            }
            BASE.contains(c).then(|| vec![c.to_string()])
        }
    }
}

pub(crate) fn hid_plus(req: &mut Value) {
    if req["key"] == "+" {
        req["key"] = "=".into();
        req["modifiers"] = json!(["shift"]);
    } else if let Some(keys) = req.get_mut("keys").and_then(Value::as_array_mut)
        && let Some(i) = keys.iter().position(|k| k == "+")
    {
        keys[i] = "=".into();
        if !keys.iter().any(|k| k == "shift") {
            keys.insert(i, "shift".into());
        }
    }
}

/// A UIA AcceleratorKey or AccessKey such as `Ctrl+Shift+S` as `ctrl+shift+s`; none when empty.
fn key_name(key: &str, os: Os, combo: &str) -> Result<String, String> {
    let k = key.trim().to_lowercase();
    if k.is_empty() {
        return Err(format!("bad key combination {combo:?}"));
    }
    let named = |alias: &str| k.chars().count() > 1 && folded(alias).eq(folded(&k));
    if LOGO.iter().any(|logo| named(logo)) {
        return Ok(os.logo().to_owned());
    }
    let name = ALIASES
        .iter()
        .find(|(alias, _)| named(alias))
        .map_or(k.as_str(), |(_, name)| name);
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        return match n {
            1..=12 => Ok(format!("f{n}")),
            _ => Err(format!(
                "unknown key {name:?} in {combo:?}: the function keys are f1 to f12"
            )),
        };
    }
    let name = match (name, os) {
        ("insert" | "numlock", Os::Mac) => {
            return Err(format!("{name:?} in {combo:?} is no key on macOS"));
        }
        ("delete", Os::Mac) => "forward_delete",
        ("menu", Os::Mac | Os::Windows) => "alt",
        ("fn", Os::Mac) => "fn",
        _ if name.chars().count() == 1 => name,
        _ => KEYS
            .into_iter()
            .find(|known| folded(known).eq(folded(name)))
            .ok_or_else(|| {
                format!(
                    "unknown key {name:?} in {combo:?}: a key is one character, f1 to f12, or one of {}",
                    KEYS.join(", ")
                )
            })?,
    };
    Ok(name.to_owned())
}

fn folded(name: &str) -> impl Iterator<Item = char> + '_ {
    name.chars().filter(|c| !matches!(c, '_' | '-' | ' '))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combinations_use_the_driver_names_of_each_os() {
        assert_eq!(
            key_names("Ctrl+Shift+S", Os::Windows).unwrap(),
            ["ctrl", "shift", "s"]
        );
        assert_eq!(key_names("super+l", Os::Windows).unwrap(), ["win", "l"]);
        assert_eq!(key_names("super+l", Os::Mac).unwrap(), ["cmd", "l"]);
        assert_eq!(key_names("option+esc", Os::Mac).unwrap(), ["alt", "escape"]);
        assert!(key_names("ctrl+", Os::Mac).is_err());
    }

    #[test]
    fn linux_combinations_take_the_keysym_aliases_and_a_plus_key() {
        assert_eq!(key_names("Super_L+l", Os::Linux).unwrap(), ["super", "l"]);
        assert_eq!(key_names("cmd+q", Os::Linux).unwrap(), ["super", "q"]);
        assert_eq!(key_names("ctrl++", Os::Linux).unwrap(), ["ctrl", "+"]);
        assert_eq!(key_names("+", Os::Linux).unwrap(), ["+"]);
        assert_eq!(key_names("KP_Enter", Os::Linux).unwrap(), ["return"]);
        assert!(key_names("ctrl+", Os::Linux).is_err());
    }

    #[test]
    fn every_os_takes_the_same_aliases_and_refuses_a_name_no_driver_knows() {
        for os in [Os::Windows, Os::Mac, Os::Linux] {
            for (combo, keys) in [
                ("ArrowUp", ["up"].as_slice()),
                ("left_arrow", &["left"]),
                ("Up-Arrow", &["up"]),
                ("kp_enter", &["return"]),
                ("bksp", &["backspace"]),
                ("spacebar", &["space"]),
                ("PgDown", &["pagedown"]),
                ("Page Down", &["pagedown"]),
                ("caps_lock", &["capslock"]),
                ("ctrl+minus", &["ctrl", "-"]),
                ("shift+parenleft", &["shift", "("]),
                ("parenright", &[")"]),
                ("alt+F12", &["alt", "f12"]),
                ("F01", &["f1"]),
            ] {
                assert_eq!(key_names(combo, os).unwrap(), keys, "{combo} on {os:?}");
            }
            for combo in ["ctrl+printscreen", "f13", "f0", "pause"] {
                let err = key_names(combo, os).unwrap_err();
                assert!(err.starts_with("unknown key"), "{combo} on {os:?}: {err}");
            }
        }
    }

    #[test]
    fn the_plus_key_is_plus_and_the_hid_path_takes_it_as_shift_and_equals() {
        for os in [Os::Windows, Os::Mac, Os::Linux] {
            assert_eq!(key_names("ctrl++", os).unwrap(), ["ctrl", "+"]);
            assert_eq!(key_names("ctrl+plus", os).unwrap(), ["ctrl", "+"]);
        }
        for (mut req, want) in [
            (
                json!({"keys": ["cmd", "+"]}),
                json!({"keys": ["cmd", "shift", "="]}),
            ),
            (
                json!({"keys": ["cmd", "shift", "+"]}),
                json!({"keys": ["cmd", "shift", "="]}),
            ),
            (
                json!({"key": "+"}),
                json!({"key": "=", "modifiers": ["shift"]}),
            ),
            (json!({"keys": ["cmd", "0"]}), json!({"keys": ["cmd", "0"]})),
            (json!({"key": "return"}), json!({"key": "return"})),
            (
                json!({"x": 10.0, "y": 20.0, "scope": "desktop", "delivery_mode": "foreground"}),
                json!({"x": 10.0, "y": 20.0, "scope": "desktop", "delivery_mode": "foreground"}),
            ),
        ] {
            hid_plus(&mut req);
            assert_eq!(req, want);
        }
    }

    #[test]
    fn os_only_keys_pass_on_their_os_and_fail_elsewhere() {
        assert_eq!(key_names("fn+f1", Os::Mac).unwrap(), ["fn", "f1"]);
        assert_eq!(key_names("menu", Os::Mac).unwrap(), ["alt"]);
        assert_eq!(key_names("menu", Os::Windows).unwrap(), ["alt"]);
        assert_eq!(
            key_names("shift+ins", Os::Windows).unwrap(),
            ["shift", "insert"]
        );
        assert_eq!(key_names("num_lock", Os::Linux).unwrap(), ["numlock"]);
        for (combo, os) in [
            ("fn+f1", Os::Windows),
            ("fn", Os::Linux),
            ("menu", Os::Linux),
        ] {
            let err = key_names(combo, os).unwrap_err();
            assert!(err.starts_with("unknown key"), "{combo} on {os:?}: {err}");
        }
        for combo in ["shift+insert", "numlock"] {
            let err = key_names(combo, Os::Mac).unwrap_err();
            assert!(err.ends_with("is no key on macOS"), "{combo}: {err}");
        }
    }

    #[test]
    fn delete_is_the_forward_delete_key_on_every_os() {
        for combo in ["del", "Forward_Delete", "fwddelete"] {
            assert_eq!(
                key_names(combo, Os::Mac).unwrap(),
                ["forward_delete"],
                "{combo}"
            );
            assert_eq!(
                key_names(combo, Os::Windows).unwrap(),
                ["delete"],
                "{combo}"
            );
            assert_eq!(key_names(combo, Os::Linux).unwrap(), ["delete"], "{combo}");
        }
        assert_eq!(
            key_names("shift+Delete", Os::Mac).unwrap(),
            ["shift", "forward_delete"]
        );
        assert_eq!(key_names("backspace", Os::Mac).unwrap(), ["backspace"]);
    }

    #[test]
    fn uia_keys_become_desk_combinations() {
        assert_eq!(uia_combo("Ctrl+Shift+S").as_deref(), Some("ctrl+shift+s"));
        assert_eq!(uia_combo("Alt+F").as_deref(), Some("alt+f"));
        assert_eq!(uia_combo("F5").as_deref(), Some("f5"));
        assert_eq!(uia_combo(" "), None);
        assert_eq!(uia_combo("Ctrl+"), None);
    }

    #[test]
    fn mac_menu_shortcuts_become_desk_combinations() {
        assert_eq!(ax_combo("S", 0).as_deref(), Some("cmd+s"));
        assert_eq!(ax_combo("S", 1).as_deref(), Some("shift+cmd+s"));
        assert_eq!(ax_combo("F", 6).as_deref(), Some("ctrl+alt+cmd+f"));
        assert_eq!(ax_combo("q", 8).as_deref(), Some("q"));
        assert_eq!(ax_combo("", 0), None);
        assert_eq!(ax_combo("\u{f704}", 0), None);
    }

    #[test]
    fn windows_characters_map_to_us_layout_keys() {
        let k = |c| keystroke(c).map(|v| v.join("+"));
        assert_eq!(k('a').as_deref(), Some("a"));
        assert_eq!(k('Q').as_deref(), Some("shift+q"));
        assert_eq!(k(' ').as_deref(), Some("space"));
        assert_eq!(k('!').as_deref(), Some("shift+1"));
        assert_eq!(k(':').as_deref(), Some("shift+;"));
        assert_eq!(k('|').as_deref(), Some("shift+\\"));
        assert_eq!(k('/').as_deref(), Some("/"));
        assert_eq!(k('\\').as_deref(), Some("\\"));
        assert_eq!(k('é'), None);
    }
}
