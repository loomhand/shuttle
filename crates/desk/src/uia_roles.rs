//! UIA control types as ARIA roles after W3C Core-AAM, and UIA properties as desk state names.

use crate::model::{self, States};

/// The UIA facts that decide an element's role.
#[derive(Clone, Copy, Debug, Default)]
pub struct Kind {
    pub control_type: i32,
    pub named: bool,
    pub toggles: bool,
    pub selects: bool,
    pub dialog: bool,
    pub submenu: bool,
}

/// The UIA facts that decide an element's states.
#[derive(Clone, Copy, Debug, Default)]
pub struct Facts {
    pub enabled: bool,
    pub focused: bool,
    pub focusable: bool,
    pub offscreen: bool,
    pub modal: bool,
    pub read_only: Option<bool>,
    pub toggle: Option<i32>,
    pub expand: Option<i32>,
    pub selected: Option<bool>,
}

/// The ARIA role of `k`, none when ARIA has no match; a nameless pane or custom element is generic.
pub fn role(k: Kind) -> Option<&'static str> {
    Some(match k.control_type {
        50000 | 50031 => "button",
        50001 => "grid",
        50002 => "checkbox",
        50003 => "combobox",
        50004 => "textbox",
        50005 => "link",
        50006 => "img",
        50007 => "listitem",
        50008 => "list",
        50009 => "menu",
        50010 => "menubar",
        50011 if k.submenu => "menu",
        50011 if k.toggles => "menuitemcheckbox",
        50011 if k.selects => "menuitemradio",
        50011 => "menuitem",
        50012 => "progressbar",
        50013 => "radio",
        50014 => "scrollbar",
        50015 => "slider",
        50016 => "spinbutton",
        50017 => "status",
        50018 => "tablist",
        50019 => "tab",
        50020 => "text",
        50021 | 50040 => "toolbar",
        50022 => "tooltip",
        50023 => "tree",
        50024 => "treeitem",
        50026 | 50034 => "group",
        50028 | 50036 => "table",
        50029 => "row",
        50030 => "document",
        50032 if k.dialog => "dialog",
        50032 => "window",
        50035 => "columnheader",
        50038 => "separator",
        50025 | 50033 if k.named => "group",
        50025 | 50033 => "generic",
        _ => return None,
    })
}

/// The UIA name of control type `id`.
pub fn native(id: i32) -> &'static str {
    match id {
        50027 => "Thumb",
        50037 => "TitleBar",
        50039 => "SemanticZoom",
        _ => "Unknown",
    }
}

/// The desk states of an element of `role` with `f`.
pub fn states(role: &str, f: Facts) -> Vec<&'static str> {
    let pressable = role == "button";
    model::states(States {
        checked: !pressable && f.toggle == Some(1),
        pressed: pressable && f.toggle == Some(1),
        mixed: f.toggle == Some(2),
        checkable: !pressable && f.toggle.is_some(),
        editable: role == "textbox" && f.read_only == Some(false),
        expanded: f.expand == Some(1),
        focusable: f.focusable,
        focused: f.focused,
        modal: f.modal,
        selected: f.selected == Some(true),
        shown: !f.offscreen,
        read_only: f.read_only == Some(true),
        disabled: !f.enabled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_types_map_to_aria_roles() {
        let k = |control_type: i32| Kind {
            control_type,
            ..Kind::default()
        };
        assert_eq!(role(k(50000)), Some("button"));
        assert_eq!(role(k(50004)), Some("textbox"));
        assert_eq!(role(k(50011)), Some("menuitem"));
        assert_eq!(
            role(Kind {
                toggles: true,
                ..k(50011)
            }),
            Some("menuitemcheckbox")
        );
        assert_eq!(
            role(Kind {
                dialog: true,
                ..k(50032)
            }),
            Some("dialog")
        );
        assert_eq!(role(k(50033)), Some("generic"));
        assert_eq!(
            role(Kind {
                named: true,
                ..k(50033)
            }),
            Some("group")
        );
        assert_eq!(role(k(50027)), None);
        assert_eq!(native(50027), "Thumb");
    }

    #[test]
    fn toggle_states_become_checked_pressed_or_mixed() {
        let on = Facts {
            enabled: true,
            toggle: Some(1),
            ..Facts::default()
        };
        assert_eq!(states("checkbox", on), ["checked", "showing", "checkable"]);
        assert_eq!(states("button", on), ["pressed", "showing"]);
        let mixed = Facts {
            toggle: Some(2),
            offscreen: true,
            ..on
        };
        assert_eq!(states("checkbox", mixed), ["checkable", "mixed"]);
        let off = Facts {
            enabled: false,
            read_only: Some(false),
            ..Facts::default()
        };
        assert_eq!(states("textbox", off), ["editable", "showing", "disabled"]);
    }
}
