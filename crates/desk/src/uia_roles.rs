//! UIA control types as ARIA roles after W3C Core-AAM, and UIA properties as desk state names.

use std::collections::{BTreeMap, HashMap, HashSet};

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

pub fn forget_closed<T>(unnamed: &mut HashMap<String, T>, last: &BTreeMap<isize, model::Frame>) {
    unnamed.retain(|id, _| last.values().any(|f| f.id == *id));
}

pub fn popup_entries<K>(
    nodes: &[model::Node],
    root: usize,
    frame: &mut Vec<(String, K)>,
) -> Vec<(String, K)> {
    let depth = nodes[root].depth;
    let inside: HashSet<&str> = nodes[root..]
        .iter()
        .enumerate()
        .take_while(|(i, n)| *i == 0 || n.depth > depth)
        .map(|(_, n)| n.id.as_str())
        .collect();
    let (popup, rest) = std::mem::take(frame)
        .into_iter()
        .partition(|(id, _)| inside.contains(id.as_str()));
    *frame = rest;
    popup
}

pub fn owned_entries<K>(nodes: &[model::Node], frame: &mut Vec<(String, K)>) -> Vec<(String, K)> {
    let mut owned = Vec::new();
    for root in (0..nodes.len()).filter(|&i| nodes[i].role == "menu") {
        owned.extend(popup_entries(nodes, root, frame));
    }
    owned
}

pub fn menu_click_reaches(
    at: Option<isize>,
    own: Option<isize>,
    owners: impl FnOnce(isize) -> Vec<isize>,
) -> bool {
    let (Some(at), Some(own)) = (at, own) else {
        return false;
    };
    at != 0 && own != 0 && (at == own || owners(at).contains(&own))
}

pub fn walked_id<'a, K: PartialEq + 'a>(
    lists: impl IntoIterator<Item = &'a (isize, bool, Vec<(String, K)>)>,
    top: Option<isize>,
    owners: &[isize],
    seen: &K,
    now: impl FnOnce(&str) -> Option<K>,
) -> Option<String> {
    let top = top.filter(|&t| t != 0)?;
    let mut found = None;
    for (id, key) in lists
        .into_iter()
        .filter(|(walked, popups, _)| *walked == top || *popups && owners.contains(walked))
        .flat_map(|(_, _, list)| list)
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
    fn the_walked_elements_of_a_window_no_longer_listed_are_dropped() {
        let frame = |id: &str| model::Frame {
            id: id.into(),
            ..model::Frame::default()
        };
        let last = BTreeMap::from([(7, frame("open"))]);
        let mut unnamed = HashMap::from([("open".to_owned(), 1), ("closed".to_owned(), 2)]);
        forget_closed(&mut unnamed, &last);
        assert_eq!(unnamed, HashMap::from([("open".to_owned(), 1)]));
    }

    #[test]
    fn an_element_without_a_runtime_id_takes_a_walked_id_only_from_its_own_top_level_window() {
        let item = |name: &str, y: i32| (name.to_owned(), [90, y, 296, 13]);
        let lists = [
            (7, false, vec![("u1".to_owned(), item("item 5", 145))]),
            (9, false, vec![("u2".to_owned(), item("Yes", 300))]),
            (
                11,
                false,
                vec![
                    ("u3".to_owned(), item("No", 330)),
                    ("u4".to_owned(), item("No", 330)),
                ],
            ),
        ];
        let found = |top, seen: (String, [i32; 4])| {
            walked_id(&lists, top, &[], &seen, |_| Some(seen.clone()))
        };
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
    fn an_entry_of_a_popup_menu_walked_with_its_frame_resolves_only_under_the_popup() {
        let entry = |name: &str| (name.to_owned(), [100, 60, 120, 22]);
        let frame = [
            (7, false, vec![("u1".to_owned(), entry("View"))]),
            (12, false, vec![("u2".to_owned(), entry("Status Bar"))]),
        ];
        let found = |top, seen: (String, [i32; 4])| {
            walked_id(&frame, top, &[], &seen, |_| Some(seen.clone()))
        };
        assert_eq!(found(Some(12), entry("Status Bar")).as_deref(), Some("u2"));
        assert_eq!(found(Some(7), entry("Status Bar")), None);
        assert_eq!(found(Some(7), entry("View")).as_deref(), Some("u1"));
    }

    #[test]
    fn a_popup_listed_inside_its_frames_tree_takes_the_entries_of_its_own_subtree() {
        let node = |id: &str, depth| model::Node {
            id: id.into(),
            depth,
            ..model::Node::default()
        };
        let nodes = [
            node("w", 0),
            node("u1", 1),
            node("m", 1),
            node("u2", 2),
            node("u3", 3),
            node("u4", 1),
            node("u5", 2),
        ];
        let mut frame: Vec<(String, i32)> = ["u1", "u2", "u3", "u4", "u5"]
            .iter()
            .map(|id| ((*id).to_owned(), 0))
            .collect();
        let popup = popup_entries(&nodes, 2, &mut frame);
        let ids = |l: &[(String, i32)]| l.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&popup), ["u2", "u3"]);
        assert_eq!(ids(&frame), ["u1", "u4", "u5"]);
    }

    #[test]
    fn a_menu_click_goes_out_only_to_the_entrys_own_window_or_a_window_it_owns() {
        let (form, dropdown, submenu, copy_b, copy_b_popup, toast) = (7, 12, 14, 20, 21, 30);
        let owners = |w: isize| match w {
            12 => vec![form],
            14 => vec![dropdown, form],
            21 => vec![copy_b],
            _ => Vec::new(),
        };
        let reaches = |at, own| menu_click_reaches(at, own, owners);
        assert!(reaches(Some(dropdown), Some(dropdown)));
        assert!(reaches(Some(form), Some(form)));
        assert!(reaches(Some(dropdown), Some(form)));
        assert!(reaches(Some(submenu), Some(form)));
        assert!(reaches(Some(submenu), Some(dropdown)));
        assert!(!reaches(Some(copy_b_popup), Some(dropdown)));
        assert!(!reaches(Some(copy_b_popup), Some(form)));
        assert!(!reaches(Some(toast), Some(dropdown)));
        assert!(!reaches(Some(toast), Some(form)));
        assert!(!reaches(Some(form), Some(dropdown)));
        assert!(!reaches(None, Some(dropdown)));
        assert!(!reaches(Some(dropdown), None));
        assert!(!reaches(Some(0), Some(0)));
    }

    #[test]
    fn a_walked_element_that_no_longer_has_its_recorded_type_name_and_bounds_gives_no_id() {
        let row = |y: i32| ("row".to_owned(), [90, y, 296, 13]);
        let lists = [(
            7,
            false,
            vec![("u1".to_owned(), row(93)), ("u2".to_owned(), row(106))],
        )];
        let at = |now: Option<(String, [i32; 4])>| {
            walked_id(&lists, Some(7), &[], &row(106), move |id| {
                assert_eq!(id, "u2");
                now
            })
        };
        assert_eq!(at(Some(row(106))).as_deref(), Some("u2"));
        assert_eq!(at(Some(row(93))), None);
        assert_eq!(at(None), None);
    }

    #[test]
    fn an_entry_of_a_popup_owned_by_a_walked_window_resolves_through_the_owner() {
        let entry = |name: &str, y: i32| (name.to_owned(), [100, y, 120, 22]);
        let lists = [
            (7, false, vec![("u1".to_owned(), entry("Status Bar", 60))]),
            (7, true, vec![("u2".to_owned(), entry("Bravo", 80))]),
            (9, true, vec![("u3".to_owned(), entry("Bravo", 80))]),
            (
                11,
                true,
                vec![
                    ("u4".to_owned(), entry("Alpha", 60)),
                    ("u5".to_owned(), entry("Alpha", 60)),
                ],
            ),
        ];
        let found = |top, owners: &[isize], seen: (String, [i32; 4])| {
            walked_id(&lists, top, owners, &seen, |_| Some(seen.clone()))
        };
        assert_eq!(
            found(Some(12), &[7], entry("Bravo", 80)).as_deref(),
            Some("u2")
        );
        assert_eq!(found(Some(12), &[8], entry("Bravo", 80)), None);
        assert_eq!(found(Some(12), &[], entry("Bravo", 80)), None);
        assert_eq!(found(Some(12), &[7], entry("Status Bar", 60)), None);
        assert_eq!(
            found(Some(7), &[], entry("Bravo", 80)).as_deref(),
            Some("u2")
        );
        assert_eq!(found(Some(12), &[11], entry("Alpha", 60)), None);
    }

    #[test]
    fn the_entries_under_every_menu_node_leave_the_frame_for_its_popup_segment() {
        let node = |id: &str, depth, role| model::Node {
            id: id.into(),
            depth,
            role,
            ..model::Node::default()
        };
        let nodes = [
            node("w", 0, "window"),
            node("u1", 1, "button"),
            node("v", 1, "menu"),
            node("u2", 2, "menuitem"),
            node("d", 1, "menu"),
            node("u3", 2, "menuitem"),
            node("u4", 1, "listitem"),
        ];
        let mut frame: Vec<(String, i32)> = ["u1", "u2", "u3", "u4"]
            .iter()
            .map(|id| ((*id).to_owned(), 0))
            .collect();
        let owned = owned_entries(&nodes, &mut frame);
        let ids = |l: &[(String, i32)]| l.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&owned), ["u2", "u3"]);
        assert_eq!(ids(&frame), ["u1", "u4"]);
    }
}
