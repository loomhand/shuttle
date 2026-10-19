//! AT-SPI over D-Bus: a window's tree from its app's `Cache.GetItems`, filled in by live calls
//! where the cache lacks children or their order, plus live reads and actions on one node.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;
use zbus::Connection;
use zbus::zvariant::{DynamicType, OwnedObjectPath, OwnedValue};

use crate::model::{MAX_DEPTH, MAX_NODES, VALUE_CHARS, format_number, is_active, is_field};

const BUS_NAME: &str = "org.a11y.Bus";
const BUS_PATH: &str = "/org/a11y/bus";
const REGISTRY: &str = "org.a11y.atspi.Registry";
const ROOT: &str = "/org/a11y/atspi/accessible/root";
const PATH_PREFIX: &str = "/org/a11y/atspi/accessible";
const CACHE_PATH: &str = "/org/a11y/atspi/cache";
const CACHE: &str = "org.a11y.atspi.Cache";
const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const ACTION: &str = "org.a11y.atspi.Action";
const COMPONENT: &str = "org.a11y.atspi.Component";
const TEXT: &str = "org.a11y.atspi.Text";
const COLLECTION: &str = "org.a11y.atspi.Collection";
const MATCH_ALL: i32 = 1;
const SORT_CANONICAL: u32 = 1;
const VALUE: &str = "org.a11y.atspi.Value";
const SELECTION: &str = "org.a11y.atspi.Selection";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const CALL_TIMEOUT: Duration = Duration::from_secs(3);
const HUNG_FOR: Duration = Duration::from_secs(10);
const APP_FRAMES: Duration = Duration::from_millis(500);
const MANAGED_KIDS: i32 = 200;
const LIST_ROLES: [u32; 4] = [31, 55, 65, 66];
const SCREEN: u32 = 0;
const IF_ACTION: u8 = 1;
const IF_COMPONENT: u8 = 2;
const IF_TEXT: u8 = 4;
const IF_VALUE: u8 = 8;
const ACTIVE: u32 = 1;
const CHECKED: u32 = 4;
const ENABLED: u32 = 8;
const INDETERMINATE: u32 = 32;
/// The state of the node that holds the keyboard focus.
pub const FOCUSED: u32 = 12;
/// The state of a node drawn on the screen.
pub const SHOWING: u32 = 25;
const MANAGES_DESCENDANTS: u32 = 31;
const LABELLED_BY: u32 = 2;
const STATE_NAMES: [(u32, &str); 16] = [
    (1, "active"),
    (4, "checked"),
    (7, "editable"),
    (10, "expanded"),
    (11, "focusable"),
    (12, "focused"),
    (16, "modal"),
    (17, "multi-line"),
    (20, "pressed"),
    (23, "selected"),
    (25, "showing"),
    (30, "visible"),
    (32, "indeterminate"),
    (36, "invalid"),
    (41, "checkable"),
    (43, "read-only"),
];
const ROLE_FILLER: u32 = 20;
const ROLE_PANEL: u32 = 39;
const ROLE_DOCUMENT_WEB: u32 = 95;
const GENERIC: [u32; 7] = [ROLE_FILLER, 24, 25, 30, ROLE_PANEL, 46, 85];
// AT-SPI role numbers, in the order of AtspiRole
const ROLE_NAMES: [&str; 130] = [
    "invalid",
    "accelerator label",
    "alert",
    "animation",
    "arrow",
    "calendar",
    "canvas",
    "check box",
    "check menu item",
    "color chooser",
    "column header",
    "combo box",
    "date editor",
    "desktop icon",
    "desktop frame",
    "dial",
    "dialog",
    "directory pane",
    "drawing area",
    "file chooser",
    "filler",
    "focus traversable",
    "font chooser",
    "frame",
    "glass pane",
    "html container",
    "icon",
    "image",
    "internal frame",
    "label",
    "layered pane",
    "list",
    "list item",
    "menu",
    "menu bar",
    "menu item",
    "option pane",
    "page tab",
    "page tab list",
    "panel",
    "password text",
    "popup menu",
    "progress bar",
    "push button",
    "radio button",
    "radio menu item",
    "root pane",
    "row header",
    "scroll bar",
    "scroll pane",
    "separator",
    "slider",
    "spin button",
    "split pane",
    "statusbar",
    "table",
    "table cell",
    "table column header",
    "table row header",
    "tearoff menu item",
    "terminal",
    "text",
    "toggle button",
    "tool bar",
    "tool tip",
    "tree",
    "tree table",
    "unknown",
    "viewport",
    "window",
    "extended",
    "header",
    "footer",
    "paragraph",
    "ruler",
    "application",
    "autocomplete",
    "editbar",
    "embedded",
    "entry",
    "chart",
    "caption",
    "document frame",
    "heading",
    "page",
    "section",
    "redundant object",
    "form",
    "link",
    "input method window",
    "table row",
    "tree item",
    "document spreadsheet",
    "document presentation",
    "document text",
    "document web",
    "document email",
    "comment",
    "list box",
    "grouping",
    "image map",
    "notification",
    "info bar",
    "level bar",
    "title bar",
    "block quote",
    "audio",
    "video",
    "definition",
    "article",
    "landmark",
    "log",
    "marquee",
    "math",
    "rating",
    "timer",
    "static",
    "math fraction",
    "math root",
    "subscript",
    "superscript",
    "description list",
    "description term",
    "description value",
    "footnote",
    "content deletion",
    "content insertion",
    "mark",
    "suggestion",
    "push button menu",
];

/// An accessible object: the bus name of its app and its object path.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Obj {
    pub bus: String,
    pub path: String,
}

impl Obj {
    /// A short printable id: the bus name without its colon, then the path without the accessible prefix.
    pub fn id(&self) -> String {
        let bus = self.bus.strip_prefix(':').unwrap_or(&self.bus);
        let path = self.path.strip_prefix(PATH_PREFIX).unwrap_or(&self.path);
        format!("{bus}{path}")
    }

    /// The object an [`Obj::id`] names.
    pub fn parse(id: &str) -> Result<Self, String> {
        let (bus, path) = id
            .split_once('/')
            .ok_or_else(|| format!("bad element id {id:?}"))?;
        let bus = if bus.starts_with(|c: char| c.is_ascii_digit()) {
            format!(":{bus}")
        } else {
            bus.to_owned()
        };
        let path = if path.starts_with("org/") {
            format!("/{path}")
        } else {
            format!("{PATH_PREFIX}/{path}")
        };
        Ok(Self { bus, path })
    }

    fn of((bus, path): (String, OwnedObjectPath)) -> Self {
        Self {
            bus,
            path: path.as_str().to_owned(),
        }
    }
}

/// One node of a snapshot, in preorder; `parent` indexes the nearest shown ancestor.
#[derive(Clone, Debug, Default)]
pub struct Node {
    pub obj: Option<Obj>,
    pub role: u32,
    pub name: String,
    pub description: String,
    pub states: u64,
    pub ifaces: u8,
    pub parent: Option<usize>,
    pub depth: usize,
    pub value: Option<String>,
    pub keys: Option<String>,
    pub bounds: Option<[i32; 4]>,
}

impl Node {
    /// The node's role in ARIA terms after W3C Core-AAM, else its AT-SPI name.
    pub fn role(&self) -> &'static str {
        role_name(
            self.role,
            !self.name.is_empty() || !self.description.is_empty(),
        )
    }

    pub fn has(&self, state: u32) -> bool {
        self.states & (1 << state) != 0
    }

    pub fn checked(&self) -> bool {
        self.has(CHECKED)
    }

    /// Whether the node is greyed out.
    pub fn disabled(&self) -> bool {
        !self.has(ENABLED)
    }

    /// The names of the node's states that matter to a caller.
    pub fn state_names(&self) -> Vec<&'static str> {
        let mut names: Vec<&str> = STATE_NAMES
            .iter()
            .filter(|(bit, _)| self.has(*bit))
            .map(|(_, n)| *n)
            .collect();
        if self.disabled() {
            names.push("disabled");
        }
        if self.has(INDETERMINATE) && matches!(self.role(), "checkbox" | "menuitemcheckbox") {
            names.push("mixed");
        }
        names
    }

    /// The node's AT-SPI role name when its role has no ARIA counterpart.
    pub fn native_role(&self) -> Option<&'static str> {
        aria_role(self.role, false)
            .is_none()
            .then(|| native_role(self.role))
    }
}

/// A top-level accessible window: a frame, dialog or alert of an app.
#[derive(Clone, Debug)]
pub struct Frame {
    pub obj: Obj,
    pub role: u32,
    pub name: String,
    pub states: u64,
    pub pid: Option<u32>,
}

impl Frame {
    pub fn active(&self) -> bool {
        self.states & (1 << ACTIVE) != 0
    }
}

/// What a snapshot also reads live, beyond the cached tree.
#[derive(Clone, Copy, Debug, Default)]
pub struct Extras {
    pub values: bool,
    pub bounds: bool,
    pub keys: bool,
}

/// The tree of one window with the time each phase took.
pub struct Tree {
    pub frame: Frame,
    pub nodes: Vec<Node>,
    pub truncated: bool,
    pub phases: Vec<(&'static str, u128)>,
}

/// Raw data of one object, from the cache or from live calls.
#[derive(Clone, Debug, Default)]
struct Raw {
    role: u32,
    name: String,
    description: String,
    states: u64,
    count: i32,
    ifaces: u8,
}

type Item = (
    (String, OwnedObjectPath),
    (String, OwnedObjectPath),
    (String, OwnedObjectPath),
    i32,
    i32,
    Vec<String>,
    String,
    u32,
    String,
    Vec<u32>,
);

type OldItem = (
    (String, OwnedObjectPath),
    (String, OwnedObjectPath),
    (String, OwnedObjectPath),
    Vec<(String, OwnedObjectPath)>,
    Vec<String>,
    String,
    u32,
    String,
    Vec<u32>,
);

/// The apps that stopped answering, by bus name, with when; shared by every client, so a restarted worker keeps skipping them.
static HUNG: LazyLock<std::sync::Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// A lazily opened connection to the accessibility bus, shared by all calls.
pub struct Atspi {
    conn: Mutex<Option<Connection>>,
}

impl Default for Atspi {
    fn default() -> Self {
        Self::new()
    }
}

impl Atspi {
    pub fn new() -> Self {
        Self {
            conn: Mutex::new(None),
        }
    }

    /// The accessible top-level windows of every app, with their pids; an app that takes over [`APP_FRAMES`] is left out and skipped for a while.
    pub async fn frames(&self) -> Result<Vec<Frame>, String> {
        let registry = Obj {
            bus: REGISTRY.to_owned(),
            path: ROOT.to_owned(),
        };
        let apps = self.children(&registry).await?;
        let per_app = join_all(
            apps.iter()
                .map(|app| async move { capped(&app.bus, self.app_frames(app)).await }),
        )
        .await;
        Ok(per_app.into_iter().flatten().collect())
    }

    async fn app_frames(&self, app: &Obj) -> Vec<Frame> {
        let (kids, pid) = tokio::join!(self.children(app), self.pid(&app.bus));
        let Ok(kids) = kids else {
            return Vec::new();
        };
        let pid = pid.ok();
        join_all(kids.into_iter().map(|obj| async move {
            let (role, name, states) =
                tokio::join!(self.role(&obj), self.name(&obj), self.state(&obj));
            Some(Frame {
                role: role.ok()?,
                name: name.unwrap_or_default(),
                states: states.ok()?,
                obj,
                pid,
            })
        }))
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
    }

    /// The tree under `frame`: shown nodes in preorder, with `extras` read live.
    pub async fn tree(&self, frame: Frame, extras: Extras) -> Result<Tree, String> {
        let mut phases = Vec::new();
        let mut clock = Instant::now();
        let mut lap = |name: &'static str, phases: &mut Vec<(&'static str, u128)>| {
            phases.push((name, clock.elapsed().as_micros()));
            clock = Instant::now();
        };
        let items = self.items(&frame.obj.bus).await.unwrap_or_default();
        lap("items", &mut phases);
        let mut raw: HashMap<Obj, Raw> = HashMap::with_capacity(items.len());
        let mut cached: HashMap<Obj, Vec<(i32, Obj)>> = HashMap::new();
        for (object, _, parent, index, count, ifaces, name, role, description, states) in items {
            let (object, parent) = (Obj::of(object), Obj::of(parent));
            cached
                .entry(parent)
                .or_default()
                .push((index, object.clone()));
            raw.insert(
                object,
                Raw {
                    role,
                    name,
                    description,
                    states: state_bits(&states),
                    count,
                    ifaces: iface_bits(&ifaces),
                },
            );
        }
        lap("parse", &mut phases);
        if !raw.contains_key(&frame.obj) {
            let r = self.raw(&frame.obj).await?;
            raw.insert(frame.obj.clone(), r);
        }
        let mut kids: HashMap<Obj, Vec<Obj>> = HashMap::new();
        let mut frontier = vec![frame.obj.clone()];
        let mut seen: HashSet<Obj> = HashSet::from([frame.obj.clone()]);
        let mut truncated = false;
        let mut by_index: HashSet<Obj> = HashSet::new();
        while !frontier.is_empty() {
            let uncounted: Vec<&Obj> = frontier
                .iter()
                .filter(|o| {
                    let r = &raw[*o];
                    LIST_ROLES.contains(&r.role)
                        && r.count <= 0
                        && cached.get(*o).is_none_or(Vec::is_empty)
                })
                .collect();
            let counts = join_all(
                uncounted
                    .iter()
                    .map(|o| self.prop::<i32>(o, ACCESSIBLE, "ChildCount")),
            )
            .await;
            for (o, count) in uncounted.into_iter().zip(counts) {
                if let Some(count) = count.ok().filter(|c| (1..=MANAGED_KIDS).contains(c)) {
                    if let Some(r) = raw.get_mut(o) {
                        r.count = count;
                    }
                    by_index.insert(o.clone());
                }
            }
            let live: Vec<&Obj> = frontier
                .iter()
                .filter(|o| needs_live(&raw[*o], cached.get(*o)))
                .collect();
            let indexed = &by_index;
            let lists = join_all(live.iter().map(|o| {
                let r = &raw[*o];
                async move {
                    if r.states & (1 << MANAGES_DESCENDANTS) == 0 && !indexed.contains(*o) {
                        return self.children(o).await;
                    }
                    join_all((0..r.count).map(|i| self.child_at(o, i)))
                        .await
                        .into_iter()
                        .collect()
                }
            }))
            .await;
            let mut listed: HashMap<Obj, Vec<Obj>> = HashMap::new();
            for (o, list) in live.iter().zip(lists) {
                if let Ok(list) = list {
                    listed.insert((*o).clone(), list);
                }
            }
            let mut queued: HashSet<&Obj> = HashSet::new();
            let mut unknown: Vec<Obj> = live
                .iter()
                .filter_map(|o| listed.get(*o))
                .flatten()
                .filter(|c| !raw.contains_key(*c) && queued.insert(*c))
                .cloned()
                .collect();
            let budget = MAX_NODES.saturating_sub(seen.len());
            if unknown.len() > budget {
                truncated = true;
                unknown.truncate(budget);
            }
            let fetched = join_all(unknown.iter().map(|o| self.raw(o))).await;
            for (o, r) in unknown.into_iter().zip(fetched) {
                if let Ok(r) = r {
                    raw.insert(o, r);
                }
            }
            let mut next = Vec::new();
            for o in frontier {
                let mut sorted = cached.remove(&o).unwrap_or_default();
                sorted.sort_by_key(|(i, _)| *i);
                let sorted = sorted.into_iter().map(|(_, c)| c);
                let list: Vec<Obj> = match listed.remove(&o) {
                    Some(mut list) => {
                        let extra: Vec<Obj> = sorted.filter(|c| !list.contains(c)).collect();
                        list.extend(extra);
                        list
                    }
                    None => sorted.collect(),
                };
                let list: Vec<Obj> = list.into_iter().filter(|c| raw.contains_key(c)).collect();
                next.extend(list.iter().filter(|c| seen.insert((*c).clone())).cloned());
                kids.insert(o, list);
                if seen.len() > MAX_NODES {
                    truncated = true;
                    break;
                }
            }
            frontier = next;
        }
        lap("live", &mut phases);
        let mut nodes = Vec::new();
        flatten(&frame.obj, &raw, &kids, None, &mut Vec::new(), &mut nodes);
        truncated |= nodes.len() >= MAX_NODES;
        let mut tree = Tree {
            frame,
            nodes,
            truncated,
            phases,
        };
        self.extras(&mut tree.nodes, extras).await;
        tree.phases.push(("extras", clock.elapsed().as_micros()));
        Ok(tree)
    }

    async fn extras(&self, nodes: &mut [Node], extras: Extras) {
        let all = &*nodes;
        let reads = join_all(nodes.iter().map(|n| async move {
            let Some(obj) = n.obj.as_ref() else {
                return (None, None, None, None);
            };
            let role = n.role();
            let value = async {
                if !(extras.values && is_field(role)) {
                    return None;
                }
                self.value(obj, n.ifaces, VALUE_CHARS)
                    .await
                    .ok()
                    .filter(|v| !v.is_empty())
            };
            let bounds = async {
                if !(extras.bounds
                    && n.has(SHOWING)
                    && n.ifaces & IF_COMPONENT != 0
                    && is_active(role))
                {
                    return None;
                }
                self.extents(obj).await.ok()
            };
            let keys = async {
                if !(extras.keys && n.ifaces & IF_ACTION != 0) {
                    return None;
                }
                self.key_binding(obj).await.ok().filter(|k| !k.is_empty())
            };
            let label = async {
                if !(extras.values && n.name.is_empty() && n.has(SHOWING) && is_field(role)) {
                    return None;
                }
                let set: Vec<(u32, Vec<(String, OwnedObjectPath)>)> = self
                    .call(obj, ACCESSIBLE, "GetRelationSet", &())
                    .await
                    .ok()?;
                let by = labelled_by(set)?;
                match all.iter().find(|m| m.obj.as_ref() == Some(&by)) {
                    Some(m) => Some(m.name.clone()),
                    None => self.name(&by).await.ok(),
                }
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
            };
            tokio::join!(value, bounds, keys, label)
        }))
        .await;
        for (n, (value, bounds, keys, label)) in nodes.iter_mut().zip(reads) {
            n.value = value;
            n.bounds = bounds;
            n.keys = keys;
            if let Some(label) = label {
                n.name = label;
            }
        }
    }

    /// The node's live role, name and states, with the reads `extras` asks for; a value has at most `chars` characters.
    pub async fn node(&self, obj: &Obj, extras: Extras, chars: usize) -> Result<Node, String> {
        let r = self.raw(obj).await?;
        let (value, bounds, keys) = tokio::join!(
            async {
                if extras.values {
                    self.value(obj, r.ifaces, chars).await.ok()
                } else {
                    None
                }
            },
            async {
                if extras.bounds && r.ifaces & IF_COMPONENT != 0 {
                    self.extents(obj).await.ok()
                } else {
                    None
                }
            },
            async {
                if extras.keys && r.ifaces & IF_ACTION != 0 {
                    self.key_binding(obj).await.ok()
                } else {
                    None
                }
            }
        );
        Ok(Node {
            obj: Some(obj.clone()),
            role: r.role,
            name: r.name.trim_end().to_owned(),
            description: r.description,
            states: r.states,
            ifaces: r.ifaces,
            value,
            bounds,
            keys,
            ..Node::default()
        })
    }

    /// The first descendant of `obj` in the focused state, by one Collection match in its app; none when nothing matches or the app does not serve Collection.
    pub async fn focused_in(&self, obj: &Obj) -> Option<Obj> {
        let found: Vec<(String, OwnedObjectPath)> = self
            .call(obj, COLLECTION, "GetMatches", &focused_rule())
            .await
            .ok()?;
        found.into_iter().next().map(Obj::of)
    }

    /// The text of a text node, at most `max` characters, else the current value of a value node.
    pub async fn value(&self, obj: &Obj, ifaces: u8, max: usize) -> Result<String, String> {
        if ifaces & IF_TEXT != 0 {
            let max = i32::try_from(max).unwrap_or(i32::MAX);
            return self.call(obj, TEXT, "GetText", &(0i32, max)).await;
        }
        if ifaces & IF_VALUE != 0 {
            let v: f64 = self.prop(obj, VALUE, "CurrentValue").await?;
            return Ok(format_number(v));
        }
        Ok(String::new())
    }

    /// The screen rectangle of `obj` as `[x, y, width, height]`.
    pub async fn extents(&self, obj: &Obj) -> Result<[i32; 4], String> {
        let (x, y, w, h): (i32, i32, i32, i32) =
            self.call(obj, COMPONENT, "GetExtents", &(SCREEN,)).await?;
        Ok([x, y, w, h])
    }

    /// The key binding of the object's first action: `mnemonic;sequence;accelerator`.
    pub async fn key_binding(&self, obj: &Obj) -> Result<String, String> {
        self.call(obj, ACTION, "GetKeyBinding", &(0i32,)).await
    }

    /// Runs the object's first action, its default one.
    pub async fn do_action(&self, obj: &Obj) -> Result<bool, String> {
        self.call(obj, ACTION, "DoAction", &(0i32,)).await
    }

    /// Gives the object the keyboard focus.
    pub async fn grab_focus(&self, obj: &Obj) -> Result<bool, String> {
        self.call(obj, COMPONENT, "GrabFocus", &()).await
    }

    /// Selects the object in its parent, as a tab, list option or radio item.
    pub async fn select(&self, obj: &Obj) -> Result<bool, String> {
        let (parent, index) = tokio::join!(
            self.prop::<(String, OwnedObjectPath)>(obj, ACCESSIBLE, "Parent"),
            self.call::<i32, _>(obj, ACCESSIBLE, "GetIndexInParent", &())
        );
        let parent = Obj::of(parent?);
        self.call(&parent, SELECTION, "SelectChild", &(index?,))
            .await
    }

    /// The deepest object under the screen point `(x, y)` inside `within`.
    pub async fn at_point(&self, within: &Obj, x: i32, y: i32) -> Result<Option<Obj>, String> {
        let mut at: Option<Obj> = None;
        let mut current = within.clone();
        for _ in 0..MAX_DEPTH {
            let hit: (String, OwnedObjectPath) = self
                .call(&current, COMPONENT, "GetAccessibleAtPoint", &(x, y, SCREEN))
                .await?;
            let hit = Obj::of(hit);
            if hit.bus.is_empty() || hit.path.ends_with("/null") || at.as_ref() == Some(&hit) {
                break;
            }
            at = Some(hit.clone());
            current = hit;
        }
        Ok(at)
    }

    /// The parent of `obj`, none at an app root.
    pub async fn parent(&self, obj: &Obj) -> Result<Option<Obj>, String> {
        let parent = Obj::of(self.prop(obj, ACCESSIBLE, "Parent").await?);
        Ok(
            (!parent.bus.is_empty() && parent.path != ROOT && !parent.path.ends_with("/null"))
                .then_some(parent),
        )
    }

    /// The objects above `obj`, from its top-level window down to its parent.
    pub async fn ancestors(&self, obj: &Obj) -> Result<Vec<Obj>, String> {
        let mut chain = Vec::new();
        let mut current = obj.clone();
        while chain.len() < MAX_DEPTH {
            let Some(parent) = self.parent(&current).await? else {
                break;
            };
            chain.push(parent.clone());
            current = parent;
        }
        chain.reverse();
        Ok(chain)
    }

    async fn raw(&self, obj: &Obj) -> Result<Raw, String> {
        let (role, name, description, states, count, ifaces) = tokio::join!(
            self.role(obj),
            self.name(obj),
            self.prop::<String>(obj, ACCESSIBLE, "Description"),
            self.state(obj),
            self.prop::<i32>(obj, ACCESSIBLE, "ChildCount"),
            self.call::<Vec<String>, _>(obj, ACCESSIBLE, "GetInterfaces", &())
        );
        Ok(Raw {
            role: role?,
            name: name?,
            description: description.unwrap_or_default(),
            states: states?,
            count: count.unwrap_or(0),
            ifaces: iface_bits(&ifaces.unwrap_or_default()),
        })
    }

    /// The app's cached objects, from the cache of at-spi2-core 2.46 and later or from the older one,
    /// which lists the children of each object instead of its index and child count.
    async fn items(&self, bus: &str) -> Result<Vec<Item>, String> {
        let cache = Obj {
            bus: bus.to_owned(),
            path: CACHE_PATH.to_owned(),
        };
        let reply = self.reply(&cache, CACHE, "GetItems", &()).await?;
        cache_items(&reply).map_err(|e| format!("{CACHE}.GetItems on {bus}: {e}"))
    }

    async fn children(&self, obj: &Obj) -> Result<Vec<Obj>, String> {
        let kids: Vec<(String, OwnedObjectPath)> =
            self.call(obj, ACCESSIBLE, "GetChildren", &()).await?;
        Ok(kids.into_iter().map(Obj::of).collect())
    }

    async fn child_at(&self, obj: &Obj, index: i32) -> Result<Obj, String> {
        let kid: (String, OwnedObjectPath) = self
            .call(obj, ACCESSIBLE, "GetChildAtIndex", &(index,))
            .await?;
        Ok(Obj::of(kid))
    }

    async fn role(&self, obj: &Obj) -> Result<u32, String> {
        self.call(obj, ACCESSIBLE, "GetRole", &()).await
    }

    async fn name(&self, obj: &Obj) -> Result<String, String> {
        self.prop(obj, ACCESSIBLE, "Name").await
    }

    async fn state(&self, obj: &Obj) -> Result<u64, String> {
        let bits: Vec<u32> = self.call(obj, ACCESSIBLE, "GetState", &()).await?;
        Ok(state_bits(&bits))
    }

    /// The process id behind the accessibility bus name `bus`.
    pub async fn pid(&self, bus: &str) -> Result<u32, String> {
        let dbus = Obj {
            bus: DBUS.to_owned(),
            path: DBUS_PATH.to_owned(),
        };
        self.call(&dbus, DBUS, "GetConnectionUnixProcessID", &(bus,))
            .await
    }

    async fn prop<T>(&self, obj: &Obj, iface: &str, name: &str) -> Result<T, String>
    where
        T: TryFrom<OwnedValue>,
        T::Error: std::fmt::Display,
    {
        let v: OwnedValue = self.call(obj, PROPERTIES, "Get", &(iface, name)).await?;
        T::try_from(v).map_err(|e| format!("{iface}.{name}: {e}"))
    }

    async fn call<R, B>(&self, obj: &Obj, iface: &str, method: &str, body: &B) -> Result<R, String>
    where
        R: DeserializeOwned + zbus::zvariant::Type,
        B: serde::Serialize + DynamicType,
    {
        self.reply(obj, iface, method, body)
            .await?
            .body()
            .deserialize::<R>()
            .map_err(|e| format!("{iface}.{method} on {}: {e}", obj.id()))
    }

    /// One method call with a timeout; a peer that times out is skipped for a while.
    async fn reply<B>(
        &self,
        obj: &Obj,
        iface: &str,
        method: &str,
        body: &B,
    ) -> Result<zbus::Message, String>
    where
        B: serde::Serialize + DynamicType,
    {
        if self.is_hung(&obj.bus) {
            return Err(not_responding(&obj.bus));
        }
        let conn = self.connection().await?;
        let call = conn.call_method(
            Some(obj.bus.as_str()),
            obj.path.as_str(),
            Some(iface),
            method,
            body,
        );
        match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(e)) => Err(format!("{iface}.{method} on {}: {e}", obj.id())),
            Err(_) => {
                mark_hung(&obj.bus);
                Err(not_responding(&obj.bus))
            }
        }
    }

    fn is_hung(&self, bus: &str) -> bool {
        let mut hung = HUNG
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        hung.retain(|_, at| at.elapsed() < HUNG_FOR);
        hung.contains_key(bus)
    }

    async fn connection(&self) -> Result<Connection, String> {
        let mut slot = self.conn.lock().await;
        if let Some(conn) = slot.as_ref() {
            return Ok(conn.clone());
        }
        let session = Connection::session()
            .await
            .map_err(|e| format!("connect the session bus: {e}"))?;
        let reply = session
            .call_method(Some(BUS_NAME), BUS_PATH, Some(BUS_NAME), "GetAddress", &())
            .await
            .map_err(|e| format!("org.a11y.Bus.GetAddress: {e}"))?;
        let address: String = reply
            .body()
            .deserialize()
            .map_err(|e| format!("org.a11y.Bus.GetAddress: {e}"))?;
        let conn = zbus::connection::Builder::address(address.as_str())
            .map_err(|e| format!("accessibility bus address {address}: {e}"))?
            .build()
            .await
            .map_err(|e| format!("connect the accessibility bus: {e}"))?;
        *slot = Some(conn.clone());
        Ok(conn)
    }

    /// Drops the connection so that the next call opens a new one.
    pub async fn reset(&self) {
        *self.conn.lock().await = None;
    }
}

fn mark_hung(bus: &str) {
    HUNG.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(bus.to_owned(), Instant::now());
}

/// The frames `read` finds for the app on `bus` within [`APP_FRAMES`], else none, and the app is skipped for a while.
async fn capped(bus: &str, read: impl Future<Output = Vec<Frame>>) -> Vec<Frame> {
    match tokio::time::timeout(APP_FRAMES, read).await {
        Ok(frames) => frames,
        Err(_) => {
            mark_hung(bus);
            Vec::new()
        }
    }
}

/// The ARIA role of AT-SPI role `role` after W3C Core-AAM, else its AT-SPI name; a nameless generic container has none.
pub fn role_name(role: u32, named: bool) -> &'static str {
    aria_role(role, named).unwrap_or_else(|| native_role(role))
}

/// The AT-SPI name of role `role`.
pub fn native_role(role: u32) -> &'static str {
    ROLE_NAMES.get(role as usize).copied().unwrap_or("unknown")
}

/// The mnemonic and the shortcut of an AT-SPI key binding `mnemonic;sequence;accelerator`, in desk key names.
pub fn key_combos(binding: &str) -> (Option<String>, Option<String>) {
    let mut parts = binding.split(';');
    let mnemonic = parts.next().and_then(gtk_combo);
    let shortcut = parts.nth(1).and_then(gtk_combo);
    (mnemonic, shortcut)
}

fn aria_role(role: u32, named: bool) -> Option<&'static str> {
    Some(match role {
        2 | 101 => "alert",
        3 | 26 | 27 => "img",
        7 => "checkbox",
        8 => "menuitemcheckbox",
        10 | 57 => "columnheader",
        11 => "combobox",
        16 | 19 => "dialog",
        23 | 69 => "window",
        29 | 116 | 1 => "text",
        31 => "list",
        32 => "listitem",
        33 | 41 => "menu",
        34 => "menubar",
        35 | 59 => "menuitem",
        37 => "tab",
        38 => "tablist",
        40 | 61 | 79 => "textbox",
        42 | 103 => "progressbar",
        43 | 62 | 129 => "button",
        44 => "radio",
        45 => "menuitemradio",
        47 | 58 => "rowheader",
        48 => "scrollbar",
        49 | 53 | 68 | 99 | 17 => "group",
        50 => "separator",
        51 => "slider",
        52 => "spinbutton",
        54 | 102 => "status",
        55 => "table",
        56 => "cell",
        63 => "toolbar",
        64 => "tooltip",
        65 => "tree",
        66 => "treegrid",
        73 => "paragraph",
        83 => "heading",
        87 => "form",
        88 => "link",
        90 => "row",
        91 => "treeitem",
        95 | 82 => "document",
        98 => "listbox",
        110 => "region",
        r if GENERIC.contains(&r) => {
            if named {
                "group"
            } else {
                "generic"
            }
        }
        _ => return None,
    })
}

/// A GTK accelerator such as `<Primary><Shift>s` as `ctrl+shift+s`; none for an empty or unknown one.
fn gtk_combo(accel: &str) -> Option<String> {
    let mut keys = Vec::new();
    let mut rest = accel.trim();
    while let Some(tail) = rest.strip_prefix('<') {
        let (modifier, after) = tail.split_once('>')?;
        keys.push(match modifier.to_lowercase().as_str() {
            "primary" | "control" | "ctrl" | "ctl" => "ctrl",
            "shift" | "shft" => "shift",
            "alt" | "mod1" => "alt",
            "super" | "meta" | "hyper" | "mod4" => "super",
            _ => return None,
        });
        rest = after;
    }
    if rest.is_empty() {
        return None;
    }
    let key = rest.to_lowercase();
    let key = match key.as_str() {
        "page_up" | "prior" => "pageup",
        "page_down" | "next" => "pagedown",
        "kp_enter" => "return",
        "iso_left_tab" => "tab",
        k => k,
    };
    let mut combo = keys.join("+");
    if !combo.is_empty() {
        combo.push('+');
    }
    combo.push_str(key);
    Some(combo)
}

/// The items of a `GetItems` reply in either cache signature.
fn cache_items(reply: &zbus::Message) -> Result<Vec<Item>, zbus::Error> {
    let body = reply.body();
    match body.deserialize::<Vec<Item>>() {
        Ok(items) => Ok(items),
        Err(_) => body.deserialize::<Vec<OldItem>>().map(indexed),
    }
}

/// Items of the older cache with each object's index among its parent's children, -1 when the parent
/// does not list it, and its child count.
fn indexed(old: Vec<OldItem>) -> Vec<Item> {
    let mut index: HashMap<(Obj, Obj), i32> = HashMap::new();
    for (object, _, _, children, ..) in &old {
        let parent = Obj::of(object.clone());
        for (i, child) in (0..).zip(children) {
            index.insert((parent.clone(), Obj::of(child.clone())), i);
        }
    }
    old.into_iter()
        .map(
            |(object, app, parent, children, ifaces, name, role, description, states)| {
                let at = (Obj::of(parent.clone()), Obj::of(object.clone()));
                let count = i32::try_from(children.len()).unwrap_or(i32::MAX);
                let i = index.get(&at).copied().unwrap_or(-1);
                (
                    object,
                    app,
                    parent,
                    i,
                    count,
                    ifaces,
                    name,
                    role,
                    description,
                    states,
                )
            },
        )
        .collect()
}

fn needs_live(r: &Raw, cached: Option<&Vec<(i32, Obj)>>) -> bool {
    if r.states & (1 << MANAGES_DESCENDANTS) != 0 && !(1..=MANAGED_KIDS).contains(&r.count) {
        return false;
    }
    if r.role == ROLE_DOCUMENT_WEB {
        return r.count > 0 && cached.is_none_or(Vec::is_empty);
    }
    let have = cached.map_or(0, Vec::len);
    r.count < 0
        || usize::try_from(r.count).is_ok_and(|c| c != have)
        || cached.is_some_and(|c| c.iter().any(|(i, _)| *i < 0))
}

/// Appends `obj` and its subtree in preorder, an object shared by two parents under both; a nameless generic
/// container gives its place to its children.
fn flatten<'a>(
    obj: &'a Obj,
    raw: &HashMap<Obj, Raw>,
    kids: &'a HashMap<Obj, Vec<Obj>>,
    parent: Option<usize>,
    path: &mut Vec<&'a Obj>,
    out: &mut Vec<Node>,
) {
    let Some(r) = raw.get(obj) else {
        return;
    };
    if out.len() >= MAX_NODES || path.contains(&obj) {
        return;
    }
    let depth = parent.map_or(0, |p| out[p].depth + 1);
    let node = Node {
        obj: Some(obj.clone()),
        role: r.role,
        name: r.name.trim_end().to_owned(),
        description: r.description.clone(),
        states: r.states,
        ifaces: r.ifaces,
        parent,
        depth,
        ..Node::default()
    };
    let parent = if node.role() == "generic" && parent.is_some() {
        parent
    } else {
        out.push(node);
        Some(out.len() - 1)
    };
    path.push(obj);
    for c in kids.get(obj).into_iter().flatten() {
        flatten(c, raw, kids, parent, path, out);
    }
    path.pop();
}

fn state_bits(words: &[u32]) -> u64 {
    words
        .iter()
        .take(2)
        .enumerate()
        .fold(0, |acc, (i, w)| acc | (u64::from(*w) << (32 * i)))
}

fn iface_bits(names: &[String]) -> u8 {
    names.iter().fold(0, |acc, n| {
        acc | match n.as_str() {
            ACTION => IF_ACTION,
            COMPONENT => IF_COMPONENT,
            TEXT => IF_TEXT,
            VALUE => IF_VALUE,
            _ => 0,
        }
    })
}

/// The Collection.GetMatches arguments for the first descendant in the focused state, in tree order: a rule on the state alone, canonical order, one match, descending.
#[allow(clippy::type_complexity)]
fn focused_rule() -> (
    (
        Vec<i32>,
        i32,
        HashMap<String, String>,
        i32,
        Vec<i32>,
        i32,
        Vec<String>,
        i32,
        bool,
    ),
    u32,
    i32,
    bool,
) {
    let rule = (
        vec![1 << FOCUSED, 0],
        MATCH_ALL,
        HashMap::new(),
        MATCH_ALL,
        Vec::new(),
        MATCH_ALL,
        Vec::new(),
        MATCH_ALL,
        false,
    );
    (rule, SORT_CANONICAL, 1, true)
}

/// The first object an AT-SPI relation set names as the label of its owner.
fn labelled_by(set: Vec<(u32, Vec<(String, OwnedObjectPath)>)>) -> Option<Obj> {
    set.into_iter()
        .find(|(kind, _)| *kind == LABELLED_BY)?
        .1
        .into_iter()
        .next()
        .map(Obj::of)
}

fn not_responding(bus: &str) -> String {
    format!("AT-SPI peer {bus} is not responding")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn an_app_that_never_answers_is_left_out_and_skipped_by_a_new_client() {
        let started = tokio::time::Instant::now();
        let frames = capped(":9.99", std::future::pending()).await;
        assert!(frames.is_empty());
        assert_eq!(started.elapsed(), APP_FRAMES);
        assert!(Atspi::new().is_hung(":9.99"));
        let answered = capped(":9.98", async { Vec::new() }).await;
        assert!(answered.is_empty() && !Atspi::new().is_hung(":9.98"));
    }

    #[test]
    fn the_focused_match_asks_collection_for_one_focused_descendant_in_tree_order() {
        use zbus::zvariant::Type as _;

        let args = focused_rule();
        assert_eq!(
            <(
                (
                    Vec<i32>,
                    i32,
                    HashMap<String, String>,
                    i32,
                    Vec<i32>,
                    i32,
                    Vec<String>,
                    i32,
                    bool
                ),
                u32,
                i32,
                bool
            )>::SIGNATURE
                .to_string(),
            "((aiia{ss}iaiiasib)uib)"
        );
        assert_eq!(args.0.0, [1 << 12, 0]);
        assert_eq!((args.1, args.2, args.3), (1, 1, true));
    }

    #[test]
    fn an_id_round_trips_to_its_object() {
        let gtk = Obj {
            bus: ":1.12".into(),
            path: "/org/a11y/atspi/accessible/379".into(),
        };
        assert_eq!(gtk.id(), "1.12/379");
        assert_eq!(Obj::parse("1.12/379").unwrap(), gtk);
        let root = Obj {
            bus: ":1.3".into(),
            path: ROOT.into(),
        };
        assert_eq!(Obj::parse(&root.id()).unwrap(), root);
        let other = Obj {
            bus: ":1.4".into(),
            path: "/org/other/7".into(),
        };
        assert_eq!(Obj::parse(&other.id()).unwrap(), other);
        assert!(Obj::parse("nope").is_err());
    }

    #[test]
    fn a_field_takes_the_first_label_of_its_labelled_by_relation_and_no_other_relation() {
        let to = |p: &str| {
            (
                ":1.6".to_owned(),
                OwnedObjectPath::try_from(p.to_owned()).unwrap(),
            )
        };
        let set = vec![
            (1, vec![to("/org/a11y/atspi/accessible/980")]),
            (
                LABELLED_BY,
                vec![
                    to("/org/a11y/atspi/accessible/978"),
                    to("/org/a11y/atspi/accessible/981"),
                ],
            ),
        ];
        assert_eq!(labelled_by(set).unwrap().id(), "1.6/978");
        assert!(labelled_by(vec![(1, vec![to("/org/a11y/atspi/accessible/977")])]).is_none());
        assert!(labelled_by(vec![(LABELLED_BY, vec![])]).is_none());
        assert!(labelled_by(Vec::new()).is_none());
    }

    #[test]
    fn roles_follow_core_aam_and_a_nameless_generic_is_hidden() {
        assert_eq!(role_name(43, true), "button");
        assert_eq!(role_name(62, false), "button");
        assert_eq!(role_name(7, false), "checkbox");
        assert_eq!(role_name(29, true), "text");
        assert_eq!(role_name(61, false), "textbox");
        assert_eq!(role_name(101, true), "alert");
        assert_eq!(role_name(ROLE_PANEL, false), "generic");
        assert_eq!(role_name(ROLE_PANEL, true), "group");
        assert_eq!(role_name(ROLE_FILLER, false), "generic");
        assert_eq!(role_name(18, false), "drawing area");
        assert_eq!(role_name(500, false), "unknown");
    }

    #[test]
    fn a_nameless_generic_container_hands_its_place_to_its_children() {
        let o = |p: &str| Obj {
            bus: ":1.1".into(),
            path: format!("{PATH_PREFIX}/{p}"),
        };
        let r = |role: u32, name: &str| Raw {
            role,
            name: name.into(),
            ..Raw::default()
        };
        let raw = HashMap::from([
            (o("f"), r(23, "W")),
            (o("p"), r(ROLE_PANEL, "")),
            (o("g"), r(ROLE_PANEL, "Box")),
            (o("b"), r(43, "OK")),
            (o("c"), r(7, "On")),
        ]);
        let kids = HashMap::from([
            (o("f"), vec![o("p"), o("g")]),
            (o("p"), vec![o("b")]),
            (o("g"), vec![o("c")]),
        ]);
        let mut out = Vec::new();
        flatten(&o("f"), &raw, &kids, None, &mut Vec::new(), &mut out);
        let shown: Vec<(&str, Option<usize>, usize)> =
            out.iter().map(|n| (n.role(), n.parent, n.depth)).collect();
        assert_eq!(
            shown,
            vec![
                ("window", None, 0),
                ("button", Some(0), 1),
                ("group", Some(0), 1),
                ("checkbox", Some(2), 2)
            ]
        );
    }

    #[test]
    fn a_child_shared_by_two_parents_shows_under_both_and_a_cycle_ends() {
        let o = |p: &str| Obj {
            bus: ":1.1".into(),
            path: format!("{PATH_PREFIX}/{p}"),
        };
        let r = |role: u32, name: &str| Raw {
            role,
            name: name.into(),
            ..Raw::default()
        };
        let raw = HashMap::from([
            (o("d"), r(16, "Save As")),
            (o("b"), r(62, "Create Folder")),
            (o("p"), r(43, "Create")),
        ]);
        let kids = HashMap::from([
            (o("d"), vec![o("b"), o("p")]),
            (o("b"), vec![o("p"), o("d")]),
        ]);
        let mut out = Vec::new();
        flatten(&o("d"), &raw, &kids, None, &mut Vec::new(), &mut out);
        let shown: Vec<(&str, Option<usize>)> =
            out.iter().map(|n| (n.name.as_str(), n.parent)).collect();
        assert_eq!(
            shown,
            vec![
                ("Save As", None),
                ("Create Folder", Some(0)),
                ("Create", Some(1)),
                ("Create", Some(0))
            ]
        );
    }

    #[test]
    fn the_cache_is_trusted_unless_children_are_missing_or_unordered_and_small_managed_lists_are_walked()
     {
        let o = |p: &str| Obj {
            bus: ":1.1".into(),
            path: p.into(),
        };
        let r = |count: i32, states: u64| Raw {
            count,
            states,
            ..Raw::default()
        };
        let two = vec![(0, o("a")), (1, o("b"))];
        assert!(!needs_live(&r(2, 0), Some(&two)));
        assert!(needs_live(&r(3, 0), Some(&two)));
        assert!(needs_live(&r(-1, 0), Some(&two)));
        assert!(needs_live(
            &r(2, 0),
            Some(&vec![(-1, o("a")), (-1, o("b"))])
        ));
        assert!(needs_live(&r(5, 1 << MANAGES_DESCENDANTS), None));
        assert!(needs_live(&r(5, 1 << MANAGES_DESCENDANTS), Some(&two)));
        assert!(!needs_live(&r(2, 1 << MANAGES_DESCENDANTS), Some(&two)));
        assert!(!needs_live(
            &r(MANAGED_KIDS + 1, 1 << MANAGES_DESCENDANTS),
            None
        ));
        assert!(!needs_live(&r(i32::MAX, 1 << MANAGES_DESCENDANTS), None));
        assert!(!needs_live(&r(0, 0), None));
    }

    #[test]
    fn the_older_cache_gives_each_object_its_index_and_child_count() {
        let o = |p: &str| {
            (
                ":1.1".to_owned(),
                OwnedObjectPath::try_from(format!("{PATH_PREFIX}/{p}")).unwrap(),
            )
        };
        let item = |name: &str, parent: &str, children: &[&str]| -> OldItem {
            (
                o(name),
                o("root"),
                o(parent),
                children.iter().map(|c| o(c)).collect(),
                vec![ACTION.to_owned()],
                name.to_owned(),
                43,
                String::new(),
                vec![1 << ENABLED, 0],
            )
        };
        let old = vec![
            item("f", "root", &["a", "b"]),
            item("b", "f", &[]),
            item("a", "f", &["c"]),
            item("c", "a", &[]),
            item("x", "f", &[]),
        ];
        let reply = zbus::Message::method_call(CACHE_PATH, "GetItems")
            .unwrap()
            .build(&old)
            .unwrap();
        let items = cache_items(&reply).unwrap();
        let got: Vec<(&str, i32, i32)> = items
            .iter()
            .map(|(_, _, _, i, n, _, name, ..)| (name.as_str(), *i, *n))
            .collect();
        assert_eq!(
            got,
            vec![
                ("f", -1, 2),
                ("b", 1, 0),
                ("a", 0, 1),
                ("c", 0, 0),
                ("x", -1, 0)
            ]
        );
        let reply = zbus::Message::method_call(CACHE_PATH, "GetItems")
            .unwrap()
            .build(&items)
            .unwrap();
        assert_eq!(cache_items(&reply).unwrap(), items);
        let other = zbus::Message::method_call(CACHE_PATH, "GetItems")
            .unwrap()
            .build(&("x",))
            .unwrap();
        assert!(cache_items(&other).is_err());
    }

    #[test]
    fn key_bindings_become_desk_key_combinations() {
        assert_eq!(
            key_combos("<Alt>a;<Alt>f:a;<Primary><Shift>s"),
            (Some("alt+a".into()), Some("ctrl+shift+s".into()))
        );
        assert_eq!(key_combos("s;;"), (Some("s".into()), None));
        assert_eq!(
            key_combos(";;<Control>Page_Down"),
            (None, Some("ctrl+pagedown".into()))
        );
        assert_eq!(key_combos(";;F11"), (None, Some("f11".into())));
        assert_eq!(key_combos("<Release>x;;<Primary>"), (None, None));
        assert_eq!(key_combos(""), (None, None));
    }

    #[test]
    fn an_unmapped_role_keeps_its_native_name() {
        let n = |role: u32| Node {
            role,
            ..Node::default()
        };
        assert_eq!(n(18).native_role(), Some("drawing area"));
        assert_eq!(n(18).role(), "drawing area");
        assert_eq!(n(43).native_role(), None);
        assert_eq!(n(ROLE_PANEL).native_role(), None);
    }

    #[test]
    fn an_indeterminate_check_box_is_mixed() {
        let n = |role: u32| Node {
            role,
            states: (1 << INDETERMINATE) | (1 << ENABLED),
            ..Node::default()
        };
        assert_eq!(n(7).state_names(), vec!["indeterminate", "mixed"]);
        assert_eq!(n(42).state_names(), vec!["indeterminate"]);
    }

    #[test]
    fn state_words_make_one_bit_set() {
        assert_eq!(state_bits(&[0x10, 0x80]), 0x80_0000_0010);
        let n = Node {
            states: (1 << CHECKED) | (1 << SHOWING),
            ..Node::default()
        };
        assert_eq!(n.state_names(), vec!["checked", "showing", "disabled"]);
    }
}
