//! The desk calls: snapshots and element actions through the OS backend; input, windows, launch and capture through cua-driver.

use std::cell::Cell;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use desk_core::args::{
    ActArgs, AtArgs, ClickArgs, DragArgs, FindArgs, IdArgs, KeyArgs, LaunchArgs, ReadArgs,
    ScrollArgs, ShotArgs, SnapArgs, StillArgs, TypeArgs, WaitArgs, WindowArgs, parse,
};
use desk_core::text::{MAX_FOUND, NAME_CHARS, matches, ms, quote};
use desk_core::window::{TITLE_TIERS, title_matches};
use desk_core::wire::Output;
use serde_json::{Map, Value, json};

use crate::allow::{Allow, app_key, app_of};
use crate::auto;
use crate::driver::{
    CAPTURE_SESSION, Input, WINDOW_SESSION, background, desktop, image_of, text_of,
};
use crate::keys::{Os, key_names, keystroke};
use crate::model::{Access, Act, Capture, Extras, Frame, Node};
use crate::press::{Press, plan_press};
use crate::shot::{crop_png, rgb_png};
use crate::still::{Frame as StillFrame, Watch};
use crate::worker::Worker;

pub const TURN_WAIT: Duration = Duration::from_secs(15);
/// The verbs a vision agent drives the screen with, which the Linux X11 desk answers.
pub const COMPUTER_VERBS: [&str; 6] = [
    "desk_computer_state",
    "desk_computer_act",
    "desk_computer_end",
    "desk_computer_scale",
    "desk_look",
    "desk_title_at",
];
pub const ACTIONS: [&str; 10] = [
    "desk_press",
    "desk_act",
    "desk_key",
    "desk_type",
    "desk_click",
    "desk_scroll",
    "desk_drag",
    "desk_focus",
    "desk_close",
    "desk_launch",
];

const CALL_LIMIT: Duration = Duration::from_secs(3);
const TREE_LIMIT: Duration = Duration::from_secs(10);
const READ_CHARS: usize = 64 << 10;
const WAIT_MAX: Duration = Duration::from_secs(5);
const WAIT_POLL: Duration = Duration::from_millis(10);
const WAIT_CHECK: Duration = Duration::from_millis(100);
const FRAME_BORDER: f64 = 16.0;
const NO_WINDOW_UNDER: &str =
    "no window under the point, and the app allowlist admits only windows";
const NO_WINDOWSERVER_CONNECTION: &str = "SLPSPostEventRecordTo returned 1002";
const WINDOW_NEVER_READ: &str = "window_never_read";
pub(crate) const BACKGROUND_TARGET: &str =
    "background input needs its target window: pass window or pid";
pub(crate) const BACKGROUND_DESKTOP: &str = "background input: this window takes no pointer input without the foreground, and background input never brings a window to the front";
pub(crate) const BACKGROUND_NO_WINDOW: &str = "background input: no window under the point";
pub(crate) const BACKGROUND_ELEMENT: &str = "background input: the element's own window could not be found, and background input never clicks a window that covers it";
pub(crate) const BACKGROUND_TWINS: &str = "background input: the element's own window is ambiguous (same or no title, and no single frame match), and background input never guesses which window to click";
pub(crate) const BACKGROUND_UIA: &str = "background input: on Windows UI Automation focuses, selects or runs an element only by bringing its window to the front; click it in its window, or use input = \"foreground\"";
const ANOTHER_SPACE: &str = "accessibility does not reach it there, so a snapshot or an element action needs it on the current Space; background key, type, click, scroll and drag reach it by its title or pid";
const OFF_SPACE: &str = "the window is on another Space, where its capture may be stale: read it back on its Space before sending the input again";
const SCROLL_POINT: &str = "scroll needs both x and y, or neither";
const OK: &str = r#"{"ok":true}"#;
const SCROLL_AMOUNT: u32 = 3;
const STILL_TIMEOUT: Duration = Duration::from_millis(600);
const STILL_MAX: Duration = Duration::from_secs(5);
const STILL_HOLD: Duration = Duration::from_millis(40);
const STILL_POLL: Duration = Duration::from_millis(10);
const STILL_EDGE: u32 = 320;
const RUN_CHARS: usize = 8;
const TYPE_SETTLE: Duration = Duration::from_millis(100);
const TYPE_POLL: Duration = Duration::from_millis(10);
const TYPE_TAIL: usize = 16;
const MAC_TYPE_DELAY_MS: u64 = 0;
#[cfg(target_os = "linux")]
pub(crate) const TYPER: Typer = crate::xtest::type_mapped;
#[cfg(not(target_os = "linux"))]
pub(crate) const TYPER: Typer = |_, _| Ok(false);

tokio::task_local! {
    static FOREGROUND: bool;
    static SENT: Cell<usize>;
}

pub(crate) type Turn = Arc<tokio::sync::OwnedMutexGuard<()>>;
/// Types a line on the core keyboard: true when typed, false when it typed nothing and leaves the line to the driver; it stops once the flag is set.
pub(crate) type Typer = fn(&str, &AtomicBool) -> Result<bool, String>;

/// The desk of one OS: `B` reads and acts on elements, `D` sends input.
pub struct Desk<B, D> {
    access: Worker<B>,
    driver: D,
    os: Os,
    acting: Arc<tokio::sync::Mutex<()>>,
    turn_wait: Duration,
    read: std::sync::Mutex<Option<Read>>,
    allow: Allow,
    overlay: bool,
    background: bool,
    auto: bool,
    off_space: std::sync::Mutex<HashSet<(i64, i64)>>,
    typer: Typer,
    #[cfg(test)]
    skew: Arc<std::sync::atomic::AtomicU64>,
}

impl<B: Access + 'static, D: Input + Sync> Desk<B, D> {
    /// A desk whose backend `open` makes on its worker thread.
    pub fn new(open: fn() -> Result<B, String>, driver: D, os: Os) -> Self {
        Self {
            access: Worker::new(open),
            driver,
            os,
            acting: Arc::new(tokio::sync::Mutex::new(())),
            turn_wait: TURN_WAIT,
            read: std::sync::Mutex::new(None),
            allow: Allow::default(),
            overlay: true,
            background: false,
            auto: false,
            off_space: std::sync::Mutex::new(HashSet::new()),
            typer: TYPER,
            #[cfg(test)]
            skew: Arc::default(),
        }
    }

    #[cfg(test)]
    fn with_typer(mut self, typer: Typer) -> Self {
        self.typer = typer;
        self
    }

    /// Limits the desk calls to windows of `apps`, matched by app name or executable, case-insensitive; empty allows every app.
    pub fn allow(mut self, apps: &[String]) -> Self {
        self.allow = Allow(apps.iter().map(|a| app_key(a)).collect());
        self
    }

    /// Whether the driver may show its cursor overlay, which a shot read by the backend must leave out; true unless the driver runs without it.
    pub fn overlay(mut self, shown: bool) -> Self {
        self.overlay = shown;
        self
    }

    /// Whether input goes to its target window without raising it or moving the pointer, refusing what cannot.
    pub fn background(mut self, on: bool) -> Self {
        self.background = on;
        self
    }

    /// Whether a background action refused before any input was sent runs once more in the foreground, its target window brought to the front.
    pub fn auto(mut self, on: bool) -> Self {
        self.auto = on;
        self
    }

    /// Runs desk call `tool`, a `desk_<verb>`; actions run one at a time, until their backend calls end, reads alongside them.
    pub async fn run(&self, tool: &str, args: Value) -> Result<Output, String> {
        let turn = if ACTIONS.contains(&tool) {
            let turn = tokio::time::timeout(self.turn_wait, self.acting.clone().lock_owned())
                .await
                .map_err(|_| "desk busy: a previous action is still running".to_owned())?;
            Some(Arc::new(turn))
        } else {
            None
        };
        if !self.allow.is_open() {
            self.admit(tool, &args).await?;
        }
        if self.auto && auto::VERBS.contains(&tool) {
            return self.auto_run(tool, args, turn).await;
        }
        self.dispatch(tool, args, turn).await
    }

    async fn auto_run(
        &self,
        tool: &str,
        args: Value,
        turn: Option<Turn>,
    ) -> Result<Output, String> {
        if !(tool == "desk_drag" && self.os == Os::Windows) {
            let tried = SENT.scope(Cell::new(0), async {
                let out = self.dispatch(tool, args.clone(), turn.clone()).await;
                (out, SENT.with(Cell::get))
            });
            match tried.await {
                (Err(e), 0) if auto::pre_send(tool, self.os, &args, &e) => {}
                (out, _) => return out,
            }
        }
        self.front_target(tool, &args).await?;
        let mut out = FOREGROUND
            .scope(true, self.dispatch(tool, args, turn))
            .await?;
        if let Ok(mut v) = serde_json::from_str::<Value>(&out.text)
            && v.is_object()
        {
            v["input"] = "foreground".into();
            out.text = v.to_string();
        }
        Ok(out)
    }

    /// Brings the window of an auto action to the front before the foreground runs it: the named window, or an element's own.
    async fn front_target(&self, tool: &str, args: &Value) -> Result<(), String> {
        let (window, pid) = if let Some(id) = args["id"]
            .as_str()
            .filter(|_| tool == "desk_press" || tool == "desk_act")
        {
            let id = id.to_owned();
            self.access
                .run(CALL_LIMIT, (), move |b| {
                    let pid = b.pid_of(&id)?;
                    let root = b.ancestors(&id)?.into_iter().next().unwrap_or(id);
                    let title = pid
                        .map(|p| b.frames_of(p))
                        .transpose()?
                        .and_then(|fs| fs.into_iter().find(|f| f.id == root))
                        .map(|f| f.title);
                    Ok((title, pid))
                })
                .await?
        } else {
            let pid = args["pid"].as_u64().and_then(|p| u32::try_from(p).ok());
            (args["window"].as_str().map(str::to_owned), pid)
        };
        if window.is_none() && pid.is_none() {
            return Ok(());
        }
        self.focus(WindowArgs {
            window: window.unwrap_or_default(),
            pid,
        })
        .await
        .map(drop)
    }

    async fn dispatch(
        &self,
        tool: &str,
        args: Value,
        turn: Option<Turn>,
    ) -> Result<Output, String> {
        let text = match tool {
            "desk_windows" => self.windows().await?,
            "desk_wait" => self.wait(parse(args)?).await?,
            "desk_snapshot" => self.snapshot(parse(args)?).await?,
            "desk_find" => self.find(parse(args)?).await?,
            "desk_read" => self.read(parse(args)?).await?,
            "desk_press" => self.press(parse::<IdArgs>(args)?.id, turn.clone()).await?,
            "desk_act" => self.act(parse(args)?, turn.clone()).await?,
            "desk_at" => self.at(parse(args)?).await?,
            "desk_key" => self.key(parse(args)?).await?,
            "desk_type" => self.type_text(parse(args)?, turn.clone()).await?,
            "desk_click" => self.click(parse(args)?).await?,
            "desk_scroll" => self.scroll(parse(args)?).await?,
            "desk_drag" => self.drag(parse(args)?).await?,
            "desk_still" => self.still(parse(args)?).await?,
            "desk_focus" => self.focus(parse(args)?).await?,
            "desk_close" => self.close(parse(args)?, turn.clone()).await?,
            "desk_launch" => {
                let app = parse::<LaunchArgs>(args)?.app;
                let v = self.call("launch_app", json!({"name": app})).await?;
                let mut out = json!({"ok": true, "launched": text_of(&v)});
                if self.os == Os::Mac {
                    out["background"] = true.into();
                }
                if self.os == Os::Linux {
                    out["scoped"] = adopt(v).await.into();
                }
                out.to_string()
            }
            "desk_shot" => return self.shot(parse(args)?).await,
            "desk_screen" => {
                let mut v = self.call("get_screen_size", json!({})).await?;
                let mut size = v["structuredContent"].take();
                if self.os == Os::Mac
                    && let Some(k) = size["scale_factor"].as_f64().filter(|k| *k > 0.0)
                {
                    for side in ["width", "height"] {
                        if let Some(n) = size[side].as_f64() {
                            size[side] = ((n * k).round() as i64).into();
                        }
                    }
                }
                size.to_string()
            }
            t if COMPUTER_VERBS.contains(&t) => {
                return Err(format!("{t}: this desk answers no computer verbs"));
            }
            other => return Err(format!("unknown desk tool {other}")),
        };
        Ok(Output::text(text))
    }

    fn bg(&self) -> bool {
        self.background && !FOREGROUND.try_with(|f| *f).unwrap_or(false)
    }

    async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
        let out = self.driver.call(tool, args).await?;
        if auto::INPUT_TOOLS.contains(&tool) {
            let _ = SENT.try_with(|n| n.set(n.get() + 1));
        }
        Ok(out)
    }

    async fn windows(&self) -> Result<String, String> {
        let frames = self.listed().await?;
        serde_json::to_string(&frames).map_err(|e| format!("encode windows: {e}"))
    }

    async fn listed(&self) -> Result<Vec<Frame>, String> {
        let (allow, background) = (self.allow.clone(), self.bg());
        self.access
            .run(CALL_LIMIT, (), move |b| {
                let mut frames = b.frames()?;
                if background {
                    frames.extend(b.off_space());
                }
                frames.retain(|f| allow.admits(f.app.as_deref()));
                frames.sort_by_key(|f| !f.active);
                let modals = b.modals();
                let shells = b.shells();
                for f in &mut frames {
                    f.shell = f.pid.is_some_and(|p| shells.contains(&p));
                    if let Some(m) = modals.iter().find(|m| m.title == f.title && m.pid == f.pid) {
                        f.modal = Some(m.place.clone());
                        f.blocks = Some(m.owner.clone());
                    }
                }
                Ok(frames)
            })
            .await
    }

    /// Waits until the window list differs from `since` or `timeout_ms` of wall time passes, list reads included.
    async fn wait(&self, a: WaitArgs) -> Result<String, String> {
        let limit = Duration::from_millis(a.timeout_ms).min(WAIT_MAX);
        let since = match a.since {
            Value::Array(ws) => Some(
                ws.into_iter()
                    .filter(|w| self.allow.admits(w["app"].as_str()))
                    .collect::<Value>(),
            ),
            _ => None,
        };
        let started = self.now();
        let mut last = Duration::ZERO;
        let mut hint = self.access.run(CALL_LIMIT, (), |b| Ok(b.hint())).await?;
        let mut checked = self.now();
        loop {
            tokio::time::sleep(WAIT_POLL.min(limit.saturating_sub(self.now() - started))).await;
            let now = self.access.run(CALL_LIMIT, (), |b| Ok(b.hint())).await?;
            let over = self.now() - started >= limit;
            let moved = now.is_some() && now != hint;
            let due = self.now() - checked
                >= if now.is_some() {
                    last.max(WAIT_CHECK)
                } else {
                    last
                };
            if !over && !moved && !due {
                continue;
            }
            hint = now;
            let at = self.now();
            let frames = self.listed().await?;
            last = self.now() - at;
            checked = self.now();
            let windows =
                serde_json::to_value(&frames).map_err(|e| format!("encode windows: {e}"))?;
            let same = since.as_ref() == Some(&windows);
            if !same || self.now() - started >= limit {
                let mut out = json!({"changed": !same});
                out["windows"] = windows;
                return Ok(out.to_string());
            }
        }
    }

    #[cfg(not(test))]
    fn now(&self) -> Instant {
        Instant::now()
    }

    #[cfg(test)]
    fn now(&self) -> Instant {
        Instant::now() + Duration::from_millis(self.skew.load(std::sync::atomic::Ordering::Relaxed))
    }

    async fn admit(&self, tool: &str, args: &Value) -> Result<(), String> {
        let id = match tool {
            "desk_launch" => return self.allow.check(args["app"].as_str()),
            "desk_read" | "desk_press" | "desk_act" => {
                Some(args["id"].as_str().ok_or("no element id")?.to_owned())
            }
            "desk_key" | "desk_type" => None,
            _ => return Ok(()),
        };
        let allow = self.allow.clone();
        let named = self.bg().then(|| {
            (
                args["window"].as_str().map(str::to_owned),
                args["pid"].as_u64(),
            )
        });
        self.access
            .run(CALL_LIMIT, (), move |b| match id {
                Some(id) => {
                    let pid = b.pid_of(&id)?;
                    check_process(b, &allow, pid)
                }
                None => {
                    let (window, pid) = named.unwrap_or_default();
                    let pid = pid.and_then(|p| u32::try_from(p).ok());
                    allow.pick(b.frames()?, window.as_deref(), pid).map(drop)
                }
            })
            .await
    }

    async fn snapshot(&self, a: SnapArgs) -> Result<String, String> {
        let started = Instant::now();
        let extras = Extras {
            values: true,
            bounds: a.bounds.unwrap_or(true),
            keys: a.keys,
            menus: a.menus,
            frame: false,
        };
        let (window, pid, id) = (a.window, a.pid, a.id);
        let (allow, background) = (self.allow.clone(), self.bg() && !self.auto);
        let (tree, found) = self
            .access
            .run(TREE_LIMIT, (), move |b| {
                let listed = frames(b, pid, &allow)?;
                let frame = live(match id {
                    Some(id) => {
                        let Some(f) = listed.into_iter().find(|f| f.id == id) else {
                            return Err(away(b, background, |f| f.id == id)
                                .unwrap_or(format!("no window {id}")));
                        };
                        allow.check(f.app.as_deref())?;
                        f
                    }
                    None => pick(b, &allow, background, listed, window.as_deref(), pid)?,
                })?;
                let found = started.elapsed().as_micros();
                Ok((b.tree(&frame, extras)?, found))
            })
            .await?;
        let mut phases = Map::new();
        phases.insert("frames".into(), ms(found));
        for (name, us) in &tree.phases {
            phases.insert((*name).into(), ms(*us));
        }
        let mut out = json!({
            "window": tree.frame,
            "ms": ms(started.elapsed().as_micros()),
            "truncated": tree.truncated,
        });
        out["phases"] = Value::Object(phases);
        if a.text {
            out["text"] = outline(&tree.nodes, a.max_chars.unwrap_or(NAME_CHARS)).into();
        } else {
            out["nodes"] = tree
                .nodes
                .iter()
                .enumerate()
                .map(|(i, n)| n.json(i))
                .collect();
        }
        Ok(out.to_string())
    }

    async fn find(&self, a: FindArgs) -> Result<String, String> {
        let extras = Extras {
            values: true,
            bounds: true,
            keys: false,
            menus: true,
            frame: false,
        };
        let (window, pid) = (a.window, a.pid);
        let (allow, background) = (self.allow.clone(), self.bg() && !self.auto);
        let tree = self
            .access
            .run(TREE_LIMIT, (), move |b| {
                let listed = frames(b, pid, &allow)?;
                let frame = live(pick(b, &allow, background, listed, window.as_deref(), pid)?)?;
                b.tree(&frame, extras)
            })
            .await?;
        let found: Vec<Value> = matches(&tree.nodes, &a.name, a.role.as_deref())
            .into_iter()
            .take(MAX_FOUND)
            .map(|i| tree.nodes[i].json(i))
            .collect();
        let mut out = json!({"window": tree.frame});
        out["found"] = Value::Array(found);
        Ok(out.to_string())
    }

    async fn read(&self, a: ReadArgs) -> Result<String, String> {
        let extras = Extras {
            values: true,
            bounds: true,
            keys: true,
            menus: false,
            frame: false,
        };
        let chars = if a.text { READ_CHARS } else { NAME_CHARS };
        let node = self
            .access
            .run(CALL_LIMIT, (), move |b| b.node(&a.id, extras, chars))
            .await?;
        Ok(node.json(0).to_string())
    }

    async fn press(&self, id: String, turn: Option<Turn>) -> Result<String, String> {
        let (os, background) = (self.os, self.bg());
        let pressed = id.clone();
        match self
            .access
            .run(TREE_LIMIT, turn, move |b| {
                plan_press(b, &pressed, os, background)
            })
            .await?
        {
            Press::Selected => Ok(json!({"ok": true, "how": "select"}).to_string()),
            Press::Acted(ok) => Ok(json!({"ok": ok, "how": "action"}).to_string()),
            Press::Click(x, y) => {
                let note = self.click_element(id, json!({"x": x, "y": y})).await?;
                let mut out = json!({"ok": true, "how": "click", "x": x, "y": y});
                if let Some(note) = note {
                    out["unverified"] = note.into();
                }
                Ok(out.to_string())
            }
        }
    }

    async fn act(&self, a: ActArgs, turn: Option<Turn>) -> Result<String, String> {
        let act = match a.action.as_str() {
            "focus" | "select" | "action" if self.bg() && self.os == Os::Windows => {
                return Err(BACKGROUND_UIA.to_owned());
            }
            "action" => Act::Action,
            "select" => Act::Select,
            "focus" => Act::Focus,
            "click" => {
                let id = a.id.clone();
                let n = self
                    .access
                    .run(CALL_LIMIT, turn, move |b| b.node(&id, Extras::BOUNDS, 0))
                    .await?;
                let [x, y, w, h] = n.bounds.ok_or("the element has no screen position")?;
                let note = self
                    .click_element(a.id, json!({"x": x + w / 2, "y": y + h / 2}))
                    .await?;
                return Ok(ok_with(note));
            }
            other => {
                return Err(format!(
                    "unknown action {other}; use click, action, select or focus"
                ));
            }
        };
        let ok = self
            .access
            .run(CALL_LIMIT, turn, move |b| b.act(&a.id, act))
            .await?;
        Ok(json!({"ok": ok}).to_string())
    }

    async fn at(&self, a: AtArgs) -> Result<String, String> {
        let (allow, background) = (self.allow.clone(), self.bg() && !self.auto);
        self.access
            .run(CALL_LIMIT, (), move |b| {
                let frames = frames(b, a.pid, &allow)?;
                let kept = (!allow.is_open()).then(|| frames.clone());
                let frame = live(pick(
                    b,
                    &allow,
                    background,
                    frames,
                    a.window.as_deref(),
                    a.pid,
                )?)?;
                let Some(hit) = b.at(&frame.id, a.x, a.y)? else {
                    return Ok(json!({"found": null, "window": frame}).to_string());
                };
                if let Some(frames) = &kept {
                    let pid = b.pid_of(&hit)?;
                    allow.check(app_of(frames, pid))?;
                }
                let node = b.node(&hit, Extras::default(), 0)?;
                let path: Vec<Value> = b
                    .ancestors(&hit)?
                    .into_iter()
                    .filter_map(|id| {
                        let n = b.node(&id, Extras::default(), 0).ok()?;
                        Some(json!({"id": id, "role": n.role, "name": n.name}))
                    })
                    .collect();
                let mut out = json!({});
                out["found"] = node.json(0);
                out["path"] = Value::Array(path);
                Ok(out.to_string())
            })
            .await
    }

    async fn close(&self, a: WindowArgs, turn: Option<Turn>) -> Result<String, String> {
        let (allow, background) = (self.allow.clone(), self.bg() && !self.auto);
        self.access
            .run(CALL_LIMIT, turn, move |b| {
                let listed = frames(b, a.pid, &allow)?;
                let frame = live(pick(b, &allow, background, listed, Some(&a.window), a.pid)?)?;
                b.close(&frame)?;
                Ok(json!({"ok": true, "title": frame.title}).to_string())
            })
            .await
    }

    async fn key(&self, a: KeyArgs) -> Result<String, String> {
        let (tool, req) = key_request(key_names(&a.keys, self.os)?);
        let to = self.target(a.window.as_deref(), a.pid).await?;
        let mut note = None;
        self.send(tool, req, to, &mut note).await?;
        Ok(ok_with(note))
    }

    async fn type_text(&self, a: TypeArgs, turn: Option<Turn>) -> Result<String, String> {
        let text = a.text.as_str();
        let to = self.target(a.window.as_deref(), a.pid).await?;
        let mut note = None;
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                self.send("press_key", json!({"key": "return"}), to, &mut note)
                    .await?;
            }
            let line = line.trim_end_matches('\r');
            if self.os == Os::Linux {
                if line.is_empty() || (to.is_none() && self.xtest(line, turn.clone()).await?) {
                    continue;
                }
                for run in text_runs(line) {
                    self.send("type_text", json!({"text": run}), to, &mut note)
                        .await?;
                }
                continue;
            }
            if self.os == Os::Mac {
                if !line.is_empty() {
                    let mut req = json!({"text": line});
                    if !self.bg() && line.chars().all(|c| c.len_utf16() == 1) {
                        req["delay_ms"] = MAC_TYPE_DELAY_MS.into();
                    }
                    self.send("type_text", req, to, &mut note).await?;
                }
                continue;
            }
            for (at, c) in line.char_indices() {
                let (tool, req) = match keystroke(c) {
                    Some(keys) => key_request(keys),
                    None => ("type_text", json!({"text": &line[at..at + c.len_utf8()]})),
                };
                self.send(tool, req, to, &mut note).await?;
            }
        }
        if self.os == Os::Linux && to.is_none() {
            self.typed(text).await;
        }
        let mut out = json!({"ok": true, "chars": text.chars().count()});
        if let Some(note) = note {
            out["unverified"] = note.into();
        }
        Ok(out.to_string())
    }

    /// Sends `req` to window `to`, or in the foreground to the focused window, and keeps the first note of background input the driver could not confirm.
    async fn send(
        &self,
        tool: &str,
        req: Value,
        to: Option<(i64, i64)>,
        note: &mut Option<String>,
    ) -> Result<(), String> {
        let out = self.call(tool, deliver(req, to)).await?;
        if note.is_none() {
            *note = self.unverified(&out, to);
        }
        Ok(())
    }

    /// The driver's note on background input whose effect it could not confirm, with the escalation it proposes.
    fn unverified(&self, out: &Value, to: Option<(i64, i64)>) -> Option<String> {
        let c = &out["structuredContent"];
        let effect = c["effect"]
            .as_str()
            .filter(|e| self.bg() && *e != "confirmed")?;
        let mut note = format!("{effect}: {}", text_of(out));
        if to.is_some_and(|t| self.off_spaced().contains(&t)) {
            note = format!("{note}; {OFF_SPACE}");
        }
        let escalation = &c["escalation"];
        Some(
            match (escalation["reason"].as_str(), escalation["target"].as_str()) {
                (Some(reason), Some(target)) => {
                    format!("{note} (escalation: {reason}, to {target})")
                }
                _ => note,
            },
        )
    }

    /// Whether the typer typed `line` on the core keyboard; it runs on a blocking thread that holds the action's turn to its end and stops at its next key once this call is dropped.
    async fn xtest(&self, line: &str, turn: Option<Turn>) -> Result<bool, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let _cancel = Cancel(stop.clone());
        let (line, typer) = (line.to_owned(), self.typer);
        tokio::task::spawn_blocking(move || {
            let _turn = turn;
            typer(&line, &stop)
        })
        .await
        .map_err(|e| format!("XTest: {e}"))?
    }

    /// Waits at most [`TYPE_SETTLE`] until the focused element of the focused window holds the end of the last line typed, since an app reads typed keys after the input call returns.
    async fn typed(&self, text: &str) {
        let Some(tail) = typed_tail(text) else {
            return;
        };
        let allow = self.allow.clone();
        let started = Instant::now();
        let _ = tokio::time::timeout(TYPE_SETTLE, async {
            let focused = self
                .access
                .run(CALL_LIMIT, (), move |b| {
                    let frame = allow.pick(b.frames()?, None, None)?;
                    b.focused(&frame, TYPE_SETTLE.saturating_sub(started.elapsed()))
                })
                .await?;
            let Some(focused) = focused else {
                return Ok(());
            };
            loop {
                let (id, tail) = (focused.clone(), tail.clone());
                let values = Extras {
                    values: true,
                    ..Extras::default()
                };
                let holds = self
                    .access
                    .run(CALL_LIMIT, (), move |b| {
                        Ok(b.node(&id, values, READ_CHARS)?
                            .value
                            .is_some_and(|v| v.contains(&tail)))
                    })
                    .await?;
                if holds {
                    return Ok::<(), String>(());
                }
                tokio::time::sleep(TYPE_POLL).await;
            }
        })
        .await;
    }

    async fn click(&self, a: ClickArgs) -> Result<String, String> {
        let mut req = json!({"x": a.x, "y": a.y});
        if let Some(button) = a.button {
            req["button"] = button.into();
        }
        if let Some(count) = a.count {
            req["count"] = count.into();
        }
        if self.bg() && (a.window.is_some() || a.pid.is_some()) {
            let (_, w) = self.pick_window(a.window.as_deref(), a.pid).await?;
            let note = self.click_in(req, &w, (a.x, a.y)).await?;
            return Ok(ok_with(note));
        }
        let note = self.click_at(req).await?;
        Ok(ok_with(note))
    }

    /// A background click at desk point `(x, y)` in driver window `w`, which the allowlist already admitted when it was picked.
    async fn click_in(
        &self,
        req: Value,
        w: &Value,
        (x, y): (f64, f64),
    ) -> Result<Option<String>, String> {
        let scale = self.scale().await?;
        let under = Under {
            pid: w["pid"].as_i64().ok_or(BACKGROUND_TARGET)?,
            id: w["window_id"].as_i64().ok_or(BACKGROUND_TARGET)?,
            bounds: bounds(w),
            point: (x / scale, y / scale),
        };
        self.click_window(req, &under).await
    }

    /// A click at the centre `(x, y)` of element `id`; in the background in the element's own window, whatever covers it.
    async fn click_element(&self, id: String, req: Value) -> Result<Option<String>, String> {
        if !self.bg() {
            return self.click_at(req).await;
        }
        let point = req["x"]
            .as_f64()
            .zip(req["y"].as_f64())
            .ok_or(BACKGROUND_ELEMENT)?;
        let (pid, title, frame) = self
            .access
            .run(CALL_LIMIT, (), move |b| {
                let pid = b.pid_of(&id)?.ok_or(BACKGROUND_ELEMENT)?;
                let root = b.ancestors(&id)?.into_iter().next().unwrap_or(id);
                let title = b.frames_of(pid)?.into_iter().find(|f| f.id == root);
                let frame = b.node(&root, Extras::FRAME, 0)?.bounds;
                Ok((pid, title.ok_or(BACKGROUND_ELEMENT)?.title, frame))
            })
            .await?;
        let (_, picked) = self.pick_window(Some(&title), Some(pid)).await?;
        let scale = self.scale().await?;
        let mut listed = self.call("list_windows", json!({})).await?;
        let Value::Array(windows) = listed["structuredContent"]["windows"].take() else {
            return Err(BACKGROUND_ELEMENT.to_owned());
        };
        let titled = |w: &Value, tier: usize| {
            w["pid"].as_u64() == Some(u64::from(pid))
                && title_matches(tier, w["title"].as_str().unwrap_or_default(), &title)
        };
        let tier = (0..TITLE_TIERS).find(|&t| windows.iter().any(|w| titled(w, t)));
        let mut same: Vec<Value> = match tier {
            Some(t) => windows.into_iter().filter(|w| titled(w, t)).collect(),
            None if picked["pid"].as_u64() == Some(u64::from(pid)) => vec![picked],
            None => Vec::new(),
        };
        let border = if self.os == Os::Windows {
            FRAME_BORDER
        } else {
            2.0
        };
        same.retain(|w| {
            let [x, y, width, height] = bounds(w);
            frame.is_some_and(|[l, t, fw, fh]| {
                let [l, t, r, b] = [l, t, l + fw, t + fh].map(|v| f64::from(v) / scale);
                [x - l, y - t, r - x - width, b - y - height]
                    .iter()
                    .all(|d| (-2.0..=border).contains(d))
            })
        });
        match same.as_slice() {
            [w] => self.click_in(req, w, point).await,
            _ => Err(BACKGROUND_TWINS.to_owned()),
        }
    }

    async fn click_at(&self, req: Value) -> Result<Option<String>, String> {
        let point = req["x"].as_f64().zip(req["y"].as_f64());
        if self.os == Os::Mac
            && let Some((x, y)) = point
        {
            let (px, py) = (x as i32, y as i32);
            let allow = self.allow.clone();
            let pressed = self
                .access
                .run(CALL_LIMIT, (), move |b| {
                    let Some(item) = b.menu_at(px, py)? else {
                        return Ok(false);
                    };
                    if !allow.is_open() {
                        let pid = b.pid_of(&item)?;
                        check_process(b, &allow, pid)?;
                    }
                    if b.act(&item, Act::Action)? {
                        Ok(true)
                    } else {
                        Err(format!("menu item {item} took no action"))
                    }
                })
                .await?;
            if pressed {
                return Ok(None);
            }
        }
        let under = match point {
            Some((x, y)) if self.bg() || self.os == Os::Mac || !self.allow.is_open() => {
                self.window_at(x, y).await?
            }
            _ => None,
        };
        if !self.allow.is_open()
            && let Some((x, y)) = point
        {
            self.check_point(x, y, under.as_ref()).await?;
        }
        if (self.bg() || self.os == Os::Mac)
            && let Some(w) = under
        {
            return self.click_window(req, &w).await;
        }
        if self.bg() {
            return Err(BACKGROUND_NO_WINDOW.to_owned());
        }
        self.call("click", desktop(req)).await?;
        Ok(None)
    }

    /// Refuses a point whose window, or the process on top there, the allowlist does not admit.
    async fn check_point(&self, x: f64, y: f64, under: Option<&Under>) -> Result<(), String> {
        let pid = u32::try_from(under.ok_or(NO_WINDOW_UNDER)?.pid).ok();
        let allow = self.allow.clone();
        self.access
            .run(CALL_LIMIT, (), move |b| {
                check_process(b, &allow, pid)?;
                match b.pid_at(x as i32, y as i32)? {
                    Some(top) if Some(top) != pid => check_process(b, &allow, Some(top)),
                    _ => Ok(()),
                }
            })
            .await
    }

    async fn admit_point(&self, x: f64, y: f64) -> Result<(), String> {
        if self.allow.is_open() {
            return Ok(());
        }
        let under = self.window_at(x, y).await?;
        self.check_point(x, y, under.as_ref()).await
    }

    /// Wheel notches at a screen point; without one, scroll keys to the front app window on macOS (the driver's topmost, or the one an allowlist picks and checks), and elsewhere wheel notches at its center, the window of the focused app on Linux.
    async fn scroll(&self, a: ScrollArgs) -> Result<String, String> {
        if a.x.is_some() != a.y.is_some() {
            return Err(SCROLL_POINT.to_owned());
        }
        let mut req =
            json!({"direction": a.direction, "amount": a.amount.unwrap_or(SCROLL_AMOUNT)});
        if self.bg() {
            let (to, w) = self.background_window(a.window.as_deref(), a.pid).await?;
            if let (Some(x), Some(y)) = (a.x, a.y) {
                let (lx, ly) = self.local(&w, (x, y)).await?;
                req["x"] = lx.into();
                req["y"] = ly.into();
            }
            let out = self.call("scroll", background(req, to)).await?;
            return Ok(ok_with(self.unverified(&out, Some(to))));
        }
        let (x, y) = match (a.x, a.y) {
            (Some(x), Some(y)) => (x, y),
            (None, None) => {
                let target = self.front_window().await?;
                if self.os == Os::Mac {
                    req["pid"] = target["pid"].clone();
                    req["window_id"] = target["window_id"].clone();
                    req["session"] = WINDOW_SESSION.into();
                    self.call("scroll", req).await?;
                    return Ok(OK.to_owned());
                }
                let [bx, by, bw, bh] = bounds(&target);
                let (x, y) = (bx + bw / 2.0, by + bh / 2.0);
                let under = self.window_at(x, y).await?;
                if under.is_none_or(|u| {
                    (Some(u.pid), Some(u.id))
                        != (target["pid"].as_i64(), target["window_id"].as_i64())
                }) {
                    return Err("another window covers the front window's center".to_owned());
                }
                (x, y)
            }
            _ => return Err(SCROLL_POINT.to_owned()),
        };
        self.admit_point(x, y).await?;
        req["x"] = x.into();
        req["y"] = y.into();
        self.call("scroll", desktop(req)).await?;
        Ok(OK.to_owned())
    }

    async fn front_window(&self) -> Result<Value, String> {
        let picked = if self.allow.is_open() && self.os != Os::Linux {
            None
        } else {
            let allow = self.allow.clone();
            let frame = self
                .access
                .run(CALL_LIMIT, (), move |b| allow.pick(b.frames()?, None, None))
                .await?;
            Some((frame.pid, frame.title))
        };
        self.on_screen()
            .await?
            .into_iter()
            .filter(|w| layer(w) == 0)
            .filter(|w| {
                picked.as_ref().is_none_or(|(pid, _)| {
                    pid.is_some_and(|p| w["pid"].as_u64() == Some(u64::from(p)))
                })
            })
            .max_by_key(|w| {
                let titled = picked
                    .as_ref()
                    .is_some_and(|(_, t)| w["title"] == t.as_str());
                (titled, w["z_index"].as_i64().unwrap_or(i64::MIN))
            })
            .ok_or_else(|| "no app window is in front to scroll".to_owned())
    }

    /// A press-drag-release from (x, y) to (to_x, to_y) in desk coordinates.
    async fn drag(&self, a: DragArgs) -> Result<String, String> {
        if self.bg() {
            let (to, w) = self.background_window(a.window.as_deref(), a.pid).await?;
            let (fx, fy) = self.local(&w, (a.x, a.y)).await?;
            let (tx, ty) = self.local(&w, (a.to_x, a.to_y)).await?;
            let req = json!({"from_x": fx, "from_y": fy, "to_x": tx, "to_y": ty});
            let out = self.call("drag", background(req, to)).await?;
            return Ok(ok_with(self.unverified(&out, Some(to))));
        }
        self.admit_point(a.x, a.y).await?;
        self.admit_point(a.to_x, a.to_y).await?;
        let req = json!({"from_x": a.x, "from_y": a.y, "to_x": a.to_x, "to_y": a.to_y});
        self.call("drag", desktop(req)).await?;
        Ok(OK.to_owned())
    }

    /// Waits until no block of the screen changes in a way that counts (see [`Watch`]) for `hold_ms`, at most `timeout_ms`; says whether one changed meanwhile.
    async fn still(&self, a: StillArgs) -> Result<String, String> {
        let limit = a
            .timeout_ms
            .map_or(STILL_TIMEOUT, Duration::from_millis)
            .min(STILL_MAX);
        let hold = a.hold_ms.map_or(STILL_HOLD, Duration::from_millis);
        let started = Instant::now();
        let mut last = self.still_frame().await?;
        let mut watch = Watch::default();
        let (mut since, mut changed) = (Instant::now(), false);
        loop {
            let still = since.elapsed() >= hold;
            if still || started.elapsed() >= limit {
                let waited = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                return Ok(json!({"still": still, "changed": changed, "ms": waited, "volatile": watch.volatile()}).to_string());
            }
            tokio::time::sleep(STILL_POLL).await;
            let now = self.still_frame().await?;
            if watch.counts(&last, &now) {
                (changed, since) = (true, Instant::now());
            }
            last = now;
        }
    }

    async fn still_frame(&self) -> Result<StillFrame, String> {
        let v = self
            .call(
                "get_desktop_state",
                json!({"max_image_dimension": STILL_EDGE, "session": CAPTURE_SESSION}),
            )
            .await?;
        let png = image_of(&v).ok_or("the driver returned no screenshot")?;
        StillFrame::decode(
            &B64.decode(png)
                .map_err(|e| format!("decode capture: {e}"))?,
        )
    }

    async fn click_window(&self, req: Value, w: &Under) -> Result<Option<String>, String> {
        if !self.bg() {
            let front = json!({"pid": w.pid, "window_id": w.id});
            self.call("bring_to_front", front).await?;
        }
        let cached = self
            .reads()
            .filter(|r| (r.pid, r.id, r.bounds) == (w.pid, w.id, w.bounds))
            .map(|r| r.k);
        let k = match cached {
            Some(k) => Some(k),
            None => self.pixels(w).await?,
        };
        let Some(k) = k else {
            return self.desktop_click(req).await.map(|()| None);
        };
        let mut win = req.clone();
        win["session"] = WINDOW_SESSION.into();
        win["pid"] = w.pid.into();
        win["window_id"] = w.id.into();
        if self.bg() {
            win["delivery_mode"] = "background".into();
        }
        let at = |win: &mut Value, (kx, ky): (f64, f64)| {
            win["x"] = ((w.point.0 - w.bounds[0]) * kx).into();
            win["y"] = ((w.point.1 - w.bounds[1]) * ky).into();
        };
        at(&mut win, k);
        let out = match self.call("click", win.clone()).await {
            Err(e) if e.contains(NO_WINDOWSERVER_CONNECTION) => {
                return self.desktop_click(req).await.map(|()| None);
            }
            Err(e) if cached.is_some() && e.contains(WINDOW_NEVER_READ) => {
                match self.pixels(w).await? {
                    Some(k) => {
                        at(&mut win, k);
                        self.call("click", win).await?
                    }
                    None => return self.desktop_click(req).await.map(|()| None),
                }
            }
            done => done?,
        };
        Ok(self.unverified(&out, Some((w.pid, w.id))))
    }

    /// A click at a screen point, for a window that takes no window click; background input refuses it.
    async fn desktop_click(&self, req: Value) -> Result<(), String> {
        if self.bg() {
            return Err(BACKGROUND_DESKTOP.to_owned());
        }
        self.call("click", desktop(req)).await?;
        Ok(())
    }

    /// The window background input goes to, as `(pid, window_id)`; none in the foreground, where input goes to the focused window.
    async fn target(
        &self,
        window: Option<&str>,
        pid: Option<u32>,
    ) -> Result<Option<(i64, i64)>, String> {
        if !self.bg() {
            return Ok(None);
        }
        Ok(Some(self.background_window(window, pid).await?.0))
    }

    async fn background_window(
        &self,
        window: Option<&str>,
        pid: Option<u32>,
    ) -> Result<((i64, i64), Value), String> {
        if window.is_none() && pid.is_none() {
            return Err(BACKGROUND_TARGET.to_owned());
        }
        let (_, w) = self.pick_window(window, pid).await?;
        let to = w["pid"]
            .as_i64()
            .zip(w["window_id"].as_i64())
            .ok_or(BACKGROUND_TARGET)?;
        Ok((to, w))
    }

    /// The desk point `(x, y)` in the screenshot pixels of driver window `w`, which the driver reads first as for a click.
    async fn local(&self, w: &Value, (x, y): (f64, f64)) -> Result<(f64, f64), String> {
        let scale = self.scale().await?;
        let (pid, id) = w["pid"]
            .as_i64()
            .zip(w["window_id"].as_i64())
            .ok_or(BACKGROUND_TARGET)?;
        let under = Under {
            pid,
            id,
            bounds: bounds(w),
            point: (x / scale, y / scale),
        };
        let cached = self
            .reads()
            .filter(|r| (r.pid, r.id, r.bounds) == (pid, id, under.bounds))
            .map(|r| r.k);
        let (kx, ky) = match cached {
            Some(k) => k,
            None => self.pixels(&under).await?.ok_or(BACKGROUND_DESKTOP)?,
        };
        Ok((
            (under.point.0 - under.bounds[0]) * kx,
            (under.point.1 - under.bounds[1]) * ky,
        ))
    }

    async fn scale(&self) -> Result<f64, String> {
        if self.os == Os::Windows {
            return Ok(1.0);
        }
        let v = self.call("get_screen_size", json!({})).await?;
        Ok(v["structuredContent"]["scale_factor"]
            .as_f64()
            .unwrap_or(1.0))
    }

    async fn window_at(&self, x: f64, y: f64) -> Result<Option<Under>, String> {
        let scale = self.scale().await?;
        let listed = self.on_screen().await?;
        let point = (x / scale, y / scale);
        let top = listed
            .iter()
            .filter(|w| layer(w) == 0)
            .filter(|w| {
                let [bx, by, bw, bh] = bounds(w);
                point.0 >= bx && point.0 < bx + bw && point.1 >= by && point.1 < by + bh
            })
            .max_by_key(|w| w["z_index"].as_i64().unwrap_or(i64::MIN));
        Ok(top.and_then(|w| {
            Some(Under {
                pid: w["pid"].as_i64()?,
                id: w["window_id"].as_i64()?,
                bounds: bounds(w),
                point,
            })
        }))
    }

    async fn on_screen(&self) -> Result<Vec<Value>, String> {
        let mut listed = self
            .call("list_windows", json!({"on_screen_only": true}))
            .await?;
        let Value::Array(windows) = listed["structuredContent"]["windows"].take() else {
            return Ok(Vec::new());
        };
        Ok(windows)
    }

    async fn pixels(&self, w: &Under) -> Result<Option<(f64, f64)>, String> {
        let read = json!({"session": WINDOW_SESSION, "pid": w.pid, "window_id": w.id});
        let v = self.call("get_window_state", read).await?;
        let c = &v["structuredContent"];
        let refused = c["background_input"]["routes"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|r| r["route"] == "window_pointer" && r["status"] == "refused");
        if refused {
            return Ok(None);
        }
        let ratio = |px: &str, pt: &str| {
            c[px]
                .as_f64()
                .zip(c["window_bounds"][pt].as_f64())
                .filter(|&(_, pt)| pt > 0.0)
                .map(|(px, pt)| px / pt)
                .ok_or_else(|| format!("get_window_state of window {} has no {px}", w.id))
        };
        let k = (
            ratio("screenshot_width", "width")?,
            ratio("screenshot_height", "height")?,
        );
        *self.reads() = Some(Read {
            pid: w.pid,
            id: w.id,
            bounds: w.bounds,
            k,
        });
        Ok(Some(k))
    }

    fn reads(&self) -> std::sync::MutexGuard<'_, Option<Read>> {
        self.read
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn off_spaced(&self) -> std::sync::MutexGuard<'_, HashSet<(i64, i64)>> {
        self.off_space
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn focus(&self, a: WindowArgs) -> Result<String, String> {
        let (w, _) = self.pick_window(Some(&a.window), a.pid).await?;
        let window_id: i64 =
            w.id.parse()
                .map_err(|_| format!("window id {:?} is not a number", w.id))?;
        let v = self
            .call(
                "bring_to_front",
                json!({"pid": w.pid, "window_id": window_id}),
            )
            .await?;
        let mut out = json!({"ok": true, "title": w.title, "pid": w.pid});
        let done = &v["structuredContent"];
        if let Some(path) = done.get("path") {
            out["path"] = path.clone();
            out["front"] = done["observed"]["workspace_frontmost_pid"].clone();
        }
        Ok(out.to_string())
    }

    /// The driver window titled like `window` or of `pid`, as a frame and as the driver lists it; the allowlist picks among them.
    async fn pick_window(
        &self,
        window: Option<&str>,
        pid: Option<u32>,
    ) -> Result<(Frame, Value), String> {
        let mut listed = self.call("list_windows", json!({})).await?;
        let mut raw = match listed["structuredContent"]["windows"].take() {
            Value::Array(windows) => windows,
            _ => Vec::new(),
        };
        let mac = self.os == Os::Mac;
        raw.retain(|w| !mac || w["is_on_screen"] != false || !w["on_current_space"].is_null());
        let mut windows: Vec<Frame> = raw
            .iter()
            .map(|w| Frame {
                id: w["window_id"].to_string(),
                title: w["title"].as_str().unwrap_or_default().to_owned(),
                pid: w["pid"].as_u64().and_then(|p| u32::try_from(p).ok()),
                app: w["app_name"].as_str().map(str::to_owned),
                ..Frame::default()
            })
            .collect();
        let want = window.unwrap_or_default().trim().to_lowercase();
        let unique = windows
            .iter()
            .filter(|w| w.title.to_lowercase().contains(&want))
            .count()
            == 1;
        if !self.allow.is_open() || !unique {
            let frames = self.access.run(CALL_LIMIT, (), |b| b.frames()).await?;
            for w in &mut windows {
                w.app = app_of(&frames, w.pid).map(str::to_owned);
                w.active = frames
                    .iter()
                    .any(|f| f.active && f.pid == w.pid && f.title == w.title);
            }
        }
        let w = self.allow.pick(windows, window, pid)?;
        let id = w.id.parse::<i64>().ok();
        let listed = raw
            .iter()
            .find(|r| r["window_id"].as_i64() == id)
            .cloned()
            .unwrap_or_default();
        if self.bg() && mac {
            let twins: Vec<&Value> = raw
                .iter()
                .filter(|r| r["pid"] == listed["pid"] && r["title"] == listed["title"])
                .collect();
            if twins.len() > 1 && twins.iter().any(|t| off_space(t)) {
                let named: Vec<String> = twins
                    .iter()
                    .map(|t| {
                        let space = if off_space(t) {
                            "another Space"
                        } else {
                            "this Space"
                        };
                        format!("{} on {space}", t["window_id"])
                    })
                    .collect();
                return Err(format!(
                    "{} windows of pid {} are titled {:?} ({}): background input cannot tell them apart; give one of them a title of its own or close the other",
                    twins.len(),
                    listed["pid"],
                    w.title,
                    named.join(", ")
                ));
            }
        }
        if let Some(to) = listed["pid"].as_i64().zip(listed["window_id"].as_i64()) {
            let mut off = self.off_spaced();
            if off_space(&listed) {
                off.insert(to);
            } else {
                off.remove(&to);
            }
        }
        Ok((w, listed))
    }

    async fn shot(&self, a: ShotArgs) -> Result<Output, String> {
        let rect = [
            a.x.unwrap_or(0.0),
            a.y.unwrap_or(0.0),
            a.w.unwrap_or(f64::MAX / 4.0),
            a.h.unwrap_or(f64::MAX / 4.0),
        ];
        let overlay = self.overlay;
        if let Some(c) = self
            .access
            .run(CALL_LIMIT, (), move |b| b.capture(rect, overlay))
            .await?
        {
            return self.captured(c, a.path).await;
        }
        let v = self
            .call("get_desktop_state", json!({"session": CAPTURE_SESSION}))
            .await?;
        let png = image_of(&v).ok_or("the driver returned no screenshot")?;
        let png = B64
            .decode(png)
            .map_err(|e| format!("decode screenshot: {e}"))?;
        let s = &v["structuredContent"];
        let shot_w = s["screenshot_width"].as_f64().unwrap_or(0.0);
        let full_w = s["screenshot_original_width"].as_f64().unwrap_or(shot_w);
        let factor = if full_w > 0.0 { shot_w / full_w } else { 1.0 };
        let mask = if self.allow.is_open() {
            None
        } else {
            Some(self.mask(factor).await?)
        };
        let (w, h, png) = crop_png(&png, rect, factor, mask.as_deref())?;
        if self.os == Os::Linux {
            return delivered(png, w, h, a.path).await;
        }
        Ok(Output {
            text: json!({"ok": true, "width": w, "height": h}).to_string(),
            image_b64: Some(B64.encode(&png)),
            ..Output::default()
        })
    }

    /// A capture the backend read itself, as a PNG image, also written to `path` when one is given.
    async fn captured(&self, c: Capture, path: Option<String>) -> Result<Output, String> {
        let rect = c.rect;
        let mask = if self.allow.is_open() {
            None
        } else {
            Some(self.mask(1.0).await?)
        };
        let png = rgb_png(c.rgb, rect, mask.as_deref())?;
        delivered(png, rect[2], rect[3], path).await
    }

    async fn mask(&self, factor: f64) -> Result<Vec<([f64; 4], bool)>, String> {
        let k = self.scale().await? * factor;
        let mut windows = self.on_screen().await?;
        let frames = self.access.run(CALL_LIMIT, (), |b| b.frames()).await?;
        windows.sort_by_key(|w| w["z_index"].as_i64().unwrap_or(i64::MIN));
        Ok(windows
            .into_iter()
            .map(|w| {
                let pid = w["pid"].as_u64().and_then(|p| u32::try_from(p).ok());
                (
                    bounds(&w).map(|v| v * k),
                    self.allow.admits(app_of(&frames, pid)),
                )
            })
            .collect())
    }
}

struct Under {
    pid: i64,
    id: i64,
    bounds: [f64; 4],
    point: (f64, f64),
}

#[derive(Clone, Copy)]
struct Read {
    pid: i64,
    id: i64,
    bounds: [f64; 4],
    k: (f64, f64),
}

/// The windows a call picks from: the app of `pid` alone, unless the allowlist needs every window to judge the call.
fn frames<B: Access>(b: &mut B, pid: Option<u32>, allow: &Allow) -> Result<Vec<Frame>, String> {
    match pid {
        Some(p) if allow.is_open() => b.frames_of(p),
        _ => b.frames(),
    }
}

fn check_process<B: Access>(b: &mut B, allow: &Allow, pid: Option<u32>) -> Result<(), String> {
    let frames = pid.map_or(Ok(Vec::new()), |p| b.frames_of(p))?;
    allow.check(app_of(&frames, pid))
}

fn pick<B: Access>(
    b: &mut B,
    allow: &Allow,
    background: bool,
    frames: Vec<Frame>,
    window: Option<&str>,
    pid: Option<u32>,
) -> Result<Frame, String> {
    allow.pick(frames, window, pid).map_err(|e| {
        let want = window.unwrap_or_default();
        let named = |f: &Frame| {
            pid.is_none_or(|p| f.pid == Some(p))
                && (0..TITLE_TIERS).any(|t| title_matches(t, &f.title, want))
        };
        window.and_then(|_| away(b, background, named)).unwrap_or(e)
    })
}

fn away<B: Access>(b: &mut B, background: bool, named: impl Fn(&Frame) -> bool) -> Option<String> {
    if !background {
        return None;
    }
    let f = b.off_space().into_iter().find(|f| named(f))?;
    Some(format!(
        "window {:?} is on another Space: {ANOTHER_SPACE}",
        f.title
    ))
}

fn live(frame: Frame) -> Result<Frame, String> {
    if frame.stale {
        return Err(format!(
            "window list is stale: {:?} did not answer; take a new snapshot",
            frame.title
        ));
    }
    Ok(frame)
}

/// A Linux shot's answer: the PNG as the image, also written to `path` when one is given.
async fn delivered(png: Vec<u8>, w: u32, h: u32, path: Option<String>) -> Result<Output, String> {
    let mut out = json!({"ok": true, "width": w, "height": h});
    if let Some(path) = path {
        tokio::fs::write(&path, &png)
            .await
            .map_err(|e| format!("write {path}: {e}"))?;
        out["path"] = path.into();
    }
    Ok(Output {
        text: out.to_string(),
        image_b64: Some(B64.encode(&png)),
        ..Output::default()
    })
}

/// Moves a Linux launch into a systemd scope of its own on a task that finishes when the call is dropped; false when the launch stays in the daemon's cgroup.
#[cfg(target_os = "linux")]
pub(crate) async fn adopt(launched: Value) -> bool {
    tokio::spawn(async move {
        crate::scope::adopt(&launched).await.unwrap_or_else(|e| {
            eprintln!("desk: scope the launch: {e}");
            false
        })
    })
    .await
    .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
pub(crate) async fn adopt(_: Value) -> bool {
    false
}

/// Sets its flag when dropped, as when a stop drops the call that holds it.
pub(crate) struct Cancel(pub(crate) Arc<AtomicBool>);

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Runs of `text` with at most [`RUN_CHARS`] distinct non-ASCII characters; the Linux driver binds a spare keycode to each for the call and has fewer than 20 to lend.
pub(crate) fn text_runs(text: &str) -> Vec<&str> {
    let (mut runs, mut start, mut seen) = (Vec::new(), 0, Vec::new());
    for (i, c) in text.char_indices() {
        if c.is_ascii() || seen.contains(&c) {
            continue;
        }
        if seen.len() == RUN_CHARS {
            runs.push(&text[start..i]);
            (start, seen) = (i, Vec::new());
        }
        seen.push(c);
    }
    if start < text.len() {
        runs.push(&text[start..]);
    }
    runs
}

/// The last characters of the last line of `text`, which a field holds once it has read the typing.
fn typed_tail(text: &str) -> Option<String> {
    let line = text.rsplit('\n').next()?.trim_end_matches('\r');
    let n = line.chars().count();
    (n > 0).then(|| line.chars().skip(n.saturating_sub(TYPE_TAIL)).collect())
}

fn key_request(mut keys: Vec<String>) -> (&'static str, Value) {
    let mut req = json!({});
    if keys.len() == 1
        && let Some(key) = keys.pop()
    {
        req["key"] = key.into();
        return ("press_key", req);
    }
    req["keys"] = keys.into();
    ("hotkey", req)
}

/// A driver window's layer, 0 when the driver gives none, as on Linux.
fn layer(w: &Value) -> i64 {
    w["layer"].as_i64().unwrap_or(0)
}

fn off_space(w: &Value) -> bool {
    w["on_current_space"] == false && w["space_ids"].as_array().is_some_and(|s| !s.is_empty())
}

/// `{"ok":true}`, with the driver's note when it could not confirm background input.
fn ok_with(unverified: Option<String>) -> String {
    match unverified {
        Some(note) => json!({"ok": true, "unverified": note}).to_string(),
        None => OK.to_owned(),
    }
}

/// `req` to the focused window, or in the background to window `to`.
fn deliver(req: Value, to: Option<(i64, i64)>) -> Value {
    match to {
        Some(to) => background(req, to),
        None => desktop(req),
    }
}

fn bounds(w: &Value) -> [f64; 4] {
    let b = &w["bounds"];
    ["x", "y", "width", "height"].map(|k| b[k].as_f64().unwrap_or(0.0))
}

/// One line per node: `- role "name" [disabled] [checked] [shortcut] [id=ID]: "value"`, indented by depth.
fn outline(nodes: &[Node], max_chars: usize) -> String {
    let mut out = String::new();
    for n in nodes {
        let _ = write!(out, "{:indent$}- {}", "", n.role, indent = n.depth * 2);
        if !n.name.is_empty() {
            let _ = write!(out, " \"{}\"", quote(&n.name, max_chars));
        }
        for state in ["disabled", "checked", "mixed"] {
            if n.has(state) {
                let _ = write!(out, " [{state}]");
            }
        }
        if let Some(k) = n.keys.as_ref().or(n.shortcut.as_ref()) {
            let _ = write!(out, " [{k}]");
        }
        if !n.id.is_empty() {
            let _ = write!(out, " [id={}]", n.id);
        }
        if let Some(value) = &n.value {
            let _ = write!(out, ": \"{}\"", quote(value, max_chars));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::model::Tree;

    const PID_CALLS: [(&str, &str); 4] = [
        ("desk_snapshot", r#"{"pid": 4}"#),
        ("desk_find", r#"{"name": "Save", "pid": 4}"#),
        ("desk_at", r#"{"x": 1, "y": 1, "pid": 4}"#),
        ("desk_close", r#"{"window": "Notepad", "pid": 4}"#),
    ];

    static ACTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    #[test]
    fn an_outline_shows_a_linux_raw_key_binding_in_place_of_the_shortcut() {
        let n = |keys: Option<&str>| Node {
            id: "1.2/3".into(),
            role: "menuitem",
            name: "Save".into(),
            shortcut: Some("ctrl+s".into()),
            keys: keys.map(str::to_owned),
            ..Node::default()
        };
        assert_eq!(
            outline(&[n(Some("<Alt>s;;<Control>s"))], 80),
            "- menuitem \"Save\" [<Alt>s;;<Control>s] [id=1.2/3]\n"
        );
        assert_eq!(
            outline(&[n(None)], 80),
            "- menuitem \"Save\" [ctrl+s] [id=1.2/3]\n"
        );
    }

    #[derive(Default)]
    pub(crate) struct Fake {
        nodes: HashMap<String, (Node, Option<String>)>,
        pub(crate) hits: HashMap<(i32, i32), String>,
        pub(crate) acts: Vec<(String, Act)>,
        pub(crate) ancestry: usize,
        opened: bool,
        menus: Vec<bool>,
        untitled: bool,
    }

    impl Fake {
        pub(crate) fn add(
            &mut self,
            id: &str,
            parent: Option<&str>,
            role: &'static str,
            states: &[&'static str],
            bounds: Option<[i32; 4]>,
        ) {
            let n = Node {
                id: id.into(),
                role,
                name: id.into(),
                states: states.to_vec(),
                bounds,
                ..Node::default()
            };
            self.nodes.insert(id.into(), (n, parent.map(str::to_owned)));
        }
    }

    impl Access for Fake {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            let title = if self.untitled {
                ""
            } else {
                "Untitled - Notepad"
            };
            Ok(vec![frame("w", title, 4, true, Some("Notepad"))])
        }

        fn tree(&mut self, frame: &Frame, extras: Extras) -> Result<Tree, String> {
            self.menus.push(extras.menus);
            Ok(Tree {
                frame: frame.clone(),
                ..Tree::default()
            })
        }

        fn node(&mut self, id: &str, _: Extras, _: usize) -> Result<Node, String> {
            let (mut n, _) = self.nodes.get(id).cloned().ok_or("gone")?;
            if self.opened && n.role == "menuitem" {
                n.states.push("showing");
            }
            Ok(n)
        }

        fn ancestors(&mut self, id: &str) -> Result<Vec<String>, String> {
            self.ancestry += 1;
            let mut chain = Vec::new();
            let mut at = self.nodes.get(id).and_then(|(_, p)| p.clone());
            while let Some(p) = at {
                at = self.nodes.get(&p).and_then(|(_, p)| p.clone());
                chain.push(p);
            }
            chain.reverse();
            Ok(chain)
        }

        fn at(&mut self, _: &str, x: i32, y: i32) -> Result<Option<String>, String> {
            Ok(self.hits.get(&(x, y)).cloned())
        }

        fn pid_of(&mut self, id: &str) -> Result<Option<u32>, String> {
            Ok(self.nodes.contains_key(id).then_some(4))
        }

        fn menu_at(&mut self, x: i32, y: i32) -> Result<Option<String>, String> {
            Ok(self
                .hits
                .get(&(x, y))
                .filter(|id| {
                    self.nodes
                        .get(*id)
                        .is_some_and(|(n, _)| n.role == "menuitem")
                })
                .cloned())
        }

        fn act(&mut self, id: &str, act: Act) -> Result<bool, String> {
            self.acts.push((id.into(), act));
            ACTED.lock().unwrap().push(id.into());
            if id == "dead" {
                return Ok(false);
            }
            if id == "slow" {
                return Err("the action timed out".into());
            }
            if self.nodes.get(id).is_some_and(|(n, _)| n.role == "menu") {
                self.opened = true;
            }
            Ok(act != Act::Select
                || self
                    .nodes
                    .get(id)
                    .is_some_and(|(n, _)| matches!(n.role, "tab" | "treeitem")))
        }

        fn close(&mut self, _: &Frame) -> Result<(), String> {
            Ok(())
        }
    }

    pub(crate) fn notepad() -> Fake {
        let mut f = Fake::default();
        f.add("w", None, "window", &["showing"], Some([0, 0, 800, 600]));
        f.add(
            "ok",
            Some("w"),
            "button",
            &["showing"],
            Some([100, 100, 80, 20]),
        );
        f.add(
            "hidden",
            Some("w"),
            "button",
            &["showing"],
            Some([300, 100, 80, 20]),
        );
        f.add("tab", Some("w"), "tab", &["showing"], Some([0, 30, 60, 20]));
        f.add(
            "file",
            Some("w"),
            "menu",
            &["showing"],
            Some([0, 0, 40, 20]),
        );
        f.add("save", Some("file"), "menuitem", &[], None);
        f.hits.insert((140, 110), "ok".into());
        f.hits.insert((340, 110), "w".into());
        f
    }

    #[derive(Default)]
    pub(crate) struct Stub {
        frames: Vec<Frame>,
        scoped: bool,
        closed: Vec<String>,
        opens: Option<Opens>,
        overlays: Vec<bool>,
        stalled: bool,
        budgets: Vec<Duration>,
        shells: Vec<u32>,
    }

    impl Stub {
        fn opens(at: usize, hinted: bool, list_ms: u64) -> Self {
            Self {
                opens: Some(Opens {
                    at,
                    hinted,
                    list_ms,
                    ..Opens::default()
                }),
                ..Self::default()
            }
        }
    }

    impl Access for Stub {
        fn focused(&mut self, frame: &Frame, within: Duration) -> Result<Option<String>, String> {
            if !self.stalled {
                return crate::model::focused_by_tree(self, frame);
            }
            self.budgets.push(within);
            std::thread::sleep(within);
            Ok(None)
        }

        fn capture(&mut self, _: [f64; 4], overlay: bool) -> Result<Option<Capture>, String> {
            self.overlays.push(overlay);
            Ok(None)
        }

        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            if let Some(o) = &mut self.opens {
                return Ok(o.frames());
            }
            if self.scoped {
                return Err("every app was read".into());
            }
            Ok(self.frames.clone())
        }

        fn frames_of(&mut self, pid: u32) -> Result<Vec<Frame>, String> {
            let frames = if self.scoped {
                self.frames.clone()
            } else {
                self.frames()?
            };
            Ok(frames.into_iter().filter(|f| f.pid == Some(pid)).collect())
        }

        fn hint(&mut self) -> Option<u64> {
            self.opens.as_mut().and_then(Opens::hint)
        }

        fn tree(&mut self, frame: &Frame, _: Extras) -> Result<Tree, String> {
            Ok(Tree {
                frame: frame.clone(),
                ..Tree::default()
            })
        }

        fn node(&mut self, _: &str, _: Extras, _: usize) -> Result<Node, String> {
            Err("no node".into())
        }

        fn ancestors(&mut self, _: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }

        fn at(&mut self, _: &str, _: i32, _: i32) -> Result<Option<String>, String> {
            Ok(None)
        }

        fn act(&mut self, _: &str, _: Act) -> Result<bool, String> {
            Ok(false)
        }

        fn close(&mut self, frame: &Frame) -> Result<(), String> {
            self.closed.push(frame.id.clone());
            Ok(())
        }

        fn shells(&mut self) -> Vec<u32> {
            self.shells.clone()
        }
    }

    #[derive(Default)]
    struct Opens {
        at: usize,
        hinted: bool,
        list_ms: u64,
        skew: Arc<std::sync::atomic::AtomicU64>,
        t: usize,
        lists: usize,
        seen: Option<usize>,
    }

    impl Opens {
        fn frames(&mut self) -> Vec<Frame> {
            self.lists += 1;
            self.skew
                .fetch_add(self.list_ms, std::sync::atomic::Ordering::Relaxed);
            if !self.hinted {
                self.t += 1;
            }
            if self.t >= self.at {
                self.seen.get_or_insert(self.t);
            }
            if self.t >= self.at {
                vec![
                    frame("save", "Save", 4, true, None),
                    frame("main", "Untitled", 4, false, None),
                ]
            } else {
                vec![frame("main", "Untitled", 4, true, None)]
            }
        }

        fn hint(&mut self) -> Option<u64> {
            self.hinted.then(|| {
                self.t += 1;
                u64::from(self.t >= self.at)
            })
        }
    }

    fn frame(id: &str, title: &str, pid: u32, active: bool, app: Option<&str>) -> Frame {
        Frame {
            id: id.into(),
            title: title.into(),
            role: "window",
            pid: Some(pid),
            active,
            app: app.map(str::to_owned),
            ..Frame::default()
        }
    }

    fn one_app() -> Stub {
        Stub {
            frames: vec![frame("w", "Untitled - Notepad", 4, true, Some("Notepad"))],
            scoped: true,
            ..Stub::default()
        }
    }

    #[tokio::test]
    async fn the_window_list_marks_the_windows_of_the_desktop_shell() {
        let desk = Desk::new(
            || {
                Ok(Stub {
                    frames: vec![
                        frame("4/1", "Untitled - Notepad", 4, true, Some("Notepad")),
                        frame("8/1", "Desktop", 8, false, Some("xfdesktop")),
                        frame("9/1", "", 9, false, Some("xfce4-panel")),
                    ],
                    shells: vec![8, 9],
                    ..Stub::default()
                })
            },
            Slow(Arc::default()),
            Os::Mac,
        );
        let out = desk.run("desk_windows", json!({})).await.unwrap().text;
        let listed: Vec<Value> = serde_json::from_str(&out).unwrap();
        let shell: Vec<(&str, bool)> = listed
            .iter()
            .map(|w| (w["id"].as_str().unwrap(), w["shell"] == true))
            .collect();
        assert_eq!(shell, [("4/1", false), ("8/1", true), ("9/1", true)]);
        assert!(listed[0].get("shell").is_none(), "{out}");
    }

    async fn closed<D: Input + Sync>(desk: &Desk<Stub, D>) -> Vec<String> {
        desk.access
            .run(CALL_LIMIT, (), |b| Ok(b.closed.clone()))
            .await
            .unwrap()
    }

    struct Recorder(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Recorder {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(json!({}))
        }
    }

    struct Twins(Arc<Mutex<Vec<(String, Value)>>>, f64, &'static str, f64);

    impl Input for Twins {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 1.0}}),
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 4, "window_id": 1, "layer": 0, "z_index": 1, "title": self.2, "bounds": {"x": 0.0, "y": 0.0, "width": 800.0, "height": 600.0}},
                    {"pid": 4, "window_id": 3, "layer": 0, "z_index": 2, "title": self.2, "bounds": {"x": self.1 + self.3, "y": self.3, "width": 800.0 - 2.0 * self.3, "height": 600.0 - 2.0 * self.3}}
                ]}}),
                "get_window_state" => json!({"structuredContent": {
                    "screenshot_width": 800, "screenshot_height": 600,
                    "window_bounds": {"width": 800.0, "height": 600.0}
                }}),
                _ => json!({}),
            })
        }
    }

    struct Solo(
        Arc<Mutex<Vec<(String, Value)>>>,
        &'static str,
        f64,
        f64,
        f64,
    );

    impl Input for Solo {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": self.3}}),
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 4, "window_id": 5, "layer": 0, "z_index": 1, "title": self.1, "bounds": {"x": self.2 + self.4, "y": 0.0, "width": 800.0 / self.3 - 2.0 * self.4, "height": 600.0 / self.3 - self.4}}
                ]}}),
                "get_window_state" => json!({"structuredContent": {
                    "screenshot_width": 800, "screenshot_height": 600,
                    "window_bounds": {"width": 800.0, "height": 600.0}
                }}),
                _ => json!({}),
            })
        }
    }

    struct Overlaid(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Overlaid {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 1.0}}),
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 4, "window_id": 1, "layer": 0, "z_index": 1, "title": "Untitled - Notepad", "bounds": {"x": 0.0, "y": 0.0, "width": 800.0, "height": 600.0}},
                    {"pid": 8, "window_id": 2, "layer": 0, "z_index": 2, "title": "Inbox", "bounds": {"x": 0.0, "y": 0.0, "width": 1000.0, "height": 1000.0}}
                ]}}),
                "get_window_state" => json!({"structuredContent": {
                    "screenshot_width": 800, "screenshot_height": 600,
                    "window_bounds": {"width": 800.0, "height": 600.0}
                }}),
                "click" => json!({"structuredContent": {"effect": "unverifiable"}}),
                _ => json!({}),
            })
        }
    }

    struct Behind(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Behind {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            let pid = args["pid"].as_i64();
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 1.0}}),
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 4, "window_id": 1, "layer": 0, "z_index": 1, "title": "Untitled - Notepad", "app_name": "Notepad", "bounds": {"x": 100.0, "y": 50.0, "width": 800.0, "height": 600.0}},
                    {"pid": 8, "window_id": 2, "layer": 0, "z_index": 2, "title": "Inbox", "app_name": "Mail", "bounds": {"x": 500.0, "y": 0.0, "width": 500.0, "height": 500.0}}
                ]}}),
                "get_window_state" if pid == Some(8) => json!({"structuredContent": {
                    "background_input": {"routes": [{"route": "window_pointer", "status": "refused"}]}
                }}),
                "get_window_state" => json!({"structuredContent": {
                    "screenshot_width": 800, "screenshot_height": 600,
                    "window_bounds": {"width": 800.0, "height": 600.0}
                }}),
                "type_text" => json!({
                    "content": [{"type": "text", "text": "Sent 1 char(s) via PostMessage (not verified)."}],
                    "structuredContent": {
                        "effect": "unverifiable",
                        "escalation": {"reason": "delivery_failed", "target": "foreground"}
                    }
                }),
                "press_key" => json!({"structuredContent": {"effect": "confirmed"}}),
                _ => json!({}),
            })
        }
    }

    struct Refuses(Behind, fn(&str, &Value) -> bool);

    impl Input for Refuses {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            if args["delivery_mode"] == "background" && (self.1)(tool, &args) {
                self.0.0.lock().unwrap().push((tool.to_owned(), args));
                return Err(format!(
                    "{tool}: Background delivery is not available for target window class 'Chrome_WidgetWin_1' on this event kind (key_combo). Retry this action with delivery_mode:\"foreground\"."
                ));
            }
            self.0.call(tool, args).await
        }
    }

    fn order(calls: &Arc<Mutex<Vec<(String, Value)>>>) -> Vec<String> {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _)| {
                ["hotkey", "press_key", "type_text", "drag", "bring_to_front"].contains(&t.as_str())
            })
            .map(|(t, a)| {
                format!(
                    "{t} {}",
                    a["delivery_mode"]
                        .as_str()
                        .or(a["scope"].as_str())
                        .unwrap_or("-")
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn auto_input_refused_before_any_was_sent_fronts_its_window_and_runs_once_in_the_foreground()
     {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(
            || Ok(Apps),
            Refuses(Behind(Arc::clone(&calls)), |t, _| t == "hotkey"),
            Os::Windows,
        )
        .background(true)
        .auto(true);
        let out = desk
            .run(
                "desk_key",
                json!({"keys": "ctrl+a", "window": "Untitled - Notepad"}),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(v["input"], "foreground", "{v}");
        assert_eq!(
            order(&calls),
            ["hotkey background", "bring_to_front -", "hotkey desktop"]
        );
        assert_eq!(sent(&calls, "bring_to_front")[0]["window_id"], 1);
    }

    #[tokio::test]
    async fn auto_input_refused_after_some_was_sent_stays_an_error_and_is_not_sent_again() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(
            || Ok(Apps),
            Refuses(Behind(Arc::clone(&calls)), |t, a| {
                t == "press_key" && a["key"] == "return"
            }),
            Os::Windows,
        )
        .background(true)
        .auto(true);
        let err = desk
            .run(
                "desk_type",
                json!({"text": "a\nb", "window": "Untitled - Notepad"}),
            )
            .await
            .unwrap_err();
        assert!(
            err.contains("Background delivery is not available"),
            "{err}"
        );
        assert!(sent(&calls, "bring_to_front").is_empty());
        assert!(
            order(&calls).iter().all(|c| c.ends_with("background")),
            "{:?}",
            order(&calls)
        );
    }

    #[tokio::test]
    async fn auto_input_the_driver_could_not_confirm_returns_its_note_without_a_foreground_run() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Mac)
            .background(true)
            .auto(true);
        let out = desk
            .run(
                "desk_type",
                json!({"text": "hi", "window": "Untitled - Notepad"}),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert!(v["unverified"].is_string(), "{v}");
        assert!(v.get("input").is_none(), "{v}");
        assert_eq!(order(&calls), ["type_text background"]);
    }

    #[tokio::test]
    async fn an_auto_drag_on_windows_runs_in_the_foreground_from_the_start() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Windows)
            .background(true)
            .auto(true);
        let out = desk
            .run(
                "desk_drag",
                json!({"x": 200.0, "y": 100.0, "to_x": 300.0, "to_y": 100.0, "window": "Untitled - Notepad"}),
            )
            .await
            .unwrap();
        assert!(
            out.text.contains("\"input\":\"foreground\""),
            "{}",
            out.text
        );
        assert_eq!(order(&calls), ["bring_to_front -", "drag desktop"]);
    }

    struct Spaces(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Spaces {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 4, "window_id": 1, "layer": 0, "title": "Untitled - Notepad", "app_name": "Notepad", "is_on_screen": true, "on_current_space": true, "space_ids": [8]},
                    {"pid": 9, "window_id": 84080, "layer": 0, "title": "微信", "app_name": "WeChat", "is_on_screen": false, "on_current_space": false, "space_ids": [18]},
                    {"pid": 9, "window_id": 727, "layer": 0, "title": "微信", "app_name": "WeChat", "is_on_screen": false, "on_current_space": null, "space_ids": null},
                    {"pid": 9, "window_id": 37333, "layer": 3, "title": "", "app_name": "WeChat", "is_on_screen": false, "on_current_space": false, "space_ids": [18]},
                    {"pid": 5, "window_id": 2, "layer": 0, "title": "Untitled", "app_name": "TextEdit", "is_on_screen": true, "on_current_space": true, "space_ids": [8]},
                    {"pid": 5, "window_id": 3, "layer": 0, "title": "Untitled", "app_name": "TextEdit", "is_on_screen": false, "on_current_space": false, "space_ids": [18]}
                ]}}),
                "type_text" => json!({
                    "content": [{"type": "text", "text": "Sent (unverified) 1 char(s) via CGEvent."}],
                    "structuredContent": {"effect": "unverifiable"}
                }),
                _ => json!({}),
            })
        }
    }

    #[tokio::test]
    async fn background_input_lists_a_window_on_another_space_and_foreground_input_does_not() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let back = Desk::new(|| Ok(Apps), Spaces(Arc::clone(&calls)), Os::Mac).background(true);
        let listed: Vec<Value> =
            serde_json::from_str(&back.run("desk_windows", json!({})).await.unwrap().text).unwrap();
        let other: Vec<&Value> = listed.iter().filter(|w| w["space"] == "other").collect();
        assert_eq!(other.len(), 1, "{listed:?}");
        assert_eq!(
            (
                &other[0]["id"],
                &other[0]["title"],
                &other[0]["pid"],
                &other[0]["active"]
            ),
            (&json!("84080"), &json!("微信"), &json!(9), &json!(false))
        );
        let fore = Desk::new(|| Ok(Apps), Spaces(Arc::clone(&calls)), Os::Mac);
        let listed = fore.run("desk_windows", json!({})).await.unwrap().text;
        assert!(!listed.contains("\"space\""), "{listed}");
    }

    #[tokio::test]
    async fn a_background_snapshot_of_a_window_on_another_space_says_so_by_title_or_id() {
        for (background, auto) in [(false, false), (true, true), (true, false)] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(|| Ok(Apps), Spaces(Arc::clone(&calls)), Os::Mac)
                .background(background)
                .auto(auto);
            let background = background && !auto;
            for args in [json!({"window": "微信"}), json!({"id": "84080"})] {
                let err = desk.run("desk_snapshot", args.clone()).await.unwrap_err();
                assert_eq!(
                    err.contains("is on another Space"),
                    background,
                    "{background} {args} {err}"
                );
            }
            let err = desk
                .run("desk_find", json!({"name": "x", "window": "微信"}))
                .await
                .unwrap_err();
            assert_eq!(
                err.contains("is on another Space"),
                background,
                "{background} {err}"
            );
            let err = desk
                .run("desk_snapshot", json!({"window": "Nope"}))
                .await
                .unwrap_err();
            assert!(!err.contains("Space"), "{err}");
        }
    }

    #[tokio::test]
    async fn background_input_to_a_title_twin_goes_to_the_window_on_a_space_and_says_its_capture_may_be_stale()
     {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Spaces(Arc::clone(&calls)), Os::Mac).background(true);
        let out = desk
            .run("desk_type", json!({"text": "x", "window": "微信"}))
            .await
            .unwrap();
        let typed = sent(&calls, "type_text");
        assert_eq!(typed.len(), 1, "{typed:?}");
        assert_eq!(typed[0]["target"]["window_id"], 84080, "{typed:?}");
        let note: Value = serde_json::from_str(&out.text).unwrap();
        assert!(
            note["unverified"]
                .as_str()
                .is_some_and(|n| n.contains(OFF_SPACE)),
            "{note}"
        );
    }

    #[tokio::test]
    async fn background_input_to_title_twins_on_two_spaces_is_refused_and_names_both() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Spaces(Arc::clone(&calls)), Os::Mac).background(true);
        let err = desk
            .run("desk_type", json!({"text": "x", "window": "Untitled"}))
            .await
            .unwrap_err();
        assert!(
            err.contains("2 on this Space") && err.contains("3 on another Space"),
            "{err}"
        );
        assert!(sent(&calls, "type_text").is_empty());
    }

    fn sent(calls: &Arc<Mutex<Vec<(String, Value)>>>, tool: &str) -> Vec<Value> {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _)| t == tool)
            .map(|(_, a)| a.clone())
            .collect()
    }

    #[tokio::test]
    async fn background_input_goes_to_the_named_window_without_raising_it() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Mac).background(true);
        let window = json!({"kind": "window", "pid": 4, "window_id": 1});
        desk.run(
            "desk_key",
            json!({"keys": "ctrl+s", "window": "Untitled - Notepad"}),
        )
        .await
        .unwrap();
        desk.run("desk_type", json!({"text": "a\nb", "pid": 4}))
            .await
            .unwrap();
        desk.run(
            "desk_click",
            json!({"x": 300.0, "y": 250.0, "window": "Untitled"}),
        )
        .await
        .unwrap();
        desk.run(
            "desk_scroll",
            json!({"direction": "down", "x": 300.0, "y": 250.0, "pid": 4}),
        )
        .await
        .unwrap();
        desk.run(
            "desk_drag",
            json!({"x": 200.0, "y": 100.0, "to_x": 400.0, "to_y": 150.0, "pid": 4}),
        )
        .await
        .unwrap();
        for tool in ["hotkey", "type_text", "press_key", "scroll", "drag"] {
            let all = sent(&calls, tool);
            assert!(!all.is_empty(), "{tool}");
            for a in all {
                assert_eq!(a["target"], window, "{tool} {a}");
                assert_eq!(a["delivery_mode"], "background", "{tool} {a}");
                assert_eq!(a["session"], WINDOW_SESSION, "{tool} {a}");
                assert!(a.get("scope").is_none(), "{tool} {a}");
            }
        }
        let click = sent(&calls, "click");
        assert_eq!(click.len(), 1, "{click:?}");
        assert_eq!(
            (
                &click[0]["session"],
                &click[0]["pid"],
                &click[0]["window_id"],
                &click[0]["delivery_mode"]
            ),
            (
                &json!("desk-window"),
                &json!(4),
                &json!(1),
                &json!("background")
            )
        );
        assert_eq!(
            (&click[0]["x"], &click[0]["y"]),
            (&json!(200.0), &json!(200.0))
        );
        assert_eq!(sent(&calls, "scroll")[0]["x"], 200.0);
        assert_eq!(
            (
                &sent(&calls, "drag")[0]["from_x"],
                &sent(&calls, "drag")[0]["to_y"]
            ),
            (&json!(100.0), &json!(100.0))
        );
        assert!(sent(&calls, "bring_to_front").is_empty());
    }

    #[tokio::test]
    async fn background_input_without_a_target_or_into_a_window_that_refuses_the_pointer_is_refused()
     {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Mac).background(true);
        for (tool, args) in [
            ("desk_key", json!({"keys": "a"})),
            ("desk_type", json!({"text": "a"})),
            ("desk_scroll", json!({"direction": "down"})),
            (
                "desk_drag",
                json!({"x": 1.0, "y": 1.0, "to_x": 2.0, "to_y": 2.0}),
            ),
        ] {
            let err = desk.run(tool, args).await.unwrap_err();
            assert!(err.contains("needs its target window"), "{tool}: {err}");
        }
        let err = desk
            .run(
                "desk_scroll",
                json!({"direction": "down", "x": 600.0, "window": "Untitled"}),
            )
            .await
            .unwrap_err();
        assert!(err.contains("both x and y"), "{err}");
        for (tool, args) in [
            (
                "desk_click",
                json!({"x": 600.0, "y": 100.0, "window": "Inbox"}),
            ),
            (
                "desk_scroll",
                json!({"direction": "down", "x": 600.0, "y": 100.0, "window": "Inbox"}),
            ),
            (
                "desk_drag",
                json!({"x": 600.0, "y": 100.0, "to_x": 700.0, "to_y": 100.0, "window": "Inbox"}),
            ),
        ] {
            let err = desk.run(tool, args).await.unwrap_err();
            assert!(
                err.contains("no pointer input without the foreground"),
                "{tool}: {err}"
            );
        }
        for tool in [
            "hotkey",
            "type_text",
            "press_key",
            "scroll",
            "drag",
            "click",
            "bring_to_front",
        ] {
            assert!(sent(&calls, tool).is_empty(), "{tool}");
        }
    }

    #[tokio::test]
    async fn background_input_the_driver_cannot_confirm_says_so_and_foreground_input_does_not() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let back = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Mac).background(true);
        let typed: Value = serde_json::from_str(
            &back
                .run("desk_type", json!({"text": "a", "pid": 4}))
                .await
                .unwrap()
                .text,
        )
        .unwrap();
        assert_eq!(
            typed["unverified"],
            "unverifiable: Sent 1 char(s) via PostMessage (not verified). (escalation: delivery_failed, to foreground)"
        );
        assert_eq!(
            back.run("desk_key", json!({"keys": "enter", "pid": 4}))
                .await
                .unwrap()
                .text,
            OK
        );
        let front = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Mac);
        let typed: Value = serde_json::from_str(
            &front
                .run("desk_type", json!({"text": "a"}))
                .await
                .unwrap()
                .text,
        )
        .unwrap();
        assert_eq!(typed, json!({"ok": true, "chars": 1}));
    }

    #[tokio::test]
    async fn macos_foreground_typing_sets_the_driver_delay_for_text_in_the_basic_plane_only() {
        for (os, background, text, delay) in [
            (Os::Mac, false, "ab\u{4e2d}\ncd", Some(MAC_TYPE_DELAY_MS)),
            (Os::Mac, true, "ab\u{4e2d}\ncd", None),
            (Os::Mac, false, "a\u{1f600}", None),
            (Os::Windows, false, "ab\u{4e2d}\ncd", None),
        ] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk =
                Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), os).background(background);
            desk.run("desk_type", json!({"text": text, "pid": 4}))
                .await
                .unwrap();
            let typed = sent(&calls, "type_text");
            assert!(!typed.is_empty(), "{os:?}");
            for req in typed {
                assert_eq!(req["delay_ms"].as_u64(), delay, "{os:?} {req}");
            }
        }
    }

    #[tokio::test]
    async fn a_background_element_click_goes_to_the_elements_own_window_under_the_window_that_covers_it()
     {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(
            || {
                let mut f = notepad();
                f.add(
                    "orphan",
                    None,
                    "button",
                    &["showing"],
                    Some([10, 10, 20, 20]),
                );
                Ok(f)
            },
            Overlaid(Arc::clone(&calls)),
            Os::Mac,
        )
        .background(true);
        let out = desk
            .run("desk_act", json!({"id": "ok", "action": "click"}))
            .await
            .unwrap();
        assert!(
            out.text.contains(r#""unverified":"unverifiable: "#),
            "{}",
            out.text
        );
        let clicks = sent(&calls, "click");
        assert_eq!(clicks.len(), 1, "{clicks:?}");
        assert_eq!(
            (
                &clicks[0]["pid"],
                &clicks[0]["window_id"],
                &clicks[0]["x"],
                &clicks[0]["y"]
            ),
            (&json!(4), &json!(1), &json!(140.0), &json!(110.0))
        );
        let err = desk
            .run("desk_act", json!({"id": "orphan", "action": "click"}))
            .await
            .unwrap_err();
        assert!(err.contains("own window could not be found"), "{err}");
        assert_eq!(sent(&calls, "click").len(), 1);
    }

    #[tokio::test]
    async fn a_background_element_click_tells_same_titled_windows_apart_by_frame_or_refuses() {
        fn untitled() -> Result<Fake, String> {
            let mut f = notepad();
            f.untitled = true;
            Ok(f)
        }
        click_one_of_twins("Untitled - Notepad", || Ok(notepad())).await;
        click_one_of_twins("", untitled).await;
        click_one_of_twins("Untitled - Notepad (2)", || Ok(notepad())).await;
    }

    async fn click_one_of_twins(title: &'static str, fake: fn() -> Result<Fake, String>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk =
            Desk::new(fake, Twins(Arc::clone(&calls), 900.0, title, 0.0), Os::Mac).background(true);
        desk.run("desk_act", json!({"id": "ok", "action": "click"}))
            .await
            .unwrap();
        let clicks = sent(&calls, "click");
        assert_eq!(
            (clicks.len(), &clicks[0]["window_id"]),
            (1, &json!(1)),
            "{title:?} {clicks:?}"
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk =
            Desk::new(fake, Twins(Arc::clone(&calls), 0.0, title, 0.0), Os::Mac).background(true);
        let err = desk
            .run("desk_act", json!({"id": "ok", "action": "click"}))
            .await
            .unwrap_err();
        assert!(err.contains("own window is ambiguous"), "{title:?} {err}");
        assert!(sent(&calls, "click").is_empty(), "{title:?}");
    }

    #[tokio::test]
    async fn only_windows_allows_a_resize_border_between_the_element_frame_and_its_window() {
        for (os, clicks) in [(Os::Mac, 1), (Os::Linux, 1), (Os::Windows, 0)] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(
                || Ok(notepad()),
                Twins(Arc::clone(&calls), 0.0, "Untitled - Notepad", 10.0),
                os,
            )
            .background(true);
            let out = desk
                .run("desk_act", json!({"id": "ok", "action": "click"}))
                .await;
            let sent = sent(&calls, "click");
            assert_eq!(sent.len(), clicks, "{os:?}: {out:?}");
            if clicks == 1 {
                assert_eq!(sent[0]["window_id"], json!(1), "{os:?}");
            }
        }
    }

    #[tokio::test]
    async fn a_background_focus_action_refuses_on_windows_where_it_fronts_the_window() {
        let back = Desk::new(|| Ok(notepad()), Slow(Arc::default()), Os::Windows).background(true);
        let mac = Desk::new(|| Ok(notepad()), Slow(Arc::default()), Os::Mac).background(true);
        for action in ["focus", "select", "action"] {
            let err = back
                .run("desk_act", json!({"id": "ok", "action": action}))
                .await
                .unwrap_err();
            assert!(
                err.contains("only by bringing its window to the front"),
                "{action}: {err}"
            );
            assert!(
                mac.run("desk_act", json!({"id": "ok", "action": action}))
                    .await
                    .is_ok(),
                "{action}"
            );
        }
    }

    #[tokio::test]
    async fn a_background_element_click_checks_the_frame_of_a_lone_or_fuzzy_title_match() {
        for (os, title, x, scale, border, clicks) in [
            (Os::Mac, "Untitled - Notepad", 900.0, 1.0, 0.0, 0),
            (Os::Mac, "Untitled - Notepad (2)", 900.0, 1.0, 0.0, 0),
            (Os::Mac, "Untitled - Notepad (2)", 0.0, 1.0, 0.0, 1),
            (Os::Mac, "Untitled - Notepad", 0.0, 2.0, 0.0, 1),
            (Os::Mac, "Untitled - Notepad", 450.0, 2.0, 0.0, 0),
            (Os::Mac, "Untitled - Notepad", 0.0, 1.0, 5.0, 0),
            (Os::Windows, "Untitled - Notepad", 0.0, 1.0, 5.0, 1),
            (Os::Windows, "Untitled - Notepad", 0.0, 1.0, 20.0, 0),
        ] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(
                || Ok(notepad()),
                Solo(Arc::clone(&calls), title, x, scale, border),
                os,
            )
            .background(true);
            let out = desk
                .run("desk_act", json!({"id": "ok", "action": "click"}))
                .await;
            assert_eq!(
                sent(&calls, "click").len(),
                clicks,
                "{title} at {x}: {out:?}"
            );
            if clicks == 0 {
                assert!(
                    out.unwrap_err().contains("own window is ambiguous"),
                    "{title} at {x}"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_allowlist_gates_named_background_input_on_its_target_app_not_on_what_covers_it() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Overlaid(Arc::clone(&calls)), Os::Mac)
            .allow(&["notepad".into()])
            .background(true);
        let notepad = json!({"x": 50.0, "y": 600.0, "window": "Untitled"});
        desk.run("desk_click", notepad.clone()).await.unwrap();
        let mut scroll = notepad.clone();
        scroll["direction"] = "down".into();
        desk.run("desk_scroll", scroll).await.unwrap();
        let mut drag = notepad;
        drag["to_x"] = 60.0.into();
        drag["to_y"] = 600.0.into();
        desk.run("desk_drag", drag).await.unwrap();
        for tool in ["click", "scroll", "drag"] {
            let all = sent(&calls, tool);
            assert_eq!(all.len(), 1, "{tool}");
            let pid = all[0]["pid"].as_i64().or(all[0]["target"]["pid"].as_i64());
            assert_eq!(pid, Some(4), "{tool} {all:?}");
        }
        assert!(
            desk.run(
                "desk_click",
                json!({"x": 50.0, "y": 600.0, "window": "Inbox"})
            )
            .await
            .is_err()
        );
        assert_eq!(sent(&calls, "click").len(), 1);
    }

    #[tokio::test]
    async fn foreground_input_ignores_a_named_window_and_goes_to_the_desktop() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Mac);
        desk.run("desk_key", json!({"keys": "ctrl+s", "window": "Inbox"}))
            .await
            .unwrap();
        assert_eq!(
            sent(&calls, "hotkey"),
            [json!({"keys": ["ctrl", "s"], "scope": "desktop"})]
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Windows);
        let named = json!({"window": "Inbox", "pid": 8});
        for (tool, mut args) in [
            ("desk_type", json!({"text": "a"})),
            ("desk_click", json!({"x": 300.0, "y": 250.0})),
            (
                "desk_scroll",
                json!({"direction": "down", "x": 300.0, "y": 250.0}),
            ),
            (
                "desk_drag",
                json!({"x": 200.0, "y": 100.0, "to_x": 400.0, "to_y": 150.0}),
            ),
        ] {
            args.as_object_mut()
                .unwrap()
                .extend(named.as_object().unwrap().clone());
            desk.run(tool, args).await.unwrap();
        }
        let input: Vec<(String, Value)> = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _)| t != "get_screen_size" && t != "list_windows")
            .cloned()
            .collect();
        assert_eq!(
            input,
            [
                (
                    "press_key".to_owned(),
                    json!({"key": "a", "scope": "desktop"})
                ),
                (
                    "click".to_owned(),
                    json!({"x": 300.0, "y": 250.0, "scope": "desktop"})
                ),
                (
                    "scroll".to_owned(),
                    json!({"direction": "down", "amount": 3, "x": 300.0, "y": 250.0, "scope": "desktop"})
                ),
                (
                    "drag".to_owned(),
                    json!({"from_x": 200.0, "from_y": 100.0, "to_x": 400.0, "to_y": 150.0, "scope": "desktop"})
                ),
            ]
        );
    }

    struct Slow(Arc<Mutex<Vec<String>>>);

    impl Input for Slow {
        async fn call(&self, _: &str, args: Value) -> Result<Value, String> {
            self.0
                .lock()
                .unwrap()
                .push(args["key"].as_str().unwrap_or_default().to_owned());
            tokio::time::sleep(Duration::from_millis(5)).await;
            Ok(json!({}))
        }
    }

    struct Screen(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Screen {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args.clone()));
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 2.0}}),
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 7, "window_id": 40, "layer": 0, "z_index": 5, "bounds": {"x": 100.0, "y": 50.0, "width": 300.0, "height": 200.0}},
                    {"pid": 8, "window_id": 41, "layer": 0, "z_index": 9, "bounds": {"x": 140.0, "y": 60.0, "width": 50.0, "height": 50.0}},
                    {"pid": 9, "window_id": 42, "layer": 25, "z_index": 30, "bounds": {"x": 0.0, "y": 0.0, "width": 2000.0, "height": 30.0}}
                ]}}),
                "get_window_state" if args["window_id"] == 40 => json!({"structuredContent": {
                    "screenshot_width": 450, "screenshot_height": 300,
                    "window_bounds": {"x": 100.0, "y": 50.0, "width": 300.0, "height": 200.0}
                }}),
                "get_window_state" => json!({"structuredContent": {
                    "screenshot_width": 100, "screenshot_height": 100,
                    "window_bounds": {"x": 140.0, "y": 60.0, "width": 50.0, "height": 50.0}
                }}),
                _ => json!({}),
            })
        }
    }

    #[derive(Default)]
    struct Reads {
        reads: Mutex<usize>,
        clicks: Mutex<usize>,
        forget: Mutex<bool>,
        scale: Mutex<Option<f64>>,
    }

    impl Input for Arc<Reads> {
        async fn call(&self, tool: &str, _: Value) -> Result<Value, String> {
            Ok(match tool {
                "get_screen_size" => {
                    json!({"structuredContent": {"scale_factor": *self.scale.lock().unwrap()}})
                }
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 7, "window_id": 40, "layer": 0, "z_index": 5, "bounds": {"x": 0.0, "y": 0.0, "width": 300.0, "height": 200.0}}
                ]}}),
                "get_window_state" => {
                    *self.reads.lock().unwrap() += 1;
                    json!({"structuredContent": {
                        "screenshot_width": 300, "screenshot_height": 200,
                        "window_bounds": {"x": 0.0, "y": 0.0, "width": 300.0, "height": 200.0}
                    }})
                }
                "click" => {
                    *self.clicks.lock().unwrap() += 1;
                    if std::mem::take(&mut *self.forget.lock().unwrap()) {
                        return Err("screenshot_context_missing: window_never_read".into());
                    }
                    json!({})
                }
                _ => json!({}),
            })
        }
    }

    struct Sheet(Arc<Mutex<Vec<Value>>>);

    impl Input for Sheet {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            Ok(match tool {
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 7, "window_id": 491, "layer": 0, "z_index": 7, "bounds": {"x": 0.0, "y": 0.0, "width": 390.0, "height": 332.0}}
                ]}}),
                "get_window_state" => json!({"structuredContent": {
                    "screenshot_width": 390, "screenshot_height": 332,
                    "window_bounds": {"x": 0.0, "y": 0.0, "width": 390.0, "height": 332.0},
                    "background_input": {"routes": [
                        {"route": "accessibility", "status": "refused"},
                        {"route": "window_pointer", "status": "refused"}
                    ]}
                }}),
                "click" => {
                    self.0.lock().unwrap().push(args);
                    json!({})
                }
                _ => json!({}),
            })
        }
    }

    struct NoConnection(Arc<Mutex<Vec<Value>>>);

    impl Input for NoConnection {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            Ok(match tool {
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 7, "window_id": 40, "layer": 0, "z_index": 5, "bounds": {"x": 0.0, "y": 0.0, "width": 300.0, "height": 200.0}}
                ]}}),
                "get_window_state" => json!({"structuredContent": {
                    "screenshot_width": 300, "screenshot_height": 200,
                    "window_bounds": {"x": 0.0, "y": 0.0, "width": 300.0, "height": 200.0}
                }}),
                "click" if args.get("window_id").is_some() => {
                    return Err(
                        "click: Background input is not available for pid 7: the WindowServer \
                        connection is invalid (SLPSPostEventRecordTo returned 1002). Run the \
                        app-bundle daemon: `open -n -g -a CuaDriver --args serve`."
                            .into(),
                    );
                }
                "click" => {
                    self.0.lock().unwrap().push(args);
                    json!({})
                }
                _ => json!({}),
            })
        }
    }

    struct Apps;

    impl Access for Apps {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            Ok(vec![
                frame("4/1", "Untitled - Notepad", 4, true, Some("Notepad.exe")),
                frame("8/1", "Inbox", 8, false, Some("Mail")),
            ])
        }

        fn off_space(&mut self) -> Vec<Frame> {
            vec![Frame {
                space: Some("other"),
                ..frame("84080", "微信", 9, false, Some("WeChat"))
            }]
        }

        fn tree(&mut self, frame: &Frame, _: Extras) -> Result<Tree, String> {
            Ok(Tree {
                frame: frame.clone(),
                ..Tree::default()
            })
        }

        fn node(&mut self, id: &str, _: Extras, _: usize) -> Result<Node, String> {
            match id {
                "4/5" => Ok(Node {
                    id: id.into(),
                    role: "button",
                    states: vec!["showing"],
                    bounds: Some([680, 190, 40, 20]),
                    ..Node::default()
                }),
                _ => Err(format!("no node {id}")),
            }
        }

        fn ancestors(&mut self, _: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }

        fn at(&mut self, _: &str, x: i32, y: i32) -> Result<Option<String>, String> {
            Ok(((x, y) == (5, 5)).then(|| "8/3".into()))
        }

        fn menu_at(&mut self, x: i32, y: i32) -> Result<Option<String>, String> {
            Ok(((x, y) == (100, 800)).then(|| "8/9".into()))
        }

        fn pid_at(&mut self, x: i32, y: i32) -> Result<Option<u32>, String> {
            Ok(Some(if (x, y) == (50, 600) { 12 } else { 4 }))
        }

        fn pid_of(&mut self, id: &str) -> Result<Option<u32>, String> {
            Ok(id.split('/').next().and_then(|p| p.parse().ok()))
        }

        fn act(&mut self, _: &str, _: Act) -> Result<bool, String> {
            Ok(true)
        }

        fn close(&mut self, _: &Frame) -> Result<(), String> {
            Ok(())
        }
    }

    struct Desktop(Arc<Mutex<Vec<String>>>);

    impl Input for Desktop {
        async fn call(&self, tool: &str, _: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push(tool.to_owned());
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 1.0}}),
                "launch_app" => {
                    json!({"content": [{"type": "text", "text": "Launched TextEdit (pid 7) in background."}]})
                }
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 98, "window_id": 4, "layer": 0, "z_index": 4, "title": "Cua.AgentCursorOverlay.default", "bounds": {"x": 0.0, "y": 900.0, "width": 100.0, "height": 100.0}},
                    {"pid": 4, "window_id": 1, "layer": 0, "z_index": 1, "bounds": {"x": 0.0, "y": 0.0, "width": 1000.0, "height": 1000.0}},
                    {"pid": 8, "window_id": 2, "layer": 0, "z_index": 2, "bounds": {"x": 500.0, "y": 0.0, "width": 500.0, "height": 500.0}}
                ]}}),
                "get_desktop_state" => {
                    let mut png = Vec::new();
                    let mut enc = png::Encoder::new(&mut png, 10, 10);
                    enc.set_color(png::ColorType::Rgb);
                    enc.write_header()
                        .unwrap()
                        .write_image_data(&[255; 300])
                        .unwrap();
                    let mut v = json!({"structuredContent": {"screenshot_width": 10, "screenshot_original_width": 1000}});
                    v["content"] = json!([{"type": "image", "data": B64.encode(&png)}]);
                    v
                }
                _ => json!({}),
            })
        }
    }

    struct Retina(Desktop, Arc<Mutex<Vec<Value>>>);

    impl Input for Retina {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.1
                .lock()
                .unwrap()
                .push(json!({"tool": tool, "args": args}));
            match tool {
                "get_screen_size" => Ok(
                    json!({"structuredContent": {"width": 1280, "height": 800, "scale_factor": 2.0}}),
                ),
                _ => self.0.call(tool, args).await,
            }
        }
    }

    #[tokio::test]
    async fn the_screen_is_in_pixels_on_a_retina_mac_and_as_the_driver_reports_it_elsewhere() {
        for (os, want) in [
            (
                Os::Mac,
                json!({"width": 2560, "height": 1600, "scale_factor": 2.0}),
            ),
            (
                Os::Windows,
                json!({"width": 1280, "height": 800, "scale_factor": 2.0}),
            ),
        ] {
            let desk = Desk::new(
                || Ok(Apps),
                Retina(Desktop(Arc::default()), Arc::default()),
                os,
            );
            let out = desk.run("desk_screen", json!({})).await.unwrap();
            let got: Value = serde_json::from_str(&out.text).unwrap();
            assert_eq!(got, want, "{os:?}");
        }
    }

    #[tokio::test]
    async fn a_shot_captures_in_its_own_driver_session() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(
            || Ok(Apps),
            Retina(Desktop(Arc::default()), calls.clone()),
            Os::Mac,
        );
        desk.run("desk_shot", json!({})).await.unwrap();
        let calls = calls.lock().unwrap();
        let shots: Vec<_> = calls
            .iter()
            .filter(|c| c["tool"] == "get_desktop_state")
            .collect();
        assert!(!shots.is_empty(), "{calls:?}");
        assert!(
            shots
                .iter()
                .all(|c| c["args"]["session"] == CAPTURE_SESSION),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn only_a_macos_launch_reports_that_it_stays_in_the_background() {
        for (os, background) in [(Os::Mac, true), (Os::Windows, false)] {
            let desk = Desk::new(|| Ok(Apps), Desktop(Arc::default()), os);
            let out = desk
                .run("desk_launch", json!({"app": "TextEdit"}))
                .await
                .unwrap();
            let v: Value = serde_json::from_str(&out.text).unwrap();
            assert_eq!(v["background"] == true, background, "{os:?} {v}");
            assert_eq!(v["launched"], "Launched TextEdit (pid 7) in background.");
        }
    }

    static LAUNCHED: AtomicBool = AtomicBool::new(false);

    struct Launched;

    impl Access for Launched {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            Ok(if LAUNCHED.load(Ordering::SeqCst) {
                vec![frame("7/1", "Untitled", 7, true, Some("TextEdit"))]
            } else {
                Vec::new()
            })
        }

        fn tree(&mut self, frame: &Frame, _: Extras) -> Result<Tree, String> {
            Ok(Tree {
                frame: frame.clone(),
                nodes: Vec::new(),
                truncated: false,
                phases: Vec::new(),
            })
        }

        fn node(&mut self, id: &str, _: Extras, _: usize) -> Result<Node, String> {
            Err(format!("element {id} is gone"))
        }

        fn ancestors(&mut self, _: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }

        fn at(&mut self, _: &str, _: i32, _: i32) -> Result<Option<String>, String> {
            Ok(None)
        }

        fn act(&mut self, _: &str, _: Act) -> Result<bool, String> {
            Ok(false)
        }

        fn close(&mut self, _: &Frame) -> Result<(), String> {
            Ok(())
        }
    }

    struct Launcher(Arc<Mutex<Vec<String>>>);

    impl Input for Launcher {
        async fn call(&self, tool: &str, _: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push(tool.to_owned());
            Ok(match tool {
                "launch_app" => {
                    LAUNCHED.store(true, Ordering::SeqCst);
                    json!({"content": [{"type": "text", "text": "Launched TextEdit (pid 7) in background."}]})
                }
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 7, "window_id": 9, "title": "Untitled", "app_name": "TextEdit"}
                ]}}),
                "bring_to_front" => json!({"structuredContent": {
                    "path": "already_front", "observed": {"workspace_frontmost_pid": 3}
                }}),
                _ => json!({}),
            })
        }
    }

    #[tokio::test]
    async fn a_plan_focuses_a_window_it_launched_on_a_macos_desk_only() {
        let runtime = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/lib");
        for (os, focused) in [(Os::Mac, true), (Os::Windows, false)] {
            LAUNCHED.store(false, Ordering::SeqCst);
            let dir = tempfile::tempdir().unwrap();
            let endpoint = if cfg!(windows) {
                format!(r"\\.\pipe\shuttle-launch-{os:?}-{}", std::process::id())
            } else {
                dir.path().join("desk.sock").to_string_lossy().into_owned()
            };
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(|| Ok(Launched), Launcher(calls.clone()), os);
            let listener = crate::server::bind(&endpoint).await.unwrap();
            let served = tokio::spawn(crate::server::serve(Arc::new(desk), listener));
            std::fs::write(
                dir.path().join("params.json"),
                json!({"steps": [{"app": "TextEdit"}]}).to_string(),
            )
            .unwrap();
            let mut node = std::process::Command::new("node");
            node.arg(runtime.join("entry.mts"))
                .env("SHUTTLE_SCRIPT", runtime.join("gui_plan.mts"))
                .env("SHUTTLE_RUN", dir.path())
                .env("SHUTTLE_FLOWS", dir.path().join("flows"))
                .env("SHUTTLE_SOCKET", &endpoint)
                .env("HOME", dir.path());
            let status = tokio::task::spawn_blocking(move || node.status())
                .await
                .unwrap()
                .unwrap();
            served.abort();
            let result = std::fs::read_to_string(dir.path().join("result.json"))
                .or_else(|_| std::fs::read_to_string(dir.path().join("error.json")))
                .unwrap();
            assert!(status.success(), "{os:?}: {result}");
            let calls = calls.lock().unwrap().clone();
            assert_eq!(
                calls.iter().any(|c| c == "bring_to_front"),
                focused,
                "{os:?}: {calls:?} {result}"
            );
            let runs = std::fs::read_to_string(dir.path().join("flows/.runs.jsonl")).unwrap();
            assert_eq!(
                runs.contains(r#""focused":"already_front front 3""#),
                focused,
                "{os:?}: {runs}"
            );
        }
    }

    fn pixels(out: &Output) -> Vec<[u8; 3]> {
        let png = B64.decode(out.image_b64.as_deref().unwrap()).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf[..info.buffer_size()]
            .chunks(3)
            .map(|p| [p[0], p[1], p[2]])
            .collect()
    }

    async fn wait_on<B: Access + 'static>(
        desk: &Desk<B, Slow>,
        timeout_ms: u64,
    ) -> (Value, Duration) {
        let since: Value =
            serde_json::from_str(&desk.run("desk_windows", json!({})).await.unwrap().text).unwrap();
        let started = desk.now();
        let out = desk
            .run(
                "desk_wait",
                json!({"since": since, "timeout_ms": timeout_ms}),
            )
            .await
            .unwrap();
        (
            serde_json::from_str(&out.text).unwrap(),
            desk.now() - started,
        )
    }

    async fn clocked(mut desk: Desk<Stub, Slow>) -> Desk<Stub, Slow> {
        desk.skew = desk
            .access
            .run(CALL_LIMIT, (), |b| {
                Ok(b.opens.as_ref().map(|o| o.skew.clone()).unwrap_or_default())
            })
            .await
            .unwrap();
        desk
    }

    async fn lists(desk: &Desk<Stub, Slow>) -> usize {
        desk.access
            .run(CALL_LIMIT, (), |b| {
                Ok(b.opens.as_ref().map_or(0, |o| o.lists))
            })
            .await
            .unwrap()
    }

    #[test]
    fn the_outline_shows_states_shortcuts_ids_and_values() {
        let nodes = vec![
            Node {
                id: "42-1".into(),
                role: "dialog",
                name: "Font".into(),
                ..Node::default()
            },
            Node {
                id: "42-1-7".into(),
                role: "checkbox",
                name: "Bold \"b\"".into(),
                states: vec!["checked"],
                depth: 1,
                parent: Some(0),
                shortcut: Some("ctrl+b".into()),
                ..Node::default()
            },
            Node {
                role: "checkbox",
                name: "Mixed".into(),
                states: vec!["mixed", "disabled"],
                depth: 1,
                parent: Some(0),
                value: Some("12".into()),
                ..Node::default()
            },
        ];
        assert_eq!(
            outline(&nodes, 80),
            "- dialog \"Font\" [id=42-1]\n  - checkbox \"Bold \\\"b\\\"\" [checked] [ctrl+b] [id=42-1-7]\n  - checkbox \"Mixed\" [disabled] [mixed]: \"12\"\n"
        );
    }

    #[tokio::test]
    async fn closing_one_of_two_same_titled_windows_closes_the_active_one() {
        let twins = || {
            Ok(Stub {
                frames: vec![
                    frame("front", "Untitled - Notepad", 4, true, None),
                    frame("back", "Untitled - Notepad", 4, false, None),
                ],
                ..Stub::default()
            })
        };
        let desk = Desk::new(twins, Slow(Arc::default()), Os::Mac);
        desk.run("desk_close", json!({"window": "Untitled - Notepad"}))
            .await
            .unwrap();
        assert_eq!(closed(&desk).await, ["front"]);
    }

    #[tokio::test]
    async fn a_snapshot_takes_the_menu_bar_only_when_asked_and_find_always_searches_it() {
        let desk = Desk::new(|| Ok(notepad()), Slow(Arc::default()), Os::Mac);
        desk.run("desk_snapshot", json!({})).await.unwrap();
        desk.run("desk_snapshot", json!({"menus": true}))
            .await
            .unwrap();
        desk.run("desk_find", json!({"name": "Save"}))
            .await
            .unwrap();
        let menus = desk
            .access
            .run(CALL_LIMIT, (), |b| Ok(b.menus.clone()))
            .await
            .unwrap();
        assert_eq!(menus, [false, true, true]);
    }

    #[tokio::test]
    async fn a_window_kept_from_an_earlier_list_is_listed_but_never_acted_on() {
        let busy = || {
            Ok(Stub {
                frames: vec![Frame {
                    stale: true,
                    ..frame("w", "Draft", 4, true, None)
                }],
                ..Stub::default()
            })
        };
        let desk = Desk::new(busy, Slow(Arc::default()), Os::Mac);
        let listed = desk.run("desk_windows", json!({})).await.unwrap();
        assert!(listed.text.contains("Draft"), "{}", listed.text);
        for (tool, args) in [
            ("desk_close", json!({"window": "Draft"})),
            ("desk_snapshot", json!({"window": "Draft"})),
            ("desk_find", json!({"name": "Save", "window": "Draft"})),
        ] {
            let err = desk.run(tool, args).await.unwrap_err();
            assert!(err.contains("stale"), "{tool}: {err}");
        }
        assert!(closed(&desk).await.is_empty());
    }

    #[tokio::test]
    async fn a_call_that_names_a_process_reads_only_that_apps_windows() {
        let desk = Desk::new(|| Ok(one_app()), Slow(Arc::default()), Os::Mac);
        for (tool, args) in PID_CALLS {
            desk.run(tool, serde_json::from_str(args).unwrap())
                .await
                .unwrap();
        }
        assert!(desk.run("desk_snapshot", json!({})).await.is_err());
    }

    #[tokio::test]
    async fn with_an_allowlist_a_call_that_names_a_process_still_reads_every_window() {
        let desk =
            Desk::new(|| Ok(one_app()), Slow(Arc::default()), Os::Mac).allow(&["Notepad".into()]);
        for (tool, args) in PID_CALLS {
            let err = desk
                .run(tool, serde_json::from_str(args).unwrap())
                .await
                .unwrap_err();
            assert_eq!(err, "every app was read", "{tool}");
        }
    }

    #[tokio::test]
    async fn two_typing_calls_at_once_type_one_after_the_other() {
        let keys = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(notepad()), Slow(keys.clone()), Os::Windows);
        let (a, b) = tokio::join!(
            desk.run("desk_type", json!({"text": "aaa"})),
            desk.run("desk_type", json!({"text": "bbb"}))
        );
        a.unwrap();
        b.unwrap();
        let got = keys.lock().unwrap().join("");
        assert!(got == "aaabbb" || got == "bbbaaa", "{got}");
    }

    #[tokio::test]
    async fn an_action_waits_a_bounded_time_for_a_running_one_then_says_the_desk_is_busy() {
        let keys = Arc::new(Mutex::new(Vec::new()));
        let mut desk = Desk::new(|| Ok(notepad()), Slow(keys.clone()), Os::Windows);
        desk.turn_wait = Duration::from_millis(50);
        let long = "a".repeat(100);
        let (typed, busy) = tokio::join!(desk.run("desk_type", json!({"text": long})), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            desk.run("desk_key", json!({"keys": "ctrl+s"})).await
        });
        typed.unwrap();
        assert_eq!(
            busy.unwrap_err(),
            "desk busy: a previous action is still running"
        );
        assert_eq!(keys.lock().unwrap().join(""), long);
    }

    #[tokio::test]
    async fn keys_and_text_go_to_the_desktop_in_driver_names() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(notepad()), Recorder(calls.clone()), Os::Mac);
        desk.run("desk_key", json!({"keys": "Super+Shift+S"}))
            .await
            .unwrap();
        desk.run("desk_type", json!({"text": "a\nb"}))
            .await
            .unwrap();
        let got = calls.lock().unwrap().clone();
        assert_eq!(
            got,
            [
                (
                    "hotkey".to_owned(),
                    json!({"keys": ["cmd", "shift", "s"], "scope": "desktop"})
                ),
                (
                    "type_text".to_owned(),
                    json!({"text": "a", "scope": "desktop", "delay_ms": MAC_TYPE_DELAY_MS})
                ),
                (
                    "press_key".to_owned(),
                    json!({"key": "return", "scope": "desktop"})
                ),
                (
                    "type_text".to_owned(),
                    json!({"text": "b", "scope": "desktop", "delay_ms": MAC_TYPE_DELAY_MS})
                ),
            ]
        );
    }

    #[tokio::test]
    async fn a_focused_lookup_that_never_answers_ends_within_the_settle_and_frees_the_desk() {
        let desk = Desk::new(
            || {
                Ok(Stub {
                    frames: vec![frame("w", "Untitled - Notepad", 4, true, Some("Notepad"))],
                    stalled: true,
                    ..Stub::default()
                })
            },
            Recorder(Arc::default()),
            Os::Linux,
        )
        .with_typer(|_, _| Ok(true));
        let started = Instant::now();
        desk.run("desk_type", json!({"text": "hello"}))
            .await
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        let started = Instant::now();
        desk.run("desk_windows", json!({})).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        let budgets = desk
            .access
            .run(CALL_LIMIT, (), |b| Ok(b.budgets.clone()))
            .await
            .unwrap();
        assert_eq!(budgets.len(), 1);
        assert!(budgets[0] <= TYPE_SETTLE, "{budgets:?}");
    }

    #[tokio::test]
    async fn linux_keys_take_keysym_names_and_text_goes_in_runs_of_eight_new_characters() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(notepad()), Recorder(calls.clone()), Os::Linux)
            .with_typer(|_, _| Ok(false));
        desk.run("desk_key", json!({"keys": "Super_L+ctrl++"}))
            .await
            .unwrap();
        desk.run("desk_type", json!({"text": "一二三四五六七八九十\nok"}))
            .await
            .unwrap();
        let got = calls.lock().unwrap().clone();
        assert_eq!(
            got,
            [
                (
                    "hotkey".to_owned(),
                    json!({"keys": ["super", "ctrl", "plus"], "scope": "desktop"})
                ),
                (
                    "type_text".to_owned(),
                    json!({"text": "一二三四五六七八", "scope": "desktop"})
                ),
                (
                    "type_text".to_owned(),
                    json!({"text": "九十", "scope": "desktop"})
                ),
                (
                    "press_key".to_owned(),
                    json!({"key": "return", "scope": "desktop"})
                ),
                (
                    "type_text".to_owned(),
                    json!({"text": "ok", "scope": "desktop"})
                ),
            ]
        );
    }

    static STOPPED: AtomicBool = AtomicBool::new(false);

    #[tokio::test]
    async fn a_linux_line_xtest_types_skips_the_driver_and_a_stop_ends_xtest_at_its_next_key() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(notepad()), Recorder(calls.clone()), Os::Linux)
            .with_typer(|_, _| Ok(true));
        desk.run("desk_type", json!({"text": "hello\nworld"}))
            .await
            .unwrap();
        assert_eq!(
            calls.lock().unwrap().clone(),
            [(
                "press_key".to_owned(),
                json!({"key": "return", "scope": "desktop"})
            )]
        );
        let desk = Desk::new(|| Ok(notepad()), Recorder(calls.clone()), Os::Linux).with_typer(
            |_, stop| {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                std::thread::sleep(Duration::from_millis(50));
                STOPPED.store(true, Ordering::Relaxed);
                Err("stopped by the user".to_owned())
            },
        );
        let typing = desk.run("desk_type", json!({"text": "long text"}));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), typing)
                .await
                .is_err()
        );
        desk.run("desk_key", json!({"keys": "a"})).await.unwrap();
        assert!(STOPPED.load(Ordering::Relaxed));
    }

    #[test]
    fn a_run_repeats_a_character_freely_and_the_typed_tail_is_the_last_line_end() {
        assert_eq!(text_runs("éé abc é"), ["éé abc é"]);
        assert_eq!(text_runs(""), Vec::<&str>::new());
        assert_eq!(
            text_runs("MCP — 你好 café 🙂! 一二三四五六七八九十"),
            ["MCP — 你好 café 🙂! 一二三", "四五六七八九十"]
        );
        assert_eq!(
            text_runs("你你你你你你你你你你好"),
            ["你你你你你你你你你你好"]
        );
        assert_eq!(text_runs("plain"), ["plain"]);
        assert_eq!(typed_tail("x\r\nhello").as_deref(), Some("hello"));
        assert_eq!(typed_tail("line\n"), None);
        assert_eq!(
            typed_tail("abcdefghijklmnopqrstuvwxyz").as_deref(),
            Some("klmnopqrstuvwxyz")
        );
    }

    struct TwoWindows(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for TwoWindows {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"window_id": 1, "title": "Report - Writer", "pid": 4},
                    {"window_id": 2, "title": "Report - Writer", "pid": 8}
                ]}}),
                _ => json!({}),
            })
        }
    }

    #[tokio::test]
    async fn focus_takes_the_active_window_among_equal_titles() {
        for os in [Os::Linux, Os::Mac, Os::Windows] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let two = || {
                Ok(Stub {
                    frames: vec![
                        frame("a", "Report - Writer", 4, true, Some("writer")),
                        frame("b", "Report - Writer", 8, false, Some("writer")),
                    ],
                    ..Stub::default()
                })
            };
            let desk = Desk::new(two, TwoWindows(calls.clone()), os);
            desk.run("desk_focus", json!({"window": "Report - Writer"}))
                .await
                .unwrap();
            assert_eq!(
                calls.lock().unwrap().last().cloned().unwrap(),
                (
                    "bring_to_front".to_owned(),
                    json!({"pid": 4, "window_id": 1})
                ),
                "{os:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_press_click_goes_through_the_driver() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(notepad()), Recorder(calls.clone()), Os::Windows);
        let out = desk.run("desk_press", json!({"id": "ok"})).await.unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out.text).unwrap()["how"],
            "click"
        );
        assert_eq!(
            calls.lock().unwrap().clone(),
            [(
                "click".to_owned(),
                json!({"x": 140, "y": 110, "scope": "desktop"})
            )]
        );
    }

    #[tokio::test]
    async fn a_macos_window_click_without_a_windowserver_connection_goes_to_the_desktop() {
        let clicks = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(notepad()), NoConnection(clicks.clone()), Os::Mac);
        desk.run("desk_click", json!({"x": 20.0, "y": 30.0}))
            .await
            .unwrap();
        assert_eq!(
            *clicks.lock().unwrap(),
            [json!({"x": 20.0, "y": 30.0, "scope": "desktop"})]
        );
    }

    #[tokio::test]
    async fn a_macos_click_into_a_window_refusing_pointer_input_goes_to_the_desktop() {
        let clicks = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(notepad()), Sheet(clicks.clone()), Os::Mac);
        desk.run("desk_click", json!({"x": 20.0, "y": 30.0}))
            .await
            .unwrap();
        assert_eq!(
            *clicks.lock().unwrap(),
            [json!({"x": 20.0, "y": 30.0, "scope": "desktop"})]
        );
    }

    #[tokio::test]
    async fn macos_clicks_read_an_unmoved_window_once_and_again_when_the_driver_forgot_it() {
        let driver = Arc::new(Reads::default());
        let desk = Desk::new(|| Ok(notepad()), driver.clone(), Os::Mac);
        let click = || desk.run("desk_click", json!({"x": 10.0, "y": 10.0}));
        click().await.unwrap();
        click().await.unwrap();
        *driver.forget.lock().unwrap() = true;
        click().await.unwrap();
        assert_eq!(
            (
                *driver.reads.lock().unwrap(),
                *driver.clicks.lock().unwrap()
            ),
            (2, 4)
        );
    }

    #[tokio::test]
    async fn a_macos_click_takes_the_display_scale_of_the_moment() {
        let driver = Arc::new(Reads::default());
        let desk = Desk::new(|| Ok(notepad()), driver.clone(), Os::Mac);
        let click = || desk.run("desk_click", json!({"x": 500.0, "y": 300.0}));
        click().await.unwrap();
        assert_eq!(*driver.reads.lock().unwrap(), 0);
        *driver.scale.lock().unwrap() = Some(2.0);
        click().await.unwrap();
        assert_eq!(*driver.reads.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn a_macos_click_on_an_open_menu_item_runs_it_instead_of_clicking() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let open = || {
            let mut f = notepad();
            f.add("wrap", Some("file"), "menuitem", &["showing"], None);
            f.hits.insert((300, 200), "wrap".into());
            Ok(f)
        };
        let desk = Desk::new(open, Screen(calls.clone()), Os::Mac);
        desk.run("desk_click", json!({"x": 300.0, "y": 200.0}))
            .await
            .unwrap();
        assert!(ACTED.lock().unwrap().iter().any(|id| id == "wrap"));
        assert!(!calls.lock().unwrap().iter().any(|(t, _)| t == "click"));
    }

    #[tokio::test]
    async fn a_macos_click_on_a_menu_item_that_takes_no_action_fails() {
        let open = || {
            let mut f = notepad();
            f.add("dead", Some("file"), "menuitem", &["showing"], None);
            f.hits.insert((300, 200), "dead".into());
            Ok(f)
        };
        let desk = Desk::new(open, Screen(Arc::default()), Os::Mac);
        let out = desk
            .run("desk_click", json!({"x": 300.0, "y": 200.0}))
            .await;
        assert!(out.is_err(), "{out:?}");
    }

    #[tokio::test]
    async fn an_app_allowlist_hides_and_refuses_the_windows_of_other_apps() {
        let desk =
            Desk::new(|| Ok(Apps), Screen(Arc::default()), Os::Mac).allow(&["NOTEPAD.EXE".into()]);
        let refused = |r: Result<Output, String>| {
            r.err()
                .is_some_and(|e| e.contains("not in the app allowlist"))
        };
        let listed = desk.run("desk_windows", json!({})).await.unwrap().text;
        assert!(
            listed.contains("Notepad") && !listed.contains("Inbox"),
            "{listed}"
        );
        assert!(
            desk.run("desk_snapshot", json!({"window": "Notepad"}))
                .await
                .is_ok()
        );
        assert!(
            desk.run("desk_snapshot", json!({"window": "Inbox"}))
                .await
                .is_err_and(|e| e.ends_with("windows: [\"Untitled - Notepad\"]"))
        );
        assert!(refused(desk.run("desk_press", json!({"id": "8/5"})).await));
        assert!(!refused(desk.run("desk_press", json!({"id": "4/5"})).await));
        assert!(refused(
            desk.run("desk_click", json!({"x": 300.0, "y": 200.0}))
                .await
        ));
        assert!(refused(
            desk.run("desk_launch", json!({"app": "Mail"})).await
        ));
        assert!(
            desk.run("desk_key", json!({"keys": "ctrl+s"}))
                .await
                .is_ok()
        );
        let mail = Desk::new(|| Ok(Apps), Screen(Arc::default()), Os::Mac).allow(&["mail".into()]);
        assert!(refused(
            mail.run("desk_key", json!({"keys": "ctrl+s"})).await
        ));
        assert!(refused(mail.run("desk_type", json!({"text": "x"})).await));
    }

    #[tokio::test]
    async fn an_app_allowlist_gates_every_point_and_element_a_call_reaches() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk =
            Desk::new(|| Ok(Apps), Desktop(calls.clone()), Os::Mac).allow(&["notepad".into()]);
        let refused = |r: Result<Output, String>| {
            r.err()
                .is_some_and(|e| e.contains("not in the app allowlist"))
        };
        assert!(refused(desk.run("desk_at", json!({"x": 5, "y": 5})).await));
        assert!(refused(
            desk.run("desk_act", json!({"id": "4/5", "action": "click"}))
                .await
        ));
        assert!(refused(
            desk.run("desk_click", json!({"x": 100.0, "y": 800.0}))
                .await
        ));
        assert!(!calls.lock().unwrap().iter().any(|t| t == "click"));
        let windows =
            Desk::new(|| Ok(Apps), Desktop(calls.clone()), Os::Windows).allow(&["notepad".into()]);
        windows
            .run("desk_click", json!({"x": 100.0, "y": 800.0}))
            .await
            .unwrap();
        assert!(refused(
            windows
                .run("desk_click", json!({"x": 50.0, "y": 950.0}))
                .await
        ));
        assert!(refused(
            windows
                .run("desk_click", json!({"x": 50.0, "y": 600.0}))
                .await
        ));
        assert!(calls.lock().unwrap().iter().any(|t| t == "click"));
        let missed = desk
            .run("desk_snapshot", json!({"window": "Nope"}))
            .await
            .unwrap_err();
        assert!(
            missed.contains("Untitled - Notepad") && !missed.contains("Inbox"),
            "{missed}"
        );
    }

    #[tokio::test]
    async fn an_app_allowlist_masks_shots_outside_its_windows_and_costs_nothing_without_one() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let open = Desk::new(|| Ok(Apps), Desktop(calls.clone()), Os::Windows);
        let all = pixels(&open.run("desk_shot", json!({})).await.unwrap());
        assert!(all.iter().all(|p| *p == [255; 3]));
        assert_eq!(*calls.lock().unwrap(), ["get_desktop_state"]);
        let masked =
            Desk::new(|| Ok(Apps), Desktop(Arc::default()), Os::Windows).allow(&["notepad".into()]);
        let shot = pixels(&masked.run("desk_shot", json!({})).await.unwrap());
        assert_eq!(
            (shot[2], shot[7], shot[97], shot[90]),
            ([255; 3], [0; 3], [255; 3], [0; 3])
        );
        let crop = masked
            .run(
                "desk_shot",
                json!({"x": 600.0, "y": 0.0, "w": 400.0, "h": 400.0}),
            )
            .await
            .unwrap();
        assert!(pixels(&crop).iter().all(|p| *p == [0; 3]));
    }

    #[tokio::test]
    async fn a_shot_tells_the_backend_whether_the_driver_may_show_its_overlay() {
        for shown in [true, false] {
            let desk = Desk::new(|| Ok(Stub::default()), Desktop(Arc::default()), Os::Linux)
                .overlay(shown);
            desk.run("desk_shot", json!({})).await.unwrap();
            let seen = desk
                .access
                .run(CALL_LIMIT, (), |b| Ok(b.overlays.clone()))
                .await
                .unwrap();
            assert_eq!(seen, [shown]);
        }
    }

    #[tokio::test]
    async fn a_linux_shot_from_the_driver_answers_the_image_and_writes_its_path() {
        let desk = Desk::new(|| Ok(Apps), Desktop(Arc::default()), Os::Linux);
        let shot = desk.run("desk_shot", json!({})).await.unwrap();
        let text: Value = serde_json::from_str(&shot.text).unwrap();
        assert_eq!(text, json!({"ok": true, "width": 10, "height": 10}));
        assert!(shot.image_b64.is_some());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        let shot = desk
            .run("desk_shot", json!({"path": path.to_string_lossy()}))
            .await
            .unwrap();
        let text: Value = serde_json::from_str(&shot.text).unwrap();
        assert_eq!(text["path"].as_str(), Some(&*path.to_string_lossy()));
        assert!(text.get("png_b64").is_none(), "{text}");
        assert_eq!(
            B64.encode(std::fs::read(&path).unwrap()),
            shot.image_b64.unwrap()
        );
    }

    #[tokio::test]
    async fn a_macos_click_targets_the_top_window_under_the_point_in_its_pixels() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(notepad()), Screen(calls.clone()), Os::Mac);
        desk.run("desk_click", json!({"x": 300.0, "y": 200.0}))
            .await
            .unwrap();
        desk.run("desk_click", json!({"x": 260.0, "y": 300.0, "count": 2}))
            .await
            .unwrap();
        desk.run("desk_click", json!({"x": 5000.0, "y": 5000.0}))
            .await
            .unwrap();
        let sent: Vec<(String, Value)> = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _)| t == "click" || t == "get_window_state" || t == "bring_to_front")
            .cloned()
            .collect();
        let window =
            |pid: u32, id: u32| json!({"session": WINDOW_SESSION, "pid": pid, "window_id": id});
        assert_eq!(
            sent,
            [
                (
                    "bring_to_front".to_owned(),
                    json!({"pid": 8, "window_id": 41})
                ),
                ("get_window_state".to_owned(), window(8, 41)),
                (
                    "click".to_owned(),
                    json!({"x": 20.0, "y": 80.0, "session": WINDOW_SESSION, "pid": 8, "window_id": 41})
                ),
                (
                    "bring_to_front".to_owned(),
                    json!({"pid": 7, "window_id": 40})
                ),
                ("get_window_state".to_owned(), window(7, 40)),
                (
                    "click".to_owned(),
                    json!({"x": 45.0, "y": 150.0, "count": 2, "session": WINDOW_SESSION, "pid": 7, "window_id": 40})
                ),
                (
                    "click".to_owned(),
                    json!({"x": 5000.0, "y": 5000.0, "scope": "desktop"})
                ),
            ]
        );
        let sizes = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _)| t == "get_screen_size")
            .count();
        assert_eq!(sizes, 3);
    }

    #[tokio::test]
    async fn a_window_names_its_app_when_the_backend_knows_it() {
        let desk = Desk::new(|| Ok(notepad()), Screen(Arc::default()), Os::Mac);
        let out = desk.run("desk_windows", json!({})).await.unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out.text).unwrap(),
            json!([{"id": "w", "title": "Untitled - Notepad", "role": "window", "pid": 4, "active": true, "app": "Notepad"}])
        );
    }

    #[tokio::test]
    async fn a_wait_returns_the_new_window_list_once_a_window_opens() {
        let desk = Desk::new(
            || Ok(Stub::opens(3, false, 0)),
            Slow(Arc::default()),
            Os::Mac,
        );
        let (out, _) = wait_on(&desk, 2000).await;
        assert_eq!(out["changed"], true);
        assert_eq!(out["windows"][0]["title"], "Save");
        assert_eq!(lists(&desk).await, 3);
    }

    #[tokio::test]
    async fn a_wait_without_a_change_returns_the_same_list_at_its_timeout() {
        let desk = Desk::new(
            || Ok(Stub::opens(usize::MAX, false, 0)),
            Slow(Arc::default()),
            Os::Mac,
        );
        let (out, took) = wait_on(&desk, 150).await;
        assert_eq!(out["changed"], false);
        assert_eq!(out["windows"][0]["title"], "Untitled");
        assert!(took >= Duration::from_millis(150), "{took:?}");
    }

    #[tokio::test]
    async fn slow_window_lists_count_against_a_waits_timeout() {
        let desk = clocked(Desk::new(
            || Ok(Stub::opens(usize::MAX, false, 200)),
            Slow(Arc::default()),
            Os::Mac,
        ))
        .await;
        let (out, took) = wait_on(&desk, 300).await;
        assert_eq!(out["changed"], false);
        assert!(took >= Duration::from_millis(300), "{took:?}");
        assert_eq!(lists(&desk).await, 3);
    }

    #[tokio::test]
    async fn a_read_that_ends_past_the_deadline_ends_the_wait() {
        let desk = clocked(Desk::new(
            || Ok(Stub::opens(usize::MAX, false, 300)),
            Slow(Arc::default()),
            Os::Mac,
        ))
        .await;
        let (out, _) = wait_on(&desk, 150).await;
        assert_eq!(out["changed"], false);
        assert_eq!(lists(&desk).await, 2);
    }

    #[tokio::test]
    async fn a_zero_timeout_wait_reads_the_list_once() {
        let desk = Desk::new(
            || Ok(Stub::opens(usize::MAX, false, 0)),
            Slow(Arc::default()),
            Os::Mac,
        );
        let (out, _) = wait_on(&desk, 0).await;
        assert_eq!(out["changed"], false);
        assert_eq!(lists(&desk).await, 2);
    }

    #[tokio::test]
    async fn a_wait_ended_early_frees_the_desk_at_once() {
        let desk = Arc::new(Desk::new(
            || Ok(Stub::opens(usize::MAX, true, 0)),
            Slow(Arc::default()),
            Os::Mac,
        ));
        let wait = tokio::spawn({
            let desk = desk.clone();
            async move { wait_on(&desk, 5000).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        wait.abort();
        let started = Instant::now();
        desk.run("desk_windows", json!({})).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_hint_change_right_after_a_slow_list_reads_the_list_at_once() {
        let desk = clocked(Desk::new(
            || Ok(Stub::opens(12, true, 150)),
            Slow(Arc::default()),
            Os::Mac,
        ))
        .await;
        let (out, _) = wait_on(&desk, 2000).await;
        assert_eq!(out["changed"], true);
        let seen = desk
            .access
            .run(CALL_LIMIT, (), |b| {
                Ok(b.opens.as_ref().and_then(|o| o.seen))
            })
            .await
            .unwrap();
        assert_eq!(seen, Some(12));
    }

    #[tokio::test]
    async fn a_wait_lists_and_compares_only_the_windows_the_allowlist_admits() {
        let desk =
            Desk::new(|| Ok(Apps), Screen(Arc::default()), Os::Mac).allow(&["notepad.exe".into()]);
        let mut since: Value =
            serde_json::from_str(&desk.run("desk_windows", json!({})).await.unwrap().text).unwrap();
        since.as_array_mut().unwrap().push(json!({"id": "8/1", "title": "Inbox", "role": "window", "pid": 8, "active": false, "app": "Mail"}));
        let out = desk
            .run("desk_wait", json!({"since": since, "timeout_ms": 50}))
            .await
            .unwrap();
        let out: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(out["changed"], false);
        assert!(!out.to_string().contains("Inbox"), "{out}");
    }

    #[tokio::test]
    async fn a_steady_hint_spares_full_window_lists() {
        let desk = Desk::new(
            || Ok(Stub::opens(usize::MAX, true, 0)),
            Slow(Arc::default()),
            Os::Mac,
        );
        let (out, _) = wait_on(&desk, 350).await;
        assert_eq!(out["changed"], false);
        assert!(lists(&desk).await <= 5);
    }

    #[tokio::test]
    async fn a_hint_change_reads_the_window_list_at_once() {
        let desk = Desk::new(
            || Ok(Stub::opens(3, true, 0)),
            Slow(Arc::default()),
            Os::Mac,
        );
        let (out, took) = wait_on(&desk, 2000).await;
        assert_eq!(out["changed"], true);
        assert_eq!(lists(&desk).await, 2);
        assert!(took < WAIT_CHECK, "{took:?}");
    }

    struct Wheel(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Wheel {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 2.0}}),
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 4, "window_id": 1, "layer": 0, "z_index": 1, "title": "Untitled - Notepad", "bounds": {"x": 0.0, "y": 0.0, "width": 400.0, "height": 300.0}},
                    {"pid": 8, "window_id": 2, "layer": 0, "z_index": 2, "title": "Inbox", "bounds": {"x": 500.0, "y": 0.0, "width": 300.0, "height": 300.0}},
                    {"pid": 4, "window_id": 3, "layer": 0, "z_index": 3, "title": "Find", "bounds": {"x": 100.0, "y": 400.0, "width": 200.0, "height": 100.0}},
                    {"pid": 98, "window_id": 9, "layer": 25, "z_index": 9, "title": "Menu"}
                ]}}),
                _ => json!({}),
            })
        }
    }

    struct Renamed(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Renamed {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 2.0}}),
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 4, "window_id": 1, "layer": 0, "z_index": 1, "title": "Notepad", "bounds": {"x": 0.0, "y": 0.0, "width": 400.0, "height": 300.0}},
                    {"pid": 4, "window_id": 3, "layer": 0, "z_index": 3, "title": "Find", "bounds": {"x": 100.0, "y": 400.0, "width": 200.0, "height": 100.0}},
                    {"pid": 8, "window_id": 2, "layer": 0, "z_index": 5, "title": "Inbox", "bounds": {"x": 500.0, "y": 0.0, "width": 300.0, "height": 300.0}}
                ]}}),
                _ => json!({}),
            })
        }
    }

    #[tokio::test]
    async fn a_checked_window_the_driver_titles_differently_falls_back_to_its_apps_topmost_window()
    {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk =
            Desk::new(|| Ok(Apps), Renamed(calls.clone()), Os::Windows).allow(&["notepad".into()]);
        desk.run("desk_scroll", json!({"direction": "down"}))
            .await
            .unwrap();
        assert_eq!(
            calls.lock().unwrap().last().cloned().unwrap(),
            (
                "scroll".to_owned(),
                json!({"direction": "down", "amount": 3, "x": 200.0, "y": 450.0, "scope": "desktop"})
            )
        );
    }

    struct Covered(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Covered {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 4, "window_id": 1, "layer": 0, "z_index": 1, "title": "Untitled - Notepad", "bounds": {"x": 0.0, "y": 0.0, "width": 400.0, "height": 300.0}},
                    {"pid": 8, "window_id": 2, "layer": 0, "z_index": 2, "title": "Inbox", "bounds": {"x": 150.0, "y": 100.0, "width": 100.0, "height": 100.0}}
                ]}}),
                _ => json!({}),
            })
        }
    }

    #[tokio::test]
    async fn a_pointless_windows_scroll_refuses_a_front_window_another_window_covers() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Covered(calls.clone()), Os::Windows)
            .allow(&["notepad".into(), "mail".into()]);
        let err = desk
            .run("desk_scroll", json!({"direction": "down"}))
            .await
            .unwrap_err();
        assert!(err.contains("covers the front window's center"), "{err}");
        assert!(!calls.lock().unwrap().iter().any(|(t, _)| t == "scroll"));
    }

    #[tokio::test]
    async fn a_scroll_wheels_at_its_point_or_at_the_front_window_and_a_drag_spans_two_points() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(
            || Err::<Fake, _>("no accessibility".to_owned()),
            Wheel(calls.clone()),
            Os::Windows,
        );
        let last = || calls.lock().unwrap().last().cloned().unwrap();
        desk.run(
            "desk_scroll",
            json!({"direction": "down", "x": 10.0, "y": 20.0}),
        )
        .await
        .unwrap();
        assert_eq!(
            last(),
            (
                "scroll".to_owned(),
                json!({"direction": "down", "amount": 3, "x": 10.0, "y": 20.0, "scope": "desktop"})
            )
        );
        desk.run("desk_scroll", json!({"direction": "up", "amount": 5}))
            .await
            .unwrap();
        assert_eq!(
            last(),
            (
                "scroll".to_owned(),
                json!({"direction": "up", "amount": 5, "x": 200.0, "y": 450.0, "scope": "desktop"})
            )
        );
        Desk::new(
            || Err::<Fake, _>("no accessibility".to_owned()),
            Wheel(calls.clone()),
            Os::Mac,
        )
        .run("desk_scroll", json!({"direction": "up", "amount": 5}))
        .await
        .unwrap();
        assert_eq!(
            last(),
            (
                "scroll".to_owned(),
                json!({"direction": "up", "amount": 5, "pid": 4, "window_id": 3, "session": WINDOW_SESSION})
            )
        );
        let allowed =
            Desk::new(|| Ok(Apps), Wheel(calls.clone()), Os::Windows).allow(&["notepad".into()]);
        allowed
            .run("desk_scroll", json!({"direction": "down"}))
            .await
            .unwrap();
        assert_eq!(
            last(),
            (
                "scroll".to_owned(),
                json!({"direction": "down", "amount": 3, "x": 200.0, "y": 150.0, "scope": "desktop"})
            )
        );
        let half = desk
            .run("desk_scroll", json!({"direction": "up", "x": 1.0}))
            .await
            .unwrap_err();
        assert!(half.contains("both x and y"), "{half}");
        desk.run(
            "desk_drag",
            json!({"x": 1.0, "y": 2.0, "to_x": 300.0, "to_y": 400.0}),
        )
        .await
        .unwrap();
        assert_eq!(
            last(),
            (
                "drag".to_owned(),
                json!({"from_x": 1.0, "from_y": 2.0, "to_x": 300.0, "to_y": 400.0, "scope": "desktop"})
            )
        );
    }

    #[tokio::test]
    async fn an_app_allowlist_gates_both_ends_of_a_drag_and_the_point_of_a_scroll() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk =
            Desk::new(|| Ok(Apps), Desktop(calls.clone()), Os::Windows).allow(&["notepad".into()]);
        let refused = |r: Result<Output, String>| {
            r.err()
                .is_some_and(|e| e.contains("not in the app allowlist"))
        };
        let drag = |to: (f64, f64)| json!({"x": 100.0, "y": 800.0, "to_x": to.0, "to_y": to.1});
        assert!(refused(desk.run("desk_drag", drag((50.0, 950.0))).await));
        assert!(refused(desk.run("desk_drag", drag((50.0, 600.0))).await));
        assert!(refused(
            desk.run(
                "desk_scroll",
                json!({"direction": "down", "x": 50.0, "y": 950.0})
            )
            .await
        ));
        assert!(
            !calls
                .lock()
                .unwrap()
                .iter()
                .any(|t| t == "drag" || t == "scroll")
        );
        let mail =
            Desk::new(|| Ok(Apps), Desktop(calls.clone()), Os::Windows).allow(&["mail".into()]);
        assert!(refused(
            mail.run("desk_scroll", json!({"direction": "down"})).await
        ));
        desk.run("desk_drag", drag((200.0, 300.0))).await.unwrap();
        desk.run(
            "desk_scroll",
            json!({"direction": "down", "x": 100.0, "y": 800.0}),
        )
        .await
        .unwrap();
        let done = calls.lock().unwrap().clone();
        assert!(done.contains(&"drag".to_owned()) && done.contains(&"scroll".to_owned()));
    }

    struct Frames(Mutex<usize>, fn(usize) -> Vec<(usize, u8)>);

    impl Input for Frames {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            assert_eq!(
                (tool, &args),
                (
                    "get_desktop_state",
                    &json!({"max_image_dimension": 320, "session": CAPTURE_SESSION})
                )
            );
            let mut n = self.0.lock().unwrap();
            *n += 1;
            let (w, h) = (32, 16);
            let mut rgb = vec![0u8; w * h * 3];
            for (block, tone) in (self.1)(*n) {
                for y in 0..8 {
                    for x in 0..8 {
                        let at = ((y + block / 4 * 8) * w + x + block % 4 * 8) * 3;
                        rgb[at..at + 3].fill(tone);
                    }
                }
            }
            let mut png = Vec::new();
            let mut enc = png::Encoder::new(&mut png, w as u32, h as u32);
            enc.set_color(png::ColorType::Rgb);
            enc.write_header().unwrap().write_image_data(&rgb).unwrap();
            Ok(json!({"content": [{"type": "image", "data": B64.encode(&png)}]}))
        }
    }

    #[tokio::test]
    async fn still_answers_once_the_screen_holds_and_a_churning_region_does_not_hold_it_back() {
        let run = |frames: fn(usize) -> Vec<(usize, u8)>, args: Value| async move {
            let desk = Desk::new(|| Ok(notepad()), Frames(Mutex::new(0), frames), Os::Mac);
            let out = desk.run("desk_still", args).await.unwrap();
            serde_json::from_str::<Value>(&out.text).unwrap()
        };
        let pair = |v: &Value| (v["still"].clone(), v["changed"].clone());
        let calm = run(|_| vec![], json!({"hold_ms": 30})).await;
        assert_eq!(pair(&calm), (json!(true), json!(false)));
        let settles = run(
            |n| vec![(0, if n < 3 { 0 } else { 200 })],
            json!({"hold_ms": 30}),
        )
        .await;
        assert_eq!(pair(&settles), (json!(true), json!(true)));
        let video = run(
            |n| vec![(5, u8::try_from(n * 40 % 250).unwrap())],
            json!({"hold_ms": 30, "timeout_ms": 2000}),
        )
        .await;
        assert_eq!(pair(&video), (json!(true), json!(true)), "{video}");
        assert_eq!(video["volatile"], 1, "{video}");
        assert!(video["ms"].as_u64().unwrap() < 2000, "{video}");
        let short = run(
            |n| vec![(0, if n < 3 { 0 } else { 200 })],
            json!({"hold_ms": 1000, "timeout_ms": 150}),
        )
        .await;
        assert_eq!(pair(&short), (json!(false), json!(true)), "{short}");
        assert!(
            (150..1000).contains(&short["ms"].as_u64().unwrap()),
            "{short}"
        );
    }
}
