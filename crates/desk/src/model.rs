//! The platform-neutral desk model: windows, element trees and the accessibility calls each OS backend serves.

use std::collections::HashMap;
use std::time::Duration;

use desk_core::text::Named;
use desk_core::window::Window;
use serde::Serialize;
use serde_json::{Value, json};

pub const MAX_NODES: usize = 2500;
pub const MAX_DEPTH: usize = 64;
pub const MAX_IDS: usize = 50_000;
pub const VALUE_CHARS: usize = 200;

const ACTIVE_ROLES: [&str; 20] = [
    "button",
    "checkbox",
    "menuitem",
    "menu",
    "tab",
    "textbox",
    "spinbutton",
    "slider",
    "combobox",
    "radio",
    "link",
    "option",
    "listitem",
    "treeitem",
    "menuitemcheckbox",
    "menuitemradio",
    "cell",
    "switch",
    "row",
    "searchbox",
];
const FIELD_ROLES: [&str; 6] = [
    "textbox",
    "searchbox",
    "spinbutton",
    "slider",
    "combobox",
    "progressbar",
];

/// A top-level window of an app.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Frame {
    pub id: String,
    pub title: String,
    pub role: &'static str,
    pub pid: Option<u32>,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// The frame that owns this one: the window of a macOS sheet, the owner window of a Windows dialog.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Where a Linux modal dialog is that the screen does not show: `hidden`, `off the screen` or `behind "TITLE"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modal: Option<String>,
    /// The title of the window such a dialog blocks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks: Option<String>,
    /// A window of the desktop shell's processes on Linux: a panel, a dock or the desktop.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub shell: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space: Option<&'static str>,
    #[serde(skip)]
    pub stale: bool,
    /// A Linux top-level object that is no frame, dialog or alert, which no call picks.
    #[serde(skip)]
    pub unlisted: bool,
    /// A Linux frame, which stands for the focused window even untitled, unlike a plain window.
    #[serde(skip)]
    pub framed: bool,
    /// A macOS window that no display shows: minimized, or hidden by its app.
    #[serde(skip)]
    pub hidden: bool,
    /// The window server's number of a macOS window, which cua-driver lists it by.
    #[serde(skip)]
    pub number: Option<u32>,
}

impl Window for Frame {
    fn title(&self) -> &str {
        &self.title
    }

    fn active(&self) -> bool {
        self.active
    }

    fn pid(&self) -> Option<u32> {
        self.pid
    }

    fn listed(&self) -> bool {
        !self.unlisted
    }

    fn focusable(&self) -> bool {
        self.framed || self.role != "window" || !self.title.is_empty()
    }

    fn shown(&self) -> bool {
        !self.hidden
    }
}

/// One element of a snapshot, in preorder; `parent` indexes the nearest listed ancestor.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Node {
    pub id: String,
    pub parent: Option<usize>,
    pub depth: usize,
    pub role: &'static str,
    pub native_role: Option<String>,
    pub name: String,
    pub description: String,
    pub states: Vec<&'static str>,
    pub value: Option<String>,
    pub shortcut: Option<String>,
    pub mnemonic: Option<String>,
    /// The raw key binding of a Linux element, `mnemonic;sequence;accelerator`.
    pub keys: Option<String>,
    pub bounds: Option<[i32; 4]>,
}

impl Node {
    pub fn has(&self, state: &str) -> bool {
        self.states.contains(&state)
    }

    /// The node as desk JSON, `i` its ref.
    pub fn json(&self, i: usize) -> Value {
        let mut v = json!({
            "ref": i,
            "id": self.id,
            "parent": self.parent.map_or(-1, |p| p as i64),
            "depth": self.depth,
            "role": self.role,
            "name": self.name,
            "states": self.states,
        });
        let optional = [
            ("native_role", self.native_role.as_deref()),
            (
                "description",
                Some(self.description.as_str()).filter(|d| !d.is_empty()),
            ),
            ("value", self.value.as_deref()),
            ("shortcut", self.shortcut.as_deref()),
            ("mnemonic", self.mnemonic.as_deref()),
            ("keys", self.keys.as_deref()),
        ];
        for (key, field) in optional {
            if let Some(field) = field {
                v[key] = field.into();
            }
        }
        if let Some(b) = self.bounds {
            v["bounds"] = json!(b);
        }
        v
    }
}

impl Named for Node {
    fn name(&self) -> &str {
        &self.name
    }

    fn role(&self) -> &str {
        self.role
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct States {
    pub checked: bool,
    pub pressed: bool,
    pub mixed: bool,
    pub checkable: bool,
    pub editable: bool,
    pub expanded: bool,
    pub focusable: bool,
    pub focused: bool,
    pub modal: bool,
    pub selected: bool,
    pub shown: bool,
    pub read_only: bool,
    pub disabled: bool,
}

/// The tree of one window with the time each phase took.
#[derive(Debug, Default)]
pub struct Tree {
    pub frame: Frame,
    pub nodes: Vec<Node>,
    pub truncated: bool,
    pub phases: Vec<(&'static str, u128)>,
}

/// What a snapshot or a read also fetches beyond role, name and states.
#[derive(Clone, Copy, Debug, Default)]
pub struct Extras {
    pub values: bool,
    pub bounds: bool,
    pub keys: bool,
    pub menus: bool,
    pub frame: bool,
}

impl Extras {
    pub const BOUNDS: Self = Self {
        values: false,
        bounds: true,
        keys: false,
        menus: false,
        frame: false,
    };
    pub const FRAME: Self = Self {
        frame: true,
        ..Self::BOUNDS
    };
}

/// A screen rectangle `[x, y, w, h]` in whole pixels and its RGB rows.
#[derive(Debug, Default)]
pub struct Capture {
    pub rect: [u32; 4],
    pub rgb: Vec<u8>,
}

/// A modal dialog the screen does not show, and the window it blocks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Modal {
    pub title: String,
    pub pid: Option<u32>,
    /// The title of the window it blocks, which it is transient for.
    pub owner: String,
    /// `hidden`, `off the screen` or `behind "TITLE"`.
    pub place: String,
}

/// An element action that is no pointer click.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Act {
    /// The element's default action: invoke, toggle, open a menu.
    Action,
    Select,
    Focus,
}

/// The accessibility calls of one OS; they block, and the host runs them on a worker thread.
pub trait Access {
    /// The top-level windows of every app.
    fn frames(&mut self) -> Result<Vec<Frame>, String>;
    /// The top-level windows of the app with process `pid`.
    fn frames_of(&mut self, pid: u32) -> Result<Vec<Frame>, String> {
        Ok(self
            .frames()?
            .into_iter()
            .filter(|f| f.pid == Some(pid))
            .collect())
    }
    fn off_space(&mut self) -> Vec<Frame> {
        Vec::new()
    }
    /// A cheap fingerprint of the windows on screen that changes when windows open, close or retitle; `None` when the backend has none.
    fn hint(&mut self) -> Option<u64> {
        None
    }
    /// The listed elements of `frame` in preorder.
    fn tree(&mut self, frame: &Frame, extras: Extras) -> Result<Tree, String>;
    /// One element read live; a value has at most `chars` characters.
    fn node(&mut self, id: &str, extras: Extras, chars: usize) -> Result<Node, String>;
    /// The id of the focused element of `frame`, if one is; a backend that cannot answer `within` gives none.
    fn focused(&mut self, frame: &Frame, _within: Duration) -> Result<Option<String>, String> {
        focused_by_tree(self, frame)
    }
    /// The element with the keyboard focus in the app of process `pid`, with its screen bounds; none when it has none or the backend cannot tell.
    fn keyboard_focus(&mut self, _pid: u32) -> Result<Option<Node>, String> {
        Ok(None)
    }
    /// The ids above `id`, from its top-level window down to its parent.
    fn ancestors(&mut self, id: &str) -> Result<Vec<String>, String>;
    /// The deepest element of the window `window` under the screen pixel `(x, y)`.
    fn at(&mut self, window: &str, x: i32, y: i32) -> Result<Option<String>, String>;
    /// The open menu item under the screen pixel `(x, y)`, above every window.
    fn menu_at(&mut self, _x: i32, _y: i32) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn pid_at(&mut self, _x: i32, _y: i32) -> Result<Option<u32>, String> {
        Ok(None)
    }
    /// The process of the topmost window at the screen pixel `(x, y)` on any window level; an error when the hit test fails, unlike [`Self::pid_at`].
    fn owner_at(&mut self, x: i32, y: i32) -> Result<Option<u32>, String> {
        self.pid_at(x, y)
    }
    /// The process of the topmost window the window server lists on screen at the screen pixel `(x, y)`, for when [`Self::owner_at`] fails.
    fn listed_owner_at(&mut self, _x: i32, _y: i32) -> Result<Option<u32>, String> {
        Ok(None)
    }
    /// The title and process of the window over `frame` at the screen pixel `(x, y)`; none when `frame` is on top there or the backend cannot tell.
    fn covering(
        &mut self,
        _frame: &Frame,
        _x: i32,
        _y: i32,
    ) -> Result<Option<(String, Option<u32>)>, String> {
        Ok(None)
    }
    /// The process of the element `id`, none when the backend cannot tell.
    fn pid_of(&mut self, _id: &str) -> Result<Option<u32>, String> {
        Ok(None)
    }
    /// Runs `act` on the element `id`.
    fn act(&mut self, id: &str, act: Act) -> Result<bool, String>;
    /// Asks the window to close as its close button does.
    fn close(&mut self, frame: &Frame) -> Result<(), String>;
    /// The screen rectangle `[x, y, w, h]` clipped to the screen, read by the backend itself; none when the driver captures, as it does while its cursor overlay shows, which the backend looks for only when `overlay` says the driver may show one.
    fn capture(&mut self, _rect: [f64; 4], _overlay: bool) -> Result<Option<Capture>, String> {
        Ok(None)
    }
    /// The modal dialogs the screen does not show.
    fn modals(&mut self) -> Vec<Modal> {
        Vec::new()
    }
    /// The pids of the desktop shell's processes: those that own a dock or desktop window.
    fn shells(&mut self) -> Vec<u32> {
        Vec::new()
    }
    /// The window the window manager names active when no frame of its process is listed, as an app that stopped answering leaves it.
    fn unanswered(&mut self) -> Option<Frame> {
        None
    }
}

/// Whether a caller can act on a node of `role`.
pub fn is_active(role: &str) -> bool {
    ACTIVE_ROLES.contains(&role)
}

/// Whether a node of `role` holds a value a caller reads.
pub fn is_field(role: &str) -> bool {
    FIELD_ROLES.contains(&role)
}

pub fn states(s: States) -> Vec<&'static str> {
    [
        (s.checked, "checked"),
        (s.pressed, "pressed"),
        (s.editable, "editable"),
        (s.expanded, "expanded"),
        (s.focusable, "focusable"),
        (s.focused, "focused"),
        (s.modal, "modal"),
        (s.selected, "selected"),
        (s.shown, "showing"),
        (s.checkable, "checkable"),
        (s.read_only, "read-only"),
        (s.disabled, "disabled"),
        (s.mixed, "mixed"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect()
}

pub fn format_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        (v as i64).to_string()
    } else {
        v.to_string()
    }
}

pub fn place(
    out: &mut Vec<Node>,
    mut node: Node,
    parent: Option<usize>,
    depth: usize,
) -> (Option<usize>, usize) {
    if !is_field(node.role) {
        node.value = None;
    }
    if node.role == "generic" && parent.is_some() {
        return (parent, depth);
    }
    node.parent = parent;
    node.depth = depth;
    out.push(node);
    (Some(out.len() - 1), depth + 1)
}

pub fn lookup<T: Clone>(ids: &HashMap<String, T>, id: &str) -> Result<T, String> {
    ids.get(id)
        .cloned()
        .ok_or_else(|| format!("unknown element id {id:?}: take a new snapshot"))
}

/// The id of the first element of `frame`'s tree in the focused state, from one whole tree read.
pub fn focused_by_tree<B: Access + ?Sized>(
    b: &mut B,
    frame: &Frame,
) -> Result<Option<String>, String> {
    Ok(b.tree(frame, Extras::default())?
        .nodes
        .into_iter()
        .find(|n| n.has("focused"))
        .map(|n| n.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Focus {
        matched: Option<String>,
        trees: usize,
    }

    impl Access for Focus {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            Ok(Vec::new())
        }

        fn tree(&mut self, frame: &Frame, _: Extras) -> Result<Tree, String> {
            self.trees += 1;
            let node = |id: &str, states: Vec<&'static str>| Node {
                id: id.into(),
                states,
                ..Node::default()
            };
            Ok(Tree {
                frame: frame.clone(),
                nodes: vec![node("w", vec!["active"]), node("field", vec!["focused"])],
                ..Tree::default()
            })
        }

        fn node(&mut self, _: &str, _: Extras, _: usize) -> Result<Node, String> {
            Err("unused".into())
        }

        fn focused(&mut self, frame: &Frame, _: Duration) -> Result<Option<String>, String> {
            match self.matched.clone() {
                Some(id) => Ok(Some(id)),
                None => focused_by_tree(self, frame),
            }
        }

        fn ancestors(&mut self, _: &str) -> Result<Vec<String>, String> {
            Err("unused".into())
        }

        fn at(&mut self, _: &str, _: i32, _: i32) -> Result<Option<String>, String> {
            Err("unused".into())
        }

        fn act(&mut self, _: &str, _: Act) -> Result<bool, String> {
            Err("unused".into())
        }

        fn close(&mut self, _: &Frame) -> Result<(), String> {
            Err("unused".into())
        }
    }

    #[test]
    fn a_match_names_the_focused_element_without_a_tree_read_and_no_match_falls_back_to_one() {
        let frame = Frame::default();
        let mut matched = Focus {
            matched: Some("field".into()),
            trees: 0,
        };
        assert_eq!(
            matched.focused(&frame, Duration::from_millis(100)),
            Ok(Some("field".into()))
        );
        assert_eq!(matched.trees, 0);
        let mut unmatched = Focus {
            matched: None,
            trees: 0,
        };
        assert_eq!(
            unmatched.focused(&frame, Duration::from_millis(100)),
            Ok(Some("field".into()))
        );
        assert_eq!(unmatched.trees, 1);
    }

    #[test]
    fn node_json_leaves_out_empty_fields() {
        let n = Node {
            id: "1/2".into(),
            parent: Some(0),
            depth: 1,
            role: "checkbox",
            name: "Wrap".into(),
            states: vec!["checked", "showing"],
            shortcut: Some("ctrl+w".into()),
            ..Node::default()
        };
        assert_eq!(
            n.json(3),
            json!({"ref": 3, "id": "1/2", "parent": 0, "depth": 1, "role": "checkbox", "name": "Wrap",
                "states": ["checked", "showing"], "shortcut": "ctrl+w"})
        );
    }

    #[test]
    fn a_whole_number_prints_without_decimals_and_zero_without_sign() {
        assert_eq!(format_number(-0.0), "0");
        assert_eq!(format_number(3.0), "3");
        assert_eq!(format_number(-2.0), "-2");
        assert_eq!(format_number(2.5), "2.5");
    }
}
