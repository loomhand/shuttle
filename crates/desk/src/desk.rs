//! The desk calls: snapshots and element actions through the OS backend; input, windows, launch and capture through cua-driver.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use desk_core::args::{
    ActArgs, AtArgs, ClickArgs, DragArgs, FindArgs, Focus, IdArgs, KeyArgs, LaunchArgs, ReadArgs,
    ScrollArgs, ShotArgs, SnapArgs, StillArgs, TypeArgs, WaitArgs, WebArgs, WindowArgs, parse,
};
use desk_core::shot::{OFF_SCREEN, crop_png, rgb_png};
use desk_core::text::{MAX_FOUND, NAME_CHARS, READ_CHARS, READ_TAIL, cut_read, matches, ms, quote};
use desk_core::window::{self, TITLE_TIERS, title_matches};
use desk_core::wire::{OK, Output};
use serde_json::{Map, Value, json};

use crate::allow::{Allow, app_key, app_of};
use crate::auto;
use crate::driver::{
    CAPTURE_SESSION, INPUT_TOOLS, Input, WINDOW_SESSION, background, desktop, image_of, text_of,
};
use crate::keys::{Os, hid_plus, key_names, keystroke};
use crate::lock;
use crate::model::{Access, Act, Capture, Extras, Frame, Node};
use crate::press::{Press, plan_press};
use crate::still::{Frame as StillFrame, Watch};
use crate::worker::{Cancel, Worker};

pub const TURN_WAIT: Duration = Duration::from_secs(15);
/// The verbs a vision agent drives the screen with, which the Linux X11, macOS and Windows desks answer.
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

pub(crate) const BACKGROUND_TARGET: &str =
    "background input needs its target window: pass window or pid";
pub(crate) const BACKGROUND_DESKTOP: &str = "background input: this window takes no pointer input without the foreground, and background input never brings a window to the front";
pub(crate) const BACKGROUND_NO_WINDOW: &str = "background input: no window under the point";
pub(crate) const BACKGROUND_ELEMENT: &str = "background input: the element's own window could not be found, and background input never clicks a window that covers it";
pub(crate) const BACKGROUND_TWINS: &str = "background input: the element's own window is ambiguous (same or no title, and no single frame match), and background input never guesses which window to click";
pub(crate) const BACKGROUND_UIA: &str = "background input: on Windows UI Automation focuses, selects or runs an element only by bringing its window to the front; click it in its window, or use input = \"foreground\"";
pub(crate) const CALL_LIMIT: Duration = Duration::from_secs(3);
pub(crate) const BUSY: &str = "desk busy: a previous action is still running";
#[cfg(target_os = "linux")]
pub(crate) const TYPER: Typer = crate::xtest::type_mapped;
#[cfg(not(target_os = "linux"))]
pub(crate) const TYPER: Typer = |_, _| Ok(false);

const APP_INPUT: [&str; 5] = [
    "desk_key",
    "desk_type",
    "desk_click",
    "desk_scroll",
    "desk_drag",
];

const TREE_LIMIT: Duration = Duration::from_secs(10);
const WAIT_MAX: Duration = Duration::from_secs(5);
const RAISE_WAIT: Duration = Duration::from_millis(500);
const WAIT_POLL: Duration = Duration::from_millis(10);
const WAIT_CHECK: Duration = Duration::from_millis(100);

const FRAME_BORDER: f64 = 16.0;
const TITLE_BAR: f64 = 48.0;

const OTHER_DISPLAY: &str =
    "; name the window with `window` to capture a window on another display";
const NO_WINDOW_UNDER: &str =
    "no window under the point, and the app allowlist admits only windows";
const NO_WINDOW_AT: &str = "no app window at the point; name one with window or pid";
const NO_WINDOWSERVER_CONNECTION: &str = "SLPSPostEventRecordTo returned 1002";
const WINDOW_NEVER_READ: &str = "window_never_read";

const COVERED: &str = "covers the point, so the click went in the background to the window under it and did not move the pointer: confirm its effect by a read-back";
const LAUNCH_FLAGS: &str = "the app allowlist admits no launch argument that starts with - (or / on Windows), since a flag can make an allowed app start another program";
const COVERED_NO_CLICK: &str = "another app's window covers the point and this window takes no click of its own, so nothing was clicked: bring the window to the front, then click again";
const COVER_UNCHECKED_NO_CLICK: &str = "the point could not be checked for another app's window and this window takes no click of its own, so nothing was clicked: bring the window to the front, then click again";
const COVER_UNKNOWN: &str = "the point could not be checked for another app's window on top, so the click went in the background to the window under it and did not move the pointer: confirm its effect by a read-back";
const ANOTHER_SPACE: &str = "accessibility does not reach it there, so a snapshot or an element action needs it on the current Space; background key, type, click, scroll and drag reach it by its title or pid";
const UNSEEN_SENT: &str = "the app may not repaint a window nobody sees, so the input may have taken effect even if a screenshot looks unchanged: do not send it again, and confirm its effect by a read-back you can trust or by asking the user";
const UNSEEN_TYPED: &str = "the app may not repaint a window nobody sees, so the text may be there even if a screenshot looks unchanged: do not type it again, and before you submit it with Return or Send, confirm that it is there by a read-back you can trust or by asking the user";

const MASK_TRIES: usize = 3;
const WINDOWS_MOVING: &str = "the windows kept changing while the screenshot was taken, so it could not be masked to the app allowlist; take it again";

const SCROLL_POINT: &str = "scroll needs both x and y, or neither";
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
const CHROMIUM_TYPE_DELAY_MS: u64 = 1;
const TERMINAL: &str = "terminal emulator";
const PROTOCOL_TAGS: [&str; 10] = [
    "text",
    "parameter",
    "invoke",
    "function_calls",
    "function_call",
    "tool_use",
    "tool_call",
    "antml:parameter",
    "antml:invoke",
    "antml:function_calls",
];

const FIELD_ROLES: [&str; 2] = ["textbox", "searchbox"];
const FOCUS_UNREAD: &str = "type refused, nothing was typed: the keyboard focus cannot be read in this window, so the text could go into another field than the one clicked; confirm the focused field in a screenshot, then call type again with focus: \"unverified\"";

const LOCKED_TOOLS: [&str; 2] = ["type_text", "press_key"];
const LOCKED_INPUT_UNSUPPORTED: &str = "nothing was sent: [desk] locked_input is on, but this cua-driver does not take locked_input; update shuttle, whose bundled driver does";
const LOCKED_SCREEN: &str = "shuttle cannot reach the lock screen: [desk] locked_input reaches only the window that had the focus before the lock, never the lock screen's password field; unlock over Screen Sharing or at the machine";
const FOCUS_UNKNOWN: &str = "focus unverified: accessibility shows no focused text field in the app, so which field took the text is unknown: confirm it in a screenshot before you press Return or Send";

tokio::task_local! {
    static FOREGROUND: bool;
    static SENT: Cell<usize>;
    static SCALE: Cell<Option<f64>>;
}

pub(crate) type Turn = Arc<tokio::sync::OwnedMutexGuard<()>>;
/// Types a line on the core keyboard; false leaves the line to the driver.
pub(crate) type Typer = fn(&str, &AtomicBool) -> Result<bool, String>;

/// The desk of one OS: `B` reads and acts on elements, `D` sends input.
pub struct Desk<B, D> {
    access: Worker<B>,
    driver: D,
    os: Os,
    acting: Arc<tokio::sync::Mutex<()>>,
    turn_wait: Duration,
    read: std::sync::Mutex<Option<Read>>,
    session: tokio::sync::Mutex<()>,
    allow: Allow,
    overlay: bool,
    background: bool,
    auto: bool,
    locked_input: bool,
    unseen: std::sync::Mutex<HashMap<(i64, i64), Unseen>>,
    clicked: std::sync::Mutex<Option<Clicked>>,
    webs: std::sync::Mutex<HashMap<u32, Held>>,
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
            session: tokio::sync::Mutex::new(()),
            allow: Allow::default(),
            overlay: true,
            background: false,
            auto: false,
            locked_input: false,
            unseen: std::sync::Mutex::new(HashMap::new()),
            clicked: std::sync::Mutex::new(None),
            webs: std::sync::Mutex::new(HashMap::new()),
            typer: TYPER,
            #[cfg(test)]
            skew: Arc::default(),
        }
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

    pub fn turn(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.acting)
    }

    /// Whether a background action refused before any input runs once more in the foreground.
    pub fn auto(mut self, on: bool) -> Self {
        self.auto = on;
        self
    }

    /// Whether background keys and typing on macOS may reach their window while the screen is locked, unverified.
    pub fn locked_input(mut self, on: bool) -> Self {
        self.locked_input = on;
        self
    }

    /// Runs desk call `tool`, a `desk_<verb>`; actions run one at a time, until their backend calls end, reads alongside them.
    pub async fn run(&self, tool: &str, mut args: Value) -> Result<Output, String> {
        if APP_INPUT.contains(&tool) {
            self.app_target(&mut args).await?;
        }
        let turn = if ACTIONS.contains(&tool) {
            self.check_mode(&args)?;
            let turn = tokio::time::timeout(self.turn_wait, self.acting.clone().lock_owned())
                .await
                .map_err(|_| BUSY.to_owned())?;
            if tool != "desk_type" {
                *lock(&self.clicked) = None;
            }
            Some(Arc::new(turn))
        } else {
            None
        };
        let foreground = self.auto && args["delivery_mode"] == "foreground";
        if !self.allow.is_open() {
            FOREGROUND
                .scope(foreground, self.admit(tool, &args))
                .await?;
        }
        let scale = Cell::new(None);
        if self.auto && !foreground && auto::VERBS.contains(&tool) {
            return SCALE
                .scope(scale, self.auto_run(tool, args, turn))
                .await
                .map_err(|e| at_lock(self.os, e));
        }
        let run = FOREGROUND.scope(foreground, self.dispatch(tool, args, turn));
        SCALE
            .scope(scale, run)
            .await
            .map_err(|e| at_lock(self.os, e))
    }

    pub fn release(&self, holder: &str) {
        let pids: Vec<u32> = lock(&self.webs).keys().copied().collect();
        for pid in pids.into_iter().filter(|&p| self.release_one(p, holder)) {
            self.access.post(move |b| drop(b.web(pid, false)));
        }
    }

    #[cfg(test)]
    fn with_typer(mut self, typer: Typer) -> Self {
        self.typer = typer;
        self
    }

    /// Turns an input call's `app` into the `pid` of that app's window.
    async fn app_target(&self, args: &mut Value) -> Result<(), String> {
        let Some(app) = args.as_object_mut().and_then(|a| a.remove("app")) else {
            return Ok(());
        };
        let app = match app {
            Value::String(s) => s,
            _ => String::new(),
        };
        if args["pid"].is_u64() {
            return Ok(());
        }
        let key = app_key(&app);
        let pid = self
            .listed()
            .await?
            .into_iter()
            .find(|f| f.app.as_deref().is_some_and(|a| app_key(a) == key))
            .and_then(|f| f.pid);
        match pid {
            Some(pid) => {
                args["pid"] = pid.into();
                Ok(())
            }
            None => Err(format!(
                "app {app:?} has no window to take the input, so nothing was sent"
            )),
        }
    }

    fn check_mode(&self, args: &Value) -> Result<(), String> {
        let input = if self.background {
            "background"
        } else {
            "foreground"
        };
        match args["delivery_mode"].as_str() {
            Some(mode) if mode != input && !(self.auto && mode == "foreground") => Err(format!(
                "delivery_mode {mode:?} refused, nothing was sent: this desk sends input in the {input}, as the user's [desk] input setting says, and a call cannot change that"
            )),
            _ => Ok(()),
        }
    }

    async fn auto_run(
        &self,
        tool: &str,
        args: Value,
        turn: Option<Turn>,
    ) -> Result<Output, String> {
        let mut refused = None;
        if !(tool == "desk_drag" && self.os == Os::Windows) {
            let tried = SENT.scope(Cell::new(0), async {
                let out = self.dispatch(tool, args.clone(), turn.clone()).await;
                (out, SENT.with(Cell::get))
            });
            match tried.await {
                (Err(e), 0) if auto::pre_send(tool, self.os, &args, &e) => refused = Some(e),
                (out, _) => return out,
            }
        }
        if !self.front_target(tool, &args).await?
            && let Some(e) = refused
        {
            return Err(e);
        }
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

    /// Brings the window of an auto action to the front before the foreground runs it; false when the action names none.
    async fn front_target(&self, tool: &str, args: &Value) -> Result<bool, String> {
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
            return Ok(false);
        }
        self.focus(WindowArgs {
            window: window.unwrap_or_default(),
            pid,
        })
        .await
        .map(|_| true)
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
            "desk_press" => self.press(parse::<IdArgs>(args)?.id, turn).await?,
            "desk_act" => self.act(parse(args)?, turn).await?,
            "desk_at" => self.at(parse(args)?).await?,
            "desk_key" => self.key(parse(args)?).await?,
            "desk_type" => self.type_text(parse(args)?, turn).await?,
            "desk_click" => self.click(parse(args)?, turn).await?,
            "desk_scroll" => self.scroll(parse(args)?).await?,
            "desk_drag" => self.drag(parse(args)?).await?,
            "desk_still" => self.still(parse(args)?).await?,
            "desk_focus" => self.focus(parse(args)?).await?,
            "desk_unanswered" => self.unanswered().await?,
            "desk_close" => self.close(parse(args)?, turn).await?,
            "desk_web" => self.web(parse(args)?).await?,
            "desk_launch" => {
                let a = parse::<LaunchArgs>(args)?;
                if a.args.is_empty()
                    && self.os != Os::Mac
                    && let Some(raised) = self.raise_running(&a.app).await?
                {
                    return Ok(Output::text(raised));
                }
                let mut req = json!({"name": launch_name(&a.app)});
                if !a.args.is_empty() {
                    if self.os != Os::Mac {
                        req["additional_arguments"] = a.args.into();
                    } else if a.args.iter().any(|s| s.starts_with('-')) {
                        req["creates_new_application_instance"] = true.into();
                        req["additional_arguments"] = a.args.into();
                    } else {
                        req["urls"] = a.args.into();
                    }
                }
                let retry = (self.os == Os::Mac).then(|| req.clone());
                let v = match (self.call("launch_app", req).await, retry) {
                    (Err(e), Some(mut req)) => {
                        let app = a.app;
                        let named = self
                            .access
                            .run(CALL_LIMIT, (), move |b| b.bundle_name(&app))
                            .await?
                            .ok_or(e)?;
                        req["name"] = named.into();
                        self.call("launch_app", req).await?
                    }
                    (v, _) => v?,
                };
                let mut out = json!({"ok": true});
                out["launched"] = text_of(&v).into();
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
            other => return Err(format!("unknown desk tool {other}")),
        };
        Ok(Output::text(text))
    }

    fn bg(&self) -> bool {
        self.background && !FOREGROUND.try_with(|f| *f).unwrap_or(false)
    }

    async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
        if self.os == Os::Linux && args["session"] == WINDOW_SESSION && tool != "get_window_state" {
            *lock(&self.read) = None;
        }
        let out = self.driver.call(tool, args).await?;
        if INPUT_TOOLS.contains(&tool) {
            let _ = SENT.try_with(|n| n.set(n.get() + 1));
        }
        Ok(out)
    }

    async fn unanswered(&self) -> Result<String, String> {
        let allow = self.allow.clone();
        let front = self
            .access
            .run(CALL_LIMIT, (), move |b| {
                Ok(b.unanswered().filter(|f| allow.admits(f.app.as_deref())))
            })
            .await?;
        serde_json::to_string(&front).map_err(|e| format!("encode unanswered: {e}"))
    }

    async fn raise_running(&self, app: &str) -> Result<Option<String>, String> {
        let frames = self.listed().await?;
        let Some(w) = frames.iter().find(|f| {
            !f.shell
                && !f.title.trim().is_empty()
                && f.app.as_deref().is_some_and(|name| runs_app(name, app))
        }) else {
            return Ok(None);
        };
        let front = WindowArgs {
            window: w.title.clone(),
            pid: w.pid,
        };
        if self.focus(front).await.is_err() {
            return Ok(None);
        }
        let launched = format!("{app} is already running; brought its window to the front");
        let out = json!({"ok": true, "launched": launched, "running": true, "title": w.title, "pid": w.pid});
        Ok(Some(out.to_string()))
    }

    async fn windows(&self) -> Result<String, String> {
        let frames = self.listed().await?;
        serde_json::to_string(&frames).map_err(|e| format!("encode windows: {e}"))
    }

    async fn listed(&self) -> Result<Vec<Frame>, String> {
        let (allow, background) = (self.allow.clone(), self.bg());
        self.access
            .run(CALL_LIMIT, (), move |b| {
                let mut frames = shown(b, background)?;
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
            "desk_launch" => {
                let flagged = args["args"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|s| s.as_str().is_some_and(|s| self.flag(s))));
                if flagged {
                    return Err(LAUNCH_FLAGS.to_owned());
                }
                return self.allow.check(args["app"].as_str());
            }
            "desk_read" | "desk_press" | "desk_act" => {
                Some(args["id"].as_str().ok_or("no element id")?.to_owned())
            }
            "desk_key" | "desk_type" => None,
            _ => return Ok(()),
        };
        let (allow, background) = (self.allow.clone(), self.bg());
        let named = background.then(|| {
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
                    allow
                        .pick(shown(b, background)?, window.as_deref(), pid)
                        .map(drop)
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
            urls: a.urls && !a.text,
            tail: false,
        };
        let (window, pid, id) = (a.window, a.pid, a.id);
        let (allow, background) = (self.allow.clone(), self.bg() && !self.auto);
        let (tree, found) = self
            .access
            .run(TREE_LIMIT, (), move |b| {
                b.evict();
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
                .into_iter()
                .enumerate()
                .map(|(i, n)| n.into_json(i))
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
            urls: a.urls,
            tail: false,
        };
        let (window, pid) = (a.window, a.pid);
        let (allow, background) = (self.allow.clone(), self.bg() && !self.auto);
        let mut tree = self
            .access
            .run(TREE_LIMIT, (), move |b| {
                b.evict();
                let listed = frames(b, pid, &allow)?;
                let frame = live(pick(b, &allow, background, listed, window.as_deref(), pid)?)?;
                b.tree(&frame, extras)
            })
            .await?;
        let found: Vec<Value> = matches(&tree.nodes, &a.name, a.role.as_deref())
            .into_iter()
            .take(MAX_FOUND)
            .map(|i| std::mem::take(&mut tree.nodes[i]).into_json(i))
            .collect();
        let mut out = json!({"window": tree.frame});
        out["found"] = Value::Array(found);
        Ok(out.to_string())
    }

    async fn read(&self, a: ReadArgs) -> Result<String, String> {
        if a.tail && !a.text {
            return Err(READ_TAIL.to_owned());
        }
        let extras = Extras {
            values: true,
            bounds: true,
            keys: true,
            menus: false,
            frame: false,
            urls: true,
            tail: a.tail,
        };
        let (text, tail) = (a.text, a.tail);
        let chars = if text { READ_CHARS + 1 } else { NAME_CHARS };
        let (mut node, (url, dom_id)) = self
            .access
            .run(CALL_LIMIT, (), move |b| {
                let node = b.node(&a.id, extras, chars)?;
                let web = if a.web {
                    b.web_of(&a.id)?
                } else {
                    (None, None)
                };
                Ok((node, web))
            })
            .await?;
        let truncated = text && (cut_read(&mut node.value, tail) | node.truncated);
        let mut out = node.into_json(0);
        if truncated {
            out["truncated"] = true.into();
        }
        if let Some(url) = url {
            out["url"] = url.into();
        }
        if let Some(dom_id) = dom_id {
            out["dom_id"] = dom_id.into();
        }
        Ok(out.to_string())
    }

    async fn web(&self, a: WebArgs) -> Result<String, String> {
        let held = lock(&self.webs).get(&a.pid).map(|h| h.was);
        let was = match (a.on, held) {
            (true, Some(was)) => was,
            (false, Some(was)) if !self.release_one(a.pid, &a.holder) => was,
            (on, held) => {
                let (allow, pid) = (self.allow.clone(), a.pid);
                let was = self
                    .access
                    .run(CALL_LIMIT, (), move |b| {
                        check_process(b, &allow, Some(pid))?;
                        b.web(pid, on)
                    })
                    .await?;
                held.unwrap_or(was)
            }
        };
        if a.on {
            let mut webs = lock(&self.webs);
            let h = webs
                .entry(a.pid)
                .and_modify(|h| h.was &= was)
                .or_insert_with(|| Held {
                    was,
                    holders: HashSet::new(),
                });
            h.holders.insert(a.holder);
        }
        Ok(json!({"ok": true, "was": was}).to_string())
    }

    fn release_one(&self, pid: u32, holder: &str) -> bool {
        let mut webs = lock(&self.webs);
        let Some(h) = webs.get_mut(&pid) else {
            return false;
        };
        h.holders.remove(holder);
        let last = h.holders.is_empty() && !h.was;
        if h.holders.is_empty() {
            webs.remove(&pid);
        }
        last
    }

    async fn press(&self, id: String, turn: Option<Turn>) -> Result<String, String> {
        let (os, background) = (self.os, self.bg());
        let pressed = id.clone();
        match self
            .access
            .run(TREE_LIMIT, turn.clone(), move |b| {
                plan_press(b, &pressed, os, background)
            })
            .await?
        {
            Press::Selected => Ok(json!({"ok": true, "how": "select"}).to_string()),
            Press::Acted(ok) => Ok(json!({"ok": ok, "how": "action"}).to_string()),
            Press::Click(x, y) => {
                let note = self
                    .click_element(id, json!({"x": x, "y": y}), turn)
                    .await?;
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
                    .run(CALL_LIMIT, turn.clone(), move |b| {
                        b.node(&id, Extras::BOUNDS, 0)
                    })
                    .await?;
                let [x, y, w, h] = n.bounds.ok_or("the element has no screen position")?;
                let note = self
                    .click_element(a.id, json!({"x": x + w / 2, "y": y + h / 2}), turn)
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

    async fn at(&self, mut a: AtArgs) -> Result<String, String> {
        let under = a.window.is_none() && a.pid.is_none();
        let mut number = None;
        if under {
            let scale = self.scale().await?;
            let point = (f64::from(a.x) / scale, f64::from(a.y) / scale);
            let top = self.top_at(point).await?.ok_or(NO_WINDOW_AT)?;
            a.window = Some(top["title"].as_str().unwrap_or_default().to_owned());
            a.pid = top["pid"].as_u64().and_then(|p| u32::try_from(p).ok());
            number = top["window_id"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok());
        }
        let (allow, background) = (self.allow.clone(), self.bg() && !self.auto);
        self.access
            .run(CALL_LIMIT, (), move |b| {
                b.evict();
                if under && !allow.is_open() {
                    check_process(b, &allow, a.pid)?;
                }
                let frames = frames(b, a.pid, &allow)?;
                if let Some(f) = number.and_then(|n| frames.iter().find(|f| f.number == Some(n))) {
                    a.window = Some(f.title.clone());
                }
                let kept = (!allow.is_open()).then(|| frames.clone());
                let window = a.window.as_deref();
                let held = under
                    .then(|| holding(b, &frames, window, a.pid, (a.x, a.y)))
                    .flatten();
                let frame = live(match held {
                    Some(held) => held?,
                    None => pick(b, &allow, background, frames, window, a.pid)?,
                })?;
                let Some(hit) = b.at(&frame.id, a.x, a.y)? else {
                    return Ok(json!({"found": null, "window": frame}).to_string());
                };
                let pid = b.pid_of(&hit)?;
                if let Some(frames) = &kept {
                    allow.check(app_of(frames, pid))?;
                }
                let covered = b.covering(&frame, a.x, a.y).ok().flatten();
                let node = b.node(&hit, Extras::default(), 0)?;
                let path: Vec<Value> = b
                    .ancestors(&hit)?
                    .into_iter()
                    .filter_map(|id| {
                        let n = b.node(&id, Extras::default(), 0).ok()?;
                        Some(Value::from_iter([
                            ("id", Value::from(id)),
                            ("role", n.role.into()),
                            ("name", n.name.into()),
                        ]))
                    })
                    .collect();
                let mut out = json!({});
                out["found"] = node.into_json(0);
                out["path"] = Value::Array(path);
                out["pid"] = pid.into();
                if let Some((title, pid)) = covered {
                    out["covered"] =
                        Value::from_iter([("title", Value::from(title)), ("pid", pid.into())]);
                }
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
                Ok(
                    Value::from_iter([("ok", Value::from(true)), ("title", frame.title.into())])
                        .to_string(),
                )
            })
            .await
    }

    async fn key(&self, a: KeyArgs) -> Result<String, String> {
        let times = a.times()?;
        let (tool, mut req) = key_request(key_names(&a.keys, self.os)?);
        let to = self.target(a.window.as_deref(), a.pid).await?;
        if self.os == Os::Mac && to.is_none() {
            hid_plus(&mut req);
        }
        let raised = self.raised_window(a.window.as_deref(), a.pid).await?;
        self.raise(raised).await?;
        let mut note = None;
        for left in (0..times).rev() {
            if left + 1 < times
                && let Some(w) = raised
                && !self.fronted(w).await
            {
                return Err(format!(
                    "{} of {times} presses were sent, then the window left the front, so the rest were not sent",
                    times - left - 1
                ));
            }
            let mut req = req.clone();
            if left > 0 && self.os == Os::Mac && to.is_some() {
                req["post_action_wait"] = "none".into();
            }
            self.send(tool, req, to, &mut note).await?;
        }
        Ok(ok_with(note))
    }

    async fn type_text(&self, a: TypeArgs, turn: Option<Turn>) -> Result<String, String> {
        let text = without_protocol_tail(&a.text);
        let to = self.target(a.window.as_deref(), a.pid).await?;
        let focus = match to {
            Some(to) if self.os == Os::Mac => Some(
                self.typing_focus(to, a.focus == Some(Focus::Unverified))
                    .await?,
            ),
            _ => None,
        };
        let raised = self.raised_window(a.window.as_deref(), a.pid).await?;
        let chromium = match to.and_then(|(pid, _)| u32::try_from(pid).ok()) {
            Some(pid) if self.os == Os::Mac => self
                .access
                .run(CALL_LIMIT, (), move |b| Ok(b.chromium(pid)))
                .await
                .unwrap_or(false),
            _ => false,
        };
        let mut note = None;
        let mut terminal = false;
        let lines = text.split('\n').count();
        for (i, line) in text.split('\n').enumerate() {
            let sent: Result<(), String> = async {
                self.raise(raised).await?;
                if i > 0 {
                    self.send("press_key", json!({"key": "return"}), to, &mut note)
                        .await?;
                }
                let line = line.trim_end_matches('\r');
                if self.os == Os::Linux {
                    if line.is_empty()
                        || (to.is_none() && xtest(self.typer, line, turn.clone()).await?)
                    {
                        return Ok(());
                    }
                    for run in text_runs(line) {
                        terminal |= self
                            .send(
                                "type_text",
                                json!({"text": run, "literal": true}),
                                to,
                                &mut note,
                            )
                            .await?;
                    }
                    return Ok(());
                }
                if self.os == Os::Mac || to.is_none() {
                    if !line.is_empty() {
                        let mut req = json!({"text": line, "literal": true});
                        if chromium {
                            req["delay_ms"] = CHROMIUM_TYPE_DELAY_MS.into();
                        } else if self.os == Os::Mac
                            && !self.bg()
                            && line.chars().all(|c| c.len_utf16() == 1)
                        {
                            req["delay_ms"] = MAC_TYPE_DELAY_MS.into();
                        }
                        let mut said = None;
                        terminal |= self.send("type_text", req, to, &mut said).await?;
                        if let Some(n) = said.as_deref().filter(|n| n.contains(TERMINAL))
                            && i + 1 < lines
                        {
                            return Err(format!(
                                "the terminal did not confirm line {} of {lines}, so Return was not sent: {n}",
                                i + 1
                            ));
                        }
                        note = note.take().or(said);
                    }
                    return Ok(());
                }
                let mut run = None;
                for (at, c) in line.char_indices() {
                    let key = keystroke(c);
                    if key.is_none() && !matches!(c, '\t' | '\r') {
                        run.get_or_insert(at);
                        continue;
                    }
                    if let Some(from) = run.take() {
                        let req = json!({"text": &line[from..at], "literal": true});
                        self.send("type_text", req, to, &mut note).await?;
                    }
                    let (tool, req) = match key {
                        Some(key) => key_request(vec![key]),
                        None => (
                            "type_text",
                            json!({"text": &line[at..at + c.len_utf8()], "literal": true}),
                        ),
                    };
                    self.send(tool, req, to, &mut note).await?;
                }
                if let Some(from) = run {
                    let req = json!({"text": &line[from..], "literal": true});
                    self.send("type_text", req, to, &mut note).await?;
                }
                Ok(())
            }
            .await;
            sent.map_err(|e| match i {
                0 => e,
                _ => format!("{e}; {i} of {lines} lines were typed before it"),
            })?;
        }
        if self.os == Os::Linux && to.is_none() {
            self.typed(text).await;
        }
        let mut out = json!({"ok": true, "chars": text.chars().count()});
        if let Some((focused, field)) = focus {
            out["focused"] = focused;
            if !field {
                note = Some(note.map_or_else(
                    || FOCUS_UNKNOWN.to_owned(),
                    |n| format!("{n}; {FOCUS_UNKNOWN}"),
                ));
            }
        }
        if let Some(note) = note {
            out["unverified"] = note.into();
        }
        if terminal {
            out["terminal"] = true.into();
        }
        Ok(out.to_string())
    }

    /// The focused element of window `to`'s app as `{role, name}` and whether it is a text field.
    async fn typing_focus(
        &self,
        to: (i64, i64),
        unverified: bool,
    ) -> Result<(Value, bool), String> {
        let clicked = lock(&self.clicked).filter(|c| c.to == to).map(|c| c.at);
        let n = self.keyboard_focus(to.0).await;
        let field = n.as_ref().is_some_and(|n| FIELD_ROLES.contains(&n.role));
        if !field && clicked.is_some() && !unverified {
            return Err(FOCUS_UNREAD.to_owned());
        }
        let Some(n) = n else {
            return Ok((Value::Null, false));
        };
        if field && clicked.and_then(|at| holds(&n, at)) == Some(false) {
            return Err(format!(
                "type refused, nothing was typed: the keyboard focus is in {} {:?}, not in the field of the last click into this window, so the text would go there; click the field to type into, then type again",
                n.role, n.name
            ));
        }
        let mut focused = json!({"role": n.role});
        focused["name"] = n.name.into();
        Ok((focused, field))
    }

    async fn keyboard_focus(&self, pid: i64) -> Option<Node> {
        let pid = u32::try_from(pid).ok()?;
        self.access
            .run(CALL_LIMIT, (), move |b| b.keyboard_focus(pid))
            .await
            .ok()
            .flatten()
    }

    async fn send(
        &self,
        tool: &str,
        req: Value,
        to: Option<(i64, i64)>,
        note: &mut Option<String>,
    ) -> Result<bool, String> {
        let mut req = match to {
            Some(to) => background(req, to),
            None => desktop(req),
        };
        let locked = self.locked_input
            && self.os == Os::Mac
            && to.is_some()
            && self.bg()
            && LOCKED_TOOLS.contains(&tool);
        if locked {
            req["locked_input"] = true.into();
        }
        let _session = match to {
            Some(_) if self.os == Os::Linux => Some(self.session.lock().await),
            _ => None,
        };
        let out = self.call(tool, req).await.map_err(|e| {
            if locked && e.contains("locked_input") {
                format!("{tool}: {LOCKED_INPUT_UNSUPPORTED}")
            } else {
                e
            }
        })?;
        if note.is_none() {
            *note = self.unverified(&out, to, tool == "type_text");
        }
        Ok(out["content"].as_array().is_some_and(|c| {
            c.iter()
                .any(|t| t["text"].as_str().is_some_and(|t| t.contains(TERMINAL)))
        }))
    }

    /// The driver's note on background input whose effect it could not confirm, with its escalation; for a window nobody sees, ours instead.
    fn unverified(&self, out: &Value, to: Option<(i64, i64)>, typed: bool) -> Option<String> {
        let c = &out["structuredContent"];
        let effect = c["effect"]
            .as_str()
            .filter(|e| self.bg() && *e != "confirmed")?;
        if let Some(u) = to.and_then(|t| lock(&self.unseen).get(&t).copied()) {
            let then = if typed { UNSEEN_TYPED } else { UNSEEN_SENT };
            return Some(format!("{effect}; {}: {then}", u.why()));
        }
        let note = format!("{effect}: {}", text_of(out));
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

    /// Waits at most [`TYPE_SETTLE`] until the focused element holds the end of the typed text.
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

    async fn click(&self, a: ClickArgs, turn: Option<Turn>) -> Result<String, String> {
        let named = match a.window.as_deref() {
            Some(window) => Some(self.shot_window(window, a.pid).await?.1),
            None if self.bg() && a.pid.is_some() => {
                Some(self.pick_window(None, a.pid, true).await?.1)
            }
            None => None,
        };
        let (x, y) = match &named {
            Some(w) if a.window.is_some() => self.desk_point(w, (a.x, a.y)).await?,
            _ => (a.x, a.y),
        };
        if let Some(w) = named
            .as_ref()
            .filter(|_| a.window.is_some() && !self.bg() && self.os != Os::Mac)
        {
            self.front(w, a.pid, (x, y)).await?;
        }
        let mut req = json!({"x": x, "y": y});
        if let Some(button) = a.button {
            req["button"] = button.into();
        }
        if let Some(count) = a.count {
            req["count"] = count.into();
        }
        if let Some(modifiers) = a.modifiers {
            req["modifier"] = modifiers.into();
        }
        let note = match named {
            Some(w) if self.bg() || self.os == Os::Mac => self.click_in(req, &w, (x, y)).await?,
            _ => self.click_at(req, turn.clone()).await?,
        };
        let mut out = json!({"ok": true});
        if let Some(note) = note {
            out["unverified"] = note.into();
        }
        let clicked = *lock(&self.clicked);
        if let Some(c) = clicked.filter(|_| self.os == Os::Mac)
            && let Some(n) = self.keyboard_focus(c.to.0).await
        {
            let at = FIELD_ROLES
                .contains(&n.role)
                .then(|| holds(&n, c.at))
                .flatten();
            let mut focused = json!({"role": n.role});
            focused["name"] = n.name.into();
            if let Some(at) = at {
                focused["at_click"] = at.into();
            }
            out["focused"] = focused;
        }
        Ok(out.to_string())
    }

    /// A click at desk point `(x, y)` in driver window `w`, which the allowlist already admitted when it was picked.
    async fn click_in(
        &self,
        req: Value,
        w: &Value,
        (x, y): (f64, f64),
    ) -> Result<Option<String>, String> {
        self.classify(w);
        let scale = self.scale().await?;
        let under = Under::of(w, (x / scale, y / scale)).ok_or(BACKGROUND_TARGET)?;
        self.click_window(req, &under).await
    }

    /// A click at the centre `(x, y)` of element `id`; in the background in the element's own window, whatever covers it.
    async fn click_element(
        &self,
        id: String,
        req: Value,
        turn: Option<Turn>,
    ) -> Result<Option<String>, String> {
        if !self.bg() {
            return self.click_at(req, turn).await;
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
        let mut listed = self.call("list_windows", json!({})).await?;
        let (_, picked, _) = self
            .pick_listed(listed.clone(), Some(&title), Some(pid), false)
            .await?;
        let scale = self.scale().await?;
        let Value::Array(windows) = listed["structuredContent"]["windows"].take() else {
            return Err(BACKGROUND_ELEMENT.to_owned());
        };
        let titled = |w: &Value, tier: usize| {
            w["pid"].as_u64() == Some(u64::from(pid))
                && title_matches(tier, w["title"].as_str().unwrap_or_default(), &title)
        };
        let tier = (0..TITLE_TIERS).find(|&t| windows.iter().any(|w| titled(w, t)));
        let same: Vec<Value> = match tier {
            Some(t) => windows.into_iter().filter(|w| titled(w, t)).collect(),
            None if picked["pid"].as_u64() == Some(u64::from(pid)) => vec![picked],
            None => Vec::new(),
        };
        let tiers: &[[f64; 4]] = match self.os {
            Os::Windows => &[[FRAME_BORDER; 4]],
            Os::Linux => &[
                [2.0; 4],
                [FRAME_BORDER, TITLE_BAR, FRAME_BORDER, FRAME_BORDER],
            ],
            Os::Mac => &[[2.0; 4]],
        };
        let fits = |w: &Value, border: [f64; 4]| {
            let [x, y, width, height] = bounds(w);
            frame.is_some_and(|[l, t, fw, fh]| {
                let [l, t, r, b] = [l, t, l + fw, t + fh].map(|v| f64::from(v) / scale);
                [x - l, y - t, r - x - width, b - y - height]
                    .iter()
                    .zip(border)
                    .all(|(d, most)| (-2.0..=most).contains(d))
            })
        };
        let fit: Vec<&Value> = tiers
            .iter()
            .map(|&t| same.iter().filter(|w| fits(w, t)).collect::<Vec<_>>())
            .find(|m| !m.is_empty())
            .unwrap_or_default();
        match fit.as_slice() {
            [w] => self.click_in(req, w, point).await,
            _ => Err(BACKGROUND_TWINS.to_owned()),
        }
    }

    async fn click_at(&self, req: Value, turn: Option<Turn>) -> Result<Option<String>, String> {
        let point = req["x"].as_f64().zip(req["y"].as_f64());
        if self.os == Os::Mac
            && let Some((x, y)) = point
        {
            let (px, py) = (x as i32, y as i32);
            let allow = self.allow.clone();
            let pressed = self
                .access
                .run(CALL_LIMIT, turn.clone(), move |b| {
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

    async fn scroll(&self, a: ScrollArgs) -> Result<String, String> {
        if a.x.is_some() != a.y.is_some() {
            return Err(SCROLL_POINT.to_owned());
        }
        let mut req =
            json!({"direction": a.direction, "amount": a.amount.unwrap_or(SCROLL_AMOUNT)});
        if self.bg() {
            let (to, w) = match (a.window.as_deref(), a.x) {
                (Some(window), Some(_)) => self.shot_window(window, a.pid).await?,
                _ => self.background_window(a.window.as_deref(), a.pid).await?,
            };
            let _session = self.session.lock().await;
            if let (Some(x), Some(y)) = (a.x, a.y) {
                let point = match a.window {
                    Some(_) => self.desk_point(&w, (x, y)).await?,
                    None => (x, y),
                };
                let (lx, ly) = self.local(&w, point).await?;
                req["x"] = lx.into();
                req["y"] = ly.into();
            }
            let out = self.call("scroll", background(req, to)).await?;
            return Ok(ok_with(self.unverified(&out, Some(to), false)));
        }
        let (x, y) = match (a.x, a.y) {
            (Some(x), Some(y)) => match a.window.as_deref() {
                Some(window) => {
                    let (_, w) = self.shot_window(window, a.pid).await?;
                    let point = self.desk_point(&w, (x, y)).await?;
                    self.front(&w, a.pid, point).await?;
                    point
                }
                None => (x, y),
            },
            _ => {
                let mut target = self.front_window().await?;
                if self.os == Os::Mac {
                    req["pid"] = target["pid"].take();
                    req["window_id"] = target["window_id"].take();
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

    async fn drag(&self, a: DragArgs) -> Result<String, String> {
        let (mut from, mut to) = ((a.x, a.y), (a.to_x, a.to_y));
        if self.bg() {
            let (target, w) = match a.window.as_deref() {
                Some(window) => self.shot_window(window, a.pid).await?,
                None => self.background_window(None, a.pid).await?,
            };
            let _session = self.session.lock().await;
            if a.window.is_some() {
                from = self.desk_point(&w, from).await?;
                to = self.desk_point(&w, to).await?;
            }
            let (fx, fy) = self.local(&w, from).await?;
            let (tx, ty) = self.local(&w, to).await?;
            let req = json!({"from_x": fx, "from_y": fy, "to_x": tx, "to_y": ty});
            let out = self.call("drag", background(req, target)).await?;
            return Ok(ok_with(self.unverified(&out, Some(target), false)));
        }
        if let Some(window) = a.window.as_deref() {
            let (_, w) = self.shot_window(window, a.pid).await?;
            from = self.desk_point(&w, from).await?;
            to = self.desk_point(&w, to).await?;
            self.front(&w, a.pid, from).await?;
        }
        self.admit_point(from.0, from.1).await?;
        self.admit_point(to.0, to.1).await?;
        let req = json!({"from_x": from.0, "from_y": from.1, "to_x": to.0, "to_y": to.1});
        self.call("drag", desktop(req)).await?;
        Ok(OK.to_owned())
    }

    /// Waits until no block of the screen changes in a way that counts (see [`Watch`]) for `hold_ms`, at most `timeout_ms`.
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
        let _session = self.session.lock().await;
        let mut cover = None;
        if !self.bg() {
            self.raise(Some((w.pid, w.id))).await?;
            cover = self.cover(&req, w.pid).await?;
        }
        let cached = lock(&self.read)
            .filter(|r| (r.pid, r.id, r.bounds) == (w.pid, w.id, w.bounds))
            .map(|r| r.k);
        let k = match cached {
            Some(k) => Some(k),
            None => self.pixels(w).await?,
        };
        let Some(k) = k else {
            return self.uncovered_click(cover.as_deref(), req).await;
        };
        let mut win = req.clone();
        win["session"] = WINDOW_SESSION.into();
        win["pid"] = w.pid.into();
        win["window_id"] = w.id.into();
        win["delivery_mode"] = if self.bg() || cover.is_some() {
            "background"
        } else {
            "foreground"
        }
        .into();
        let at = |win: &mut Value, (kx, ky): (f64, f64)| {
            win["x"] = ((w.point.0 - w.bounds[0]) * kx).into();
            win["y"] = ((w.point.1 - w.bounds[1]) * ky).into();
        };
        at(&mut win, k);
        let out = match self.call("click", win.clone()).await {
            Err(e) if e.contains(NO_WINDOWSERVER_CONNECTION) => {
                return self.uncovered_click(cover.as_deref(), req).await;
            }
            Err(e) if cached.is_some() && e.contains(WINDOW_NEVER_READ) => {
                match self.pixels(w).await? {
                    Some(k) => {
                        at(&mut win, k);
                        self.call("click", win).await?
                    }
                    None => return self.uncovered_click(cover.as_deref(), req).await,
                }
            }
            done => done?,
        };
        if cover.is_some() {
            return Ok(cover);
        }
        if self.bg()
            && let Some(at) = req["x"].as_f64().zip(req["y"].as_f64())
        {
            *lock(&self.clicked) = Some(Clicked {
                to: (w.pid, w.id),
                at,
            });
        }
        Ok(self.unverified(&out, Some((w.pid, w.id)), false))
    }

    fn flag(&self, arg: &str) -> bool {
        arg.starts_with('-') || (self.os == Os::Windows && arg.starts_with('/'))
    }

    async fn uncovered_click(
        &self,
        cover: Option<&str>,
        req: Value,
    ) -> Result<Option<String>, String> {
        match cover {
            Some(COVER_UNKNOWN) => Err(COVER_UNCHECKED_NO_CLICK.to_owned()),
            Some(_) => Err(COVERED_NO_CLICK.to_owned()),
            None => self.desktop_click(req).await.map(|()| None),
        }
    }

    /// Why the point of `req` takes no foreground click into process `pid`: another app's window is on top there.
    async fn cover(&self, req: &Value, pid: i64) -> Result<Option<String>, String> {
        let Some((x, y)) = req["x"].as_f64().zip(req["y"].as_f64()) else {
            return Ok(None);
        };
        let (x, y) = (x as i32, y as i32);
        let top = match self
            .access
            .run(CALL_LIMIT, (), move |b| b.owner_at(x, y))
            .await
        {
            Ok(top) => top,
            Err(_) => match self
                .access
                .run(CALL_LIMIT, (), move |b| b.listed_owner_at(x, y))
                .await
            {
                Ok(Some(top)) => Some(top),
                _ => return Ok(Some(COVER_UNKNOWN.to_owned())),
            },
        };
        let Some(top) = top.filter(|&t| i64::from(t) != pid) else {
            return Ok(None);
        };
        let v = self.call("list_windows", json!({"pid": top})).await?;
        let app = v["structuredContent"]["windows"][0]["app_name"]
            .as_str()
            .map_or_else(|| format!("process {top}"), str::to_owned);
        Ok(Some(format!("{app} {COVERED}")))
    }

    async fn desktop_click(&self, req: Value) -> Result<(), String> {
        if self.bg() {
            return Err(BACKGROUND_DESKTOP.to_owned());
        }
        self.call("click", desktop(req)).await?;
        Ok(())
    }

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

    async fn raised_window(
        &self,
        window: Option<&str>,
        pid: Option<u32>,
    ) -> Result<Option<(i64, i64)>, String> {
        let named = pid.is_some() || (self.os != Os::Linux && window.is_some());
        if self.bg() || !named {
            return Ok(None);
        }
        let (_, w, _) = self.pick_window(window, pid, true).await?;
        let (pid, id, _) = window_key(&w)?;
        Ok(Some((pid, id)))
    }

    /// Raises window `w`, the target of pointer input named by `window`, when the point lies in it and another window than one of [`Self::own_popup`] is on top there, under the rule [`Self::raised_window`] keeps.
    async fn front(&self, w: &Value, pid: Option<u32>, (x, y): (f64, f64)) -> Result<(), String> {
        if pid.is_none() && self.os == Os::Linux {
            return Ok(());
        }
        let scale = self.scale().await?;
        let point = (x / scale, y / scale);
        let [bx, by, bw, bh] = bounds(w);
        if !(bx..bx + bw).contains(&point.0) || !(by..by + bh).contains(&point.1) {
            return Ok(());
        }
        let (tp, tid, _) = window_key(w)?;
        let to = (tp, tid);
        let key = |w: &Value| (w["pid"].as_i64(), w["window_id"].as_i64());
        let Some(cover) = self.top_at(point).await? else {
            return Ok(());
        };
        if key(&cover) == (Some(to.0), Some(to.1)) || self.own_popup(&cover, to.0).await? {
            return Ok(());
        }
        self.raise(Some(to)).await?;
        let end = Instant::now() + RAISE_WAIT;
        while Instant::now() < end
            && self
                .top_at(point)
                .await?
                .is_some_and(|w| key(&w) == key(&cover))
        {
            tokio::time::sleep(WAIT_POLL).await;
        }
        Ok(())
    }

    /// Whether driver window `w` is a window of process `pid` other than an unowned top-level window the desk lists, such as its popup menu or a dialog it owns.
    async fn own_popup(&self, w: &Value, pid: i64) -> Result<bool, String> {
        if w["pid"].as_i64() != Some(pid) {
            return Ok(false);
        }
        let title = w["title"].as_str().unwrap_or_default().trim();
        if title.is_empty() {
            return Ok(true);
        }
        Ok(!lists_top_level(&self.listed().await?, w))
    }

    async fn raise(&self, window: Option<(i64, i64)>) -> Result<(), String> {
        let Some((pid, window_id)) = window else {
            return Ok(());
        };
        if self.os == Os::Mac && self.fronted((pid, window_id)).await {
            return Ok(());
        }
        if let Err(e) = self
            .call(
                "bring_to_front",
                json!({"pid": pid, "window_id": window_id}),
            )
            .await
        {
            let front = match e
                .rsplit_once(crate::driver::ACTUAL_FRONT)
                .and_then(|(_, id)| id.trim().parse::<u64>().ok())
            {
                Some(id) => self
                    .on_screen()
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .find(|w| w["window_id"].as_u64() == Some(id))
                    .and_then(|w| w["title"].as_str().map(str::to_owned)),
                None => None,
            };
            return Err(match front {
                Some(title) => format!("{e}; the front window is {title:?}"),
                None => e,
            });
        }
        Ok(())
    }

    /// Whether window `(pid, window_id)` is in front: on macOS by its window number, elsewhere by an active window of its process.
    async fn fronted(&self, (pid, window_id): (i64, i64)) -> bool {
        let Ok(app) = u32::try_from(pid) else {
            return false;
        };
        let mac = self.os == Os::Mac;
        self.access
            .run(CALL_LIMIT, (), move |b| b.frames_of(app))
            .await
            .is_ok_and(|frames| {
                frames.iter().any(|f| {
                    f.active && (!mac || (!f.stale && f.number.map(i64::from) == Some(window_id)))
                })
            })
    }

    async fn background_window(
        &self,
        window: Option<&str>,
        pid: Option<u32>,
    ) -> Result<((i64, i64), Value), String> {
        if window.is_none() && pid.is_none() {
            return Err(BACKGROUND_TARGET.to_owned());
        }
        let (_, w, _) = self.pick_window(window, pid, true).await?;
        let to = w["pid"]
            .as_i64()
            .zip(w["window_id"].as_i64())
            .ok_or(BACKGROUND_TARGET)?;
        Ok((to, w))
    }

    /// The window input with `window` goes to; a title that matches two listed windows equally is refused.
    async fn shot_window(
        &self,
        window: &str,
        pid: Option<u32>,
    ) -> Result<((i64, i64), Value), String> {
        let (_, w, rivals) = self.pick_window(Some(window), pid, true).await?;
        if rivals > 1 {
            return Err(format!(
                "{rivals} windows match the title {window:?}, so the pixels of a shot by that title may be another window's: pass a longer title that only one window matches, give the window a title of its own, or pass desk coordinates without window"
            ));
        }
        let (pid, id, _) = window_key(&w)?;
        Ok(((pid, id), w))
    }

    /// The desk point `(x, y)` in the screenshot pixels of driver window `w`, which the driver reads first as for a click.
    async fn local(&self, w: &Value, (x, y): (f64, f64)) -> Result<(f64, f64), String> {
        let scale = self.scale().await?;
        let under = Under::of(w, (x / scale, y / scale)).ok_or(BACKGROUND_TARGET)?;
        let cached = lock(&self.read)
            .filter(|r| (r.pid, r.id, r.bounds) == (under.pid, under.id, under.bounds))
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
        if let Ok(Some(k)) = SCALE.try_with(Cell::get) {
            return Ok(k);
        }
        let v = self.call("get_screen_size", json!({})).await?;
        let k = v["structuredContent"]["scale_factor"]
            .as_f64()
            .unwrap_or(1.0);
        let _ = SCALE.try_with(|s| s.set(Some(k)));
        Ok(k)
    }

    async fn window_at(&self, x: f64, y: f64) -> Result<Option<Under>, String> {
        let scale = self.scale().await?;
        let point = (x / scale, y / scale);
        Ok(self.top_at(point).await?.and_then(|w| Under::of(&w, point)))
    }

    /// The topmost app window the driver lists at `point`, in points.
    async fn top_at(&self, point: (f64, f64)) -> Result<Option<Value>, String> {
        let top = self
            .on_screen()
            .await?
            .into_iter()
            .filter(|w| layer(w) == 0)
            .filter(|w| {
                let [bx, by, bw, bh] = bounds(w);
                point.0 >= bx && point.0 < bx + bw && point.1 >= by && point.1 < by + bh
            })
            .max_by_key(|w| w["z_index"].as_i64().unwrap_or(i64::MIN));
        if let Some(w) = &top {
            self.classify(w);
        }
        Ok(top)
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
        let read = json!({
            "session": WINDOW_SESSION,
            "pid": w.pid,
            "window_id": w.id,
            "include_accessibility_tree": false
        });
        let v = self.call("get_window_state", read).await?;
        self.remember((w.pid, w.id, w.bounds), &v, false)
    }

    /// Keeps the image scale of read `v` of window `(pid, id, bounds)`, which the driver maps the next window click by.
    fn remember(
        &self,
        (pid, id, bounds): (i64, i64, [f64; 4]),
        v: &Value,
        full: bool,
    ) -> Result<Option<(f64, f64)>, String> {
        *lock(&self.read) = None;
        if refuses_pointer(v) {
            return Ok(None);
        }
        let k = image_scale(v, id)?;
        *lock(&self.read) = Some(Read {
            pid,
            id,
            bounds,
            k,
            full,
        });
        Ok(Some(k))
    }

    /// The desk point of pixel `(x, y)` in the `shot` image of driver window `w`.
    async fn desk_point(&self, w: &Value, (x, y): (f64, f64)) -> Result<(f64, f64), String> {
        let key = window_key(w)?;
        let cached = lock(&self.read)
            .filter(|r| r.full && (r.pid, r.id, r.bounds) == key)
            .map(|r| r.k);
        let (kx, ky) = match cached {
            Some(k) => k,
            None => {
                let v = self
                    .call("get_window_state", full_read(key.0, key.1))
                    .await?;
                match self.remember(key, &v, true)? {
                    Some(k) => k,
                    None => image_scale(&v, key.1)?,
                }
            }
        };
        let scale = self.scale().await?;
        Ok(((key.2[0] + x / kx) * scale, (key.2[1] + y / ky) * scale))
    }

    /// Keeps why nobody sees driver window `w`, which the note on unconfirmed input to it gives.
    fn classify(&self, w: &Value) {
        if let Some(to) = w["pid"].as_i64().zip(w["window_id"].as_i64()) {
            let mut unseen = lock(&self.unseen);
            match unseen_of(w, self.os == Os::Mac) {
                Some(u) => unseen.insert(to, u),
                None => unseen.remove(&to),
            };
        }
    }

    async fn focus(&self, a: WindowArgs) -> Result<String, String> {
        let (w, _, _) = self.pick_window(Some(&a.window), a.pid, true).await?;
        let window_id: i64 =
            w.id.parse()
                .map_err(|_| format!("window id {:?} is not a number", w.id))?;
        let mut v = self
            .call(
                "bring_to_front",
                json!({"pid": w.pid, "window_id": window_id}),
            )
            .await?;
        let mut out = Value::from_iter([
            ("ok", Value::from(true)),
            ("title", w.title.into()),
            ("pid", w.pid.into()),
        ]);
        let done = &mut v["structuredContent"];
        if let Some(path) = done.get_mut("path") {
            out["path"] = path.take();
            out["front"] = done["observed"]["workspace_frontmost_pid"].take();
        }
        Ok(out.to_string())
    }

    async fn pick_window(
        &self,
        window: Option<&str>,
        pid: Option<u32>,
        by_title: bool,
    ) -> Result<(Frame, Value, usize), String> {
        let listed = self.call("list_windows", json!({})).await?;
        self.pick_listed(listed, window, pid, by_title).await
    }

    /// The driver window titled like `window` or of `pid`, as a frame and as the driver lists it, with how many listed windows its title picks among.
    async fn pick_listed(
        &self,
        mut listed: Value,
        window: Option<&str>,
        pid: Option<u32>,
        by_title: bool,
    ) -> Result<(Frame, Value, usize), String> {
        let mut raw = match listed["structuredContent"]["windows"].take() {
            Value::Array(windows) => windows,
            _ => Vec::new(),
        };
        let mac = self.os == Os::Mac;
        if mac {
            let keep: Vec<bool> = raw.iter().map(|w| pickable(w, &raw)).collect();
            let mut keep = keep.into_iter();
            raw.retain(|_| keep.next().unwrap_or(true));
        }
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
            let background = self.bg();
            let frames = self
                .access
                .run(CALL_LIMIT, (), move |b| shown(b, background))
                .await?;
            for w in &mut windows {
                w.app = app_of(&frames, w.pid).map(str::to_owned);
                let number = w.id.parse::<u32>().ok();
                match frames
                    .iter()
                    .find(|f| f.number.is_some() && f.number == number)
                {
                    Some(f) => {
                        w.title.clone_from(&f.title);
                        w.active = f.active;
                    }
                    None => {
                        w.active = frames
                            .iter()
                            .any(|f| f.active && f.pid == w.pid && f.title == w.title);
                    }
                }
            }
        }
        let rivals = window.map_or(1, |want| {
            let admitted = windows
                .iter()
                .filter(|w| self.allow.admits(w.app.as_deref()));
            window::rivals(admitted, want, pid)
        });
        let w = self.allow.pick(windows, window, pid)?;
        let id = w.id.parse::<i64>().ok();
        let at = raw.iter().position(|r| r["window_id"].as_i64() == id);
        if by_title
            && self.bg()
            && mac
            && let Some(listed) = at.map(|i| &raw[i])
        {
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
        let listed = at.map_or(Value::Null, |i| raw.swap_remove(i));
        self.classify(&listed);
        Ok((w, listed, rivals))
    }

    async fn shot(&self, a: ShotArgs) -> Result<Output, String> {
        let rect = [
            a.x.unwrap_or(0.0),
            a.y.unwrap_or(0.0),
            a.w.unwrap_or(f64::MAX / 4.0),
            a.h.unwrap_or(f64::MAX / 4.0),
        ];
        if let Some(window) = a.window.as_deref() {
            return self.window_shot(window, a.pid, rect).await;
        }
        let (grab, shown) = if self.allow.is_open() {
            (self.grab(rect).await?, None)
        } else {
            let (grab, shown) = self.steady_grab(rect).await?;
            (grab, Some(shown))
        };
        match grab {
            Grab::Read(c) => {
                let mask = self.mask(shown, 1.0).await?;
                let png = rgb_png(c.rgb, c.rect, mask.as_deref())?;
                Ok(delivered(png, sized(c.rect[2], c.rect[3])))
            }
            Grab::Driver(png, factor) => {
                let mask = self.mask(shown, factor).await?;
                let (w, h, png) = crop_png(&png, rect, factor, mask.as_deref()).map_err(|e| {
                    if e == OFF_SCREEN {
                        format!("{e}{OTHER_DISPLAY}")
                    } else {
                        e
                    }
                })?;
                Ok(delivered(png, sized(w, h)))
            }
        }
    }

    async fn grab(&self, rect: [f64; 4]) -> Result<Grab, String> {
        let overlay = self.overlay;
        if let Some(c) = self
            .access
            .run(CALL_LIMIT, (), move |b| b.capture(rect, overlay))
            .await?
        {
            return Ok(Grab::Read(c));
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
        Ok(Grab::Driver(png, factor))
    }

    async fn steady_grab(&self, rect: [f64; 4]) -> Result<(Grab, Vec<([f64; 4], bool)>), String> {
        for _ in 0..MASK_TRIES {
            let before = self.mask_rects().await?;
            let grab = self.grab(rect).await?;
            let after = self.mask_rects().await?;
            if after == before {
                return Ok((grab, after));
            }
        }
        Err(WINDOWS_MOVING.to_owned())
    }

    /// The image of the window titled `window`, as the driver captures that window alone, cropped to `rect` in its pixels.
    async fn window_shot(
        &self,
        window: &str,
        pid: Option<u32>,
        rect: [f64; 4],
    ) -> Result<Output, String> {
        let (_, w, _) = self.pick_window(Some(window), pid, true).await?;
        let key = window_key(&w)?;
        let stale = lock(&self.unseen)
            .get(&(key.0, key.1))
            .filter(|_| self.os == Os::Mac)
            .map(|u| {
                let app = w["app_name"].as_str().filter(|a| !a.is_empty());
                format!(
                    "pixels may be stale: {} repaints only when visible, and {}; do not conclude an action failed from this image",
                    app.unwrap_or("the app"),
                    u.why()
                )
            });
        let session = self.session.lock().await;
        let v = self
            .call("get_window_state", full_read(key.0, key.1))
            .await?;
        let _ = self.remember(key, &v, true);
        drop(session);
        let png = image_of(&v).ok_or("the driver returned no screenshot of the window")?;
        let png = B64
            .decode(png)
            .map_err(|e| format!("decode screenshot: {e}"))?;
        let (w, h, png) = crop_png(&png, rect, 1.0, None).map_err(|e| {
            if e == OFF_SCREEN {
                "the rectangle is off the window".to_owned()
            } else {
                e
            }
        })?;
        let mut out = sized(w, h);
        out["origin"] = json!([rect[0].max(0.0) as u32, rect[1].max(0.0) as u32]);
        if let Some(note) = stale {
            out["stale"] = true.into();
            out["note"] = note.into();
        }
        Ok(delivered(png, out))
    }

    async fn mask(
        &self,
        shown: Option<Vec<([f64; 4], bool)>>,
        factor: f64,
    ) -> Result<Option<Vec<([f64; 4], bool)>>, String> {
        let Some(mut mask) = shown else {
            return Ok(None);
        };
        let k = self.scale().await? * factor;
        for (rect, _) in &mut mask {
            *rect = rect.map(|v| v * k);
        }
        Ok(Some(mask))
    }

    async fn mask_rects(&self) -> Result<Vec<([f64; 4], bool)>, String> {
        let mut windows = self.on_screen().await?;
        let frames = self.access.run(CALL_LIMIT, (), |b| b.frames()).await?;
        windows.sort_by_key(|w| w["z_index"].as_i64().unwrap_or(i64::MIN));
        Ok(windows
            .into_iter()
            .map(|w| {
                let pid = w["pid"].as_u64().and_then(|p| u32::try_from(p).ok());
                (bounds(&w), self.allow.admits(app_of(&frames, pid)))
            })
            .collect())
    }
}

enum Grab {
    Read(Capture),
    Driver(Vec<u8>, f64),
}

struct Under {
    pid: i64,
    id: i64,
    bounds: [f64; 4],
    point: (f64, f64),
}

impl Under {
    fn of(w: &Value, point: (f64, f64)) -> Option<Self> {
        Some(Self {
            pid: w["pid"].as_i64()?,
            id: w["window_id"].as_i64()?,
            bounds: bounds(w),
            point,
        })
    }
}

/// The window of the last background click and its desk point, which typing into that window checks the focus against.
#[derive(Clone, Copy)]
struct Clicked {
    to: (i64, i64),
    at: (f64, f64),
}

struct Held {
    was: bool,
    holders: HashSet<String>,
}

#[derive(Clone, Copy)]
struct Read {
    pid: i64,
    id: i64,
    bounds: [f64; 4],
    k: (f64, f64),
    full: bool,
}

/// Why nobody sees a window, which its app then may not repaint.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Unseen {
    OffSpace,
    Hidden,
}

impl Unseen {
    fn why(self) -> &'static str {
        match self {
            Self::OffSpace => "the window is on another Space",
            Self::Hidden => "the window is minimized or hidden",
        }
    }
}

/// Moves a Linux launch into a systemd scope of its own; false when it stays in the daemon's cgroup.
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

/// The name a `launch_app` call gets for `app`: on Linux, the desktop file id of the application whose menu name `app` repeats, else `app`.
#[cfg(target_os = "linux")]
pub(crate) fn launch_name(app: &str) -> Cow<'_, str> {
    crate::entry::id_for(app).map_or(Cow::Borrowed(app), Cow::Owned)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn launch_name(app: &str) -> Cow<'_, str> {
    Cow::Borrowed(app)
}

pub(crate) fn runs_app(window_app: &str, app: &str) -> bool {
    names_app(window_app, app) || names_app(window_app, &launch_name(app))
}

pub(crate) fn without_protocol_tail(text: &str) -> &str {
    let mut end = text.trim_end().len();
    let mut stripped = false;
    loop {
        let head = text[..end].trim_end();
        let Some(rest) = head.strip_suffix('>') else {
            break;
        };
        let Some(open) = rest.rfind("</") else {
            break;
        };
        let name = &rest[open + 2..];
        if name.is_empty()
            || name.contains(char::is_whitespace)
            || !PROTOCOL_TAGS.iter().any(|t| name.eq_ignore_ascii_case(t))
            || opens(&head[..open], name)
        {
            break;
        }
        end = open;
        stripped = true;
    }
    if stripped {
        text[..end].trim_end()
    } else {
        text
    }
}

/// Runs of `text` with at most [`RUN_CHARS`] distinct non-ASCII characters, one spare keycode each.
pub(crate) fn text_runs(text: &str) -> Vec<&str> {
    let (mut runs, mut start, mut seen) = (Vec::new(), 0, Vec::new());
    for (i, c) in text.char_indices() {
        if c.is_ascii() || seen.contains(&c) {
            continue;
        }
        if seen.len() == RUN_CHARS {
            runs.push(&text[start..i]);
            start = i;
            seen.clear();
        }
        seen.push(c);
    }
    if start < text.len() {
        runs.push(&text[start..]);
    }
    runs
}

/// Whether the typer typed `text`; a drop of this call stops it at its next key.
pub(crate) async fn xtest(typer: Typer, text: &str, turn: Option<Turn>) -> Result<bool, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let _cancel = Cancel(stop.clone());
    let text = text.to_owned();
    tokio::task::spawn_blocking(move || {
        let _turn = turn;
        typer(&text, &stop)
    })
    .await
    .map_err(|e| format!("XTest: {e}"))?
}

fn opens(prelude: &str, name: &str) -> bool {
    let prelude = prelude.to_ascii_lowercase();
    let tag = format!("<{}", name.to_ascii_lowercase());
    prelude.match_indices(&tag).any(|(i, _)| {
        prelude[i + tag.len()..]
            .chars()
            .next()
            .is_none_or(|c| c == '>' || c.is_whitespace())
    })
}

fn names_app(window_app: &str, wanted: &str) -> bool {
    let (have, want) = (words(window_app), words(wanted));
    !have.is_empty()
        && !want.is_empty()
        && (want.iter().all(|w| have.iter().any(|h| part_of_word(h, w)))
            || have.iter().all(|h| want.contains(h)))
}

fn part_of_word(word: &str, part: &str) -> bool {
    word == part
        || (part.len() >= 3
            && word
                .strip_prefix(part)
                .is_some_and(|rest| rest.starts_with(char::is_alphanumeric)))
        || (part.len() >= 4 && word.ends_with(part))
}

fn words(name: &str) -> Vec<String> {
    name.split(|c: char| !c.is_alphanumeric() && c != '+')
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn shown<B: Access>(b: &mut B, background: bool) -> Result<Vec<Frame>, String> {
    let mut frames = b.frames()?;
    if background {
        frames.extend(b.off_space());
    }
    Ok(frames)
}

/// The windows a call picks from: the app of `pid` alone, unless the allowlist needs every window to judge the call.
fn frames<B: Access>(b: &mut B, pid: Option<u32>, allow: &Allow) -> Result<Vec<Frame>, String> {
    match pid {
        Some(p) if allow.is_open() => b.frames_of(p),
        _ => b.frames(),
    }
}

fn holding<B: Access>(
    b: &mut B,
    frames: &[Frame],
    window: Option<&str>,
    pid: Option<u32>,
    (x, y): (i32, i32),
) -> Option<Result<Frame, String>> {
    let want = window?;
    let alike: Vec<&Frame> = (0..TITLE_TIERS)
        .map(|tier| {
            frames
                .iter()
                .filter(|f| {
                    pid.is_none_or(|p| f.pid == Some(p)) && title_matches(tier, &f.title, want)
                })
                .collect::<Vec<_>>()
        })
        .find(|alike| !alike.is_empty())?;
    if alike.len() < 2 {
        return None;
    }
    let held: Vec<&Frame> = alike
        .iter()
        .copied()
        .filter(|f| {
            b.node(&f.id, Extras::FRAME, 0)
                .ok()
                .and_then(|n| n.bounds)
                .is_some_and(|[l, t, w, h]| x >= l && x < l + w && y >= t && y < t + h)
        })
        .collect();
    Some(match held.as_slice() {
        [one] => Ok(Frame::clone(one)),
        _ => Err(format!(
            "{} windows match the title {want:?}, and their bounds do not single out the one at the point; give one of them a title of its own",
            alike.len()
        )),
    })
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
            "window list is stale: {:?} did not answer in time, so its app is busy or hung; wait and retry, or close the app",
            frame.title
        ));
    }
    Ok(frame)
}

fn delivered(png: Vec<u8>, out: Value) -> Output {
    Output {
        text: out.to_string(),
        image_b64: Some(B64.encode(&png)),
        ..Output::default()
    }
}

fn sized(w: u32, h: u32) -> Value {
    json!({"ok": true, "width": w, "height": h})
}

/// The last characters of the last line of `text`, which a field holds once it has read the typing.
fn typed_tail(text: &str) -> Option<String> {
    let line = text.rsplit('\n').next()?.trim_end_matches('\r');
    let n = line.chars().count();
    (n > 0).then(|| line.chars().skip(n.saturating_sub(TYPE_TAIL)).collect())
}

fn at_lock(os: Os, e: String) -> String {
    if os == Os::Mac && e.contains("cannot receive input: the screen is locked") {
        format!("{e}; {LOCKED_SCREEN}")
    } else {
        e
    }
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

/// Whether `frames` hold driver window `w` as an unowned top-level window: by window number where frames carry one, else by title.
fn lists_top_level(frames: &[Frame], w: &Value) -> bool {
    let pid = w["pid"].as_u64().and_then(|p| u32::try_from(p).ok());
    let number = w["window_id"].as_u64().and_then(|n| u32::try_from(n).ok());
    let title = w["title"].as_str().unwrap_or_default().trim();
    frames.iter().any(|f| {
        f.pid == pid
            && f.owner.is_none()
            && match f.number {
                Some(n) => Some(n) == number,
                None => f.title.trim() == title,
            }
    })
}

fn window_key(w: &Value) -> Result<(i64, i64, [f64; 4]), String> {
    w["pid"]
        .as_i64()
        .zip(w["window_id"].as_i64())
        .map(|(pid, id)| (pid, id, bounds(w)))
        .ok_or_else(|| "the driver lists the window without its pid or window id".to_owned())
}

fn full_read(pid: i64, id: i64) -> Value {
    json!({
        "session": WINDOW_SESSION,
        "pid": pid,
        "window_id": id,
        "include_accessibility_tree": false,
        "max_image_dimension": 0
    })
}

fn refuses_pointer(v: &Value) -> bool {
    v["structuredContent"]["background_input"]["routes"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|r| r["route"] == "window_pointer" && r["status"] == "refused")
}

/// The image pixels per point of driver read `v` of window `id`, across and down.
fn image_scale(v: &Value, id: i64) -> Result<(f64, f64), String> {
    let c = &v["structuredContent"];
    let ratio = |px: &str, pt: &str| {
        c[px]
            .as_f64()
            .zip(c["window_bounds"][pt].as_f64())
            .filter(|&(_, pt)| pt > 0.0)
            .map(|(px, pt)| px / pt)
            .ok_or_else(|| format!("get_window_state of window {id} has no {px}"))
    };
    Ok((
        ratio("screenshot_width", "width")?,
        ratio("screenshot_height", "height")?,
    ))
}

/// A driver window's layer, 0 when the driver gives none, as on Linux.
fn layer(w: &Value) -> i64 {
    w["layer"].as_i64().unwrap_or(0)
}

/// Whether a title or pid may pick macOS driver window `w` of `all`: one on screen, or one off screen in a known Space.
fn pickable(w: &Value, all: &[Value]) -> bool {
    let shown = |o: &Value| o["is_on_screen"] != false;
    shown(w)
        || (!w["on_current_space"].is_null()
            && (w["on_current_space"] != true
                || !all
                    .iter()
                    .any(|o| shown(o) && o["pid"] == w["pid"] && o["title"] == w["title"])))
}

fn off_space(w: &Value) -> bool {
    w["on_current_space"] == false && w["space_ids"].as_array().is_some_and(|s| !s.is_empty())
}

fn unseen_of(w: &Value, mac: bool) -> Option<Unseen> {
    if off_space(w) {
        return Some(Unseen::OffSpace);
    }
    (mac && w["is_on_screen"] == false).then_some(Unseen::Hidden)
}

fn holds(n: &Node, (x, y): (f64, f64)) -> Option<bool> {
    let [l, t, w, h] = n.bounds?.map(f64::from);
    Some(x >= l && x < l + w && y >= t && y < t + h)
}

fn ok_with(unverified: Option<String>) -> String {
    match unverified {
        Some(note) => {
            Value::from_iter([("ok", Value::from(true)), ("unverified", note.into())]).to_string()
        }
        None => OK.to_owned(),
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
    use std::sync::atomic::Ordering;
    use std::sync::{Mutex, OnceLock};

    use desk_core::text::clip;

    use super::*;
    use crate::model::{MAX_IDS, Tree, evict_ids, lookup};

    const PID_CALLS: [(&str, &str); 4] = [
        ("desk_snapshot", r#"{"pid": 4}"#),
        ("desk_find", r#"{"name": "Save", "pid": 4}"#),
        ("desk_at", r#"{"x": 1, "y": 1, "pid": 4}"#),
        ("desk_close", r#"{"window": "Notepad", "pid": 4}"#),
    ];

    const LONG_TITLE: &str = "Example Competitive Analysis: Pricing and GTM (October 2026) - Example Docs - Google Chrome \u{2013} Work (example.com)";

    static ACTED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static PAGE_LIT: AtomicBool = AtomicBool::new(false);
    static LAUNCHED: AtomicBool = AtomicBool::new(false);
    static STOPPED: AtomicBool = AtomicBool::new(false);

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

    #[tokio::test]
    async fn the_unanswered_front_window_is_named_only_for_an_allowed_app() {
        let busy = || {
            Ok(Stub {
                unanswered: Some(frame(
                    "",
                    "answers.xlsx - LibreOffice Calc",
                    5173,
                    false,
                    Some("soffice.bin"),
                )),
                ..Stub::default()
            })
        };
        let open = Desk::new(busy, Slow(Arc::default()), Os::Linux);
        let out = open.run("desk_unanswered", json!({})).await.unwrap().text;
        let front: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            (&front["title"], &front["pid"], &front["app"]),
            (
                &json!("answers.xlsx - LibreOffice Calc"),
                &json!(5173),
                &json!("soffice.bin")
            ),
            "{out}"
        );
        let gated = Desk::new(busy, Slow(Arc::default()), Os::Linux).allow(&["mousepad".into()]);
        assert_eq!(
            gated.run("desk_unanswered", json!({})).await.unwrap().text,
            "null"
        );
        let none = Desk::new(|| Ok(Stub::default()), Slow(Arc::default()), Os::Linux);
        assert_eq!(
            none.run("desk_unanswered", json!({})).await.unwrap().text,
            "null"
        );
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
            [
                "hotkey background",
                "bring_to_front -",
                "bring_to_front -",
                "hotkey desktop"
            ]
        );
        assert_eq!(sent(&calls, "bring_to_front")[0]["window_id"], 1);
    }

    #[tokio::test]
    async fn input_naming_an_app_goes_to_that_app_or_nowhere() {
        for (background, auto) in [(true, true), (true, false), (false, false)] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Mac)
                .background(background)
                .auto(auto);
            for (tool, args) in [
                ("desk_key", json!({"keys": "cmd+0", "app": "Notes"})),
                ("desk_type", json!({"text": "a", "app": "Notes"})),
                (
                    "desk_click",
                    json!({"x": 300.0, "y": 250.0, "app": "Notes"}),
                ),
            ] {
                let err = desk.run(tool, args).await.unwrap_err();
                assert!(
                    err.contains("app \"Notes\" has no window") && err.contains("nothing was sent"),
                    "{background} {auto} {tool}: {err}"
                );
            }
            assert!(
                order(&calls).is_empty() && sent(&calls, "click").is_empty(),
                "{background} {auto}: {:?}",
                order(&calls)
            );
            desk.run("desk_key", json!({"keys": "cmd+0", "app": "notepad"}))
                .await
                .unwrap();
            let key = sent(&calls, "hotkey");
            assert_eq!(key.len(), 1, "{background} {auto}");
            if background {
                assert_eq!(key[0]["target"]["pid"], 4, "{}", key[0]);
            } else {
                assert_eq!(sent(&calls, "bring_to_front")[0]["pid"], 4);
            }
        }
    }

    #[tokio::test]
    async fn a_macos_key_raises_its_window_only_when_it_is_not_already_active() {
        fn listed(active: bool) -> Stub {
            Stub {
                frames: vec![Frame {
                    number: Some(1),
                    ..frame("w", "Untitled - Notepad", 4, active, Some("Notepad"))
                }],
                ..Stub::default()
            }
        }
        async fn raises(stub: fn() -> Result<Stub, String>) -> usize {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(stub, Behind(Arc::clone(&calls)), Os::Mac);
            desk.run(
                "desk_key",
                json!({"keys": "cmd+0", "window": "Untitled - Notepad"}),
            )
            .await
            .unwrap();
            assert_eq!(sent(&calls, "hotkey").len(), 1);
            sent(&calls, "bring_to_front").len()
        }
        assert_eq!(raises(|| Ok(listed(true))).await, 0);
        assert_eq!(raises(|| Ok(listed(false))).await, 1);
        let stale = || {
            let mut s = listed(true);
            s.frames[0].stale = true;
            Ok(s)
        };
        assert_eq!(raises(stale).await, 1);
    }

    #[tokio::test]
    async fn a_windows_background_type_sends_a_shifted_character_as_text_not_a_shift_hotkey() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk =
            Desk::new(|| Ok(notepad()), Behind(Arc::clone(&calls)), Os::Windows).background(true);
        desk.run("desk_type", json!({"text": "aB!", "pid": 4}))
            .await
            .unwrap();
        assert!(sent(&calls, "hotkey").is_empty(), "{:?}", order(&calls));
        let keys: Vec<Value> = sent(&calls, "press_key")
            .into_iter()
            .map(|r| r["key"].clone())
            .collect();
        let texts: Vec<Value> = sent(&calls, "type_text")
            .into_iter()
            .map(|r| r["text"].clone())
            .collect();
        assert_eq!(keys, [json!("a")]);
        assert_eq!(texts, [json!("B!")]);
    }

    #[tokio::test]
    async fn a_windows_background_type_sends_each_run_without_a_single_key_as_one_text() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk =
            Desk::new(|| Ok(notepad()), Behind(Arc::clone(&calls)), Os::Windows).background(true);
        desk.run("desk_type", json!({"text": "aB!@c€D\tE\rF", "pid": 4}))
            .await
            .unwrap();
        let seq: Vec<String> = calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(t, a)| match t.as_str() {
                "press_key" => Some(format!("press {}", a["key"].as_str().unwrap_or_default())),
                "type_text" => Some(format!("type {}", a["text"].as_str().unwrap_or_default())),
                "hotkey" => Some("hotkey".to_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(
            seq,
            [
                "press a",
                "type B!@",
                "press c",
                "type €D",
                "type \t",
                "type E",
                "type \r",
                "type F"
            ]
        );
    }

    #[tokio::test]
    async fn a_type_that_fails_on_a_later_line_says_how_many_went_in() {
        let desk = Desk::new(|| Ok(Apps), FailsOn("b"), Os::Mac);
        let err = desk
            .run("desk_type", json!({"text": "a\nb\nc"}))
            .await
            .unwrap_err();
        assert!(
            err.ends_with("; 1 of 3 lines were typed before it"),
            "{err}"
        );
        let err = desk
            .run("desk_type", json!({"text": "b"}))
            .await
            .unwrap_err();
        assert!(!err.contains("lines were typed"), "{err}");
    }

    #[tokio::test]
    async fn a_type_into_a_terminal_says_so_whether_or_not_the_driver_confirmed_it() {
        let tty = "Sent (unverified) 1 char(s) via CGEvent (terminal emulator, 0ms delay).";
        for (quiet, said, terminal) in [
            ("a", tty, true),
            ("-", tty, true),
            ("a", "Posted via CGEvent.", false),
            ("-", "Posted via CGEvent.", false),
        ] {
            let desk =
                Desk::new(|| Ok(Apps), Tty(Arc::default(), quiet, said), Os::Mac).background(true);
            let out = desk
                .run("desk_type", json!({"text": "a", "window": "Doc"}))
                .await
                .unwrap();
            let v: Value = serde_json::from_str(&out.text).unwrap();
            assert_eq!(v["terminal"] == true, terminal, "{quiet} {said}: {v}");
        }
    }

    #[tokio::test]
    async fn a_line_a_terminal_did_not_confirm_gets_no_return_and_other_lines_do() {
        let tty = "Sent (unverified) 1 char(s) via CGEvent (terminal emulator, 0ms delay).";
        for (quiet, said, text, stopped, returns) in [
            ("a", tty, "a\nb", Some("line 1 of 2"), 0),
            ("b", tty, "a\nb\nc", Some("line 2 of 3"), 1),
            ("b", tty, "a\nb", None, 1),
            ("-", tty, "a\nb", None, 1),
            ("a", "Posted via CGEvent.", "a\nb", None, 1),
        ] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(|| Ok(Apps), Tty(Arc::clone(&calls), quiet, said), Os::Mac)
                .background(true);
            let out = desk
                .run("desk_type", json!({"text": text, "window": "Doc"}))
                .await;
            match stopped {
                Some(line) => {
                    let err = out.unwrap_err();
                    assert!(
                        err.contains(&format!(
                            "the terminal did not confirm {line}, so Return was not sent: unverifiable: {tty}"
                        )),
                        "{err}"
                    );
                    assert_eq!(
                        err.ends_with("; 1 of 3 lines were typed before it"),
                        returns == 1,
                        "{err}"
                    );
                }
                None => {
                    let note = note(out.unwrap());
                    assert_eq!(note.is_empty(), quiet == "-", "{text} {quiet}: {note}");
                }
            }
            assert_eq!(sent(&calls, "press_key").len(), returns, "{text} {quiet}");
            assert_eq!(
                sent(&calls, "type_text").len(),
                returns + 1,
                "{text} {quiet}"
            );
        }
    }

    #[tokio::test]
    async fn a_text_read_keeps_its_head_or_with_tail_its_end_and_says_when_it_was_cut() {
        let desk = Desk::new(
            || Ok(Scrollback),
            Stack(Arc::default(), Mutex::new(json!({}))),
            Os::Mac,
        );
        let head = "a".repeat(READ_CHARS);
        let end = format!("{}中🙂end", "a".repeat(READ_CHARS - 5));
        let edge = "b".repeat(READ_CHARS);
        for (id, tail, want, cut) in [
            ("long", false, &head, true),
            ("long", true, &end, true),
            ("edge", true, &edge, false),
            ("edge", false, &edge, false),
        ] {
            let out = desk
                .run("desk_read", json!({"id": id, "text": true, "tail": tail}))
                .await
                .unwrap();
            let v: Value = serde_json::from_str(&out.text).unwrap();
            assert!(v["value"] == want.as_str(), "{id} {tail}");
            assert_eq!(v.get("truncated").is_some(), cut, "{id} {tail}");
            assert_eq!(v["truncated"] == true, cut, "{id} {tail}");
        }
        let err = desk
            .run("desk_read", json!({"id": "long", "tail": true}))
            .await
            .unwrap_err();
        assert_eq!(err, READ_TAIL);
    }

    #[tokio::test]
    async fn a_read_without_text_previews_eighty_characters_of_the_value_and_the_whole_name() {
        let desk = Desk::new(
            || Ok(Scrollback),
            Stack(Arc::default(), Mutex::new(json!({}))),
            Os::Mac,
        );
        let out = desk.run("desk_read", json!({"id": "edge"})).await.unwrap();
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(v["value"], "b".repeat(NAME_CHARS).as_str());
        assert_eq!(v["name"], "n".repeat(100).as_str());
        assert!(v.get("truncated").is_none(), "{v}");
    }

    #[tokio::test]
    async fn auto_input_asked_for_the_foreground_passes_the_allowlist_by_the_front_window() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Windows)
            .background(true)
            .auto(true)
            .allow(&["Mail".to_owned()]);
        let err = desk
            .run(
                "desk_key",
                json!({"keys": "ctrl+s", "window": "Inbox", "delivery_mode": "foreground"}),
            )
            .await
            .unwrap_err();
        assert!(err.contains("allowlist"), "{err}");
        assert!(order(&calls).is_empty(), "{:?}", order(&calls));
    }

    #[tokio::test]
    async fn auto_input_asked_for_the_foreground_runs_there_without_a_window() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Mac)
            .background(true)
            .auto(true);
        desk.run(
            "desk_key",
            json!({"keys": "cmd+0", "delivery_mode": "foreground"}),
        )
        .await
        .unwrap();
        assert_eq!(order(&calls), ["hotkey desktop"]);
        let err = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Mac)
            .background(true)
            .run(
                "desk_key",
                json!({"keys": "cmd+0", "delivery_mode": "foreground"}),
            )
            .await
            .unwrap_err();
        assert!(
            err.contains("delivery_mode \"foreground\" refused"),
            "{err}"
        );
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

    #[tokio::test]
    async fn a_shot_by_window_captures_that_window_on_any_display_within_the_allowlist() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), SecondDisplay(Arc::clone(&calls)), Os::Mac);
        let size = |out: Output| {
            let v: Value = serde_json::from_str(&out.text).unwrap();
            (
                v["width"].clone(),
                v["height"].clone(),
                out.image_b64.is_some(),
            )
        };
        let whole = desk
            .run("desk_shot", json!({"window": "\u{5fae}\u{4fe1}"}))
            .await
            .unwrap();
        assert_eq!(size(whole), (json!(846), json!(774), true));
        let read = sent(&calls, "get_window_state");
        assert_eq!(read.len(), 1, "{read:?}");
        assert_eq!(
            (
                &read[0]["window_id"],
                &read[0]["include_accessibility_tree"]
            ),
            (&json!(100), &json!(false))
        );
        assert!(sent(&calls, "get_desktop_state").is_empty());
        let part = desk
            .run(
                "desk_shot",
                json!({"window": "\u{5fae}\u{4fe1}", "x": 10, "y": 20, "w": 100, "h": 50}),
            )
            .await
            .unwrap();
        assert_eq!(size(part), (json!(100), json!(50), true));
        desk.run("desk_shot", json!({"pid": 9})).await.unwrap();
        assert_eq!(sent(&calls, "get_window_state").len(), 2);
        assert_eq!(sent(&calls, "get_desktop_state").len(), 1);
        let off = desk
            .run("desk_shot", json!({"x": -1416, "y": 0, "w": 100, "h": 100}))
            .await
            .unwrap_err();
        assert!(off.ends_with(OTHER_DISPLAY), "{off}");
        let gated_calls = Arc::new(Mutex::new(Vec::new()));
        let gated = Desk::new(
            || Ok(Apps),
            SecondDisplay(Arc::clone(&gated_calls)),
            Os::Mac,
        )
        .allow(&["notepad".into()]);
        let refused = gated
            .run("desk_shot", json!({"window": "\u{5fae}\u{4fe1}"}))
            .await
            .unwrap_err();
        assert_eq!(
            refused,
            "no window \"\u{5fae}\u{4fe1}\"; windows: [\"Untitled - Notepad\"]"
        );
        assert!(sent(&gated_calls, "get_window_state").is_empty());
    }

    #[tokio::test]
    async fn a_click_after_a_shot_of_its_window_lands_where_the_click_before_it_landed() {
        for background in [false, true] {
            let driver = Wide::new(2.0);
            let desk = Desk::new(|| Ok(notepad()), driver.clone(), Os::Mac).background(background);
            let point = json!({"x": -1600.0, "y": 800.0});
            desk.run("desk_click", point.clone()).await.unwrap();
            let before = driver.landed();
            desk.run("desk_shot", json!({"window": "Wide"}))
                .await
                .unwrap();
            desk.run("desk_click", point).await.unwrap();
            let after = driver.landed();
            assert_eq!(before, [(-800.0, 400.0)], "background {background}");
            assert_eq!(after[1], (-800.0, 400.0), "background {background}");
        }
    }

    #[tokio::test]
    async fn with_a_window_x_and_y_are_pixels_of_its_shot_on_any_display_and_scale() {
        for (main, background, shot_first) in [
            (1.0, true, false),
            (2.0, true, true),
            (1.0, false, true),
            (2.0, false, false),
        ] {
            let driver = Wide::new(main);
            let desk = Desk::new(|| Ok(notepad()), driver.clone(), Os::Mac).background(background);
            if shot_first {
                desk.run("desk_shot", json!({"window": "Wide"}))
                    .await
                    .unwrap();
            } else {
                desk.run("desk_click", json!({"x": -800.0 * main, "y": 400.0 * main}))
                    .await
                    .unwrap();
            }
            desk.run(
                "desk_click",
                json!({"x": 1000.0, "y": 600.0, "window": "Wide"}),
            )
            .await
            .unwrap();
            assert_eq!(
                driver.landed().last(),
                Some(&(-800.0, 400.0)),
                "main {main}, background {background}, shot first {shot_first}"
            );
        }
    }

    #[tokio::test]
    async fn a_scroll_or_drag_with_a_window_takes_pixels_of_its_shot() {
        let driver = Wide::new(2.0);
        let back = Desk::new(|| Ok(notepad()), driver.clone(), Os::Mac).background(true);
        back.run(
            "desk_scroll",
            json!({"direction": "down", "x": 1000.0, "y": 600.0, "window": "Wide"}),
        )
        .await
        .unwrap();
        back.run(
            "desk_drag",
            json!({"x": 100.0, "y": 200.0, "to_x": 1000.0, "to_y": 600.0, "window": "Wide"}),
        )
        .await
        .unwrap();
        let (scroll, drag) = (driver.sent("scroll"), driver.sent("drag"));
        assert_eq!(
            (&scroll[0]["x"], &scroll[0]["y"]),
            (&json!(1000.0), &json!(600.0))
        );
        assert_eq!(
            [
                &drag[0]["from_x"],
                &drag[0]["from_y"],
                &drag[0]["to_x"],
                &drag[0]["to_y"]
            ],
            [&json!(100.0), &json!(200.0), &json!(1000.0), &json!(600.0)]
        );
        assert_eq!(*driver.ratio.lock().unwrap(), 1.0);
        let driver = Wide::new(2.0);
        let fore = Desk::new(|| Ok(notepad()), driver.clone(), Os::Mac);
        fore.run(
            "desk_scroll",
            json!({"direction": "down", "x": 1000.0, "y": 600.0, "window": "Wide"}),
        )
        .await
        .unwrap();
        fore.run(
            "desk_drag",
            json!({"x": 100.0, "y": 200.0, "to_x": 1000.0, "to_y": 600.0, "window": "Wide"}),
        )
        .await
        .unwrap();
        let (scroll, drag) = (driver.sent("scroll"), driver.sent("drag"));
        assert_eq!(
            (&scroll[0]["x"], &scroll[0]["y"], &scroll[0]["scope"]),
            (&json!(-1600.0), &json!(800.0), &json!("desktop"))
        );
        assert_eq!(
            [
                &drag[0]["from_x"],
                &drag[0]["from_y"],
                &drag[0]["to_x"],
                &drag[0]["to_y"]
            ],
            [
                &json!(-2500.0),
                &json!(400.0),
                &json!(-1600.0),
                &json!(800.0)
            ]
        );
    }

    #[tokio::test]
    async fn a_title_two_windows_share_takes_no_points_in_shot_pixels() {
        let driver = Wide::with(1.0, ("Wide", Some("Wide")), false);
        let desk = Desk::new(|| Ok(notepad()), driver.clone(), Os::Mac).background(true);
        for (tool, args) in [
            (
                "desk_click",
                json!({"x": 10.0, "y": 10.0, "window": "Wide"}),
            ),
            (
                "desk_scroll",
                json!({"direction": "down", "x": 10.0, "y": 10.0, "window": "Wide"}),
            ),
            (
                "desk_drag",
                json!({"x": 10.0, "y": 10.0, "to_x": 20.0, "to_y": 20.0, "window": "Wide"}),
            ),
        ] {
            let err = desk.run(tool, args).await.unwrap_err();
            assert!(err.contains("a title of its own"), "{tool}: {err}");
        }
        assert!(driver.sent("click").is_empty() && driver.sent("scroll").is_empty());
        desk.run("desk_click", json!({"x": -800.0, "y": 400.0}))
            .await
            .unwrap();
        assert_eq!(driver.landed(), [(-800.0, 400.0)]);
    }

    #[tokio::test]
    async fn a_title_that_matches_two_windows_at_one_tier_takes_no_points_in_shot_pixels() {
        let driver = Wide::with(1.0, ("Draft — Chrome", Some("Inbox — Chrome")), false);
        let desk = Desk::new(|| Ok(notepad()), driver.clone(), Os::Mac).background(true);
        let click = |window: &'static str| {
            desk.run(
                "desk_click",
                json!({"x": 1000.0, "y": 600.0, "window": window}),
            )
        };
        let err = click("Chrome").await.unwrap_err();
        assert!(err.contains("a longer title"), "{err}");
        assert!(driver.landed().is_empty());
        click("Draft").await.unwrap();
        assert_eq!(driver.landed(), [(-800.0, 400.0)]);
    }

    #[tokio::test]
    async fn a_window_shot_during_a_click_does_not_change_the_scale_the_click_is_sent_by() {
        let driver = Wide::with(2.0, ("Wide", None), true);
        let desk = Desk::new(|| Ok(notepad()), driver.clone(), Os::Mac).background(true);
        let (clicked, shot) = tokio::join!(
            desk.run("desk_click", json!({"x": -1600.0, "y": 800.0})),
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                desk.run("desk_shot", json!({"window": "Wide"})).await
            }
        );
        clicked.unwrap();
        shot.unwrap();
        assert_eq!(driver.landed(), [(-800.0, 400.0)]);
    }

    #[tokio::test]
    async fn a_cropped_window_shot_names_its_origin_in_the_window_pixels() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), SecondDisplay(Arc::clone(&calls)), Os::Mac);
        let origin = |args: Value| async {
            let out = desk.run("desk_shot", args).await.unwrap();
            serde_json::from_str::<Value>(&out.text).unwrap()["origin"].clone()
        };
        assert_eq!(
            origin(json!({"window": "\u{5fae}\u{4fe1}"})).await,
            json!([0, 0])
        );
        assert_eq!(
            origin(json!({"window": "\u{5fae}\u{4fe1}", "x": 10.5, "y": 20, "w": 100, "h": 50}))
                .await,
            json!([10, 20])
        );
    }

    #[tokio::test]
    async fn a_macos_window_shot_says_its_pixels_may_be_stale_on_another_space_or_minimized_and_not_when_covered()
     {
        let away = json!({"is_on_screen": false, "on_current_space": false, "space_ids": [18], "bounds": {"x": 900.0, "y": 100.0, "width": 400.0, "height": 300.0}});
        let mut minimized = doc_at(900.0);
        minimized["is_on_screen"] = false.into();
        for (os, doc, why) in [
            (Os::Mac, doc_at(100.0), None),
            (Os::Mac, away.clone(), Some(Unseen::OffSpace)),
            (Os::Mac, minimized, Some(Unseen::Hidden)),
            (Os::Mac, doc_at(900.0), None),
            (Os::Windows, doc_at(100.0), None),
            (Os::Windows, away, None),
        ] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(
                || Ok(Apps),
                Stack(Arc::clone(&calls), Mutex::new(doc.clone())),
                os,
            );
            let out = desk
                .run("desk_shot", json!({"window": "Doc"}))
                .await
                .unwrap();
            assert!(out.image_b64.is_some(), "{os:?} {doc}");
            let v: Value = serde_json::from_str(&out.text).unwrap();
            match why {
                Some(why) => {
                    assert_eq!(v["stale"], true, "{os:?} {doc} {v}");
                    assert_eq!(
                        v["note"],
                        format!(
                            "pixels may be stale: TextEdit repaints only when visible, and {}; do not conclude an action failed from this image",
                            why.why()
                        ),
                        "{doc}"
                    );
                }
                None => {
                    assert!(
                        v.get("stale").is_none() && v.get("note").is_none(),
                        "{os:?} {doc} {v}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn unconfirmed_background_input_to_a_minimized_window_may_have_taken_effect_and_to_a_covered_one_keeps_the_driver_note()
     {
        let mut minimized = doc_at(900.0);
        minimized["is_on_screen"] = false.into();
        for (doc, hidden) in [(minimized, true), (doc_at(100.0), false)] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(
                || Ok(Apps),
                Stack(Arc::clone(&calls), Mutex::new(doc.clone())),
                Os::Mac,
            )
            .background(true);
            let clicked = desk
                .run("desk_click", json!({"x": 10.0, "y": 10.0, "window": "Doc"}))
                .await
                .unwrap();
            let typed = desk
                .run("desk_type", json!({"text": "x", "window": "Doc"}))
                .await
                .unwrap();
            let (clicked, typed) = (note(clicked), note(typed));
            if hidden {
                let why = Unseen::Hidden.why();
                assert_eq!(clicked, format!("unverifiable; {why}: {UNSEEN_SENT}"));
                assert_eq!(typed, format!("unverifiable; {why}: {UNSEEN_TYPED}"));
            } else {
                assert_eq!(clicked, "unverifiable: Posted via CGEvent.");
                assert_eq!(typed, "unverifiable: Posted via CGEvent.");
            }
            assert_eq!(sent(&calls, "click").len(), 1, "{doc}");
            assert_eq!(sent(&calls, "type_text").len(), 1, "{doc}");
        }
    }

    #[tokio::test]
    async fn a_restored_window_clicked_by_its_point_keeps_the_driver_note() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut minimized = doc_at(900.0);
        minimized["is_on_screen"] = false.into();
        let desk = Desk::new(
            || Ok(Apps),
            Stack(Arc::clone(&calls), Mutex::new(minimized)),
            Os::Mac,
        )
        .background(true);
        let shot = desk
            .run("desk_shot", json!({"window": "Doc"}))
            .await
            .unwrap();
        assert!(shot.text.contains("\"stale\":true"), "{}", shot.text);
        *desk.driver.1.lock().unwrap() = doc_at(900.0);
        let out = desk
            .run("desk_click", json!({"x": 1000.0, "y": 200.0}))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(v["unverified"], "unverifiable: Posted via CGEvent.");
        assert_eq!(sent(&calls, "click")[0]["window_id"], 3);
    }

    #[test]
    fn a_window_is_unseen_on_another_space_and_on_macos_also_off_screen() {
        let doc = json!({"window_id": 1, "is_on_screen": true, "on_current_space": true, "space_ids": [8]});
        let mut away = doc.clone();
        away["is_on_screen"] = false.into();
        away["on_current_space"] = false.into();
        away["space_ids"] = json!([18]);
        let mut minimized = doc.clone();
        minimized["is_on_screen"] = false.into();
        for mac in [true, false] {
            assert_eq!(unseen_of(&doc, mac), None);
            assert_eq!(unseen_of(&away, mac), Some(Unseen::OffSpace));
        }
        assert_eq!(unseen_of(&minimized, true), Some(Unseen::Hidden));
        assert_eq!(unseen_of(&minimized, false), None);
    }

    #[tokio::test]
    async fn a_title_picks_the_on_screen_window_over_its_hidden_twin_and_still_reaches_a_lone_minimized_window()
     {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), HiddenTwin(Arc::clone(&calls)), Os::Mac).background(true);
        desk.run(
            "desk_click",
            json!({"x": -1000.0, "y": 300.0, "window": "\u{5fae}\u{4fe1}"}),
        )
        .await
        .unwrap();
        let clicks = sent(&calls, "click");
        assert_eq!(clicks.len(), 1, "{clicks:?}");
        assert_eq!(clicks[0]["window_id"], 100, "{clicks:?}");
        desk.run("desk_focus", json!({"window": "Doc"}))
            .await
            .unwrap();
        let fronted = sent(&calls, "bring_to_front");
        assert_eq!(fronted.len(), 1, "{fronted:?}");
        assert_eq!(fronted[0]["window_id"], 300, "{fronted:?}");
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
            (
                &json!("84080"),
                &json!("\u{5fae}\u{4fe1}"),
                &json!(9),
                &json!(false)
            )
        );
        let fore = Desk::new(|| Ok(Apps), Spaces(Arc::clone(&calls)), Os::Mac);
        let listed = fore.run("desk_windows", json!({})).await.unwrap().text;
        assert!(!listed.contains("\"space\""), "{listed}");
    }

    #[tokio::test]
    async fn an_allowlisted_app_whose_windows_are_all_on_another_space_takes_background_input() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Spaces(Arc::clone(&calls)), Os::Mac)
            .background(true)
            .allow(&["WeChat".to_owned()]);
        for (tool, args) in [
            (
                "desk_type",
                json!({"text": "x", "window": "\u{5fae}\u{4fe1}"}),
            ),
            (
                "desk_key",
                json!({"keys": "cmd+a", "window": "\u{5fae}\u{4fe1}"}),
            ),
        ] {
            desk.run(tool, args).await.unwrap();
        }
        assert_eq!(sent(&calls, "type_text").len(), 1);
    }

    #[tokio::test]
    async fn a_background_snapshot_of_a_window_on_another_space_says_so_by_title_or_id() {
        for (background, auto) in [(false, false), (true, true), (true, false)] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(|| Ok(Apps), Spaces(Arc::clone(&calls)), Os::Mac)
                .background(background)
                .auto(auto);
            let background = background && !auto;
            for args in [
                json!({"window": "\u{5fae}\u{4fe1}"}),
                json!({"id": "84080"}),
            ] {
                let err = desk.run("desk_snapshot", args.clone()).await.unwrap_err();
                assert_eq!(
                    err.contains("is on another Space"),
                    background,
                    "{background} {args} {err}"
                );
            }
            let err = desk
                .run(
                    "desk_find",
                    json!({"name": "x", "window": "\u{5fae}\u{4fe1}"}),
                )
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
    async fn background_input_to_a_title_twin_goes_to_the_window_on_a_space_and_says_not_to_send_it_again()
     {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Spaces(Arc::clone(&calls)), Os::Mac).background(true);
        let typed = desk
            .run(
                "desk_type",
                json!({"text": "x", "window": "\u{5fae}\u{4fe1}"}),
            )
            .await
            .unwrap();
        let sent_text = sent(&calls, "type_text");
        assert_eq!(sent_text.len(), 1, "{sent_text:?}");
        assert_eq!(sent_text[0]["target"]["window_id"], 84080, "{sent_text:?}");
        let typed = note(typed);
        let away = Unseen::OffSpace.why();
        assert!(
            typed.ends_with(&format!("; {away}: {UNSEEN_TYPED}")),
            "{typed}"
        );
        let key = desk
            .run(
                "desk_key",
                json!({"keys": "return", "window": "\u{5fae}\u{4fe1}"}),
            )
            .await
            .unwrap();
        let key = note(key);
        assert!(key.ends_with(&format!("; {away}: {UNSEEN_SENT}")), "{key}");
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
            (&json!(300.0), &json!(250.0))
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
    async fn background_typing_after_a_click_the_keyboard_focus_did_not_follow_is_refused() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk =
            Desk::new(|| Ok(Chat("textbox")), Behind(Arc::clone(&calls)), Os::Mac).background(true);
        let click = |y: f64| {
            desk.run(
                "desk_click",
                json!({"x": 300.0, "y": y, "window": "Untitled"}),
            )
        };
        let type_cmgs = || desk.run("desk_type", json!({"text": "cmgs", "window": "Untitled"}));
        let missed: Value = serde_json::from_str(&click(30.0).await.unwrap().text).unwrap();
        assert_eq!(
            missed["focused"],
            json!({"role": "textbox", "name": "Message", "at_click": false})
        );
        let err = type_cmgs().await.unwrap_err();
        assert!(
            err.contains("nothing was typed") && err.contains("textbox \"Message\""),
            "{err}"
        );
        assert!(sent(&calls, "type_text").is_empty());
        let hit: Value = serde_json::from_str(&click(480.0).await.unwrap().text).unwrap();
        assert_eq!(hit["focused"]["at_click"], true, "{hit}");
        let typed: Value = serde_json::from_str(&type_cmgs().await.unwrap().text).unwrap();
        assert_eq!(
            typed["focused"],
            json!({"role": "textbox", "name": "Message"})
        );
        assert_eq!(sent(&calls, "type_text").len(), 1);
        click(30.0).await.unwrap();
        desk.run("desk_key", json!({"keys": "tab", "window": "Untitled"}))
            .await
            .unwrap();
        type_cmgs().await.unwrap();
        assert_eq!(sent(&calls, "type_text").len(), 2);
    }

    #[tokio::test]
    async fn background_typing_after_a_click_into_an_app_with_no_focused_text_field_needs_focus_unverified()
     {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk =
            Desk::new(|| Ok(Chat("window")), Behind(Arc::clone(&calls)), Os::Mac).background(true);
        desk.run(
            "desk_click",
            json!({"x": 300.0, "y": 30.0, "window": "Untitled"}),
        )
        .await
        .unwrap();
        let err = desk
            .run("desk_type", json!({"text": "cmgs", "window": "Untitled"}))
            .await
            .unwrap_err();
        assert_eq!(err, FOCUS_UNREAD);
        assert!(sent(&calls, "type_text").is_empty());
        let typed: Value = serde_json::from_str(
            &desk
                .run(
                    "desk_type",
                    json!({"text": "cmgs", "window": "Untitled", "focus": "unverified"}),
                )
                .await
                .unwrap()
                .text,
        )
        .unwrap();
        assert_eq!(sent(&calls, "type_text").len(), 1);
        assert_eq!(
            typed["focused"],
            json!({"role": "window", "name": "Message"})
        );
        assert!(
            typed["unverified"]
                .as_str()
                .is_some_and(|n| n.ends_with(FOCUS_UNKNOWN)),
            "{typed}"
        );
    }

    #[tokio::test]
    async fn a_lock_screen_refusal_says_once_that_shuttle_cannot_reach_the_password_field() {
        for (os, auto, notes) in [
            (Os::Mac, false, 1),
            (Os::Mac, true, 1),
            (Os::Linux, false, 0),
        ] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(|| Ok(Apps), Locked(Behind(Arc::clone(&calls))), os)
                .background(true)
                .auto(auto);
            let err = desk
                .run(
                    "desk_key",
                    json!({"keys": "return", "window": "Untitled - Notepad"}),
                )
                .await
                .unwrap_err();
            assert!(err.contains(LOCK_HINT), "{os:?} {auto} {err}");
            assert_eq!(
                err.matches(LOCKED_SCREEN).count(),
                notes,
                "{os:?} {auto} {err}"
            );
        }
    }

    #[tokio::test]
    async fn locked_input_typing_aimed_at_loginwindows_pid_sends_nothing() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(
            || Ok(Apps),
            LoginWindow(Behind(Arc::clone(&calls))),
            Os::Mac,
        )
        .background(true)
        .locked_input(true);
        for (tool, args) in [
            ("desk_type", json!({"text": "a", "pid": 776})),
            ("desk_key", json!({"keys": "return", "pid": 776})),
        ] {
            let err = desk.run(tool, args).await.unwrap_err();
            assert_eq!(err, "no focused window", "{tool}");
        }
        assert!(sent(&calls, "type_text").is_empty());
        assert!(sent(&calls, "press_key").is_empty());
        desk.run("desk_type", json!({"text": "a", "pid": 4}))
            .await
            .unwrap();
        assert_eq!(sent(&calls, "type_text").len(), 1);
    }

    #[tokio::test]
    async fn locked_input_asks_the_driver_to_send_background_keys_and_typing_only() {
        for (background, on) in [(true, false), (true, true), (false, true)] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Mac)
                .background(background)
                .locked_input(on);
            desk.run("desk_key", json!({"keys": "return", "pid": 4}))
                .await
                .unwrap();
            desk.run("desk_type", json!({"text": "a", "pid": 4}))
                .await
                .unwrap();
            desk.run("desk_click", json!({"x": 300.0, "y": 250.0, "pid": 4}))
                .await
                .unwrap();
            let asked = background && on;
            for tool in ["press_key", "type_text"] {
                let all = sent(&calls, tool);
                assert!(!all.is_empty(), "{tool}");
                for a in all {
                    assert_eq!(a.get("locked_input").is_some(), asked, "{tool} {a}");
                }
            }
            assert!(
                sent(&calls, "click")
                    .iter()
                    .all(|a| a.get("locked_input").is_none())
            );
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        let old = Desk::new(
            || Ok(Apps),
            NoLockedInput(Behind(Arc::clone(&calls))),
            Os::Mac,
        )
        .background(true)
        .locked_input(true);
        let err = old
            .run("desk_type", json!({"text": "a", "pid": 4}))
            .await
            .unwrap_err();
        assert_eq!(err, format!("type_text: {LOCKED_INPUT_UNSUPPORTED}"));
    }

    #[tokio::test]
    async fn a_repeated_macos_background_key_waits_for_window_changes_after_its_last_press_only() {
        for background in [true, false] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk =
                Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Mac).background(background);
            let args = if background {
                json!({"keys": "backspace", "pid": 4, "repeat": 3})
            } else {
                json!({"keys": "backspace", "repeat": 3})
            };
            desk.run("desk_key", args).await.unwrap();
            let waits: Vec<_> = sent(&calls, "press_key")
                .iter()
                .map(|a| a.get("post_action_wait").cloned())
                .collect();
            let skip = background.then(|| json!("none"));
            assert_eq!(waits, [skip.clone(), skip, None], "{background}");
        }
    }

    #[tokio::test]
    async fn a_repeated_key_sends_no_window_watch_choice_off_macos() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Windows).background(true);
        desk.run(
            "desk_key",
            json!({"keys": "backspace", "pid": 4, "repeat": 3}),
        )
        .await
        .unwrap();
        let presses = sent(&calls, "press_key");
        assert_eq!(presses.len(), 3);
        assert!(presses.iter().all(|a| a.get("post_action_wait").is_none()));
    }

    #[tokio::test]
    async fn a_repeated_foreground_key_stops_at_the_first_press_its_window_is_not_in_front_for() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Windows);
        let err = desk
            .run(
                "desk_key",
                json!({"keys": "backspace", "pid": 8, "repeat": 3}),
            )
            .await
            .unwrap_err();
        assert_eq!(
            err,
            "1 of 3 presses were sent, then the window left the front, so the rest were not sent"
        );
        assert_eq!(sent(&calls, "press_key").len(), 1);
        desk.run(
            "desk_key",
            json!({"keys": "backspace", "pid": 4, "repeat": 3}),
        )
        .await
        .unwrap();
        assert_eq!(sent(&calls, "press_key").len(), 4);
    }

    #[tokio::test]
    async fn input_asking_for_a_delivery_mode_the_desk_does_not_use_is_refused_before_it_is_sent() {
        for background in [true, false] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk =
                Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Mac).background(background);
            let (other, same) = if background {
                ("foreground", "background")
            } else {
                ("background", "foreground")
            };
            let click = |mode: &str| {
                desk.run(
                    "desk_click",
                    json!({"x": 300.0, "y": 250.0, "window": "Untitled", "delivery_mode": mode}),
                )
            };
            let err = click(other).await.unwrap_err();
            assert!(
                err.contains(&format!(
                    "delivery_mode \"{other}\" refused, nothing was sent"
                )),
                "{err}"
            );
            assert!(sent(&calls, "click").is_empty());
            click(same).await.unwrap();
            assert_eq!(sent(&calls, "click").len(), 1);
        }
    }

    #[tokio::test]
    async fn a_linux_launch_raises_the_running_app_and_starts_it_only_when_asked_or_absent() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Linux);
        let raised: Value = serde_json::from_str(
            &desk
                .run("desk_launch", json!({"app": "mail"}))
                .await
                .unwrap()
                .text,
        )
        .unwrap();
        assert_eq!(
            (&raised["running"], &raised["title"], &raised["pid"]),
            (&json!(true), &json!("Inbox"), &json!(8)),
            "{raised}"
        );
        assert_eq!(sent(&calls, "bring_to_front").len(), 1);
        assert!(sent(&calls, "launch_app").is_empty());
        let args = json!({"app": "mail", "args": ["--compose"]});
        desk.run("desk_launch", args).await.unwrap();
        desk.run("desk_launch", json!({"app": "Mousepad"}))
            .await
            .unwrap();
        assert_eq!(sent(&calls, "launch_app").len(), 2);
        assert_eq!(sent(&calls, "bring_to_front").len(), 1);
    }

    #[tokio::test]
    async fn a_macos_launch_never_raises_a_running_app_and_a_windows_one_does() {
        for (os, raised) in [(Os::Mac, false), (Os::Windows, true)] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), os);
            desk.run("desk_launch", json!({"app": "Mail"}))
                .await
                .unwrap();
            assert_eq!(sent(&calls, "launch_app").is_empty(), raised, "{os:?}");
            assert_eq!(
                sent(&calls, "bring_to_front").len(),
                usize::from(raised),
                "{os:?}"
            );
        }
    }

    #[test]
    fn a_window_names_an_app_by_its_process_or_the_desktop_id_of_its_menu_name() {
        assert!(names_app("xfce4-terminal", "Terminal"));
        assert!(names_app("xfce4-terminal", "xfce4-terminal"));
        assert!(names_app("xfce4-terminal", "xfce4-terminal-emulator"));
        assert!(!names_app("xfce4-terminal", "Mousepad"));
        assert!(!names_app("", "Terminal"));
    }

    #[test]
    fn a_name_matches_the_start_or_end_of_app_words_never_a_short_fragment() {
        for (app, name) in [
            ("Notepad.exe", "Notepad"),
            ("Notepad.exe", "Note"),
            ("Google Chrome", "Chrome"),
            ("chrome", "Google Chrome"),
            ("gnome-terminal-server", "gnome-terminal"),
            ("TextEdit", "textedit"),
            ("TextEdit", "Text"),
            ("xfce4-terminal", "term"),
            ("xfce4-terminal", "Xfce Terminal"),
            ("mousepad", "mouse"),
            ("mspaint.exe", "Paint"),
            ("msedge.exe", "Edge"),
            ("notepad++.exe", "Notepad++"),
        ] {
            assert!(names_app(app, name), "{app} {name}");
        }
        for (app, name) in [
            ("mousepad", "pad"),
            ("xfce4-terminal", "rmin"),
            ("xfce4-terminal", "e"),
            ("xfce4-terminal", "xf"),
            ("xfce4-terminal", ""),
            ("xfce4-terminal", "  "),
            ("Notepad.exe", "No"),
            ("notepad++.exe", "notepad"),
            ("Notepad.exe", "notepad++"),
        ] {
            assert!(!names_app(app, name), "{app} {name:?}");
        }
    }

    #[tokio::test]
    async fn an_allowlist_refuses_launch_flags_and_keeps_plain_arguments() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Linux)
            .allow(&["Google Chrome".into()]);
        let err = desk
            .run(
                "desk_launch",
                json!({"app": "Google Chrome", "args": ["--gpu-launcher=/bin/sh"]}),
            )
            .await
            .unwrap_err();
        assert_eq!(err, LAUNCH_FLAGS);
        assert!(sent(&calls, "launch_app").is_empty());
        desk.run(
            "desk_launch",
            json!({"app": "Google Chrome", "args": ["https://example.com/"]}),
        )
        .await
        .unwrap();
        assert_eq!(sent(&calls, "launch_app").len(), 1);
        let windows = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Windows)
            .allow(&["Google Chrome".into()]);
        let err = windows
            .run(
                "desk_launch",
                json!({"app": "Google Chrome", "args": ["/gpu-launcher=cmd"]}),
            )
            .await
            .unwrap_err();
        assert_eq!(err, LAUNCH_FLAGS);
    }

    #[tokio::test]
    async fn a_launch_with_arguments_starts_a_new_instance_on_macos_with_them() {
        for os in [Os::Mac, Os::Linux, Os::Windows] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), os);
            let args = json!(["--profile-directory=Profile 1", "https://example.com/"]);
            desk.run("desk_launch", json!({"app": "Google Chrome", "args": args}))
                .await
                .unwrap();
            desk.run("desk_launch", json!({"app": "Google Chrome"}))
                .await
                .unwrap();
            let sent = sent(&calls, "launch_app");
            assert_eq!(sent[0]["additional_arguments"], args, "{os:?} {sent:?}");
            assert_eq!(
                sent[0].get("creates_new_application_instance").is_some(),
                os == Os::Mac,
                "{os:?} {sent:?}"
            );
            assert!(sent[1].get("additional_arguments").is_none(), "{sent:?}");
            assert!(
                sent[1].get("creates_new_application_instance").is_none(),
                "{sent:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_launch_with_only_urls_hands_them_to_the_running_app_on_macos() {
        for os in [Os::Mac, Os::Linux, Os::Windows] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), os);
            let urls = json!(["https://example.com/", "/tmp/page.html"]);
            desk.run("desk_launch", json!({"app": "Safari", "args": urls}))
                .await
                .unwrap();
            let sent = sent(&calls, "launch_app");
            let (opened, argv) = if os == Os::Mac {
                (&urls, &Value::Null)
            } else {
                (&Value::Null, &urls)
            };
            assert_eq!(&sent[0]["urls"], opened, "{os:?} {sent:?}");
            assert_eq!(&sent[0]["additional_arguments"], argv, "{os:?} {sent:?}");
            assert!(
                sent[0].get("creates_new_application_instance").is_none(),
                "{os:?} {sent:?}"
            );
        }
    }

    #[tokio::test]
    async fn input_names_a_window_by_its_whole_accessibility_title_where_the_driver_lists_it_elided()
     {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Long), Elided(Arc::clone(&calls)), Os::Mac).background(true);
        desk.run("desk_key", json!({"keys": "cmd+t", "window": LONG_TITLE}))
            .await
            .unwrap();
        let sent = sent(&calls, "hotkey");
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0]["target"]["window_id"], 41, "{sent:?}");
        let err = desk
            .run("desk_key", json!({"keys": "cmd+t", "window": "Nope"}))
            .await
            .unwrap_err();
        assert!(err.contains(LONG_TITLE), "{err}");
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
    async fn macos_sends_the_plus_key_as_shift_and_equals_only_on_the_hid_path() {
        for (background, want) in [
            (false, json!(["cmd", "shift", "="])),
            (true, json!(["cmd", "+"])),
        ] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk =
                Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Mac).background(background);
            desk.run("desk_key", json!({"keys": "cmd++", "pid": 4}))
                .await
                .unwrap();
            let sent = sent(&calls, "hotkey");
            assert_eq!(sent.len(), 1, "background {background}: {sent:?}");
            assert_eq!(sent[0]["keys"], want, "background {background}");
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Mac);
        desk.run("desk_key", json!({"keys": "enter"}))
            .await
            .unwrap();
        assert_eq!(
            sent(&calls, "press_key"),
            [json!({"key": "return", "scope": "desktop"})]
        );
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
    async fn macos_typing_sets_the_driver_delay_for_foreground_basic_plane_text_and_background_chromium_apps()
     {
        for (os, background, pid, text, delay) in [
            (Os::Mac, false, 4, "ab\u{4e2d}\ncd", Some(MAC_TYPE_DELAY_MS)),
            (Os::Mac, true, 4, "ab\u{4e2d}\ncd", None),
            (Os::Mac, false, 4, "a\u{1f600}", None),
            (
                Os::Mac,
                true,
                8,
                "ab\u{4e2d}\ncd",
                Some(CHROMIUM_TYPE_DELAY_MS),
            ),
            (Os::Mac, true, 8, "a\u{1f600}", Some(CHROMIUM_TYPE_DELAY_MS)),
            (Os::Mac, false, 8, "ab\u{4e2d}\ncd", Some(MAC_TYPE_DELAY_MS)),
            (Os::Mac, false, 8, "a\u{1f600}", None),
            (Os::Windows, true, 8, "ab\u{4e2d}\ncd", None),
            (Os::Windows, false, 4, "ab\u{4e2d}\ncd", None),
        ] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk =
                Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), os).background(background);
            desk.run("desk_type", json!({"text": text, "pid": pid}))
                .await
                .unwrap();
            let typed = sent(&calls, "type_text");
            assert!(!typed.is_empty(), "{os:?} {pid}");
            for req in typed {
                assert_eq!(req["delay_ms"].as_u64(), delay, "{os:?} {pid} {req}");
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
        assert_eq!(sent(&calls, "list_windows").len(), 1);
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

    #[tokio::test]
    async fn on_linux_a_background_element_click_allows_the_window_manager_s_decorations() {
        fn decorated() -> Result<Fake, String> {
            let mut f = notepad();
            f.add("w", None, "window", &["showing"], Some([-5, -29, 810, 634]));
            Ok(f)
        }
        fn too_wide() -> Result<Fake, String> {
            let mut f = notepad();
            f.add(
                "w",
                None,
                "window",
                &["showing"],
                Some([-30, -29, 860, 634]),
            );
            Ok(f)
        }
        let cases = [
            (Os::Linux, decorated as fn() -> _, 900.0, true),
            (Os::Mac, decorated, 900.0, false),
            (Os::Linux, decorated, 0.0, false),
            (Os::Linux, too_wide, 900.0, false),
        ];
        for (os, fake, twin, clicked) in cases {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let twins = Twins(Arc::clone(&calls), twin, "Untitled - Notepad", 0.0);
            let desk = Desk::new(fake, twins, os).background(true);
            let done = desk
                .run("desk_act", json!({"id": "ok", "action": "click"}))
                .await;
            let got: Vec<Value> = sent(&calls, "click")
                .iter()
                .map(|c| c["window_id"].clone())
                .collect();
            assert_eq!(done.is_ok(), clicked, "{os:?} {twin} {done:?}");
            assert_eq!(
                got,
                if clicked { vec![json!(1)] } else { vec![] },
                "{os:?} {twin}"
            );
        }
    }

    #[tokio::test]
    async fn on_linux_a_background_type_waits_for_a_window_shot_s_read() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(
            || Ok(notepad()),
            SlowRead(Behind(Arc::clone(&calls))),
            Os::Linux,
        )
        .background(true);
        let (_, typed) = tokio::join!(
            desk.run("desk_shot", json!({"window": "Untitled - Notepad"})),
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                desk.run("desk_type", json!({"text": "x", "pid": 4})).await
            }
        );
        typed.unwrap();
        let tools: Vec<String> = calls
            .lock()
            .unwrap()
            .iter()
            .map(|(t, _)| t.clone())
            .collect();
        let mut open = 0;
        for tool in &tools {
            match tool.as_str() {
                "get_window_state" => open += 1,
                "read_done" => open -= 1,
                "type_text" => assert_eq!(open, 0, "{tools:?}"),
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn on_linux_a_window_input_drops_the_read_its_driver_session_held() {
        for (os, reads) in [(Os::Linux, 3), (Os::Mac, 1)] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(|| Ok(notepad()), Behind(Arc::clone(&calls)), os).background(true);
            for _ in 0..2 {
                desk.run("desk_click", json!({"x": 300.0, "y": 250.0, "pid": 4}))
                    .await
                    .unwrap();
            }
            desk.run(
                "desk_drag",
                json!({"x": 200.0, "y": 100.0, "to_x": 300.0, "to_y": 100.0, "pid": 4}),
            )
            .await
            .unwrap();
            assert_eq!(
                sent(&calls, "get_window_state").len(),
                reads,
                "{os:?} {:?}",
                order(&calls)
            );
        }
    }

    #[tokio::test]
    async fn a_background_element_click_picks_its_window_by_frame_over_a_title_twin_on_another_space()
     {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let twins = SpaceTwins(Twins(Arc::clone(&calls), 900.0, "Untitled - Notepad", 0.0));
        let desk = Desk::new(|| Ok(notepad()), twins, Os::Mac).background(true);
        desk.run("desk_act", json!({"id": "ok", "action": "click"}))
            .await
            .unwrap();
        let clicks = sent(&calls, "click");
        assert_eq!(
            (clicks.len(), &clicks[0]["window_id"]),
            (1, &json!(1)),
            "{clicks:?}"
        );
        let err = desk
            .run(
                "desk_type",
                json!({"text": "x", "window": "Untitled - Notepad"}),
            )
            .await
            .unwrap_err();
        assert!(err.contains("cannot tell them apart"), "{err}");
    }

    #[tokio::test]
    async fn a_background_element_click_into_its_window_on_another_space_says_so() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let twins = AwayTwin(Twins(Arc::clone(&calls), 900.0, "Untitled - Notepad", 0.0));
        let desk = Desk::new(|| Ok(notepad()), twins, Os::Mac).background(true);
        let out = desk
            .run("desk_act", json!({"id": "ok", "action": "click"}))
            .await
            .unwrap();
        assert_eq!(sent(&calls, "click")[0]["window_id"], 1);
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(
            v["unverified"],
            format!("unverifiable; {}: {UNSEEN_SENT}", Unseen::OffSpace.why())
        );
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
    async fn a_background_click_in_a_named_window_does_not_raise_it() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Windows).background(true);
        let _ = desk
            .run(
                "desk_click",
                json!({"x": 300.0, "y": 250.0, "window": "Inbox", "pid": 8}),
            )
            .await;
        assert!(
            sent(&calls, "bring_to_front").is_empty(),
            "{:?}",
            order(&calls)
        );
    }

    #[tokio::test]
    async fn a_foreground_click_where_no_window_is_does_not_raise_the_named_one() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(
            || Ok(Apps),
            BehindCover(
                Behind(Arc::clone(&calls)),
                AtomicBool::new(false),
                (4, "Untitled - Notepad"),
            ),
            Os::Windows,
        );
        let _ = desk
            .run(
                "desk_click",
                json!({"x": 600.0, "y": 250.0, "window": "Inbox", "pid": 8}),
            )
            .await;
        assert!(
            sent(&calls, "bring_to_front").is_empty(),
            "{:?}",
            order(&calls)
        );
    }

    #[tokio::test]
    async fn a_same_app_window_on_top_is_raised_past_unless_the_desk_does_not_list_it() {
        for (cover, raised) in [
            ((8, "Other - Mail"), true),
            ((8, ""), false),
            ((8, "PopupHost"), false),
        ] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(
                || Ok(Sibling),
                BehindCover(Behind(Arc::clone(&calls)), AtomicBool::new(false), cover),
                Os::Windows,
            );
            let _ = desk
                .run(
                    "desk_scroll",
                    json!({"direction": "down", "x": 300.0, "y": 250.0, "window": "Inbox", "pid": 8}),
                )
                .await;
            assert_eq!(
                !sent(&calls, "bring_to_front").is_empty(),
                raised,
                "{cover:?}: {:?}",
                order(&calls)
            );
        }
    }

    #[tokio::test]
    async fn a_point_outside_the_named_window_does_not_raise_it() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(
            || Ok(Apps),
            BehindCover(
                Behind(Arc::clone(&calls)),
                AtomicBool::new(false),
                (4, "Program Manager"),
            ),
            Os::Windows,
        );
        let _ = desk
            .run(
                "desk_click",
                json!({"x": 300.0, "y": 550.0, "window": "Inbox", "pid": 8}),
            )
            .await;
        assert!(
            sent(&calls, "bring_to_front").is_empty(),
            "{:?}",
            order(&calls)
        );
    }

    #[test]
    fn a_listed_window_is_found_by_its_number_where_frames_carry_one_else_by_title() {
        let numbered = Frame {
            title: "A very long document title that the window server shortens".into(),
            pid: Some(8),
            number: Some(41),
            ..Frame::default()
        };
        let elided = json!({"pid": 8, "window_id": 41, "title": "A very long document…"});
        assert!(lists_top_level(std::slice::from_ref(&numbered), &elided));
        let other = json!({"pid": 8, "window_id": 42, "title": numbered.title});
        assert!(!lists_top_level(std::slice::from_ref(&numbered), &other));
        let titled = Frame {
            title: "Other - Mail".into(),
            pid: Some(8),
            ..Frame::default()
        };
        let sibling = json!({"pid": 8, "window_id": 7, "title": "Other - Mail"});
        assert!(lists_top_level(std::slice::from_ref(&titled), &sibling));
        let owned = Frame {
            owner: Some("8/1".into()),
            ..titled
        };
        assert!(!lists_top_level(&[owned], &sibling));
    }

    #[tokio::test]
    async fn foreground_input_raises_the_named_window_and_goes_to_the_desktop_at_its_pixels() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Mac);
        desk.run("desk_key", json!({"keys": "ctrl+s", "window": "Inbox"}))
            .await
            .unwrap();
        assert_eq!(order(&calls), ["bring_to_front -", "hotkey desktop"]);
        assert_eq!(
            sent(&calls, "bring_to_front"),
            [json!({"pid": 8, "window_id": 2})]
        );
        assert_eq!(
            sent(&calls, "hotkey"),
            [json!({"keys": ["ctrl", "s"], "scope": "desktop"})]
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(
            || Ok(Apps),
            BehindCover(
                Behind(Arc::clone(&calls)),
                AtomicBool::new(false),
                (4, "Untitled - Notepad"),
            ),
            Os::Windows,
        );
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
            .filter(|(t, _)| {
                !["get_screen_size", "list_windows", "get_window_state"].contains(&t.as_str())
            })
            .cloned()
            .collect();
        assert_eq!(
            input,
            [
                (
                    "bring_to_front".to_owned(),
                    json!({"pid": 8, "window_id": 2})
                ),
                (
                    "type_text".to_owned(),
                    json!({"literal": true, "text": "a", "scope": "desktop"})
                ),
                (
                    "bring_to_front".to_owned(),
                    json!({"pid": 8, "window_id": 2})
                ),
                (
                    "click".to_owned(),
                    json!({"x": 800.0, "y": 250.0, "scope": "desktop"})
                ),
                (
                    "bring_to_front".to_owned(),
                    json!({"pid": 8, "window_id": 2})
                ),
                (
                    "scroll".to_owned(),
                    json!({"direction": "down", "amount": 3, "x": 800.0, "y": 250.0, "scope": "desktop"})
                ),
                (
                    "bring_to_front".to_owned(),
                    json!({"pid": 8, "window_id": 2})
                ),
                (
                    "drag".to_owned(),
                    json!({"from_x": 700.0, "from_y": 100.0, "to_x": 900.0, "to_y": 150.0, "scope": "desktop"})
                ),
            ]
        );
    }

    #[tokio::test]
    async fn a_foreground_click_on_a_covered_point_goes_to_the_named_window_on_macos() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Overlaid(Arc::clone(&calls)), Os::Mac);
        desk.run(
            "desk_click",
            json!({"x": 50.0, "y": 60.0, "window": "Untitled"}),
        )
        .await
        .unwrap();
        assert_eq!(
            sent(&calls, "bring_to_front"),
            [json!({"pid": 4, "window_id": 1})]
        );
        let clicks = sent(&calls, "click");
        assert_eq!(clicks.len(), 1, "{clicks:?}");
        assert_eq!(
            (&clicks[0]["pid"], &clicks[0]["window_id"]),
            (&json!(4), &json!(1)),
            "{clicks:?}"
        );
    }

    #[tokio::test]
    async fn foreground_input_to_a_window_that_cannot_come_to_the_front_sends_nothing_on_macos() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Unfronted(Behind(Arc::clone(&calls))), Os::Mac);
        let err = desk
            .run("desk_type", json!({"text": "hi\nthere", "window": "Inbox"}))
            .await
            .unwrap_err();
        assert!(err.contains("is not frontmost"), "{err}");
        let err = desk
            .run("desk_key", json!({"keys": "ctrl+s", "window": "Inbox"}))
            .await
            .unwrap_err();
        assert!(err.contains("is not frontmost"), "{err}");
        assert_eq!(order(&calls), ["bring_to_front -", "bring_to_front -"]);
        assert!(sent(&calls, "type_text").is_empty());
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
    async fn snapshot_and_find_read_link_urls_only_when_asked_and_read_always_does() {
        let desk = Desk::new(|| Ok(notepad()), Slow(Arc::default()), Os::Mac);
        desk.run("desk_snapshot", json!({})).await.unwrap();
        desk.run("desk_snapshot", json!({"urls": true}))
            .await
            .unwrap();
        desk.run("desk_snapshot", json!({"urls": true, "text": true}))
            .await
            .unwrap();
        desk.run("desk_find", json!({"name": "Save"}))
            .await
            .unwrap();
        desk.run("desk_find", json!({"name": "Save", "urls": true}))
            .await
            .unwrap();
        desk.run("desk_read", json!({"id": "ok"})).await.unwrap();
        let urls = desk
            .access
            .run(CALL_LIMIT, (), |b| Ok(b.urls.clone()))
            .await
            .unwrap();
        assert_eq!(urls, [false, true, false, false, true, true]);
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
        assert!(
            listed.text.contains(r#""title":"Draft""#) && listed.text.contains(r#""stale":true"#),
            "{}",
            listed.text
        );
        for (tool, args) in [
            ("desk_close", json!({"window": "Draft"})),
            ("desk_snapshot", json!({"window": "Draft"})),
            ("desk_find", json!({"name": "Save", "window": "Draft"})),
        ] {
            let err = desk.run(tool, args).await.unwrap_err();
            assert!(err.contains("busy or hung"), "{tool}: {err}");
        }
        assert!(closed(&desk).await.is_empty());
    }

    #[tokio::test]
    async fn at_names_the_process_of_the_element_under_the_point() {
        let desk = Desk::new(|| Ok(notepad()), Desktop(Arc::default()), Os::Windows);
        let out = desk
            .run("desk_at", json!({"x": 140, "y": 110}))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(
            (v["found"]["id"].as_str(), v["pid"].as_u64()),
            (Some("ok"), Some(4)),
            "{v}"
        );
        assert!(v.get("covered").is_none(), "{v}");
    }

    #[tokio::test]
    async fn at_without_a_window_reads_the_top_window_at_the_point_not_the_front_one() {
        let open = Desk::new(|| Ok(Apps), Desktop(Arc::default()), Os::Mac);
        assert_eq!(at_window(&open, 600, 100).await, Ok(json!("Inbox")));
        assert_eq!(
            at_window(&open, 300, 700).await,
            Ok(json!("Untitled - Notepad"))
        );
        assert_eq!(
            at_window(&open, 2000, 2000).await,
            Err(NO_WINDOW_AT.to_owned())
        );
        let notepad =
            Desk::new(|| Ok(Apps), Desktop(Arc::default()), Os::Mac).allow(&["notepad".into()]);
        let err = at_window(&notepad, 600, 100).await.unwrap_err();
        assert!(err.starts_with("Mail is not in the app allowlist"), "{err}");
    }

    #[tokio::test]
    async fn at_without_a_window_reads_the_twin_whose_bounds_hold_the_point() {
        let desk = Desk::new(|| Ok(TwinFrames), Pair, Os::Linux);
        let out = desk
            .run("desk_at", json!({"x": 600, "y": 100}))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(
            (&v["found"]["id"], &v["pid"]),
            (&json!("5/b1"), &json!(5)),
            "{v}"
        );
        let err = desk
            .run("desk_at", json!({"x": 450, "y": 100}))
            .await
            .unwrap_err();
        assert!(err.starts_with("2 windows match the title"), "{err}");
    }

    #[tokio::test]
    async fn at_names_the_window_that_covers_the_point() {
        let covered = || {
            let mut f = notepad();
            f.covers = Some(("tmp - Thunar".to_owned(), Some(9)));
            Ok(f)
        };
        let desk = Desk::new(covered, Desktop(Arc::default()), Os::Linux);
        let out = desk
            .run("desk_at", json!({"x": 140, "y": 110}))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(
            v["covered"],
            json!({"title": "tmp - Thunar", "pid": 9}),
            "{v}"
        );
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
    async fn a_masked_shot_never_shows_a_window_that_left_while_it_was_taken() {
        let desk = Desk::new(|| Ok(Apps), Shifting(Default::default()), Os::Windows)
            .allow(&["notepad".into()]);
        let shot = pixels(&desk.run("desk_shot", json!({})).await.unwrap());
        assert!(shot.iter().all(|p| *p != [255; 3]), "{shot:?}");
    }

    #[tokio::test]
    async fn at_a_point_reads_the_window_the_driver_lists_there_by_its_number_not_its_elided_title()
    {
        let desk = Desk::new(|| Ok(Long), Elided(Arc::default()), Os::Mac);
        let out = desk
            .run("desk_at", json!({"x": 10, "y": 10}))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(v["window"]["title"], LONG_TITLE, "{v}");
    }

    #[tokio::test]
    async fn foreground_keys_naming_an_app_bring_its_window_to_the_front_first() {
        for os in [Os::Windows, Os::Linux] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(|| Ok(notepad()), Behind(Arc::clone(&calls)), os);
            desk.run("desk_key", json!({"keys": "a", "app": "Notepad"}))
                .await
                .unwrap();
            let calls = calls.lock().unwrap();
            let at = |tool: &str| calls.iter().position(|(t, _)| t == tool);
            let raised = at("bring_to_front");
            assert!(
                raised.is_some() && raised < at("press_key"),
                "{os:?}: {calls:?}"
            );
            assert_eq!(
                calls[raised.unwrap()].1,
                json!({"pid": 4, "window_id": 1}),
                "{os:?}"
            );
        }
    }

    #[tokio::test]
    async fn background_input_naming_only_the_pid_of_an_app_behind_reaches_its_window() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk =
            Desk::new(|| Ok(notepad()), Behind(Arc::clone(&calls)), Os::Windows).background(true);
        desk.run("desk_key", json!({"keys": "a", "pid": 8}))
            .await
            .unwrap();
        let calls = calls.lock().unwrap();
        let (_, pressed) = calls.iter().find(|(t, _)| t == "press_key").unwrap();
        assert_eq!(
            pressed["target"],
            json!({"kind": "window", "pid": 8, "window_id": 2}),
            "{calls:?}"
        );
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
    async fn windows_foreground_typing_sends_each_line_as_one_text() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Windows);
        desk.run("desk_type", json!({"text": "Ab!\nC é"}))
            .await
            .unwrap();
        assert_eq!(
            order(&calls),
            [
                "type_text desktop",
                "press_key desktop",
                "type_text desktop"
            ]
        );
        assert_eq!(
            sent(&calls, "type_text"),
            [
                json!({"text": "Ab!", "literal": true, "scope": "desktop"}),
                json!({"text": "C é", "literal": true, "scope": "desktop"})
            ]
        );
    }

    fn closing(name: &str) -> String {
        format!("{}/{name}>", '<')
    }

    #[tokio::test]
    async fn typed_lines_keep_a_closing_tag_the_text_opens_and_drop_one_it_does_not() {
        let svg = format!("<text x=\"1\">\nhello\n{}", closing("text"));
        let dangling = format!("hello\n{}", closing("text"));
        let mixed = format!("café\n{}", closing("invoke"));
        for (os, text, lines) in [
            (
                Os::Windows,
                svg.clone(),
                vec![
                    "<text x=\"1\">".to_owned(),
                    "hello".to_owned(),
                    closing("text"),
                ],
            ),
            (Os::Mac, dangling, vec!["hello".to_owned()]),
            (Os::Linux, mixed, vec!["café".to_owned()]),
        ] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), os);
            desk.run("desk_type", json!({"text": text})).await.unwrap();
            let typed = sent(&calls, "type_text");
            assert!(
                typed.iter().all(|r| r["literal"] == true),
                "{os:?} {typed:?}"
            );
            let got: Vec<String> = typed
                .iter()
                .map(|r| r["text"].as_str().unwrap().to_owned())
                .collect();
            assert_eq!(got, lines, "{os:?}");
        }
    }

    #[test]
    fn a_trailing_protocol_tail_goes_only_when_the_text_does_not_open_it() {
        let c = closing;
        let cases = [
            (
                format!("End of document.\n{}\n{}", c("text"), c("invoke")),
                "End of document.".to_owned(),
            ),
            (
                format!("Done.\n{}\n{}", c("antml:parameter"), c("antml:invoke")),
                "Done.".to_owned(),
            ),
            (
                format!("<text>foo{}", c("text")),
                format!("<text>foo{}", c("text")),
            ),
            (
                format!("<parameter name=\"foo\">body{}", c("parameter")),
                format!("<parameter name=\"foo\">body{}", c("parameter")),
            ),
            (
                format!("Hello world {}", c("div")),
                format!("Hello world {}", c("div")),
            ),
            (
                "Lorem ipsum dolor sit amet.".to_owned(),
                "Lorem ipsum dolor sit amet.".to_owned(),
            ),
            (
                format!("Hello <b>world{}\n{}", c("b"), c("text")),
                format!("Hello <b>world{}", c("b")),
            ),
            (String::new(), String::new()),
            (
                format!("<text>\nhello\n{}", c("text")),
                format!("<text>\nhello\n{}", c("text")),
            ),
            (format!("hello\n{}", c("text")), "hello".to_owned()),
        ];
        for (text, kept) in cases {
            assert_eq!(without_protocol_tail(&text), kept, "{text:?}");
        }
    }

    #[tokio::test]
    async fn windows_foreground_typing_into_a_named_window_raises_it_first() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Behind(Arc::clone(&calls)), Os::Windows);
        desk.run("desk_type", json!({"text": "a", "window": "Inbox"}))
            .await
            .unwrap();
        assert_eq!(order(&calls), ["bring_to_front -", "type_text desktop"]);
        assert_eq!(
            sent(&calls, "bring_to_front"),
            [json!({"pid": 8, "window_id": 2})]
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(
            || Ok(Apps),
            NoFront(Behind(Arc::clone(&calls))),
            Os::Windows,
        );
        let err = desk
            .run("desk_type", json!({"text": "a", "window": "Inbox"}))
            .await
            .unwrap_err();
        assert!(
            err.ends_with("; the front window is \"Untitled - Notepad\""),
            "{err}"
        );
        assert!(sent(&calls, "type_text").is_empty());
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
                    json!({"literal": true, "text": "a", "scope": "desktop", "delay_ms": MAC_TYPE_DELAY_MS})
                ),
                (
                    "press_key".to_owned(),
                    json!({"key": "return", "scope": "desktop"})
                ),
                (
                    "type_text".to_owned(),
                    json!({"literal": true, "text": "b", "scope": "desktop", "delay_ms": MAC_TYPE_DELAY_MS})
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
        desk.run("desk_type", json!({"text": "\u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{4e94}\u{516d}\u{4e03}\u{516b}\u{4e5d}\u{5341}\nok"}))
            .await
            .unwrap();
        let got = calls.lock().unwrap().clone();
        assert_eq!(
            got,
            [
                (
                    "hotkey".to_owned(),
                    json!({"keys": ["super", "ctrl", "+"], "scope": "desktop"})
                ),
                (
                    "type_text".to_owned(),
                    json!({"literal": true, "text": "\u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{4e94}\u{516d}\u{4e03}\u{516b}", "scope": "desktop"})
                ),
                (
                    "type_text".to_owned(),
                    json!({"literal": true, "text": "\u{4e5d}\u{5341}", "scope": "desktop"})
                ),
                (
                    "press_key".to_owned(),
                    json!({"key": "return", "scope": "desktop"})
                ),
                (
                    "type_text".to_owned(),
                    json!({"literal": true, "text": "ok", "scope": "desktop"})
                ),
            ]
        );
    }

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
            text_runs(
                "MCP — \u{4f60}\u{597d} café 🙂! \u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{4e94}\u{516d}\u{4e03}\u{516b}\u{4e5d}\u{5341}"
            ),
            [
                "MCP — \u{4f60}\u{597d} café 🙂! \u{4e00}\u{4e8c}\u{4e09}",
                "\u{56db}\u{4e94}\u{516d}\u{4e03}\u{516b}\u{4e5d}\u{5341}"
            ]
        );
        assert_eq!(
            text_runs(
                "\u{4f60}\u{4f60}\u{4f60}\u{4f60}\u{4f60}\u{4f60}\u{4f60}\u{4f60}\u{4f60}\u{4f60}\u{597d}"
            ),
            [
                "\u{4f60}\u{4f60}\u{4f60}\u{4f60}\u{4f60}\u{4f60}\u{4f60}\u{4f60}\u{4f60}\u{4f60}\u{597d}"
            ]
        );
        assert_eq!(text_runs("plain"), ["plain"]);
        assert_eq!(typed_tail("x\r\nhello").as_deref(), Some("hello"));
        assert_eq!(typed_tail("line\n"), None);
        assert_eq!(
            typed_tail("abcdefghijklmnopqrstuvwxyz").as_deref(),
            Some("klmnopqrstuvwxyz")
        );
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
    async fn an_element_click_holds_the_input_turn_until_the_driver_clicks() {
        for call in [json!({"id": "ok"}), json!({"id": "ok", "action": "click"})] {
            let (turn, held) = (Arc::new(OnceLock::new()), Arc::new(Mutex::new(Vec::new())));
            let driver = HeldWhileClicking(Arc::clone(&turn), Arc::clone(&held));
            let desk = Desk::new(|| Ok(notepad()), driver, Os::Windows);
            turn.set(desk.turn()).unwrap();
            let tool = if call["action"].is_null() {
                "desk_press"
            } else {
                "desk_act"
            };
            desk.run(tool, call).await.unwrap();
            assert_eq!(*held.lock().unwrap(), [true], "{tool}");
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
    async fn a_macos_press_resolves_an_id_of_the_snapshot_that_crossed_the_id_cap() {
        let desk = Desk::new(|| Ok(Capped::full()), Recorder(Arc::default()), Os::Mac);
        let snap = desk.run("desk_snapshot", json!({})).await.unwrap();
        let snap: Value = serde_json::from_str(&snap.text).unwrap();
        assert_eq!(snap["nodes"][0]["id"], "ok", "{snap}");
        let out = desk.run("desk_press", json!({"id": "ok"})).await.unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out.text).unwrap(),
            json!({"ok": true, "how": "action"})
        );
        desk.run("desk_snapshot", json!({})).await.unwrap();
        let held = desk
            .access
            .run(CALL_LIMIT, (), |b| Ok(b.ids.len()))
            .await
            .unwrap();
        assert_eq!(held, 2);
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
    async fn a_linux_shot_from_the_driver_answers_the_image_and_writes_no_file() {
        let desk = Desk::new(|| Ok(Apps), Desktop(Arc::default()), Os::Linux);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        let shot = desk
            .run("desk_shot", json!({"path": path.to_string_lossy()}))
            .await
            .unwrap();
        let text: Value = serde_json::from_str(&shot.text).unwrap();
        assert_eq!(text, json!({"ok": true, "width": 10, "height": 10}));
        assert!(shot.image_b64.is_some());
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_drag_in_a_window_reads_the_screen_scale_once() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(notepad()), Sized(calls.clone()), Os::Mac);
        let drag = json!({"window": "Untitled - Notepad", "x": 10.0, "y": 10.0, "to_x": 50.0, "to_y": 20.0});
        desk.run("desk_drag", drag).await.unwrap();
        let calls = calls.lock().unwrap();
        let sizes = calls.iter().filter(|(t, _)| t == "get_screen_size").count();
        assert_eq!(sizes, 1, "{calls:?}");
        assert!(calls.iter().any(|(t, _)| t == "drag"), "{calls:?}");
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
        let window = |pid: u32, id: u32| json!({"session": WINDOW_SESSION, "pid": pid, "window_id": id, "include_accessibility_tree": false});
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
                    json!({"x": 20.0, "y": 80.0, "session": WINDOW_SESSION, "pid": 8, "window_id": 41, "delivery_mode": "foreground"})
                ),
                (
                    "bring_to_front".to_owned(),
                    json!({"pid": 7, "window_id": 40})
                ),
                ("get_window_state".to_owned(), window(7, 40)),
                (
                    "click".to_owned(),
                    json!({"x": 45.0, "y": 150.0, "count": 2, "session": WINDOW_SESSION, "pid": 7, "window_id": 40, "delivery_mode": "foreground"})
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
    async fn a_macos_foreground_click_under_another_apps_window_goes_in_the_background_and_says_so()
    {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fake = || {
            let mut f = notepad();
            f.top = Some(9);
            Ok(f)
        };
        let desk = Desk::new(fake, Screen(calls.clone()), Os::Mac);
        let out = desk
            .run("desk_click", json!({"x": 300.0, "y": 200.0}))
            .await
            .unwrap();
        let text: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(
            text["unverified"].as_str(),
            Some(format!("Panel {COVERED}").as_str()),
            "{text}"
        );
        let click = sent(&calls, "click");
        assert_eq!(click[0]["delivery_mode"], "background", "{click:?}");
        assert_eq!(sent(&calls, "bring_to_front").len(), 1);
    }

    #[tokio::test]
    async fn a_macos_foreground_click_where_the_hit_test_fails_goes_in_the_background_and_says_so()
    {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fake = || {
            let mut f = notepad();
            f.blind = true;
            Ok(f)
        };
        let desk = Desk::new(fake, Screen(calls.clone()), Os::Mac);
        let out = desk
            .run("desk_click", json!({"x": 300.0, "y": 200.0}))
            .await
            .unwrap();
        let text: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(text["unverified"].as_str(), Some(COVER_UNKNOWN), "{text}");
        let click = sent(&calls, "click");
        assert_eq!(click[0]["delivery_mode"], "background", "{click:?}");
    }

    #[tokio::test]
    async fn a_macos_foreground_click_where_the_hit_test_fails_follows_the_window_list() {
        fn listing(top: u32) -> Result<Fake, String> {
            let mut f = notepad();
            f.blind = true;
            f.listed = Some(top);
            Ok(f)
        }
        async fn click(fake: fn() -> Result<Fake, String>) -> (Value, Value) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let desk = Desk::new(fake, Screen(calls.clone()), Os::Mac);
            let out = desk
                .run("desk_click", json!({"x": 300.0, "y": 200.0}))
                .await
                .unwrap();
            let text: Value = serde_json::from_str(&out.text).unwrap();
            (text, sent(&calls, "click")[0].clone())
        }
        let (text, sent_click) = click(|| listing(8)).await;
        assert!(text.get("unverified").is_none(), "{text}");
        assert_eq!(sent_click["delivery_mode"], "foreground", "{sent_click}");
        let (text, sent_click) = click(|| listing(9)).await;
        assert_eq!(
            text["unverified"].as_str(),
            Some(format!("Panel {COVERED}").as_str()),
            "{text}"
        );
        assert_eq!(sent_click["delivery_mode"], "background", "{sent_click}");
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
    async fn a_read_with_web_adds_the_page_address_and_dom_id_and_a_plain_read_does_not() {
        let desk = Desk::new(|| Ok(Page), Slow(Arc::default()), Os::Mac);
        let read = |args: Value| {
            let desk = &desk;
            async move {
                let out = desk.run("desk_read", args).await.unwrap().text;
                serde_json::from_str::<Value>(&out).unwrap()
            }
        };
        let web = read(json!({"id": "form", "web": true})).await;
        assert_eq!(
            (&web["url"], &web["dom_id"]),
            (&json!("https://shop.test/form"), &json!("pay"))
        );
        let plain = read(json!({"id": "form"})).await;
        assert!(
            plain.get("url").is_none() && plain.get("dom_id").is_none(),
            "{plain}"
        );
    }

    #[tokio::test]
    async fn a_launch_the_driver_refuses_by_name_retries_once_by_the_running_apps_bundle() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Page), Launches(sent.clone()), Os::Mac);
        let args = json!({"app": "Chrome", "args": ["https://shop.test/form"]});
        desk.run("desk_launch", args).await.unwrap();
        desk.run("desk_launch", json!({"app": "TextEdit"}))
            .await
            .unwrap();
        let names: Vec<Value> = sent
            .lock()
            .unwrap()
            .iter()
            .map(|a| a["name"].clone())
            .collect();
        assert_eq!(
            names,
            [json!("Chrome"), json!("Google Chrome"), json!("TextEdit")]
        );
        assert_eq!(
            sent.lock().unwrap()[1]["urls"],
            json!(["https://shop.test/form"])
        );
    }

    #[tokio::test]
    async fn a_page_tree_stays_on_while_anyone_holds_it_and_goes_off_with_the_last_hold() {
        let desk = Desk::new(|| Ok(Page), Slow(Arc::default()), Os::Mac)
            .allow(&["Google Chrome".to_owned()]);
        let switch = |pid: u32, on: bool, holder: &str| {
            desk.run("desk_web", json!({"pid": pid, "on": on, "holder": holder}))
        };
        let lit = || PAGE_LIT.load(Ordering::SeqCst);
        let off = r#"{"ok":true,"was":false}"#;
        assert_eq!(switch(7, true, "run-1").await.unwrap().text, off);
        assert_eq!(switch(7, true, "record").await.unwrap().text, off);
        assert!(lit());
        assert_eq!(switch(7, false, "run-1").await.unwrap().text, off);
        assert!(lit(), "the recording still holds it");
        desk.release("record");
        for _ in 0..50 {
            if !lit() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!lit());
        let refused = switch(9, true, "run-2").await.unwrap_err();
        assert!(refused.contains("1Password"), "{refused}");
        assert!(!lit());
        let (go, held) = std::sync::mpsc::channel::<()>();
        desk.access.post(move |_| {
            let _ = held.recv();
        });
        let first = switch(7, true, "run-3");
        let second = switch(7, true, "run-4");
        tokio::pin!(first, second);
        for pending in [&mut first, &mut second] {
            assert!(tokio::time::timeout(Duration::ZERO, pending).await.is_err());
        }
        go.send(()).unwrap();
        let on = r#"{"ok":true,"was":true}"#;
        assert_eq!(second.await.unwrap().text, on);
        assert_eq!(first.await.unwrap().text, off);
        switch(7, false, "run-4").await.unwrap();
        switch(7, false, "run-3").await.unwrap();
        assert!(
            !lit(),
            "two first holders turned the tree on, so the last one off turns it off"
        );
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
            started.elapsed() < Duration::from_secs(2),
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
        let (out, _) = wait_on(&desk, 2000).await;
        assert_eq!(out["changed"], true);
        assert_eq!(lists(&desk).await, 2);
        let seen = desk
            .access
            .run(CALL_LIMIT, (), |b| {
                Ok(b.opens.as_ref().and_then(|o| o.seen))
            })
            .await
            .unwrap();
        assert_eq!(seen, Some(3));
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
            |n| vec![(0, if n < 2 { 0 } else { 200 })],
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
            |n| vec![(0, if n < 2 { 0 } else { 200 })],
            json!({"hold_ms": 1000, "timeout_ms": 150}),
        )
        .await;
        assert_eq!(pair(&short), (json!(false), json!(true)), "{short}");
        assert!(
            (150..1000).contains(&short["ms"].as_u64().unwrap()),
            "{short}"
        );
    }

    struct RefusingSheet(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for RefusingSheet {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            lock(&self.0).push((tool.to_owned(), args.clone()));
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 1.0}}),
                "list_windows" if args["pid"] == 12 => {
                    json!({"structuredContent": {"windows": [{"pid": 12, "window_id": 9, "app_name": "Terminal"}]}})
                }
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 4, "window_id": 1, "layer": 0, "z_index": 1, "title": "Untitled - Notepad", "app_name": "Notepad", "bounds": {"x": 0.0, "y": 0.0, "width": 800.0, "height": 800.0}}
                ]}}),
                "get_window_state" => json!({"structuredContent": {
                    "screenshot_width": 800, "screenshot_height": 800,
                    "window_bounds": {"width": 800.0, "height": 800.0},
                    "background_input": {"routes": [{"route": "window_pointer", "status": "refused"}]}
                }}),
                _ => json!({}),
            })
        }
    }

    #[tokio::test]
    async fn a_named_window_click_under_another_apps_window_never_falls_back_to_the_desktop() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), RefusingSheet(Arc::clone(&calls)), Os::Mac)
            .allow(&["notepad".into()]);
        let out = desk
            .run(
                "desk_click",
                json!({"x": 50.0, "y": 600.0, "window": "Untitled"}),
            )
            .await;
        assert_eq!(out.unwrap_err(), COVERED_NO_CLICK);
        assert!(
            sent(&calls, "click").is_empty(),
            "{:?}",
            sent(&calls, "click")
        );
    }

    #[derive(Default)]
    pub(crate) struct Fake {
        nodes: HashMap<String, (Node, Option<String>)>,
        pub(crate) hits: HashMap<(i32, i32), String>,
        pub(crate) acts: Vec<(String, Act)>,
        pub(crate) ancestry: usize,
        covers: Option<(String, Option<u32>)>,
        opened: bool,
        menus: Vec<bool>,
        urls: Vec<bool>,
        untitled: bool,
        top: Option<u32>,
        blind: bool,
        listed: Option<u32>,
        pub(crate) behind: bool,
        pub(crate) pid: Option<u32>,
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
            Ok(vec![frame("w", title, 4, !self.behind, Some("Notepad"))])
        }

        fn tree(&mut self, frame: &Frame, extras: Extras) -> Result<Tree, String> {
            self.menus.push(extras.menus);
            self.urls.push(extras.urls);
            Ok(Tree {
                frame: frame.clone(),
                ..Tree::default()
            })
        }

        fn node(&mut self, id: &str, extras: Extras, _: usize) -> Result<Node, String> {
            self.urls.push(extras.urls);
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
            Ok(self.nodes.contains_key(id).then_some(self.pid.unwrap_or(4)))
        }

        fn pid_at(&mut self, _: i32, _: i32) -> Result<Option<u32>, String> {
            Ok(self.top)
        }

        fn owner_at(&mut self, x: i32, y: i32) -> Result<Option<u32>, String> {
            if self.blind {
                return Err("accessibility hit test failed with -25204".into());
            }
            self.pid_at(x, y)
        }

        fn listed_owner_at(&mut self, _: i32, _: i32) -> Result<Option<u32>, String> {
            Ok(self.listed)
        }

        fn covering(
            &mut self,
            _: &Frame,
            _: i32,
            _: i32,
        ) -> Result<Option<(String, Option<u32>)>, String> {
            Ok(self.covers.clone())
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

    struct Capped {
        ids: HashMap<String, ()>,
        last: Vec<Frame>,
    }

    impl Capped {
        fn full() -> Self {
            Self {
                ids: (1..MAX_IDS - 1)
                    .map(|n| (format!("0/{n}"), ()))
                    .chain([("w".to_owned(), ())])
                    .collect(),
                last: Vec::new(),
            }
        }
    }

    impl Access for Capped {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            self.ids.insert("w".into(), ());
            self.last = vec![frame("w", "Untitled - Notepad", 4, true, Some("Notepad"))];
            Ok(self.last.clone())
        }

        fn evict(&mut self) {
            evict_ids(&mut self.ids, &self.last);
        }

        fn tree(&mut self, frame: &Frame, _: Extras) -> Result<Tree, String> {
            lookup(&self.ids, &frame.id)?;
            self.ids.insert("ok".into(), ());
            Ok(Tree {
                frame: frame.clone(),
                nodes: vec![self.node("ok", Extras::BOUNDS, 0)?],
                ..Tree::default()
            })
        }

        fn node(&mut self, id: &str, _: Extras, _: usize) -> Result<Node, String> {
            lookup(&self.ids, id)?;
            Ok(Node {
                id: id.into(),
                role: if id == "w" { "window" } else { "button" },
                states: vec!["showing"],
                bounds: Some([100, 100, 80, 20]),
                ..Node::default()
            })
        }

        fn ancestors(&mut self, id: &str) -> Result<Vec<String>, String> {
            lookup(&self.ids, id)?;
            Ok(if id == "w" {
                Vec::new()
            } else {
                vec!["w".into()]
            })
        }

        fn pid_of(&mut self, id: &str) -> Result<Option<u32>, String> {
            lookup(&self.ids, id).map(|()| Some(4))
        }

        fn at(&mut self, _: &str, _: i32, _: i32) -> Result<Option<String>, String> {
            Ok(None)
        }

        fn act(&mut self, id: &str, _: Act) -> Result<bool, String> {
            lookup(&self.ids, id).map(|()| true)
        }

        fn close(&mut self, _: &Frame) -> Result<(), String> {
            Ok(())
        }
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
        unanswered: Option<Frame>,
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

        fn unanswered(&mut self) -> Option<Frame> {
            self.unanswered.clone()
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
            self.skew.fetch_add(self.list_ms, Ordering::Relaxed);
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

    struct HeldWhileClicking(
        Arc<OnceLock<Arc<tokio::sync::Mutex<()>>>>,
        Arc<Mutex<Vec<bool>>>,
    );

    impl Input for HeldWhileClicking {
        async fn call(&self, tool: &str, _: Value) -> Result<Value, String> {
            if tool == "click"
                && let Some(turn) = self.0.get()
            {
                self.1.lock().unwrap().push(turn.try_lock().is_err());
            }
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

    struct SlowRead(Behind);

    impl Input for SlowRead {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            let out = self.0.call(tool, args).await;
            if tool == "get_window_state" {
                tokio::time::sleep(Duration::from_millis(50)).await;
                self.0
                    .0
                    .lock()
                    .unwrap()
                    .push(("read_done".to_owned(), json!({})));
            }
            out
        }
    }

    struct SpaceTwins(Twins);

    impl Input for SpaceTwins {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            let mut out = self.0.call(tool, args).await?;
            if let Value::Array(windows) = &mut out["structuredContent"]["windows"] {
                for w in windows {
                    let here = w["window_id"] == 1;
                    w["is_on_screen"] = here.into();
                    w["on_current_space"] = here.into();
                    w["space_ids"] = json!([if here { 8 } else { 18 }]);
                }
            }
            Ok(out)
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

    struct BehindCover(Behind, AtomicBool, (i64, &'static str));

    impl Input for BehindCover {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            let mut out = self.0.call(tool, args).await?;
            match tool {
                "bring_to_front" => self.1.store(true, Ordering::SeqCst),
                "list_windows" if !self.1.load(Ordering::SeqCst) => {
                    for w in out["structuredContent"]["windows"]
                        .as_array_mut()
                        .into_iter()
                        .flatten()
                        .filter(|w| w["pid"] == 4)
                    {
                        w["z_index"] = 3.into();
                        w["pid"] = self.2.0.into();
                        w["title"] = self.2.1.into();
                    }
                }
                "list_windows" | "get_window_state" | "get_screen_size" => {}
                _ => self.1.store(false, Ordering::SeqCst),
            }
            Ok(out)
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
                    "screenshot_width": 500, "screenshot_height": 500,
                    "window_bounds": {"width": 500.0, "height": 500.0},
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

    const LOCK_HINT: &str = "the screen is locked or showing the screen saver, which also happens when the screen saver starts or the display sleeps with no password required. While it lasts, macOS hides every app's windows from accessibility and sends input to the lock screen, so Cua Driver refuses instead of acting; screenshots still work. Wake and unlock the Mac, then retry.";

    struct Locked(Behind);

    impl Input for Locked {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            if tool == "press_key" {
                return Err(format!(
                    "{tool}: window 1 cannot receive input: {LOCK_HINT}"
                ));
            }
            self.0.call(tool, args).await
        }
    }

    struct LoginWindow(Behind);

    impl Input for LoginWindow {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            if tool == "list_windows" && args["pid"] == 776 {
                return Ok(json!({"structuredContent": {"windows": [
                    {"pid": 776, "window_id": 692, "layer": 2002, "title": "", "app_name": "loginwindow", "bounds": {"x": 0.0, "y": 0.0, "width": 1280.0, "height": 800.0}}
                ]}}));
            }
            self.0.call(tool, args).await
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

    struct FailsOn(&'static str);

    impl Input for FailsOn {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            if args["text"] == self.0 {
                return Err(format!("{tool}: refused"));
            }
            Ok(json!({}))
        }
    }

    struct Spaces(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Spaces {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 4, "window_id": 1, "layer": 0, "title": "Untitled - Notepad", "app_name": "Notepad", "is_on_screen": true, "on_current_space": true, "space_ids": [8]},
                    {"pid": 9, "window_id": 84080, "layer": 0, "title": "\u{5fae}\u{4fe1}", "app_name": "WeChat", "is_on_screen": false, "on_current_space": false, "space_ids": [18]},
                    {"pid": 9, "window_id": 727, "layer": 0, "title": "\u{5fae}\u{4fe1}", "app_name": "WeChat", "is_on_screen": false, "on_current_space": null, "space_ids": null},
                    {"pid": 9, "window_id": 37333, "layer": 3, "title": "", "app_name": "WeChat", "is_on_screen": false, "on_current_space": false, "space_ids": [18]},
                    {"pid": 5, "window_id": 2, "layer": 0, "title": "Untitled", "app_name": "TextEdit", "is_on_screen": true, "on_current_space": true, "space_ids": [8]},
                    {"pid": 5, "window_id": 3, "layer": 0, "title": "Untitled", "app_name": "TextEdit", "is_on_screen": false, "on_current_space": false, "space_ids": [18]}
                ]}}),
                "type_text" | "press_key" => json!({
                    "content": [{"type": "text", "text": "Sent (unverified) 1 char(s) via CGEvent."}],
                    "structuredContent": {"effect": "unverifiable"}
                }),
                _ => json!({}),
            })
        }
    }

    struct SecondDisplay(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for SecondDisplay {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 1.0}}),
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 4, "window_id": 1, "layer": 0, "z_index": 2, "title": "Untitled - Notepad", "app_name": "Notepad", "is_on_screen": true, "on_current_space": true, "space_ids": [8], "bounds": {"x": 0.0, "y": 0.0, "width": 800.0, "height": 600.0}},
                    {"pid": 9, "window_id": 100, "layer": 0, "z_index": 1, "title": "\u{5fae}\u{4fe1}", "app_name": "WeChat", "is_on_screen": true, "on_current_space": true, "space_ids": [8], "bounds": {"x": -1416.0, "y": 0.0, "width": 846.0, "height": 774.0}}
                ]}}),
                "get_window_state" => {
                    let mut v = json!({"structuredContent": {"screenshot_width": 846, "screenshot_height": 774}});
                    v["content"] = png_content(846, 774);
                    v
                }
                "get_desktop_state" => {
                    let mut v = json!({"structuredContent": {"screenshot_width": 100, "screenshot_original_width": 100}});
                    v["content"] = png_content(100, 60);
                    v
                }
                _ => json!({}),
            })
        }
    }

    fn png_content(w: u32, h: u32) -> Value {
        let mut png = Vec::new();
        let mut enc = png::Encoder::new(&mut png, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.write_header()
            .unwrap()
            .write_image_data(&vec![200; (w * h * 3) as usize])
            .unwrap();
        json!([{"type": "image", "data": B64.encode(&png)}])
    }

    struct Wide {
        calls: Mutex<Vec<(String, Value)>>,
        main: f64,
        titles: (&'static str, Option<&'static str>),
        slow: bool,
        ratio: Mutex<f64>,
        landed: Mutex<Vec<(f64, f64)>>,
    }

    impl Wide {
        const BOUNDS: [f64; 4] = [-1300.0, 100.0, 1000.0, 600.0];
        const BACKING: f64 = 2.0;
        const CAP: f64 = 1280.0;

        fn new(main: f64) -> Arc<Self> {
            Self::with(main, ("Wide", None), false)
        }

        fn with(main: f64, titles: (&'static str, Option<&'static str>), slow: bool) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                main,
                titles,
                slow,
                ratio: Mutex::new(0.0),
                landed: Mutex::new(Vec::new()),
            })
        }

        fn sent(&self, tool: &str) -> Vec<Value> {
            sent(&self.calls, tool)
        }

        fn landed(&self) -> Vec<(f64, f64)> {
            self.landed.lock().unwrap().clone()
        }
    }

    impl Input for Arc<Wide> {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.calls
                .lock()
                .unwrap()
                .push((tool.to_owned(), args.clone()));
            let [x, y, w, h] = Wide::BOUNDS;
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": self.main}}),
                "list_windows" => {
                    let window = |id: i64, z: i64, title: &str| json!({"pid": 7, "window_id": id, "layer": 0, "z_index": z, "title": title, "is_on_screen": true, "bounds": {"x": x, "y": y, "width": w, "height": h}});
                    let mut windows = vec![window(40, 5, self.titles.0)];
                    if let Some(title) = self.titles.1 {
                        windows.push(window(41, 4, title));
                    }
                    json!({"structuredContent": {"windows": windows}})
                }
                "get_window_state" => {
                    let native = w * Wide::BACKING;
                    let shown = if args["max_image_dimension"] == 0 {
                        native
                    } else {
                        Wide::CAP
                    };
                    *self.ratio.lock().unwrap() = native / shown;
                    if self.slow && shown < native {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    let k = shown / w;
                    json!({"structuredContent": {
                        "screenshot_width": shown, "screenshot_height": h * k,
                        "window_bounds": {"x": x, "y": y, "width": w, "height": h}
                    }, "content": [{"type": "image", "data": B64.encode(rgb_png(vec![0; 3], [0, 0, 1, 1], None).unwrap())}]})
                }
                "click" => {
                    let ratio = *self.ratio.lock().unwrap();
                    let at = |v: &Value| v.as_f64().unwrap() * ratio / Wide::BACKING;
                    self.landed
                        .lock()
                        .unwrap()
                        .push((x + at(&args["x"]), y + at(&args["y"])));
                    json!({})
                }
                _ => json!({}),
            })
        }
    }

    struct Stack(Arc<Mutex<Vec<(String, Value)>>>, Mutex<Value>);

    impl Input for Stack {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 1.0}}),
                "list_windows" => {
                    let mut doc = json!({"pid": 5, "window_id": 3, "layer": 0, "z_index": 1, "title": "Doc", "app_name": "TextEdit", "is_on_screen": true, "on_current_space": true, "space_ids": [8]});
                    for (k, v) in self.1.lock().unwrap().as_object().unwrap() {
                        doc[k] = v.clone();
                    }
                    json!({"structuredContent": {"windows": [
                        {"pid": 4, "window_id": 1, "layer": 0, "z_index": 3, "title": "Untitled - Notepad", "app_name": "Notepad", "is_on_screen": true, "on_current_space": true, "space_ids": [8], "bounds": {"x": 0.0, "y": 0.0, "width": 300.0, "height": 600.0}},
                        {"pid": 8, "window_id": 2, "layer": 0, "z_index": 2, "title": "Inbox", "app_name": "Mail", "is_on_screen": true, "on_current_space": true, "space_ids": [8], "bounds": {"x": 250.0, "y": 0.0, "width": 550.0, "height": 600.0}},
                        doc
                    ]}})
                }
                "get_window_state" => {
                    let mut v = json!({"structuredContent": {
                        "screenshot_width": 400, "screenshot_height": 300,
                        "window_bounds": {"width": 400.0, "height": 300.0}
                    }});
                    v["content"] = png_content(400, 300);
                    v
                }
                "click" | "type_text" => json!({
                    "content": [{"type": "text", "text": "Posted via CGEvent."}],
                    "structuredContent": {"effect": "unverifiable"}
                }),
                _ => json!({}),
            })
        }
    }

    fn doc_at(x: f64) -> Value {
        json!({"bounds": {"x": x, "y": 100.0, "width": 400.0, "height": 300.0}})
    }

    struct Tty(Arc<Mutex<Vec<(String, Value)>>>, &'static str, &'static str);

    impl Input for Tty {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            let quiet = args["text"] == self.1;
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 5, "window_id": 3, "layer": 0, "z_index": 1, "title": "Doc", "app_name": "TextEdit", "is_on_screen": true, "on_current_space": true, "space_ids": [8]}
                ]}}),
                "type_text" if quiet => json!({
                    "content": [{"type": "text", "text": self.2}],
                    "structuredContent": {"effect": "unverifiable"}
                }),
                "type_text" => json!({
                    "content": [{"type": "text", "text": if self.2.contains(TERMINAL) { "Typed 1 char(s) (terminal emulator)." } else { "Typed 1 char(s)." }}],
                    "structuredContent": {"effect": "confirmed"}
                }),
                _ => json!({}),
            })
        }
    }

    struct HiddenTwin(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for HiddenTwin {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            let window = args["window_id"].as_i64();
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 1.0}}),
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 9, "window_id": 100, "layer": 0, "z_index": 2, "title": "\u{5fae}\u{4fe1}", "app_name": "WeChat", "is_on_screen": true, "on_current_space": true, "space_ids": [8], "bounds": {"x": -1416.0, "y": 0.0, "width": 846.0, "height": 774.0}},
                    {"pid": 9, "window_id": 200, "layer": 0, "z_index": 1, "title": "\u{5fae}\u{4fe1}", "app_name": "WeChat", "is_on_screen": false, "on_current_space": true, "space_ids": [8], "bounds": {"x": 0.0, "y": 0.0, "width": 280.0, "height": 380.0}},
                    {"pid": 5, "window_id": 300, "layer": 0, "z_index": 0, "title": "Doc", "app_name": "TextEdit", "is_on_screen": false, "on_current_space": true, "space_ids": [8], "bounds": {"x": 0.0, "y": 0.0, "width": 500.0, "height": 400.0}}
                ]}}),
                "get_window_state" if window == Some(200) => json!({"structuredContent": {
                    "background_input": {"routes": [{"route": "window_pointer", "status": "refused"}]}
                }}),
                "get_window_state" => json!({"structuredContent": {
                    "screenshot_width": 846, "screenshot_height": 774,
                    "window_bounds": {"width": 846.0, "height": 774.0}
                }}),
                "click" => json!({"structuredContent": {"effect": "unverifiable"}}),
                _ => json!({}),
            })
        }
    }

    fn sent(calls: &Mutex<Vec<(String, Value)>>, tool: &str) -> Vec<Value> {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _)| t == tool)
            .map(|(_, a)| a.clone())
            .collect()
    }

    fn refused(r: Result<Output, String>) -> bool {
        r.err()
            .is_some_and(|e| e.contains("not in the app allowlist"))
    }

    fn note(out: Output) -> String {
        let v: Value = serde_json::from_str(&out.text).unwrap();
        v["unverified"].as_str().unwrap_or_default().to_owned()
    }

    struct NoLockedInput(Behind);

    impl Input for NoLockedInput {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            if args.get("locked_input").is_some() {
                return Err(format!(
                    "{tool}: argument locked_input; accepted: text, pid, window_id, delivery_mode"
                ));
            }
            self.0.call(tool, args).await
        }
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

    struct AwayTwin(Twins);

    impl Input for AwayTwin {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            let mut out = self.0.call(tool, args).await?;
            if tool == "click" {
                out = json!({"structuredContent": {"effect": "unverifiable"}});
            }
            if let Value::Array(windows) = &mut out["structuredContent"]["windows"] {
                for w in windows {
                    let here = w["window_id"] == 3;
                    w["is_on_screen"] = here.into();
                    w["on_current_space"] = here.into();
                    w["space_ids"] = json!([if here { 8 } else { 18 }]);
                }
            }
            Ok(out)
        }
    }

    struct Unfronted(Behind);

    impl Input for Unfronted {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            if tool == "bring_to_front" {
                self.0.0.lock().unwrap().push((tool.to_owned(), args));
                return Err("bring_to_front: window 2 is not frontmost".to_owned());
            }
            self.0.call(tool, args).await
        }
    }

    struct Slow(Arc<Mutex<Vec<String>>>);

    impl Input for Slow {
        async fn call(&self, _: &str, args: Value) -> Result<Value, String> {
            let typed = args["key"]
                .as_str()
                .or(args["text"].as_str())
                .unwrap_or_default();
            let n = typed.chars().count().max(1) as u64;
            self.0.lock().unwrap().push(typed.to_owned());
            tokio::time::sleep(Duration::from_millis(5 * n)).await;
            Ok(json!({}))
        }
    }

    struct NoFront(Behind);

    impl Input for NoFront {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            if tool == "bring_to_front" {
                return Err(format!(
                    "bring_to_front: foreground_unavailable{}1",
                    crate::driver::ACTUAL_FRONT
                ));
            }
            self.0.call(tool, args).await
        }
    }

    struct Screen(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Screen {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args.clone()));
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 2.0}}),
                "list_windows" if args["pid"] == 9 => json!({"structuredContent": {"windows": [
                    {"pid": 9, "window_id": 43, "app_name": "Panel", "layer": 3, "z_index": 31, "bounds": {"x": 100.0, "y": 50.0, "width": 200.0, "height": 100.0}}
                ]}}),
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

    struct Sized(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Sized {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 2.0}}),
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 4, "window_id": 40, "title": "Untitled - Notepad", "app_name": "Notepad", "bounds": {"x": 100.0, "y": 50.0, "width": 300.0, "height": 200.0}}
                ]}}),
                "get_window_state" => json!({"structuredContent": {
                    "screenshot_width": 600, "screenshot_height": 400,
                    "window_bounds": {"x": 100.0, "y": 50.0, "width": 300.0, "height": 200.0}
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

    struct Launches(Arc<Mutex<Vec<Value>>>);

    impl Input for Launches {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            if tool == "launch_app" {
                self.0.lock().unwrap().push(args.clone());
                if args["name"] == "Chrome" {
                    return Err(
                        "launch_app: No installed macOS app found for name 'Chrome'.".into(),
                    );
                }
            }
            Ok(json!({"content": [{"type": "text", "text": "Launched"}]}))
        }
    }

    struct Page;

    impl Access for Page {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            Ok(vec![
                frame("7/1", "Form - Chrome", 7, true, Some("Google Chrome")),
                frame("9/1", "Vault", 9, false, Some("1Password")),
            ])
        }

        fn tree(&mut self, frame: &Frame, _: Extras) -> Result<Tree, String> {
            Ok(Tree {
                frame: frame.clone(),
                ..Tree::default()
            })
        }

        fn node(&mut self, id: &str, _: Extras, _: usize) -> Result<Node, String> {
            Ok(Node {
                id: id.into(),
                role: "document",
                ..Node::default()
            })
        }

        fn web_of(&mut self, id: &str) -> Result<(Option<String>, Option<String>), String> {
            Ok((Some(format!("https://shop.test/{id}")), Some("pay".into())))
        }

        fn web(&mut self, _: u32, on: bool) -> Result<bool, String> {
            Ok(PAGE_LIT.swap(on, Ordering::SeqCst))
        }

        fn bundle_name(&mut self, app: &str) -> Result<Option<String>, String> {
            Ok((app == "Chrome").then(|| "Google Chrome".to_owned()))
        }

        fn ancestors(&mut self, _: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }

        fn at(&mut self, _: &str, _: i32, _: i32) -> Result<Option<String>, String> {
            Ok(None)
        }

        fn act(&mut self, _: &str, _: Act) -> Result<bool, String> {
            Ok(true)
        }

        fn close(&mut self, _: &Frame) -> Result<(), String> {
            Ok(())
        }
    }

    struct Scrollback;

    impl Access for Scrollback {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            Ok(Vec::new())
        }

        fn tree(&mut self, _: &Frame, _: Extras) -> Result<Tree, String> {
            Err("no tree".into())
        }

        fn node(&mut self, id: &str, extras: Extras, chars: usize) -> Result<Node, String> {
            Ok(Node {
                id: id.into(),
                role: "document",
                value: Some(clip(
                    match id {
                        "long" => format!("{}中🙂end", "a".repeat(READ_CHARS)),
                        _ => "b".repeat(READ_CHARS),
                    },
                    chars,
                    extras.tail,
                )),
                name: "n".repeat(100),
                ..Node::default()
            })
        }

        fn ancestors(&mut self, _: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }

        fn at(&mut self, _: &str, _: i32, _: i32) -> Result<Option<String>, String> {
            Ok(None)
        }

        fn act(&mut self, _: &str, _: Act) -> Result<bool, String> {
            Ok(true)
        }

        fn close(&mut self, _: &Frame) -> Result<(), String> {
            Ok(())
        }
    }

    struct Sibling;

    impl Access for Sibling {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            let mut frames = Apps.frames()?;
            frames.push(frame("8/3", "Other - Mail", 8, false, Some("Mail")));
            Ok(frames)
        }

        fn tree(&mut self, frame: &Frame, extras: Extras) -> Result<Tree, String> {
            Apps.tree(frame, extras)
        }

        fn node(&mut self, id: &str, extras: Extras, chars: usize) -> Result<Node, String> {
            Apps.node(id, extras, chars)
        }

        fn ancestors(&mut self, id: &str) -> Result<Vec<String>, String> {
            Apps.ancestors(id)
        }

        fn at(&mut self, window: &str, x: i32, y: i32) -> Result<Option<String>, String> {
            Apps.at(window, x, y)
        }

        fn act(&mut self, id: &str, act: Act) -> Result<bool, String> {
            Apps.act(id, act)
        }

        fn close(&mut self, frame: &Frame) -> Result<(), String> {
            Apps.close(frame)
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

        fn chromium(&mut self, pid: u32) -> bool {
            pid == 8
        }

        fn off_space(&mut self) -> Vec<Frame> {
            vec![Frame {
                space: Some("other"),
                ..frame("84080", "\u{5fae}\u{4fe1}", 9, false, Some("WeChat"))
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

        fn keyboard_focus(&mut self, _: u32) -> Result<Option<Node>, String> {
            Ok(Some(Node {
                role: "textbox",
                ..Node::default()
            }))
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

    struct Long;

    impl Access for Long {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            Ok(vec![
                Frame {
                    number: Some(41),
                    ..frame("4/1", LONG_TITLE, 4, false, Some("Google Chrome"))
                },
                Frame {
                    number: Some(42),
                    ..frame(
                        "4/2",
                        "New Tab - Google Chrome \u{2013} Home",
                        4,
                        false,
                        Some("Google Chrome"),
                    )
                },
            ])
        }

        fn tree(&mut self, frame: &Frame, _: Extras) -> Result<Tree, String> {
            Ok(Tree {
                frame: frame.clone(),
                ..Tree::default()
            })
        }

        fn node(&mut self, id: &str, _: Extras, _: usize) -> Result<Node, String> {
            Err(format!("no node {id}"))
        }

        fn ancestors(&mut self, _: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }

        fn at(&mut self, _: &str, _: i32, _: i32) -> Result<Option<String>, String> {
            Ok(None)
        }

        fn act(&mut self, _: &str, _: Act) -> Result<bool, String> {
            Ok(true)
        }

        fn close(&mut self, _: &Frame) -> Result<(), String> {
            Ok(())
        }
    }

    struct Elided(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Elided {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 4, "window_id": 41, "layer": 0, "z_index": 1, "is_on_screen": true, "title": "Example Competitive Analy\u{2026}(October 2026) - Example Docs", "app_name": "Google Chrome", "bounds": {"x": 0.0, "y": 0.0, "width": 800.0, "height": 600.0}},
                    {"pid": 4, "window_id": 42, "layer": 0, "z_index": 2, "is_on_screen": true, "title": "New Tab", "app_name": "Google Chrome", "bounds": {"x": 40.0, "y": 40.0, "width": 800.0, "height": 600.0}}
                ]}}),
                _ => json!({}),
            })
        }
    }

    struct Chat(&'static str);

    impl Access for Chat {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            Ok(vec![frame(
                "4/1",
                "Untitled - Notepad",
                4,
                false,
                Some("Notepad"),
            )])
        }

        fn tree(&mut self, frame: &Frame, _: Extras) -> Result<Tree, String> {
            Ok(Tree {
                frame: frame.clone(),
                ..Tree::default()
            })
        }

        fn node(&mut self, id: &str, _: Extras, _: usize) -> Result<Node, String> {
            Err(format!("no node {id}"))
        }

        fn keyboard_focus(&mut self, pid: u32) -> Result<Option<Node>, String> {
            Ok((pid == 4).then(|| Node {
                id: "4/7".into(),
                role: self.0,
                name: "Message".into(),
                bounds: Some([100, 500, 800, 100]),
                ..Node::default()
            }))
        }

        fn ancestors(&mut self, _: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }

        fn at(&mut self, _: &str, _: i32, _: i32) -> Result<Option<String>, String> {
            Ok(None)
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

    struct Pair;

    impl Input for Pair {
        async fn call(&self, tool: &str, _: Value) -> Result<Value, String> {
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 1.0}}),
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 5, "window_id": 1, "layer": 0, "z_index": 1, "title": "Untitled", "bounds": {"x": 0.0, "y": 0.0, "width": 400.0, "height": 300.0}},
                    {"pid": 5, "window_id": 2, "layer": 0, "z_index": 2, "title": "Untitled", "bounds": {"x": 500.0, "y": 0.0, "width": 400.0, "height": 300.0}},
                    {"pid": 5, "window_id": 3, "layer": 0, "z_index": 3, "title": "Untitled", "bounds": {"x": 400.0, "y": 0.0, "width": 100.0, "height": 300.0}}
                ]}}),
                _ => json!({}),
            })
        }
    }

    struct TwinFrames;

    impl Access for TwinFrames {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            Ok(vec![
                frame("5/a", "Untitled", 5, true, Some("Mousepad")),
                frame("5/b", "Untitled", 5, false, Some("Mousepad")),
            ])
        }

        fn tree(&mut self, frame: &Frame, _: Extras) -> Result<Tree, String> {
            Ok(Tree {
                frame: frame.clone(),
                ..Tree::default()
            })
        }

        fn node(&mut self, id: &str, _: Extras, _: usize) -> Result<Node, String> {
            let bounds = match id {
                "5/a" => Some([0, 0, 400, 300]),
                "5/b" => Some([500, 0, 400, 300]),
                _ => None,
            };
            Ok(Node {
                id: id.into(),
                role: "button",
                bounds,
                ..Node::default()
            })
        }

        fn ancestors(&mut self, _: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }

        fn at(&mut self, window: &str, _: i32, _: i32) -> Result<Option<String>, String> {
            Ok((window == "5/b").then(|| "5/b1".into()))
        }

        fn pid_of(&mut self, _: &str) -> Result<Option<u32>, String> {
            Ok(Some(5))
        }

        fn act(&mut self, _: &str, _: Act) -> Result<bool, String> {
            Ok(false)
        }

        fn close(&mut self, _: &Frame) -> Result<(), String> {
            Ok(())
        }
    }

    async fn at_window(desk: &Desk<Apps, Desktop>, x: i32, y: i32) -> Result<Value, String> {
        let out = desk.run("desk_at", json!({"x": x, "y": y})).await?;
        let v: Value = serde_json::from_str(&out.text).unwrap();
        Ok(v["window"]["title"].clone())
    }

    struct Shifting(std::sync::atomic::AtomicUsize);

    impl Input for Shifting {
        async fn call(&self, tool: &str, _: Value) -> Result<Value, String> {
            let captures = self.0.load(Ordering::Relaxed);
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 1.0}}),
                "list_windows" => {
                    let pid = if captures == 0 { 8 } else { 4 };
                    json!({"structuredContent": {"windows": [
                        {"pid": pid, "window_id": pid, "layer": 0, "z_index": 1, "bounds": {"x": 0.0, "y": 0.0, "width": 10.0, "height": 10.0}}
                    ]}})
                }
                "get_desktop_state" => {
                    self.0.fetch_add(1, Ordering::Relaxed);
                    let shade = if captures == 0 { 255 } else { 128 };
                    let mut png = Vec::new();
                    let mut enc = png::Encoder::new(&mut png, 10, 10);
                    enc.set_color(png::ColorType::Rgb);
                    enc.write_header()
                        .unwrap()
                        .write_image_data(&[shade; 300])
                        .unwrap();
                    let mut v = json!({"structuredContent": {"screenshot_width": 10}});
                    v["content"] = json!([{"type": "image", "data": B64.encode(&png)}]);
                    v
                }
                _ => json!({}),
            })
        }
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
}
