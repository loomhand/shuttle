//! The desk key vocabulary per OS: combinations such as `ctrl+shift+s` in the names cua-driver takes.

/// The OS whose key names a combination uses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Os {
    Windows,
    Mac,
}

impl Os {
    /// The OS this binary runs on; Windows when it is neither.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else {
            Self::Windows
        }
    }

    fn logo(self) -> &'static str {
        match self {
            Self::Windows => "win",
            Self::Mac => "cmd",
        }
    }
}

/// The driver's key names of `combo`: `Ctrl+Shift+S` is `["ctrl", "shift", "s"]`.
pub fn key_names(combo: &str, os: Os) -> Result<Vec<String>, String> {
    let keys: Vec<String> = combo.split('+').map(|k| key_name(k, os)).collect();
    if keys.iter().any(String::is_empty) {
        return Err(format!("bad key combination {combo:?}"));
    }
    Ok(keys)
}

/// A UIA AcceleratorKey or AccessKey such as `Ctrl+Shift+S` as `ctrl+shift+s`; none when empty.
pub fn uia_combo(text: &str, os: Os) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    key_names(text, os).ok().map(|k| k.join("+"))
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

fn key_name(key: &str, os: Os) -> String {
    let k = key.trim().to_lowercase();
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
    fn uia_keys_become_desk_combinations() {
        assert_eq!(
            uia_combo("Ctrl+Shift+S", Os::Windows).as_deref(),
            Some("ctrl+shift+s")
        );
        assert_eq!(uia_combo("Alt+F", Os::Windows).as_deref(), Some("alt+f"));
        assert_eq!(uia_combo("F5", Os::Windows).as_deref(), Some("f5"));
        assert_eq!(uia_combo(" ", Os::Windows), None);
        assert_eq!(uia_combo("Ctrl+", Os::Windows), None);
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
}
