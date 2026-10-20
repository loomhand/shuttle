//! The desk calls of GUI plans and scripts on Android: windows and element trees from the UI server, input through adb.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use desk_core::args::{
    ActArgs, AtArgs, ClickArgs, FindArgs, IdArgs, KeyArgs, LaunchArgs, ReadArgs, ShotArgs,
    SnapArgs, TypeArgs, WaitArgs, WindowArgs, parse,
};
use desk_core::text::{MAX_FOUND, NAME_CHARS, Named, matches, ms, quote};
use desk_core::window::{self, Window};
use desk_core::wire::Output;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::adb::{Android, is_package};

const APPLICATION: i64 = 1;
const PID_BASE: u32 = 1 << 22;
const FOCUS_WAIT: Duration = Duration::from_secs(5);
const FOCUS_POLL: Duration = Duration::from_millis(100);
const SWITCH_POLLS: u32 = 10;
const SCROLL_TRIES: u32 = 6;
const SCROLL_MS: &str = "600";
const SCROLL_SETTLE: Duration = Duration::from_millis(400);
const MIN_SPAN: i32 = 8;
const MIN_SWIPE: i32 = 48;
const WAIT_MAX: Duration = Duration::from_secs(5);
const WAIT_POLL: Duration = Duration::from_millis(10);

/// One element of a window as the desk and computer_state show it.
#[derive(Debug)]
pub(crate) struct Node {
    path: String,
    parent: i64,
    depth: usize,
    role: &'static str,
    name: String,
    description: String,
    value: Option<String>,
    states: Vec<&'static str>,
    bounds: [i32; 4],
    /// Whether any of the element is on the screen; a list keeps rows next to the shown ones.
    visible: bool,
    /// The part of the screen that every list or page scrolling the element shows, as `[x, y, w, h]`.
    scroller: Option<[i32; 4]>,
}

impl Node {
    /// The computer_state line of the element as `[i]`.
    pub(crate) fn line(&self, i: usize) -> String {
        let (x, y) = self.center();
        let mut line = format!("[{i}] {} {:?} at {x},{y}", self.role, self.name);
        for s in self.states.iter().filter(|s| **s != "showing") {
            let _ = write!(line, " {s}");
        }
        if let Some(v) = self.value.as_deref().filter(|v| !v.is_empty()) {
            let _ = write!(line, " value {v:?}");
        }
        line
    }

    pub(crate) fn center(&self) -> (i32, i32) {
        let [x, y, w, h] = self.bounds;
        (x + w / 2, y + h / 2)
    }

    /// The part of the element that shows: its bounds cut to its list and the screen; none when nothing shows.
    fn shown(&self, (screen_w, screen_h): (i32, i32)) -> Option<[i32; 4]> {
        let [x, y, w, h] = self.bounds;
        let (mut left, mut top) = (x.max(0), y.max(0));
        let (mut right, mut bottom) = ((x + w).min(screen_w), (y + h).min(screen_h));
        if let Some([sx, sy, sw, sh]) = self.scroller {
            (left, top) = (left.max(sx), top.max(sy));
            (right, bottom) = (right.min(sx + sw), bottom.min(sy + sh));
        }
        (right > left && bottom > top).then_some([left, top, right - left, bottom - top])
    }

    /// Whether the name, description or value contains lowercase `q`, ignoring their case.
    pub(crate) fn mentions(&self, q: &str) -> bool {
        [
            Some(self.name.as_str()),
            Some(self.description.as_str()),
            self.value.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|t| t.to_lowercase().contains(q))
    }

    fn id(&self, app: &str) -> String {
        format!("{app}:{}:{}:{}", self.path, self.role, self.name)
    }

    fn json(&self, i: usize, app: &str) -> Value {
        let mut v = json!({
            "ref": i,
            "id": self.id(app),
            "parent": self.parent,
            "depth": self.depth,
            "role": self.role,
            "name": self.name,
            "states": self.states,
            "bounds": self.bounds,
        });
        if let Some(value) = &self.value {
            v["value"] = value.as_str().into();
        }
        if !self.description.is_empty() {
            v["description"] = self.description.as_str().into();
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

/// An Android window the desk lists: an app's activity or dialog, titled `<title> - <package>`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Frame {
    id: String,
    package: String,
    title: String,
    dialog: bool,
    pid: u32,
    active: bool,
    bounds: [i32; 4],
    covers: Vec<[i32; 4]>,
    screen: (i32, i32),
}

impl Frame {
    fn json(&self) -> Value {
        json!({
            "id": self.id,
            "title": self.title,
            "role": if self.dialog { "dialog" } else { "frame" },
            "pid": self.pid,
            "active": self.active,
        })
    }

    fn span(&self, x: i32, top: i32, bottom: i32) -> Option<(i32, i32)> {
        if !(0..self.screen.0).contains(&x) {
            return None;
        }
        let mut open = vec![(top.max(0), bottom.min(self.screen.1))];
        for &[cx, cy, cw, ch] in &self.covers {
            if (cx..cx + cw).contains(&x) {
                open = open
                    .into_iter()
                    .flat_map(|(a, b)| [(a, b.min(cy)), (a.max(cy + ch), b)])
                    .filter(|(a, b)| b > a)
                    .collect();
            }
        }
        open.into_iter()
            .filter(|(a, b)| b - a >= MIN_SPAN)
            .max_by_key(|(a, b)| b - a)
    }

    pub(crate) fn window(&self) -> &str {
        &self.id
    }

    /// Whether a system window covers screen point `(x, y)`.
    pub(crate) fn covered(&self, (x, y): (i32, i32)) -> bool {
        self.covers
            .iter()
            .any(|&[cx, cy, cw, ch]| (cx..cx + cw).contains(&x) && (cy..cy + ch).contains(&y))
    }

    /// The part of element `n` a tap reaches: its shown part, less what a bar or keyboard covers at its middle column.
    fn reach(&self, n: &Node) -> Option<[i32; 4]> {
        let [x, y, w, h] = n.shown(self.screen).filter(|_| n.visible)?;
        let (top, bottom) = self.span(x + w / 2, y, y + h)?;
        Some([x, top, w, bottom - top])
    }

    /// Where a tap reaches element `n`: the middle of that part.
    fn tap_at(&self, n: &Node) -> Option<(i32, i32)> {
        let [x, y, w, h] = self.reach(n)?;
        Some((x + w / 2, y + h / 2))
    }

    fn swipe(&self, [sx, sy, sw, sh]: [i32; 4], target: i32) -> Option<(i32, i32, i32)> {
        let x = sx + sw / 2;
        let (top, bottom) = self
            .span(x, sy, sy + sh)
            .filter(|(top, bottom)| bottom - top >= MIN_SWIPE)?;
        let (low, high) = (top + (bottom - top) * 3 / 4, top + (bottom - top) / 4);
        Some(if target >= top + (bottom - top) / 2 {
            (x, low, high)
        } else {
            (x, high, low)
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
        Some(self.pid)
    }

    fn focusable(&self) -> bool {
        true
    }
}

#[derive(Deserialize)]
struct WindowInfo {
    id: i64,
    #[serde(rename = "type")]
    kind: i64,
    title: String,
    package: String,
    active: bool,
    #[serde(default)]
    focused: bool,
    #[serde(default)]
    layer: i64,
    bounds: [i32; 4],
}

struct Raw {
    path: String,
    class: String,
    text: String,
    desc: String,
    hint: String,
    showing_hint: bool,
    clickable: bool,
    checkable: bool,
    checked: bool,
    editable: bool,
    enabled: bool,
    focused: bool,
    selected: bool,
    scrollable: bool,
    visible: bool,
    rect: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// Runs desk call `verb` on Android.
pub async fn run(android: &Android, verb: &str, args: Value) -> Result<Output, String> {
    let text = match verb {
        "desk_windows" => {
            Value::Array(frames(android).await?.iter().map(Frame::json).collect()).to_string()
        }
        "desk_wait" => {
            let a: WaitArgs = parse(args)?;
            wait_for(a.since, Duration::from_millis(a.timeout_ms), || async {
                Ok(Value::Array(
                    frames(android).await?.iter().map(Frame::json).collect(),
                ))
            })
            .await?
            .to_string()
        }
        "desk_snapshot" => snapshot(android, parse(args)?).await?,
        "desk_find" => find(android, parse(args)?).await?,
        "desk_read" => read(android, parse(args)?).await?,
        "desk_press" => press(android, &parse::<IdArgs>(args)?.id).await?,
        "desk_act" => {
            let a: ActArgs = parse(args)?;
            match a.action.as_str() {
                "click" | "action" | "select" | "focus" => press(android, &a.id).await?,
                other => {
                    return Err(format!(
                        "unknown action {other}; use click, action, select or focus"
                    ));
                }
            }
        }
        "desk_at" => at(android, parse(args)?).await?,
        "desk_key" => {
            android.keys(&parse::<KeyArgs>(args)?.keys).await?;
            json!({"ok": true}).to_string()
        }
        "desk_type" => type_text(android, &parse::<TypeArgs>(args)?.text).await?,
        "desk_click" => click(android, parse(args)?).await?,
        "desk_focus" => focus(android, parse(args)?).await?,
        "desk_close" => {
            let a: WindowArgs = parse(args)?;
            let frame = window::pick(frames(android).await?, Some(&a.window), a.pid)?;
            if !frame.active {
                return Err(format!(
                    "{} is not in front, and back closes only the front window",
                    frame.title
                ));
            }
            android.keys("back").await?;
            json!({"ok": true, "title": frame.title}).to_string()
        }
        "desk_launch" => {
            let app = parse::<LaunchArgs>(args)?.app;
            json!({"ok": true, "launched": android.launch(&app).await?}).to_string()
        }
        "desk_screen" => {
            let (width, height) = android.screen().await?;
            json!({"width": width, "height": height}).to_string()
        }
        "desk_shot" => return shot(android, parse(args)?).await,
        other => return Err(format!("unknown desk tool {other}")),
    };
    Ok(Output::text(text))
}

/// The elements of a UI server dump that a tap reaches in `frame`, each with the bounds of that part.
pub(crate) fn elements(xml: &str, frame: &Frame) -> Vec<Node> {
    nodes(&Frame::default(), &raws(xml))
        .into_iter()
        .skip(1)
        .filter_map(|n| {
            Some(Node {
                bounds: frame.reach(&n)?,
                ..n
            })
        })
        .collect()
}

/// Whether a dump is of the window `front`; a dump without a window id, or a front frame without one, is not told apart.
pub(crate) fn same_window(xml: &str, front: &Frame) -> bool {
    front.window().is_empty() || attribute(xml, "window").is_none_or(|w| w == front.window())
}

/// The front window with the system windows over it; with none listed, a bare frame of the screen.
pub(crate) async fn front(android: &Android) -> Result<Frame, String> {
    let frames = frames(android).await?;
    match frames.into_iter().find(|f| f.active) {
        Some(frame) => Ok(frame),
        None => Ok(Frame {
            screen: android.screen().await?,
            ..Frame::default()
        }),
    }
}

/// The listed windows, the front one active; an activity switch lists none for a moment, which this waits out.
async fn frames(android: &Android) -> Result<Vec<Frame>, String> {
    let screen = android.screen().await?;
    let mut shown = Vec::new();
    for _ in 0..SWITCH_POLLS {
        let list: Vec<WindowInfo> = serde_json::from_str(android.windows().await?.trim())
            .map_err(|e| format!("decode windows: {e}"))?;
        shown = frames_of(list, screen);
        if !shown.is_empty() {
            break;
        }
        tokio::time::sleep(FOCUS_POLL).await;
    }
    Ok(shown)
}

fn frames_of(list: Vec<WindowInfo>, screen: (i32, i32)) -> Vec<Frame> {
    let system: Vec<(i64, [i32; 4])> = list
        .iter()
        .filter(|w| w.kind != APPLICATION)
        .map(|w| (w.layer, w.bounds))
        .collect();
    let shown = listed(list);
    let alone = shown.len() == 1;
    shown
        .into_iter()
        .map(|w| {
            let covers = system
                .iter()
                .filter(|(layer, _)| *layer > w.layer)
                .map(|&(_, [l, t, r, b])| [l, t, r - l, b - t])
                .collect();
            Frame {
                covers,
                ..frame(w, alone, screen)
            }
        })
        .collect()
}

fn listed(list: Vec<WindowInfo>) -> Vec<WindowInfo> {
    let mut shown: Vec<WindowInfo> = list
        .into_iter()
        .filter(|w| !w.package.is_empty() && (w.kind == APPLICATION || w.active))
        .collect();
    if !shown.iter().any(|w| w.active)
        && let Some(w) = shown.iter_mut().max_by_key(|w| (w.focused, w.layer))
    {
        w.active = true;
    }
    shown
}

/// A window short of the screen is a dialog over another window; alone it is an activity in a transition.
fn frame(w: WindowInfo, alone: bool, screen: (i32, i32)) -> Frame {
    let [l, t, r, b] = w.bounds;
    let full = alone || (r - l >= screen.0 && b - t >= screen.1 * 9 / 10);
    let title = if w.title.trim().is_empty() || w.title == w.package {
        w.package.clone()
    } else {
        format!("{} - {}", w.title.trim(), w.package)
    };
    Frame {
        id: w.id.to_string(),
        pid: package_pid(&w.package),
        package: w.package,
        title,
        dialog: !full,
        active: w.active,
        bounds: [l, t, r - l, b - t],
        covers: Vec::new(),
        screen,
    }
}

/// A stable stand-in pid for an Android package, above Linux's pid range so it names no box process.
fn package_pid(package: &str) -> u32 {
    let hash = package.bytes().fold(2_166_136_261u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(16_777_619)
    });
    PID_BASE + hash % PID_BASE
}

/// The window an id or title names, else the focused one, with its nodes: the window itself first.
async fn tree(
    android: &Android,
    id: Option<&str>,
    title: Option<&str>,
    pid: Option<u32>,
) -> Result<(Frame, Vec<Node>), String> {
    let frames = frames(android).await?;
    let frame = match id {
        Some(id) => frames
            .into_iter()
            .find(|f| f.id == id)
            .ok_or_else(|| format!("no window {id}"))?,
        None => window::pick(frames, title, pid)?,
    };
    let xml = android.tree(Some(&frame.id)).await?;
    let nodes = nodes(&frame, &raws(&xml));
    Ok((frame, nodes))
}

/// The front window of package `app` with its nodes.
async fn app_tree(android: &Android, app: &str) -> Result<(Frame, Vec<Node>), String> {
    let mut own: Vec<Frame> = frames(android)
        .await?
        .into_iter()
        .filter(|f| f.package == app)
        .collect();
    own.sort_by_key(|f| !f.active);
    let frame = own
        .into_iter()
        .next()
        .ok_or_else(|| format!("no window of {app}"))?;
    let xml = android.tree(Some(&frame.id)).await?;
    let nodes = nodes(&frame, &raws(&xml));
    Ok((frame, nodes))
}

fn raws(xml: &str) -> Vec<Raw> {
    xml.split("<node ")
        .skip(1)
        .filter_map(|node| {
            let attr = |key: &str| attribute(node, key).unwrap_or_default();
            let flag = |key: &str| attr(key) == "true";
            Some(Raw {
                path: attr("path"),
                class: attr("class"),
                text: attr("text"),
                desc: attr("content-desc"),
                hint: attr("hint"),
                showing_hint: flag("showing-hint"),
                clickable: flag("clickable") || flag("long-clickable"),
                checkable: flag("checkable"),
                checked: flag("checked"),
                editable: flag("editable"),
                enabled: attr("enabled") != "false",
                focused: flag("focused"),
                selected: flag("selected"),
                scrollable: flag("scrollable"),
                visible: attr("visible") != "false",
                rect: bounds(&attr("bounds"))?,
            })
        })
        .collect()
}

/// The unescaped value of `key="..."` in the attributes of one node.
fn attribute(node: &str, key: &str) -> Option<String> {
    let head = node.split('>').next()?;
    let start = head
        .find(&format!(" {key}=\""))
        .map(|i| i + key.len() + 3)
        .or_else(|| {
            head.strip_prefix(&format!("{key}=\""))
                .map(|_| key.len() + 2)
        })?;
    let len = head[start..].find('"')?;
    Some(unescape(&head[start..start + len]))
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';') else { break };
        let entity = &rest[1..end];
        let ch = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `[l,t][r,b]` of a dump node.
fn bounds(value: &str) -> Option<Rect> {
    let nums: Vec<i32> = value
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .filter(|s| !s.is_empty())
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    match nums[..] {
        [left, top, right, bottom] => Some(Rect {
            left,
            top,
            right,
            bottom,
        }),
        _ => None,
    }
}

/// The window, then each element that shows text or takes input; a toggle in a checkable row is the row.
fn nodes(frame: &Frame, raws: &[Raw]) -> Vec<Node> {
    let mut out = vec![Node {
        path: "w".to_owned(),
        parent: -1,
        depth: 0,
        role: if frame.dialog { "dialog" } else { "frame" },
        name: frame.title.clone(),
        description: String::new(),
        value: None,
        states: vec!["showing", "active"],
        bounds: frame.bounds,
        visible: true,
        scroller: None,
    }];
    let mut listed: Vec<(usize, usize)> = Vec::new();
    for (i, r) in raws.iter().enumerate() {
        let interactive = r.clickable || r.checkable || r.editable;
        let own = if r.editable {
            r.hint.as_str()
        } else {
            r.text.as_str()
        };
        if !interactive && r.text.is_empty() && r.desc.is_empty() {
            continue;
        }
        let up = listed
            .iter()
            .rev()
            .find(|(raw, _)| under(&r.path, &raws[*raw].path))
            .copied();
        if r.checkable && up.is_some_and(|(raw, _)| raws[raw].checkable) {
            continue;
        }
        let mut name = if own.is_empty() {
            r.desc.clone()
        } else {
            own.to_owned()
        };
        let mut description = String::new();
        if name.is_empty() && interactive {
            let mut texts = labels_of(raws, i);
            if !texts.is_empty() {
                name = texts.remove(0);
                description = texts.join(" ");
            }
        }
        if name.is_empty()
            && r.checkable
            && let Some((_, at)) = up
        {
            name = out[at].name.clone();
        }
        let mut states = vec!["showing"];
        if !r.enabled {
            states.push("disabled");
        }
        if r.checked {
            states.push("checked");
        }
        if r.focused {
            states.push("focused");
        }
        if r.selected {
            states.push("selected");
        }
        let value = r.editable.then(|| {
            if r.showing_hint {
                String::new()
            } else {
                r.text.clone()
            }
        });
        let (parent, depth) = up.map_or((0, 1), |(_, at)| (at as i64, out[at].depth + 1));
        listed.push((i, out.len()));
        out.push(Node {
            path: r.path.clone(),
            parent,
            depth,
            role: role(r),
            name: one_line(&name),
            description,
            value,
            states,
            bounds: xywh(r.rect),
            visible: r.visible,
            scroller: viewport(raws, &r.path),
        });
    }
    out
}

fn viewport(raws: &[Raw], path: &str) -> Option<[i32; 4]> {
    raws.iter()
        .filter(|a| a.scrollable && under(path, &a.path))
        .map(|a| a.rect)
        .reduce(|a, b| Rect {
            left: a.left.max(b.left),
            top: a.top.max(b.top),
            right: a.right.min(b.right),
            bottom: a.bottom.min(b.bottom),
        })
        .map(|r| {
            xywh(Rect {
                right: r.right.max(r.left),
                bottom: r.bottom.max(r.top),
                ..r
            })
        })
}

fn under(path: &str, ancestor: &str) -> bool {
    path.strip_prefix(ancestor)
        .is_some_and(|rest| rest.starts_with('.'))
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn xywh(r: Rect) -> [i32; 4] {
    [r.left, r.top, r.right - r.left, r.bottom - r.top]
}

/// The texts of the descendants of raw node `i` in screen-reader order: a row's title, then its summary.
fn labels_of(raws: &[Raw], i: usize) -> Vec<String> {
    raws.iter()
        .filter(|r| under(&r.path, &raws[i].path))
        .map(|r| if r.text.is_empty() { &r.desc } else { &r.text })
        .filter(|t| !t.is_empty())
        .take(2)
        .cloned()
        .collect()
}

fn role(r: &Raw) -> &'static str {
    let class = r.class.rsplit('.').next().unwrap_or_default();
    match class {
        c if c.contains("Switch") => "switch",
        "CheckBox" | "CheckedTextView" => "checkbox",
        "ToggleButton" => "togglebutton",
        "RadioButton" => "radio",
        "Spinner" => "combobox",
        "SeekBar" | "RatingBar" => "slider",
        c if r.editable || c.contains("EditText") || c.contains("AutoCompleteTextView") => {
            "textbox"
        }
        "Button" | "ImageButton" => "button",
        _ if r.checkable => "checkbox",
        _ if r.clickable => "button",
        "ImageView" => "img",
        _ if r.scrollable => "list",
        _ => "text",
    }
}

async fn snapshot(android: &Android, a: SnapArgs) -> Result<String, String> {
    let started = Instant::now();
    let (frame, nodes) = tree(android, a.id.as_deref(), a.window.as_deref(), a.pid).await?;
    let mut out = json!({
        "window": frame.json(),
        "ms": ms(started.elapsed().as_micros()),
        "truncated": false,
    });
    if a.text {
        out["text"] = outline(&nodes, &frame.package, a.max_chars.unwrap_or(NAME_CHARS)).into();
    } else {
        out["nodes"] = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| n.json(i, &frame.package))
            .collect();
    }
    Ok(out.to_string())
}

fn outline(nodes: &[Node], app: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for n in nodes {
        let _ = write!(out, "{}- {}", "  ".repeat(n.depth), n.role);
        if !n.name.is_empty() {
            let _ = write!(out, " \"{}\"", quote(&n.name, max_chars));
        }
        for s in ["disabled", "checked"] {
            if n.states.contains(&s) {
                let _ = write!(out, " [{s}]");
            }
        }
        let _ = write!(out, " [id={}]", n.id(app));
        if let Some(v) = n.value.as_deref().filter(|v| !v.is_empty()) {
            let _ = write!(out, ": \"{}\"", quote(v, max_chars));
        }
        out.push('\n');
    }
    out
}

async fn find(android: &Android, a: FindArgs) -> Result<String, String> {
    let (frame, nodes) = tree(android, None, a.window.as_deref(), a.pid).await?;
    let found: Vec<Value> = matches(&nodes, &a.name, a.role.as_deref())
        .into_iter()
        .take(MAX_FOUND)
        .map(|i| nodes[i].json(i, &frame.package))
        .collect();
    let mut out = json!({"window": frame.json()});
    out["found"] = Value::Array(found);
    Ok(out.to_string())
}

/// The node of id `package:path:role:name` in the app's front window: at that path, else the first shown of that role and name.
async fn node(android: &Android, id: &str) -> Result<(Frame, Node), String> {
    let mut parts = id.splitn(4, ':');
    let (Some(app), Some(path), Some(role), Some(name)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(format!("bad element id {id}"));
    };
    let (frame, mut nodes) = app_tree(android, app).await?;
    let i = nodes
        .iter()
        .position(|n| n.path == path && n.role == role && n.name == name)
        .or_else(|| unique(&nodes, role, name))
        .ok_or_else(|| format!("element {id} is gone or not unique; read the window again"))?;
    Ok((frame, nodes.swap_remove(i)))
}

fn unique(nodes: &[Node], role: &str, name: &str) -> Option<usize> {
    let mut hits = (0..nodes.len()).filter(|&i| nodes[i].role == role && nodes[i].name == name);
    let first = hits.next()?;
    hits.next().is_none().then_some(first)
}

async fn read(android: &Android, a: ReadArgs) -> Result<String, String> {
    let (frame, mut node) = node(android, &a.id).await?;
    node.parent = -1;
    Ok(node.json(0, &frame.package).to_string())
}

/// Taps an element, after scrolling its list until it shows: the element is asked to scroll itself into view first, and swiped for when it does not move.
async fn press(android: &Android, id: &str) -> Result<String, String> {
    let (mut frame, mut node) = node(android, id).await?;
    let mut ask = true;
    for _ in 0..SCROLL_TRIES {
        if let Some((x, y)) = frame.tap_at(&node) {
            android.tap((x, y)).await?;
            return Ok(json!({"ok": true, "how": "click", "x": x, "y": y}).to_string());
        }
        let Some([sx, sy, sw, sh]) = node.scroller else {
            break;
        };
        if ask {
            ask = false;
            if android.show(&frame.id, &node.path).await {
                let (refreshed, mut nodes) = app_tree(android, &frame.package).await?;
                frame = refreshed;
                if let Some(i) = unique(&nodes, node.role, &node.name) {
                    let next = nodes.swap_remove(i);
                    let moved = next.bounds != node.bounds || frame.tap_at(&next).is_some();
                    node = next;
                    if moved {
                        continue;
                    }
                }
            }
        }
        let (cx, from, to) = frame
            .swipe([sx, sy, sw, sh], node.bounds[1])
            .ok_or_else(|| format!("element {id} is off the screen, and its list is covered"))?;
        android
            .input(&[
                "swipe",
                &cx.to_string(),
                &from.to_string(),
                &cx.to_string(),
                &to.to_string(),
                SCROLL_MS,
            ])
            .await?;
        tokio::time::sleep(SCROLL_SETTLE).await;
        let (refreshed, mut nodes) = app_tree(android, &frame.package).await?;
        frame = refreshed;
        let i = unique(&nodes, node.role, &node.name).ok_or_else(|| {
            format!("element {id} is gone or not unique after a scroll; read the window again")
        })?;
        node = nodes.swap_remove(i);
    }
    Err(format!("element {id} is off the screen"))
}

/// The innermost element at a screen point, with its ancestors.
async fn at(android: &Android, a: AtArgs) -> Result<String, String> {
    let (frame, nodes) = tree(android, None, a.window.as_deref(), a.pid).await?;
    let inside = |n: &Node| {
        let [x, y, w, h] = n.bounds;
        a.x >= x && a.x < x + w && a.y >= y && a.y < y + h
    };
    let Some((i, hit)) = nodes
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, n)| inside(n))
        .max_by_key(|(i, n)| (n.depth, *i))
    else {
        return Ok(json!({"found": null, "window": frame.json()}).to_string());
    };
    let mut path = Vec::new();
    let mut p = hit.parent;
    while p >= 0 {
        let n = &nodes[p as usize];
        path.push(json!({"id": n.id(&frame.package), "role": n.role, "name": n.name}));
        p = n.parent;
    }
    let mut out = json!({});
    out["found"] = hit.json(i, &frame.package);
    out["path"] = Value::Array(path);
    Ok(out.to_string())
}

/// Types text into the focused field: each line pasted, Return between lines.
async fn type_text(android: &Android, text: &str) -> Result<String, String> {
    let typed = android.type_text(text, None).await?;
    Ok(json!({"ok": true, "chars": text.chars().count(), "typed": typed}).to_string())
}

async fn click(android: &Android, a: ClickArgs) -> Result<String, String> {
    for _ in 0..a.count.unwrap_or(1).max(1) {
        android.tap((a.x as i32, a.y as i32)).await?;
    }
    Ok(json!({"ok": true}).to_string())
}

/// The window `window` names, which on Android is on the screen; a package name without a window starts its app.
async fn focus(android: &Android, a: WindowArgs) -> Result<String, String> {
    let frame = match window::pick(frames(android).await?, Some(&a.window), a.pid) {
        Ok(f) => f,
        Err(e) if is_package(&a.window) && e.starts_with("no window") => {
            android.launch(&a.window).await?;
            let end = Instant::now() + FOCUS_WAIT;
            loop {
                let frames = frames(android).await?;
                if let Some(f) = frames
                    .into_iter()
                    .find(|f| f.active && f.title.ends_with(&a.window))
                {
                    break f;
                }
                if Instant::now() > end {
                    return Err(format!("no window of {} after its launch", a.window));
                }
                tokio::time::sleep(FOCUS_POLL).await;
            }
        }
        Err(e) => return Err(e),
    };
    Ok(json!({"ok": true, "title": frame.title, "pid": frame.pid}).to_string())
}

/// A PNG of a screen rectangle, the whole screen by default.
async fn shot(android: &Android, a: ShotArgs) -> Result<Output, String> {
    let png = android.screenshot().await?;
    let (w, h) = android.screen().await?;
    let rect = [
        a.x.unwrap_or(0.0),
        a.y.unwrap_or(0.0),
        a.w.unwrap_or(f64::from(w)),
        a.h.unwrap_or(f64::from(h)),
    ];
    let (w, h, png) = match png_size(&png) {
        Some((iw, ih))
            if rect[0] <= 0.0
                && rect[1] <= 0.0
                && rect[0] + rect[2] >= f64::from(iw)
                && rect[1] + rect[3] >= f64::from(ih) =>
        {
            (iw, ih, png)
        }
        _ => crop(&png, rect)?,
    };
    let mut out = json!({"ok": true, "width": w, "height": h});
    if let Some(path) = a.path {
        write_bytes(std::path::Path::new(&path), &png).await?;
        out["path"] = path.into();
    }
    Ok(Output {
        text: out.to_string(),
        image_b64: Some(B64.encode(&png)),
        ..Output::default()
    })
}

/// The width and height a PNG's header gives.
fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    let size = |at: usize| Some(u32::from_be_bytes(png.get(at..at + 4)?.try_into().ok()?));
    png.starts_with(b"\x89PNG\r\n\x1a\n").then_some(())?;
    Some((size(16)?, size(20)?))
}

/// The RGB PNG of rectangle `[x, y, w, h]` of a PNG.
fn crop(png: &[u8], [x, y, w, h]: [f64; 4]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(png));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec
        .read_info()
        .map_err(|e| format!("decode screenshot: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("screenshot too large")?];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("decode screenshot: {e}"))?;
    let channels = info.color_type.samples();
    let clip = |v: f64, max: u32| v.floor().clamp(0.0, f64::from(max)) as u32;
    let (left, top) = (clip(x, info.width), clip(y, info.height));
    let (right, bottom) = (clip(x + w, info.width), clip(y + h, info.height));
    if right <= left || bottom <= top {
        return Err("the rectangle is off the screen".to_owned());
    }
    let (cw, ch) = (right - left, bottom - top);
    let mut rgb = Vec::with_capacity((cw * ch * 3) as usize);
    for row in top..bottom {
        for col in left..right {
            let at = (row as usize * info.width as usize + col as usize) * channels;
            rgb.extend_from_slice(&buf[at..at + 3]);
        }
    }
    Ok((cw, ch, png_rgb(cw, ch, &rgb)?))
}

/// Lists the windows until they differ from `since` or `limit` of wall time passes, list reads included, 5 s at most.
async fn wait_for<F, Fut>(since: Value, limit: Duration, mut read: F) -> Result<Value, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let started = tokio::time::Instant::now();
    let limit = limit.min(WAIT_MAX);
    let mut last = Duration::ZERO;
    loop {
        tokio::time::sleep(
            last.max(WAIT_POLL)
                .min(limit.saturating_sub(started.elapsed())),
        )
        .await;
        let at = tokio::time::Instant::now();
        let windows = read().await?;
        last = at.elapsed();
        let same = windows == since;
        if !same || started.elapsed() >= limit {
            let mut out = json!({"changed": !same});
            out["windows"] = windows;
            return Ok(out);
        }
    }
}

fn png_rgb(width: u32, height: u32, rgb: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width, height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .and_then(|mut w| w.write_image_data(rgb))
        .map_err(|e| format!("encode PNG: {e}"))?;
    Ok(out)
}

async fn write_bytes(path: &std::path::Path, data: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    tokio::fs::write(path, data)
        .await
        .map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REDROID: (i32, i32) = (720, 1280);
    const DUMP: &str = r#"<hierarchy><node text="" class="android.widget.FrameLayout" content-desc="" hint="" showing-hint="false" checkable="false" checked="false" clickable="false" long-clickable="false" editable="false" enabled="true" focused="false" selected="false" scrollable="false" visible="true" path="0" bounds="[0,0][720,1280]" /><node text="" class="android.widget.LinearLayout" content-desc="" hint="" showing-hint="false" checkable="true" checked="true" clickable="true" long-clickable="false" editable="false" enabled="true" focused="false" selected="false" scrollable="false" visible="true" path="0.0" bounds="[0,300][720,400]" /><node text="Dark theme" class="android.widget.TextView" content-desc="" hint="" showing-hint="false" checkable="false" checked="false" clickable="false" long-clickable="false" editable="false" enabled="true" focused="false" selected="false" scrollable="false" visible="true" path="0.0.0" bounds="[40,320][400,380]" /><node text="" class="android.widget.Switch" content-desc="" hint="" showing-hint="false" checkable="true" checked="true" clickable="false" long-clickable="false" editable="false" enabled="true" focused="false" selected="false" scrollable="false" visible="true" path="0.0.1" bounds="[600,320][680,380]" /><node text="Search…" class="android.widget.EditText" content-desc="" hint="Search…" showing-hint="true" checkable="false" checked="false" clickable="true" long-clickable="false" editable="true" enabled="false" focused="true" selected="false" scrollable="false" visible="true" path="0.1" bounds="[40,100][680,180]" /><node text="gone" class="android.widget.TextView" content-desc="" hint="" showing-hint="false" checkable="false" checked="false" clickable="false" long-clickable="false" editable="false" enabled="true" focused="false" selected="false" scrollable="false" visible="false" path="0.2" bounds="[0,0][0,0]" /></hierarchy>"#;

    fn window(bounds: [i32; 4]) -> WindowInfo {
        WindowInfo {
            id: 7,
            kind: APPLICATION,
            title: "Settings".into(),
            package: "com.android.settings".into(),
            active: true,
            focused: true,
            layer: 0,
            bounds,
        }
    }

    #[test]
    fn an_active_system_window_is_in_front_of_the_apps() {
        let app = |active| WindowInfo {
            active,
            ..window([0, 0, 720, 1280])
        };
        let shade = WindowInfo {
            id: 9,
            kind: 3,
            title: "Notification shade".into(),
            package: "com.android.systemui".into(),
            ..app(true)
        };
        let shown = listed(vec![app(false), shade]);
        assert_eq!(
            shown.iter().map(|w| (w.id, w.active)).collect::<Vec<_>>(),
            [(7, false), (9, true)]
        );
        let shown = listed(vec![app(false)]);
        assert!(
            shown[0].active,
            "without an active window the top app is in front"
        );
    }

    #[test]
    fn a_tap_under_the_navigation_or_status_bar_does_not_reach_the_app() {
        let frames = frames_of(
            vec![
                bar(5, 3, 2, [0, 1184, 720, 1280]),
                bar(1, 3, 1, [0, 0, 720, 48]),
                window([0, 0, 720, 1280]),
            ],
            REDROID,
        );
        assert_eq!(frames.len(), 1);
        let settings = &frames[0];
        assert_eq!(settings.covers, [[0, 1184, 720, 96], [0, 0, 720, 48]]);
        assert_eq!(settings.span(360, 700, 826), Some((700, 826)));
        assert_eq!(
            settings.span(360, 1224, 1248),
            None,
            "the navigation bar takes the tap"
        );
        assert_eq!(
            settings.span(360, 10, 40),
            None,
            "the status bar takes the tap"
        );
        assert_eq!(settings.span(360, 1300, 1340), None);
        assert_eq!(settings.span(800, 700, 826), None);
        assert_eq!(
            settings.span(360, 1144, 1224),
            Some((1144, 1184)),
            "a button partly under the bar is tapped in its open part"
        );
        assert_eq!(
            settings.span(360, 600, 1280),
            Some((600, 1184)),
            "a list's swipe stays above the bar"
        );
        let ime = bar(9, 2, 3, [0, 700, 720, 1184]);
        let typing = frames_of(
            vec![
                ime,
                bar(5, 3, 2, [0, 1184, 720, 1280]),
                window([0, 0, 720, 1280]),
            ],
            REDROID,
        );
        assert_eq!(
            typing[0].span(360, 760, 820),
            None,
            "the keyboard covers the field"
        );
        let closed = frames_of(
            vec![
                bar(5, 3, 2, [0, 1184, 720, 1280]),
                window([0, 0, 720, 1280]),
            ],
            REDROID,
        );
        assert_eq!(
            closed[0].span(360, 760, 820),
            Some((760, 820)),
            "a refreshed frame without the keyboard"
        );
    }

    #[test]
    fn a_dump_is_told_from_the_front_window_by_its_id() {
        let front = frames_of(vec![window([0, 0, 720, 1280])], REDROID).remove(0);
        assert_eq!(front.window(), "7");
        assert!(same_window(r#"<hierarchy window="7"><node />"#, &front));
        assert!(!same_window(r#"<hierarchy window="9"><node />"#, &front));
        assert!(same_window("<hierarchy><node />", &front));
        assert!(same_window(r#"<hierarchy window="9">"#, &Frame::default()));
    }

    #[test]
    fn a_saved_point_is_covered_once_the_keyboard_is_up() {
        let closed = frames_of(
            vec![
                bar(5, 3, 2, [0, 1184, 720, 1280]),
                window([0, 0, 720, 1280]),
            ],
            REDROID,
        );
        let typing = frames_of(
            vec![
                bar(9, 2, 3, [0, 700, 720, 1184]),
                bar(5, 3, 2, [0, 1184, 720, 1280]),
                window([0, 0, 720, 1280]),
            ],
            REDROID,
        );
        assert!(!closed[0].covered((360, 760)));
        assert!(typing[0].covered((360, 760)));
        assert!(typing[0].covered((360, 1200)));
        assert!(!typing[0].covered((360, 699)));
        assert!(!typing[0].covered((720, 760)));
    }

    #[test]
    fn a_list_swipe_runs_in_its_open_band_and_needs_room() {
        let phone = frames_of(
            vec![
                bar(5, 3, 2, [0, 1184, 720, 1280]),
                window([0, 0, 720, 1280]),
            ],
            REDROID,
        );
        assert_eq!(
            phone[0].swipe([0, 600, 720, 680], 1224),
            Some((360, 1038, 746))
        );
        assert_eq!(
            phone[0].swipe([0, 600, 720, 680], 610),
            Some((360, 746, 1038))
        );
        let typing = frames_of(
            vec![
                bar(9, 2, 3, [0, 0, 720, 1000]),
                bar(5, 3, 2, [0, 1008, 720, 1280]),
                window([0, 0, 720, 1280]),
            ],
            REDROID,
        );
        assert_eq!(typing[0].span(360, 600, 1280), Some((1000, 1008)));
        assert_eq!(
            typing[0].swipe([0, 600, 720, 680], 1224),
            None,
            "an 8 px band is no swipe"
        );
    }

    #[test]
    fn a_stale_element_matches_only_a_unique_role_and_name() {
        let nodes = nodes(&settings(), &raws(DUMP));
        assert_eq!(unique(&nodes, "textbox", "Search…"), Some(3));
        assert_eq!(unique(&nodes, "text", "Dark theme"), Some(2));
        assert_eq!(unique(&nodes, "button", "Search…"), None);
        let mut twice = nodes;
        twice.extend(super::nodes(&settings(), &raws(DUMP)));
        assert_eq!(unique(&twice, "text", "Dark theme"), None);
    }

    fn settings() -> Frame {
        frame(window([0, 0, 720, 1280]), false, REDROID)
    }

    fn bar(id: i64, kind: i64, layer: i64, bounds: [i32; 4]) -> WindowInfo {
        WindowInfo {
            id,
            kind,
            title: String::new(),
            package: "com.android.systemui".into(),
            active: false,
            focused: false,
            layer,
            bounds,
        }
    }

    #[test]
    fn a_window_is_titled_with_its_package() {
        let f = settings();
        assert_eq!(f.title, "Settings - com.android.settings");
        assert!(!f.dialog);
        assert!(f.pid >= PID_BASE);
        assert_eq!(f.pid, package_pid("com.android.settings"));
        assert!(frame(window([40, 400, 680, 900]), false, REDROID).dialog);
        assert!(!frame(window([40, 400, 680, 900]), true, REDROID).dialog);
    }

    #[test]
    fn nodes_list_the_row_as_one_named_checkbox_and_a_field_by_its_hint() {
        let nodes = nodes(&settings(), &raws(DUMP));
        let shown: Vec<(&str, &str, i64, usize, Vec<&str>)> = nodes
            .iter()
            .map(|n| (n.role, n.name.as_str(), n.parent, n.depth, n.states.clone()))
            .collect();
        assert_eq!(
            shown,
            [
                (
                    "frame",
                    "Settings - com.android.settings",
                    -1,
                    0,
                    vec!["showing", "active"]
                ),
                ("checkbox", "Dark theme", 0, 1, vec!["showing", "checked"]),
                ("text", "Dark theme", 1, 2, vec!["showing"]),
                (
                    "textbox",
                    "Search…",
                    0,
                    1,
                    vec!["showing", "disabled", "focused"]
                ),
                ("text", "gone", 0, 1, vec!["showing"]),
            ]
        );
        assert!(nodes[1].visible && !nodes[4].visible);
        assert_eq!(nodes[3].value.as_deref(), Some(""));
        assert_eq!(nodes[1].center(), (360, 350));
        assert_eq!(
            nodes[1].json(1, "com.android.settings")["id"],
            "com.android.settings:0.0:checkbox:Dark theme",
            "an element id is its app, path, role and name"
        );
    }

    #[test]
    fn computer_state_lists_the_shown_elements() {
        let lines: Vec<String> = elements(DUMP, &settings())
            .iter()
            .enumerate()
            .map(|(i, e)| e.line(i))
            .collect();
        assert_eq!(
            lines,
            [
                "[0] checkbox \"Dark theme\" at 360,350 checked",
                "[1] text \"Dark theme\" at 220,350",
                "[2] textbox \"Search…\" at 360,140 disabled focused",
            ]
        );
        let shown = elements(DUMP, &settings());
        assert!(shown[0].mentions("dark") && !shown[0].mentions("search"));
    }

    fn row(path: &str, scrollable: bool, bounds: &str) -> String {
        format!(
            r#"<node text="{path}" class="android.widget.TextView" content-desc="" hint="" showing-hint="false" checkable="false" checked="false" clickable="true" long-clickable="false" editable="false" enabled="true" focused="false" selected="false" scrollable="{scrollable}" visible="true" path="{path}" bounds="{bounds}" />"#
        )
    }

    /// A list at y 200..1184 under a toolbar, with a row cut by the toolbar edge, one in full and one below the list.
    fn list_dump() -> String {
        format!(
            "<hierarchy>{}{}{}{}</hierarchy>",
            row("0.0", true, "[0,200][720,1184]"),
            row("0.0.0", false, "[0,100][720,260]"),
            row("0.0.1", false, "[0,260][720,420]"),
            row("0.0.2", false, "[0,1200][720,1360]"),
        )
    }

    fn tap_of(frame: &Frame, dump: &str, name: &str) -> Option<(i32, i32)> {
        let nodes = nodes(frame, &raws(dump));
        frame.tap_at(nodes.iter().find(|n| n.name == name).unwrap())
    }

    #[test]
    fn a_tap_on_a_row_under_the_toolbar_lands_in_its_shown_part() {
        let frame = settings();
        let dump = list_dump();
        assert_eq!(
            tap_of(&frame, &dump, "0.0.0"),
            Some((360, 230)),
            "the raw centre y 180 is the toolbar's"
        );
        assert_eq!(
            tap_of(&frame, &dump, "0.0.1"),
            Some((360, 340)),
            "a row in full is tapped at its centre"
        );
        assert_eq!(
            tap_of(&frame, &dump, "0.0.2"),
            None,
            "a row clipped away is off the screen"
        );
    }

    #[test]
    fn computer_state_points_into_the_shown_part_and_drops_clipped_rows() {
        let lines: Vec<String> = elements(&list_dump(), &settings())
            .iter()
            .enumerate()
            .map(|(i, e)| e.line(i))
            .collect();
        assert_eq!(
            lines,
            [
                "[0] button \"0.0\" at 360,692",
                "[1] button \"0.0.0\" at 360,230",
                "[2] button \"0.0.1\" at 360,340",
            ]
        );
    }

    #[test]
    fn a_row_in_a_nested_list_is_cut_to_every_viewport() {
        let dump = format!(
            "<hierarchy>{}{}{}{}</hierarchy>",
            row("0.0", true, "[0,160][720,1000]"),
            row("0.0.0", true, "[0,900][720,1100]"),
            row("0.0.0.0", false, "[100,950][300,1050]"),
            row("0.0.0.1", false, "[100,1010][300,1090]"),
        );
        let frame = settings();
        assert_eq!(
            tap_of(&frame, &dump, "0.0.0.0"),
            Some((200, 975)),
            "the outer list ends above the row's lower half"
        );
        assert_eq!(
            tap_of(&frame, &dump, "0.0.0.1"),
            None,
            "a row below the outer viewport is off the screen"
        );
    }

    #[test]
    fn computer_state_points_above_the_navigation_bar_and_the_keyboard() {
        let dump = format!(
            "<hierarchy>{}{}{}{}</hierarchy>",
            row("0.0", true, "[0,160][720,1280]"),
            row("0.0.0", false, "[0,1134][720,1280]"),
            row("0.0.1", false, "[0,1190][720,1280]"),
            row("0.0.2", false, "[0,600][720,700]"),
        );
        let lines = |frames: Vec<Frame>| -> Vec<String> {
            elements(&dump, &frames[0])
                .iter()
                .enumerate()
                .map(|(i, e)| e.line(i))
                .collect()
        };
        let closed = lines(frames_of(
            vec![
                bar(5, 3, 2, [0, 1184, 720, 1280]),
                window([0, 0, 720, 1280]),
            ],
            REDROID,
        ));
        assert_eq!(
            closed,
            [
                "[0] button \"0.0\" at 360,672",
                "[1] button \"0.0.0\" at 360,1159",
                "[2] button \"0.0.2\" at 360,650",
            ],
            "a row cut by the navigation bar is tapped above it; one under it is gone"
        );
        let typing = lines(frames_of(
            vec![
                bar(9, 2, 3, [0, 640, 720, 1184]),
                bar(5, 3, 2, [0, 1184, 720, 1280]),
                window([0, 0, 720, 1280]),
            ],
            REDROID,
        ));
        assert_eq!(
            typing,
            [
                "[0] button \"0.0\" at 360,400",
                "[1] button \"0.0.2\" at 360,620",
            ],
            "the keyboard takes the lower rows and cuts the list's reach"
        );
    }

    #[test]
    fn a_row_without_a_list_is_cut_to_the_screen_only() {
        let dump = format!(
            "<hierarchy>{}{}</hierarchy>",
            row("0.0", false, "[0,1200][720,1360]"),
            row("0.1", false, "[0,1300][720,1400]"),
        );
        let frame = settings();
        assert_eq!(tap_of(&frame, &dump, "0.0"), Some((360, 1240)));
        assert_eq!(tap_of(&frame, &dump, "0.1"), None);
    }

    #[test]
    fn bounds_and_entities_parse() {
        assert_eq!(
            bounds("[1,2][3,4]"),
            Some(Rect {
                left: 1,
                top: 2,
                right: 3,
                bottom: 4
            })
        );
        assert_eq!(bounds("[1,2]"), None);
        assert_eq!(
            unescape("a&lt;b&gt;&quot;&apos;&#20320;&#x597D;&bogus"),
            "a<b>\"'你好&bogus"
        );
    }

    #[test]
    fn crop_cuts_an_rgb_rectangle() {
        let rgba: Vec<u8> = (0..4 * 3 * 2).map(|i| i as u8).collect();
        let mut src = Vec::new();
        let mut enc = png::Encoder::new(&mut src, 3, 2);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&rgba).unwrap();
        let (w, h, out) = crop(&src, [1.0, 1.0, 5.0, 5.0]).unwrap();
        assert_eq!((w, h), (2, 1));
        let mut dec = png::Decoder::new(std::io::Cursor::new(out))
            .read_info()
            .unwrap();
        let mut buf = vec![0; dec.output_buffer_size().unwrap()];
        dec.next_frame(&mut buf).unwrap();
        assert_eq!(&buf[..6], &[16, 17, 18, 20, 21, 22]);
        assert!(crop(&src, [9.0, 9.0, 1.0, 1.0]).is_err());
    }

    #[test]
    fn a_png_header_gives_its_size_and_crop_reads_rgb_and_rgba_alike() {
        let encode = |color, channels: usize| {
            let data: Vec<u8> = (0..3 * 2 * channels).map(|i| i as u8 + 1).collect();
            let mut out = Vec::new();
            let mut enc = png::Encoder::new(&mut out, 3, 2);
            enc.set_color(color);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&data).unwrap();
            out
        };
        let rgba = encode(png::ColorType::Rgba, 4);
        let rgb = encode(png::ColorType::Rgb, 3);
        assert_eq!(png_size(&rgba), Some((3, 2)));
        assert_eq!(png_size(&rgb), Some((3, 2)));
        assert_eq!(png_size(b"not a png at all, but long enough"), None);
        let pixels = |png: &[u8]| {
            let (_, _, cut) = crop(png, [0.0, 0.0, 3.0, 2.0]).unwrap();
            let mut dec = png::Decoder::new(std::io::Cursor::new(cut))
                .read_info()
                .unwrap();
            let mut buf = vec![0; dec.output_buffer_size().unwrap()];
            dec.next_frame(&mut buf).unwrap();
            buf
        };
        let from_rgba: Vec<u8> = (0..6)
            .flat_map(|i| [i * 4 + 1, i * 4 + 2, i * 4 + 3])
            .collect();
        assert_eq!(pixels(&rgba), from_rgba);
        assert_eq!(pixels(&rgb), (1..=18).collect::<Vec<u8>>());
    }

    #[tokio::test(start_paused = true)]
    async fn a_zero_timeout_reads_the_list_once_at_once() {
        let since = json!([{"title": "Settings"}]);
        let lists = std::cell::Cell::new(0);
        let started = tokio::time::Instant::now();
        let out = wait_for(since.clone(), Duration::ZERO, || {
            lists.set(lists.get() + 1);
            let since = since.clone();
            async move { Ok(since) }
        })
        .await
        .unwrap();
        assert_eq!(out["changed"], false);
        assert_eq!(lists.get(), 1);
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn after_a_slow_first_read_the_last_read_starts_at_the_deadline() {
        let since = json!([{"title": "Settings"}]);
        let lists = std::cell::Cell::new(0);
        let started = tokio::time::Instant::now();
        let out = wait_for(since.clone(), Duration::from_millis(150), || {
            lists.set(lists.get() + 1);
            let slow = lists.get() == 1;
            let since = since.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(if slow { 100 } else { 1 })).await;
                Ok(since)
            }
        })
        .await
        .unwrap();
        assert_eq!(out["changed"], false);
        assert_eq!(lists.get(), 2);
        assert_eq!(started.elapsed(), Duration::from_millis(151));
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_whose_lists_take_a_millisecond_ends_at_its_timeout_in_wall_time() {
        let since = json!([{"title": "Settings"}]);
        let started = tokio::time::Instant::now();
        let out = wait_for(since.clone(), Duration::from_millis(480), || {
            let since = since.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(1)).await;
                Ok(since)
            }
        })
        .await
        .unwrap();
        let t = started.elapsed();
        assert_eq!(out["changed"], false);
        assert!(
            t >= Duration::from_millis(480) && t <= Duration::from_millis(491),
            "{t:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_cap_holds_for_a_longer_timeout() {
        let since = json!([]);
        let started = tokio::time::Instant::now();
        let out = wait_for(since.clone(), Duration::from_secs(60), || {
            let since = since.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(400)).await;
                Ok(since)
            }
        })
        .await
        .unwrap();
        let t = started.elapsed();
        assert_eq!(out["changed"], false);
        assert!(
            t >= WAIT_MAX && t < WAIT_MAX + Duration::from_millis(401),
            "{t:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_first_read_longer_than_the_timeout_is_the_only_read() {
        let since = json!([{"title": "Settings"}]);
        let lists = std::cell::Cell::new(0);
        let started = tokio::time::Instant::now();
        let out = wait_for(since.clone(), Duration::from_millis(150), || {
            lists.set(lists.get() + 1);
            let since = since.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                Ok(since)
            }
        })
        .await
        .unwrap();
        assert_eq!(out["changed"], false);
        assert_eq!(lists.get(), 1);
        assert_eq!(started.elapsed(), Duration::from_millis(310));
    }

    #[tokio::test(start_paused = true)]
    async fn a_change_on_the_read_at_the_deadline_is_reported() {
        let since = json!([{"title": "Settings"}]);
        let after = json!([{"title": "Settings"}, {"title": "Wi-Fi"}]);
        let started = tokio::time::Instant::now();
        let out = wait_for(since.clone(), Duration::from_millis(150), || {
            let list = if started.elapsed() >= Duration::from_millis(150) {
                after.clone()
            } else {
                since.clone()
            };
            async move {
                tokio::time::sleep(Duration::from_millis(1)).await;
                Ok(list)
            }
        })
        .await
        .unwrap();
        assert_eq!(out["changed"], true);
        assert_eq!(out["windows"], after);
        assert_eq!(started.elapsed(), Duration::from_millis(151));
    }
}
