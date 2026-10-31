//! AT-SPI over D-Bus: a window's tree from its app's `Cache.GetItems`, filled in by live calls where the cache lacks children or their order, plus live reads and actions on one node.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;
use zbus::Connection;
use zbus::zvariant::{DynamicType, OwnedObjectPath, OwnedValue};

use crate::model::{MAX_DEPTH, MAX_NODES, VALUE_CHARS, format_number, hung, is_active, is_field};

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
const APPLICATION: &str = "org.a11y.atspi.Application";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const CALL_TIMEOUT: Duration = Duration::from_secs(3);
const HUNG_FOR: Duration = Duration::from_secs(10);
const WRITES: [&str; 3] = ["DoAction", "GrabFocus", "SelectChild"];
const APP_FRAMES: Duration = Duration::from_millis(500);
const MANAGED_KIDS: i32 = 200;
const LIST_ROLES: [u32; 4] = [31, 55, 65, 66];
const PASSWORD_TEXT: u32 = 40;
const SCREEN: u32 = 0;
const IF_ACTION: u8 = 1;
const IF_COMPONENT: u8 = 2;
const IF_TEXT: u8 = 4;
const IF_VALUE: u8 = 8;
const ACTIVE: u32 = 1;
const CHECKED: u32 = 4;
const ENABLED: u32 = 8;
const SENSITIVE: u32 = 24;
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

/// The apps that stopped answering, by bus name, with when; shared by every client, so a restarted worker keeps skipping them.
static HUNG: LazyLock<std::sync::Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

static SEEN: LazyLock<std::sync::Mutex<HashMap<String, Seen>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// The apps, by bus name, already asked for their attributes by [`Atspi::wake`].
static WOKEN: LazyLock<std::sync::Mutex<HashSet<String>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

/// Whether each app, by bus name, answers screen coordinates.
static SCREEN_COORDS: LazyLock<std::sync::Mutex<HashMap<String, bool>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

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
        let path = if path.contains('/') {
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

    /// Whether the node is greyed out: neither enabled nor sensitive, since GTK 4 marks a usable widget sensitive only.
    pub fn disabled(&self) -> bool {
        !self.has(ENABLED) && !self.has(SENSITIVE)
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
    pub stale: bool,
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
    pub frame: bool,
}

/// The tree of one window with the time each phase took.
pub struct Tree {
    pub frame: Frame,
    pub nodes: Vec<Node>,
    pub truncated: bool,
    pub phases: Vec<(&'static str, u128)>,
}

struct Seen {
    listed: Instant,
    slow: Option<Instant>,
    last: Vec<Frame>,
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

/// A lazily opened connection to the accessibility bus, shared by all calls.
pub struct Atspi {
    conn: Mutex<Option<Connection>>,
    probing: Arc<std::sync::Mutex<HashSet<String>>>,
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
            probing: Arc::default(),
        }
    }

    /// The accessible top-level windows of every app, with their pids; an app that takes over [`APP_FRAMES`] keeps its last windows, marked stale, until it counts as hung and is skipped for a while.
    pub async fn frames(&self) -> Result<Vec<Frame>, String> {
        let registry = Obj {
            bus: REGISTRY.to_owned(),
            path: ROOT.to_owned(),
        };
        let apps = self.children(&registry).await?;
        let mut answering = Vec::with_capacity(apps.len());
        for app in &apps {
            if !self.skips(&app.bus).await {
                answering.push(app);
            }
        }
        let asked = Instant::now();
        let reads = join_all(answering.into_iter().map(|app| async move {
            let read = tokio::time::timeout(APP_FRAMES, self.app_frames(app)).await;
            (app.bus.as_str(), read.ok())
        }))
        .await;
        let mut seen = SEEN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        seen.retain(|bus, _| apps.iter().any(|app| app.bus == *bus));
        Ok(reads
            .into_iter()
            .flat_map(|(bus, read)| settle(&mut seen, bus, read, asked))
            .collect())
    }

    /// The tree under `frame`: shown nodes in preorder, with `extras` read live.
    pub async fn tree(&self, frame: Frame, extras: Extras) -> Result<Tree, String> {
        let mut phases = Vec::new();
        let mut clock = Instant::now();
        let mut lap = |name: &'static str, phases: &mut Vec<(&'static str, u128)>| {
            phases.push((name, clock.elapsed().as_micros()));
            clock = Instant::now();
        };
        self.wake(&frame.obj.bus).await;
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

    /// The node's live role, name and states, with the reads `extras` asks for; a value has at most `chars` characters.
    pub async fn node(&self, obj: &Obj, extras: Extras, chars: usize) -> Result<Node, String> {
        let r = self.raw(obj).await?;
        let (value, bounds, keys) = tokio::join!(
            async {
                if extras.values {
                    self.value(obj, r.role, r.ifaces, chars).await
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

    /// The text of a text node, at most `max` characters, else the current value of a value node; none for a password field or a failed read.
    pub async fn value(&self, obj: &Obj, role: u32, ifaces: u8, max: usize) -> Option<String> {
        if secret(role) {
            return None;
        }
        if ifaces & IF_TEXT != 0 {
            let max = i32::try_from(max).unwrap_or(i32::MAX);
            return self.call(obj, TEXT, "GetText", &(0i32, max)).await.ok();
        }
        if ifaces & IF_VALUE != 0 {
            return self
                .prop::<f64>(obj, VALUE, "CurrentValue")
                .await
                .ok()
                .map(format_number);
        }
        Some(String::new())
    }

    /// The screen rectangle of `obj` as `[x, y, width, height]`.
    pub async fn extents(&self, obj: &Obj) -> Result<[i32; 4], String> {
        if !self.screen_coords(&obj.bus).await {
            return Err(no_screen_coords(obj));
        }
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
        if !self.screen_coords(&within.bus).await {
            return Err(no_screen_coords(within));
        }
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

    /// The process id behind the accessibility bus name `bus`.
    pub async fn pid(&self, bus: &str) -> Result<u32, String> {
        let dbus = Obj {
            bus: DBUS.to_owned(),
            path: DBUS_PATH.to_owned(),
        };
        self.call(&dbus, DBUS, "GetConnectionUnixProcessID", &(bus,))
            .await
    }

    /// Drops the connection so that the next call opens a new one.
    pub async fn reset(&self) {
        *self.conn.lock().await = None;
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
                stale: false,
            })
        }))
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
    }

    async fn extras(&self, nodes: &mut [Node], extras: Extras) {
        if extras.bounds
            && let Some(first) = nodes.first().and_then(|n| n.obj.as_ref())
        {
            self.screen_coords(&first.bus).await;
        }
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
                self.value(obj, n.role, n.ifaces, VALUE_CHARS)
                    .await
                    .filter(|v| !v.is_empty())
            };
            let bounds = async {
                if !(extras.bounds
                    && n.has(SHOWING)
                    && n.ifaces & IF_COMPONENT != 0
                    && (extras.frame || is_active(role)))
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

    /// Asks the app on `bus` for its root's attributes once, as a screen reader does: Chromium builds its tree only after that.
    async fn wake(&self, bus: &str) {
        if first_wake(bus) {
            let root = Obj {
                bus: bus.to_owned(),
                path: ROOT.to_owned(),
            };
            let _ = self.reply(&root, ACCESSIBLE, "GetAttributes", &()).await;
        }
    }

    /// Whether the app on `bus` answers screen coordinates, read once from its toolkit name and version.
    async fn screen_coords(&self, bus: &str) -> bool {
        if let Some(known) = SCREEN_COORDS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(bus)
        {
            return *known;
        }
        let root = Obj {
            bus: bus.to_owned(),
            path: ROOT.to_owned(),
        };
        let (Ok(name), Ok(version)) = tokio::join!(
            self.prop::<String>(&root, APPLICATION, "ToolkitName"),
            self.prop::<String>(&root, APPLICATION, "Version")
        ) else {
            return true;
        };
        let known = answers_screen_coords(&name, &version);
        SCREEN_COORDS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(bus.to_owned(), known);
        known
    }

    /// The app's cached objects, from the cache of at-spi2-core 2.46 and later or from the older one, which lists the children of each object instead of its index and child count.
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
        if self.skips(&obj.bus).await {
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
                Err(timed_out(&obj.bus, method))
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

    async fn skips(&self, bus: &str) -> bool {
        if !self.is_hung(bus) {
            return false;
        }
        if let Ok(conn) = self.connection().await {
            let root = bus.to_owned();
            release_on_answer(&self.probing, bus, async move {
                let asked =
                    conn.call_method(Some(root.as_str()), ROOT, Some(ACCESSIBLE), "GetState", &());
                tokio::time::timeout(HUNG_FOR, asked).await.is_ok()
            });
        }
        true
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

fn first_wake(bus: &str) -> bool {
    WOKEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(bus.to_owned())
}

/// Whether a toolkit answers screen coordinates; GTK 4 answers every screen point and extent with the origin.
fn answers_screen_coords(toolkit: &str, version: &str) -> bool {
    !(toolkit == "GTK" && version.split('.').next() == Some("4"))
}

fn mark_hung(bus: &str) {
    HUNG.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(bus.to_owned(), Instant::now());
}

fn release_on_answer(
    probing: &Arc<std::sync::Mutex<HashSet<String>>>,
    bus: &str,
    answer: impl Future<Output = bool> + Send + 'static,
) {
    if !probing
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(bus.to_owned())
    {
        return;
    }
    let (probing, bus) = (Arc::clone(probing), bus.to_owned());
    tokio::spawn(async move {
        if answer.await {
            HUNG.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&bus);
        }
        probing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&bus);
    });
}

/// The frames of the app on `bus` from a read asked at `asked`: what it answered, else its last frames marked stale, and none once it counts as hung.
fn settle(
    seen: &mut HashMap<String, Seen>,
    bus: &str,
    read: Option<Vec<Frame>>,
    asked: Instant,
) -> Vec<Frame> {
    let s = seen.entry(bus.to_owned()).or_insert_with(|| Seen {
        listed: asked,
        slow: None,
        last: Vec::new(),
    });
    if let Some(frames) = read {
        s.slow = None;
        s.last.clone_from(&frames);
        return frames;
    }
    let slow = s.slow.get_or_insert(asked).elapsed();
    if hung(slow, s.listed.elapsed()) {
        s.last.clear();
        mark_hung(bus);
        return Vec::new();
    }
    s.last
        .iter()
        .map(|f| Frame {
            stale: true,
            ..f.clone()
        })
        .collect()
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

/// Items of the older cache with each object's index among its parent's children, -1 when the parent does not list it, and its child count.
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

/// Appends `obj` and its subtree in preorder, an object shared by two parents under both; a nameless generic container gives its place to its children.
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

fn no_screen_coords(obj: &Obj) -> String {
    format!(
        "{}: its GTK 4 app reports no screen position; press it by its action",
        obj.id()
    )
}

fn secret(role: u32) -> bool {
    role == PASSWORD_TEXT
}

fn not_responding(bus: &str) -> String {
    format!("AT-SPI peer {bus} is not responding")
}

fn timed_out(bus: &str, method: &str) -> String {
    if WRITES.contains(&method) {
        format!(
            "{}; the {method} was sent and may still take effect once it answers, so read its effect back before acting again",
            not_responding(bus)
        )
    } else {
        not_responding(bus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_late_app_keeps_its_last_frames_stale_until_it_has_missed_for_two_seconds() {
        let window = |bus: &str| Frame {
            obj: Obj {
                bus: bus.into(),
                path: format!("{PATH_PREFIX}/1"),
            },
            role: 23,
            name: "Doc".into(),
            states: 0,
            pid: Some(7),
            stale: false,
        };
        let mut seen = HashMap::new();
        let now = Instant::now();
        let ago = |s: u64| now.checked_sub(Duration::from_secs(s)).unwrap();
        let answered = settle(&mut seen, ":9.94", Some(vec![window(":9.94")]), ago(30));
        assert!(!answered[0].stale);
        let late = settle(&mut seen, ":9.94", None, now);
        assert_eq!(late.len(), 1);
        assert!(late[0].stale && late[0].name == "Doc");
        assert!(!Atspi::new().is_hung(":9.94"));
        seen.get_mut(":9.94").unwrap().slow = Some(ago(2));
        assert!(settle(&mut seen, ":9.94", None, now).is_empty());
        assert!(Atspi::new().is_hung(":9.94"));
        assert!(seen[":9.94"].last.is_empty());
    }

    #[test]
    fn an_app_in_its_launch_grace_is_never_marked_hung_and_an_answer_ends_its_misses() {
        let mut seen = HashMap::new();
        let now = Instant::now();
        let ago = |s: u64| now.checked_sub(Duration::from_secs(s)).unwrap();
        assert!(settle(&mut seen, ":9.93", None, ago(5)).is_empty());
        assert!(settle(&mut seen, ":9.93", None, now).is_empty());
        assert!(!Atspi::new().is_hung(":9.93"));
        assert!(settle(&mut seen, ":9.93", Some(Vec::new()), now).is_empty());
        assert_eq!(seen[":9.93"].slow, None);
    }

    #[tokio::test]
    async fn an_answer_to_the_probe_releases_a_hung_app_and_one_probe_runs_at_a_time() {
        let probing = Arc::default();
        mark_hung(":9.92");
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        release_on_answer(&probing, ":9.92", async { rx.await.is_ok() });
        release_on_answer(&probing, ":9.92", async { true });
        tokio::task::yield_now().await;
        assert!(Atspi::new().is_hung(":9.92"));
        tx.send(()).unwrap();
        while probing.lock().unwrap().contains(":9.92") {
            tokio::task::yield_now().await;
        }
        assert!(!Atspi::new().is_hung(":9.92"));
        mark_hung(":9.91");
        release_on_answer(&probing, ":9.91", async { false });
        while probing.lock().unwrap().contains(":9.91") {
            tokio::task::yield_now().await;
        }
        assert!(Atspi::new().is_hung(":9.91"));
    }

    #[test]
    fn a_write_that_timed_out_may_still_land_and_a_read_says_nothing_of_it() {
        for write in WRITES {
            assert!(timed_out(":1.0", write).contains("may still take effect"));
        }
        assert_eq!(timed_out(":1.0", "GetText"), not_responding(":1.0"));
    }

    #[test]
    fn only_a_password_field_keeps_its_value_unread() {
        assert!(secret(PASSWORD_TEXT));
        assert!(!secret(61));
    }

    #[test]
    fn each_app_is_woken_once_per_bus_name() {
        assert!(first_wake(":9.97"));
        assert!(!first_wake(":9.97"));
        assert!(first_wake(":9.96"));
        assert!(!first_wake(":9.96"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_app_costs_its_wake_no_wait_and_is_not_asked_again() {
        let started = tokio::time::Instant::now();
        mark_hung(":9.95");
        Atspi::new().wake(":9.95").await;
        assert_eq!(started.elapsed(), Duration::ZERO);
        assert!(!first_wake(":9.95"));
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
        let gtk4 = Obj {
            bus: ":1.14".into(),
            path: "/dev/cmgs/Probe/a11y/8c851f3c".into(),
        };
        assert_eq!(Obj::parse(&gtk4.id()).unwrap(), gtk4);
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
    fn a_node_is_disabled_only_when_neither_enabled_nor_sensitive() {
        let n = |states: u64| Node {
            states,
            ..Node::default()
        };
        assert!(!n(1 << SENSITIVE).disabled());
        assert!(!n((1 << ENABLED) | (1 << SENSITIVE)).disabled());
        assert!(!n(1 << ENABLED).disabled());
        assert!(n(1 << SHOWING).disabled());
        assert_eq!(n(1 << SHOWING).state_names(), vec!["showing", "disabled"]);
        assert_eq!(
            n((1 << SHOWING) | (1 << SENSITIVE)).state_names(),
            vec!["showing"]
        );
    }

    #[test]
    fn only_gtk_4_lacks_screen_coordinates() {
        assert!(!answers_screen_coords("GTK", "4.14.5"));
        assert!(!answers_screen_coords("GTK", "4.20.1"));
        assert!(answers_screen_coords("GTK", "3.24.41"));
        assert!(answers_screen_coords("Chromium", "156.0.8078.47"));
        assert!(answers_screen_coords("Qt", "6.4.2"));
        assert!(answers_screen_coords("", ""));
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
