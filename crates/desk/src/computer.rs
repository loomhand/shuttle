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
use super::xshm::{Grabber, Placement};

const STATE_ELEMENTS: u64 = 300;
const TREE_CHARS: usize = 15_000;
const VIEW_MIN: usize = 3;
const VIEW_LABEL: usize = 60;
const VIEW_VALUE: usize = 80;
const VIEW_DISABLED: usize = 16;
const STRUCTURE: [&str; 16] = [
    "panel",
    "section",
    "filler",
    "frame",
    "document web",
    "form",
    "landmark",
    "tool bar",
    "page tab list",
    "split pane",
    "internal frame",
    "scroll bar",
    "scroll pane",
    "viewport",
    "file chooser",
    "menu bar",
];
const TOGGLES: [&str; 3] = ["check", "radio", "toggle"];
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
const STILL_EDGE: u32 = 160;
const STILL_TOLERANCE: u8 = 48;
const FINE_TOLERANCE: u8 = 8;
const SMALL_BLOCKS: usize = 8;
const STRIP_ROWS: usize = 15;
const MOVED_PERCENT: usize = 5;
const POPUP_ROWS: [&str; 6] = [
    "menu item",
    "menu",
    "list item",
    "tree item",
    "table cell",
    "option",
];
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
    /// Titles of the windows an action opened, to name them when they close.
    titles: std::sync::Mutex<HashMap<u32, String>>,
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
        let view = self.view(a.view.as_deref());
        if view != View::Screenshot {
            let pin = a.pid.map(|pid| (pid, a.window_id));
            let (image_b64, seen) = self
                .observe(&session, view, a.max_edge, false, pin, a.query)
                .await?;
            text.push_str(&seen);
            return Ok(Output {
                text,
                is_error: false,
                image_b64,
            });
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
        let (image_b64, scale, screen) = self.capture(&session, a.max_edge).await?;
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
        let look = Look {
            view: self.view(a.view.as_deref()),
            max_edge: a.max_edge,
        };
        self.act_one(&session, a, Some(look)).await
    }

    /// Runs the actions in order, each settled, stops at the first failure and observes once at the end.
    async fn act_list(&self, session: &str, list: ActList) -> Result<Output, String> {
        let total = list.actions.len();
        let mut lines = Vec::with_capacity(total);
        let mut failed = false;
        for (i, a) in list.actions.into_iter().enumerate() {
            let action = a.action.clone();
            let step = self
                .act_one(session, a, None)
                .await
                .unwrap_or_else(Output::error);
            let mark = if step.is_error { " failed" } else { "" };
            lines.push(format!("{}. {action}{mark}: {}", i + 1, step.text));
            if step.is_error {
                failed = true;
                if i + 1 < total {
                    lines.push(format!(
                        "the remaining {} actions did not run",
                        total - i - 1
                    ));
                }
                break;
            }
        }
        let view = self.view(list.view.as_deref());
        let (image_b64, seen) = self
            .observe(session, view, list.max_edge, failed, None, None)
            .await?;
        Ok(Output {
            text: lines.join("\n") + &seen,
            is_error: failed,
            image_b64,
        })
    }

    /// Runs one action; with `look`, also the observation after it, else no screenshot or view.
    async fn act_one(
        &self,
        session: &str,
        a: ActArgs,
        look: Option<Look>,
    ) -> Result<Output, String> {
        timing::note("action", &a.action);
        if a.action == "wait" {
            let secs = a.seconds.unwrap_or(1.0).clamp(0.1, MAX_WAIT_SECS);
            tokio::time::sleep(Duration::from_secs_f64(secs)).await;
            timing::mark("wait");
            let (image_b64, seen) = self.look(session, look, false).await?;
            return Ok(Output {
                text: format!("waited {secs:.1}s{seen}"),
                is_error: false,
                image_b64,
            });
        }
        if a.action == "type"
            && (a.text.as_deref().is_some_and(|t| !t.is_ascii())
                || self.web_pixel(session, &a).await)
        {
            return self.type_mixed(session, &a, look).await;
        }
        if a.action == "launch_app" {
            return self.launch(session, &a, look).await;
        }
        let (tool, req) = self.action_request(session, &a).await?;
        let input = self.input.lock().await;
        timing::mark("resolve");
        let before = self.before_input(Some(&a)).await;
        let once = self.once(&req);
        let result = self
            .request(once.as_deref().unwrap_or(session), tool, req)
            .await;
        self.end_once(once).await;
        timing::mark("act");
        let changes = self.settle(before).await;
        let (shot, seen) = self.look(session, look, result.is_err()).await?;
        drop(input);
        let (text, is_error) = match result {
            Ok(v) => (content_text(&v) + &changes + &seen, false),
            Err(e) => (e + &seen, true),
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

    /// Captures the desktop and returns the screen pixels per image pixel that element centres divide by:
    /// the driver's frame scale on cbdesk, where it maps desktop-scope pixels of this capture back to the
    /// screen; 1 on cbplan, where x,y and centres are always screen pixels. A capture over MIT-SHM is
    /// shrunk to `max_edge` here and also returns the full-size pixels.
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
                    encode(tw, th, &small)
                }
                _ => encode(w, h, &rgb),
            };
            if let Some(image) = image {
                timing::mark("encode");
                timing::note("image_bytes", image.len());
                let screen = Screen {
                    width: w,
                    height: h,
                    rgb,
                };
                return Ok((Some(B64.encode(image)), 1.0, Some(screen)));
            }
        }
        let mut args = json!({});
        if let Some(edge) = max_edge {
            args["max_image_dimension"] = edge.into();
        }
        let v = self.request(session, "get_desktop_state", args).await?;
        timing::mark("capture");
        let mut scale = v["structuredContent"]["frame_scale"]
            .as_f64()
            .filter(|s| *s > 0.0)
            .unwrap_or(1.0);
        let mut image = content_image(&v);
        if self.cbplan {
            if let Some(smaller) = image.as_deref().and_then(shrink) {
                image = Some(smaller);
            }
            // the driver rescales later desktop clicks by its last capture; a full-size one keeps it at 1
            if scale != 1.0 {
                let _ = self.request(session, "get_desktop_state", json!({})).await;
                timing::mark("rescale");
            }
            scale = 1.0;
        }
        timing::note("image_bytes", image.as_ref().map_or(0, |b| b.len() / 4 * 3));
        Ok((image, scale, None))
    }

    /// Types text with non-ASCII characters: ASCII runs in one call, every other character in its own call.
    async fn type_mixed(
        &self,
        session: &str,
        a: &ActArgs,
        look: Option<Look>,
    ) -> Result<Output, String> {
        let text = a.text.as_deref().unwrap_or_default();
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
            let (tool, req) = self.action_request(session, &focus).await?;
            let once = self.once(&req);
            failure = self
                .request(once.as_deref().unwrap_or(session), tool, req)
                .await
                .err();
            self.end_once(once).await;
        }
        let mut base = json!({});
        if let Some((pid, window_id)) = self.app_window(session).await? {
            if !a.foreground && self.keyboard(pid, window_id) {
                timing::note("route", "desktop");
                base["target"] = json!({"kind": "desktop", "display_id": "primary"});
            } else {
                base["pid"] = pid.into();
                base["window_id"] = window_id.into();
            }
        }
        if a.foreground {
            base["delivery_mode"] = "foreground".into();
        }
        timing::mark("resolve");
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
                .request(once.as_deref().unwrap_or(session), "type_text", req)
                .await
            {
                Ok(_) => typed += run.chars().count(),
                Err(e) => failure = Some(e),
            }
        }
        self.end_once(once).await;
        timing::mark("act");
        let changes = self.settle(before).await;
        let (shot, seen) = self.look(session, look, failure.is_some()).await?;
        drop(input);
        let total = text.chars().count();
        let check = match look.map(|l| l.view) {
            Some(View::Elements) => "the elements",
            _ => "the screenshot",
        };
        let summary = format!(
            "typed {typed} of {total} characters in {} calls (non-ASCII characters go one per call); verify on {check}{changes}",
            runs.len()
        );
        Ok(Output {
            text: match &failure {
                Some(e) => format!("{summary}; stopped: {e}{seen}"),
                None => summary + &seen,
            },
            is_error: failure.is_some(),
            image_b64: shot,
        })
    }

    /// Launches an app, or brings its existing window to the front when it already runs.
    async fn launch(
        &self,
        session: &str,
        a: &ActArgs,
        look: Option<Look>,
    ) -> Result<Output, String> {
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
                        if let Some(probe) = before.and_then(|b| b.probe) {
                            let _ = self.request(&probe, "end_session", json!({})).await;
                        }
                        drop(input);
                        let (image_b64, seen) = self.look(session, look, true).await?;
                        return Ok(Output {
                            text: e + &seen,
                            is_error: true,
                            image_b64,
                        });
                    }
                },
            },
        };
        timing::mark("act");
        let changes = self.settle(before).await;
        let (shot, seen) = self.look(session, look, false).await?;
        drop(input);
        Ok(Output {
            text: text + &changes + &seen,
            is_error: false,
            image_b64: shot,
        })
    }

    /// On cbplan, a thumbnail of the screen before an input action: over MIT-SHM, else from a probe
    /// driver session of its own, which leaves the capture context of the action's session as it was.
    /// A commit action `a` (see [`commits`]) in Chrome may load a page; a [`fine`] one shows a small change.
    async fn before_input(&self, a: Option<&ActArgs>) -> Option<Before> {
        if !self.cbplan {
            return None;
        }
        let (commit, fine) = a.map_or((false, false), |a| (commits(a), fine(a)));
        if let Some((frame, columns, windows, active, page)) = self.shm(|g| {
            let active = g.active();
            let page = commit && active.is_some_and(|w| g.class(w) == CHROME_CLASS);
            Ok((g.thumb()?, g.thumb_columns(), g.mapped()?, active, page))
        }) {
            timing::mark("probe");
            return Some(Before {
                probe: None,
                frame,
                columns,
                windows,
                active,
                page,
                fine,
            });
        }
        let probe = format!("still-{}", uuid::Uuid::new_v4().simple());
        let before = match self.frame(Some(&probe)).await {
            Some(frame) => Some(Before {
                probe: Some(probe),
                frame,
                columns: STILL_EDGE as usize,
                windows: Vec::new(),
                active: None,
                page: false,
                fine,
            }),
            None => {
                let _ = self.request(&probe, "end_session", json!({})).await;
                None
            }
        };
        timing::mark("probe");
        before
    }

    /// Waits after an input action until the screen changed from `before` and held still, at most
    /// [`SETTLE`]; without `before`, the fixed [`SETTLE`]. The hold is [`STILL_FOR`], or
    /// [`WINDOW_STILL_FOR`] once a window maps, unmaps or moves or the focus moves; after a commit
    /// in Chrome it is [`COMMIT_STILL_FOR`], at most [`COMMIT_SETTLE`] once the screen changed, so a
    /// page that loads over the network or adds content from timers is in the frame. Frames are
    /// thumbnails compared with a tolerance; a change within [`SMALL_BLOCKS`] square, such as a caret
    /// or a spinner, does not restart the hold, except in the Chrome tab strip and toolbar after a
    /// commit, where it shows a page loading. A [`fine`] action counts a change past
    /// [`FINE_TOLERANCE`], so one typed character ends the wait. Returns lines naming the windows the
    /// action opened, closed or brought to the front.
    async fn settle(&self, before: Option<Before>) -> String {
        let Some(Before {
            probe,
            frame: first,
            columns,
            windows,
            active,
            page,
            fine,
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
        let mut deadline = start + SETTLE;
        let (mut last, mut since): (Option<Vec<u8>>, Instant) = (None, Instant::now());
        let (mut seen, mut focus, mut hold) = (windows.clone(), active, first_hold);
        let tolerance = if fine {
            FINE_TOLERANCE
        } else {
            STILL_TOLERANCE
        };
        let (mut still, mut changed) = (false, false);
        while !still && Instant::now() < deadline {
            tokio::time::sleep(poll.min(deadline.saturating_duration_since(Instant::now()))).await;
            let polled = match probe.as_deref() {
                Some(probe) => self
                    .frame(Some(probe))
                    .await
                    .map(|f| (f, seen.clone(), focus)),
                None => self.shm(|g| Ok((g.thumb()?, g.mapped()?, g.active()))),
            };
            let Some((frame, now, now_focus)) = polled else {
                tokio::time::sleep_until(deadline).await;
                break;
            };
            if now != seen || now_focus != focus {
                (seen, focus, hold) = (now, now_focus, window_hold);
                since = Instant::now();
            }
            changed |= change_box(&first, &frame, columns, tolerance).is_some();
            let moved = match last.as_deref() {
                Some(l) => change_box(l, &frame, columns, STILL_TOLERANCE),
                None => Some([0, 0, columns, usize::MAX]),
            };
            if let Some([x0, y0, x1, y1]) = moved {
                if x1 - x0 > SMALL_BLOCKS || y1 - y0 > SMALL_BLOCKS || (page && y0 < STRIP_ROWS) {
                    since = Instant::now();
                }
                last = Some(frame);
            }
            if page && changed {
                deadline = start + COMMIT_SETTLE;
            }
            still = changed && since.elapsed() >= hold;
        }
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
        Ok(self.front(session).await?.map(|w| (w.pid, w.window_id)))
    }

    async fn front(&self, session: &str) -> Result<Option<Window>, String> {
        let windows = self
            .request(session, "list_windows", json!({"on_screen_only": true}))
            .await?;
        Ok(window_list(&windows)
            .into_iter()
            .find(|w| !SHELL_APPS.contains(&w.app.as_str())))
    }

    /// The observation a `view` argument asks for on cbplan; the screenshot without one or on cbdesk.
    fn view(&self, view: Option<&str>) -> View {
        match (self.cbplan, view) {
            (true, Some("elements")) => View::Elements,
            (true, Some("both")) => View::Both,
            _ => View::Screenshot,
        }
    }

    /// The observation `look` asks for after an action, or nothing without one.
    async fn look(
        &self,
        session: &str,
        look: Option<Look>,
        failed: bool,
    ) -> Result<(Option<String>, String), String> {
        match look {
            Some(l) => {
                self.observe(session, l.view, l.max_edge, failed, None, None)
                    .await
            }
            None => Ok((None, String::new())),
        }
    }

    /// The screenshot and the text of an observation: on the elements view, the compact element list of
    /// the open popup, of `pin` or of the frontmost window, with the screenshot only for `both`, a
    /// `failed` action or a list shorter than [`VIEW_MIN`].
    async fn observe(
        &self,
        session: &str,
        view: View,
        max_edge: Option<u32>,
        failed: bool,
        pin: Option<(i64, Option<i64>)>,
        query: Option<String>,
    ) -> Result<(Option<String>, String), String> {
        if view == View::Screenshot {
            return Ok((self.screenshot(session, max_edge).await?, String::new()));
        }
        let all = query.is_some();
        let walked = self.walk(session, pin, query).await;
        timing::mark("elements");
        let size = self.shm(|g| Ok(g.size()));
        let listed = match &walked {
            Ok((.., state)) => compact(state, size, all).2,
            Err(_) => 0,
        };
        let (image, screen) = if view == View::Both || failed || listed < VIEW_MIN {
            let (image, _, screen) = self.capture(session, max_edge).await?;
            (image, screen)
        } else {
            let screen = self.shm(|g| {
                let (width, height, rgb) = g.rgb()?;
                Ok(Screen { width, height, rgb })
            });
            (None, screen)
        };
        let text = match walked {
            Ok((head, pid, window_id, state)) => {
                let (list, elements, listed) = compact(&state, size, all);
                timing::note("view_listed", listed);
                self.last.lock().await.insert(
                    session.to_owned(),
                    Snapshot {
                        pid,
                        window_id: window_id_of(&state).or(window_id),
                        elements,
                        screen,
                    },
                );
                format!(
                    "\n\nElements of {head}, [id] role \"name\" = \"value\" (center x,y) state:\n{list}"
                )
            }
            Err(e) => format!("\n\nNo element view: {e}"),
        };
        timing::note("view_chars", text.len());
        Ok((image, text))
    }

    /// The window state of the open popup, of `pin` or of the frontmost window, with a heading naming it.
    async fn walk(
        &self,
        session: &str,
        pin: Option<(i64, Option<i64>)>,
        query: Option<String>,
    ) -> Result<(String, i64, Option<i64>, Value), String> {
        let popup = match pin {
            Some(_) => None,
            None => self.shm(Grabber::popup).flatten(),
        };
        let (pid, window_id, head) = match (pin, popup) {
            (Some((pid, window_id)), _) => (pid, window_id, format!("pid {pid}")),
            (None, Some((pid, w))) => (
                i64::from(pid),
                Some(i64::from(w)),
                format!("the open menu or popup of pid {pid}"),
            ),
            (None, None) => {
                let w = self
                    .front(session)
                    .await?
                    .ok_or("no application window is open")?;
                (
                    w.pid,
                    Some(w.window_id),
                    format!("{} \"{}\" (pid {})", w.app, w.title, w.pid),
                )
            }
        };
        let mut req = json!({
            "pid": pid,
            "include_screenshot": false,
            "max_elements": STATE_ELEMENTS,
        });
        if let Some(id) = window_id {
            req["window_id"] = id.into();
        }
        if let Some(q) = query {
            req["query"] = q.into();
        }
        let state = self.request(session, "get_window_state", req).await?;
        Ok((head, pid, window_id, state))
    }

    async fn action_request(
        &self,
        session: &str,
        a: &ActArgs,
    ) -> Result<(&'static str, Value), String> {
        let desktop = json!({"kind": "desktop", "display_id": "primary"});
        let clicks = matches!(a.action.as_str(), "click" | "double_click" | "right_click");
        let pixel = match a.element {
            Some(index) if clicks && !a.foreground => self.element_pixel(session, index).await,
            _ => None,
        };
        let mut req = json!({});
        let tool = match a.action.as_str() {
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
                req = json!({"from_x": x, "from_y": y, "to_x": tx, "to_y": ty, "target": desktop});
                return Ok(("drag", req));
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
            req["x"] = x.into();
            req["y"] = y.into();
            req["target"] = desktop;
        } else if clicks {
            return Err(format!("{} needs element or x and y", a.action));
        } else if let Some((pid, window_id)) = self.app_window(session).await? {
            if tool != "scroll" && !a.foreground && self.keyboard(pid, window_id) {
                timing::note("route", "desktop");
                req["target"] = desktop;
            } else {
                req["pid"] = pid.into();
                req["window_id"] = window_id.into();
            }
        }
        if a.foreground {
            req["delivery_mode"] = "foreground".into();
        }
        Ok((tool, req))
    }

    /// On cbplan, a driver session of its own for the pid-addressed input `req`, ended by [`Self::end_once`]
    /// right after: the virtual master keyboard the driver makes for it would outlive the call, and while
    /// one exists Chrome drops core keyboard input, from XTest and VNC alike.
    fn once(&self, req: &Value) -> Option<String> {
        (self.cbplan && req.get("pid").is_some())
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
        web && self.element_pixel(session, index).await.is_some()
    }

    /// On cbplan, the screen point at which a pixel click reaches element `index` of `session`'s snapshot,
    /// which skips the driver's 270 ms accessibility click: the element looks as it did in the snapshot,
    /// and the top window at its centre is the snapshot's window while that is active and nothing grabs
    /// the pointer, or a popup of the same app around a menu or list row.
    async fn element_pixel(&self, session: &str, index: i64) -> Option<(f64, f64)> {
        let last = self.last.lock().await;
        let snap = last.get(session)?;
        let (element, screen) = (snap.elements.get(&index)?, snap.screen.as_ref()?);
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
            Ok(reachable && !moved(screen, rect, &g.region(rect)?))
        });
        reached.unwrap_or(false).then_some((cx, cy))
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
    /// The mapped top-level windows over MIT-SHM; a change holds the settle for [`WINDOW_STILL_FOR`].
    windows: Vec<Placement>,
    active: Option<u32>,
    /// A commit action in Chrome: the settle holds [`COMMIT_STILL_FOR`], at most [`COMMIT_SETTLE`].
    page: bool,
    /// Columns of a `frame` row.
    columns: usize,
    /// The action shows small changes, which count past [`FINE_TOLERANCE`].
    fine: bool,
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
    #[serde(default)]
    view: Option<String>,
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
    #[serde(default)]
    view: Option<String>,
}

#[derive(Deserialize)]
struct ActList {
    actions: Vec<ActArgs>,
    #[serde(default)]
    max_edge: Option<u32>,
    #[serde(default)]
    view: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Screenshot,
    Elements,
    Both,
}

/// The observation after an action and the long edge of its screenshot.
#[derive(Clone, Copy)]
struct Look {
    view: View,
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

/// Whether `a` may commit a page load: a left click, Return or a navigation key, or text ending in a newline.
fn commits(a: &ActArgs) -> bool {
    let keys = match a.action.as_str() {
        "type" => return a.text.as_deref().is_some_and(|t| t.ends_with(['\n', '\r'])),
        "click" | "double_click" => return a.button.as_deref().is_none_or(|b| b == "left"),
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

/// Whether `a` is typing or a key whose change may be one character: text without a newline, or a key
/// or hotkey that does not commit.
fn fine(a: &ActArgs) -> bool {
    matches!(a.action.as_str(), "type" | "key" | "hotkey") && !commits(a)
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

/// The elements of a window state, one line each without structural containers: on-screen ones with
/// their centre in screen pixels, off-screen ones too when `all`, then the disabled controls the driver
/// gives no id; returns the text, every element by index and the line count.
fn compact(
    result: &Value,
    screen: Option<(u16, u16)>,
    all: bool,
) -> (String, HashMap<i64, Element>, usize) {
    let state = &result["structuredContent"];
    let (names, disabled) = tree_extras(
        state["tree_markdown"].as_str().unwrap_or_default(),
        state["popup"] == true,
    );
    let mut by_index = HashMap::new();
    let mut list = String::new();
    let (mut listed, mut hidden) = (0, 0);
    for e in state["elements"].as_array().into_iter().flatten() {
        let Some(index) = e["element_index"].as_i64() else {
            continue;
        };
        let role = e["role"].as_str().unwrap_or("?");
        let f = &e["frame"];
        let frame = [&f["x"], &f["y"], &f["w"], &f["h"]].map(Value::as_f64);
        let frame = frame
            .iter()
            .all(Option::is_some)
            .then(|| frame.map(Option::unwrap_or_default));
        if let Some(t) = e["element_token"].as_str() {
            let element = Element {
                token: t.to_owned(),
                role: role.to_owned(),
                frame,
                web: e["in_web_content"] == true,
            };
            by_index.insert(index, element);
        }
        let mut label = clean(e["label"].as_str().unwrap_or_default(), VIEW_LABEL);
        if label.is_empty()
            && let Some(name) = names.get(&index)
        {
            label.clone_from(name);
        }
        let value = clean(e["value"].as_str().unwrap_or_default(), VIEW_VALUE);
        if STRUCTURE.contains(&role)
            || (role == "table cell" && label.is_empty() && value.is_empty())
        {
            continue;
        }
        let center = frame.map(|[x, y, w, h]| (x + w / 2.0, y + h / 2.0));
        let on_screen = center.is_some_and(|(x, y)| {
            screen.is_none_or(|(w, h)| {
                (0.0..f64::from(w)).contains(&x) && (0.0..f64::from(h)).contains(&y)
            })
        });
        if (!on_screen && !all) || list.len() >= TREE_CHARS {
            hidden += 1;
            continue;
        }
        let _ = write!(list, "[{index}] {role}");
        if !label.is_empty() {
            let _ = write!(list, " \"{label}\"");
        }
        if !value.is_empty() && value != label {
            let _ = write!(list, " = \"{value}\"");
        }
        match center.filter(|_| on_screen) {
            Some((x, y)) => {
                let _ = write!(list, " ({x:.0},{y:.0})");
            }
            None => list.push_str(" off-screen"),
        }
        if e["selected"] == true {
            let toggle = TOGGLES.iter().any(|t| role.contains(t));
            list.push_str(if toggle { " checked" } else { " selected" });
        }
        let description = e["description"].as_str().unwrap_or_default();
        if description.contains("not expanded") || description.contains("closed menu") {
            list.push_str(" closed");
        }
        if e["enabled"] == false {
            list.push_str(" disabled");
        }
        list.push('\n');
        listed += 1;
    }
    if !disabled.is_empty() {
        let _ = writeln!(list, "disabled, no id: {}", disabled.join(", "));
    }
    if hidden > 0 {
        let _ = writeln!(
            list,
            "({hidden} more are off-screen, in closed menus or past the limit; computer_state with query lists them)"
        );
    }
    (list, by_index, listed)
}

/// From the driver's tree outline: names for unlabelled elements taken from a label inside them, and
/// the disabled controls it lists without an id; disabled menu items only for an open `popup`, since a
/// closed menu keeps its items in the tree.
fn tree_extras(outline: &str, popup: bool) -> (HashMap<i64, String>, Vec<String>) {
    let mut names = HashMap::new();
    let mut disabled: Vec<String> = Vec::new();
    // indexed ancestors of the current line: indent, index, unlabelled
    let mut stack: Vec<(usize, i64, bool)> = Vec::new();
    for line in outline.lines() {
        let body = line.trim_start();
        let indent = line.len() - body.len();
        let Some(body) = body.strip_prefix("- ") else {
            continue;
        };
        while stack.last().is_some_and(|(i, ..)| *i >= indent) {
            stack.pop();
        }
        if let Some((n, rest)) = body.strip_prefix('[').and_then(|b| b.split_once("] ")) {
            if let Ok(index) = n.parse() {
                let unlabelled = rest
                    .split_once(" \"")
                    .is_some_and(|(_, label)| label.starts_with('"'));
                stack.push((indent, index, unlabelled));
            }
            continue;
        }
        let Some((role, rest)) = body.split_once(" = \"") else {
            continue;
        };
        let Some((text, tail)) = rest.rsplit_once('"') else {
            continue;
        };
        let text = clean(text, VIEW_LABEL);
        if text.is_empty() {
            continue;
        }
        if tail.trim() == "(disabled)" {
            if role == "menu item" && !popup {
                continue;
            }
            let entry = format!("{role} \"{text}\"");
            if disabled.len() < VIEW_DISABLED && !disabled.contains(&entry) {
                disabled.push(entry);
            }
        } else if matches!(role, "label" | "static")
            && let Some(&(_, index, true)) = stack.last()
        {
            names.entry(index).or_insert(text);
        }
    }
    (names, disabled)
}

/// `s` on one line without object-replacement characters, cut to `max` characters; the driver's off-screen note is no name.
fn clean(s: &str, max: usize) -> String {
    let s = s.replace('\u{fffc}', "");
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.starts_with("off-screen:") {
        return String::new();
    }
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
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

/// The blocks `[x0, y0, x1, y1)` of RGB frames `a` and `b`, `columns` pixels a row, where a channel
/// differs by more than `tolerance`; the whole frame when the sizes differ.
fn change_box(a: &[u8], b: &[u8], columns: usize, tolerance: u8) -> Option<[usize; 4]> {
    if a.len() != b.len() || columns == 0 {
        return Some([0, 0, columns, usize::MAX]);
    }
    let mut out: Option<[usize; 4]> = None;
    for (i, (p, q)) in a
        .as_chunks::<3>()
        .0
        .iter()
        .zip(b.as_chunks::<3>().0)
        .enumerate()
    {
        if p.iter().zip(q).any(|(x, y)| x.abs_diff(*y) > tolerance) {
            let (x, y) = (i % columns, i / columns);
            let o = out.get_or_insert([x, y, x + 1, y + 1]);
            *o = [o[0].min(x), o[1].min(y), o[2].max(x + 1), o[3].max(y + 1)];
        }
    }
    out
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
    fn a_change_box_bounds_the_blocks_past_the_tolerance() {
        let mut a = vec![100u8; 4 * 3 * 3];
        assert_eq!(change_box(&a, &a, 4, 8), None);
        let b = a.clone();
        a[(4 + 1) * 3] = 120;
        a[(2 * 4 + 2) * 3 + 1] = 90;
        assert_eq!(change_box(&a, &b, 4, 8), Some([1, 1, 3, 3]));
        assert_eq!(change_box(&a, &b, 4, 48), None);
        assert_eq!(change_box(&a, &b[3..], 4, 8), Some([0, 0, 4, usize::MAX]));
    }

    #[test]
    fn compact_lists_on_screen_controls_with_states_and_drops_containers() {
        let state = json!({"structuredContent": {
        "popup": true,
        "tree_markdown": "- dialog = \"Save As\"\n  - [0] panel \"\"\n    - [1] check box \"Wrap  long lines\" [actions=[click]]\n    - [3] list item \"\" [actions=[]]\n        - label = \"Home\"\n        - label = \"New bookmark\"\n    - push button = \"Save\" (disabled)\n    - menu item = \"Undo      \" (disabled)\n    - push button = \"Save\" (disabled)\n",
        "elements": [
            {"element_index": 0, "element_token": "t0", "role": "panel", "label": "\u{fffc}\u{fffc}", "frame": {"x": 0, "y": 0, "w": 100, "h": 100}},
            {"element_index": 1, "element_token": "t1", "role": "check box", "label": "Wrap  long lines", "selected": true, "frame": {"x": 10, "y": 20, "w": 30, "h": 10}},
            {"element_index": 2, "element_token": "t2", "role": "text", "label": "Name:", "value": "a.txt", "frame": {"x": 0, "y": 0, "w": 50, "h": 10}},
            {"element_index": 3, "element_token": "t3", "role": "list item", "selected": true, "frame": {"x": 0, "y": 40, "w": 100, "h": 20}},
            {"element_index": 4, "element_token": "t4", "role": "menu", "label": "File", "description": "closed menu with 19 items; not expanded: click it to open", "frame": {"x": 200, "y": 0, "w": 40, "h": 20}},
            {"element_index": 5, "element_token": "t5", "role": "push button", "label": "Cancel", "frame": {"x": 900, "y": 860, "w": 80, "h": 30}},
            {"element_index": 6, "element_token": "t6", "role": "push button", "label": "Help", "description": "off-screen"},
            {"element_index": 7, "element_token": "t7", "role": "table cell", "frame": {"x": 0, "y": 0, "w": 2, "h": 2}}
        ]}});
        let (list, elements, listed) = compact(&state, Some((1280, 800)), false);
        assert_eq!(
            list,
            "[1] check box \"Wrap long lines\" (25,25) checked\n[2] text \"Name:\" = \"a.txt\" (25,5)\n[3] list item \"Home\" (50,50) selected\n[4] menu \"File\" (220,10) closed\ndisabled, no id: push button \"Save\", menu item \"Undo\"\n(2 more are off-screen, in closed menus or past the limit; computer_state with query lists them)\n"
        );
        assert_eq!((elements.len(), listed), (8, 4));
        assert_eq!(elements[&1].frame, Some([10.0, 20.0, 30.0, 10.0]));
        let (all, _, listed) = compact(&state, Some((1280, 800)), true);
        assert!(
            all.contains(
                "[5] push button \"Cancel\" off-screen\n[6] push button \"Help\" off-screen\n"
            ),
            "{all}"
        );
        assert_eq!(listed, 6);
        let mut window = state.clone();
        window["structuredContent"]["popup"] = false.into();
        let (list, ..) = compact(&window, Some((1280, 800)), false);
        assert!(
            list.contains("disabled, no id: push button \"Save\"\n"),
            "{list}"
        );
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
