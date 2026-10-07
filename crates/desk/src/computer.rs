//! cua-driver client: line-delimited JSON over its unix socket.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::rpc::Output;
use super::timing;
use super::xshm::Grabber;

const STATE_ELEMENTS: u64 = 300;
const TREE_CHARS: usize = 15_000;
const MIN_WINDOW: i64 = 10;
const SHELL_APPS: [&str; 2] = ["Xfce4-panel", "Xfdesktop"];
const SETTLE: Duration = Duration::from_millis(600);
const STILL_POLL: Duration = Duration::from_millis(40);
const SHM_POLL: Duration = Duration::from_millis(10);
const STILL_FOR: Duration = Duration::from_millis(40);
const STILL_EDGE: u32 = 160;
const STILL_TOLERANCE: u8 = 48;
const JPEG_QUALITY: u8 = 80;
const PHOTO_COLORS: usize = 2500;
const COLOR_STRIDE: usize = 7;
const MAX_WAIT_SECS: f64 = 10.0;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_RETRY: Duration = Duration::from_millis(250);
const CONNECT_WAIT: Duration = Duration::from_secs(10);
const DEFAULT_SESSION: &str = "cocoonbox";
const RETRIED: [&str; 4] = [
    "list_windows",
    "get_window_state",
    "get_desktop_state",
    "end_session",
];

/// Screen executor client; remembers the element tokens of the latest window state.
pub struct Cua {
    socket: PathBuf,
    last: Mutex<HashMap<String, Snapshot>>,
    input: Mutex<()>,
    /// The cbplan family: settles on a still screen, recompresses screenshots and runs action lists.
    cbplan: bool,
    /// The cbplan family's MIT-SHM grabber, opened on first use and again after a failure.
    grabber: std::sync::Mutex<Option<Grabber>>,
}

impl Cua {
    /// Creates a client for the driver socket at `socket`; `cbplan` turns on the cbplan family's behaviour.
    pub fn new(socket: PathBuf, cbplan: bool) -> Self {
        Self {
            socket,
            last: Mutex::new(HashMap::new()),
            input: Mutex::new(()),
            cbplan,
            grabber: std::sync::Mutex::new(None),
        }
    }

    /// `computer_state`: window list, accessibility elements and a desktop screenshot.
    pub async fn state(&self, args: Value) -> Result<Output, String> {
        let session = session_of(&args);
        let a: StateArgs =
            serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))?;
        let windows = self
            .request(&session, "list_windows", json!({"on_screen_only": true}))
            .await?;
        timing::mark("windows");
        let listed = window_list(&windows);
        let mut text = String::from("Windows (frontmost first):\n");
        for w in &listed {
            let _ = writeln!(
                text,
                "- pid={} window_id={} {} \"{}\" at ({},{}) {}x{}",
                w.pid, w.window_id, w.app, w.title, w.x, w.y, w.w, w.h
            );
        }
        let target = match a.pid {
            Some(pid) => Some((pid, a.window_id)),
            None => listed
                .iter()
                .find(|w| !SHELL_APPS.contains(&w.app.as_str()))
                .or(listed.first())
                .map(|w| (w.pid, Some(w.window_id))),
        };
        let mut elements = None;
        if let Some((pid, window_id)) = target {
            let mut req = json!({
                "pid": pid,
                "include_screenshot": false,
                "max_elements": STATE_ELEMENTS,
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
            timing::mark("elements");
        }
        // the capture comes last: a later get_window_state replaces the session's pixel-click context
        let (image_b64, scale) = self.capture(&session, a.max_edge).await?;
        if let Some((pid, window_id, state)) = elements {
            let (tree, tokens) = element_tree(&state, scale);
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
                    tokens,
                },
            );
        }
        Ok(Output {
            text,
            is_error: false,
            image_b64,
        })
    }

    /// `computer_act`: one input action followed by a fresh desktop screenshot; on cbplan also a list of `actions`.
    pub async fn act(&self, args: Value) -> Result<Output, String> {
        let session = session_of(&args);
        if self.cbplan && args.get("actions").is_some() {
            let list: ActList =
                serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))?;
            return self.act_list(&session, list).await;
        }
        let a: ActArgs =
            serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))?;
        self.act_one(&session, a).await
    }

    /// Runs the actions in order, each settled, stops at the first failure and returns the last screenshot.
    async fn act_list(&self, session: &str, list: ActList) -> Result<Output, String> {
        let total = list.actions.len();
        let mut lines = Vec::with_capacity(total);
        let mut out = Output::default();
        for (i, mut a) in list.actions.into_iter().enumerate() {
            a.max_edge = list.max_edge;
            let action = a.action.clone();
            let step = self.act_one(session, a).await.unwrap_or_else(Output::error);
            let failed = if step.is_error { " failed" } else { "" };
            lines.push(format!("{}. {action}{failed}: {}", i + 1, step.text));
            out.image_b64 = step.image_b64;
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

    async fn act_one(&self, session: &str, a: ActArgs) -> Result<Output, String> {
        timing::note("action", &a.action);
        if a.action == "wait" {
            let secs = a.seconds.unwrap_or(1.0).clamp(0.1, MAX_WAIT_SECS);
            tokio::time::sleep(Duration::from_secs_f64(secs)).await;
            timing::mark("wait");
            return Ok(Output {
                text: format!("waited {secs:.1}s"),
                is_error: false,
                image_b64: self.screenshot(session, a.max_edge).await?,
            });
        }
        if a.action == "type" && a.text.as_deref().is_some_and(|t| !t.is_ascii()) {
            return self.type_mixed(session, &a).await;
        }
        if a.action == "launch_app" {
            return self.launch(session, &a).await;
        }
        let (tool, req) = self.action_request(session, &a).await?;
        let input = self.input.lock().await;
        timing::mark("resolve");
        let before = self.before_input().await;
        let result = self.request(session, tool, req).await;
        timing::mark("act");
        self.settle(before).await;
        let shot = self.screenshot(session, a.max_edge).await?;
        drop(input);
        let (text, is_error) = match result {
            Ok(v) => (content_text(&v), false),
            Err(e) => (e, true),
        };
        Ok(Output {
            text,
            is_error,
            image_b64: shot,
        })
    }

    /// Base64 PNG of the whole desktop, scaled so its long edge is at most `max_edge`.
    pub async fn screenshot(
        &self,
        session: &str,
        max_edge: Option<u32>,
    ) -> Result<Option<String>, String> {
        Ok(self.capture(session, max_edge).await?.0)
    }

    /// Screenshot in a one-shot driver session that is ended right away.
    pub async fn screenshot_once(&self, max_edge: Option<u32>) -> Result<Option<String>, String> {
        let session = format!("shot-{}", uuid::Uuid::new_v4().simple());
        let shot = self.screenshot(&session, max_edge).await;
        self.request(&session, "end_session", json!({})).await?;
        timing::mark("end");
        shot
    }

    /// Ends a driver session, releasing its input devices.
    pub async fn end(&self, args: Value) -> Result<String, String> {
        let session = session_of(&args);
        self.request(&session, "end_session", json!({})).await?;
        self.last.lock().await.remove(&session);
        Ok(format!("ended session {session}"))
    }

    /// Captures the desktop; the driver maps desktop-scope pixels of this capture back to the screen.
    async fn capture(
        &self,
        session: &str,
        max_edge: Option<u32>,
    ) -> Result<(Option<String>, f64), String> {
        if self.cbplan
            && max_edge.is_none()
            && let Some((w, h, rgb)) = self
                .shm(|g| (!g.overlay_shown()?).then(|| g.rgb()).transpose())
                .flatten()
        {
            timing::mark("grab");
            if let Some(image) = encode(w, h, &rgb) {
                timing::mark("encode");
                timing::note("image_bytes", image.len());
                return Ok((Some(B64.encode(image)), 1.0));
            }
        }
        let mut args = json!({});
        if let Some(edge) = max_edge {
            args["max_image_dimension"] = edge.into();
        }
        let v = self.request(session, "get_desktop_state", args).await?;
        timing::mark("capture");
        let scale = v["structuredContent"]["frame_scale"]
            .as_f64()
            .filter(|s| *s > 0.0)
            .unwrap_or(1.0);
        let mut image = content_image(&v);
        if self.cbplan
            && let Some(smaller) = image.as_deref().and_then(shrink)
        {
            image = Some(smaller);
        }
        timing::note("image_bytes", image.as_ref().map_or(0, |b| b.len() / 4 * 3));
        Ok((image, scale))
    }

    /// Types text with non-ASCII characters: ASCII runs in one call, every other character in its own call.
    async fn type_mixed(&self, session: &str, a: &ActArgs) -> Result<Output, String> {
        let text = a.text.as_deref().unwrap_or_default();
        let input = self.input.lock().await;
        let before = self.before_input().await;
        let mut failure = None;
        if a.element.is_some() || (a.x.is_some() && a.y.is_some()) {
            let focus = ActArgs {
                action: "click".to_owned(),
                element: a.element,
                x: a.x,
                y: a.y,
                foreground: a.foreground,
                ..ActArgs::default()
            };
            let (tool, req) = self.action_request(session, &focus).await?;
            failure = self.request(session, tool, req).await.err();
        }
        let mut base = json!({});
        if let Some((pid, window_id)) = self.app_window(session).await? {
            base["pid"] = pid.into();
            base["window_id"] = window_id.into();
        }
        if a.foreground {
            base["delivery_mode"] = "foreground".into();
        }
        timing::mark("resolve");
        let runs = text_runs(text);
        let mut typed = 0;
        for run in &runs {
            if failure.is_some() {
                break;
            }
            let mut req = base.clone();
            req["text"] = (*run).into();
            match self.request(session, "type_text", req).await {
                Ok(_) => typed += run.chars().count(),
                Err(e) => failure = Some(e),
            }
        }
        timing::mark("act");
        self.settle(before).await;
        let shot = self.screenshot(session, a.max_edge).await?;
        drop(input);
        let total = text.chars().count();
        let summary = format!(
            "typed {typed} of {total} characters in {} calls (non-ASCII characters go one per call); verify on the screenshot",
            runs.len()
        );
        Ok(Output {
            text: match &failure {
                Some(e) => format!("{summary}; stopped: {e}"),
                None => summary,
            },
            is_error: failure.is_some(),
            image_b64: shot,
        })
    }

    /// Launches an app, or brings its existing window to the front when it already runs.
    async fn launch(&self, session: &str, a: &ActArgs) -> Result<Output, String> {
        let app = required(&a.app, "app")?;
        let input = self.input.lock().await;
        let before = self.before_input().await;
        let text = match self.app_windows(session, app).await?.first() {
            Some(w) => {
                let req = json!({"pid": w.pid, "window_id": w.window_id});
                match self.request(session, "bring_to_front", req).await {
                    Ok(_) => format!(
                        "{app} is already running; brought its window {} \"{}\" (pid {}) to the front",
                        w.window_id, w.title, w.pid
                    ),
                    Err(e) => format!(
                        "{app} is already running as window {} \"{}\" (pid {}); raising it failed: {e}",
                        w.window_id, w.title, w.pid
                    ),
                }
            }
            None => match self
                .request(session, "launch_app", json!({"name": app}))
                .await
            {
                Ok(v) => content_text(&v),
                Err(e) => match self.app_windows(session, app).await?.first() {
                    Some(w) => format!(
                        "{app} is running as window {} \"{}\" (pid {}); the launcher reported: {e}",
                        w.window_id, w.title, w.pid
                    ),
                    None => {
                        if let Some(probe) = before.and_then(|b| b.probe) {
                            let _ = self.request(&probe, "end_session", json!({})).await;
                        }
                        drop(input);
                        return Ok(Output {
                            text: e,
                            is_error: true,
                            image_b64: self.screenshot(session, a.max_edge).await?,
                        });
                    }
                },
            },
        };
        timing::mark("act");
        self.settle(before).await;
        let shot = self.screenshot(session, a.max_edge).await?;
        drop(input);
        Ok(Output {
            text,
            is_error: false,
            image_b64: shot,
        })
    }

    /// On cbplan, a thumbnail of the screen before an input action: over MIT-SHM, else from a probe
    /// driver session of its own, which leaves the capture context of the action's session as it was.
    async fn before_input(&self) -> Option<Before> {
        if !self.cbplan {
            return None;
        }
        if let Some(frame) = self.shm(Grabber::thumb) {
            timing::mark("probe");
            return Some(Before { probe: None, frame });
        }
        let probe = format!("still-{}", uuid::Uuid::new_v4().simple());
        let before = match self.frame(Some(&probe)).await {
            Some(frame) => Some(Before {
                probe: Some(probe),
                frame,
            }),
            None => {
                let _ = self.request(&probe, "end_session", json!({})).await;
                None
            }
        };
        timing::mark("probe");
        before
    }

    /// Waits after an input action until the screen changed from `before` and held still for
    /// [`STILL_FOR`], at most [`SETTLE`]; without `before`, the fixed [`SETTLE`]. Frames are
    /// thumbnails compared with a tolerance, so a caret or a small animation does not count as a change.
    async fn settle(&self, before: Option<Before>) {
        let Some(Before {
            probe,
            frame: first,
        }) = before
        else {
            tokio::time::sleep(SETTLE).await;
            timing::mark("settle");
            return;
        };
        let poll = if probe.is_some() {
            STILL_POLL
        } else {
            SHM_POLL
        };
        let deadline = Instant::now() + SETTLE;
        let (mut last, mut since): (Option<Vec<u8>>, Instant) = (None, Instant::now());
        let mut still = false;
        while !still && Instant::now() < deadline {
            tokio::time::sleep(poll.min(deadline.saturating_duration_since(Instant::now()))).await;
            let Some(frame) = self.frame(probe.as_deref()).await else {
                tokio::time::sleep_until(deadline).await;
                break;
            };
            if last.as_deref().is_none_or(|l| differs(l, &frame)) {
                since = Instant::now();
                last = Some(frame);
            }
            still =
                last.as_deref().is_some_and(|l| differs(&first, l)) && since.elapsed() >= STILL_FOR;
        }
        timing::mark("settle");
        timing::note("still", still);
        if let Some(probe) = probe {
            let _ = self.request(&probe, "end_session", json!({})).await;
        }
    }

    /// A thumbnail from the probe driver session `probe`, or over MIT-SHM without one.
    async fn frame(&self, probe: Option<&str>) -> Option<Vec<u8>> {
        let Some(probe) = probe else {
            return self.shm(Grabber::thumb);
        };
        let edge = json!({"max_image_dimension": STILL_EDGE});
        let v = self.request(probe, "get_desktop_state", edge).await.ok()?;
        let data = B64.decode(content_image(&v)?).ok()?;
        Some(decode_png(&data)?.1)
    }

    /// Runs `read` on the MIT-SHM grabber, opening it when needed; `None` when it is unavailable or fails.
    fn shm<T>(&self, read: impl FnOnce(&Grabber) -> Result<T, String>) -> Option<T> {
        let mut slot = self
            .grabber
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            match Grabber::open() {
                Ok(g) => *slot = Some(g),
                Err(e) => {
                    eprintln!("toolhost: MIT-SHM capture unavailable, using cua-driver: {e}");
                    return None;
                }
            }
        }
        match read(slot.as_ref()?) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("toolhost: MIT-SHM capture: {e}");
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
        Ok(window_list(&windows)
            .iter()
            .find(|w| !SHELL_APPS.contains(&w.app.as_str()))
            .map(|w| (w.pid, w.window_id)))
    }

    async fn action_request(
        &self,
        session: &str,
        a: &ActArgs,
    ) -> Result<(&'static str, Value), String> {
        let desktop = json!({"kind": "desktop", "display_id": "primary"});
        let mut req = json!({});
        let tool = match a.action.as_str() {
            "click" | "double_click" | "right_click" => {
                let tool = match (a.action.as_str(), a.element.is_some()) {
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
                req = json!({"from_x": x, "from_y": y, "to_x": tx, "to_y": ty, "target": desktop});
                return Ok(("drag", req));
            }
            other => return Err(format!("unknown action {other}")),
        };
        if let Some(index) = a.element {
            let last = self.last.lock().await;
            let snap = last
                .get(session)
                .ok_or("no element list yet; call computer_state first")?;
            let token = snap.tokens.get(&index).ok_or_else(|| {
                format!(
                    "element {index} is not in the latest computer_state; call computer_state again"
                )
            })?;
            req["pid"] = snap.pid.into();
            req["element_token"] = token.as_str().into();
            if let Some(id) = snap.window_id {
                req["window_id"] = id.into();
            }
        } else if let (Some(x), Some(y)) = (a.x, a.y) {
            req["x"] = x.into();
            req["y"] = y.into();
            req["target"] = desktop;
        } else if matches!(tool, "click" | "double_click" | "right_click") {
            return Err(format!("{} needs element or x and y", a.action));
        } else if let Some((pid, window_id)) = self.app_window(session).await? {
            req["pid"] = pid.into();
            req["window_id"] = window_id.into();
        }
        if a.foreground {
            req["delivery_mode"] = "foreground".into();
        }
        Ok((tool, req))
    }

    async fn request(&self, session: &str, name: &str, args: Value) -> Result<Value, String> {
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
        let result = v["result"].take();
        if result["isError"] == true {
            return Err(format!("{name}: {}", content_text(&result)));
        }
        Ok(result)
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
    async fn connect(&self) -> Result<UnixStream, String> {
        let deadline = tokio::time::Instant::now() + CONNECT_WAIT;
        loop {
            match UnixStream::connect(&self.socket).await {
                Ok(stream) => return Ok(stream),
                Err(e)
                    if matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound)
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(CONNECT_RETRY).await;
                }
                Err(e) => {
                    return Err(format!("connect cua-driver {}: {e}", self.socket.display()));
                }
            }
        }
    }
}

struct Before {
    probe: Option<String>,
    frame: Vec<u8>,
}

struct Snapshot {
    pid: i64,
    window_id: Option<i64>,
    tokens: HashMap<i64, String>,
}

#[derive(Deserialize)]
struct StateArgs {
    #[serde(default)]
    pid: Option<i64>,
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
}

#[derive(Deserialize)]
struct ActList {
    actions: Vec<ActArgs>,
    #[serde(default)]
    max_edge: Option<u32>,
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

/// Splits text into maximal ASCII runs and single non-ASCII characters; the driver
/// remaps one spare keycode per non-ASCII character, so several in one call garble.
fn text_runs(text: &str) -> Vec<&str> {
    let mut runs = Vec::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if !c.is_ascii() {
            if start < i {
                runs.push(&text[start..i]);
            }
            runs.push(&text[i..i + c.len_utf8()]);
            start = i + c.len_utf8();
        }
    }
    if start < text.len() {
        runs.push(&text[start..]);
    }
    runs
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
            Some(Window {
                pid: w["pid"].as_i64()?,
                window_id: w["window_id"].as_i64()?,
                app: w["app_name"].as_str().unwrap_or_default().to_owned(),
                title: w["title"].as_str().unwrap_or_default().to_owned(),
                x: b["x"].as_i64().unwrap_or(0),
                y: b["y"].as_i64().unwrap_or(0),
                w: b["width"].as_i64().or_else(|| b["w"].as_i64()).unwrap_or(0),
                h: b["height"]
                    .as_i64()
                    .or_else(|| b["h"].as_i64())
                    .unwrap_or(0),
                z: w["z_index"].as_i64().unwrap_or(i64::MIN),
            })
        })
        .filter(|w| w.w >= MIN_WINDOW && w.h >= MIN_WINDOW && w.x + w.w > 0 && w.y + w.h > 0)
        .collect();
    out.sort_by_key(|w| std::cmp::Reverse(w.z));
    out
}

fn window_id_of(state: &Value) -> Option<i64> {
    state["structuredContent"]["window_id"].as_i64()
}

fn element_tree(result: &Value, scale: f64) -> (String, HashMap<i64, String>) {
    let mut tokens = HashMap::new();
    let mut tree = String::new();
    let elements = result["structuredContent"]["elements"].as_array();
    for e in elements.into_iter().flatten() {
        let Some(index) = e["element_index"].as_i64() else {
            continue;
        };
        if let Some(t) = e["element_token"].as_str() {
            tokens.insert(index, t.to_owned());
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
        let f = &e["frame"];
        if let (Some(x), Some(y), Some(w), Some(h)) = (
            f["x"].as_f64(),
            f["y"].as_f64(),
            f["w"].as_f64(),
            f["h"].as_f64(),
        ) {
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
        tree = content_text(result);
    }
    (tree, tokens)
}

fn content_text(result: &Value) -> String {
    let mut text = String::new();
    for c in result["content"].as_array().into_iter().flatten() {
        if c["type"] == "text"
            && let Some(t) = c["text"].as_str()
        {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(t);
        }
    }
    text
}

fn content_image(result: &Value) -> Option<String> {
    result["content"]
        .as_array()?
        .iter()
        .find(|c| c["type"] == "image")
        .and_then(|c| c["data"].as_str())
        .map(str::to_owned)
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
    timing::mark("decode");
    let out = encode(info.width, info.height, &rgb)?;
    timing::mark("encode");
    (out.len() < data.len()).then(|| B64.encode(&out))
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
    use super::*;

    #[tokio::test]
    async fn a_request_waits_for_a_driver_that_is_restarting() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("cua.sock");
        let path = socket.clone();
        let driver = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
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
        let cua = Cua::new(socket, false);
        let out = cua.request("s", "list_windows", json!({})).await;
        assert!(out.is_ok(), "{out:?}");
        driver.await.unwrap();
    }

    async fn closing_then_answering_driver(socket: PathBuf) -> Vec<String> {
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
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
        let socket = dir.path().join("cua.sock");
        let driver = tokio::spawn(closing_then_answering_driver(socket.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let out = Cua::new(socket, false)
            .request("s", "list_windows", json!({}))
            .await;
        assert!(out.is_ok(), "{out:?}");
        assert_eq!(driver.await.unwrap(), ["list_windows", "list_windows"]);
    }

    #[tokio::test]
    async fn an_input_action_is_never_retried_after_an_empty_reply() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("cua.sock");
        let driver = tokio::spawn(closing_then_answering_driver(socket.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let out = Cua::new(socket, false)
            .request("s", "click", json!({}))
            .await;
        assert!(out.unwrap_err().starts_with("decode cua-driver reply"));
        driver.abort();
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
    fn frames_differ_only_past_the_tolerance() {
        let a = [200u8, 200, 200, 255];
        assert!(!differs(&a, &[200 - STILL_TOLERANCE, 200, 200, 255]));
        assert!(differs(&a, &[199 - STILL_TOLERANCE, 200, 200, 255]));
        assert!(differs(&a, &a[..3]));
    }

    #[test]
    fn text_runs_isolate_every_non_ascii_character() {
        assert_eq!(
            text_runs("MCP — 你好 café 🙂!"),
            ["MCP ", "—", " ", "你", "好", " caf", "é", " ", "🙂", "!"]
        );
        assert_eq!(text_runs("plain"), ["plain"]);
        assert_eq!(text_runs("é"), ["é"]);
        assert!(text_runs("").is_empty());
    }
}
