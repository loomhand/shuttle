//! The platform-neutral desk model: windows, element trees and the accessibility calls each OS backend serves.

use desk_core::text::Named;
use desk_core::window::Window;
use serde_json::{Value, json};

/// The ARIA roles a caller can act on.
pub const ACTIVE_ROLES: [&str; 20] = [
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
/// The ARIA roles whose value a caller reads.
pub const FIELD_ROLES: [&str; 6] = [
    "textbox",
    "searchbox",
    "spinbutton",
    "slider",
    "combobox",
    "progressbar",
];

/// A top-level window of an app.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub id: String,
    pub title: String,
    pub role: &'static str,
    pub pid: Option<u32>,
    pub active: bool,
}

impl Frame {
    /// The window as desk JSON.
    pub fn json(&self) -> Value {
        json!({
            "id": self.id,
            "title": self.title,
            "role": self.role,
            "pid": self.pid,
            "active": self.active,
        })
    }
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

    fn focusable(&self) -> bool {
        self.role != "window" || !self.title.is_empty()
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
    /// The listed elements of `frame` in preorder.
    fn tree(&mut self, frame: &Frame, extras: Extras) -> Result<Tree, String>;
    /// One element read live; a value has at most `chars` characters.
    fn node(&mut self, id: &str, extras: Extras, chars: usize) -> Result<Node, String>;
    /// The ids above `id`, from its top-level window down to its parent.
    fn ancestors(&mut self, id: &str) -> Result<Vec<String>, String>;
    /// The deepest element of the window `window` under the screen pixel `(x, y)`.
    fn at(&mut self, window: &str, x: i32, y: i32) -> Result<Option<String>, String>;
    /// Runs `act` on the element `id`.
    fn act(&mut self, id: &str, act: Act) -> Result<bool, String>;
    /// Asks the window to close as its close button does.
    fn close(&mut self, frame: &Frame) -> Result<(), String>;
}

/// Whether a caller can act on a node of `role`.
pub fn is_active(role: &str) -> bool {
    ACTIVE_ROLES.contains(&role)
}

/// Whether a node of `role` holds a value a caller reads.
pub fn is_field(role: &str) -> bool {
    FIELD_ROLES.contains(&role)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
