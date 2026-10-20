//! The computer verbs: `desk_computer_state`, `desk_computer_act`, `desk_computer_end` and `desk_look` for a vision
//! agent, with the settle, the window report and the press guards, over cua-driver's line-delimited JSON in named driver sessions.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::time::Instant;

use desk_core::args::parse;
use desk_core::press::{MENU_POLL, MENU_POLLS};
use desk_core::wire::Output;

use crate::desk::{Cancel, Turn, Typer, text_runs};
use crate::driver::{Driver, image_of, text_of};
use crate::keys::{Os, key_names};
use crate::model::Modal;
use crate::press::libreoffice;
use crate::server::Conn;
use crate::still::{Block, VISIBLE_STEP};
use crate::volatile::{Shown, Volatile};
use crate::xshm::{self, Grabber, Placement, clip};

const STATE_ELEMENTS: u64 = 300;
const BACKGROUND_ACT: &str = "background input: computer actions send desktop input (screen-point clicks, core-keyboard typing), which needs the foreground; use the desk verbs with a window";
const TREE_CHARS: usize = 15_000;
const MIN_WINDOW: i64 = 10;
const CHROME_CLASS: &str = "Google-chrome";
const COMMIT_KEYS: [&str; 6] = ["return", "enter", "kp_enter", "f5", "alt+left", "alt+right"];
const SETTLE: Duration = Duration::from_millis(600);
const NO_CHANGE: Duration = Duration::from_millis(300);
const COMMIT_SETTLE: Duration = Duration::from_millis(3000);
const STILL_POLL: Duration = Duration::from_millis(40);
const SHM_POLL: Duration = Duration::from_millis(10);
const STILL_FOR: Duration = Duration::from_millis(40);
const WINDOW_STILL_FOR: Duration = Duration::from_millis(150);
const COMMIT_STILL_FOR: Duration = Duration::from_millis(400);
const COMMIT_FIRST: Duration = Duration::from_millis(1500);
const RETURN_GAP: Duration = Duration::from_millis(300);
const STILL_EDGE: u32 = 160;
const STILL_TOLERANCE: u8 = 48;
const MOVED_PERCENT: usize = 5;
const POPUP_ROWS: [&str; 6] = [
    "menu item",
    "menu",
    "list item",
    "tree item",
    "table cell",
    "option",
];
const PRESSED: [&str; 2] = ["push button", "menu item"];
const JPEG_QUALITY: u8 = 80;
const DEFAULT_EDGE: u32 = 1024;
const PIXELS_PER_TOKEN: u32 = 750;
const PHOTO_COLORS: usize = 2500;
const COLOR_STRIDE: usize = 7;
const MAX_WAIT_SECS: f64 = 10.0;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_RETRY: Duration = Duration::from_millis(250);
const CONNECT_WAIT: Duration = Duration::from_secs(10);
const DEFAULT_SESSION: &str = "computer";
const SKIPPED: &str = "had no keycode";
const WM_WAIT: Duration = Duration::from_secs(10);
const WM_POLL: Duration = Duration::from_millis(100);
const AX_UNSUPPORTED: &str = "returned -25206";
const OFF_SCREEN: &str = "the rectangle is off the screen";
const XTEST_TYPED: &str = "Typed text into the focused desktop application.";
const NO_TEXT_TARGET: &str =
    "no application window is on the screen to type into; open or focus one first";
#[cfg(target_os = "macos")]
const UNDER: Under = crate::ax::window_at;
#[cfg(not(target_os = "macos"))]
const UNDER: Under = |_, _| None;
const RETRIED: [&str; 4] = [
    "list_windows",
    "get_window_state",
    "get_desktop_state",
    "end_session",
];

type Under = fn(f64, f64) -> Option<(i64, i64)>;

/// The computer verbs' state: the element tokens of each session's latest window state and the scale of its latest screenshot.
pub struct Computer {
    driver: Arc<Driver>,
    os: Os,
    typer: Typer,
    under: Under,
    last: Mutex<HashMap<String, Snapshot>>,
    input: Arc<Mutex<()>>,
    /// The MIT-SHM grabber, opened on first use and again after a failure.
    grabber: std::sync::Mutex<Option<Grabber>>,
    /// Titles of the windows an action opened, to name them when they close.
    titles: std::sync::Mutex<HashMap<u32, String>>,
    /// Per session, screen pixels per image pixel of the last screenshot.
    scales: std::sync::Mutex<HashMap<String, f64>>,
    /// Screen pixels per image pixel of the latest screenshot of any session, one-shot included.
    last_scale: std::sync::Mutex<f64>,
    backing: std::sync::Mutex<f64>,
    unmapped: std::sync::Mutex<HashSet<String>>,
    /// The screen blocks that change while no action runs, which the settle ignores; sampled from the first computer call.
    volatile: OnceLock<Volatile>,
    ids: AtomicU64,
    /// How many default sessions were ended, which names every caller's next default.
    defaults: AtomicU64,
    background: bool,
}

impl Computer {
    /// Creates a client of `driver`'s daemon on `os`, whose click spacing it shares.
    pub fn new(driver: Arc<Driver>, os: Os) -> Self {
        Self {
            driver,
            os,
            typer: crate::desk::TYPER,
            under: UNDER,
            last: Mutex::new(HashMap::new()),
            input: Arc::new(Mutex::new(())),
            grabber: std::sync::Mutex::new(None),
            titles: std::sync::Mutex::new(HashMap::new()),
            scales: std::sync::Mutex::new(HashMap::new()),
            last_scale: std::sync::Mutex::new(1.0),
            backing: std::sync::Mutex::new(1.0),
            unmapped: std::sync::Mutex::new(HashSet::new()),
            volatile: OnceLock::new(),
            ids: AtomicU64::new(0),
            defaults: AtomicU64::new(0),
            background: false,
        }
    }

    /// Whether input must reach its window without raising it or moving the pointer; the computer actions refuse then.
    pub fn background(mut self, on: bool) -> Self {
        self.background = on;
        self
    }

    /// Answers the computer verb `tool` of caller `agent`, whose default session is its own.
    pub async fn run(&self, agent: &str, tool: &str, mut args: Value) -> Result<Output, String> {
        let default =
            !matches!(tool, "desk_look" | "desk_title_at") && !args["session"].is_string();
        if default {
            args["session"] = self.default_session(agent).into();
        }
        match tool {
            "desk_computer_state" => self.state(args).await,
            "desk_computer_act" if self.background => Err(BACKGROUND_ACT.to_owned()),
            "desk_computer_act" => self.act(args).await,
            "desk_computer_end" => {
                let ended = self.end(args).await;
                if default {
                    self.defaults.fetch_add(1, Ordering::Relaxed);
                }
                ended.map(Output::text)
            }
            "desk_look" => self.look(&args).await,
            "desk_computer_scale" => {
                let scale = self.scale_of(&session_of(&args));
                Ok(Output::text(json!({"scale": scale}).to_string()))
            }
            "desk_title_at" => {
                let a: PointArgs = parse(args)?;
                Ok(Output::text(
                    json!({"title": self.title_at(a.x, a.y)}).to_string(),
                ))
            }
            other => Err(format!("unknown computer verb {other}")),
        }
    }

    /// `computer_state`: window list, accessibility elements and a desktop screenshot.
    async fn state(&self, args: Value) -> Result<Output, String> {
        self.touch();
        let session = session_of(&args);
        let a: StateArgs = parse(args)?;
        let windows = self
            .request(&session, "list_windows", json!({"on_screen_only": true}))
            .await?;
        let listed = window_list(&windows);
        let mut text = String::new();
        let target = match a.pid {
            Some(pid) => Some((
                pid,
                a.window_id
                    .or_else(|| listed.iter().find(|w| w.pid == pid).map(|w| w.window_id)),
            )),
            None => self
                .app_of(&listed)
                .or(listed.first())
                .map(|w| (w.pid, Some(w.window_id))),
        };
        let mut elements = None;
        if let Some((pid, window_id)) = target {
            let mut req = json!({
                "pid": pid,
                "include_screenshot": false,
                "max_elements": STATE_ELEMENTS,
                "tree_format": "elements",
            });
            if let Some(id) = window_id {
                req["window_id"] = id.into();
            }
            if let Some(q) = a.query {
                req["query"] = q.into();
            }
            match self.request(&session, "get_window_state", req).await {
                Ok(state) => {
                    elements = Some((pid, window_id_of(&state).or(window_id), state));
                }
                Err(e) => {
                    let _ = writeln!(text, "\nElements of pid={pid} unavailable: {e}");
                }
            }
        }
        // the capture comes last: a later get_window_state replaces the session's pixel-click context
        let (image_b64, scale, screen) =
            self.capture(&session, a.max_edge, a.workspace_only).await?;
        let k = scale;
        let mut head = String::from("Windows (frontmost first):\n");
        for w in &listed {
            let _ = writeln!(
                head,
                "- pid={} window_id={} {} \"{}\" at ({:.0},{:.0}) {:.0}x{:.0}",
                w.pid,
                w.window_id,
                w.app,
                w.title,
                w.x as f64 / k,
                w.y as f64 / k,
                w.w as f64 / k,
                w.h as f64 / k
            );
        }
        for m in self.modals() {
            head.push_str(&modal_line(&m));
        }
        text.insert_str(0, &head);
        if let Some((pid, window_id, state)) = elements {
            let (tree, elements) = element_tree(&state, scale);
            let _ = writeln!(
                text,
                "\nElements of pid={pid} (center = screenshot pixels):"
            );
            text.push_str(&tree);
            self.last.lock().await.insert(
                session.clone(),
                Snapshot {
                    pid,
                    window_id,
                    elements,
                    screen,
                },
            );
        }
        if let Some(b64) = &image_b64 {
            let _ = write!(text, "\nScreenshot: {}", weigh(b64));
        }
        Ok(Output {
            text,
            is_error: false,
            image_b64,
            ..Output::default()
        })
    }

    /// `computer_act`: one input action, or a list of `actions`, followed by a fresh desktop screenshot.
    async fn act(&self, args: Value) -> Result<Output, String> {
        self.touch();
        let session = session_of(&args);
        if args.get("actions").is_some() {
            let list: ActList = parse(args)?;
            positive(list.scale)?;
            return self.act_list(&session, list).await;
        }
        let a: ActArgs = parse(args)?;
        positive(a.scale)?;
        self.act_one(&session, a).await
    }

    /// Screen pixels per pixel of the screenshot `scale` names, else of `session`'s latest.
    fn mapped(&self, session: &str, scale: Option<f64>) -> f64 {
        scale.unwrap_or_else(|| self.scale_of(session))
    }

    /// Runs the actions in order, each settled, stops at the first failure and returns the last screenshot;
    /// every x,y maps by the screenshot returned before the list, the one the caller saw.
    async fn act_list(&self, session: &str, list: ActList) -> Result<Output, String> {
        let total = list.actions.len();
        let mut lines = Vec::with_capacity(total);
        let mut out = Output::default();
        let seen = self.mapped(session, list.scale);
        for (i, mut a) in list.actions.into_iter().enumerate() {
            a.max_edge = list.max_edge;
            a.scale = Some(seen);
            let action = a.action.clone();
            let step = self.act_one(session, a).await.unwrap_or_else(Output::error);
            let failed = if step.is_error { " failed" } else { "" };
            lines.push(format!("{}. {action}{failed}: {}", i + 1, step.text));
            out.image_b64 = step.image_b64;
            out.secrets.extend(step.secrets);
            if step.is_error {
                out.is_error = true;
                if i + 1 < total {
                    lines.push(format!(
                        "the remaining {} actions did not run",
                        total - i - 1
                    ));
                }
                break;
            }
        }
        if out.image_b64.is_none() {
            out.image_b64 = self.screenshot(session, list.max_edge).await?;
        }
        out.text = lines.join("\n");
        Ok(out)
    }

    /// Runs one action: a launch, or an input action.
    async fn act_one(&self, session: &str, a: ActArgs) -> Result<Output, String> {
        if a.action == "launch_app" {
            return self.launch(session, &a).await;
        }
        self.act_input(session, a).await
    }

    async fn act_input(&self, session: &str, mut a: ActArgs) -> Result<Output, String> {
        if a.action == "left_click" {
            "click".clone_into(&mut a.action);
        }
        driver_keys(&mut a, self.os)?;
        let screen = self.shm(|g| Ok(g.size()));
        if let Some(e) = screen.and_then(|s| off_screen(&a, s, self.mapped(session, a.scale))) {
            return Err(e);
        }
        if a.action == "wait" {
            let secs = a.seconds.unwrap_or(1.0).clamp(0.1, MAX_WAIT_SECS);
            tokio::time::sleep(Duration::from_secs_f64(secs)).await;
            return Ok(Output {
                text: format!("waited {secs:.1}s"),
                is_error: false,
                image_b64: self.screenshot(session, a.max_edge).await?,
                ..Output::default()
            });
        }
        if a.action == "type"
            && (a.text.as_deref().is_some_and(|t| !t.is_ascii())
                || (a.element.is_none() && a.x.is_some() && a.y.is_some())
                || self.web_pixel(session, &a).await)
        {
            return self.type_mixed(session, &a).await;
        }
        let (tool, mut req) = self.action_request(session, &a).await?;
        let line = req["text"]
            .as_str()
            .and_then(before_newline)
            .map(str::to_owned);
        let enter = tool == "type_text"
            && req.get("element_token").is_none()
            && line.is_some()
            && self.to_dialog(&req);
        if enter && let Some(line) = line {
            req["text"] = line.into();
        }
        let input: Turn = Arc::new(self.input.clone().lock_owned().await);
        let before = self.before_input(Some(&a)).await;
        let once = self.once(&req);
        let target = once.as_ref().map_or(session, Owned::name);
        let typed = enter.then(|| req.clone());
        let pressed = req.get("element_token").is_some().then(|| req.clone());
        let mut result = self.send(target, tool, req, enter, &input).await;
        if let (Some(pressed), Err(e)) = (pressed, &result)
            && e.contains(AX_UNSUPPORTED)
            && let Some(retry) = self.unpressable(session, &a, pressed).await
        {
            result = self.request(target, "click", retry).await;
        }
        if let Some(typed) = typed
            && result.is_ok()
        {
            result = self.press_return(target, &typed).await.and(result);
        }
        self.end_once(once).await;
        let k = self.mapped(session, a.scale);
        let changes = self.settle(before, k).await;
        let shot = self.screenshot(session, a.max_edge).await?;
        drop(input);
        let (text, is_error) = match result {
            Ok(v) => (shot_points(&text_of(&v), k) + &changes, false),
            Err(e) => (e, true),
        };
        Ok(Output {
            text,
            is_error,
            image_b64: shot,
            ..Output::default()
        })
    }

    /// `screenshot`: the whole desktop, or `region`, a part of it at full resolution, with the image's
    /// size and input tokens in the text.
    async fn look(&self, args: &Value) -> Result<Output, String> {
        let max_edge = args["max_edge"]
            .as_u64()
            .and_then(|e| u32::try_from(e).ok());
        let session = args["session"].as_str();
        let shared = args["workspace_only"] == true;
        if let Some(region) = args.get("region").filter(|r| !r.is_null()) {
            return self.zoom(session, region, max_edge).await;
        }
        let image_b64 = match session {
            Some(session) => {
                self.touch();
                self.capture(session, max_edge, shared).await?.0
            }
            None => self.screenshot_once(max_edge).await?,
        };
        let text = match &image_b64 {
            Some(b64) => format!("screenshot of the whole screen: {}", weigh(b64)),
            None => "no screenshot returned".to_owned(),
        };
        Ok(Output {
            text,
            is_error: false,
            image_b64,
            ..Output::default()
        })
    }

    /// `region` {x, y, w, h} in the pixels of `session`'s last screenshot, at full resolution unless `max_edge` is smaller.
    async fn zoom(
        &self,
        session: Option<&str>,
        region: &Value,
        max_edge: Option<u32>,
    ) -> Result<Output, String> {
        let k = session.map_or(1.0, |s| self.scale_of(s));
        let [Some(x), Some(y), Some(w), Some(h)] = ["x", "y", "w", "h"].map(|f| region[f].as_f64())
        else {
            return Err("region needs x, y, w and h".to_owned());
        };
        let area = [x * k, y * k, w * k, h * k];
        let Cut {
            rect,
            width: rw,
            height: rh,
            rgb,
            per_unit: ps,
        } = match self.region_rgb(area) {
            Some(cut) => cut?,
            None => {
                self.captured_region(session.unwrap_or(DEFAULT_SESSION), area)
                    .await?
            }
        };
        let (iw, ih, rgb) = match max_edge {
            Some(edge) if edge > 0 && edge < rw.max(rh) => fit(rw, rh, &rgb, edge),
            _ => (rw, rh, rgb),
        };
        let image = B64.encode(encode(iw, ih, &rgb).ok_or("encode the region")?);
        let f = f64::from(rw) / f64::from(iw) / ps / k;
        let (ox, oy) = (f64::from(rect[0]) / k, f64::from(rect[1]) / k);
        Ok(Output {
            text: format!(
                "region of the screen: {}; its pixel px,py is x = {ox:.0} + px*{f:.2}, y = {oy:.0} + py*{f:.2} for computer_act",
                weigh(&image)
            ),
            is_error: false,
            image_b64: Some(image),
            ..Output::default()
        })
    }

    /// The modal dialogs that are open where the screen does not show them.
    fn modals(&self) -> Vec<Modal> {
        self.shm(Grabber::modals).unwrap_or_default()
    }

    /// Base64 PNG of the whole desktop, scaled so its long edge is at most `max_edge`.
    async fn screenshot(
        &self,
        session: &str,
        max_edge: Option<u32>,
    ) -> Result<Option<String>, String> {
        self.touch();
        Ok(self.capture(session, max_edge, false).await?.0)
    }

    /// Screenshot in a one-shot driver session that is ended right away.
    async fn screenshot_once(&self, max_edge: Option<u32>) -> Result<Option<String>, String> {
        let session = self.own("shot");
        let shot = self.screenshot(session.name(), max_edge).await;
        self.scales().remove(session.name());
        self.end_own(session).await?;
        shot
    }

    /// Ends a driver session, releasing its input devices.
    async fn end(&self, args: Value) -> Result<String, String> {
        let session = session_of(&args);
        self.request(&session, "end_session", json!({})).await?;
        self.last.lock().await.remove(&session);
        self.scales().remove(&session);
        lock(&self.unmapped).remove(&session);
        Ok(format!("ended session {session}"))
    }

    /// Captures the desktop; the driver maps desktop-scope pixels of this capture back to the screen.
    /// A capture over MIT-SHM is shrunk to `max_edge` here and also returns the full-size pixels.
    async fn capture(
        &self,
        session: &str,
        max_edge: Option<u32>,
        shared: bool,
    ) -> Result<(Option<String>, f64, Option<Screen>), String> {
        let max_edge = max_edge.or(Some(DEFAULT_EDGE));
        if let Some((w, h, rgb)) = self
            .shm(|g| (!g.overlay_shown()?).then(|| g.rgb()).transpose())
            .flatten()
        {
            let image = match max_edge {
                Some(edge) if edge > 0 && edge < w.max(h) => {
                    let (tw, th, small) = fit(w, h, &rgb, edge);
                    encode(tw, th, &small).map(|i| (i, f64::from(w) / f64::from(tw)))
                }
                _ => encode(w, h, &rgb).map(|i| (i, 1.0)),
            };
            if let Some((image, scale)) = image {
                self.keep_scale(session, scale, shared);
                let screen = Screen {
                    width: w,
                    height: h,
                    rgb,
                };
                return Ok((Some(B64.encode(image)), scale, Some(screen)));
            }
        }
        let mut args = json!({});
        if let Some(edge) = max_edge {
            args["max_image_dimension"] = edge.into();
        }
        let v = self.request(session, "get_desktop_state", args).await?;
        let state = &v["structuredContent"];
        let scale = state["frame_scale"]
            .as_f64()
            .or_else(|| Some(state["screen_width"].as_f64()? / state["screenshot_width"].as_f64()?))
            .filter(|s| s.is_finite() && *s > 0.0)
            .unwrap_or(1.0);
        let capped = state.get("screenshot_original_width").is_some()
            || state["frame_scale"].as_f64().is_some_and(|s| s != 1.0);
        self.keep_backing(state);
        let mut image = image_of(&v).map(str::to_owned);
        if let Some(smaller) = image.as_deref().and_then(shrink) {
            image = Some(smaller);
        }
        // the driver rescales later desktop clicks by its last capture; a full-size one keeps it at 1
        if capped && let Err(e) = self.full_capture(session).await {
            lock(&self.unmapped).insert(session.to_owned());
            return Err(format!("the screenshot's click mapping is unknown: {e}"));
        }
        lock(&self.unmapped).remove(session);
        self.keep_scale(session, scale, shared);
        Ok((image, scale, None))
    }

    /// Types text with non-ASCII characters or at a point, which a click focuses first, in the calls [`text_runs`] cuts.
    async fn type_mixed(&self, session: &str, a: &ActArgs) -> Result<Output, String> {
        let full = a.text.as_deref().unwrap_or_default();
        let input = self.input.lock().await;
        let before = self.before_input(Some(a)).await;
        let mut failure = None;
        if a.element.is_some() || (a.x.is_some() && a.y.is_some()) {
            let focus = ActArgs {
                action: "click".to_owned(),
                element: a.element,
                x: a.x,
                y: a.y,
                foreground: a.foreground,
                scale: a.scale,
                ..ActArgs::default()
            };
            let (tool, req) = self.action_request(session, &focus).await?;
            let once = self.once(&req);
            failure = self
                .request(once.as_ref().map_or(session, Owned::name), tool, req)
                .await
                .err();
            self.end_once(once).await;
        }
        let mut base = json!({});
        if let Some((pid, window_id)) = self.key_window(session).await? {
            if !a.foreground && self.keyboard(pid, window_id) {
                self.on_desktop(&mut base);
            } else {
                base["pid"] = pid.into();
                base["window_id"] = window_id.into();
            }
        } else {
            failure = failure.or_else(|| Some(NO_TEXT_TARGET.to_owned()));
        }
        if a.foreground {
            base["delivery_mode"] = "foreground".into();
        }
        let text = before_newline(full)
            .filter(|_| self.to_dialog(&base))
            .unwrap_or(full);
        let runs = text_runs(text);
        let mut typed = 0;
        let once = self.once(&base);
        for run in &runs {
            if failure.is_some() {
                break;
            }
            let mut req = base.clone();
            req["text"] = (*run).into();
            match self
                .call(once.as_ref().map_or(session, Owned::name), "type_text", req)
                .await
            {
                Ok(v) if v["isError"] == true || text_of(&v).contains(SKIPPED) => {
                    typed += landed(run, &v);
                    failure = Some(format!("type_text: {}", text_of(&v)));
                }
                Ok(_) => typed += run.chars().count(),
                Err(e) => failure = Some(e),
            }
        }
        if failure.is_none() && text.len() < full.len() {
            match self
                .press_return(once.as_ref().map_or(session, Owned::name), &base)
                .await
            {
                Ok(_) => typed += 1,
                Err(e) => failure = Some(e),
            }
        }
        self.end_once(once).await;
        let changes = self.settle(before, self.mapped(session, a.scale)).await;
        let shot = self.screenshot(session, a.max_edge).await?;
        drop(input);
        let total = full.chars().count();
        let summary =
            format!("typed {typed} of {total} characters; verify on the screenshot{changes}");
        Ok(Output {
            text: match &failure {
                Some(e) => format!("{summary}; stopped: {e}"),
                None => summary,
            },
            is_error: failure.is_some(),
            image_b64: shot,
            ..Output::default()
        })
    }

    /// Launches an app, or brings its existing window to the front when it already runs.
    async fn launch(&self, session: &str, a: &ActArgs) -> Result<Output, String> {
        let app = required(&a.app, "app")?;
        let input = self.input.lock().await;
        let before = self.before_input(None).await;
        let launched = match self.app_windows(session, app).await?.first() {
            Some(w) => {
                let req = json!({"pid": w.pid, "window_id": w.window_id});
                Ok(match self.request(session, "bring_to_front", req).await {
                    Ok(_) => format!(
                        "{app} is already running; brought its window {} \"{}\" (pid {}) to the front",
                        w.window_id, w.title, w.pid
                    ),
                    Err(e) => format!(
                        "{app} is already running as window {} \"{}\" (pid {}); raising it failed: {e}",
                        w.window_id, w.title, w.pid
                    ),
                })
            }
            None => match self
                .request(session, "launch_app", json!({"name": app}))
                .await
            {
                Ok(v) => Ok(text_of(&v)),
                Err(e) => match self.app_windows(session, app).await?.first() {
                    Some(w) => Ok(format!(
                        "{app} is running as window {} \"{}\" (pid {}); the launcher reported: {e}",
                        w.window_id, w.title, w.pid
                    )),
                    None => Err(e),
                },
            },
        };
        let text = match launched {
            Ok(text) => text,
            Err(e) => {
                self.end_volatile();
                if let Some(probe) = before.and_then(|b| b.probe) {
                    let _ = self.end_own(probe).await;
                }
                drop(input);
                return Ok(Output {
                    text: e,
                    is_error: true,
                    image_b64: self.screenshot(session, a.max_edge).await?,
                    ..Output::default()
                });
            }
        };
        let changes = self.settle(before, self.mapped(session, a.scale)).await;
        let shot = self.screenshot(session, a.max_edge).await?;
        drop(input);
        Ok(Output {
            text: text + &changes,
            is_error: false,
            image_b64: shot,
            ..Output::default()
        })
    }

    /// The screen before an input action: MIT-SHM pixels and block hashes with each block's past, else a thumbnail
    /// from a probe session that leaves the action session's capture context alone. A commit action `a` (see [`commits`]) in Chrome may load a page.
    async fn before_input(&self, a: Option<&ActArgs>) -> Option<Before> {
        let commit = a.is_some_and(commits);
        if let Some((pixels, blocks, windows, active, page, wait)) = self.shm(|g| {
            let active = g.active();
            let page = commit && active.is_some_and(|w| g.class(w) == CHROME_CLASS);
            let confirm = a.is_some_and(|a| {
                confirms(a) || (clicks_left(a) && active.is_some_and(|w| g.dialog(w)))
            });
            let (width, height) = g.size();
            let pixels = g.pixels()?.to_vec();
            let blocks = xshm::block_hashes(&pixels, width, height, &g.docks());
            let wait = if confirm {
                COMMIT_FIRST
            } else if page || a.is_none() {
                SETTLE
            } else {
                NO_CHANGE
            };
            Ok((pixels, blocks, g.mapped()?, active, page, wait))
        }) {
            let volatile = self.volatile.get_or_init(Volatile::start).begin(&blocks);
            return Some(Before {
                probe: None,
                frame: Frame::Blocks(blocks),
                pixels,
                volatile,
                windows,
                active,
                page,
                wait,
            });
        }
        let probe = self.own("still");
        match self.frame(probe.name()).await {
            Some(frame) => Some(Before {
                probe: Some(probe),
                frame: Frame::Thumb(frame),
                pixels: Vec::new(),
                volatile: Vec::new(),
                windows: Vec::new(),
                active: None,
                page: false,
                wait: SETTLE,
            }),
            None => {
                let _ = self.end_own(probe).await;
                None
            }
        }
    }

    /// Waits until the screen differs from `before` and holds still, within the wait [`Before`] carries; blocks that churn or toggle (see [`Block`], [`Shown`]) and changes under [`VISIBLE_STEP`] do not count.
    /// Returns lines naming the windows the action opened, closed or brought to the front.
    async fn settle(&self, before: Option<Before>, scale: f64) -> String {
        let Some(Before {
            probe,
            frame: first,
            pixels,
            mut volatile,
            windows,
            active,
            page,
            wait,
        }) = before
        else {
            tokio::time::sleep(SETTLE).await;
            return String::new();
        };
        let poll = if probe.is_some() {
            STILL_POLL
        } else {
            SHM_POLL
        };
        let (first_hold, window_hold) = if page {
            (COMMIT_STILL_FOR, COMMIT_STILL_FOR)
        } else {
            (STILL_FOR, WINDOW_STILL_FOR)
        };
        let start = Instant::now();
        let mut deadline = start + wait;
        let (mut last, mut since): (Option<Frame>, Instant) = (None, Instant::now());
        let (mut seen, mut focus, mut hold) = (windows.clone(), active, first_hold);
        let (mut still, mut changed) = (false, false);
        let (mut shown, mut prev) = (Shown::default(), None::<Vec<u64>>);
        while !still && Instant::now() < deadline {
            let at = Instant::now();
            let mut nap = poll.min(deadline.saturating_duration_since(at));
            if changed {
                nap = nap.min((since + hold).saturating_duration_since(at));
            }
            tokio::time::sleep(nap).await;
            let polled = match probe.as_ref().map(Owned::name) {
                Some(probe) => self.frame(probe).await.map(|f| {
                    let f = Frame::Thumb(f);
                    let differs = first.changes(&f, &volatile);
                    (f, differs, seen.clone(), focus)
                }),
                None => self.shm(|g| {
                    let (width, height) = g.size();
                    let docks = g.docks();
                    let current = g.pixels()?;
                    let blocks = xshm::block_hashes(current, width, height, &docks);
                    let differs = match &first {
                        Frame::Blocks(before) => {
                            before.iter().zip(&blocks).enumerate().any(|(i, (x, y))| {
                                x != y
                                    && volatile.get(i).is_none_or(|v| v.counts(*y))
                                    && xshm::visible(
                                        &pixels,
                                        current,
                                        width,
                                        height,
                                        i,
                                        VISIBLE_STEP,
                                    )
                            })
                        }
                        Frame::Thumb(_) => true,
                    };
                    Ok((Frame::Blocks(blocks), differs, g.mapped()?, g.active()))
                }),
            };
            let Some((frame, differs, now, now_focus)) = polled else {
                tokio::time::sleep_until(deadline).await;
                break;
            };
            if now != seen || now_focus != focus {
                (seen, focus, hold) = (now, now_focus, window_hold);
                since = Instant::now();
            }
            if differs && !changed {
                deadline = deadline.max((Instant::now() + SETTLE).min(start + COMMIT_SETTLE));
            }
            changed = differs;
            let moved = last.as_ref().is_none_or(|l| l.changes(&frame, &volatile));
            if let Frame::Blocks(blocks) = &frame {
                if let (Frame::Blocks(first), Some(prev)) = (&first, prev.as_deref()) {
                    shown.learn(first, prev, blocks, &mut volatile);
                }
                prev = Some(blocks.clone());
            }
            if moved {
                since = Instant::now();
                last = Some(frame);
            }
            if page && changed {
                deadline = start + COMMIT_SETTLE;
            }
            still = changed && since.elapsed() >= hold;
        }
        self.end_volatile();
        if let Some(probe) = probe {
            let _ = self.end_own(probe).await;
            return String::new();
        }
        if seen == windows && focus == active {
            return String::new();
        }
        self.shm(|g| Ok(self.window_report(g, &windows, &seen, active, focus, scale)))
            .unwrap_or_default()
    }

    /// The session of `agent` when its call names none; every default takes a new name once one is ended, since the driver refuses an ended name.
    fn default_session(&self, agent: &str) -> String {
        let mut name = DEFAULT_SESSION.to_owned();
        if !agent.is_empty() {
            name.push('-');
            name.extend(
                agent
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }),
            );
        }
        match self.defaults.load(Ordering::Relaxed) {
            0 => name,
            n => format!("{name}-{n}"),
        }
    }

    /// A driver session of its own for one step, named uniquely in this process.
    fn own(&self, prefix: &str) -> Owned {
        let n = self.ids.fetch_add(1, Ordering::Relaxed);
        Owned {
            driver: Arc::clone(&self.driver),
            name: format!("{prefix}-{}-{n}", std::process::id()),
            open: true,
        }
    }

    /// Ends the driver session `session`, releasing its input devices.
    async fn end_own(&self, mut session: Owned) -> Result<Value, String> {
        let done = self.request(&session.name, "end_session", json!({})).await;
        session.open = false;
        done
    }

    fn touch(&self) {
        self.volatile.get_or_init(Volatile::start).touch();
    }

    fn end_volatile(&self) {
        if let Some(v) = self.volatile.get() {
            v.end();
        }
    }

    /// The windows that opened, closed or moved between `before` and `after`, and a new front window.
    fn window_report(
        &self,
        g: &Grabber,
        before: &[Placement],
        after: &[Placement],
        was_active: Option<u32>,
        active: Option<u32>,
        scale: f64,
    ) -> String {
        let mut titles = lock(&self.titles);
        let mut out = String::new();
        let mut opened = Vec::new();
        for p in after {
            let old = before.iter().find(|b| b.id == p.id);
            if p.popup {
                if old.is_none() {
                    out.push_str("\nmenu opened");
                }
                continue;
            }
            match old {
                None => {
                    let title = g.title(p.id);
                    let _ = write!(
                        out,
                        "\nopened window \"{title}\" {:.0}x{:.0} at ({:.0},{:.0})",
                        f64::from(p.width) / scale,
                        f64::from(p.height) / scale,
                        f64::from(p.x) / scale,
                        f64::from(p.y) / scale
                    );
                    opened.push(title.clone());
                    titles.insert(p.id, title);
                }
                Some(b) if b != p => {
                    let title = titles.get(&p.id).cloned().unwrap_or_else(|| g.title(p.id));
                    let _ = write!(
                        out,
                        "\nwindow \"{title}\" is now {:.0}x{:.0} at ({:.0},{:.0})",
                        f64::from(p.width) / scale,
                        f64::from(p.height) / scale,
                        f64::from(p.x) / scale,
                        f64::from(p.y) / scale
                    );
                }
                Some(_) => {}
            }
        }
        for b in before
            .iter()
            .filter(|b| !after.iter().any(|p| p.id == b.id))
        {
            match (b.popup, titles.remove(&b.id)) {
                (true, _) => out.push_str("\nmenu closed"),
                (false, Some(title)) => {
                    let _ = write!(out, "\nclosed window \"{title}\"");
                }
                (false, None) => out.push_str("\nclosed a window"),
            }
        }
        if active != was_active
            && let Some(w) = active
        {
            let title = g.title(w);
            if !title.is_empty() && !opened.contains(&title) {
                let _ = write!(out, "\nfront window is now \"{title}\"");
            }
        }
        out
    }

    /// A thumbnail from the probe driver session `probe`.
    async fn frame(&self, probe: &str) -> Option<Vec<u8>> {
        let edge = json!({"max_image_dimension": STILL_EDGE});
        let v = self.request(probe, "get_desktop_state", edge).await.ok()?;
        let data = B64.decode(image_of(&v)?).ok()?;
        Some(decode_png(&data)?.1)
    }

    fn scales(&self) -> std::sync::MutexGuard<'_, HashMap<String, f64>> {
        lock(&self.scales)
    }

    fn last_scale(&self) -> std::sync::MutexGuard<'_, f64> {
        lock(&self.last_scale)
    }

    fn keep_scale(&self, session: &str, scale: f64, shared: bool) {
        self.scales().insert(session.to_owned(), scale);
        if !shared {
            *self.last_scale() = scale;
        }
    }

    /// Screen pixels per pixel of the last screenshot of `session`, else of any session, so its x,y map to the screen.
    fn scale_of(&self, session: &str) -> f64 {
        let own = self.scales().get(session).copied();
        own.unwrap_or_else(|| *self.last_scale())
    }

    /// Runs `read` on the MIT-SHM grabber, opening it when needed; `None` when it is unavailable or fails.
    fn shm<T>(&self, read: impl FnOnce(&Grabber) -> Result<T, String>) -> Option<T> {
        let mut slot = lock(&self.grabber);
        if slot.is_none() {
            match Grabber::open() {
                Ok(g) => *slot = Some(g),
                Err(e) => {
                    eprintln!("desk: MIT-SHM capture unavailable, using cua-driver: {e}");
                    return None;
                }
            }
        }
        match read(slot.as_ref()?) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("desk: MIT-SHM capture: {e}");
                *slot = None;
                None
            }
        }
    }

    /// On-screen windows of `app`, frontmost first, matched on the app name case-insensitively.
    async fn app_windows(&self, session: &str, app: &str) -> Result<Vec<Window>, String> {
        let windows = self
            .request(session, "list_windows", json!({"on_screen_only": true}))
            .await?;
        let wanted = app.to_lowercase();
        Ok(window_list(&windows)
            .into_iter()
            .filter(|w| {
                let name = w.app.to_lowercase();
                !name.is_empty()
                    && (name == wanted || name.contains(&wanted) || wanted.contains(&name))
            })
            .collect())
    }

    /// The frontmost application window, skipping the desktop and panels.
    async fn app_window(&self, session: &str) -> Result<Option<(i64, i64)>, String> {
        let windows = self
            .request(session, "list_windows", json!({"on_screen_only": true}))
            .await?;
        Ok(self
            .app_of(&window_list(&windows))
            .map(|w| (w.pid, w.window_id)))
    }

    /// The first of `listed` that is no dock or desktop window by its X window type.
    fn app_of<'a>(&self, listed: &'a [Window]) -> Option<&'a Window> {
        let shell = self.shm(|g| Ok(g.shell_windows())).unwrap_or_default();
        listed
            .iter()
            .find(|w| !u32::try_from(w.window_id).is_ok_and(|id| shell.contains(&id)))
    }

    /// [`Self::app_window`] once a window manager runs; while none does, as for a while after a desktop
    /// start, waits for one at most [`WM_WAIT`], then fails.
    async fn key_window(&self, session: &str) -> Result<Option<(i64, i64)>, String> {
        if let Some(window) = self.app_window(session).await? {
            return Ok(Some(window));
        }
        let (start, mut waited) = (Instant::now(), false);
        while !self.shm(Grabber::wm).unwrap_or(true) {
            if start.elapsed() >= WM_WAIT {
                return Err(format!(
                    "no window manager after {} s: the desktop is still starting; try again",
                    WM_WAIT.as_secs()
                ));
            }
            waited = true;
            tokio::time::sleep(WM_POLL).await;
        }
        if !waited {
            return Ok(None);
        }
        self.app_window(session).await
    }

    async fn action_request(
        &self,
        session: &str,
        a: &ActArgs,
    ) -> Result<(&'static str, Value), String> {
        let clicks = matches!(
            a.action.as_str(),
            "click" | "double_click" | "right_click" | "triple_click"
        );
        let pixel = match a.element {
            Some(index) if clicks && !a.foreground => {
                match self.element_pixel(session, index, false).await {
                    None if presses(a) => match self.open_menus(session, index).await {
                        None => self.hidden_press(session, index).await?,
                        pixel => pixel,
                    },
                    pixel => pixel,
                }
            }
            _ => None,
        };
        let mut req = json!({});
        let tool = match a.action.as_str() {
            "triple_click" => {
                req["count"] = 3.into();
                "click"
            }
            "click" | "double_click" | "right_click" => {
                let tool = match (a.action.as_str(), a.element.is_some() && pixel.is_none()) {
                    ("double_click", true) => "double_click",
                    ("right_click", true) => "right_click",
                    ("double_click", false) => {
                        req["count"] = 2.into();
                        "click"
                    }
                    ("right_click", false) => {
                        req["button"] = "right".into();
                        "click"
                    }
                    _ => "click",
                };
                if let Some(b) = a
                    .button
                    .as_deref()
                    .filter(|_| tool == "click" && a.action != "right_click")
                {
                    req["button"] = b.into();
                }
                tool
            }
            "type" => {
                req["text"] = required(&a.text, "text")?.into();
                "type_text"
            }
            "key" => {
                req["key"] = required(&a.key, "key")?.into();
                "press_key"
            }
            "hotkey" => {
                let keys = a.keys.clone().ok_or("hotkey needs keys")?;
                req["keys"] = keys.into();
                "hotkey"
            }
            "scroll" => {
                req["direction"] = required(&a.direction, "direction")?.into();
                req["amount"] = a.amount.unwrap_or(3).into();
                "scroll"
            }
            "drag" => {
                let (Some(x), Some(y), Some(tx), Some(ty)) = (a.x, a.y, a.to_x, a.to_y) else {
                    return Err("drag needs x, y, to_x and to_y".to_owned());
                };
                let k = self.mapped(session, a.scale);
                let (x, y, tx, ty) = (x * k, y * k, tx * k, ty * k);
                req = json!({"from_x": x, "from_y": y, "to_x": tx, "to_y": ty});
                self.on_desktop(&mut req);
                return Ok(("drag", req));
            }
            other => return Err(format!("unknown action {other}")),
        };
        let mut desktop = false;
        if let Some((x, y)) = pixel {
            req["x"] = x.into();
            req["y"] = y.into();
            desktop = true;
        } else if let Some(index) = a.element {
            let last = self.last.lock().await;
            let snap = last
                .get(session)
                .ok_or("no element list yet; call computer_state first")?;
            let element = snap.elements.get(&index).ok_or_else(|| {
                format!(
                    "element {index} is not in the latest computer_state; call computer_state again"
                )
            })?;
            req["pid"] = snap.pid.into();
            req["element_token"] = element.token.as_str().into();
            if let Some(id) = snap.window_id {
                req["window_id"] = id.into();
            }
        } else if let (Some(x), Some(y)) = (a.x, a.y) {
            let k = self.mapped(session, a.scale);
            req["x"] = (x * k).into();
            req["y"] = (y * k).into();
            desktop = true;
        } else if clicks {
            return Err(format!("{} needs element or x and y", a.action));
        } else if let Some((pid, window_id)) = self.key_window(session).await? {
            if tool != "scroll" && !a.foreground && self.keyboard(pid, window_id) {
                desktop = true;
            } else {
                req["pid"] = pid.into();
                req["window_id"] = window_id.into();
            }
        } else if tool == "type_text" {
            return Err(NO_TEXT_TARGET.to_owned());
        } else if tool != "scroll" && !a.foreground {
            desktop = true;
        }
        if desktop {
            self.on_desktop(&mut req);
        }
        if a.foreground {
            req["delivery_mode"] = "foreground".into();
        }
        Ok((tool, req))
    }

    async fn send(
        &self,
        session: &str,
        tool: &str,
        req: Value,
        enter: bool,
        input: &Turn,
    ) -> Result<Value, String> {
        if tool == "type_text"
            && !enter
            && req["target"]["kind"] == "desktop"
            && self
                .xtest(req["text"].as_str().unwrap_or_default(), input.clone())
                .await?
        {
            return Ok(json!({"content": [{"type": "text", "text": XTEST_TYPED}]}));
        }
        self.request(session, tool, req).await
    }

    /// Whether the typer typed `text`; it runs on a blocking thread that holds the input turn to its end and stops at its next key once this call is dropped.
    async fn xtest(&self, text: &str, turn: Turn) -> Result<bool, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let _cancel = Cancel(stop.clone());
        let (text, typer) = (text.to_owned(), self.typer);
        tokio::task::spawn_blocking(move || {
            let _turn = turn;
            typer(&text, &stop)
        })
        .await
        .map_err(|e| format!("XTest: {e}"))?
    }

    /// A desktop click at the centre of the element `a` names, when the topmost window there is the element's own.
    async fn unpressable(&self, session: &str, a: &ActArgs, mut req: Value) -> Option<Value> {
        let (window, x, y) = {
            let last = self.last.lock().await;
            let snap = last.get(session)?;
            let [x, y, w, h] = snap.elements.get(&a.element?)?.frame?;
            (snap.window_id?, x + w / 2.0, y + h / 2.0)
        };
        let (_, top) = (self.under)(x, y)?;
        if top != window {
            return None;
        }
        let req = req.as_object_mut()?;
        for key in ["element_token", "pid", "window_id"] {
            req.remove(key);
        }
        match a.action.as_str() {
            "double_click" => req.insert("count".to_owned(), 2.into()),
            "right_click" => req.insert("button".to_owned(), "right".into()),
            _ => None,
        };
        req.insert("x".to_owned(), x.into());
        req.insert("y".to_owned(), y.into());
        let mut req = Value::Object(std::mem::take(req));
        self.on_desktop(&mut req);
        Some(req)
    }

    fn to_dialog(&self, req: &Value) -> bool {
        let window = req["window_id"]
            .as_u64()
            .and_then(|w| u32::try_from(w).ok());
        self.shm(|g| Ok(window.or_else(|| g.active()).is_some_and(|w| g.dialog(w))))
            .unwrap_or(true)
    }

    async fn press_return(&self, session: &str, typed: &Value) -> Result<Value, String> {
        tokio::time::sleep(RETURN_GAP).await;
        let mut req = typed.clone();
        if let Some(fields) = req.as_object_mut() {
            for k in ["text", "x", "y"] {
                fields.remove(k);
            }
        }
        req["key"] = "return".into();
        self.request(session, "press_key", req).await
    }

    /// A one-call driver session for the pid-addressed input `req`, ended by [`Self::end_once`]: its virtual master
    /// keyboard would outlive the call, and while one exists Chrome drops core keyboard input from XTest and VNC.
    fn once(&self, req: &Value) -> Option<Owned> {
        req.get("pid").is_some().then(|| self.own("input"))
    }

    async fn end_once(&self, once: Option<Owned>) {
        if let Some(session) = once {
            let _ = self.end_own(session).await;
        }
    }

    /// Whether keys for `window` of `pid` can go to the desktop, which skips the driver's
    /// 220 ms focus guard: on X11 the window is active and holds the input focus, and no popup of another app is open;
    /// off Linux always, since the key window is the frontmost and the driver's per-window keys miss XAML windows.
    fn keyboard(&self, pid: i64, window: i64) -> bool {
        let (Ok(pid), Ok(window)) = (u32::try_from(pid), u32::try_from(window)) else {
            return false;
        };
        self.shm(|g| g.keyboard(window, pid))
            .unwrap_or(!cfg!(target_os = "linux"))
    }

    /// Whether `a` types into a web page element of `session`'s snapshot that a pixel click can focus.
    async fn web_pixel(&self, session: &str, a: &ActArgs) -> bool {
        let Some(index) = a.element.filter(|_| !a.foreground) else {
            return false;
        };
        let web = self
            .last
            .lock()
            .await
            .get(session)
            .and_then(|s| s.elements.get(&index))
            .is_some_and(|e| e.web);
        web && self.element_pixel(session, index, false).await.is_some()
    }

    /// The point where a pixel click reaches element `index` of `session`'s snapshot, skipping the driver's 270 ms click and an AT-SPI dispatch a modal dialog deadlocks;
    /// none unless it looks as in the snapshot (a `menu` to open need not) under its active window or an own popup, and none for LibreOffice outside menus.
    async fn element_pixel(&self, session: &str, index: i64, menu: bool) -> Option<(f64, f64)> {
        let last = self.last.lock().await;
        let snap = last.get(session)?;
        let (element, screen) = (snap.elements.get(&index)?, snap.screen.as_ref()?);
        if !menu
            && !element.role.starts_with("menu")
            && u32::try_from(snap.pid).is_ok_and(libreoffice)
        {
            return None;
        }
        let [x, y, w, h] = element.frame?;
        let (cx, cy) = ((x + w / 2.0).floor(), (y + h / 2.0).floor());
        let rect = clip([x, y, w, h], screen.size())?;
        let (pid, window) = (
            u32::try_from(snap.pid).ok()?,
            u32::try_from(snap.window_id?).ok()?,
        );
        let reached = self.shm(|g| {
            let Some(top) = g.top_at(cx as i32, cy as i32)? else {
                return Ok(false);
            };
            let reachable = if top.popup {
                let role = element.role.to_ascii_lowercase();
                g.pid(top.id) == Some(pid)
                    && POPUP_ROWS.iter().any(|r| role.contains(r))
                    && top.holds(rect)
            } else {
                g.active() == Some(window) && g.top_level(window)? == top.id && !g.grabbed(true)?
            };
            Ok(reachable && (menu || !moved(screen, rect, &g.region(rect)?)))
        });
        reached.unwrap_or(false).then_some((cx, cy))
    }

    /// Opens the menus that hold menu item `index` of `session`'s snapshot, outermost first, with pixel clicks while no
    /// popup of its app is open, and returns the item's click point.
    async fn open_menus(&self, session: &str, index: i64) -> Option<(f64, f64)> {
        let (pid, menus) = {
            let last = self.last.lock().await;
            let snap = last.get(session)?;
            let mut element = snap
                .elements
                .get(&index)
                .filter(|e| e.role == "menu item")?;
            let mut menus = Vec::new();
            while let Some((parent, menu)) = element
                .parent
                .and_then(|p| snap.elements.get(&p).map(|e| (p, e)))
                .filter(|(_, e)| e.role == "menu")
            {
                menus.push(parent);
                element = menu;
            }
            (snap.pid, menus)
        };
        let owner = u32::try_from(pid).ok()?;
        let open = self.shm(|g| {
            Ok(g.mapped()?
                .iter()
                .any(|p| p.popup && p.drawn && g.pid(p.id) == Some(owner)))
        });
        if menus.is_empty() || open != Some(false) {
            return None;
        }
        for menu in menus.into_iter().rev() {
            let (x, y) = self.poll_pixel(session, menu, true).await?;
            self.pixel_click(x, y).await.ok()?;
        }
        self.poll_pixel(session, index, false).await
    }

    /// The menu bar twin's point for an off-screen push button or menu item of a GTK app outside a web page, whose
    /// AT-SPI action can deadlock on a modal dialog; an error when none is found, `None` for any other element.
    async fn hidden_press(&self, session: &str, index: i64) -> Result<Option<(f64, f64)>, String> {
        let (pid, window_id, name, button, menus) = {
            let last = self.last.lock().await;
            let Some(snap) = last.get(session) else {
                return Ok(None);
            };
            let (Some(element), Some(screen)) = (snap.elements.get(&index), snap.screen.as_ref())
            else {
                return Ok(None);
            };
            let shown = element.frame.and_then(|f| clip(f, screen.size())).is_some();
            if shown || element.web || !PRESSED.contains(&element.role.as_str()) {
                return Ok(None);
            }
            let mut menus: Vec<(f64, i64)> = snap
                .elements
                .iter()
                .filter(|(_, e)| {
                    e.role == "menu"
                        && e.parent
                            .and_then(|p| snap.elements.get(&p))
                            .is_none_or(|p| p.role != "menu")
                })
                .filter_map(|(i, e)| Some((e.frame?[0], *i)))
                .collect();
            menus.sort_by(|a, b| a.0.total_cmp(&b.0));
            let menus: Vec<i64> = menus.into_iter().map(|(_, i)| i).collect();
            let button = element.role == "push button";
            (
                snap.pid,
                snap.window_id,
                label_key(&element.label),
                button,
                menus,
            )
        };
        if !gtk(pid) {
            return Ok(None);
        }
        if button && let Some(point) = self.menu_twin(session, pid, window_id, &name, &menus).await
        {
            return Ok(Some(point));
        }
        Err(format!(
            "element {index} is not on the screen; open its menu or use a visible control"
        ))
    }

    /// Opens the menu bar `menus` of `session`'s snapshot in turn and returns the point of the first
    /// shown menu item named `name` that a pixel click reaches; closes the menu when none is.
    async fn menu_twin(
        &self,
        session: &str,
        pid: i64,
        window_id: Option<i64>,
        name: &str,
        menus: &[i64],
    ) -> Option<(f64, f64)> {
        let owner = u32::try_from(pid).ok()?;
        let mut req = json!({"pid": pid, "include_screenshot": false, "max_elements": STATE_ELEMENTS, "tree_format": "elements"});
        if let Some(id) = window_id {
            req["window_id"] = id.into();
        }
        for (n, menu) in menus.iter().enumerate() {
            if n > 0 {
                self.escape().await;
            }
            let Some((x, y)) = self.poll_pixel(session, *menu, true).await else {
                continue;
            };
            if self.pixel_click(x, y).await.is_err() {
                continue;
            }
            let probe = self.own("menu");
            let state = self
                .request(probe.name(), "get_window_state", req.clone())
                .await;
            let _ = self.end_own(probe).await;
            let Ok(state) = state else {
                continue;
            };
            let frames = state["structuredContent"]["elements"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|e| e["role"] == "menu item")
                .filter(|e| e["label"].as_str().is_some_and(|l| label_key(l) == name))
                .filter_map(|e| {
                    let f = &e["frame"];
                    Some([
                        f["x"].as_f64()?,
                        f["y"].as_f64()?,
                        f["w"].as_f64()?,
                        f["h"].as_f64()?,
                    ])
                });
            for [fx, fy, fw, fh] in frames {
                let (cx, cy) = ((fx + fw / 2.0).floor(), (fy + fh / 2.0).floor());
                let rect = [fx, fy, fw, fh].map(|v| v.round() as i32);
                let reached = self.shm(|g| {
                    Ok(g.top_at(cx as i32, cy as i32)?
                        .is_some_and(|t| t.popup && g.pid(t.id) == Some(owner) && t.holds(rect)))
                });
                if reached == Some(true) {
                    return Some((cx, cy));
                }
            }
        }
        if !menus.is_empty() {
            self.escape().await;
        }
        None
    }

    /// A left click at the screen point `(x, y)`.
    async fn pixel_click(&self, x: f64, y: f64) -> Result<Value, String> {
        self.desktop_input("click", json!({"x": x, "y": y})).await
    }

    /// Escape to the focused window, which closes an open menu.
    async fn escape(&self) {
        let _ = self
            .desktop_input("press_key", json!({"key": "Escape"}))
            .await;
    }

    /// Whether the screen point `(x, y)` shows a window title: a desktop panel or a window manager's title bar.
    fn title_at(&self, x: f64, y: f64) -> bool {
        self.shm(|g| g.title_at(x.round() as i32, y.round() as i32))
            .unwrap_or(false)
    }

    /// The screen rectangle `[x, y, w, h]` clipped to the screen, and its RGB rows over MIT-SHM; none without MIT-SHM.
    fn region_rgb(&self, area: [f64; 4]) -> Option<Result<Cut, String>> {
        self.shm(|g| {
            let Some(rect) = clip(area, g.size()) else {
                return Ok(None);
            };
            Ok(Some(Cut {
                rect,
                width: rect[2].unsigned_abs(),
                height: rect[3].unsigned_abs(),
                rgb: g.region(rect)?,
                per_unit: 1.0,
            }))
        })
        .map(|cut| cut.ok_or_else(|| OFF_SCREEN.to_owned()))
    }

    /// The screen rectangle `area` clipped to the screen, cut from a full-size driver capture.
    async fn captured_region(&self, session: &str, area: [f64; 4]) -> Result<Cut, String> {
        let v = self
            .request(
                session,
                "get_desktop_state",
                json!({"max_image_dimension": 0}),
            )
            .await?;
        self.keep_backing(&v["structuredContent"]);
        let png = B64
            .decode(image_of(&v).ok_or("the capture has no image")?)
            .map_err(|e| format!("decode the capture: {e}"))?;
        let (info, data) = decode_png(&png).ok_or("decode the capture")?;
        let state = &v["structuredContent"];
        let screen = state["screen_width"]
            .as_f64()
            .zip(state["screen_height"].as_f64())
            .unwrap_or((f64::from(info.width), f64::from(info.height)));
        let ps = f64::from(info.width) / screen.0;
        let rect = clip(area, (screen.0 as usize, screen.1 as usize)).ok_or(OFF_SCREEN)?;
        let (width, height, rgb) = crop(
            &info,
            &data,
            rect.map(|v| (f64::from(v) * ps).round() as u32),
        )
        .ok_or("crop the capture")?;
        Ok(Cut {
            rect,
            width,
            height,
            rgb,
            per_unit: ps,
        })
    }

    /// The desktop-addressed input `req` of `tool` in a driver session of its own, whose x,y no capture maps.
    async fn desktop_input(&self, tool: &str, mut req: Value) -> Result<Value, String> {
        self.on_desktop(&mut req);
        let once = self.own("input");
        let done = self.request(once.name(), tool, req).await;
        self.end_once(Some(once)).await;
        done
    }

    fn on_desktop(&self, req: &mut Value) {
        if self.os == Os::Linux {
            req["target"] = json!({"kind": "desktop", "display_id": "primary"});
            return;
        }
        req["scope"] = "desktop".into();
        req["delivery_mode"] = "foreground".into();
        let backing = *lock(&self.backing);
        for key in ["x", "y", "from_x", "from_y", "to_x", "to_y"] {
            if let Some(v) = req[key].as_f64() {
                req[key] = (v * backing).into();
            }
        }
    }

    fn keep_backing(&self, state: &Value) {
        if self.os == Os::Mac
            && let Some(backing) = state["scale_factor"]
                .as_f64()
                .filter(|b| b.is_finite() && *b > 0.0)
        {
            *lock(&self.backing) = backing;
        }
    }

    /// [`Self::element_pixel`] once it answers, at most [`MENU_POLLS`] times [`MENU_POLL`] apart.
    async fn poll_pixel(&self, session: &str, index: i64, menu: bool) -> Option<(f64, f64)> {
        for _ in 0..MENU_POLLS {
            if let Some(point) = self.element_pixel(session, index, menu).await {
                return Some(point);
            }
            tokio::time::sleep(MENU_POLL).await;
        }
        None
    }

    async fn request(&self, session: &str, name: &str, args: Value) -> Result<Value, String> {
        if lock(&self.unmapped).contains(session) && desktop_point(&args) {
            self.full_capture(session).await.map_err(|e| {
                format!("{name}: the click mapping of session {session} is unknown: {e}")
            })?;
            lock(&self.unmapped).remove(session);
        }
        let point = match (args["x"].as_f64(), args["y"].as_f64()) {
            (Some(x), Some(y)) if name == "click" => Some((x, y)),
            _ => None,
        };
        let result = match point {
            Some(point) => {
                self.driver
                    .spaced(point, self.call(session, name, args))
                    .await
            }
            None => self.call(session, name, args).await,
        }?;
        if result["isError"] == true {
            return Err(format!("{name}: {}", text_of(&result)));
        }
        if name == "launch_app" {
            crate::desk::adopt(result.clone()).await;
        }
        Ok(result)
    }

    async fn full_capture(&self, session: &str) -> Result<(), String> {
        let full = json!({"max_image_dimension": 0});
        let v = self.call(session, "get_desktop_state", full).await?;
        if v["isError"] == true {
            return Err(format!("get_desktop_state: {}", text_of(&v)));
        }
        Ok(())
    }

    /// One driver call; a result the driver marks isError comes back as is.
    async fn call(&self, session: &str, name: &str, args: Value) -> Result<Value, String> {
        let retry = RETRIED.contains(&name).then(|| args.clone());
        let mut reply = self.exchange(session, name, args).await?;
        if let Some(args) = retry.filter(|_| reply.trim().is_empty()) {
            tokio::time::sleep(CONNECT_RETRY).await;
            reply = self.exchange(session, name, args).await?;
        }
        let mut v = serde_json::from_str::<Value>(&reply)
            .map_err(|e| format!("decode cua-driver reply: {e}"))?;
        if v["ok"] != true {
            return Err(format!(
                "{name}: {}",
                v["error"].as_str().unwrap_or("cua-driver call failed")
            ));
        }
        Ok(v["result"].take())
    }

    /// Sends one call and returns the raw reply line, empty when the driver closed without one.
    async fn exchange(&self, session: &str, name: &str, args: Value) -> Result<String, String> {
        let exchange = async {
            let mut stream = self.connect().await?;
            let mut line = json!({"method": "call", "name": name});
            line["args"] = args;
            line["args"]["session"] = session.into();
            let mut bytes = serde_json::to_vec(&line).map_err(|e| format!("encode: {e}"))?;
            bytes.push(b'\n');
            stream
                .write_all(&bytes)
                .await
                .map_err(|e| format!("write cua-driver: {e}"))?;
            let mut reply = String::new();
            BufReader::new(stream)
                .read_line(&mut reply)
                .await
                .map_err(|e| format!("read cua-driver: {e}"))?;
            Ok(reply)
        };
        tokio::time::timeout(REQUEST_TIMEOUT, exchange)
            .await
            .map_err(|_| format!("cua-driver {name} timed out"))?
    }

    /// Connects to the driver, waiting out the restart that each desktop session start triggers.
    async fn connect(&self) -> Result<Conn, String> {
        let deadline = tokio::time::Instant::now() + CONNECT_WAIT;
        loop {
            match crate::server::connect(self.driver.endpoint()).await {
                Ok(stream) => return Ok(stream),
                Err(e)
                    if matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound)
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(CONNECT_RETRY).await;
                }
                Err(e) => {
                    return Err(format!(
                        "connect cua-driver {}: {e}",
                        self.driver.endpoint()
                    ));
                }
            }
        }
    }
}

struct Before {
    probe: Option<Owned>,
    frame: Frame,
    /// The BGRX pixels of a [`Frame::Blocks`].
    pixels: Vec<u8>,
    /// Per block of a [`Frame::Blocks`], what it did before the action.
    volatile: Vec<Block>,
    /// The mapped top-level windows over MIT-SHM; a change holds the settle for [`WINDOW_STILL_FOR`].
    windows: Vec<Placement>,
    active: Option<u32>,
    /// A commit action in Chrome: the settle holds [`COMMIT_STILL_FOR`], at most [`COMMIT_SETTLE`].
    page: bool,
    wait: Duration,
}

/// A driver session for one step; one dropped while open is ended by a detached call.
struct Owned {
    driver: Arc<Driver>,
    name: String,
    open: bool,
}

impl Owned {
    fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.open {
            return;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (driver, session) = (Arc::clone(&self.driver), std::mem::take(&mut self.name));
        rt.spawn(async move {
            let _ = driver
                .send("end_session", &json!({"session": session}))
                .await;
        });
    }
}

/// A settle frame: block hashes over MIT-SHM, or a probe session's thumbnail.
enum Frame {
    Blocks(Vec<u64>),
    Thumb(Vec<u8>),
}

impl Frame {
    /// Whether `other` differs from this frame: in a block whose new contents count (see
    /// [`Block::counts`]), or past [`STILL_TOLERANCE`] between thumbnails.
    fn changes(&self, other: &Self, volatile: &[Block]) -> bool {
        match (self, other) {
            (Self::Blocks(a), Self::Blocks(b)) => {
                a.len() != b.len()
                    || a.iter()
                        .zip(b)
                        .enumerate()
                        .any(|(i, (x, y))| x != y && volatile.get(i).is_none_or(|v| v.counts(*y)))
            }
            (Self::Thumb(a), Self::Thumb(b)) => differs(a, b),
            _ => true,
        }
    }
}

struct Snapshot {
    pid: i64,
    window_id: Option<i64>,
    elements: HashMap<i64, Element>,
    /// The screen when the elements were read, to tell whether one has moved since.
    screen: Option<Screen>,
}

struct Element {
    token: String,
    role: String,
    label: String,
    parent: Option<i64>,
    /// The screen rectangle `[x, y, w, h]`.
    frame: Option<[f64; 4]>,
    /// Inside a web page, whose fields only take typing after a click.
    web: bool,
}

struct Screen {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

impl Screen {
    fn size(&self) -> (usize, usize) {
        (self.width as usize, self.height as usize)
    }
}

#[derive(Deserialize)]
struct StateArgs {
    #[serde(default)]
    pid: Option<i64>,
    #[serde(default)]
    workspace_only: bool,
    #[serde(default)]
    window_id: Option<i64>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    max_edge: Option<u32>,
}

#[derive(Default, Deserialize)]
struct ActArgs {
    action: String,
    #[serde(default)]
    element: Option<i64>,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    to_x: Option<f64>,
    #[serde(default)]
    to_y: Option<f64>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    keys: Option<Vec<String>>,
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    amount: Option<u32>,
    #[serde(default)]
    app: Option<String>,
    #[serde(default)]
    button: Option<String>,
    #[serde(default)]
    seconds: Option<f64>,
    #[serde(default)]
    foreground: bool,
    #[serde(default)]
    max_edge: Option<u32>,
    /// Screen pixels per pixel of the screenshot the caller saw, in place of the session's latest.
    #[serde(default)]
    scale: Option<f64>,
}

#[derive(Deserialize)]
struct PointArgs {
    x: f64,
    y: f64,
}

#[derive(Deserialize)]
struct ActList {
    actions: Vec<ActArgs>,
    #[serde(default)]
    max_edge: Option<u32>,
    #[serde(default)]
    scale: Option<f64>,
}

struct Window {
    pid: i64,
    window_id: i64,
    app: String,
    title: String,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    z: i64,
}

fn positive(scale: Option<f64>) -> Result<(), String> {
    match scale {
        Some(k) if !(k.is_finite() && k > 0.0) => {
            Err(format!("scale {k} is not a positive number"))
        }
        _ => Ok(()),
    }
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn before_newline(text: &str) -> Option<&str> {
    text.strip_suffix(['\n', '\r']).filter(|t| !t.is_empty())
}

/// How many characters of `run` a failed type_text call typed: all but the skipped ones on the
/// virtual-keyboard route, else the count its "typed N of" error names.
fn landed(run: &str, result: &Value) -> usize {
    if let Some(skipped) = result["structuredContent"]["skipped_characters"].as_str() {
        return run.chars().count().saturating_sub(skipped.chars().count());
    }
    text_of(result)
        .split("typed ")
        .nth(1)
        .and_then(|t| t.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Rewrites the keys of a `key` or `hotkey` action in the driver's names; a `key` that names a
/// combination such as `ctrl+l` becomes a `hotkey`, and a `hotkey` of one key a `key`.
fn driver_keys(a: &mut ActArgs, os: Os) -> Result<(), String> {
    match a.action.as_str() {
        "key" => {
            let mut keys = key_names(required(&a.key, "key")?, os)?;
            if keys.len() == 1 {
                a.key = keys.pop();
            } else {
                "hotkey".clone_into(&mut a.action);
                a.keys = Some(keys);
            }
        }
        "hotkey" => {
            let keys = a.keys.as_deref().ok_or("hotkey needs keys")?;
            let mut names = keys
                .iter()
                .map(|k| key_names(k, os))
                .collect::<Result<Vec<_>, _>>()?
                .concat();
            if names.len() == 1 {
                "key".clone_into(&mut a.action);
                a.key = names.pop();
            } else {
                a.keys = Some(names);
            }
        }
        _ => {}
    }
    Ok(())
}

/// Why a point of `a` in screenshot pixels, `scale` screen pixels each, lies off a screen of `width` by
/// `height`: its x,y unless it targets an element, and the end of a drag.
fn off_screen(a: &ActArgs, (width, height): (usize, usize), scale: f64) -> Option<String> {
    let (width, height) = (width as f64, height as f64);
    let drag = a.action == "drag";
    let start = (drag || a.element.is_none()).then_some((a.x, a.y));
    let end = drag.then_some((a.to_x, a.to_y));
    let (x, y) = [start, end].into_iter().flatten().find_map(|p| match p {
        (Some(x), Some(y))
            if !(0.0..width).contains(&(x * scale)) || !(0.0..height).contains(&(y * scale)) =>
        {
            Some((x, y))
        }
        _ => None,
    })?;
    let (w, h) = ((width / scale).floor(), (height / scale).floor());
    Some(format!(
        "({x},{y}) is off the screen, which is {w}x{h} screenshot pixels (x 0 to {}, y 0 to {}); the {} did not run: take a new screenshot and use a point inside it",
        w - 1.0,
        h - 1.0,
        a.action
    ))
}

/// One computer_state line on modal dialog `m`, which blocks the window it belongs to.
fn modal_line(m: &Modal) -> String {
    let pid = m.pid.map_or_else(String::new, |p| format!(" (pid={p})"));
    let owner = if m.owner.is_empty() {
        "its app".to_owned()
    } else {
        format!("\"{}\"", m.owner)
    };
    format!(
        "! modal dialog \"{}\"{pid} is open but {}: {owner} takes no input until it closes. Bring it to the front with launch_app of its app, then finish it or press Escape to cancel it.\n",
        m.title, m.place
    )
}

fn confirms(a: &ActArgs) -> bool {
    if a.action == "type" {
        return a.text.as_deref().is_some_and(|t| t.ends_with(['\n', '\r']));
    }
    combo(a).is_some_and(|k| ["return", "enter", "kp_enter"].contains(&k.as_str()))
}

/// `label` without case, mnemonic underscores, a trailing ellipsis or colon, and extra spaces, so a
/// toolbar button and its menu item compare equal.
fn label_key(label: &str) -> String {
    let words = label.replace('_', "").to_lowercase();
    let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
    words
        .trim_end_matches(['.', '\u{2026}', ':'])
        .trim_end()
        .to_owned()
}

/// Whether process `pid` has GTK loaded.
fn gtk(pid: i64) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/maps")).is_ok_and(|maps| {
        maps.contains("/libgtk-3.so")
            || maps.contains("/libgtk-x11-2.0.so")
            || maps.contains("/libgtk-4.so")
    })
}

/// Whether `a` is a single left click, which presses a button or menu item.
fn presses(a: &ActArgs) -> bool {
    a.action == "click" && a.button.as_deref().is_none_or(|b| b == "left")
}

fn clicks_left(a: &ActArgs) -> bool {
    matches!(a.action.as_str(), "click" | "double_click" | "triple_click")
        && a.button.as_deref().is_none_or(|b| b == "left")
}

/// Whether `a` may commit a page load: a left click, Return or a navigation key, or text ending in a newline.
fn commits(a: &ActArgs) -> bool {
    match a.action.as_str() {
        "type" => a.text.as_deref().is_some_and(|t| t.ends_with(['\n', '\r'])),
        "click" | "double_click" | "triple_click" => {
            a.button.as_deref().is_none_or(|b| b == "left")
        }
        _ => combo(a).is_some_and(|k| COMMIT_KEYS.contains(&k.as_str())),
    }
}

/// The lowercase combination a `key` or `hotkey` action presses.
fn combo(a: &ActArgs) -> Option<String> {
    match a.action.as_str() {
        "key" => a.key.as_deref().map(str::to_lowercase),
        "hotkey" => a.keys.as_ref().map(|k| k.join("+").to_lowercase()),
        _ => None,
    }
}

fn session_of(args: &Value) -> String {
    args["session"]
        .as_str()
        .unwrap_or(DEFAULT_SESSION)
        .to_owned()
}

fn window_list(result: &Value) -> Vec<Window> {
    let mut out: Vec<Window> = result["structuredContent"]["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| {
            let b = &w["bounds"];
            let n = |v: &Value| v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64));
            Some(Window {
                pid: w["pid"].as_i64()?,
                window_id: w["window_id"].as_i64()?,
                app: w["app_name"].as_str().unwrap_or_default().to_owned(),
                title: w["title"].as_str().unwrap_or_default().to_owned(),
                x: n(&b["x"]).unwrap_or(0),
                y: n(&b["y"]).unwrap_or(0),
                w: n(&b["width"]).or_else(|| n(&b["w"])).unwrap_or(0),
                h: n(&b["height"]).or_else(|| n(&b["h"])).unwrap_or(0),
                z: w["z_index"].as_i64().unwrap_or(i64::MIN),
            })
        })
        .filter(|w| w.w >= MIN_WINDOW && w.h >= MIN_WINDOW && w.x + w.w > 0 && w.y + w.h > 0)
        .collect();
    out.sort_by_key(|w| std::cmp::Reverse(w.z));
    out
}

fn desktop_point(args: &Value) -> bool {
    (args["scope"] == "desktop" || args["target"]["kind"] == "desktop")
        && ["x", "from_x"].iter().any(|k| args[k].is_number())
}

fn window_id_of(state: &Value) -> Option<i64> {
    state["structuredContent"]["window_id"].as_i64()
}

fn element_tree(result: &Value, scale: f64) -> (String, HashMap<i64, Element>) {
    let mut by_index = HashMap::new();
    let mut tree = String::new();
    let elements = result["structuredContent"]["elements"].as_array();
    for e in elements.into_iter().flatten() {
        let Some(index) = e["element_index"].as_i64() else {
            continue;
        };
        let f = &e["frame"];
        let frame = [&f["x"], &f["y"], &f["w"], &f["h"]].map(Value::as_f64);
        let frame = frame
            .iter()
            .all(Option::is_some)
            .then(|| frame.map(Option::unwrap_or_default));
        if let Some(t) = e["element_token"].as_str() {
            let element = Element {
                token: t.to_owned(),
                role: e["role"].as_str().unwrap_or_default().to_owned(),
                label: e["label"].as_str().unwrap_or_default().to_owned(),
                parent: e["parent_index"].as_i64(),
                frame,
                web: e["in_web_content"] == true,
            };
            by_index.insert(index, element);
        }
        if tree.len() >= TREE_CHARS {
            continue;
        }
        let depth = e["depth"].as_u64().unwrap_or(0).min(10) as usize;
        let _ = write!(
            tree,
            "{:indent$}[{index}] {} \"{}\"",
            "",
            e["role"].as_str().unwrap_or("?"),
            e["label"].as_str().unwrap_or_default(),
            indent = depth * 2
        );
        if let Some(v) = e["value"].as_str().filter(|v| !v.is_empty()) {
            let v: String = v.chars().take(60).collect();
            let _ = write!(tree, " value=\"{v}\"");
        }
        if let Some([x, y, w, h]) = frame {
            let _ = write!(
                tree,
                " center=({:.0},{:.0})",
                (x + w / 2.0) / scale,
                (y + h / 2.0) / scale
            );
        }
        if e["enabled"] == false {
            tree.push_str(" disabled");
        }
        tree.push('\n');
    }
    if tree.len() >= TREE_CHARS {
        tree.push_str("[element list truncated; pass query to narrow it]\n");
    }
    if tree.is_empty() {
        tree = text_of(result);
    }
    (tree, by_index)
}

/// Whether more than [`MOVED_PERCENT`] of the pixels of `rect` in `screen` differ by more than
/// [`STILL_TOLERANCE`] from `now`, the same rectangle's RGB rows.
fn moved(screen: &Screen, [x, y, w, h]: [i32; 4], now: &[u8]) -> bool {
    let (x, y, w, h) = (x as usize, y as usize, w as usize, h as usize);
    if now.len() != w * h * 3 {
        return true;
    }
    let stride = screen.width as usize * 3;
    let changed = (0..h)
        .flat_map(|row| {
            let start = (y + row) * stride + x * 3;
            let then = &screen.rgb[start..start + w * 3];
            let now = &now[row * w * 3..(row + 1) * w * 3];
            then.as_chunks::<3>().0.iter().zip(now.as_chunks::<3>().0)
        })
        .filter(|(a, b)| differs(&a[..], &b[..]))
        .count();
    changed * 100 > w * h * MOVED_PERCENT
}

/// RGB rows shrunk by area averaging so the long edge is `edge`.
fn fit(width: u32, height: u32, rgb: &[u8], edge: u32) -> (u32, u32, Vec<u8>) {
    let (tw, th) = if width >= height {
        (edge, (height * edge / width).max(1))
    } else {
        ((width * edge / height).max(1), edge)
    };
    let (w, h, ow, oh) = (width as usize, height as usize, tw as usize, th as usize);
    let mut out = Vec::with_capacity(ow * oh * 3);
    for oy in 0..oh {
        let (y0, y1) = (oy * h / oh, ((oy + 1) * h / oh).max(oy * h / oh + 1));
        for ox in 0..ow {
            let (x0, x1) = (ox * w / ow, ((ox + 1) * w / ow).max(ox * w / ow + 1));
            let mut sum = [0u32; 3];
            for y in y0..y1 {
                for p in rgb[(y * w + x0) * 3..(y * w + x1) * 3].as_chunks::<3>().0 {
                    for (s, c) in sum.iter_mut().zip(p) {
                        *s += u32::from(*c);
                    }
                }
            }
            let n = u32::try_from((y1 - y0) * (x1 - x0)).unwrap_or(u32::MAX);
            out.extend(sum.map(|s| u8::try_from(s / n).unwrap_or(u8::MAX)));
        }
    }
    (tw, th, out)
}

/// `text` with each point `(x,y)` or `(x, y)` divided by `scale`, from screen to screenshot pixels.
fn shot_points(text: &str, scale: f64) -> String {
    if scale == 1.0 {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        let point = rest.find(')').and_then(|close| {
            let (x, y) = rest[1..close].split_once(',')?;
            Some((
                close,
                x.trim().parse::<f64>().ok()?,
                y.trim().parse::<f64>().ok()?,
            ))
        });
        match point {
            Some((close, x, y)) => {
                let _ = write!(out, "({:.0},{:.0})", x / scale, y / scale);
                rest = &rest[close + 1..];
            }
            None => {
                out.push('(');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The opaque RGBA PNG `b64` re-encoded at the same size by [`encode`]; `None` when that is not smaller.
fn shrink(b64: &str) -> Option<String> {
    let data = B64.decode(b64).ok()?;
    let (info, rgba) = decode_png(&data)?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let rgb: Vec<u8> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    let out = encode(info.width, info.height, &rgb)?;
    (out.len() < data.len()).then(|| B64.encode(&out))
}

/// The format, pixel size, bytes and estimated input tokens of the base64 image `b64`.
fn weigh(b64: &str) -> String {
    let data = B64.decode(b64).unwrap_or_default();
    let kb = data.len().div_ceil(1024);
    match image_size(&data) {
        Some((format, w, h)) => format!(
            "{w}x{h} {format}, {kb} KB, about {} input tokens",
            (w * h).div_ceil(PIXELS_PER_TOKEN)
        ),
        None => format!("{kb} KB"),
    }
}

/// The format and pixel size of a PNG, lossless WebP or JPEG image.
fn image_size(data: &[u8]) -> Option<(&'static str, u32, u32)> {
    let be = |i: usize| {
        Some(u32::from(u16::from_be_bytes([
            *data.get(i)?,
            *data.get(i + 1)?,
        ])))
    };
    if data.starts_with(b"\x89PNG") {
        let w = u32::from_be_bytes(data.get(16..20)?.try_into().ok()?);
        let h = u32::from_be_bytes(data.get(20..24)?.try_into().ok()?);
        return Some(("png", w, h));
    }
    if data.get(8..16) == Some(b"WEBPVP8L") {
        let bits = u32::from_le_bytes(data.get(21..25)?.try_into().ok()?);
        return Some(("webp", (bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1));
    }
    let mut i = 2;
    while data.get(i) == Some(&0xff) {
        let marker = *data.get(i + 1)?;
        if matches!(marker, 0xc0..=0xc3) {
            return Some(("jpeg", be(i + 7)?, be(i + 5)?));
        }
        i += 2 + usize::try_from(be(i + 2)?).ok()?;
    }
    None
}

/// RGB rows as JPEG when a pixel sample has photo-like colour counts, else as lossless WebP.
fn encode(width: u32, height: u32, rgb: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    if photo(rgb) {
        let (w, h) = (u16::try_from(width).ok()?, u16::try_from(height).ok()?);
        jpeg_encoder::Encoder::new(&mut out, JPEG_QUALITY)
            .encode(rgb, w, h, jpeg_encoder::ColorType::Rgb)
            .ok()?;
    } else {
        image_webp::WebPEncoder::new(&mut out)
            .encode(rgb, width, height, image_webp::ColorType::Rgb8)
            .ok()?;
    }
    Some(out)
}

/// Whether every [`COLOR_STRIDE`]th pixel shows more than [`PHOTO_COLORS`] distinct colours.
fn photo(rgb: &[u8]) -> bool {
    let mut seen = HashSet::new();
    rgb.as_chunks::<3>()
        .0
        .iter()
        .step_by(COLOR_STRIDE)
        .any(|p| seen.insert(*p) && seen.len() > PHOTO_COLORS)
}

fn decode_png(data: &[u8]) -> Option<(png::OutputInfo, Vec<u8>)> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(data))
        .read_info()
        .ok()?;
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut pixels).ok()?;
    pixels.truncate(info.buffer_size());
    Some((info, pixels))
}

/// A screen rectangle's pixels: the rectangle in screen units, its size and RGB rows in image pixels, and the image pixels per screen unit.
struct Cut {
    rect: [i32; 4],
    width: u32,
    height: u32,
    rgb: Vec<u8>,
    per_unit: f64,
}

/// The RGB rows of the pixel rectangle `[x, y, w, h]` of an 8-bit RGB or RGBA image, clipped to it.
fn crop(
    info: &png::OutputInfo,
    data: &[u8],
    [x, y, w, h]: [u32; 4],
) -> Option<(u32, u32, Vec<u8>)> {
    let channels = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgb, png::BitDepth::Eight) => 3,
        (png::ColorType::Rgba, png::BitDepth::Eight) => 4,
        _ => return None,
    };
    let (w, h) = (
        w.min(info.width.checked_sub(x)?),
        h.min(info.height.checked_sub(y)?),
    );
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for row in y..y + h {
        let start = (row as usize * info.line_size) + x as usize * channels;
        for p in data
            .get(start..start + w as usize * channels)?
            .chunks(channels)
        {
            rgb.extend_from_slice(&p[..3]);
        }
    }
    (w > 0 && h > 0).then_some((w, h, rgb))
}

/// Whether two frames differ by more than [`STILL_TOLERANCE`] in any channel of any pixel.
fn differs(a: &[u8], b: &[u8]) -> bool {
    a.len() != b.len()
        || a.iter()
            .zip(b)
            .any(|(x, y)| x.abs_diff(*y) > STILL_TOLERANCE)
}

fn required<'a>(v: &'a Option<String>, name: &str) -> Result<&'a str, String> {
    v.as_deref().ok_or_else(|| format!("missing {name}"))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::driver::CLICK_SPACING;

    fn cua(socket: &Path) -> Computer {
        cua_on(socket, Os::Linux)
    }

    fn cua_on(socket: &Path, os: Os) -> Computer {
        Computer::new(
            Arc::new(Driver::new(socket.to_string_lossy().into_owned())),
            os,
        )
    }

    #[cfg(unix)]
    fn endpoint(dir: &Path) -> PathBuf {
        dir.join("cua.sock")
    }

    #[cfg(windows)]
    fn endpoint(_: &Path) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        PathBuf::from(format!(
            r"\\.\pipe\shuttle-test-cua-{}-{n}",
            std::process::id()
        ))
    }

    #[cfg(unix)]
    struct Fake(tokio::net::UnixListener);

    #[cfg(unix)]
    impl Fake {
        fn bind(socket: impl AsRef<Path>) -> std::io::Result<Self> {
            tokio::net::UnixListener::bind(socket).map(Self)
        }

        async fn accept(&self) -> std::io::Result<(crate::server::Accepted, ())> {
            self.0.accept().await.map(|(stream, _)| (stream, ()))
        }
    }

    #[cfg(windows)]
    struct Fake {
        name: String,
        next: std::sync::Mutex<tokio::net::windows::named_pipe::NamedPipeServer>,
    }

    #[cfg(windows)]
    impl Fake {
        fn bind(socket: impl AsRef<Path>) -> std::io::Result<Self> {
            let name = socket.as_ref().to_string_lossy().into_owned();
            let first = tokio::net::windows::named_pipe::ServerOptions::new()
                .first_pipe_instance(true)
                .create(&name)?;
            Ok(Self {
                name,
                next: std::sync::Mutex::new(first),
            })
        }

        async fn accept(&self) -> std::io::Result<(crate::server::Accepted, ())> {
            let fresh = tokio::net::windows::named_pipe::ServerOptions::new().create(&self.name)?;
            let server = std::mem::replace(&mut *self.next.lock().unwrap(), fresh);
            server.connect().await?;
            Ok((server, ()))
        }
    }

    #[tokio::test]
    async fn background_input_refuses_computer_actions_before_any_driver_call() {
        let dir = tempfile::tempdir().unwrap();
        let cua = cua(&dir.path().join("none.sock")).background(true);
        let err = cua
            .run(
                "a",
                "desk_computer_act",
                json!({"action": "click", "x": 10, "y": 10}),
            )
            .await
            .unwrap_err();
        assert!(err.contains("needs the foreground"), "{err}");
    }

    #[tokio::test]
    async fn background_input_never_reaches_the_xtest_typer() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let calls = std::sync::Arc::default();
        let driver = window_driver(&socket, &calls, usize::MAX);
        let mut cua = cua(&socket).background(true);
        cua.typer = |_, _| panic!("the XTest typer ran in background");
        for args in [
            json!({"action": "type", "text": "Ab3!x"}),
            json!({"actions": [{"action": "type", "text": "Ab3!x"}]}),
        ] {
            let err = cua.run("a", "desk_computer_act", args).await.unwrap_err();
            assert!(err.contains("needs the foreground"), "{err}");
        }
        assert!(types(&calls).is_empty(), "{:?}", calls.lock().unwrap());
        driver.abort();
    }

    #[tokio::test]
    async fn a_request_waits_for_a_driver_that_is_restarting() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let path = socket.clone();
        let driver = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            let listener = Fake::bind(&path).unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            reader
                .into_inner()
                .write_all(b"{\"ok\":true,\"result\":{\"structuredContent\":{}}}\n")
                .await
                .unwrap();
        });
        let cua = cua(&socket);
        let out = cua.request("s", "list_windows", json!({})).await;
        assert!(out.is_ok(), "{out:?}");
        driver.await.unwrap();
    }

    #[tokio::test]
    async fn a_click_near_the_last_one_waits_out_the_double_click_time() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let listener = Fake::bind(&socket).unwrap();
        let driver = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                reader
                    .into_inner()
                    .write_all(b"{\"ok\":true,\"result\":{}}\n")
                    .await
                    .unwrap();
            }
        });
        let cua = cua(&socket);
        let click = |x: f64| json!({"x": x, "y": 300.0});
        cua.request("s", "click", click(300.0)).await.unwrap();
        let start = Instant::now();
        cua.request("s", "click", click(400.0)).await.unwrap();
        assert!(start.elapsed() < CLICK_SPACING / 2, "{:?}", start.elapsed());
        cua.request("s", "click", click(405.0)).await.unwrap();
        assert!(start.elapsed() >= CLICK_SPACING, "{:?}", start.elapsed());
        driver.abort();
    }

    async fn closing_then_answering_driver(socket: PathBuf) -> Vec<String> {
        let listener = Fake::bind(&socket).unwrap();
        let mut names = Vec::new();
        for answer in [false, true] {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            names.push(req["name"].as_str().unwrap().to_owned());
            if answer {
                reader
                    .into_inner()
                    .write_all(b"{\"ok\":true,\"result\":{\"structuredContent\":{}}}\n")
                    .await
                    .unwrap();
            }
        }
        names
    }

    #[tokio::test]
    async fn a_read_is_retried_once_after_an_empty_reply() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let driver = tokio::spawn(closing_then_answering_driver(socket.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let out = cua(&socket).request("s", "list_windows", json!({})).await;
        assert!(out.is_ok(), "{out:?}");
        assert_eq!(driver.await.unwrap(), ["list_windows", "list_windows"]);
    }

    #[tokio::test]
    async fn an_input_action_is_never_retried_after_an_empty_reply() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let driver = tokio::spawn(closing_then_answering_driver(socket.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let out = cua(&socket).request("s", "click", json!({})).await;
        assert!(out.unwrap_err().starts_with("decode cua-driver reply"));
        driver.abort();
    }

    #[tokio::test]
    async fn a_key_with_no_window_on_the_screen_goes_to_the_desktop_and_typing_fails() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let listener = Fake::bind(&socket).unwrap();
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let log = std::sync::Arc::clone(&calls);
        let driver = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                log.lock()
                    .unwrap()
                    .push(serde_json::from_str::<Value>(&line).unwrap());
                reader
                    .into_inner()
                    .write_all(
                        b"{\"ok\":true,\"result\":{\"structuredContent\":{\"windows\":[]}}}\n",
                    )
                    .await
                    .unwrap();
            }
        });
        let cua = cua(&socket);
        let out = cua
            .act(json!({"action": "key", "key": "escape"}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.text);
        let pressed = calls
            .lock()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "press_key")
            .cloned()
            .unwrap();
        assert_eq!(pressed["name"], "press_key");
        assert_eq!(
            pressed["args"]["target"],
            json!({"kind": "desktop", "display_id": "primary"})
        );
        assert_eq!(pressed["args"]["session"], DEFAULT_SESSION);
        assert!(pressed["args"].get("pid").is_none(), "{pressed}");
        for text in ["hello", "héllo"] {
            let failed = match cua.act(json!({"action": "type", "text": text})).await {
                Ok(out) => {
                    assert!(out.is_error, "{text}: {}", out.text);
                    out.text
                }
                Err(e) => e,
            };
            assert!(failed.contains(NO_TEXT_TARGET), "{text}: {failed}");
        }
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|c| c["name"] != "type_text"),
            "{:?}",
            calls.lock().unwrap()
        );
        driver.abort();
    }

    /// A driver on `socket` that logs every call in `calls`, lists one Chrome window and types the first `typed` texts.
    fn window_driver(
        socket: &Path,
        calls: &std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
        typed: usize,
    ) -> tokio::task::JoinHandle<()> {
        let listener = Fake::bind(socket).unwrap();
        let log = std::sync::Arc::clone(calls);
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                let text = req["args"]["text"].as_str().unwrap_or_default().to_owned();
                let types = log
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|c| c["name"] == "type_text")
                    .count();
                let reply = match req["name"].as_str().unwrap_or_default() {
                    "type_text" if types < typed => {
                        json!({"ok": true, "result": {"content": [{"type": "text", "text": format!("Typed {text}")}]}})
                    }
                    "type_text" => json!({"ok": false, "error": format!("cannot type {text}")}),
                    "list_windows" => {
                        json!({"ok": true, "result": {"structuredContent": {"windows": [{"pid": 7, "window_id": 9, "app_name": "Google-chrome", "title": "checkout", "bounds": {"x": 0, "y": 0, "width": 800, "height": 600}, "z_index": 1}]}}})
                    }
                    _ => json!({"ok": true, "result": {"structuredContent": {}}}),
                };
                log.lock().unwrap().push(req);
                let mut out = reply.to_string().into_bytes();
                out.push(b'\n');
                reader.into_inner().write_all(&out).await.unwrap();
            }
        })
    }

    #[tokio::test]
    async fn a_type_at_a_point_clicks_it_then_types_into_the_focus() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let calls = std::sync::Arc::default();
        let driver = window_driver(&socket, &calls, usize::MAX);
        calls.lock().unwrap().clear();
        let cua = cua(&socket);
        let out = cua
            .act(json!({"action": "type", "text": "hello", "x": 10, "y": 20}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.text);
        let calls = calls.lock().unwrap();
        let input: Vec<&Value> = calls
            .iter()
            .filter(|c| c["name"] == "click" || c["name"] == "type_text")
            .collect();
        assert_eq!(input.len(), 2, "{input:?}");
        assert_eq!(input[0]["name"], "click");
        assert_eq!(
            (
                input[0]["args"]["x"].as_f64(),
                input[0]["args"]["y"].as_f64()
            ),
            (Some(10.0), Some(20.0))
        );
        assert_eq!(input[1]["args"]["text"], "hello");
        assert!(input[1]["args"].get("x").is_none(), "{}", input[1]);
        driver.abort();
    }

    async fn turn(cua: &Computer) -> Turn {
        Arc::new(cua.input.clone().lock_owned().await)
    }

    fn types(calls: &std::sync::Mutex<Vec<Value>>) -> Vec<String> {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["name"] == "type_text")
            .map(|c| c["args"]["text"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    #[tokio::test]
    async fn a_typeable_desktop_line_goes_on_the_core_keyboard_without_a_driver_type_call() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let calls = std::sync::Arc::default();
        let driver = window_driver(&socket, &calls, usize::MAX);
        let mut cua = cua(&socket);
        cua.typer = |text, _| Ok(text == "Ab3!x-Y_z?");
        let req =
            json!({"text": "Ab3!x-Y_z?", "target": {"kind": "desktop", "display_id": "primary"}});
        let out = cua
            .send("s", "type_text", req, false, &turn(&cua).await)
            .await
            .unwrap();
        assert_eq!(text_of(&out), XTEST_TYPED);
        assert!(types(&calls).is_empty(), "{:?}", calls.lock().unwrap());
        driver.abort();
    }

    #[tokio::test]
    async fn a_desktop_line_the_typer_cannot_type_goes_to_the_driver() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let calls = std::sync::Arc::default();
        let driver = window_driver(&socket, &calls, usize::MAX);
        let mut cua = cua(&socket);
        cua.typer = |_, _| Ok(false);
        let req = json!({"text": "abc", "target": {"kind": "desktop", "display_id": "primary"}});
        cua.send("s", "type_text", req, false, &turn(&cua).await)
            .await
            .unwrap();
        assert_eq!(types(&calls), ["abc"]);
        driver.abort();
    }

    #[tokio::test]
    async fn a_typer_error_fails_the_type_without_a_driver_retry() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let calls = std::sync::Arc::default();
        let driver = window_driver(&socket, &calls, usize::MAX);
        let mut cua = cua(&socket);
        cua.typer = |_, _| Err("XTest: connection lost".to_owned());
        let req = json!({"text": "abc", "target": {"kind": "desktop", "display_id": "primary"}});
        let err = cua
            .send("s", "type_text", req, false, &turn(&cua).await)
            .await
            .unwrap_err();
        assert!(err.contains("connection lost"), "{err}");
        assert!(types(&calls).is_empty(), "{:?}", calls.lock().unwrap());
        driver.abort();
    }

    #[tokio::test]
    async fn a_cancelled_type_holds_the_input_turn_until_its_typer_returns() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let calls = std::sync::Arc::default();
        let driver = window_driver(&socket, &calls, usize::MAX);
        let mut cua = cua(&socket);
        cua.typer = |_, stop| {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(300));
            Err("stopped by the user".to_owned())
        };
        let req = json!({"text": "abc", "target": {"kind": "desktop", "display_id": "primary"}});
        let held = turn(&cua).await;
        let send = cua.send("s", "type_text", req, false, &held);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), send)
                .await
                .is_err()
        );
        drop(held);
        let start = Instant::now();
        drop(turn(&cua).await);
        assert!(
            start.elapsed() >= Duration::from_millis(250),
            "{:?}",
            start.elapsed()
        );
        driver.abort();
    }

    #[tokio::test]
    async fn pid_addressed_input_a_dialog_line_and_cjk_never_reach_the_typer() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let calls = std::sync::Arc::default();
        let driver = window_driver(&socket, &calls, usize::MAX);
        let mut cua = cua(&socket);
        cua.typer = |_, _| Err("the typer ran".to_owned());
        let pid = json!({"text": "abc", "pid": 7, "window_id": 9});
        cua.send("s", "type_text", pid, false, &turn(&cua).await)
            .await
            .unwrap();
        let line = json!({"text": "name", "target": {"kind": "desktop", "display_id": "primary"}});
        cua.send("s", "type_text", line, true, &turn(&cua).await)
            .await
            .unwrap();
        let out = cua
            .act(json!({"action": "type", "text": "中文测试"}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(types(&calls), ["abc", "name", "中文测试"]);
        driver.abort();
    }

    #[tokio::test]
    async fn a_foreground_type_of_twenty_distinct_cjk_goes_as_runs_of_at_most_eight() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let calls = std::sync::Arc::default();
        let driver = window_driver(&socket, &calls, usize::MAX);
        let text = "一二三四五六七八九十百千万亿甲乙丙丁戊己";
        calls.lock().unwrap().clear();
        let cua = cua(&socket);
        let out = cua
            .act(json!({"action": "type", "text": text, "foreground": true}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.text);
        let calls = calls.lock().unwrap();
        let runs: Vec<&str> = calls
            .iter()
            .filter(|c| c["name"] == "type_text")
            .map(|c| c["args"]["text"].as_str().unwrap())
            .collect();
        assert_eq!(runs.len(), 3, "{runs:?}");
        assert!(runs.iter().all(|r| r.chars().count() <= 8), "{runs:?}");
        assert_eq!(runs.concat(), text);
        driver.abort();
    }

    #[test]
    fn a_window_list_takes_whole_or_fractional_bounds() {
        let listed = window_list(&json!({"structuredContent": {"windows": [
            {"pid": 7, "window_id": 9, "app_name": "TextEdit", "title": "a", "bounds": {"x": 180.0, "y": 97.0, "width": 920.0, "height": 436.0}, "z_index": 2},
            {"pid": 8, "window_id": 3, "app_name": "Notepad.exe", "title": "b", "bounds": {"x": 57, "y": 52, "width": 950, "height": 538}, "z_index": 1}
        ]}}));
        let got: Vec<_> = listed.iter().map(|w| (w.pid, w.x, w.y, w.w, w.h)).collect();
        assert_eq!(got, [(7, 180, 97, 920, 436), (8, 57, 52, 950, 538)]);
    }

    fn press_driver(
        socket: &Path,
        calls: &std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
        refusal: &'static str,
    ) -> tokio::task::JoinHandle<()> {
        let listener = Fake::bind(socket).unwrap();
        let log = std::sync::Arc::clone(calls);
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                let reply = match req["name"].as_str().unwrap_or_default() {
                    "list_windows" => {
                        json!({"ok": true, "result": {"structuredContent": {"windows": [{"pid": 7, "window_id": 9, "app_name": "TextEdit", "title": "t", "bounds": {"x": 0.0, "y": 0.0, "width": 800.0, "height": 600.0}, "z_index": 2}]}}})
                    }
                    "get_window_state" => {
                        json!({"ok": true, "result": {"structuredContent": {"window_id": 9, "elements": [{"element_index": 0, "element_token": "t0", "role": "AXTextArea", "label": "", "frame": {"x": 100.0, "y": 100.0, "w": 200.0, "h": 40.0}}]}}})
                    }
                    "click" if req["args"]["element_token"].is_string() => {
                        json!({"ok": true, "result": {"isError": true, "content": [{"type": "text", "text": refusal}]}})
                    }
                    _ => json!({"ok": true, "result": {"structuredContent": {}}}),
                };
                log.lock().unwrap().push(req);
                let mut out = reply.to_string().into_bytes();
                out.push(b'\n');
                reader.into_inner().write_all(&out).await.unwrap();
            }
        })
    }

    async fn press(refusal: &'static str, under: Under) -> (Output, Vec<Value>) {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let calls = std::sync::Arc::default();
        let driver = press_driver(&socket, &calls, refusal);
        let mut cua = cua(&socket);
        cua.under = under;
        cua.state(json!({"pid": 7})).await.unwrap();
        let out = cua
            .act(json!({"action": "click", "element": 0}))
            .await
            .unwrap();
        driver.abort();
        let clicks = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["name"] == "click")
            .map(|c| c["args"].clone())
            .collect();
        (out, clicks)
    }

    #[tokio::test]
    async fn an_element_without_the_press_action_gets_one_desktop_click_at_its_centre() {
        let (out, clicks) = press(
            "AX action failed: AXUIElementPerformAction(AXPress) returned -25206",
            |x, y| ((x, y) == (200.0, 120.0)).then_some((7, 9)),
        )
        .await;
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(clicks.len(), 2, "{clicks:?}");
        assert_eq!(
            (
                &clicks[1]["x"],
                &clicks[1]["y"],
                &clicks[1]["target"]["kind"]
            ),
            (&json!(200.0), &json!(120.0), &json!("desktop"))
        );
        assert!(clicks[1]["element_token"].is_null(), "{clicks:?}");
    }

    #[tokio::test]
    async fn a_pid_without_a_window_id_reads_its_frontmost_listed_window() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let calls = std::sync::Arc::default();
        let driver = press_driver(&socket, &calls, "");
        cua(&socket).state(json!({"pid": 7})).await.unwrap();
        driver.abort();
        let read = calls.lock().unwrap().clone();
        let read = read.iter().find(|c| c["name"] == "get_window_state");
        assert_eq!(read.unwrap()["args"]["window_id"], 9, "{read:?}");
    }

    #[tokio::test]
    async fn a_press_that_timed_out_is_not_clicked_again() {
        let (out, clicks) = press(
            "AX action failed: AXUIElementPerformAction(AXPress) returned -25204",
            |x, y| ((x, y) == (200.0, 120.0)).then_some((7, 9)),
        )
        .await;
        assert!(out.is_error && out.text.contains("-25204"), "{}", out.text);
        assert_eq!(clicks.len(), 1, "{clicks:?}");
    }

    #[tokio::test]
    async fn an_element_under_another_window_or_a_popup_even_of_its_own_app_is_not_clicked_through_it()
     {
        let covered: [Under; 3] = [|_, _| Some((8, 10)), |_, _| Some((7, 11)), |_, _| None];
        for under in covered {
            let (out, clicks) = press(
                "AX action failed: AXUIElementPerformAction(AXPress) returned -25206",
                under,
            )
            .await;
            assert!(out.is_error && out.text.contains("-25206"), "{}", out.text);
            assert_eq!(clicks.len(), 1, "{clicks:?}");
        }
    }

    fn png_of(width: u32, height: u32, rgba: &[u8]) -> String {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(rgba)
            .unwrap();
        B64.encode(out)
    }

    #[test]
    fn a_crop_keeps_the_rgb_of_its_rectangle_and_stops_at_the_image_edge() {
        let rgba: Vec<u8> = (0..4u8 * 2 * 4).collect();
        let png = B64.decode(png_of(4, 2, &rgba)).unwrap();
        let (info, data) = decode_png(&png).unwrap();
        let (w, h, rgb) = crop(&info, &data, [2, 1, 5, 5]).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(rgb, [24, 25, 26, 28, 29, 30]);
        assert!(crop(&info, &data, [4, 0, 1, 1]).is_none());
    }

    #[tokio::test]
    async fn a_zoom_without_mit_shm_cuts_the_region_from_a_full_size_capture() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let listener = Fake::bind(&socket).unwrap();
        let image = png_of(8, 4, &[200; 8 * 4 * 4]);
        let driver = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                let reply = match req["name"].as_str().unwrap_or_default() {
                    "get_desktop_state" => {
                        json!({"ok": true, "result": {"content": [{"type": "image", "data": image, "mimeType": "image/png"}], "structuredContent": {"screen_width": 4, "screen_height": 2}}})
                    }
                    _ => json!({"ok": true, "result": {"structuredContent": {}}}),
                };
                let mut out = reply.to_string().into_bytes();
                out.push(b'\n');
                reader.into_inner().write_all(&out).await.unwrap();
            }
        });
        let cua = cua(&socket);
        let out = cua
            .look(&json!({"region": {"x": 1, "y": 0, "w": 2, "h": 1}, "max_edge": 0}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.contains("4x2"), "{}", out.text);
        assert!(
            out.text.contains("x = 1 + px*0.50, y = 0 + py*0.50"),
            "{}",
            out.text
        );
        driver.abort();
    }

    #[tokio::test]
    async fn a_driver_without_frame_scale_maps_by_its_screen_and_screenshot_widths() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let listener = Fake::bind(&socket).unwrap();
        let clicks = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(f64, f64)>::new()));
        let log = std::sync::Arc::clone(&clicks);
        let driver = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                let args = &req["args"];
                let reply = match req["name"].as_str().unwrap_or_default() {
                    "get_desktop_state" => {
                        let shot = args["max_image_dimension"].as_u64().unwrap_or(1280);
                        json!({"ok": true, "result": {"structuredContent": {"screen_width": 1280, "screenshot_width": shot.min(1280)}}})
                    }
                    name => {
                        if name == "click" {
                            log.lock()
                                .unwrap()
                                .push((args["x"].as_f64().unwrap(), args["y"].as_f64().unwrap()));
                        }
                        json!({"ok": true, "result": {"structuredContent": {}}})
                    }
                };
                let mut out = serde_json::to_vec(&reply).unwrap();
                out.push(b'\n');
                reader.into_inner().write_all(&out).await.unwrap();
            }
        });
        let cua = cua(&socket);
        cua.state(json!({})).await.unwrap();
        let out = cua
            .act(json!({"action": "click", "x": 100.0, "y": 40.0}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(*clicks.lock().unwrap(), [(125.0, 50.0)]);
        driver.abort();
    }

    #[tokio::test]
    async fn every_point_of_an_action_list_maps_by_the_screenshot_taken_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let listener = Fake::bind(&socket).unwrap();
        let clicks = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(f64, f64)>::new()));
        let log = std::sync::Arc::clone(&clicks);
        let driver = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                let args = &req["args"];
                let reply = match req["name"].as_str().unwrap_or_default() {
                    "get_desktop_state" => {
                        let scale = if args["max_image_dimension"].as_u64().is_some_and(|d| d > 0) {
                            2.0
                        } else {
                            1.0
                        };
                        json!({"ok": true, "result": {"structuredContent": {"frame_scale": scale}}})
                    }
                    name => {
                        if name == "click" {
                            log.lock()
                                .unwrap()
                                .push((args["x"].as_f64().unwrap(), args["y"].as_f64().unwrap()));
                        }
                        json!({"ok": true, "result": {"structuredContent": {}}})
                    }
                };
                let mut out = serde_json::to_vec(&reply).unwrap();
                out.push(b'\n');
                reader.into_inner().write_all(&out).await.unwrap();
            }
        });
        let cua = cua(&socket);
        let click = |x: f64| json!({"action": "click", "x": x, "y": 30.0});
        cua.state(json!({"max_edge": 0})).await.unwrap();
        let first = json!({"actions": [click(100.0), click(110.0), click(120.0)], "max_edge": 640});
        let out = cua.act(first).await.unwrap();
        assert!(!out.is_error, "{}", out.text);
        let second = json!({"actions": [click(100.0), click(110.0)], "max_edge": 640});
        cua.act(second).await.unwrap();
        assert_eq!(
            *clicks.lock().unwrap(),
            [
                (100.0, 30.0),
                (110.0, 30.0),
                (120.0, 30.0),
                (200.0, 60.0),
                (220.0, 60.0)
            ]
        );
        driver.abort();
    }

    #[tokio::test]
    async fn a_pinned_scale_maps_an_action_and_every_action_of_a_list() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, clicks, driver) = scaling_driver(dir.path());
        let cua = cua(&socket);
        let click = |x: f64| json!({"action": "click", "x": x, "y": 40.0});
        cua.state(json!({"max_edge": 640})).await.unwrap();
        let mut one = click(100.0);
        one["scale"] = 1.0.into();
        cua.act(one).await.unwrap();
        let list = json!({"actions": [click(100.0), click(130.0)], "max_edge": 640, "scale": 1.25});
        cua.act(list).await.unwrap();
        let bad = cua.act(json!({"action": "click", "x": 1.0, "y": 1.0, "scale": 0.0}));
        assert_eq!(bad.await.unwrap_err(), "scale 0 is not a positive number");
        cua.state(json!({"max_edge": 640})).await.unwrap();
        let typo = cua.act(json!({"action": "clik", "x": 1.0, "y": 1.0, "scale": 1.0}));
        assert_eq!(typo.await.unwrap_err(), "unknown action clik");
        cua.act(click(100.0)).await.unwrap();
        assert_eq!(
            *clicks.lock().unwrap(),
            [(100.0, 40.0), (125.0, 50.0), (162.5, 50.0), (200.0, 80.0)]
        );
        driver.abort();
    }

    #[tokio::test]
    async fn two_acts_at_once_on_one_session_each_map_by_their_own_scale() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, clicks, driver) = scaling_driver(dir.path());
        let cua = cua(&socket);
        cua.state(json!({"max_edge": 640})).await.unwrap();
        let (one, two) = tokio::join!(
            cua.act(json!({"action": "click", "x": 100.0, "y": 40.0, "scale": 1.0})),
            cua.act(json!({"action": "click", "x": 300.0, "y": 40.0, "scale": 3.0}))
        );
        assert!(!one.unwrap().is_error && !two.unwrap().is_error);
        let mut got = clicks.lock().unwrap().clone();
        got.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(got, [(100.0, 40.0), (900.0, 120.0)]);
        driver.abort();
    }

    #[tokio::test]
    async fn a_session_without_its_own_screenshot_maps_by_the_latest_one_of_any_session() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, clicks, driver) = scaling_driver(dir.path());
        let cua = cua(&socket);
        let click =
            |session: &str| json!({"action": "click", "x": 100.0, "y": 40.0, "session": session});
        cua.screenshot_once(None).await.unwrap();
        cua.act(click("after-default-shot")).await.unwrap();
        cua.screenshot_once(Some(0)).await.unwrap();
        cua.act(click("after-full-shot")).await.unwrap();
        cua.state(json!({"session": "ended", "max_edge": 640}))
            .await
            .unwrap();
        cua.end(json!({"session": "ended"})).await.unwrap();
        cua.act(click("rotated")).await.unwrap();
        cua.state(json!({"session": "own", "max_edge": 1280}))
            .await
            .unwrap();
        cua.screenshot_once(None).await.unwrap();
        cua.act(click("own")).await.unwrap();
        assert_eq!(
            *clicks.lock().unwrap(),
            [(125.0, 50.0), (100.0, 40.0), (200.0, 80.0), (100.0, 40.0)]
        );
        driver.abort();
    }

    #[tokio::test]
    async fn a_read_share_screenshot_leaves_the_scale_of_other_sessions_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, clicks, driver) = scaling_driver(dir.path());
        let cua = cua(&socket);
        let click =
            |session: &str| json!({"action": "click", "x": 100.0, "y": 40.0, "session": session});
        cua.state(json!({"session": "owner", "max_edge": 640}))
            .await
            .unwrap();
        cua.end(json!({"session": "owner"})).await.unwrap();
        cua.look(&json!({"session": "guest", "max_edge": 128, "workspace_only": true}))
            .await
            .unwrap();
        cua.state(json!({"session": "guest", "max_edge": 256, "workspace_only": true}))
            .await
            .unwrap();
        cua.act(click("rotated")).await.unwrap();
        cua.act(click("guest")).await.unwrap();
        assert_eq!(*clicks.lock().unwrap(), [(200.0, 80.0), (500.0, 200.0)]);
        driver.abort();
    }

    type Clicks = std::sync::Arc<std::sync::Mutex<Vec<(f64, f64)>>>;

    type Landed = std::sync::Arc<std::sync::Mutex<Vec<Vec<f64>>>>;

    fn retina_driver(
        dir: &Path,
        full_fails: bool,
    ) -> (PathBuf, Landed, tokio::task::JoinHandle<()>) {
        let socket = endpoint(dir);
        let listener = Fake::bind(&socket).unwrap();
        let landed = Landed::default();
        let log = std::sync::Arc::clone(&landed);
        let driver = tokio::spawn(async move {
            let mut factors = HashMap::<String, f64>::new();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                let args = &req["args"];
                let session = args["session"].as_str().unwrap_or_default().to_owned();
                let windowless = args["pid"].is_null() && args["element_token"].is_null();
                let refused = |text: &str| json!({"ok": true, "result": {"isError": true, "content": [{"type": "text", "text": text}]}});
                let reply = match req["name"].as_str().unwrap_or_default() {
                    "get_desktop_state" if full_fails && args["max_image_dimension"] == 0 => {
                        refused("Desktop screenshot failed: the display is asleep")
                    }
                    "get_desktop_state" => {
                        let width = match args["max_image_dimension"].as_u64() {
                            None => 120,
                            Some(0) => 200,
                            Some(edge) => edge.min(200),
                        };
                        let mut state = json!({"screenshot_width": width, "screenshot_height": width / 2, "screen_width": 100, "screen_height": 50, "scale_factor": 2.0});
                        if width < 200 {
                            state["screenshot_original_width"] = 200.into();
                            state["screenshot_original_height"] = 100.into();
                            factors.insert(session, 200.0 / width as f64);
                        } else {
                            factors.remove(&session);
                        }
                        let (w, h) = (width as u32, width as u32 / 2);
                        let image = png_of(w, h, &vec![9; (w * h * 4) as usize]);
                        json!({"ok": true, "result": {"content": [{"type": "image", "data": image, "mimeType": "image/png"}], "structuredContent": state}})
                    }
                    "end_session" => {
                        factors.remove(&session);
                        json!({"ok": true, "result": {}})
                    }
                    "list_windows" => {
                        json!({"ok": true, "result": {"structuredContent": {"windows": []}}})
                    }
                    _ if args.get("target").is_some() => refused("target is read on Linux only"),
                    _ if windowless && args["scope"] != "desktop" => {
                        refused("desktop_scope_disabled: pass scope=\"desktop\"")
                    }
                    "click" | "drag" if windowless && args["delivery_mode"] != "foreground" => {
                        refused(
                            "a desktop click moves the user's real pointer: pass delivery_mode=\"foreground\"",
                        )
                    }
                    name => {
                        let mut text = String::new();
                        if matches!(name, "click" | "drag") {
                            let factor = factors.get(&session).copied().unwrap_or(1.0);
                            let points: Vec<f64> = ["x", "y", "from_x", "from_y", "to_x", "to_y"]
                                .iter()
                                .filter_map(|k| args[k].as_f64())
                                .map(|v| v * factor / 2.0)
                                .collect();
                            text = format!(
                                "Sent {name} to screen-point ({},{})",
                                points[0], points[1]
                            );
                            log.lock().unwrap().push(points);
                        }
                        json!({"ok": true, "result": {"content": [{"type": "text", "text": text}]}})
                    }
                };
                let mut out = serde_json::to_vec(&reply).unwrap();
                out.push(b'\n');
                reader.into_inner().write_all(&out).await.unwrap();
            }
        });
        (socket, landed, driver)
    }

    #[tokio::test]
    async fn a_capture_whose_full_size_reset_fails_fails_and_no_click_goes_with_its_scaling() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, landed, driver) = retina_driver(dir.path(), true);
        let cua = cua_on(&socket, Os::Mac);
        let shot = cua.state(json!({"max_edge": 50})).await;
        assert!(
            shot.as_ref()
                .is_err_and(|e| e.contains("mapping is unknown")),
            "{:?}",
            shot.map(|o| o.text)
        );
        let click = json!({"action": "click", "x": 10.0, "y": 5.0, "scale": 2.0});
        let out = cua.act(click).await;
        assert!(
            out.as_ref().map_or(true, |o| o.is_error),
            "{:?}",
            out.map(|o| o.text)
        );
        assert!(
            landed.lock().unwrap().is_empty(),
            "{:?}",
            landed.lock().unwrap()
        );
        driver.abort();
    }

    #[tokio::test]
    async fn off_linux_windowless_input_goes_with_desktop_scope_and_no_target() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, landed, driver) = retina_driver(dir.path(), false);
        let cua = cua_on(&socket, Os::Mac);
        cua.state(json!({"max_edge": 50})).await.unwrap();
        for act in [
            json!({"action": "click", "x": 10.0, "y": 5.0}),
            json!({"action": "drag", "x": 10.0, "y": 5.0, "to_x": 20.0, "to_y": 10.0}),
            json!({"action": "key", "key": "escape"}),
        ] {
            let out = cua.act(act).await.unwrap();
            assert!(!out.is_error, "{}", out.text);
        }
        assert_eq!(landed.lock().unwrap().len(), 2);
        driver.abort();
    }

    #[tokio::test]
    async fn a_retina_desktop_click_lands_where_the_screenshot_showed_after_a_shot_a_zoom_or_a_full_shot()
     {
        let dir = tempfile::tempdir().unwrap();
        let (socket, landed, driver) = retina_driver(dir.path(), false);
        let cua = cua_on(&socket, Os::Mac);
        let click = json!({"action": "click", "x": 10.0, "y": 5.0, "max_edge": 50});
        cua.state(json!({"max_edge": 50})).await.unwrap();
        let clicked = cua.act(click.clone()).await.unwrap();
        assert!(
            clicked.text.contains("screen-point (10,5)"),
            "{}",
            clicked.text
        );
        let drag = json!({"action": "drag", "x": 10.0, "y": 5.0, "to_x": 20.0, "to_y": 10.0, "max_edge": 50});
        cua.act(drag).await.unwrap();
        let zoom = json!({"region": {"x": 5, "y": 5, "w": 10, "h": 5}, "session": DEFAULT_SESSION});
        let zoomed = cua.look(&zoom).await.unwrap();
        assert!(zoomed.text.contains("px*0.25"), "{}", zoomed.text);
        cua.act(click).await.unwrap();
        cua.state(json!({"max_edge": 0})).await.unwrap();
        cua.act(json!({"action": "click", "x": 40.0, "y": 20.0}))
            .await
            .unwrap();
        assert_eq!(
            *landed.lock().unwrap(),
            [
                vec![20.0, 10.0],
                vec![20.0, 10.0, 40.0, 20.0],
                vec![20.0, 10.0],
                vec![20.0, 10.0]
            ]
        );
        driver.abort();
    }

    fn scaling_driver(dir: &Path) -> (PathBuf, Clicks, tokio::task::JoinHandle<()>) {
        let socket = endpoint(dir);
        let listener = Fake::bind(&socket).unwrap();
        let clicks = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(f64, f64)>::new()));
        let log = std::sync::Arc::clone(&clicks);
        let driver = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                let args = &req["args"];
                let reply = match req["name"].as_str().unwrap_or_default() {
                    "get_desktop_state" => {
                        let scale = match args["max_image_dimension"].as_u64() {
                            Some(edge) if edge > 0 => 1280.0 / edge as f64,
                            _ => 1.0,
                        };
                        json!({"ok": true, "result": {"structuredContent": {"frame_scale": scale}}})
                    }
                    name => {
                        if name == "click" {
                            log.lock()
                                .unwrap()
                                .push((args["x"].as_f64().unwrap(), args["y"].as_f64().unwrap()));
                        }
                        json!({"ok": true, "result": {"structuredContent": {}}})
                    }
                };
                let mut out = serde_json::to_vec(&reply).unwrap();
                out.push(b'\n');
                reader.into_inner().write_all(&out).await.unwrap();
            }
        });
        (socket, clicks, driver)
    }

    #[tokio::test]
    async fn window_lines_and_click_replies_are_in_screenshot_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let listener = Fake::bind(&socket).unwrap();
        let driver = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                let args = &req["args"];
                let reply = match req["name"].as_str().unwrap_or_default() {
                    "list_windows" => {
                        json!({"ok": true, "result": {"structuredContent": {"windows": [{"pid": 7, "window_id": 9, "app_name": "Mousepad", "title": "notes", "bounds": {"x": 100, "y": 50, "width": 640, "height": 480}, "z_index": 1}]}}})
                    }
                    "get_desktop_state" => {
                        let scale = match args["max_image_dimension"].as_u64() {
                            Some(edge) if edge > 0 => 1280.0 / edge as f64,
                            _ => 1.0,
                        };
                        json!({"ok": true, "result": {"structuredContent": {"frame_scale": scale}}})
                    }
                    "click" => {
                        let text = format!(
                            "\u{2705} Sent screen-absolute click at ({},{}) (desktop scope).",
                            args["x"], args["y"]
                        );
                        json!({"ok": true, "result": {"content": [{"type": "text", "text": text}]}})
                    }
                    _ => json!({"ok": true, "result": {"structuredContent": {}}}),
                };
                let mut out = serde_json::to_vec(&reply).unwrap();
                out.push(b'\n');
                reader.into_inner().write_all(&out).await.unwrap();
            }
        });
        let cua = cua(&socket);
        let state = cua.state(json!({})).await.unwrap();
        assert!(
            state.text.contains("\"notes\" at (80,40) 512x384"),
            "{}",
            state.text
        );
        let act = json!({"action": "click", "x": 100.0, "y": 40.0});
        let out = cua.act(act).await.unwrap();
        assert!(
            out.text.contains("click at (100,40) (desktop"),
            "{}",
            out.text
        );
        driver.abort();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn an_input_session_dropped_mid_call_is_still_ended() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let listener = Fake::bind(&socket).unwrap();
        let ended = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = std::sync::Arc::clone(&ended);
        let driver = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let log = std::sync::Arc::clone(&log);
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    reader.read_line(&mut line).await.unwrap();
                    let req: Value = serde_json::from_str(&line).unwrap();
                    let session = req["args"]["session"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    let reply = match req["name"].as_str().unwrap_or_default() {
                        "type_text" => return std::future::pending().await,
                        "end_session" => {
                            log.lock().unwrap().push(session);
                            json!({"ok": true, "result": {}})
                        }
                        "list_windows" => {
                            json!({"ok": true, "result": {"structuredContent": {"windows": [{"pid": 7, "window_id": 9, "app_name": "Mousepad", "title": "notes", "bounds": {"x": 0, "y": 0, "width": 800, "height": 600}, "z_index": 1}]}}})
                        }
                        _ => json!({"ok": true, "result": {"structuredContent": {}}}),
                    };
                    let mut out = reply.to_string().into_bytes();
                    out.push(b'\n');
                    reader.into_inner().write_all(&out).await.unwrap();
                });
            }
        });
        let cua = cua(&socket);
        let act = cua.act(json!({"action": "type", "text": "hello"}));
        assert!(
            tokio::time::timeout(Duration::from_secs(2), act)
                .await
                .is_err()
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ended
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.starts_with("input-"))
        {
            assert!(Instant::now() < deadline, "{:?}", ended.lock().unwrap());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        driver.abort();
    }

    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn keys_for_the_frontmost_window_go_to_the_desktop_off_linux() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let listener = Fake::bind(&socket).unwrap();
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let log = std::sync::Arc::clone(&sent);
        let driver = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                let reply = match req["name"].as_str().unwrap_or_default() {
                    "list_windows" => {
                        json!({"ok": true, "result": {"structuredContent": {"windows": [{"pid": 7, "window_id": 9, "app_name": "Notepad.exe", "title": "notes", "bounds": {"x": 0, "y": 0, "width": 800, "height": 600}, "z_index": 1}]}}})
                    }
                    "press_key" | "type_text" => {
                        log.lock().unwrap().push(req.clone());
                        json!({"ok": true, "result": {}})
                    }
                    _ => json!({"ok": true, "result": {"structuredContent": {}}}),
                };
                let mut out = reply.to_string().into_bytes();
                out.push(b'\n');
                reader.into_inner().write_all(&out).await.unwrap();
            }
        });
        let cua = cua_on(&socket, Os::Mac);
        cua.act(json!({"action": "key", "key": "escape"}))
            .await
            .unwrap();
        cua.act(json!({"action": "type", "text": "hi"}))
            .await
            .unwrap();
        let sent = sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 2, "{sent:?}");
        for req in &sent {
            assert_eq!(req["args"]["scope"], "desktop", "{req}");
            assert!(req["args"]["target"].is_null(), "{req}");
            assert!(req["args"]["pid"].is_null(), "{req}");
        }
        driver.abort();
    }
    #[tokio::test]
    async fn the_settle_sampler_starts_with_the_first_computer_call() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let calls = std::sync::Arc::default();
        let driver = window_driver(&socket, &calls, usize::MAX);
        let cua = cua(&socket);
        cua.run("a", "desk_title_at", json!({"x": 1, "y": 1}))
            .await
            .unwrap();
        cua.run("a", "desk_computer_scale", json!({}))
            .await
            .unwrap();
        assert!(cua.volatile.get().is_none());
        cua.run("a", "desk_computer_state", json!({}))
            .await
            .unwrap();
        assert!(cua.volatile.get().is_some());
        driver.abort();
    }

    #[tokio::test]
    async fn callers_without_a_session_get_their_own_and_an_ended_default_is_named_anew() {
        let dir = tempfile::tempdir().unwrap();
        let socket = endpoint(dir.path());
        let listener = Fake::bind(&socket).unwrap();
        let sessions = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let log = std::sync::Arc::clone(&sessions);
        let driver = tokio::spawn(async move {
            let mut ended = Vec::<String>::new();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                let name = req["name"].as_str().unwrap_or_default().to_owned();
                let session = req["args"]["session"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                log.lock().unwrap().push((name.clone(), session.clone()));
                let reply = if ended.contains(&session) {
                    json!({"ok": false, "error": format!("session {session} ended")})
                } else if name == "end_session" {
                    ended.push(session);
                    json!({"ok": true, "result": {}})
                } else if name == "list_windows" {
                    json!({"ok": true, "result": {"structuredContent": {"windows": [{"pid": 7, "window_id": 9, "app_name": "Mousepad", "title": "notes", "bounds": {"x": 0, "y": 0, "width": 800, "height": 600}, "z_index": 1}]}}})
                } else if name == "get_window_state" {
                    json!({"ok": true, "result": {"structuredContent": {"elements": [{"element_index": 0, "element_token": "t0", "role": "push button", "label": "OK"}]}}})
                } else {
                    json!({"ok": true, "result": {"structuredContent": {}}})
                };
                let mut out = reply.to_string().into_bytes();
                out.push(b'\n');
                reader.into_inner().write_all(&out).await.unwrap();
            }
        });
        let cua = cua(&socket);
        let state = |agent: &'static str| cua.run(agent, "desk_computer_state", json!({}));
        state("a").await.unwrap();
        let other = cua
            .run(
                "b:c",
                "desk_computer_act",
                json!({"action": "key", "key": "x"}),
            )
            .await;
        assert!(!other.unwrap().is_error);
        let foreign = cua
            .run(
                "b:c",
                "desk_computer_act",
                json!({"action": "click", "element": 0}),
            )
            .await
            .unwrap_err();
        assert_eq!(foreign, "no element list yet; call computer_state first");
        let own = cua.run(
            "a",
            "desk_computer_act",
            json!({"action": "click", "element": 0}),
        );
        assert!(!own.await.unwrap().is_error);
        cua.run("a", "desk_computer_end", Value::Null)
            .await
            .unwrap();
        let again = state("a").await.unwrap();
        assert!(!again.is_error, "{}", again.text);
        let named: Vec<String> = sessions
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, _)| n == "list_windows" || n == "end_session")
            .map(|(_, s)| s.clone())
            .filter(|s| s.starts_with("computer"))
            .collect();
        assert_eq!(
            named,
            ["computer-a", "computer-b-c", "computer-a", "computer-a-1"]
        );
        driver.abort();
    }

    #[test]
    fn shot_points_divides_each_point_by_the_scale() {
        assert_eq!(
            shot_points(
                "\u{2705} Sent screen-absolute click at (325,296) (desktop scope).",
                1.25
            ),
            "\u{2705} Sent screen-absolute click at (260,237) (desktop scope)."
        );
        assert_eq!(
            shot_points("Clicked at (100.0, 50.0) x 2", 1.25),
            "Clicked at (80,40) x 2"
        );
        assert_eq!(
            shot_points("at (325,296) (a, b) (", 1.0),
            "at (325,296) (a, b) ("
        );
        assert_eq!(shot_points("(a, b) (8,4", 2.0), "(a, b) (8,4");
    }

    fn fast_png(w: u32, h: u32, pixels: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(pixels).unwrap();
        writer.finish().unwrap();
        out
    }

    #[test]
    fn shrink_keeps_every_pixel_of_a_flat_screen_as_webp_and_turns_noise_into_jpeg() {
        let (w, h) = (320u32, 200u32);
        let flat: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                let v = if y % 20 == 0 || x % 40 == 0 { 40 } else { 230 };
                [v, v, (x / 40 * 30) as u8, 255]
            })
            .collect();
        let driver = fast_png(w, h, &flat);
        let small = B64.decode(shrink(&B64.encode(&driver)).unwrap()).unwrap();
        assert!(small.len() < driver.len());
        let mut webp = image_webp::WebPDecoder::new(std::io::Cursor::new(&small)).unwrap();
        assert_eq!((webp.dimensions(), webp.has_alpha()), ((w, h), false));
        let mut decoded = vec![0; webp.output_buffer_size().unwrap()];
        webp.read_image(&mut decoded).unwrap();
        let rgb: Vec<u8> = flat
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        assert_eq!(decoded, rgb);
        assert!(shrink("iVBORw0KGgoAAAANSUhEUg==").is_none());

        let mut seed = 7u32;
        let noise: Vec<u8> = (0..w * h * 4)
            .map(|i| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                if i % 4 == 3 { 255 } else { (seed >> 24) as u8 }
            })
            .collect();
        let photo = B64
            .decode(shrink(&B64.encode(fast_png(w, h, &noise))).unwrap())
            .unwrap();
        assert!(photo.starts_with(&[0xff, 0xd8]), "{:?}", &photo[..4]);
    }

    #[test]
    fn a_screenshot_reports_its_format_size_and_input_tokens() {
        let (w, h) = (1024u32, 640u32);
        let flat = vec![200u8; (w * h * 3) as usize];
        let mut seed = 7u32;
        let noise: Vec<u8> = (0..w * h * 3)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (seed >> 24) as u8
            })
            .collect();
        let rgba: Vec<u8> = flat
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect();
        for (format, image) in [
            ("webp", encode(w, h, &flat).unwrap()),
            ("jpeg", encode(w, h, &noise).unwrap()),
            ("png", fast_png(w, h, &rgba)),
        ] {
            assert_eq!(image_size(&image), Some((format, w, h)));
            let kb = image.len().div_ceil(1024);
            assert_eq!(
                weigh(&B64.encode(&image)),
                format!("1024x640 {format}, {kb} KB, about 874 input tokens")
            );
        }
        assert_eq!(
            image_size(&encode(1, 3, &[9; 9]).unwrap()),
            Some(("webp", 1, 3))
        );
        assert_eq!(weigh("AAAA"), "1 KB");
    }

    #[test]
    fn an_element_has_moved_once_enough_of_its_pixels_changed() {
        let screen = Screen {
            width: 4,
            height: 2,
            rgb: vec![100; 4 * 2 * 3],
        };
        let rect = [1, 0, 2, 2];
        let mut now = vec![100 + STILL_TOLERANCE; 2 * 2 * 3];
        assert!(!moved(&screen, rect, &now));
        now[0] = 255;
        assert!(moved(&screen, rect, &now));
        assert!(moved(&screen, rect, &now[3..]));
    }

    #[test]
    fn a_change_counts_outside_the_volatile_blocks_only() {
        let first = Frame::Blocks(vec![1, 2, 3]);
        let toggling = [Block::Quiet, Block::Toggling(vec![2, 9]), Block::Churning];
        assert!(!first.changes(&Frame::Blocks(vec![1, 2, 3]), &[]));
        assert!(first.changes(&Frame::Blocks(vec![1, 9, 3]), &[]));
        assert!(!first.changes(&Frame::Blocks(vec![1, 9, 4]), &toggling));
        assert!(first.changes(&Frame::Blocks(vec![1, 8, 3]), &toggling));
        assert!(first.changes(&Frame::Blocks(vec![7, 2, 3]), &toggling));
        assert!(first.changes(&Frame::Blocks(vec![1, 2]), &[]));
        assert!(first.changes(&Frame::Thumb(vec![1, 2, 3]), &[]));
        let thumb = Frame::Thumb(vec![100; 6]);
        assert!(!thumb.changes(&Frame::Thumb(vec![100 + STILL_TOLERANCE; 6]), &[]));
        assert!(thumb.changes(&Frame::Thumb(vec![101 + STILL_TOLERANCE; 6]), &[]));
    }

    #[test]
    fn fit_averages_blocks_down_to_the_long_edge() {
        let (w, h) = (8u32, 4u32);
        let rgb: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let v = if (i % w) % 2 == 0 { 0 } else { 200 };
                [v, v, 50]
            })
            .collect();
        let (tw, th, small) = fit(w, h, &rgb, 4);
        assert_eq!((tw, th), (4, 2));
        assert_eq!(small, [100, 100, 50].repeat(8));
    }

    #[test]
    fn frames_differ_only_past_the_tolerance() {
        let a = [200u8, 200, 200, 255];
        assert!(!differs(&a, &[200 - STILL_TOLERANCE, 200, 200, 255]));
        assert!(differs(&a, &[199 - STILL_TOLERANCE, 200, 200, 255]));
        assert!(differs(&a, &a[..3]));
    }

    #[test]
    fn key_aliases_become_driver_names_and_a_key_combination_a_hotkey() {
        let rewrite = |action: &str, key: Option<&str>, keys: Option<&[&str]>| {
            let mut a = ActArgs {
                action: action.to_owned(),
                key: key.map(str::to_owned),
                keys: keys.map(|k| k.iter().map(|s| (*s).to_owned()).collect()),
                ..ActArgs::default()
            };
            driver_keys(&mut a, Os::Linux).map(|()| (a.action, a.key, a.keys))
        };
        let names = |k: &[&str]| Some(k.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
        assert_eq!(
            rewrite("key", Some("ctrl+l"), None).unwrap(),
            (
                "hotkey".to_owned(),
                Some("ctrl+l".to_owned()),
                names(&["ctrl", "l"])
            )
        );
        assert_eq!(
            rewrite("key", Some("Prior"), None).unwrap(),
            ("key".to_owned(), Some("pageup".to_owned()), None)
        );
        assert_eq!(
            rewrite("key", Some("Return"), None).unwrap().1.as_deref(),
            Some("return")
        );
        assert_eq!(
            rewrite("hotkey", None, Some(&["Control_L", "Next"]))
                .unwrap()
                .2,
            names(&["ctrl", "pagedown"])
        );
        assert_eq!(
            rewrite("hotkey", None, Some(&["CTRL+SHIFT+t"])).unwrap().2,
            names(&["ctrl", "shift", "t"])
        );
        assert_eq!(
            rewrite("hotkey", None, Some(&["BackSpace"])).unwrap(),
            (
                "key".to_owned(),
                Some("backspace".to_owned()),
                Some(vec!["BackSpace".to_owned()])
            )
        );
        assert_eq!(
            rewrite("click", None, None).unwrap(),
            ("click".to_owned(), None, None)
        );
        assert!(rewrite("key", None, None).is_err());
        assert!(rewrite("hotkey", None, Some(&["ctrl+"])).is_err());
    }

    #[test]
    fn a_point_off_the_screen_is_refused_in_screenshot_pixels() {
        let act = |action: &str, element: Option<i64>, xy: [Option<f64>; 4]| ActArgs {
            action: action.to_owned(),
            element,
            x: xy[0],
            y: xy[1],
            to_x: xy[2],
            to_y: xy[3],
            ..ActArgs::default()
        };
        let screen = (1280, 800);
        let at = |x: f64, y: f64| [Some(x), Some(y), None, None];
        assert_eq!(
            off_screen(&act("click", None, at(1500.0, 400.0)), screen, 1.0).as_deref(),
            Some(
                "(1500,400) is off the screen, which is 1280x800 screenshot pixels (x 0 to 1279, y 0 to 799); the click did not run: take a new screenshot and use a point inside it"
            )
        );
        assert_eq!(
            off_screen(&act("click", None, at(1279.0, 799.0)), screen, 1.0),
            None
        );
        for (action, x, y) in [
            ("click", -1.0, 10.0),
            ("scroll", 640.0, 950.0),
            ("type", 10.0, 800.0),
        ] {
            assert!(
                off_screen(&act(action, None, at(x, y)), screen, 1.0).is_some(),
                "{action} {x},{y}"
            );
        }
        assert_eq!(
            off_screen(&act("click", Some(3), at(1500.0, 400.0)), screen, 1.0),
            None
        );
        assert_eq!(off_screen(&act("key", None, [None; 4]), screen, 1.0), None);
        let drag = [Some(10.0), Some(10.0), Some(10.0), Some(820.0)];
        let refused = off_screen(&act("drag", Some(3), drag), screen, 1.0).unwrap_or_default();
        assert!(
            refused.starts_with("(10,820) is off the screen"),
            "{refused}"
        );
        let small =
            off_screen(&act("click", None, at(1000.0, 10.0)), screen, 1.28).unwrap_or_default();
        assert!(small.contains("1000x625 screenshot pixels"), "{small}");
        assert_eq!(
            off_screen(&act("click", None, at(999.0, 624.0)), screen, 1.28),
            None
        );
    }

    #[test]
    fn a_hidden_modal_dialog_names_the_window_it_blocks() {
        let modal = |owner: &str, pid: Option<u32>| Modal {
            title: "Save As".to_owned(),
            pid,
            owner: owner.to_owned(),
            place: "hidden".to_owned(),
        };
        assert_eq!(
            modal_line(&modal("Untitled 1 - Mousepad", Some(833))),
            "! modal dialog \"Save As\" (pid=833) is open but hidden: \"Untitled 1 - Mousepad\" takes no input until it closes. Bring it to the front with launch_app of its app, then finish it or press Escape to cancel it.\n"
        );
        assert!(
            modal_line(&modal("", None)).starts_with(
                "! modal dialog \"Save As\" is open but hidden: its app takes no input"
            )
        );
    }

    #[test]
    fn a_left_click_a_navigation_key_or_a_typed_newline_commits() {
        let act = |action: &str, key: Option<&str>, keys: Option<&[&str]>, button: Option<&str>| {
            ActArgs {
                action: action.to_owned(),
                key: key.map(str::to_owned),
                keys: keys.map(|k| k.iter().map(|s| (*s).to_owned()).collect()),
                button: button.map(str::to_owned),
                ..ActArgs::default()
            }
        };
        assert!(commits(&act("click", None, None, None)));
        assert!(commits(&act("double_click", None, None, Some("left"))));
        assert!(!commits(&act("click", None, None, Some("right"))));
        assert!(!commits(&act("right_click", None, None, None)));
        assert!(commits(&act("key", Some("Return"), None, None)));
        assert!(commits(&act("key", Some("KP_Enter"), None, None)));
        assert!(!commits(&act("key", Some("Tab"), None, None)));
        assert!(commits(&act("hotkey", None, Some(&["alt", "Left"]), None)));
        assert!(!commits(&act("hotkey", None, Some(&["ctrl", "l"]), None)));
        assert!(!commits(&act("type", None, None, None)));
        let typed = |text: &str| ActArgs {
            action: "type".to_owned(),
            text: Some(text.to_owned()),
            ..ActArgs::default()
        };
        assert!(commits(&typed("https://example.com\n")));
        assert!(!commits(&typed("Ada Lovelace")));
    }

    #[test]
    fn return_enter_and_a_typed_line_confirm_and_only_left_clicks_click_left() {
        let act =
            |action: &str, key: Option<&str>, text: Option<&str>, button: Option<&str>| ActArgs {
                action: action.to_owned(),
                key: key.map(str::to_owned),
                text: text.map(str::to_owned),
                button: button.map(str::to_owned),
                ..ActArgs::default()
            };
        assert!(confirms(&act("key", Some("Return"), None, None)));
        assert!(confirms(&act("key", Some("KP_Enter"), None, None)));
        assert!(!confirms(&act("key", Some("F5"), None, None)));
        assert!(confirms(&act("type", None, Some("name.txt\n"), None)));
        assert!(!confirms(&act("type", None, Some("name.txt"), None)));
        assert!(clicks_left(&act("click", None, None, None)));
        assert!(!clicks_left(&act("click", None, None, Some("right"))));
        assert!(!clicks_left(&act("right_click", None, None, None)));
    }

    #[test]
    fn a_failed_type_counts_the_characters_that_landed() {
        let mpx = json!({"isError": true, "content": [{"type": "text", "text": "Typed 5 character(s)"}], "structuredContent": {"skipped_characters": "三四"}});
        assert_eq!(landed("一二三四五", &mpx), 3);
        let xtest = json!({"isError": true, "content": [{"type": "text", "text": "type_text failed: typed 2 of 5 characters: no spare keycode"}]});
        assert_eq!(landed("一二三四五", &xtest), 2);
        assert_eq!(landed("一二", &json!({"isError": true})), 0);
    }

    #[test]
    fn only_a_newline_after_other_text_is_split_off() {
        assert_eq!(before_newline("/tmp/name.txt\n"), Some("/tmp/name.txt"));
        assert_eq!(before_newline("a\nb\r"), Some("a\nb"));
        for kept in ["\n", "plain", "a\nb"] {
            assert_eq!(before_newline(kept), None);
        }
    }

    #[test]
    fn a_toolbar_button_and_its_menu_item_have_one_label_key() {
        assert_eq!(label_key("Save As..."), "save as");
        assert_eq!(label_key("Save _As\u{2026}      "), "save as");
        assert_eq!(label_key("Go to..."), "go to");
        assert_eq!(label_key("Find and Replace..."), "find and replace");
        assert_ne!(label_key("Save"), label_key("Save As..."));
    }
}
