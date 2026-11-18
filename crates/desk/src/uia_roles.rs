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
    pub password: bool,
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
        protected: f.password,
    })
}

pub fn walked_id<'a, K: PartialEq + 'a>(
    lists: impl IntoIterator<Item = &'a (isize, Vec<(String, K)>)>,
    top: Option<isize>,
    seen: &K,
    now: impl FnOnce(&str) -> Option<K>,
) -> Option<String> {
    let top = top.filter(|&t| t != 0)?;
    let mut found = None;
    for (id, key) in lists
        .into_iter()
        .filter(|(walked, _)| *walked == top)
        .flat_map(|(_, list)| list)
    {
        if key == seen {
            if found.is_some() {
                return None;
            }
            found = Some(id);
        }
    }
    found.filter(|id| now(id).as_ref() == Some(seen)).cloned()
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

    #[test]
    fn a_password_box_is_protected() {
        let pw = Facts {
            enabled: true,
            offscreen: true,
            password: true,
            ..Facts::default()
        };
        assert_eq!(states("textbox", pw), ["protected"]);
    }

    #[test]
    fn an_element_without_a_runtime_id_takes_a_walked_id_only_from_its_own_top_level_window() {
        let item = |name: &str, y: i32| (name.to_owned(), [90, y, 296, 13]);
        let lists = [
            (7, vec![("u1".to_owned(), item("item 5", 145))]),
            (9, vec![("u2".to_owned(), item("Yes", 300))]),
            (
                11,
                vec![
                    ("u3".to_owned(), item("No", 330)),
                    ("u4".to_owned(), item("No", 330)),
                ],
            ),
        ];
        let found =
            |top, seen: (String, [i32; 4])| walked_id(&lists, top, &seen, |_| Some(seen.clone()));
        assert_eq!(found(Some(7), item("item 5", 145)).as_deref(), Some("u1"));
        assert_eq!(found(Some(8), item("item 5", 145)), None);
        assert_eq!(found(Some(9), item("item 5", 145)), None);
        assert_eq!(found(Some(9), item("Yes", 300)).as_deref(), Some("u2"));
        assert_eq!(found(Some(7), item("Yes", 300)), None);
        assert_eq!(found(None, item("item 5", 145)), None);
        assert_eq!(found(Some(11), item("No", 330)), None);
        assert_eq!(found(Some(7), item("item 5", 158)), None);
    }

    #[test]
    fn a_walked_element_that_no_longer_has_its_recorded_type_name_and_bounds_gives_no_id() {
        let row = |y: i32| ("row".to_owned(), [90, y, 296, 13]);
        let lists = [(
            7,
            vec![("u1".to_owned(), row(93)), ("u2".to_owned(), row(106))],
        )];
        let at = |now: Option<(String, [i32; 4])>| {
            walked_id(&lists, Some(7), &row(106), move |id| {
                assert_eq!(id, "u2");
                now
            })
        };
        assert_eq!(at(Some(row(106))).as_deref(), Some("u2"));
        assert_eq!(at(Some(row(93))), None);
        assert_eq!(at(None), None);
    }
}
