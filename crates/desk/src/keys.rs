//! The desk key vocabulary per OS: combinations such as `ctrl+shift+s` in the names cua-driver takes.

const SHIFTED: &str = "!@#$%^&*()_+:\"<>?~{}|";
const BASE: &str = "1234567890-=;',./`[]\\";
const LINUX_ALIASES: [(&str, &str); 47] = [
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
    ("mod1", "alt"),
    ("cmd", "super"),
    ("command", "super"),
    ("meta", "super"),
    ("win", "super"),
    ("windows", "super"),
    ("winleft", "super"),
    ("winright", "super"),
    ("super_l", "super"),
    ("super_r", "super"),
    ("mod4", "super"),
    ("enter", "return"),
    ("kp_enter", "return"),
    ("esc", "escape"),
    ("del", "delete"),
    ("ins", "insert"),
    ("prior", "pageup"),
    ("page_up", "pageup"),
    ("pgup", "pageup"),
    ("next", "pagedown"),
    ("page_down", "pagedown"),
    ("pgdn", "pagedown"),
    ("arrowup", "up"),
    ("arrowdown", "down"),
    ("arrowleft", "left"),
    ("arrowright", "right"),
    ("spacebar", "space"),
    ("caps_lock", "capslock"),
    ("num_lock", "numlock"),
    ("back_space", "backspace"),
    ("bksp", "backspace"),
];

/// The OS whose key names a combination uses.
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

/// The driver's key names of `combo`: `Ctrl+Shift+S` is `["ctrl", "shift", "s"]`.
pub fn key_names(combo: &str, os: Os) -> Result<Vec<String>, String> {
    let mut parts: Vec<&str> = combo.split('+').collect();
    if os == Os::Linux && parts.ends_with(&["", ""]) {
        parts.truncate(parts.len() - 2);
        parts.push("plus");
    }
    let keys: Vec<String> = parts.iter().map(|k| key_name(k, os)).collect();
    if keys.iter().any(String::is_empty) {
        return Err(format!("bad key combination {combo:?}"));
    }
    Ok(keys)
}

/// A UIA AcceleratorKey or AccessKey such as `Ctrl+Shift+S` as `ctrl+shift+s`; none when empty.
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

fn key_name(key: &str, os: Os) -> String {
    let k = key.trim().to_lowercase();
    if os == Os::Linux {
        return LINUX_ALIASES
            .iter()
            .find(|(alias, _)| *alias == k)
            .map_or(k, |(_, name)| (*name).to_owned());
    }
    match k.as_str() {
        "control" | "primary" | "ctl" => "ctrl".to_owned(),
        "enter" => "return".to_owned(),
        "esc" => "escape".to_owned(),
        "del" => "delete".to_owned(),
        "option" | "opt" | "menu" => "alt".to_owned(),
        "pgup" | "page up" | "page_up" | "prior" => "pageup".to_owned(),
        "pgdn" | "page down" | "page_down" | "next" => "pagedown".to_owned(),
        "cmd" | "command" | "meta" | "win" | "windows" | "super" => os.logo().to_owned(),
        _ => k,
    }
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
        assert_eq!(key_names("ctrl++", Os::Linux).unwrap(), ["ctrl", "plus"]);
        assert_eq!(key_names("+", Os::Linux).unwrap(), ["plus"]);
        assert_eq!(key_names("menu", Os::Linux).unwrap(), ["menu"]);
        assert_eq!(key_names("KP_Enter", Os::Linux).unwrap(), ["return"]);
        assert!(key_names("ctrl+", Os::Linux).is_err());
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
