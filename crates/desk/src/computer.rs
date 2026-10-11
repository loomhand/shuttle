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
use super::volatile::{Block, Shown, Volatile};
use super::xshm::{self, Grabber, Placement};

const STATE_ELEMENTS: u64 = 300;
const TREE_CHARS: usize = 15_000;
const MIN_WINDOW: i64 = 10;
pub(super) const SHELL_APPS: [&str; 2] = ["Xfce4-panel", "Xfdesktop"];
const CHROME_CLASS: &str = "Google-chrome";
const COMMIT_KEYS: [&str; 6] = ["return", "enter", "kp_enter", "f5", "alt+left", "alt+right"];
const SETTLE: Duration = Duration::from_millis(600);
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
const VISIBLE_STEP: u8 = 24;
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
const MENU_POLL: Duration = Duration::from_millis(30);
const MENU_POLLS: u32 = 10;
const JPEG_QUALITY: u8 = 80;
const PHOTO_COLORS: usize = 2500;
const COLOR_STRIDE: usize = 7;
const MAX_WAIT_SECS: f64 = 10.0;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_RETRY: Duration = Duration::from_millis(250);
const CONNECT_WAIT: Duration = Duration::from_secs(10);
const DEFAULT_SESSION: &str = "cocoonbox";
const SKIPPED: &str = "had no keycode";
const RUN_CHARS: usize = 8;
const WM_WAIT: Duration = Duration::from_secs(10);
const WM_POLL: Duration = Duration::from_millis(100);
const NO_TEXT_TARGET: &str =
    "no application window is on the screen to type into; open or focus one first";
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
    /// Titles of the windows an action opened, to name them when they close.
    titles: std::sync::Mutex<HashMap<u32, String>>,
    /// Per session, screen pixels per image pixel of the last screenshot returned on cbplan.
    scales: std::sync::Mutex<HashMap<String, f64>>,
    /// On cbplan, the screen blocks that change while no action runs, which the settle ignores.
    volatile: Option<Volatile>,
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
            titles: std::sync::Mutex::new(HashMap::new()),
            scales: std::sync::Mutex::new(HashMap::new()),
            volatile: cbplan.then(Volatile::start),
        }
    }

    /// `computer_state`: window list, accessibility elements and a desktop screenshot.
    pub async fn state(&self, args: Value) -> Result<Output, String> {
        self.touch();
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
        let (image_b64, scale, mut screen) = self.capture(&session, a.max_edge).await?;
        if let Some((pid, window_id, state)) = elements {
            if screen.is_none() && !self.cbplan {
                screen = self.shm(Grabber::rgb).map(|(width, height, rgb)| Screen {
                    width,
                    height,
                    rgb,
                });
                timing::mark("grab");
            }
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
        Ok(Output {
            text,
            is_error: false,
            image_b64,
        })
    }

    /// `computer_act`: one input action followed by a fresh desktop screenshot; on cbplan also a list of `actions`.
    pub async fn act(&self, args: Value) -> Result<Output, String> {
        self.touch();
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

    async fn act_one(&self, session: &str, mut a: ActArgs) -> Result<Output, String> {
        if self.cbplan && a.action == "left_click" {
            "click".clone_into(&mut a.action);
        }
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
        if a.action == "type"
            && (a.text.as_deref().is_some_and(|t| !t.is_ascii())
                || self.web_pixel(session, &a).await)
        {
            return self.type_mixed(session, &a).await;
        }
        if a.action == "launch_app" {
            return self.launch(session, &a).await;
        }
        let (tool, mut req, pixel) = self.action_request(session, &a).await?;
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
        let input = self.input.lock().await;
        timing::mark("resolve");
        let before = self.before_input(Some(&a)).await;
        let once = self.once(&req, pixel);
        let target = once.as_deref().unwrap_or(session);
        let typed = enter.then(|| req.clone());
        let mut result = self.request(target, tool, req).await;
        if let Some(typed) = typed
            && result.is_ok()
        {
            result = self.press_return(target, &typed).await.and(result);
        }
        self.end_once(once).await;
        timing::mark("act");
        let changes = self.settle(before).await;
        let shot = self.screenshot(session, a.max_edge).await?;
        drop(input);
        let (text, is_error) = match result {
            Ok(v) => (content_text(&v) + &changes, false),
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
        self.touch();
        Ok(self.capture(session, max_edge).await?.0)
    }

    /// Screenshot in a one-shot driver session that is ended right away.
    pub async fn screenshot_once(&self, max_edge: Option<u32>) -> Result<Option<String>, String> {
        let session = format!("shot-{}", uuid::Uuid::new_v4().simple());
        let shot = self.screenshot(&session, max_edge).await;
        self.scales().remove(&session);
        self.request(&session, "end_session", json!({})).await?;
        timing::mark("end");
        shot
    }

    /// Ends a driver session, releasing its input devices.
    pub async fn end(&self, args: Value) -> Result<String, String> {
        let session = session_of(&args);
        self.request(&session, "end_session", json!({})).await?;
        self.last.lock().await.remove(&session);
        self.scales().remove(&session);
        Ok(format!("ended session {session}"))
    }

    /// Captures the desktop; the driver maps desktop-scope pixels of this capture back to the screen.
    /// A capture over MIT-SHM is shrunk to `max_edge` here and also returns the full-size pixels.
    async fn capture(
        &self,
        session: &str,
        max_edge: Option<u32>,
    ) -> Result<(Option<String>, f64, Option<Screen>), String> {
        if self.cbplan
            && let Some((w, h, rgb)) = self
                .shm(|g| (!g.overlay_shown()?).then(|| g.rgb()).transpose())
                .flatten()
        {
            timing::mark("grab");
            let image = match max_edge {
                Some(edge) if edge > 0 && edge < w.max(h) => {
                    let (tw, th, small) = fit(w, h, &rgb, edge);
                    encode(tw, th, &small).map(|i| (i, f64::from(w) / f64::from(tw)))
                }
                _ => encode(w, h, &rgb).map(|i| (i, 1.0)),
            };
            if let Some((image, scale)) = image {
                timing::mark("encode");
                timing::note("image_bytes", image.len());
                self.scales().insert(session.to_owned(), scale);
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
        timing::mark("capture");
        let scale = v["structuredContent"]["frame_scale"]
            .as_f64()
            .filter(|s| *s > 0.0)
            .unwrap_or(1.0);
        let mut image = content_image(&v);
        if self.cbplan {
            if let Some(smaller) = image.as_deref().and_then(shrink) {
                image = Some(smaller);
            }
            self.scales().insert(session.to_owned(), scale);
            // the driver rescales later desktop clicks by its last capture; a full-size one keeps it at 1
            if scale != 1.0 {
                let _ = self.request(session, "get_desktop_state", json!({})).await;
                timing::mark("rescale");
            }
        }
        timing::note("image_bytes", image.as_ref().map_or(0, |b| b.len() / 4 * 3));
        Ok((image, scale, None))
    }

    /// Types text with non-ASCII characters; on cbdesk and on cbplan's pid route in calls of at most [`RUN_CHARS`]
    /// distinct non-ASCII characters.
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
                ..ActArgs::default()
            };
            let (tool, req, pixel) = self.action_request(session, &focus).await?;
            let once = self.once(&req, pixel);
            failure = self
                .request(once.as_deref().unwrap_or(session), tool, req)
                .await
                .err();
            self.end_once(once).await;
        }
        let mut base = json!({});
        if let Some((pid, window_id)) = self.key_window(session).await? {
            if !a.foreground && self.keyboard(pid, window_id) {
                timing::note("route", "desktop");
                base["target"] = json!({"kind": "desktop", "display_id": "primary"});
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
        timing::mark("resolve");
        let runs = if self.cbplan && (a.foreground || base.get("target").is_some()) {
            vec![text]
        } else {
            text_runs(text)
        };
        let mut typed = 0;
        let once = self.once(&base, false);
        for run in &runs {
            if failure.is_some() {
                break;
            }
            let mut req = base.clone();
            req["text"] = (*run).into();
            match self
                .call(once.as_deref().unwrap_or(session), "type_text", req)
                .await
            {
                Ok(v) if v["isError"] == true || content_text(&v).contains(SKIPPED) => {
                    typed += landed(run, &v);
                    failure = Some(format!("type_text: {}", content_text(&v)));
                }
                Ok(_) => typed += run.chars().count(),
                Err(e) => failure = Some(e),
            }
        }
        if failure.is_none() && text.len() < full.len() {
            match self
                .press_return(once.as_deref().unwrap_or(session), &base)
                .await
            {
                Ok(_) => typed += 1,
                Err(e) => failure = Some(e),
            }
        }
        self.end_once(once).await;
        timing::mark("act");
        let changes = self.settle(before).await;
        let shot = self.screenshot(session, a.max_edge).await?;
        drop(input);
        let total = full.chars().count();
        let summary = if self.cbplan {
            format!("typed {typed} of {total} characters; verify on the screenshot{changes}")
        } else {
            format!(
                "typed {typed} of {total} characters in {} calls (at most {RUN_CHARS} distinct non-ASCII characters per call); verify on the screenshot{changes}",
                runs.len()
            )
        };
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
        let before = self.before_input(None).await;
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
                        self.end_volatile();
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
        let changes = self.settle(before).await;
        let shot = self.screenshot(session, a.max_edge).await?;
        drop(input);
        Ok(Output {
            text: text + &changes,
            is_error: false,
            image_b64: shot,
        })
    }

    /// On cbplan, the screen before an input action: its pixels and block hashes over MIT-SHM with what
    /// each block did before it, else a thumbnail from a probe driver session of its own, which leaves
    /// the capture context of the action's session as it was. A commit action `a` (see [`commits`])
    /// in Chrome may load a page.
    async fn before_input(&self, a: Option<&ActArgs>) -> Option<Before> {
        if !self.cbplan {
            return None;
        }
        let commit = a.is_some_and(commits);
        if let Some((pixels, blocks, windows, active, page, confirm)) = self.shm(|g| {
            let active = g.active();
            let page = commit && active.is_some_and(|w| g.class(w) == CHROME_CLASS);
            let confirm = a.is_some_and(|a| {
                confirms(a) || (clicks_left(a) && active.is_some_and(|w| g.dialog(w)))
            });
            let (width, height) = g.size();
            let pixels = g.pixels()?.to_vec();
            let blocks = xshm::block_hashes(&pixels, width, height);
            Ok((pixels, blocks, g.mapped()?, active, page, confirm))
        }) {
            let volatile = self
                .volatile
                .as_ref()
                .map(|v| v.begin(&blocks))
                .unwrap_or_default();
            timing::mark("probe");
            timing::note(
                "volatile",
                volatile.iter().filter(|b| **b != Block::Quiet).count(),
            );
            return Some(Before {
                probe: None,
                frame: Frame::Blocks(blocks),
                pixels,
                volatile,
                windows,
                active,
                page,
                confirm,
            });
        }
        let probe = format!("still-{}", uuid::Uuid::new_v4().simple());
        let before = match self.frame(Some(&probe)).await {
            Some(frame) => Some(Before {
                probe: Some(probe),
                frame: Frame::Thumb(frame),
                pixels: Vec::new(),
                volatile: Vec::new(),
                windows: Vec::new(),
                active: None,
                page: false,
                confirm: false,
            }),
            None => {
                let _ = self.request(&probe, "end_session", json!({})).await;
                None
            }
        };
        timing::mark("probe");
        before
    }

    /// Waits after an input action until the screen differs from `before` and held still, at most
    /// [`SETTLE`] for a change, [`COMMIT_FIRST`] after a confirm, and [`SETTLE`] after it, so an
    /// effect that shows late, like a Save As dialog closing, is still in the frame; a flash that
    /// reverts, like a pressed button, is no change. Without `before`, the fixed [`SETTLE`]. Over
    /// MIT-SHM the screen is split into 16-pixel blocks whose new contents count (see [`Block`]): a
    /// block that kept changing before the action, like a clock, a spinner or a video, never
    /// counts; a caret counts only with contents it did not blink between; and a block back at its
    /// contents from before toggles from then on (see [`Shown`]). The screen differs once a
    /// counting block has a pixel more than [`VISIBLE_STEP`] off its value from before, so a faint
    /// hover fade is not the effect; any change of a counting block restarts the hold. The hold is
    /// [`STILL_FOR`], or [`WINDOW_STILL_FOR`] once a window maps, unmaps or moves or the focus
    /// moves; after a commit in Chrome it is [`COMMIT_STILL_FOR`], at most [`COMMIT_SETTLE`] once
    /// the screen changed, so a page that loads over the network or adds content from timers is in
    /// the frame. Returns lines naming the windows the action opened, closed or brought to the
    /// front.
    async fn settle(&self, before: Option<Before>) -> String {
        let Some(Before {
            probe,
            frame: first,
            pixels,
            mut volatile,
            windows,
            active,
            page,
            confirm,
        }) = before
        else {
            tokio::time::sleep(SETTLE).await;
            timing::mark("settle");
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
        let mut deadline = start + if confirm { COMMIT_FIRST } else { SETTLE };
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
            let polled = match probe.as_deref() {
                Some(probe) => self.frame(Some(probe)).await.map(|f| {
                    let f = Frame::Thumb(f);
                    let differs = first.changes(&f, &volatile);
                    (f, differs, seen.clone(), focus)
                }),
                None => self.shm(|g| {
                    let (width, height) = g.size();
                    let current = g.pixels()?;
                    let blocks = xshm::block_hashes(current, width, height);
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
        timing::mark("settle");
        timing::note("still", still);
        timing::note("hold_ms", hold.as_millis());
        timing::note("page", page);
        if let Some(probe) = probe {
            let _ = self.request(&probe, "end_session", json!({})).await;
            return String::new();
        }
        if seen == windows && focus == active {
            return String::new();
        }
        let report = self
            .shm(|g| Ok(self.window_report(g, &windows, &seen, active, focus)))
            .unwrap_or_default();
        timing::mark("report");
        report
    }

    fn touch(&self) {
        if let Some(v) = &self.volatile {
            v.touch();
        }
    }

    fn end_volatile(&self) {
        if let Some(v) = &self.volatile {
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
    ) -> String {
        let mut titles = self
            .titles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                        "\nopened window \"{title}\" {}x{} at ({},{})",
                        p.width, p.height, p.x, p.y
                    );
                    opened.push(title.clone());
                    titles.insert(p.id, title);
                }
                Some(b) if b != p => {
                    let title = titles.get(&p.id).cloned().unwrap_or_else(|| g.title(p.id));
                    let _ = write!(
                        out,
                        "\nwindow \"{title}\" is now {}x{} at ({},{})",
                        p.width, p.height, p.x, p.y
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

    fn scales(&self) -> std::sync::MutexGuard<'_, HashMap<String, f64>> {
        self.scales
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Screen pixels per pixel of the last screenshot `session` returned on cbplan, so its x,y map to the screen.
    fn scale_of(&self, session: &str) -> f64 {
        self.scales().get(session).copied().unwrap_or(1.0)
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
        timing::note("wm_wait_ms", start.elapsed().as_millis());
        self.app_window(session).await
    }

    async fn action_request(
        &self,
        session: &str,
        a: &ActArgs,
    ) -> Result<(&'static str, Value, bool), String> {
        let desktop = json!({"kind": "desktop", "display_id": "primary"});
        let clicks = matches!(a.action.as_str(), "click" | "double_click" | "right_click")
            || (self.cbplan && a.action == "triple_click");
        let pixel = match a.element {
            Some(index) if clicks && !a.foreground && (self.cbplan || presses(a)) => {
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
            "triple_click" if self.cbplan => {
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
                let k = self.scale_of(session);
                let (x, y, tx, ty) = (x * k, y * k, tx * k, ty * k);
                req = json!({"from_x": x, "from_y": y, "to_x": tx, "to_y": ty, "target": desktop});
                return Ok(("drag", req, false));
            }
            other => return Err(format!("unknown action {other}")),
        };
        if let Some((x, y)) = pixel {
            timing::note("route", "pixel");
            req["x"] = x.into();
            req["y"] = y.into();
            req["target"] = desktop;
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
            let k = self.scale_of(session);
            req["x"] = (x * k).into();
            req["y"] = (y * k).into();
            req["target"] = desktop;
        } else if clicks {
            return Err(format!("{} needs element or x and y", a.action));
        } else if let Some((pid, window_id)) = self.key_window(session).await? {
            if tool != "scroll" && !a.foreground && self.keyboard(pid, window_id) {
                timing::note("route", "desktop");
                req["target"] = desktop;
            } else {
                req["pid"] = pid.into();
                req["window_id"] = window_id.into();
            }
        } else if tool == "type_text" {
            return Err(NO_TEXT_TARGET.to_owned());
        } else if tool != "scroll" && !a.foreground {
            timing::note("route", "desktop");
            req["target"] = desktop;
        }
        if a.foreground {
            req["delivery_mode"] = "foreground".into();
        }
        Ok((tool, req, pixel.is_some()))
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

    /// A driver session of its own for the pid-addressed input `req`, ended by [`Self::end_once`] right
    /// after: the virtual master keyboard the driver makes for it would outlive the call, and while one
    /// exists Chrome drops core keyboard input, from XTest and VNC alike. A `pixel` click at an element
    /// gets one too: its x,y are screen pixels, which the driver maps by the capture of a session that
    /// took one, and a session's second pointer action waits 1.1 s for its agent cursor to glide.
    fn once(&self, req: &Value, pixel: bool) -> Option<String> {
        (pixel || req.get("pid").is_some())
            .then(|| format!("input-{}", uuid::Uuid::new_v4().simple()))
    }

    async fn end_once(&self, once: Option<String>) {
        if let Some(session) = once {
            let _ = self.request(&session, "end_session", json!({})).await;
            timing::mark("end");
        }
    }

    /// On cbplan, whether keys for `window` of `pid` can go to the desktop, which skips the driver's
    /// 220 ms focus guard: the window is active and holds the input focus, and no popup of another app is open.
    fn keyboard(&self, pid: i64, window: i64) -> bool {
        let (Ok(pid), Ok(window)) = (u32::try_from(pid), u32::try_from(window)) else {
            return false;
        };
        self.cbplan && self.shm(|g| g.keyboard(window, pid)).unwrap_or(false)
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

    /// The screen point at which a pixel click reaches element `index` of `session`'s snapshot, on
    /// cbdesk only for a push button or menu item outside a web page, whose accessibility action runs
    /// its handler inside the app's accessibility dispatch, where a modal dialog it opens deadlocks the
    /// app on the first key; on cbplan it also skips the driver's 270 ms accessibility click. The
    /// element looks as it did in the snapshot, and the top window at its centre is the snapshot's
    /// window while that is active and nothing grabs the pointer, or a popup of the same app around a
    /// menu or list row. A `menu` to open need not look as it did, since it showed open then.
    async fn element_pixel(&self, session: &str, index: i64, menu: bool) -> Option<(f64, f64)> {
        let last = self.last.lock().await;
        let snap = last.get(session)?;
        let (element, screen) = (snap.elements.get(&index)?, snap.screen.as_ref()?);
        if !self.cbplan && !menu && (element.web || !PRESSED.contains(&element.role.as_str())) {
            return None;
        }
        let [x, y, w, h] = element.frame?;
        let (cx, cy) = ((x + w / 2.0).floor(), (y + h / 2.0).floor());
        let rect = clip([x, y, w, h], screen)?;
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

    /// For menu item `index` of `session`'s snapshot while no popup of its app is open, opens the
    /// menus that hold it, outermost first, with pixel clicks, and returns the point at which a pixel
    /// click then reaches the item.
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
        timing::mark("menus");
        self.poll_pixel(session, index, false).await
    }

    /// For a push button or menu item of `session`'s snapshot that is not on the screen, in a GTK app
    /// and outside a web page, whose accessibility action would deadlock the app if it opened a modal
    /// dialog: the point of the menu bar item of the same name, opened in its menu, else an error;
    /// `None` for any other element.
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
            let shown = element.frame.and_then(|f| clip(f, screen)).is_some();
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
        let mut req =
            json!({"pid": pid, "include_screenshot": false, "max_elements": STATE_ELEMENTS});
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
            let probe = format!("menu-{}", uuid::Uuid::new_v4().simple());
            let state = self.request(&probe, "get_window_state", req.clone()).await;
            let _ = self.request(&probe, "end_session", json!({})).await;
            timing::mark("twin");
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

    /// The desktop-addressed input `req` of `tool` in a driver session of its own (see [`Self::once`]).
    async fn desktop_input(&self, tool: &str, mut req: Value) -> Result<Value, String> {
        req["target"] = json!({"kind": "desktop", "display_id": "primary"});
        let once = format!("input-{}", uuid::Uuid::new_v4().simple());
        let done = self.request(&once, tool, req).await;
        self.end_once(Some(once)).await;
        done
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
        let result = self.call(session, name, args).await?;
        if result["isError"] == true {
            return Err(format!("{name}: {}", content_text(&result)));
        }
        Ok(result)
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
    confirm: bool,
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
    /// On cbplan, the screen when the elements were read, to tell whether one has moved since.
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

/// Splits text into runs with at most [`RUN_CHARS`] distinct non-ASCII characters; the driver's
/// virtual-keyboard route binds a spare keycode to each of them for the call and has fewer than 20 to lend.
fn text_runs(text: &str) -> Vec<&str> {
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

fn before_newline(text: &str) -> Option<&str> {
    text.strip_suffix(['\n', '\r']).filter(|t| !t.is_empty())
}

/// How many characters of `run` a failed type_text call typed: all but the skipped ones on the
/// virtual-keyboard route, else the count its "typed N of" error names.
fn landed(run: &str, result: &Value) -> usize {
    if let Some(skipped) = result["structuredContent"]["skipped_characters"].as_str() {
        return run.chars().count().saturating_sub(skipped.chars().count());
    }
    content_text(result)
        .split("typed ")
        .nth(1)
        .and_then(|t| t.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn confirms(a: &ActArgs) -> bool {
    let keys = match a.action.as_str() {
        "type" => return a.text.as_deref().is_some_and(|t| t.ends_with(['\n', '\r'])),
        "key" => a.key.clone().into_iter().collect(),
        "hotkey" => a
            .keys
            .clone()
            .map(|k| vec![k.join("+")])
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    keys.iter()
        .any(|k| ["return", "enter", "kp_enter"].contains(&k.to_lowercase().as_str()))
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
    let keys = match a.action.as_str() {
        "type" => return a.text.as_deref().is_some_and(|t| t.ends_with(['\n', '\r'])),
        "click" | "double_click" | "triple_click" => {
            return a.button.as_deref().is_none_or(|b| b == "left");
        }
        "key" => a.key.clone().into_iter().collect(),
        "hotkey" => a
            .keys
            .clone()
            .map(|k| vec![k.join("+")])
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    keys.iter()
        .any(|k| COMMIT_KEYS.contains(&k.to_lowercase().as_str()))
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
        tree = content_text(result);
    }
    (tree, by_index)
}

/// The screen rectangle `[x, y, w, h]` clipped to `screen`, in whole pixels; `None` when nothing is left.
fn clip([x, y, w, h]: [f64; 4], screen: &Screen) -> Option<[i32; 4]> {
    let (width, height) = (f64::from(screen.width), f64::from(screen.height));
    let (left, top) = (x.max(0.0).floor(), y.max(0.0).floor());
    let (right, bottom) = ((x + w).min(width).ceil(), (y + h).min(height).ceil());
    (right > left && bottom > top)
        .then(|| [left, top, right - left, bottom - top].map(|v| v as i32))
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

    #[tokio::test]
    async fn a_key_with_no_window_on_the_screen_goes_to_the_desktop_and_typing_fails() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("cua.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
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
        let cua = Cua::new(socket, false);
        let out = cua
            .act(json!({"action": "key", "key": "escape"}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.text);
        let pressed = calls.lock().unwrap()[1].clone();
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
    fn an_element_has_moved_once_enough_of_its_pixels_changed() {
        let screen = Screen {
            width: 4,
            height: 2,
            rgb: vec![100; 4 * 2 * 3],
        };
        assert_eq!(clip([-1.5, 0.5, 3.0, 9.0], &screen), Some([0, 0, 2, 2]));
        assert_eq!(clip([4.0, 0.0, 2.0, 2.0], &screen), None);
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

    #[test]
    fn text_runs_hold_at_most_eight_distinct_non_ascii_characters() {
        assert_eq!(
            text_runs("MCP — 你好 café 🙂! 一二三四五六七八九十"),
            ["MCP — 你好 café 🙂! 一二三", "四五六七八九十"]
        );
        assert_eq!(
            text_runs("你你你你你你你你你你好"),
            ["你你你你你你你你你你好"]
        );
        assert_eq!(text_runs("plain"), ["plain"]);
        assert_eq!(text_runs("é"), ["é"]);
        assert!(text_runs("").is_empty());
    }
}
