//! Screenshot, UI elements and input on an Android device through adb, with text pasted by a scrcpy control session.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use desk_core::args::parse;
use desk_core::shot::png_size;
use desk_core::wire::{Output, into_password_field};
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, OnceCell};

use crate::desk::{self, Node};

const ADB_TIMEOUT: Duration = Duration::from_secs(30);
const SETTLE: Duration = Duration::from_millis(500);
const MAX_WAIT: f64 = 10.0;
const MAX_ELEMENTS: usize = 200;
const LONG_PRESS_MS: u32 = 800;
const SWIPE_MS: u32 = 300;
const TAP_GAP: Duration = Duration::from_millis(100);
const SCROLL_FRACTION: i32 = 3;
const MAX_SCROLL: u32 = 50;
const DEVICE_JAR: &str = "/data/local/tmp/shuttle-scrcpy.jar";
const DEVICE_SU: &str = "/data/local/tmp/su1000";
const SCRCPY_VERSION: &str = "4.1";
const SCRCPY_SCID: &str = "0cb0c11b";
const SET_CLIPBOARD: u8 = 9;
const CONNECT_TRIES: u32 = 50;
const CONNECT_PAUSE: Duration = Duration::from_millis(100);
const ACK_CLIPBOARD: u8 = 1;
const ACK_TIMEOUT: Duration = Duration::from_secs(2);
const PASTE_TIMEOUT: Duration = Duration::from_secs(5);
const DIAGNOSE_TIMEOUT: Duration = Duration::from_secs(1);
const NOT_INJECTED: &str = "the paste key was not injected";
const NO_FIELD: &str = "no field of the window holds the input focus, so the line was not pasted";
const NO_FIELD_FIRST: &str =
    "no field holds the input focus, so nothing was typed of a text that starts with a line break";
const TYPE_WAIT: Duration = Duration::from_secs(1);
const TYPE_DEADLINE: Duration = Duration::from_millis(1500);
const TYPE_FOCUS_POLL: Duration = Duration::from_millis(20);
const VERIFY_WAIT: Duration = Duration::from_millis(300);
const PASTE_SETTLE: Duration = Duration::from_millis(500);
const VERIFY_POLL: Duration = Duration::from_millis(10);
const VERIFY_TAIL: usize = 16;
const DEVICE_UI_JAR: &str = "/data/local/tmp/shuttle-ui.jar";
const UI_TIMEOUT: Duration = Duration::from_secs(10);
const STATE_TRIES: u32 = 2;
const UI_END: &str = "<<end>>";
const MAX_PNG: usize = 32 << 20;
const UI_STDERR_KEEP: usize = 4096;
const UI_EXIT_WAIT: Duration = Duration::from_secs(2);
const UI_LOG_WAIT: Duration = Duration::from_secs(2);
const UI_LOG_LINES: &str = "200";
const UI_ABORT_KEEP: usize = 512;
const KEYCODES: [(&str, u32); 26] = [
    ("return", 66),
    ("enter", 66),
    ("kp_enter", 66),
    ("tab", 61),
    ("escape", 111),
    ("backspace", 67),
    ("delete", 112),
    ("home", 122),
    ("end", 123),
    ("pageup", 92),
    ("prior", 92),
    ("pagedown", 93),
    ("next", 93),
    ("up", 19),
    ("down", 20),
    ("left", 21),
    ("right", 22),
    ("space", 62),
    ("menu", 82),
    ("back", 4),
    ("home_screen", 3),
    ("recents", 187),
    ("ctrl", 113),
    ("control", 113),
    ("shift", 59),
    ("alt", 57),
];

/// Where an Android device is, and the host files its UI server and paste push to it.
pub struct Device {
    pub adb: PathBuf,
    /// The adb serial; none takes adb's own choice, such as `ANDROID_SERIAL`.
    pub serial: Option<String>,
    pub ui_jar: PathBuf,
    pub scrcpy_jar: PathBuf,
    /// The helper that runs the paste server as the system uid; none runs it as adb's shell uid.
    pub su: Option<PathBuf>,
}

/// adb on an Android device, with the front window and element centres of each session's latest computer_state.
pub struct Android {
    device: Device,
    points: std::sync::Mutex<HashMap<String, Saved>>,
    ui: Mutex<Option<UiServer>>,
    server_input: AtomicBool,
    control: Arc<Mutex<Option<Control>>>,
    natural: OnceCell<(i32, i32)>,
}

impl Android {
    pub fn new(device: Device) -> Self {
        Self {
            device,
            points: std::sync::Mutex::new(HashMap::new()),
            ui: Mutex::new(None),
            server_input: AtomicBool::new(true),
            control: Arc::default(),
            natural: OnceCell::new(),
        }
    }

    /// The screen as PNG.
    pub async fn look(&self) -> Result<Output, String> {
        let png = self.screenshot().await?;
        Ok(Output {
            text: png_size(&png)
                .map_or_else(|| "screen".to_owned(), |(w, h)| format!("{w}x{h} screen")),
            image_b64: Some(B64.encode(png)),
            ..Output::default()
        })
    }

    /// The screen as PNG from the UI server, else from `screencap` when the server has no screenshot to give.
    pub(crate) async fn screenshot(&self) -> Result<Vec<u8>, String> {
        match self.served_png().await {
            Ok(png) => Ok(png),
            Err(_) => self.adb(&["exec-out", "screencap", "-p"]).await,
        }
    }

    /// The UI elements of the screen, [index] for computer_act, and a screenshot.
    pub async fn state(&self, args: Value) -> Result<Output, String> {
        let args: StateArgs = parse(args)?;
        let mut tries = 0;
        let (xml, shot, front) = loop {
            let (xml, shot, front) =
                tokio::join!(self.served("dump"), self.look(), desk::front(self));
            let (xml, front) = (xml?, front?);
            if desk::same_window(&xml, &front) {
                break (xml, shot, front);
            }
            tries += 1;
            if tries == STATE_TRIES {
                return Err("the screen is changing; call computer_state again".to_owned());
            }
        };
        let mut elements = desk::elements(&xml, &front);
        if let Some(q) = args.query.as_deref().map(str::to_lowercase) {
            elements.retain(|e| e.mentions(&q));
        }
        elements.truncate(MAX_ELEMENTS);
        let mut text = String::new();
        for (i, e) in elements.iter().enumerate() {
            if i > 0 {
                text.push('\n');
            }
            text.push_str(&e.line(i));
        }
        let saved = Saved {
            window: front.window().to_owned(),
            screen: front.screen(),
            points: elements.iter().map(Node::center).collect(),
        };
        self.saved().insert(args.session, saved);
        let mut out = shot?;
        out.text = if text.is_empty() {
            "no UI elements".to_owned()
        } else {
            text
        };
        Ok(out)
    }

    /// Performs one input action, then returns a fresh screenshot.
    pub async fn act(&self, args: Value) -> Result<Output, String> {
        let args: ActArgs = parse(args)?;
        let point = self.point(&args).await?;
        let target = || point.ok_or_else(|| format!("{} needs element or x,y", args.action));
        let report = match args.action.as_str() {
            "tap" | "click" => {
                let (x, y) = target()?;
                self.tap((x, y)).await?;
                format!("tapped {x},{y}")
            }
            "double_tap" | "double_click" => {
                let (x, y) = target()?;
                self.taps((x, y), 2).await?;
                format!("double-tapped {x},{y}")
            }
            "long_press" => {
                let (x, y) = target()?;
                self.swipe((x, y), (x, y), LONG_PRESS_MS).await?;
                format!("long-pressed {x},{y}")
            }
            "swipe" | "drag" => {
                let from = target()?;
                let to = (
                    args.to_x.ok_or("swipe needs to_x")? as i32,
                    args.to_y.ok_or("swipe needs to_y")? as i32,
                );
                self.swipe(from, to, SWIPE_MS).await?;
                format!("swiped {},{} to {},{}", from.0, from.1, to.0, to.1)
            }
            "scroll" => {
                let (w, h) = desk::screen(self).await?;
                let (x, y) = point.unwrap_or((w / 2, h / 2));
                let (dx, dy) = match args.direction.as_deref().unwrap_or("down") {
                    "up" => (0, h / SCROLL_FRACTION),
                    "down" => (0, -h / SCROLL_FRACTION),
                    "left" => (w / SCROLL_FRACTION, 0),
                    "right" => (-w / SCROLL_FRACTION, 0),
                    other => return Err(format!("unknown direction {other}")),
                };
                for _ in 0..args.amount.unwrap_or(1).clamp(1, MAX_SCROLL) {
                    self.swipe((x, y), (x + dx, y + dy), SWIPE_MS).await?;
                }
                "scrolled".to_owned()
            }
            "type" => {
                let text = args.text.as_deref().ok_or("type needs text")?;
                if let Some(p) = point {
                    self.tap(p).await?;
                }
                let chars = text.chars().count();
                match self.type_text(text, point, || async { Ok(None) }).await?.0 {
                    Some(true) => format!("typed {chars} characters"),
                    Some(false) => {
                        format!("pasted {chars} characters; the field does not show them")
                    }
                    None => format!("pasted {chars} characters, unverified"),
                }
            }
            "key" => {
                let key = args.key.as_deref().ok_or("key needs key")?;
                self.keys(key).await?;
                format!("pressed {key}")
            }
            "launch_app" => {
                let app = args.app.as_deref().ok_or("launch_app needs app")?;
                format!("launched {}", self.launch(app).await?)
            }
            "wait" => {
                let s = args.seconds.unwrap_or(1.0).clamp(0.0, MAX_WAIT);
                tokio::time::sleep(Duration::from_secs_f64(s)).await;
                format!("waited {s}s")
            }
            other => return Err(format!("unknown action {other}")),
        };
        tokio::time::sleep(SETTLE).await;
        Ok(match self.look().await {
            Ok(mut out) => {
                out.text = report;
                out
            }
            Err(e) => Output::text(format!("{report}; no screenshot after it: {e}")),
        })
    }

    /// Forgets the element centres of a session, the default one when none is named.
    pub async fn end(&self, args: &Value) -> Result<String, String> {
        self.release(args["session"].as_str().unwrap_or_default());
        Ok("ended".to_owned())
    }

    pub fn release(&self, session: &str) {
        self.saved().remove(session);
    }

    /// Starts component `app`, or the launcher activity of package `app`; its component.
    pub(crate) async fn launch(&self, app: &str) -> Result<String, String> {
        if is_component(app) {
            self.start(app).await?;
            return Ok(app.to_owned());
        }
        if !is_package(app) {
            return Err(format!(
                "launch {app:?}: not an Android package or component name"
            ));
        }
        let out = self
            .adb(&[
                "shell",
                "cmd",
                "package",
                "resolve-activity",
                "--brief",
                "-c",
                "android.intent.category.LAUNCHER",
                app,
            ])
            .await?;
        let component = launcher(&String::from_utf8_lossy(&out), app)
            .filter(|c| is_component(c))
            .ok_or_else(|| format!("launch {app}: no launcher activity"))?;
        self.start(&component).await?;
        Ok(component)
    }

    pub(crate) async fn tap(&self, (x, y): (i32, i32)) -> Result<(), String> {
        if self
            .inject(&format!("tap {x} {y}"), || format!("tap {x},{y}"))
            .await?
        {
            return Ok(());
        }
        self.input(&["tap", &x.to_string(), &y.to_string()]).await
    }

    pub(crate) async fn taps(&self, (x, y): (i32, i32), count: u32) -> Result<(), String> {
        if count <= 1 {
            return self.tap((x, y)).await;
        }
        let cmd = format!("tap {x} {y}");
        for i in 1..=count {
            if i > 1 {
                tokio::time::sleep(TAP_GAP).await;
            }
            if !self
                .inject(&cmd, || format!("tap {i} of {count} at {x},{y}"))
                .await?
            {
                return Err(if i == 1 {
                    format!(
                        "{count} taps at {x},{y} need the UI server's touch injection, since adb input taps too slowly for one gesture"
                    )
                } else {
                    format!("tap {i} of {count} at {x},{y} was not sent")
                });
            }
        }
        Ok(())
    }

    /// Presses a key or a combination such as `ctrl+a`.
    pub(crate) async fn keys(&self, keys: &str) -> Result<(), String> {
        let codes = keycodes(keys)?;
        if let [code] = codes[..]
            && self
                .inject(&format!("key {code}"), || format!("key {keys}"))
                .await?
        {
            return Ok(());
        }
        let codes: Vec<String> = codes.iter().map(u32::to_string).collect();
        let mut argv = vec![if codes.len() == 1 {
            "keyevent"
        } else {
            "keycombination"
        }];
        argv.extend(codes.iter().map(String::as_str));
        self.input(&argv).await
    }

    pub(crate) async fn swipe(
        &self,
        from: (i32, i32),
        to: (i32, i32),
        ms: u32,
    ) -> Result<(), String> {
        let args = [from.0, from.1, to.0, to.1].map(|v| v.to_string());
        let ms = ms.to_string();
        self.input(&["swipe", &args[0], &args[1], &args[2], &args[3], &ms])
            .await
    }

    /// The UI tree of window `window` from the UI server.
    pub(crate) async fn tree(&self, window: &str) -> Result<String, String> {
        self.served(&format!("dump {window}")).await
    }

    /// Whether the element at `path` of window `window` took the UI server's request to scroll itself into view.
    pub(crate) async fn show(&self, window: &str, path: &str) -> bool {
        let answer = self.served(&format!("show {window} {path}")).await;
        answer.is_ok_and(|a| a.trim() == "true")
    }

    /// The interactive windows as the UI server lists them, as JSON.
    pub(crate) async fn windows(&self) -> Result<String, String> {
        self.served("windows").await
    }

    /// Types `text` into the focused field: each line pasted after `check` passes, Return between lines, and the clipboard emptied after; true beside the outcome when a line went into a password field.
    pub(crate) async fn type_text<C, Cf>(
        &self,
        text: &str,
        at: Option<(i32, i32)>,
        check: C,
    ) -> Result<(Option<bool>, bool), String>
    where
        C: FnMut() -> Cf,
        Cf: Future<Output = Result<Option<i64>, String>>,
    {
        let mut wipe = Wipe(Some(Arc::clone(&self.control)));
        let mut protected = false;
        let typed = self.type_lines(text, at, check, &mut protected).await;
        wipe.0 = None;
        if wiped(&self.control).await
            && let Ok(mut control) = self.open_control().await
            && control.set_clipboard("").await.is_ok()
        {
            *self.control.lock().await = Some(control);
        }
        typed
            .map(|typed| (typed, protected))
            .map_err(|e| into_password_field(e, protected))
    }

    async fn type_lines<C, Cf>(
        &self,
        text: &str,
        at: Option<(i32, i32)>,
        mut check: C,
        protected: &mut bool,
    ) -> Result<Option<bool>, String>
    where
        C: FnMut() -> Cf,
        Cf: Future<Output = Result<Option<i64>, String>>,
    {
        let mut typed = Some(true);
        let mut pin = None;
        let rows: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
        let lines = rows.len();
        for (i, line) in rows.iter().copied().enumerate() {
            let partly = |e: String| match i {
                0 => e,
                _ => format!("{e}; {i} of {lines} lines were typed before it"),
            };
            let returned = |e: String| match i {
                0 => e,
                _ => format!(
                    "{e}; {i} of {lines} lines and the Return after line {i} were typed before it"
                ),
            };
            let mut window = check().await.map_err(partly)?;
            if i > 0 {
                self.keys("return").await.map_err(partly)?;
            }
            if line.is_empty() {
                if i == 0 && window.is_none() && at.is_none() && rows.iter().any(|l| !l.is_empty())
                {
                    let (id, password) = self.field_window().await?.ok_or(NO_FIELD_FIRST)?;
                    *protected |= password;
                    pin = Some(id);
                }
                continue;
            }
            if i > 0 {
                window = match check().await.map_err(returned)? {
                    Some(w) => Some(w),
                    None => Some(pin.ok_or_else(|| returned(NO_FIELD.to_owned()))?),
                };
            }
            let mut took = None;
            let refreshed = AtomicBool::new(i == 0);
            let shown = type_with(
                line,
                at.filter(|_| i == 0),
                window,
                i == 0,
                &mut took,
                |live| self.field_focus(live, window, &refreshed),
                || {
                    refreshed.store(true, Ordering::Relaxed);
                    self.paste(line)
                },
            )
            .await;
            *protected |= took.is_some_and(|(_, password)| password);
            let shown = shown.map_err(returned)?;
            if i == 0 {
                pin = took.and_then(|(id, _)| id);
            }
            typed = match (typed, shown) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (None, _) | (_, None) => None,
                _ => Some(true),
            };
        }
        Ok(typed)
    }

    async fn focus(&self, live: bool) -> Result<Option<Focus>, String> {
        let answer = self
            .served(if live { "focus live" } else { "focus" })
            .await?;
        serde_json::from_str(answer.trim()).map_err(|e| format!("decode focus: {e}"))
    }

    async fn field_focus(
        &self,
        live: bool,
        window: Option<i64>,
        refreshed: &AtomicBool,
    ) -> Result<Option<Focus>, String> {
        let focus = self.focus(live).await?;
        if let (Some(w), Some(f)) = (window, &focus)
            && !f.editable
            && f.window
            && f.id == Some(w)
            && !refreshed.swap(true, Ordering::Relaxed)
            && self.tree(&w.to_string()).await.is_ok()
        {
            return self.focus(live).await;
        }
        Ok(focus)
    }

    async fn field_window(&self) -> Result<Option<(i64, bool)>, String> {
        let focus = self.focus(true).await?;
        Ok(focus
            .filter(|f| f.editable && f.window)
            .and_then(|f| f.id.map(|id| (id, f.password))))
    }

    /// The screen size `wm size` reports in the device's natural rotation, read once per device.
    pub(crate) async fn natural(&self) -> Result<(i32, i32), String> {
        self.natural
            .get_or_try_init(|| async {
                let out = self.adb(&["shell", "wm", "size"]).await?;
                let out = String::from_utf8_lossy(&out);
                wm_size(&out).ok_or_else(|| format!("wm size said {:?}", out.trim()))
            })
            .await
            .copied()
    }

    async fn input(&self, args: &[&str]) -> Result<(), String> {
        let mut argv = vec!["shell", "input"];
        argv.extend_from_slice(args);
        self.adb(&argv).await.map(drop)
    }

    /// Pastes `text` through the clipboard, which takes any Unicode; false when the device refused the paste key.
    async fn paste(&self, text: &str) -> Result<bool, String> {
        {
            let mut slot = self.control.lock().await;
            let control = match slot.take() {
                Some(control) if control.is_open() => slot.insert(control),
                _ => slot.insert(self.open_control().await?),
            };
            if let Err(e) = control.set_clipboard(text).await {
                *slot = self.open_control().await.ok();
                return Err(e);
            }
        }
        match tokio::time::timeout(PASTE_TIMEOUT, self.served("paste")).await {
            Ok(answer) => answer.map(|a| a.trim() == "true"),
            Err(_) => Err(unhandled_paste(&self.focused_window().await)),
        }
    }

    async fn adb(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        let mut cmd = self.command();
        cmd.args(args).stdin(Stdio::null()).kill_on_drop(true);
        let out = tokio::time::timeout(ADB_TIMEOUT, cmd.output())
            .await
            .map_err(|_| format!("adb {}: timed out", args.join(" ")))?
            .map_err(|e| format!("run adb: {e}"))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(format!(
                "adb {}: {}: {}",
                args.join(" "),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    /// Starts activity `component`, quoted so the device shell keeps a nested class's `$`.
    async fn start(&self, component: &str) -> Result<(), String> {
        self.adb(&[
            "shell",
            "am",
            "start",
            "-W",
            "-n",
            &format!("'{component}'"),
        ])
        .await
        .map(drop)
    }

    async fn inject(&self, cmd: &str, what: impl FnOnce() -> String) -> Result<bool, String> {
        if !self.server_input.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let mut slot = self.ui.lock().await;
        let Ok(mut server) = self.ui_server(&mut slot).await else {
            return Ok(false);
        };
        if server.send(cmd).await.is_err() {
            return Ok(false);
        }
        let answer = match tokio::time::timeout(UI_TIMEOUT, server.read_answer()).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(e)) => return Err(unconfirmed(&what(), &e)),
            Err(_) => return Err(unconfirmed(&what(), "UI server: timed out")),
        };
        *slot = Some(server);
        match answer.trim() {
            "true" => Ok(true),
            "false" => Err(unconfirmed(&what(), "the injection was not confirmed")),
            other => match server_error(other) {
                Some(e) => Err(unconfirmed(&what(), e)),
                None => {
                    self.server_input.store(false, Ordering::Relaxed);
                    Ok(false)
                }
            },
        }
    }

    /// The window `dumpsys window` names as focused, for an error about it.
    async fn focused_window(&self) -> String {
        let out = tokio::time::timeout(
            DIAGNOSE_TIMEOUT,
            self.adb(&["shell", "dumpsys", "window", "displays"]),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
        String::from_utf8_lossy(&out)
            .lines()
            .find_map(|l| l.trim().strip_prefix("mCurrentFocus="))
            .map_or_else(|| "the focused window".to_owned(), str::to_owned)
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.device.adb);
        if let Some(serial) = &self.device.serial {
            cmd.args(["-s", serial]);
        }
        cmd
    }

    async fn point(&self, args: &ActArgs) -> Result<Option<(i32, i32)>, String> {
        let Some(i) = args.element else {
            return Ok(args.x.zip(args.y).map(|(x, y)| (x as i32, y as i32)));
        };
        let (window, screen, point) = {
            self.saved()
                .get(&args.session)
                .and_then(|s| Some((s.window.clone(), s.screen, *s.points.get(i)?)))
                .ok_or_else(|| format!("no element [{i}] in the latest computer_state"))?
        };
        let front = desk::front(self).await?;
        front.settled()?;
        if window.is_empty() || front.window().is_empty() {
            return Err(
                "the window list is unavailable; use x,y or call computer_state again".to_owned(),
            );
        }
        if front.window() != window || front.screen() != screen {
            return Err("the screen changed since computer_state; call it again".to_owned());
        }
        if front.covered(point) {
            return Err(format!(
                "element [{i}] is covered now; call computer_state again"
            ));
        }
        Ok(Some(point))
    }

    fn saved(&self) -> std::sync::MutexGuard<'_, HashMap<String, Saved>> {
        self.points
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// One answer of the UI server, which is dropped when it fails or is cancelled mid-answer.
    async fn served(&self, cmd: &str) -> Result<String, String> {
        let mut slot = self.ui.lock().await;
        let mut server = self.ui_server(&mut slot).await?;
        let answer = tokio::time::timeout(UI_TIMEOUT, server.ask(cmd))
            .await
            .map_err(|_| "UI server: timed out".to_owned())??;
        *slot = Some(server);
        if let Some(e) = server_error(&answer) {
            return Err(format!("UI server: {e}"));
        }
        Ok(answer)
    }

    /// The server's PNG frame; a server that answers with an error stays up.
    async fn served_png(&self) -> Result<Vec<u8>, String> {
        let mut slot = self.ui.lock().await;
        let mut server = self.ui_server(&mut slot).await?;
        let png = tokio::time::timeout(UI_TIMEOUT, server.ask_png())
            .await
            .map_err(|_| "UI server: timed out".to_owned())??;
        *slot = Some(server);
        png
    }

    async fn ui_server(&self, slot: &mut Option<UiServer>) -> Result<UiServer, String> {
        if let Some(mut server) = slot.take()
            && matches!(server.child.try_wait(), Ok(None))
        {
            return Ok(server);
        }
        self.start_ui().await
    }

    async fn start_ui(&self) -> Result<UiServer, String> {
        self.adb(&["push", &self.device.ui_jar.to_string_lossy(), DEVICE_UI_JAR])
            .await?;
        let _ = self.adb(&["shell", "pkill", "-f", "shuttle.Ui"]).await;
        let mut cmd = self.command();
        cmd.args([
            "shell",
            &format!("CLASSPATH={DEVICE_UI_JAR}"),
            "app_process",
            "/",
            "shuttle.Ui",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| format!("run UI server: {e}"))?;
        let stdin = child.stdin.take().ok_or("take UI server stdin")?;
        let mut stdout = BufReader::new(child.stdout.take().ok_or("take UI server stdout")?);
        let stderr = child.stderr.take().ok_or("take UI server stderr")?;
        let mut ready = String::new();
        tokio::time::timeout(UI_TIMEOUT, stdout.read_line(&mut ready))
            .await
            .map_err(|_| "UI server start timed out".to_owned())?
            .map_err(read_err)?;
        if ready.trim() != "ready" {
            return Err(self.start_failure(&mut child, stderr, ready.trim()).await);
        }
        Ok(UiServer {
            child,
            stdin,
            stdout,
        })
    }

    async fn start_failure(&self, child: &mut Child, stderr: ChildStderr, said: &str) -> String {
        let status = match tokio::time::timeout(UI_EXIT_WAIT, child.wait()).await {
            Ok(Ok(status)) => status.to_string(),
            _ => {
                let _ = child.start_kill();
                "still running".to_owned()
            }
        };
        let tail = tokio::time::timeout(UI_EXIT_WAIT, tail(stderr, UI_STDERR_KEEP))
            .await
            .unwrap_or_default();
        let log = tokio::time::timeout(
            UI_LOG_WAIT,
            self.adb(&["logcat", "-d", "-b", "crash", "-t", UI_LOG_LINES]),
        )
        .await;
        let cause = match log {
            Ok(Ok(log)) => abort_cause(&String::from_utf8_lossy(&log)),
            _ => None,
        };
        format!(
            "UI server said {said:?} and {status}: {}\n{}",
            String::from_utf8_lossy(&tail).trim(),
            cause.map_or_else(
                || "see adb logcat".to_owned(),
                |c| format!(
                    "last shuttle.Ui abort in the crash log (may be from an earlier start): {c}"
                )
            )
        )
    }

    async fn open_control(&self) -> Result<Control, String> {
        let _ = self
            .adb(&["shell", "pkill", "-f", &format!("scid={SCRCPY_SCID}")])
            .await;
        self.adb(&[
            "push",
            &self.device.scrcpy_jar.to_string_lossy(),
            DEVICE_JAR,
        ])
        .await?;
        if let Some(su) = &self.device.su {
            self.adb(&["push", &su.to_string_lossy(), DEVICE_SU])
                .await?;
        }
        let socket = format!("localabstract:scrcpy_{SCRCPY_SCID}");
        let port = String::from_utf8_lossy(&self.adb(&["forward", "tcp:0", &socket]).await?)
            .trim()
            .to_owned();
        let forward = Forward::new(self, port);
        let server = self.scrcpy_control()?;
        let stream = connect_control(&forward.port).await?;
        Ok(Control {
            stream,
            sequence: 0,
            pending: false,
            filled: false,
            _server: server,
            _forward: forward,
        })
    }

    /// A control-only scrcpy server, as the system uid through `su` when the device has one; dropping the child stops it.
    fn scrcpy_control(&self) -> Result<Child, String> {
        let mut cmd = self.command();
        cmd.args(["shell", &format!("CLASSPATH={DEVICE_JAR}")]);
        if self.device.su.is_some() {
            cmd.arg(DEVICE_SU);
        }
        cmd.args([
            "app_process",
            "/",
            "com.genymobile.scrcpy.Server",
            SCRCPY_VERSION,
            &format!("scid={SCRCPY_SCID}"),
            "log_level=warn",
            "tunnel_forward=true",
            "video=false",
            "audio=false",
            "control=true",
            "send_device_meta=false",
            "send_dummy_byte=true",
            "cleanup=false",
            "clipboard_autosync=false",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
        cmd.spawn().map_err(|e| format!("run scrcpy control: {e}"))
    }
}

struct Saved {
    window: String,
    screen: (i32, i32),
    points: Vec<(i32, i32)>,
}

/// The UI server (`android/ui`) on the device, one answer per request line; dropping it stops the server.
struct UiServer {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl UiServer {
    /// The answer to `shot`: `png N`, N bytes and the end line; the inner error is the server's answer, read to its end.
    async fn ask_png(&mut self) -> Result<Result<Vec<u8>, String>, String> {
        self.send("shot").await?;
        let mut head = String::new();
        self.stdout.read_line(&mut head).await.map_err(read_err)?;
        let Some(len) = head
            .strip_prefix("png ")
            .and_then(|n| n.trim().parse::<usize>().ok())
            .filter(|n| *n <= MAX_PNG)
        else {
            if head.trim_end() != UI_END {
                self.read_answer().await?;
            }
            return Ok(Err(match server_error(&head) {
                Some(e) => format!("UI server: {e}"),
                None => format!("UI server: unexpected answer to shot: {}", head.trim()),
            }));
        };
        let mut png = vec![0; len];
        self.stdout.read_exact(&mut png).await.map_err(read_err)?;
        let mut end = String::new();
        self.stdout.read_line(&mut end).await.map_err(read_err)?;
        if end.trim_end() != UI_END {
            return Err("UI server: the PNG frame has no end".to_owned());
        }
        Ok(Ok(png))
    }

    async fn ask(&mut self, cmd: &str) -> Result<String, String> {
        self.send(cmd).await?;
        self.read_answer().await
    }

    async fn send(&mut self, cmd: &str) -> Result<(), String> {
        self.stdin
            .write_all(format!("{cmd}\n").as_bytes())
            .await
            .map_err(|e| format!("ask UI server: {e}"))
    }

    async fn read_answer(&mut self) -> Result<String, String> {
        let mut answer = String::new();
        loop {
            let start = answer.len();
            let n = self.stdout.read_line(&mut answer).await.map_err(read_err)?;
            if n == 0 {
                return Err("UI server exited".to_owned());
            }
            if answer[start..].trim_end() == UI_END {
                answer.truncate(start);
                return Ok(answer);
            }
        }
    }
}

/// The resident control-only scrcpy session of the paste: its socket, server and adb forward, which a drop ends.
struct Control {
    stream: TcpStream,
    sequence: u64,
    pending: bool,
    filled: bool,
    _server: Child,
    _forward: Forward,
}

impl Control {
    /// Whether the server still holds the session; it sends nothing unasked, so anything readable but a due acknowledgement is its end.
    fn is_open(&self) -> bool {
        self.pending
            || matches!(
                self.stream.try_read(&mut [0; 1]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
            )
    }

    async fn set_clipboard(&mut self, text: &str) -> Result<(), String> {
        if self.pending {
            self.ack().await?;
        }
        self.sequence += 1;
        self.pending = true;
        self.filled |= !text.is_empty();
        self.stream
            .write_all(&clipboard_message(text, self.sequence))
            .await
            .map_err(|e| format!("send clipboard: {e}"))?;
        self.ack().await?;
        self.filled = !text.is_empty();
        Ok(())
    }

    async fn ack(&mut self) -> Result<(), String> {
        tokio::time::timeout(ACK_TIMEOUT, read_ack(&mut self.stream, self.sequence))
            .await
            .map_err(|_| "scrcpy control: no clipboard acknowledgement".to_owned())??;
        self.pending = false;
        Ok(())
    }
}

struct Wipe(Option<Arc<Mutex<Option<Control>>>>);

impl Drop for Wipe {
    fn drop(&mut self) {
        if let Some(slot) = self.0.take()
            && let Ok(rt) = tokio::runtime::Handle::try_current()
        {
            rt.spawn(async move { wiped(&slot).await });
        }
    }
}

struct Forward {
    port: String,
    remove: Command,
}

impl Forward {
    fn new(android: &Android, port: String) -> Self {
        let mut remove = android.command();
        remove
            .args(["forward", "--remove", &format!("tcp:{port}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Self { port, remove }
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        let _ = self.remove.spawn();
    }
}

/// The node with input focus, as the UI server's `focus` names it: its text is a length, a hash and its last 32 code points.
#[derive(Debug, Default, Deserialize)]
struct Focus {
    editable: bool,
    password: bool,
    window: bool,
    id: Option<i64>,
    bounds: [i32; 4],
    len: usize,
    hash: i32,
    tail: String,
}

impl Focus {
    fn holds(&self, (x, y): (i32, i32)) -> bool {
        let [left, top, right, bottom] = self.bounds;
        (left..right).contains(&x) && (top..bottom).contains(&y)
    }

    fn same_text(&self, other: &Self) -> bool {
        self.len == other.len && self.hash == other.hash && self.tail == other.tail
    }

    fn ends_with(&self, line: &str) -> bool {
        let start = line
            .char_indices()
            .rev()
            .nth(VERIFY_TAIL - 1)
            .map_or(0, |(i, _)| i);
        self.tail.ends_with(&line[start..])
    }
}

#[derive(Deserialize)]
struct StateArgs {
    #[serde(default)]
    session: String,
    query: Option<String>,
}

#[derive(Deserialize)]
struct ActArgs {
    #[serde(default)]
    session: String,
    action: String,
    element: Option<usize>,
    x: Option<f64>,
    y: Option<f64>,
    to_x: Option<f64>,
    to_y: Option<f64>,
    text: Option<String>,
    key: Option<String>,
    direction: Option<String>,
    amount: Option<u32>,
    app: Option<String>,
    seconds: Option<f64>,
}

/// Whether `s` is an Android package name, such as `com.android.settings`, which a device shell takes as it is.
pub(crate) fn is_package(s: &str) -> bool {
    s.contains('.')
        && s.split('.').all(|part| {
            part.starts_with(|c: char| c.is_ascii_alphabetic())
                && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

fn unhandled_paste(focus: &str) -> String {
    let stalled = match focus {
        "null" => "no window has input focus to handle the paste key".to_owned(),
        window => format!("{window} did not handle the paste key"),
    };
    format!(
        "{stalled} within {} s; the paste may still land, so check the field before typing again",
        PASTE_TIMEOUT.as_secs()
    )
}

fn read_err(e: std::io::Error) -> String {
    format!("read UI server: {e}")
}

fn server_error(answer: &str) -> Option<&str> {
    answer
        .trim()
        .strip_prefix("<error>")
        .map(|e| e.trim_end_matches("</error>"))
}

fn abort_cause(log: &str) -> Option<String> {
    let mut ui = false;
    let mut cause = None;
    for line in log.lines() {
        if let Some((_, cmdline)) = line.split_once("Cmdline: ") {
            ui = cmdline.trim().ends_with("shuttle.Ui");
        } else if let Some((_, abort)) = line.split_once("Abort message: ")
            && ui
        {
            let abort = abort.trim().trim_start_matches('\'').trim_end_matches('\'');
            let end = abort.floor_char_boundary(UI_ABORT_KEEP);
            cause = Some(abort[..end].to_owned()).filter(|c| !c.is_empty());
        }
    }
    cause
}

async fn tail(mut reader: impl AsyncRead + Unpin, keep: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunk = [0; 4096];
    while let Ok(n) = reader.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n]);
        if out.len() > keep {
            out.drain(..out.len() - keep);
        }
    }
    out
}

fn unconfirmed(what: &str, why: &str) -> String {
    format!(
        "{what} is unconfirmed ({why}) and was not repeated, since it may have reached the device"
    )
}

/// The keycodes of `keys` such as `ctrl+a`; desktop names keep their meaning, the phone's keys are back, home_screen, recents.
fn keycodes(keys: &str) -> Result<Vec<u32>, String> {
    keys.split('+')
        .map(|k| {
            let k = k.trim().to_lowercase();
            KEYCODES
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, code)| *code)
                .or_else(|| letter(&k))
                .ok_or_else(|| format!("unknown key {k:?} in {keys:?}"))
        })
        .collect()
}

/// The keycode of one letter, digit or function key.
fn letter(k: &str) -> Option<u32> {
    let mut chars = k.chars();
    match (chars.next(), chars.next()) {
        (Some(c @ 'a'..='z'), None) => Some(29 + (c as u32 - 'a' as u32)),
        (Some(c @ '0'..='9'), None) => Some(7 + (c as u32 - '0' as u32)),
        (Some('f'), Some(_)) => k[1..]
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=12).contains(n))
            .map(|n| 130 + n),
        _ => None,
    }
}

/// The `package/activity` line of `cmd package resolve-activity --brief` for `app`.
fn launcher(out: &str, app: &str) -> Option<String> {
    let prefix = format!("{app}/");
    out.lines()
        .map(str::trim)
        .find(|l| l.starts_with(&prefix))
        .map(str::to_owned)
}

/// Pastes `line` into the field `focus` names: Some(true) when it shows, Some(false) when the text stayed the same, None when unchecked.
async fn type_with<F, Ff, P, Pf>(
    line: &str,
    at: Option<(i32, i32)>,
    window: Option<i64>,
    blind: bool,
    took: &mut Option<(Option<i64>, bool)>,
    mut focus: F,
    mut paste: P,
) -> Result<Option<bool>, String>
where
    F: FnMut(bool) -> Ff,
    Ff: Future<Output = Result<Option<Focus>, String>>,
    P: FnMut() -> Pf,
    Pf: Future<Output = Result<bool, String>>,
{
    let usable = |f: &Focus| {
        f.editable
            && f.window
            && at.is_none_or(|p| f.holds(p))
            && window.is_none_or(|w| f.id == Some(w))
    };
    let start = tokio::time::Instant::now();
    let end = start + TYPE_WAIT;
    let before = loop {
        let cached = focus(false).await?;
        if cached.as_ref().is_some_and(|f| usable(f) && f.password) {
            *took = Some((None, true));
        }
        if cached.as_ref().is_some_and(usable)
            && let Some(f) = focus(true).await?.filter(usable)
        {
            break Some((f, cached));
        }
        if tokio::time::Instant::now() >= end {
            if let (Some((x, y)), Some(f)) = (at, &cached)
                && f.editable
                && f.window
            {
                return Err(format!(
                    "the field at ({x},{y}) has no input focus; another field does"
                ));
            }
            if let (Some(w), Some(f)) = (window, &cached)
                && f.id.is_some_and(|id| id != w)
            {
                return Err(
                    "the input focus is in another window, so the line was not pasted".to_owned(),
                );
            }
            break None;
        }
        tokio::time::sleep(TYPE_FOCUS_POLL).await;
    };
    if before.is_none() && !blind {
        return Err(NO_FIELD.to_owned());
    }
    let latched = took.is_some_and(|(_, password)| password);
    *took = match &before {
        Some((f, _)) => Some((f.id, f.password || latched)),
        None => latched.then_some((None, true)),
    };
    let (before, cached) = match before {
        Some((f, cached)) if !f.password => (f, cached),
        unchecked => {
            if !paste().await? {
                return Err(NOT_INJECTED.to_owned());
            }
            match unchecked {
                Some((field, cached)) => {
                    let end = tokio::time::Instant::now() + TYPE_WAIT;
                    while tokio::time::Instant::now() < end {
                        match focus(false).await {
                            Err(_) => break,
                            Ok(Some(f))
                                if !f.same_text(&field)
                                    && cached.as_ref().is_none_or(|c| !f.same_text(c)) =>
                            {
                                break;
                            }
                            Ok(_) => tokio::time::sleep(TYPE_FOCUS_POLL).await,
                        }
                    }
                }
                None => tokio::time::sleep(PASTE_SETTLE).await,
            }
            return Ok(None);
        }
    };
    let shows = |f: &Focus| !f.same_text(&before) && f.ends_with(line);
    let mut delivered = false;
    loop {
        if !delivered {
            delivered = paste().await?;
        }
        if delivered {
            let deadline = tokio::time::Instant::now() + VERIFY_WAIT;
            loop {
                let Ok(read) = focus(false).await else {
                    return Ok(None);
                };
                if read
                    .filter(usable)
                    .is_some_and(|f| cached.as_ref().is_none_or(|c| !f.same_text(c)) && shows(&f))
                {
                    return Ok(Some(true));
                }
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(VERIFY_POLL).await;
            }
        } else {
            tokio::time::sleep(TYPE_FOCUS_POLL).await;
        }
        let now = match focus(true).await {
            Err(_) if delivered => return Ok(None),
            read => read?.filter(usable),
        };
        if now.as_ref().is_some_and(shows) {
            return Ok(Some(true));
        }
        if !now.is_some_and(|f| f.same_text(&before)) {
            return if delivered {
                Ok(None)
            } else {
                Err(NOT_INJECTED.to_owned())
            };
        }
        if start.elapsed() >= TYPE_DEADLINE {
            return if delivered {
                Ok(Some(false))
            } else {
                Err(NOT_INJECTED.to_owned())
            };
        }
    }
}

/// Whether `s` is an activity component, `<package>/<class>` with the class relative (`.Main`) or full.
fn is_component(s: &str) -> bool {
    let Some((package, class)) = s.split_once('/') else {
        return false;
    };
    let class = class.strip_prefix('.').unwrap_or(class);
    is_package(package)
        && !class.is_empty()
        && class.split('.').all(|part| {
            part.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        })
}

/// The `WxH` of `wm size` output, its override size before its physical size.
fn wm_size(out: &str) -> Option<(i32, i32)> {
    let size = |label: &str| {
        out.lines()
            .find_map(|l| l.trim().strip_prefix(label))
            .and_then(|v| v.trim().split_once('x'))
            .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
    };
    size("Override size:").or_else(|| size("Physical size:"))
}

/// A scrcpy SET_CLIPBOARD message that sets the clipboard to `text`, without scrcpy's own paste key, and asks for an acknowledgement of `sequence`.
fn clipboard_message(text: &str, sequence: u64) -> Vec<u8> {
    let mut msg = Vec::with_capacity(14 + text.len());
    msg.push(SET_CLIPBOARD);
    msg.extend_from_slice(&sequence.to_be_bytes());
    msg.push(0);
    msg.extend_from_slice(&(text.len() as u32).to_be_bytes());
    msg.extend_from_slice(text.as_bytes());
    msg
}

/// The scrcpy control socket that adb forwards to `port`, once the server's first byte shows it listens.
async fn connect_control(port: &str) -> Result<TcpStream, String> {
    let addr = format!("127.0.0.1:{port}");
    for _ in 0..CONNECT_TRIES {
        if let Ok(mut stream) = TcpStream::connect(&addr).await {
            let mut ready = [0u8; 1];
            if matches!(stream.read(&mut ready).await, Ok(1)) {
                return Ok(stream);
            }
        }
        tokio::time::sleep(CONNECT_PAUSE).await;
    }
    Err("scrcpy control did not start".to_owned())
}

async fn wiped(slot: &Mutex<Option<Control>>) -> bool {
    let mut slot = slot.lock().await;
    let Some(control) = slot.as_mut() else {
        return false;
    };
    if control.is_open() && control.set_clipboard("").await.is_ok() {
        return false;
    }
    let filled = control.filled;
    *slot = None;
    filled
}

/// Reads the device message that acknowledges the clipboard of `sequence`, the only one a server without clipboard autosync sends.
async fn read_ack<R: AsyncRead + Unpin>(r: &mut R, sequence: u64) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("read scrcpy control: {e}");
    let kind = r.read_u8().await.map_err(fail)?;
    if kind != ACK_CLIPBOARD {
        return Err(format!("scrcpy control sent message type {kind}"));
    }
    let acked = r.read_u64().await.map_err(fail)?;
    if acked != sequence {
        return Err(format!(
            "scrcpy control acknowledged clipboard {acked}, not {sequence}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CRASH_LOG: &str = "10-19 08:01:40.100  3200  3200 F DEBUG   : Cmdline: /system/bin/other\n10-19 08:01:40.100  3200  3200 F DEBUG   : Abort message: 'not ours'\n10-19 08:01:50.777  3318  3318 F DEBUG   : Cmdline: app_process / shuttle.Ui\n10-19 08:01:50.777  3318  3318 F DEBUG   : pid: 3308, tid: 3308, name: main  >>> app_process <<<\n10-19 08:01:50.777  3318  3318 F DEBUG   : Abort message: 'No pending exception expected: java.lang.ClassNotFoundException: shuttle.Ui\n10-19 08:01:50.777  3318  3318 F DEBUG   : (Throwable with empty stack trace)'\n10-19 08:01:51.000  3330  3330 F DEBUG   : Cmdline: /system/bin/other\n10-19 08:01:51.000  3330  3330 F DEBUG   : Abort message: 'later, not ours'\n";

    #[test]
    fn desktop_key_names_map_to_android_keycodes() {
        assert_eq!(keycodes("Return").unwrap(), [66]);
        assert_eq!(keycodes("shift+end").unwrap(), [59, 123]);
        assert_eq!(keycodes("ctrl+a").unwrap(), [113, 29]);
        assert_eq!(keycodes("home").unwrap(), [122]);
        assert_eq!(keycodes("back").unwrap(), [4]);
        assert_eq!(keycodes("F5").unwrap(), [135]);
        assert!(keycodes("hyper").is_err());
    }

    #[test]
    fn launcher_is_the_component_line_of_the_package() {
        let out = "priority=0 preferredOrder=0 match=0x108000\norg.fossify.math/.activities.SplashActivity.Green\n";
        assert_eq!(
            launcher(out, "org.fossify.math").as_deref(),
            Some("org.fossify.math/.activities.SplashActivity.Green")
        );
        assert_eq!(launcher("No activity found\n", "x.y"), None);
    }

    #[test]
    fn only_package_and_component_names_reach_the_device_shell() {
        for ok in ["com.android.settings", "org.fossify.math", "a.b_2"] {
            assert!(is_package(ok), "{ok}");
        }
        for bad in [
            "Settings",
            "com..x",
            "1com.x",
            "com.x;reboot",
            "com.x reboot",
            "com.x\nreboot",
            "$(id).x",
            "com.x/.Main",
            "",
        ] {
            assert!(!is_package(bad), "{bad:?}");
        }
        for ok in [
            "org.fossify.math/.activities.SplashActivity.Green",
            "com.android.settings/com.android.settings.Settings",
            "com.example/.Main$Launcher",
        ] {
            assert!(is_component(ok), "{ok}");
        }
        for bad in [
            "com.x/",
            "com.x/.",
            "com.x/.Main;reboot",
            "com.x/.Main'x",
            "com.x/.$Main",
            "com.x/.Main other",
            "x/.Main",
            "com.x",
        ] {
            assert!(!is_component(bad), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn a_launch_of_a_name_that_is_no_package_runs_no_adb() {
        let android = android_at("/nonexistent/adb");
        let err = android.launch("com.x; reboot").await.unwrap_err();
        assert!(err.contains("not an Android package"), "{err}");
        let err = android.launch("com.x").await.unwrap_err();
        assert!(err.contains("run adb"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_component_reaches_the_device_shell_quoted() {
        let dir = tempfile::tempdir().unwrap();
        let adb = dir.path().join("adb");
        let argv = dir.path().join("argv");
        script(
            &adb,
            &format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n", argv.display()),
        );
        let android = android_at(adb);
        android.launch("com.example/.Main$Launcher").await.unwrap();
        let args = std::fs::read_to_string(&argv).unwrap();
        assert_eq!(args.lines().last(), Some("'com.example/.Main$Launcher'"));
    }

    #[test]
    fn the_abort_cause_is_the_last_one_of_a_ui_server_process() {
        assert_eq!(
            abort_cause(CRASH_LOG).as_deref(),
            Some("No pending exception expected: java.lang.ClassNotFoundException: shuttle.Ui")
        );
        assert_eq!(abort_cause(""), None);
        let long = format!(
            "F DEBUG   : Cmdline: app_process / shuttle.Ui\nF DEBUG   : Abort message: '{}'\n",
            "é".repeat(400)
        );
        let cause = abort_cause(&long).unwrap();
        assert!(cause.len() <= UI_ABORT_KEEP && cause.chars().all(|c| c == 'é'));
    }

    #[tokio::test]
    async fn a_stderr_tail_keeps_only_its_last_bytes() {
        let text: String = (0..600).map(|i| format!("noise {i}\n")).collect();
        let kept = tail(text.as_bytes(), 100).await;
        assert_eq!(kept.len(), 100);
        assert!(text.as_bytes().ends_with(&kept));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_ui_server_that_aborts_before_ready_reports_its_exit_stderr_and_cause() {
        let err = failing_ui_server(CRASH_LOG, "Aborted (core dumped)", 134).await;
        assert_eq!(
            err,
            "UI server said \"\" and exit status: 134: Aborted (core dumped)\nlast shuttle.Ui abort in the crash log (may be from an earlier start): No pending exception expected: java.lang.ClassNotFoundException: shuttle.Ui"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_start_with_no_abort_in_the_log_points_to_logcat() {
        let err = failing_ui_server("", "Aborted (core dumped)", 134).await;
        assert!(err.ends_with("\nsee adb logcat"), "{err}");
        assert!(err.contains("exit status: 134"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_abort_is_labelled_when_this_start_failed_another_way() {
        let err = failing_ui_server(CRASH_LOG, "adb: device offline", 1).await;
        assert_eq!(
            err,
            "UI server said \"\" and exit status: 1: adb: device offline\nlast shuttle.Ui abort in the crash log (may be from an earlier start): No pending exception expected: java.lang.ClassNotFoundException: shuttle.Ui"
        );
    }

    #[test]
    fn an_empty_abort_message_is_no_cause() {
        let log = "F DEBUG   : Cmdline: app_process / shuttle.Ui\nF DEBUG   : Abort message: ''\n";
        assert_eq!(abort_cause(log), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_screenshot_is_the_ui_servers_length_prefixed_png() {
        let fake = Fake::new();
        fake.windows("");
        fake.put("shot.frame", "png 11\nx\n<<end>>\ny<<end>>\n");
        assert_eq!(fake.android.screenshot().await.unwrap(), b"x\n<<end>>\ny");
        assert!(fake.android.windows().await.unwrap().contains("\"id\":7"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_server_that_fails_to_shoot_falls_back_for_that_call_only() {
        let fake = Fake::new();
        fake.put(
            "shot.frame",
            "<error>java.io.IOException: no screenshot</error>\n<<end>>\n",
        );
        assert_eq!(fake.android.screenshot().await.unwrap(), b"png");
        assert_eq!(fake.android.screenshot().await.unwrap(), b"png");
        let requests = std::fs::read_to_string(fake.dir.path().join("requests")).unwrap();
        assert_eq!(requests.matches("shot").count(), 2, "{requests}");
        assert_eq!(log(&fake, "starts").lines().count(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_uncropped_shot_passes_the_png_through_and_a_cropped_one_is_cut() {
        let fake = Fake::new();
        let mut png = Vec::new();
        let mut enc = png::Encoder::new(&mut png, 3, 2);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let pixels: Vec<u8> = (0..24).collect();
        enc.write_header()
            .unwrap()
            .write_image_data(&pixels)
            .unwrap();
        std::fs::write(fake.dir.path().join("screen.png"), &png).unwrap();
        let whole = crate::desk::run(&fake.android, "desk_shot", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(B64.decode(whole.image_b64.unwrap()).unwrap(), png);
        assert_eq!(
            serde_json::from_str::<Value>(&whole.text).unwrap()["width"],
            3
        );
        let part = crate::desk::run(
            &fake.android,
            "desk_shot",
            serde_json::json!({"x": 1, "y": 1, "w": 1, "h": 1}),
        )
        .await
        .unwrap();
        assert_ne!(B64.decode(part.image_b64.unwrap()).unwrap(), png);
        assert_eq!(
            serde_json::from_str::<Value>(&part.text).unwrap()["height"],
            1
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn at_names_the_process_of_the_window_it_hits() {
        let fake = Fake::new();
        fake.windows("");
        list(&fake, "dump.xml", "[0,600][720,700]");
        let run = |verb: &'static str, args: Value| crate::desk::run(&fake.android, verb, args);
        let windows: Value = serde_json::from_str(
            &run("desk_windows", serde_json::json!({}))
                .await
                .unwrap()
                .text,
        )
        .unwrap();
        let pid = windows[0]["pid"].as_u64();
        let hit: Value = serde_json::from_str(
            &run("desk_at", serde_json::json!({"x": 360, "y": 650}))
                .await
                .unwrap()
                .text,
        )
        .unwrap();
        assert_eq!(hit["found"]["name"], "Target", "{hit}");
        assert!(
            pid.is_some() && hit["pid"].as_u64() == pid,
            "{hit} {windows}"
        );
        assert_eq!(hit["path"][0]["id"], windows[0]["id"], "{hit} {windows}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_element_that_shows_itself_is_tapped_without_a_swipe() {
        let fake = Fake::new();
        fake.windows("");
        fake.put("show.answer", "true");
        list(&fake, "dump.xml", "[0,1300][720,1400]");
        list(&fake, "after-show.xml", "[0,600][720,700]");
        let pressed = press_target(&fake).await;
        assert_eq!(
            (pressed["x"].as_i64(), pressed["y"].as_i64()),
            (Some(360), Some(650))
        );
        assert_eq!(log(&fake, "adbinput"), "shell input tap 360 650\n");
        assert!(log(&fake, "requests").contains("show 7 0.0.1\n"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_element_that_says_true_and_does_not_move_is_swiped_for() {
        let fake = Fake::new();
        fake.windows("");
        fake.put("show.answer", "true");
        list(&fake, "dump.xml", "[0,1300][720,1400]");
        list(&fake, "after-swipe.xml", "[0,600][720,700]");
        let pressed = press_target(&fake).await;
        assert_eq!(pressed["y"].as_i64(), Some(650));
        let input = log(&fake, "adbinput");
        assert_eq!(input.lines().count(), 2, "{input}");
        assert!(input.starts_with("shell input swipe "), "{input}");
        assert_eq!(log(&fake, "requests").matches("show ").count(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_element_that_moves_and_stays_off_the_screen_is_asked_once_then_swiped_for() {
        let fake = Fake::new();
        fake.windows("");
        fake.put("show.answer", "true");
        list(&fake, "dump.xml", "[0,1300][720,1400]");
        list(&fake, "after-show.xml", "[0,1250][720,1350]");
        list(&fake, "after-swipe.xml", "[0,600][720,700]");
        assert_eq!(press_target(&fake).await["y"].as_i64(), Some(650));
        assert_eq!(log(&fake, "requests").matches("show ").count(), 1);
        assert_eq!(log(&fake, "adbinput").matches("swipe").count(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_refusing_element_is_swiped_for() {
        let fake = Fake::new();
        fake.windows("");
        fake.put("show.answer", "false");
        list(&fake, "dump.xml", "[0,1300][720,1400]");
        list(&fake, "after-swipe.xml", "[0,600][720,700]");
        assert_eq!(press_target(&fake).await["y"].as_i64(), Some(650));
        assert!(log(&fake, "adbinput").starts_with("shell input swipe "));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_press_whose_scroll_leaves_no_window_in_front_says_it_scrolled_and_sends_no_tap() {
        let fake = Fake::new();
        fake.put("inject.answer", "true");
        fake.put("show.answer", "false");
        fake.windows("unsettled");
        fake.put("after-swipe.json", &log(&fake, "windows.json"));
        fake.windows("");
        list(&fake, "dump.xml", "[0,1300][720,1400]");
        list(&fake, "after-swipe.xml", "[0,600][720,700]");
        let err = desk(
            &fake,
            "desk_press",
            serde_json::json!({"id": "com.android.settings:0.0.1:button:Target"}),
        )
        .await
        .unwrap_err();
        assert!(
            err.starts_with("the list of element com.android.settings:0.0.1:button:Target was scrolled, then this stopped the tap: the device lists no window in front"),
            "{err}"
        );
        assert_eq!(log(&fake, "adbinput").matches("swipe").count(), 1);
        assert_eq!(log(&fake, "requests").matches("tap").count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_press_whose_read_after_the_element_showed_itself_fails_says_so_and_sends_no_tap() {
        let id = "com.android.settings:0.0.1:button:Target";
        for after in ["unsettled", "empty"] {
            let fake = Fake::new();
            fake.put("inject.answer", "true");
            fake.put("show.answer", "true");
            match after {
                "unsettled" => {
                    fake.windows("unsettled");
                    fake.put("after-show.json", &log(&fake, "windows.json"));
                    list(&fake, "after-show.xml", "[0,600][720,700]");
                }
                _ => fake.put("after-show.xml", "<hierarchy></hierarchy>"),
            }
            fake.windows("");
            list(&fake, "dump.xml", "[0,1300][720,1400]");
            let err = desk(&fake, "desk_press", serde_json::json!({"id": id}))
                .await
                .unwrap_err();
            assert!(
                err.starts_with(&format!(
                    "the list of element {id} was asked to scroll it into view, then this stopped the tap: "
                )),
                "{after}: {err}"
            );
            assert_eq!(log(&fake, "requests").matches("show ").count(), 1);
            assert_eq!(log(&fake, "requests").matches("tap").count(), 0);
            assert_eq!(log(&fake, "adbinput"), "");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_press_whose_read_after_its_scroll_fails_says_it_scrolled_and_sends_no_tap() {
        let id = "com.android.settings:0.0.1:button:Target";
        for after in ["empty", "gone"] {
            let fake = Fake::new();
            fake.put("inject.answer", "true");
            fake.put("show.answer", "false");
            fake.windows("");
            list(&fake, "dump.xml", "[0,1300][720,1400]");
            match after {
                "empty" => fake.put("after-swipe.xml", "<hierarchy></hierarchy>"),
                _ => {
                    list(&fake, "after-swipe.xml", "[0,600][720,700]");
                    let other = log(&fake, "after-swipe.xml").replace("Target", "Other");
                    fake.put("after-swipe.xml", &other);
                }
            }
            let err = desk(&fake, "desk_press", serde_json::json!({"id": id}))
                .await
                .unwrap_err();
            assert!(
                err.starts_with(&format!(
                    "the list of element {id} was scrolled, then this stopped the tap: "
                )),
                "{after}: {err}"
            );
            assert_eq!(log(&fake, "adbinput").matches("swipe").count(), 1);
            assert_eq!(log(&fake, "requests").matches("tap").count(), 0);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_press_or_act_while_no_window_is_in_front_is_refused_before_any_tap() {
        let fake = Fake::new();
        fake.put("inject.answer", "true");
        fake.windows("unsettled");
        list(&fake, "dump.xml", "[0,200][720,300]");
        let id = "com.android.settings:0.0.1:button:Target";
        for (verb, args) in [
            ("desk_press", serde_json::json!({"id": id})),
            ("desk_act", serde_json::json!({"id": id, "action": "click"})),
        ] {
            let err = desk(&fake, verb, args).await.unwrap_err();
            assert!(
                err.starts_with("the device lists no window in front"),
                "{verb} {err}"
            );
        }
        assert_eq!(log(&fake, "requests").matches("tap").count(), 0);
        assert_eq!(log(&fake, "adbinput"), "");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_tap_and_a_single_key_are_injected_by_the_ui_server() {
        let fake = Fake::new();
        fake.put("inject.answer", "true");
        fake.android.tap((10, 20)).await.unwrap();
        fake.android.keys("back").await.unwrap();
        assert_eq!(log(&fake, "requests"), "tap 10 20\nkey 4\n");
        assert_eq!(log(&fake, "adbinput"), "");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_combination_and_a_swipe_stay_on_adb_input() {
        let fake = Fake::new();
        fake.put("inject.answer", "true");
        fake.android.keys("ctrl+a").await.unwrap();
        fake.android
            .input(&["swipe", "1", "2", "3", "4", "300"])
            .await
            .unwrap();
        assert_eq!(log(&fake, "requests"), "");
        let adb = log(&fake, "adbinput");
        assert!(adb.contains("shell input keycombination 113 29"), "{adb}");
        assert!(adb.contains("shell input swipe 1 2 3 4 300"), "{adb}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_tap_the_server_may_have_delivered_is_never_repeated() {
        for answer in [
            "false",
            "<error>java.lang.SecurityException</error>",
            "exit",
        ] {
            let fake = Fake::new();
            fake.put("inject.answer", answer);
            let err = fake.android.tap((10, 20)).await.unwrap_err();
            assert!(err.contains("tap 10,20 is unconfirmed"), "{answer}: {err}");
            let err = fake.android.keys("enter").await.unwrap_err();
            assert!(err.contains("key enter is unconfirmed"), "{answer}: {err}");
            assert_eq!(log(&fake, "adbinput"), "", "{answer}");
            let requests = log(&fake, "requests");
            assert_eq!(requests.matches("tap").count(), 1, "{answer}: {requests}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_server_that_cannot_inject_or_predates_tap_leaves_it_to_adb_for_good() {
        for answer in ["unavailable", ""] {
            let fake = Fake::new();
            fake.put("inject.answer", answer);
            fake.android.tap((10, 20)).await.unwrap();
            fake.android.keys("home").await.unwrap();
            fake.android.tap((30, 40)).await.unwrap();
            assert_eq!(log(&fake, "requests"), "tap 10 20\n", "{answer}");
            let adb = log(&fake, "adbinput");
            assert_eq!(adb.lines().count(), 3, "{answer}: {adb}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_ui_server_that_does_not_start_leaves_a_tap_to_adb() {
        let dir = tempfile::tempdir().unwrap();
        let adb = dir.path().join("adb");
        script(
            &adb,
            "#!/bin/sh\nDIR=$(dirname \"$0\")\ncase \"$*\" in\n*app_process*) exit 1;;\n*\"shell input\"*) echo \"$*\" >> \"$DIR/adbinput\";;\nesac\n",
        );
        let android = android_at(adb);
        android.tap((10, 20)).await.unwrap();
        let sent = std::fs::read_to_string(dir.path().join("adbinput")).unwrap();
        assert_eq!(sent, "shell input tap 10 20\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_indexed_point_is_refused_once_the_screen_it_came_from_changed() {
        let fake = Fake::new();
        fake.windows("");
        fake.android.saved().insert(
            "s".into(),
            Saved {
                window: "7".into(),
                screen: (720, 1280),
                points: vec![(360, 760)],
            },
        );
        let act = tap_element("s", 0);
        assert_eq!(fake.android.point(&act).await, Ok(Some((360, 760))));
        fake.windows("keyboard");
        assert_eq!(
            fake.android.point(&act).await,
            Err("element [0] is covered now; call computer_state again".to_owned())
        );
        fake.windows("shade");
        assert_eq!(
            fake.android.point(&act).await,
            Err("the screen changed since computer_state; call it again".to_owned())
        );
        fake.windows("");
        assert_eq!(fake.android.point(&act).await, Ok(Some((360, 760))));
        fake.windows("landscape");
        assert_eq!(
            fake.android.point(&act).await,
            Err("the screen changed since computer_state; call it again".to_owned())
        );
        let explicit: ActArgs = serde_json::from_value(
            serde_json::json!({"session": "s", "action": "tap", "x": 360.0, "y": 760.0}),
        )
        .unwrap();
        fake.windows("shade");
        assert_eq!(fake.android.point(&explicit).await, Ok(Some((360, 760))));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_indexed_tap_while_no_window_is_in_front_is_refused_before_any_tap() {
        let fake = Fake::new();
        fake.put("inject.answer", "true");
        fake.windows("unsettled");
        fake.android.saved().insert(
            "s".into(),
            Saved {
                window: "7".into(),
                screen: (720, 1280),
                points: vec![(360, 760)],
            },
        );
        for action in ["tap", "type"] {
            let err = fake
                .android
                .act(serde_json::json!({"session": "s", "action": action, "element": 0, "text": "a"}))
                .await
                .unwrap_err();
            assert!(
                err.starts_with("the device lists no window in front"),
                "{action} {err}"
            );
        }
        assert_eq!(log(&fake, "requests").matches("tap").count(), 0);
        assert_eq!(log(&fake, "requests").matches("paste").count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_indexed_point_is_refused_when_either_window_id_is_unknown() {
        let fake = Fake::new();
        let unavailable = "the window list is unavailable; use x,y or call computer_state again";
        fake.windows("none");
        fake.android.saved().insert(
            "known".into(),
            Saved {
                window: "7".into(),
                screen: (720, 1280),
                points: vec![(360, 760)],
            },
        );
        fake.android.saved().insert(
            "unknown".into(),
            Saved {
                window: String::new(),
                screen: (720, 1280),
                points: vec![(360, 760)],
            },
        );
        assert_eq!(
            fake.android.point(&tap_element("known", 0)).await,
            Err(unavailable.to_owned()),
            "no window is listed now"
        );
        fake.windows("");
        assert_eq!(
            fake.android.point(&tap_element("unknown", 0)).await,
            Err(unavailable.to_owned()),
            "no window was listed at the state"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn computer_state_retries_a_tree_of_another_window_once_then_refuses() {
        let fake = Fake::new();
        fake.windows("");
        fake.dump("dump.xml", "7");
        fake.dump("dump-first.xml", "9");
        let out = fake.android.state(serde_json::json!({})).await.unwrap();
        assert_eq!(out.text, "[0] button \"OK\" at 200,250");
        assert_eq!(
            fake.android.saved().get("").map(|s| s.window.as_str()),
            Some("7")
        );
        fake.dump("dump.xml", "9");
        fake.dump("dump-first.xml", "9");
        let err = fake.android.state(serde_json::json!({})).await.unwrap_err();
        assert_eq!(err, "the screen is changing; call computer_state again");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_device_on_its_side_has_the_turned_screen() {
        let fake = Fake::new();
        fake.windows("landscape");
        fake.put(
            "dump.xml",
            r#"<hierarchy window="7"><node text="Far" class="android.widget.TextView" content-desc="" hint="" showing-hint="false" checkable="false" checked="false" clickable="true" long-clickable="false" editable="false" password="false" enabled="true" focused="false" selected="false" scrollable="false" visible="true" path="0.0" bounds="[900,200][1100,300]" /></hierarchy>"#,
        );
        let mut png = Vec::new();
        let mut enc = png::Encoder::new(&mut png, 1280, 720);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&vec![0; 1280 * 720])
            .unwrap();
        std::fs::write(fake.dir.path().join("screen.png"), &png).unwrap();
        let screen = desk(&fake, "desk_screen", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(
            (screen["width"].as_i64(), screen["height"].as_i64()),
            (Some(1280), Some(720))
        );
        let state = fake.android.state(serde_json::json!({})).await.unwrap();
        assert_eq!(state.text, "[0] button \"Far\" at 1000,250");
        let shot = desk(&fake, "desk_shot", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(
            (shot["width"].as_i64(), shot["height"].as_i64()),
            (Some(1280), Some(720))
        );
        assert_eq!(fake.android.look().await.unwrap().text, "1280x720 screen");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_click_the_device_cannot_make_is_refused_before_any_tap() {
        let fake = Fake::new();
        fake.put("inject.answer", "true");
        fake.windows("shade");
        for (args, want) in [
            (
                serde_json::json!({"x": 10, "y": 20, "button": "right"}),
                "a tap on a device has no right button",
            ),
            (
                serde_json::json!({"x": 10, "y": 20, "modifiers": ["shift"]}),
                "a tap on a device takes no modifier keys",
            ),
            (
                serde_json::json!({"x": 10, "y": 20, "window": "Settings"}),
                "Settings - com.android.settings is not in front",
            ),
        ] {
            let err = desk(&fake, "desk_click", args).await.unwrap_err();
            assert!(err.starts_with(want), "{err}");
        }
        for (verb, args) in [
            (
                "desk_key",
                serde_json::json!({"keys": "a", "window": "Settings"}),
            ),
            (
                "desk_type",
                serde_json::json!({"text": "hi", "window": "Settings"}),
            ),
        ] {
            let err = desk(&fake, verb, args).await.unwrap_err();
            assert!(err.contains("not in front"), "{verb}: {err}");
        }
        assert_eq!(log(&fake, "requests").matches("tap ").count(), 0);
        assert_eq!(log(&fake, "requests").matches("key ").count(), 0);
        assert_eq!(log(&fake, "adbinput"), "");
        fake.windows("");
        desk(
            &fake,
            "desk_key",
            serde_json::json!({"keys": "a", "window": "Settings"}),
        )
        .await
        .unwrap();
        desk(
            &fake,
            "desk_click",
            serde_json::json!({"x": 10, "y": 20, "button": "left"}),
        )
        .await
        .unwrap();
        assert!(log(&fake, "requests").ends_with("key 29\ntap 10 20\n"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_repeated_key_stops_when_the_named_window_leaves_the_front() {
        let fake = Fake::new();
        fake.put("inject.answer", "true");
        fake.windows("shade");
        fake.put("after-input.json", &log(&fake, "windows.json"));
        fake.windows("");
        let err = desk(
            &fake,
            "desk_key",
            serde_json::json!({"keys": "back", "window": "Settings", "repeat": 3}),
        )
        .await
        .unwrap_err();
        assert!(
            err.starts_with(
                "1 of 3 presses were sent, then this stopped the rest: Settings - com.android.settings is not in front"
            ),
            "{err}"
        );
        assert_eq!(log(&fake, "requests"), "windows\nkey 4\nwindows\n");
        assert_eq!(log(&fake, "adbinput"), "");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_close_with_the_keyboard_up_hides_it_then_closes_the_window() {
        let close = || serde_json::json!({"window": "Settings"});
        let keyboard_then = |after: &str| {
            let fake = Fake::new();
            fake.put("inject.answer", "true");
            fake.put("after-input.json", after);
            fake.windows("keyboard");
            fake
        };
        let layout = |name: &str| {
            let fake = Fake::new();
            fake.windows(name);
            log(&fake, "windows.json")
        };
        let fake = keyboard_then(&layout(""));
        let out = desk(&fake, "desk_close", close()).await.unwrap();
        assert_eq!(out["title"], "Settings - com.android.settings");
        assert_eq!(log(&fake, "requests").matches("key 4").count(), 2);
        let fake = keyboard_then(&documents(false));
        let out = desk(&fake, "desk_close", close()).await.unwrap();
        assert_eq!(out["title"], "Settings - com.android.settings");
        assert_eq!(log(&fake, "requests").matches("key 4").count(), 1);
        let fake = keyboard_then(&layout("shade"));
        let err = desk(&fake, "desk_close", close()).await.unwrap_err();
        assert_eq!(
            err,
            "Settings - com.android.settings left the front after a back key to hide the keyboard, so no closing back was sent"
        );
        assert_eq!(log(&fake, "requests").matches("key 4").count(), 1);
        let fake = keyboard_then(&layout("keyboard"));
        let err = desk(&fake, "desk_close", close()).await.unwrap_err();
        assert_eq!(
            err,
            "the keyboard stayed up after a back key, so Settings - com.android.settings was not closed"
        );
        assert_eq!(log(&fake, "requests").matches("key 4").count(), 1);
        let fake = Fake::new();
        fake.put("inject.answer", "true");
        fake.windows("");
        desk(&fake, "desk_close", close()).await.unwrap();
        assert_eq!(log(&fake, "requests"), "windows\nkey 4\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_close_that_fails_after_the_back_that_hid_the_keyboard_says_it_was_sent() {
        let close = || serde_json::json!({"window": "Settings"});
        let fake = Fake::new();
        fake.put("inject.answer", "true");
        fake.put("after-input.json", "not json");
        fake.windows("keyboard");
        let err = desk(&fake, "desk_close", close()).await.unwrap_err();
        assert!(err.starts_with("decode windows: "), "{err}");
        assert!(
            err.ends_with(", after a back key to hide the keyboard, so no closing back was sent"),
            "{err}"
        );
        assert_eq!(log(&fake, "requests").matches("key 4").count(), 1);
        let fake = Fake::new();
        fake.put("inject.answer", "true\nfalse");
        fake.windows("");
        fake.put("after-input.json", &log(&fake, "windows.json"));
        fake.windows("keyboard");
        let err = desk(&fake, "desk_close", close()).await.unwrap_err();
        assert_eq!(
            err,
            "key back is unconfirmed (the injection was not confirmed) and was not repeated, since it may have reached the device; a back key had hidden the keyboard before it"
        );
        assert_eq!(log(&fake, "requests").matches("key 4").count(), 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_named_close_or_focus_while_no_window_is_in_front_is_refused_before_any_key() {
        let fake = Fake::new();
        fake.put("inject.answer", "true");
        fake.windows("unsettled");
        for verb in ["desk_close", "desk_focus"] {
            for args in [
                serde_json::json!({"window": "Settings"}),
                serde_json::json!({"window": "Settings", "id": "7"}),
            ] {
                let err = desk(&fake, verb, args).await.unwrap_err();
                assert!(
                    err.starts_with("the device lists no window in front"),
                    "{verb} {err}"
                );
            }
        }
        assert_eq!(log(&fake, "requests").matches("key").count(), 0);
        assert_eq!(log(&fake, "adbinput"), "");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_close_sends_no_closing_back_while_the_list_after_the_keyboard_back_has_no_front() {
        let fake = Fake::new();
        fake.put("inject.answer", "true");
        fake.windows("unsettled");
        fake.put("after-input.json", &log(&fake, "windows.json"));
        fake.windows("keyboard");
        let err = desk(
            &fake,
            "desk_close",
            serde_json::json!({"window": "Settings"}),
        )
        .await
        .unwrap_err();
        assert!(err.contains("was not closed"), "{err}");
        assert_eq!(log(&fake, "requests").matches("key 4").count(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_action_that_ran_is_reported_even_when_the_screenshot_after_it_fails() {
        let fake = Fake::new();
        fake.put("inject.answer", "true");
        fake.put("screencap.fail", "");
        let out = fake
            .android
            .act(serde_json::json!({"action": "tap", "x": 10, "y": 20}))
            .await
            .unwrap();
        assert!(
            out.text.starts_with("tapped 10,20; no screenshot"),
            "{}",
            out.text
        );
        assert!(out.image_b64.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_type_that_fails_midway_says_how_many_lines_went_in() {
        let fake = Fake::new();
        fake.put("inject.answer", "true\nfalse");
        let err = desk(&fake, "desk_type", serde_json::json!({"text": "\n\n"}))
            .await
            .unwrap_err();
        assert!(err.contains("key return is unconfirmed"), "{err}");
        assert!(
            err.ends_with("; 2 of 3 lines were typed before it"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_crlf_text_types_its_lines_without_the_carriage_returns() {
        let fake = Fake::new();
        fake.put("inject.answer", "true");
        fake.windows("");
        fake.put("focus.json", &focused(7, ""));
        let pasted = acked(&fake).await;
        let typed = desk(&fake, "desk_type", serde_json::json!({"text": "\r\n\r\n"})).await;
        let requests = log(&fake, "requests");
        assert!(typed.is_ok(), "{typed:?} {requests}");
        assert_eq!(requests.matches("key ").count(), 2, "{requests}");
        assert_eq!(requests.matches("paste").count(), 0, "{requests}");
        fake.put("after-paste.json", &focused(7, "a"));
        let _ = desk(&fake, "desk_type", serde_json::json!({"text": "a\r\nb"})).await;
        let pasted = pasted.lock().unwrap();
        assert_eq!(pasted.iter().find(|t| !t.is_empty()), Some(&"a".to_owned()));
        assert!(pasted.iter().all(|t| !t.contains('\r')), "{pasted:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_multi_line_type_stops_when_the_named_window_leaves_the_front() {
        let fake = Fake::new();
        fake.put("inject.answer", "true");
        fake.windows("shade");
        fake.put("after-input.json", &log(&fake, "windows.json"));
        fake.windows("");
        let err = desk(
            &fake,
            "desk_type",
            serde_json::json!({"text": "\nsecret", "window": "Settings"}),
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("Settings - com.android.settings is not in front"),
            "{err}"
        );
        assert!(
            err.ends_with("; 1 of 2 lines and the Return after line 1 were typed before it"),
            "{err}"
        );
        let requests = log(&fake, "requests");
        assert_eq!(requests.matches("key ").count(), 1);
        assert_eq!(requests.matches("focus").count(), 0, "{requests}");
        assert_eq!(requests.matches("paste").count(), 0, "{requests}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_line_after_a_return_goes_only_into_a_field_of_the_window_checked_in_front() {
        let other = r#"{"editable":true,"password":false,"window":true,"id":9,"len":0,"hash":0,"tail":"","bounds":[0,0,720,100]}"#;
        let stale = r#"{"editable":false,"password":false,"window":true,"id":7,"len":0,"hash":0,"tail":"","bounds":[0,0,720,100]}"#;
        let parent = r#"{"editable":false,"password":false,"window":true,"id":9,"len":0,"hash":0,"tail":"","bounds":[0,0,720,100]}"#;
        for (focus, want) in [
            ("null", "no field of the window holds the input focus"),
            (other, "the input focus is in another window"),
            (stale, "no field of the window holds the input focus"),
            (parent, "the input focus is in another window"),
        ] {
            let fake = Fake::new();
            fake.put("inject.answer", "true");
            fake.windows("");
            fake.put("focus.json", focus);
            let err = desk(
                &fake,
                "desk_type",
                serde_json::json!({"text": "\nsecret", "window": "Settings"}),
            )
            .await
            .unwrap_err();
            assert!(err.starts_with(want), "{err}");
            assert!(
                err.ends_with("; 1 of 2 lines and the Return after line 1 were typed before it"),
                "{err}"
            );
            let requests = log(&fake, "requests");
            assert_eq!(requests.matches("key ").count(), 1, "{requests}");
            assert_eq!(requests.matches("paste").count(), 0, "{requests}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_line_after_a_return_reads_the_tree_once_when_the_focus_names_no_field() {
        let view = r#"{"editable":false,"password":false,"window":true,"id":7,"len":0,"hash":0,"tail":"","bounds":[0,160,720,658]}"#;
        let field = focused(7, "");
        let shown = focused(7, "pass");
        for (after_return, after_dump, after_paste, typed, dumps) in [
            (view, Some(&field), &shown, Some(Value::Bool(true)), 1),
            (view, None, &shown, None, 1),
            (&field, None, &view.to_owned(), Some(Value::Null), 0),
        ] {
            let fake = Fake::new();
            fake.put("inject.answer", "true");
            fake.windows("");
            fake.put("focus.json", &field);
            fake.put("after-key-focus.json", after_return);
            if let Some(focus) = after_dump {
                fake.put("after-dump-focus.json", focus);
            }
            fake.put("after-paste.json", after_paste);
            acked(&fake).await;
            let out = desk(&fake, "desk_type", serde_json::json!({"text": "\npass"})).await;
            let requests = log(&fake, "requests");
            match typed {
                Some(typed) => assert_eq!(
                    out,
                    Ok(serde_json::json!({"ok": true, "chars": 5, "typed": typed})),
                    "{requests}"
                ),
                None => assert!(
                    out.as_ref().is_err_and(|e| e.starts_with(NO_FIELD)),
                    "{out:?}"
                ),
            }
            assert_eq!(requests.matches("dump 7").count(), dumps, "{requests}");
            assert_eq!(
                requests.matches("paste").count(),
                usize::from(out.is_ok()),
                "{requests}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_type_says_protected_when_a_line_went_into_a_password_field() {
        let secret = r#"{"editable":true,"password":true,"window":true,"id":7,"len":0,"hash":0,"tail":"","bounds":[0,0,720,100]}"#;
        let pasted = r#"{"editable":true,"password":true,"window":true,"id":7,"len":2,"hash":0,"tail":"","bounds":[0,0,720,100]}"#;
        for (focus, after, want) in [
            (
                secret.to_owned(),
                pasted.to_owned(),
                serde_json::json!({"ok": true, "chars": 2, "typed": null, "protected": true}),
            ),
            (
                focused(7, ""),
                focused(7, "pw"),
                serde_json::json!({"ok": true, "chars": 2, "typed": true}),
            ),
        ] {
            let fake = Fake::new();
            fake.put("inject.answer", "true");
            fake.windows("");
            fake.put("focus.json", &focus);
            fake.put("after-paste.json", &after);
            acked(&fake).await;
            let out = desk(&fake, "desk_type", serde_json::json!({"text": "pw"})).await;
            assert_eq!(out, Ok(want), "{}", log(&fake, "requests"));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_type_that_fails_after_any_focus_read_named_a_password_field_ends_by_saying_so() {
        use desk_core::wire::INTO_PASSWORD_FIELD;

        let secret = r#"{"editable":true,"password":true,"window":true,"id":7,"len":0,"hash":0,"tail":"","bounds":[0,0,720,100]}"#;
        for (text, focus, live_fails, clause) in [
            ("pw", secret.to_owned(), true, true),
            ("\npw", secret.to_owned(), false, true),
            ("pw", focused(7, ""), true, false),
            ("\npw", focused(7, ""), false, false),
        ] {
            let fake = Fake::new();
            fake.put("inject.answer", "false");
            fake.windows("");
            fake.put("focus.json", &focus);
            if live_fails {
                fake.put("live.fail", "");
            }
            let err = fake
                .android
                .type_text(text, None, || async { Ok(None) })
                .await
                .unwrap_err();
            assert_eq!(
                err.ends_with(INTO_PASSWORD_FIELD),
                clause,
                "{text:?} {err} {}",
                log(&fake, "requests")
            );
            assert_eq!(
                err.matches(INTO_PASSWORD_FIELD).count(),
                usize::from(clause)
            );
            assert!(!log(&fake, "requests").contains("paste"), "{text:?}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_type_whose_paste_fails_after_its_focus_read_found_a_password_field_ends_by_saying_so()
     {
        use desk_core::wire::INTO_PASSWORD_FIELD;

        let secret = r#"{"editable":true,"password":true,"window":true,"id":7,"len":0,"hash":0,"tail":"","bounds":[0,0,720,100]}"#;
        let failed = "read scrcpy control: unexpected end of file";
        for (text, focus, want) in [
            (
                "pw",
                secret.to_owned(),
                format!("{failed}{INTO_PASSWORD_FIELD}"),
            ),
            (
                "\npw",
                secret.to_owned(),
                format!(
                    "{failed}; 1 of 2 lines and the Return after line 1 were typed before it{INTO_PASSWORD_FIELD}"
                ),
            ),
            ("pw", focused(7, ""), failed.to_owned()),
        ] {
            let fake = Fake::new();
            fake.put("inject.answer", "true");
            fake.windows("");
            fake.put("focus.json", &focus);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            fake.put("port", &listener.local_addr().unwrap().port().to_string());
            let mut peer = controlled(&fake.android).await;
            let device = tokio::spawn(async move {
                let mut set = vec![0; clipboard_message("pw", 1).len()];
                peer.read_exact(&mut set).await.unwrap();
                drop(peer);
                let (mut fresh, _) = listener.accept().await.unwrap();
                fresh.write_all(&[0]).await.unwrap();
                let mut clear = vec![0; clipboard_message("", 1).len()];
                fresh.read_exact(&mut clear).await.unwrap();
                fresh.write_all(&ack(1)).await.unwrap();
            });
            let typed = fake
                .android
                .type_text(text, None, || async { Ok(None) })
                .await;
            assert_eq!(typed, Err(want), "{text:?}");
            tokio::time::timeout(Duration::from_secs(5), device)
                .await
                .expect("no clear through a new session")
                .unwrap();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unnamed_type_pastes_after_a_return_only_into_the_window_its_first_line_went_into() {
        for (after_return, want) in [(focused(9, ""), false), (focused(7, "a"), true)] {
            let fake = Fake::new();
            fake.put("inject.answer", "true");
            fake.windows("");
            fake.put("focus.json", &focused(7, ""));
            fake.put("after-paste.json", &focused(7, "a"));
            fake.put("after-key-focus.json", &after_return);
            let mut peer = controlled(&fake.android).await;
            tokio::spawn(async move {
                let mut head = [0u8; 14];
                while peer.read_exact(&mut head).await.is_ok() {
                    let len = u32::from_be_bytes([head[10], head[11], head[12], head[13]]);
                    let mut text = vec![0u8; len as usize];
                    peer.read_exact(&mut text).await.unwrap();
                    let mut ack = vec![ACK_CLIPBOARD];
                    ack.extend_from_slice(&head[1..9]);
                    peer.write_all(&ack).await.unwrap();
                }
            });
            let out = desk(&fake, "desk_type", serde_json::json!({"text": "a\nb"})).await;
            let pastes = log(&fake, "requests").matches("paste").count();
            if want {
                assert!(out.is_ok(), "{out:?}");
                assert_eq!(pastes, 2);
            } else {
                let err = out.unwrap_err();
                assert!(
                    err.starts_with("the input focus is in another window"),
                    "{err}"
                );
                assert!(
                    err.ends_with(
                        "; 1 of 2 lines and the Return after line 1 were typed before it"
                    ),
                    "{err}"
                );
                assert_eq!(pastes, 1);
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_named_type_goes_on_only_in_the_window_it_began_in() {
        for closes in [true, false] {
            let fake = Fake::new();
            fake.put("inject.answer", "true");
            fake.put("windows.json", &documents(true));
            if closes {
                fake.put("after-input.json", &documents(false));
                fake.put("after-key-focus.json", &focused(10, ""));
            }
            fake.put("focus.json", &focused(11, ""));
            fake.put("after-paste.json", &focused(11, "a"));
            acked(&fake).await;
            let out = desk(
                &fake,
                "desk_type",
                serde_json::json!({"text": "a\nb", "window": "New folder - com.android.documentsui"}),
            )
            .await;
            let pastes = log(&fake, "requests").matches("paste").count();
            if closes {
                let err = out.unwrap_err();
                assert!(
                    err.starts_with(
                        "the window New folder - com.android.documentsui the call began in is gone"
                    ),
                    "{err}"
                );
                assert!(
                    err.ends_with(
                        "; 1 of 2 lines and the Return after line 1 were typed before it"
                    ),
                    "{err}"
                );
                assert_eq!(pastes, 1);
            } else {
                assert!(out.is_ok(), "{out:?}");
                assert_eq!(pastes, 2);
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_named_repeated_key_stops_when_the_window_it_began_in_is_gone() {
        let fake = Fake::new();
        fake.put("inject.answer", "true");
        fake.put("windows.json", &documents(true));
        fake.put("after-input.json", &documents(false));
        let err = desk(
            &fake,
            "desk_key",
            serde_json::json!({"keys": "back", "window": "New folder - com.android.documentsui", "repeat": 2}),
        )
        .await
        .unwrap_err();
        assert!(
            err.starts_with("1 of 2 presses were sent, then this stopped the rest: the window New folder - com.android.documentsui the call began in is gone"),
            "{err}"
        );
        assert_eq!(log(&fake, "requests"), "windows\nkey 4\nwindows\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unnamed_text_that_starts_with_a_line_break_goes_into_the_field_focused_before_it() {
        for (text, focus, keys, pastes) in [
            ("\nbody", focused(7, ""), 1, 1),
            ("\n\nbody", focused(7, ""), 2, 1),
            ("\nbody", "null".to_owned(), 0, 0),
        ] {
            let fake = Fake::new();
            fake.put("inject.answer", "true\ntrue");
            fake.windows("");
            fake.put("focus.json", &focus);
            fake.put("after-paste.json", &focused(7, "body"));
            acked(&fake).await;
            let out = desk(&fake, "desk_type", serde_json::json!({"text": text})).await;
            let requests = log(&fake, "requests");
            if keys == 0 {
                let err = out.unwrap_err();
                assert_eq!(
                    err,
                    "no field holds the input focus, so nothing was typed of a text that starts with a line break"
                );
            } else {
                assert!(out.is_ok(), "{text:?} {out:?}");
            }
            assert_eq!(
                requests.matches("key ").count(),
                keys,
                "{text:?} {requests}"
            );
            assert_eq!(
                requests.matches("paste").count(),
                pastes,
                "{text:?} {requests}"
            );
            if keys > 0 {
                let first = requests.find("focus live").unwrap_or(usize::MAX);
                assert!(first < requests.find("key ").unwrap_or(0), "{requests}");
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_text_at_a_point_that_starts_with_a_line_break_is_refused_after_its_return() {
        let fake = Fake::new();
        fake.put("inject.answer", "true\ntrue");
        fake.windows("");
        fake.put("focus.json", &focused(7, ""));
        acked(&fake).await;
        let err = fake
            .android
            .act(serde_json::json!({"action": "type", "text": "\nbody", "x": 10, "y": 20}))
            .await
            .unwrap_err();
        assert!(err.starts_with(NO_FIELD), "{err}");
        assert!(
            err.ends_with("; 1 of 2 lines and the Return after line 1 were typed before it"),
            "{err}"
        );
        let requests = log(&fake, "requests");
        assert_eq!(requests.matches("key ").count(), 1, "{requests}");
        assert_eq!(requests.matches("focus").count(), 0, "{requests}");
        assert_eq!(requests.matches("paste").count(), 0, "{requests}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_first_line_into_a_named_window_is_pasted_though_no_field_reports_the_focus() {
        let fake = Fake::new();
        fake.windows("");
        fake.put("focus.json", "null");
        let mut peer = controlled(&fake.android).await;
        tokio::spawn(async move {
            let mut head = [0u8; 14];
            while peer.read_exact(&mut head).await.is_ok() {
                let len = u32::from_be_bytes([head[10], head[11], head[12], head[13]]);
                let mut text = vec![0u8; len as usize];
                peer.read_exact(&mut text).await.unwrap();
                let mut ack = vec![ACK_CLIPBOARD];
                ack.extend_from_slice(&head[1..9]);
                peer.write_all(&ack).await.unwrap();
            }
        });
        let out = desk(
            &fake,
            "desk_type",
            serde_json::json!({"text": "hi", "window": "Settings"}),
        )
        .await
        .unwrap();
        assert_eq!(out["typed"], Value::Null, "{out}");
        assert_eq!(log(&fake, "requests").matches("paste").count(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn focus_refuses_a_listed_window_that_is_not_in_front() {
        let fake = Fake::new();
        fake.windows("shade");
        let err = desk(
            &fake,
            "desk_focus",
            serde_json::json!({"window": "Settings"}),
        )
        .await
        .unwrap_err();
        assert!(
            err.starts_with("Settings - com.android.settings is not in front"),
            "{err}"
        );
        assert_eq!(log(&fake, "requests"), "windows\n");
        assert_eq!(log(&fake, "adbinput"), "");
        fake.windows("");
        let out = desk(
            &fake,
            "desk_focus",
            serde_json::json!({"window": "Settings"}),
        )
        .await
        .unwrap();
        assert_eq!(out["title"], "Settings - com.android.settings");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dump_without_a_node_is_no_answer_and_not_an_empty_window() {
        let fake = Fake::new();
        fake.windows("");
        fake.put("dump.xml", "<hierarchy></hierarchy>");
        let cut = desk(
            &fake,
            "desk_snapshot",
            serde_json::json!({"window": "Settings"}),
        )
        .await
        .unwrap();
        assert_eq!(cut["truncated"], true, "{cut}");
        assert_eq!(cut["nodes"].as_array().map(Vec::len), Some(1));
        let err = desk(
            &fake,
            "desk_find",
            serde_json::json!({"name": "OK", "window": "Settings"}),
        )
        .await
        .unwrap_err();
        assert!(
            err.starts_with("no accessibility answer from window"),
            "{err}"
        );
        fake.dump("dump.xml", "7");
        let out = desk(
            &fake,
            "desk_snapshot",
            serde_json::json!({"window": "Settings"}),
        )
        .await
        .unwrap();
        assert_eq!(out["truncated"], false);
        assert_eq!(out["nodes"].as_array().map(Vec::len), Some(2));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_type_empties_the_device_clipboard_after_it() {
        let (android, mut peer) = session("/nonexistent/adb").await;
        peer.write_all(&ack(1)).await.unwrap();
        let device = tokio::spawn(async move {
            let mut buf = [0; 64];
            let n = peer.read(&mut buf).await.unwrap();
            buf[..n].to_vec()
        });
        let typed = android.type_text("", None, || async { Ok(None) }).await;
        assert_eq!(typed, Ok((Some(true), false)));
        let sent = tokio::time::timeout(Duration::from_secs(5), device).await;
        assert_eq!(sent.unwrap().unwrap(), clipboard_message("", 1));
        assert!(android.control.lock().await.is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_type_dropped_before_its_end_still_empties_the_device_clipboard() {
        let (android, mut peer) = session("/nonexistent/adb").await;
        let typing = android.type_text("secret", None, std::future::pending);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), typing)
                .await
                .is_err()
        );
        let mut buf = [0; 64];
        let n = tokio::time::timeout(Duration::from_secs(5), peer.read(&mut buf))
            .await
            .expect("no clear after the dropped type")
            .unwrap();
        assert_eq!(buf[..n], clipboard_message("", 1));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_type_dropped_while_its_clipboard_waits_for_the_ack_still_empties_it() {
        let fake = Fake::new();
        fake.put("focus.json", &focused(7, ""));
        let mut peer = controlled(&fake.android).await;
        let typing = fake
            .android
            .type_text("secret", None, || async { Ok(None) });
        let mut set = vec![0; clipboard_message("secret", 1).len()];
        tokio::select! {
            _ = typing => panic!("the type ended without an acknowledgement"),
            read = peer.read_exact(&mut set) => {
                read.unwrap();
            }
        }
        assert_eq!(set, clipboard_message("secret", 1));
        peer.write_all(&ack(1)).await.unwrap();
        let mut clear = vec![0; clipboard_message("", 2).len()];
        tokio::time::timeout(Duration::from_secs(5), peer.read_exact(&mut clear))
            .await
            .expect("no clear after the dropped type")
            .unwrap();
        assert_eq!(clear, clipboard_message("", 2));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_clipboard_set_without_an_ack_is_emptied_through_a_new_session() {
        let fake = Fake::new();
        fake.put("focus.json", &focused(7, ""));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        fake.put("port", &listener.local_addr().unwrap().port().to_string());
        let mut peer = controlled(&fake.android).await;
        let device = tokio::spawn(async move {
            let mut set = vec![0; clipboard_message("secret", 1).len()];
            peer.read_exact(&mut set).await.unwrap();
            drop(peer);
            let (mut fresh, _) = listener.accept().await.unwrap();
            fresh.write_all(&[0]).await.unwrap();
            let mut clear = vec![0; clipboard_message("", 1).len()];
            fresh.read_exact(&mut clear).await.unwrap();
            fresh.write_all(&ack(1)).await.unwrap();
            (set, clear)
        });
        let typed = fake
            .android
            .type_text("secret", None, || async { Ok(None) })
            .await;
        assert_eq!(
            typed,
            Err("read scrcpy control: unexpected end of file".to_owned())
        );
        let sent = tokio::time::timeout(Duration::from_secs(5), device)
            .await
            .expect("no clear through a new session")
            .unwrap();
        assert_eq!(
            sent,
            (clipboard_message("secret", 1), clipboard_message("", 1))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_clear_through_a_session_that_ended_after_the_paste_goes_through_a_new_one() {
        let fake = Fake::new();
        fake.put("focus.json", &focused(7, ""));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        fake.put("port", &listener.local_addr().unwrap().port().to_string());
        let mut peer = controlled(&fake.android).await;
        let device = tokio::spawn(async move {
            let mut set = vec![0; clipboard_message("secret", 1).len()];
            peer.read_exact(&mut set).await.unwrap();
            peer.write_all(&ack(1)).await.unwrap();
            drop(peer);
            let (mut fresh, _) = listener.accept().await.unwrap();
            fresh.write_all(&[0]).await.unwrap();
            let mut clear = vec![0; clipboard_message("", 1).len()];
            fresh.read_exact(&mut clear).await.unwrap();
            fresh.write_all(&ack(1)).await.unwrap();
            (set, clear)
        });
        let typed = fake
            .android
            .type_text("secret", None, || async { Ok(None) })
            .await;
        assert!(typed.is_ok(), "{typed:?}");
        let sent = tokio::time::timeout(Duration::from_secs(5), device)
            .await
            .expect("no clear through a new session")
            .unwrap();
        assert_eq!(
            sent,
            (clipboard_message("secret", 1), clipboard_message("", 1))
        );
        assert!(fake.android.control.lock().await.is_some());
        drop(controlled(&fake.android).await);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        fake.put("port", &listener.local_addr().unwrap().port().to_string());
        let typed = fake
            .android
            .type_text("", None, || async { Ok(None) })
            .await;
        assert_eq!(typed, Ok((Some(true), false)));
        let reopened = tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
        assert!(
            reopened.is_err(),
            "a session was opened to clear what no paste set"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_double_tap_is_two_server_taps_inside_the_double_tap_window() {
        let fake = Fake::new();
        fake.put("inject.answer", "true");
        fake.android.tap((1, 2)).await.unwrap();
        let started = std::time::Instant::now();
        desk(
            &fake,
            "desk_click",
            serde_json::json!({"x": 10, "y": 20, "count": 2}),
        )
        .await
        .unwrap();
        assert!(started.elapsed() >= TAP_GAP);
        assert_eq!(log(&fake, "requests"), "tap 1 2\ntap 10 20\ntap 10 20\n");
        let fake = Fake::new();
        fake.put("inject.answer", "unavailable");
        let err = fake
            .android
            .act(serde_json::json!({"action": "double_tap", "x": 10, "y": 20}))
            .await
            .unwrap_err();
        assert!(err.contains("too slowly"), "{err}");
        assert_eq!(log(&fake, "adbinput"), "");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_scroll_amount_is_capped_at_the_drivers_fifty() {
        let fake = Fake::new();
        fake.windows("");
        fake.android
            .act(serde_json::json!({"action": "scroll", "direction": "down", "amount": 1000}))
            .await
            .unwrap();
        assert_eq!(
            log(&fake, "adbinput").matches("swipe").count(),
            MAX_SCROLL as usize
        );
    }

    #[tokio::test]
    async fn a_session_end_forgets_only_that_sessions_elements() {
        let android = android_at("/nonexistent/adb");
        {
            let mut points = android.saved();
            points.insert("cu-1".into(), saved(vec![(10, 20)]));
            points.insert("cu-2".into(), saved(vec![(30, 40)]));
        }
        assert_eq!(
            android
                .end(&serde_json::json!({"session": "cu-1"}))
                .await
                .unwrap(),
            "ended"
        );
        let points = android.saved();
        assert!(!points.contains_key("cu-1"));
        assert_eq!(points.get("cu-2").map(|s| &s.points), Some(&vec![(30, 40)]));
    }

    #[tokio::test]
    async fn a_released_holder_loses_its_default_sessions_elements_and_no_other() {
        let android = android_at("/nonexistent/adb");
        {
            let mut points = android.saved();
            points.insert("run-a".into(), saved(vec![(10, 20)]));
            points.insert("run-b".into(), saved(vec![(30, 40)]));
        }
        android.release("run-a");
        let err = android
            .act(serde_json::json!({"session": "run-a", "action": "tap", "element": 0}))
            .await
            .unwrap_err();
        assert_eq!(err, "no element [0] in the latest computer_state");
        assert_eq!(
            android.saved().get("run-b").map(|s| &s.points),
            Some(&vec![(30, 40)])
        );
    }

    #[tokio::test]
    async fn an_end_without_a_session_forgets_the_default_one_and_ends_are_idempotent() {
        let android = android_at("/nonexistent/adb");
        {
            let mut points = android.saved();
            points.insert(String::new(), saved(vec![(10, 20)]));
            points.insert("cu-2".into(), saved(vec![(30, 40)]));
        }
        let end = |args| {
            let android = &android;
            async move { android.end(&args).await.unwrap() }
        };
        assert_eq!(end(serde_json::json!({})).await, "ended");
        assert!(!android.saved().contains_key(""));
        assert_eq!(
            end(serde_json::json!({"session": "unknown"})).await,
            "ended"
        );
        assert_eq!(end(serde_json::json!({"session": "cu-2"})).await, "ended");
        assert_eq!(end(serde_json::json!({"session": "cu-2"})).await, "ended");
        assert!(android.saved().is_empty());
    }

    #[test]
    fn clipboard_message_is_scrcpy_set_clipboard_without_its_paste_key() {
        let msg = clipboard_message("\u{4f60}\u{597d}", 7);
        assert_eq!(msg[0], 9);
        assert_eq!(&msg[1..9], &7u64.to_be_bytes());
        assert_eq!(msg[9], 0);
        assert_eq!(&msg[10..14], &6u32.to_be_bytes());
        assert_eq!(&msg[14..], "\u{4f60}\u{597d}".as_bytes());
    }

    #[tokio::test]
    async fn the_paste_takes_only_its_own_acknowledgement() {
        assert_eq!(read_ack(&mut ack(7).as_slice(), 7).await, Ok(()));
        assert!(read_ack(&mut ack(6).as_slice(), 7).await.is_err());
        assert!(
            read_ack(&mut &ack(7)[..5], 7).await.is_err(),
            "ends before the sequence"
        );
        let mut other: &[u8] = &[0, 0, 0, 0, 2, b'h', b'i'];
        assert!(read_ack(&mut other, 7).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_paste_sent_without_an_acknowledgement_fails_and_is_not_sent_again() {
        let (android, mut peer) = session("/nonexistent/adb").await;
        let device = tokio::spawn(async move {
            let mut got = Vec::new();
            let mut buf = [0; 64];
            let n = peer.read(&mut buf).await.unwrap();
            got.extend_from_slice(&buf[..n]);
            drop(peer);
            got
        });
        let err = android.paste("hi").await.unwrap_err();
        assert!(err.contains("read scrcpy control"), "{err}");
        assert_eq!(device.await.unwrap(), clipboard_message("hi", 1));
        assert!(android.control.lock().await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_the_server_closed_is_reopened_before_anything_is_sent() {
        let (android, mut peer) = session("/nonexistent/adb").await;
        peer.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while android
                .control
                .lock()
                .await
                .as_ref()
                .is_none_or(Control::is_open)
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let err = android.paste("hi").await.unwrap_err();
        assert!(err.contains("run adb"), "{err}");
        let mut sent = Vec::new();
        peer.read_to_end(&mut sent).await.unwrap();
        assert!(sent.is_empty(), "{sent:?}");
    }

    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn a_paste_key_the_window_does_not_handle_fails_in_time_and_drops_the_ui_server() {
        let (android, peer) = session("/nonexistent/adb").await;
        let mut hung = Command::new("sleep");
        hung.arg("30");
        assert_eq!(
            paste_through(&android, peer, hung).await,
            Err("the focused window did not handle the paste key within 5 s; the paste may still land, so check the field before typing again".to_owned())
        );
        assert!(android.ui.lock().await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_paste_key_the_device_refused_answers_not_delivered() {
        let (android, peer) = session("/nonexistent/adb").await;
        let mut refused = Command::new("sh");
        refused.args(["-c", "read l; printf 'false\\n<<end>>\\n'; sleep 30"]);
        assert_eq!(paste_through(&android, peer, refused).await, Ok(false));
    }

    #[test]
    fn a_paste_key_with_no_window_in_focus_fails_naming_no_focus() {
        assert_eq!(
            unhandled_paste("null"),
            "no window has input focus to handle the paste key within 5 s; the paste may still land, so check the field before typing again"
        );
        assert!(
            unhandled_paste("Window{1 u0 Settings}")
                .starts_with("Window{1 u0 Settings} did not handle")
        );
    }

    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn the_focused_window_diagnostic_gives_up_after_a_second() {
        let dir = tempfile::tempdir().unwrap();
        let adb = dir.path().join("adb");
        script(&adb, "#!/bin/sh\nexec sleep 30\n");
        let (android, _peer) = session(adb.to_str().unwrap()).await;
        let started = tokio::time::Instant::now();
        assert_eq!(android.focused_window().await, "the focused window");
        assert!(started.elapsed() < ADB_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_paste_key_is_pressed_again_until_one_is_delivered() {
        let pastes = std::cell::Cell::new(0);
        let out = type_with(
            "abc",
            None,
            None,
            true,
            &mut None,
            |_| {
                let f = field(if pastes.get() < 2 { "" } else { "abc" });
                async move { Ok(Some(f)) }
            },
            || {
                pastes.set(pastes.get() + 1);
                let delivered = pastes.get() > 1;
                async move { Ok(delivered) }
            },
        )
        .await
        .unwrap();
        assert_eq!((out, pastes.get()), (Some(true), 2));
    }

    #[tokio::test(start_paused = true)]
    async fn an_unchecked_paste_returns_only_once_the_field_took_it_or_the_settle_passed() {
        let pastes = std::cell::Cell::new(0);
        let after = std::cell::Cell::new(0);
        let out = type_with(
            "secret",
            None,
            None,
            true,
            &mut None,
            |_| {
                if pastes.get() > 0 {
                    after.set(after.get() + 1);
                }
                let f = secret(if after.get() < 3 { "" } else { "secret" });
                async move { Ok(Some(f)) }
            },
            || {
                pastes.set(pastes.get() + 1);
                async { Ok(true) }
            },
        )
        .await;
        assert_eq!((out, pastes.get()), (Ok(None), 1));
        assert_eq!(after.get(), 3);
        let pastes = std::cell::Cell::new(0);
        let began = tokio::time::Instant::now();
        let same = type_with(
            "new",
            None,
            None,
            true,
            &mut None,
            |_| {
                let f = secret(if pastes.get() == 0 { "old" } else { "new" });
                async move { Ok(Some(f)) }
            },
            || {
                pastes.set(pastes.get() + 1);
                async { Ok(true) }
            },
        )
        .await;
        assert_eq!((same, pastes.get()), (Ok(None), 1));
        assert!(began.elapsed() >= TYPE_WAIT, "{:?}", began.elapsed());
        let started = tokio::time::Instant::now();
        let blind = type_with(
            "secret",
            None,
            None,
            true,
            &mut None,
            |_| async { Ok(None) },
            || async { Ok(true) },
        )
        .await;
        assert_eq!(blind, Ok(None));
        assert!(started.elapsed() >= TYPE_WAIT + PASTE_SETTLE);
    }

    #[tokio::test(start_paused = true)]
    async fn a_sent_paste_whose_field_cannot_be_read_back_is_unchecked_not_failed() {
        let pastes = std::cell::Cell::new(0);
        let out = type_with(
            "abc",
            None,
            None,
            true,
            &mut None,
            |_| {
                let read = if pastes.get() == 0 {
                    Ok(Some(field("")))
                } else {
                    Err("decode focus: EOF".to_owned())
                };
                async move { read }
            },
            || {
                pastes.set(pastes.get() + 1);
                async { Ok(true) }
            },
        )
        .await;
        assert_eq!((out, pastes.get()), (Ok(None), 1));
    }

    #[tokio::test(start_paused = true)]
    async fn an_unchecked_paste_waits_past_a_lagging_cached_read_and_ends_on_a_failed_one() {
        let pastes = std::cell::Cell::new(0);
        let after = std::cell::Cell::new(0);
        let out = type_with(
            "secret",
            None,
            None,
            true,
            &mut None,
            |live| {
                if pastes.get() > 0 {
                    after.set(after.get() + 1);
                }
                let text = match (live, pastes.get(), after.get()) {
                    (true, _, _) => "",
                    (false, 0, _) => "old",
                    (false, _, n) if n < 3 => "old",
                    _ => "secret",
                };
                let f = secret(text);
                async move { Ok(Some(f)) }
            },
            || {
                pastes.set(pastes.get() + 1);
                async { Ok(true) }
            },
        )
        .await;
        assert_eq!((out, pastes.get()), (Ok(None), 1));
        assert_eq!(after.get(), 3);
        let pastes = std::cell::Cell::new(0);
        let failed = type_with(
            "secret",
            None,
            None,
            true,
            &mut None,
            |_| {
                let read = if pastes.get() == 0 {
                    Ok(Some(secret("")))
                } else {
                    Err("decode focus: EOF".to_owned())
                };
                async move { read }
            },
            || {
                pastes.set(pastes.get() + 1);
                async { Ok(true) }
            },
        )
        .await;
        assert_eq!((failed, pastes.get()), (Ok(None), 1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_line_after_a_return_goes_only_into_an_editable_field_of_the_checked_window() {
        let run = |window: Option<i64>, focus: fn(usize) -> Option<Focus>, blind: bool| async move {
            let pastes = std::cell::Cell::new(0);
            let out = type_with(
                "abc",
                None,
                window,
                blind,
                &mut None,
                |_| {
                    let f = focus(pastes.get());
                    async move { Ok(f) }
                },
                || {
                    pastes.set(pastes.get() + 1);
                    async { Ok(true) }
                },
            )
            .await;
            (
                out.map_err(|e| e.split(',').next().unwrap_or_default().to_owned()),
                pastes.get(),
            )
        };
        let refused = Err("no field of the window holds the input focus".to_owned());
        fn typed(n: usize) -> Focus {
            field(if n == 0 { "" } else { "abc" })
        }
        for window in [Some(7), None] {
            assert_eq!(
                run(
                    window,
                    |n| Some(Focus {
                        id: Some(7),
                        ..typed(n)
                    }),
                    false
                )
                .await,
                (Ok(Some(true)), 1),
                "{window:?}"
            );
            assert_eq!(
                run(
                    window,
                    |_| Some(Focus {
                        editable: false,
                        id: Some(7),
                        ..field("")
                    }),
                    false
                )
                .await,
                (refused.clone(), 0),
                "{window:?}"
            );
        }
        assert_eq!(
            run(
                None,
                |n| Some(Focus {
                    window: false,
                    ..typed(n)
                }),
                false
            )
            .await,
            (refused.clone(), 0)
        );
        assert_eq!(
            run(
                Some(7),
                |_| Some(Focus {
                    window: false,
                    id: None,
                    ..field("")
                }),
                true
            )
            .await,
            (Ok(None), 1)
        );
        assert_eq!(
            run(
                Some(7),
                |_| Some(Focus {
                    window: false,
                    id: None,
                    ..field("")
                }),
                false
            )
            .await,
            (refused, 0)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn paste_keys_refused_until_the_deadline_fail_as_not_injected() {
        let pastes = std::cell::Cell::new(0);
        let err = type_with(
            "abc",
            None,
            None,
            true,
            &mut None,
            |_| async { Ok(Some(field(""))) },
            || {
                pastes.set(pastes.get() + 1);
                async { Ok(false) }
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err, NOT_INJECTED);
        assert!(pastes.get() > 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_paste_after_the_focus_moved_fails_as_not_injected() {
        let reads = std::cell::Cell::new(0);
        let err = type_with(
            "abc",
            None,
            None,
            true,
            &mut None,
            |_| {
                reads.set(reads.get() + 1);
                let text = if reads.get() > 2 { "other" } else { "" };
                async move { Ok(Some(field(text))) }
            },
            || async { Ok(false) },
        )
        .await
        .unwrap_err();
        assert_eq!(err, NOT_INJECTED);
    }

    #[tokio::test(start_paused = true)]
    async fn a_paste_that_fails_is_not_pasted_again() {
        let pastes = std::cell::Cell::new(0);
        let err = type_with(
            "abc",
            None,
            None,
            true,
            &mut None,
            |_| async { Ok(Some(field(""))) },
            || {
                pastes.set(pastes.get() + 1);
                async { Err("the paste key was not injected".to_owned()) }
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            (err.as_str(), pastes.get()),
            ("the paste key was not injected", 1)
        );
    }

    #[test]
    fn the_screen_is_the_override_size_before_the_physical_one() {
        assert_eq!(wm_size("Physical size: 720x1280\n"), Some((720, 1280)));
        assert_eq!(
            wm_size("Physical size: 1080x2400\r\nOverride size: 720x1600\r\n"),
            Some((720, 1600))
        );
        assert_eq!(wm_size("Physical size: ?\n"), None);
        assert_eq!(wm_size(""), None);
    }

    #[test]
    fn the_ui_servers_focus_answer_decodes_and_null_is_no_focus() {
        let f: Option<Focus> = serde_json::from_str(
            r#"{"editable":true,"password":false,"window":true,"id":7,"len":5,"hash":93184636,"tail":"ab \u4f60\u597d","bounds":[100,80,620,180]}"#,
        )
        .unwrap();
        let f = f.unwrap();
        assert!(f.editable && f.window && !f.password && f.holds((360, 136)));
        assert!(
            !f.holds((360, 180))
                && f.ends_with("\u{4f60}\u{597d}")
                && f.ends_with("b \u{4f60}\u{597d}")
                && !f.ends_with("x ab \u{4f60}\u{597d}")
        );
        let none: Option<Focus> = serde_json::from_str("null").unwrap();
        assert!(none.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_type_that_shows_is_pasted_once_and_verified() {
        assert_eq!(
            typed("\u{4f60}\u{597d}", None, "", &["\u{4f60}\u{597d}"]).await,
            (Some(true), 1)
        );
        assert_eq!(typed("abc", None, "x ", &["x abc"]).await, (Some(true), 1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_delivered_paste_is_never_pasted_again_and_may_show_late() {
        assert_eq!(typed("abc", None, "", &[""]).await, (Some(false), 1));
        let pastes = std::cell::Cell::new(0);
        let started = tokio::time::Instant::now();
        let out = type_with(
            "abc",
            None,
            None,
            true,
            &mut None,
            |_| {
                let late = started.elapsed() >= Duration::from_millis(800);
                let f = field(if pastes.get() > 0 && late { "abc" } else { "" });
                async move { Ok(Some(f)) }
            },
            || {
                pastes.set(pastes.get() + 1);
                async { Ok(true) }
            },
        )
        .await
        .unwrap();
        assert_eq!((out, pastes.get()), (Some(true), 1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_lagging_cached_answer_is_read_live_before_a_repeat() {
        let pastes = std::cell::Cell::new(0);
        let out = type_with(
            "abc",
            None,
            None,
            true,
            &mut None,
            |live| {
                let f = match (live, pastes.get()) {
                    (false, _) => field("x"),
                    (true, 0) => field(""),
                    (true, _) => field("abc"),
                };
                async move { Ok(Some(f)) }
            },
            || {
                pastes.set(pastes.get() + 1);
                async { Ok(true) }
            },
        )
        .await
        .unwrap();
        assert_eq!((out, pastes.get()), (Some(true), 1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_cached_answer_that_still_shows_older_text_is_no_proof_of_the_paste() {
        let pastes = std::cell::Cell::new(0);
        let out = type_with(
            "abc",
            None,
            None,
            true,
            &mut None,
            |live| {
                let f = field(if live { "" } else { "abc" });
                async move { Ok(Some(f)) }
            },
            || {
                pastes.set(pastes.get() + 1);
                async { Ok(true) }
            },
        )
        .await
        .unwrap();
        assert_eq!((out, pastes.get()), (Some(false), 1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_field_that_already_ends_with_the_line_shows_the_paste_only_once_it_changes() {
        assert_eq!(typed("bc", None, "abc", &["abc"]).await, (Some(false), 1));
        assert_eq!(typed("bc", None, "abc", &["abcbc"]).await, (Some(true), 1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_field_that_changed_otherwise_is_unverified_and_not_pasted_again() {
        assert_eq!(typed("abc", None, "", &["ABC"]).await, (None, 1));
        assert_eq!(
            typed("5551234567", None, "", &["(555) 123-4567"]).await,
            (None, 1)
        );
        assert_eq!(typed("x", None, "abc", &["axbc"]).await, (None, 1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_password_field_or_no_field_gets_one_unverified_paste() {
        let pastes = std::cell::Cell::new(0);
        let password = type_with(
            "secret",
            None,
            None,
            true,
            &mut None,
            |_| async { Ok(Some(secret(""))) },
            || {
                pastes.set(pastes.get() + 1);
                async { Ok(true) }
            },
        )
        .await
        .unwrap();
        assert_eq!((password, pastes.get()), (None, 1));
        let started = tokio::time::Instant::now();
        let none = type_with(
            "abc",
            None,
            None,
            true,
            &mut None,
            |_| async { Ok(None) },
            || async { Ok(true) },
        )
        .await
        .unwrap();
        assert_eq!(none, None);
        assert!(started.elapsed() >= TYPE_WAIT);
    }

    #[tokio::test(start_paused = true)]
    async fn a_type_after_a_tap_checks_the_field_under_the_tap() {
        assert_eq!(
            typed("abc", Some((360, 136)), "", &["abc"]).await,
            (Some(true), 1)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_type_after_a_tap_beside_the_focused_field_fails_without_a_paste() {
        let pastes = std::cell::Cell::new(0);
        let err = type_with(
            "abc",
            Some((360, 350)),
            None,
            true,
            &mut None,
            |_| async { Ok(Some(field(""))) },
            || {
                pastes.set(pastes.get() + 1);
                async { Ok(true) }
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            (err.as_str(), pastes.get()),
            (
                "the field at (360,350) has no input focus; another field does",
                0
            )
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_type_after_a_tap_with_no_accessible_field_gets_one_unverified_paste() {
        let pastes = std::cell::Cell::new(0);
        let out = type_with(
            "abc",
            Some((360, 350)),
            None,
            true,
            &mut None,
            |_| async { Ok(None) },
            || {
                pastes.set(pastes.get() + 1);
                async { Ok(true) }
            },
        )
        .await
        .unwrap();
        assert_eq!((out, pastes.get()), (None, 1));
    }

    #[cfg(unix)]
    async fn acked(fake: &Fake) -> Arc<std::sync::Mutex<Vec<String>>> {
        let mut peer = controlled(&fake.android).await;
        let pasted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&pasted);
        tokio::spawn(async move {
            let mut head = [0u8; 14];
            while peer.read_exact(&mut head).await.is_ok() {
                let len = u32::from_be_bytes([head[10], head[11], head[12], head[13]]);
                let mut text = vec![0u8; len as usize];
                peer.read_exact(&mut text).await.unwrap();
                seen.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&text).into_owned());
                let mut ack = vec![ACK_CLIPBOARD];
                ack.extend_from_slice(&head[1..9]);
                peer.write_all(&ack).await.unwrap();
            }
        });
        pasted
    }

    #[cfg(unix)]
    fn focused(id: i64, text: &str) -> String {
        format!(
            r#"{{"editable":true,"password":false,"window":true,"id":{id},"len":{},"hash":{},"tail":"{text}","bounds":[0,0,720,100]}}"#,
            text.encode_utf16().count(),
            java_hash(text)
        )
    }

    #[cfg(unix)]
    fn documents(dialog: bool) -> String {
        let main = format!(
            r#"{{"id":10,"type":1,"layer":0,"title":"Downloads","package":"com.android.documentsui","active":{},"focused":{},"bounds":[0,0,720,1280]}}"#,
            !dialog, !dialog
        );
        if dialog {
            format!(
                r#"[{{"id":11,"type":1,"layer":1,"title":"New folder","package":"com.android.documentsui","active":true,"focused":true,"bounds":[60,400,660,800]}},{main}]"#
            )
        } else {
            format!("[{main}]")
        }
    }

    #[cfg(unix)]
    struct Fake {
        dir: tempfile::TempDir,
        android: Android,
    }

    #[cfg(unix)]
    impl Fake {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let adb = dir.path().join("adb");
            script(
                &adb,
                "#!/bin/sh\nDIR=$(dirname \"$0\")\ncase \"$*\" in\n*\"forward tcp:0\"*) cat \"$DIR/port\" 2>/dev/null;;\n*\"wm size\"*) echo 'Physical size: 720x1280';;\n*\"shell input\"*) echo \"$*\" >> \"$DIR/adbinput\"; if [ -f \"$DIR/after-swipe.xml\" ]; then cp \"$DIR/after-swipe.xml\" \"$DIR/dump.xml\"; fi; if [ -f \"$DIR/after-swipe.json\" ]; then mv \"$DIR/after-swipe.json\" \"$DIR/windows.json\"; fi;;\n*screencap*) if [ -f \"$DIR/screencap.fail\" ]; then exit 1; fi; cat \"$DIR/screen.png\" 2>/dev/null || printf png;;\n*app_process*)\n  echo start >> \"$DIR/starts\"\n  echo ready\n  while read l; do\n    echo \"$l\" >> \"$DIR/requests\"\n    case \"$l\" in\n      shot) if [ -f \"$DIR/shot.frame\" ]; then cat \"$DIR/shot.frame\"; continue; fi;;\n      tap*|key*) if [ -f \"$DIR/after-key-focus.json\" ]; then mv \"$DIR/after-key-focus.json\" \"$DIR/focus.json\"; fi; if [ -f \"$DIR/after-input.json\" ]; then mv \"$DIR/after-input.json\" \"$DIR/windows.json\"; fi; a=$(head -n 1 \"$DIR/inject.answer\" 2>/dev/null); if [ -f \"$DIR/inject.answer\" ] && [ \"$(wc -l < \"$DIR/inject.answer\")\" -ge 1 ]; then tail -n +2 \"$DIR/inject.answer\" > \"$DIR/inject.next\"; mv \"$DIR/inject.next\" \"$DIR/inject.answer\"; fi; if [ \"$a\" = exit ]; then exit 0; fi; printf %s \"$a\";;\n      show*) cat \"$DIR/show.answer\" 2>/dev/null; if [ -f \"$DIR/after-show.xml\" ]; then cp \"$DIR/after-show.xml\" \"$DIR/dump.xml\"; fi; if [ -f \"$DIR/after-show.json\" ]; then mv \"$DIR/after-show.json\" \"$DIR/windows.json\"; fi;;\n      windows) cat \"$DIR/windows.json\";;\n      \"focus live\") if [ -f \"$DIR/live.fail\" ]; then printf '<error>focus read failed</error>'; else cat \"$DIR/focus.json\" 2>/dev/null; fi;;\n      focus*) cat \"$DIR/focus.json\" 2>/dev/null;;\n      paste) if [ -f \"$DIR/after-paste.json\" ]; then mv \"$DIR/after-paste.json\" \"$DIR/focus.json\"; fi; printf true;;\n      dump*) if [ -f \"$DIR/after-dump-focus.json\" ]; then mv \"$DIR/after-dump-focus.json\" \"$DIR/focus.json\"; fi; if [ -f \"$DIR/dump-first.xml\" ]; then cat \"$DIR/dump-first.xml\"; rm \"$DIR/dump-first.xml\"; else cat \"$DIR/dump.xml\"; fi;;\n    esac\n    echo\n    echo '<<end>>'\n  done;;\nesac\n",
            );
            let android = android_at(adb);
            Self { dir, android }
        }

        fn put(&self, name: &str, text: &str) {
            std::fs::write(self.dir.path().join(name), text).unwrap();
        }

        fn windows(&self, layout: &str) {
            let app = |active| {
                format!(
                    r#"{{"id":7,"type":1,"layer":0,"title":"Settings","package":"com.android.settings","active":{active},"focused":{active},"bounds":[0,0,720,1280]}}"#
                )
            };
            let bar = r#"{"id":5,"type":3,"layer":2,"title":"","package":"com.android.systemui","active":false,"focused":false,"bounds":[0,1184,720,1280]}"#;
            let list = match layout {
                "shade" => format!(
                    r#"[{bar},{{"id":9,"type":3,"layer":4,"title":"Shade","package":"com.android.systemui","active":true,"focused":true,"bounds":[0,0,720,1280]}},{}]"#,
                    app(false)
                ),
                "keyboard" => format!(
                    r#"[{{"id":8,"type":2,"layer":3,"title":"","package":"com.android.inputmethod.latin","active":false,"focused":false,"bounds":[0,700,720,1184]}},{bar},{}]"#,
                    app(true)
                ),
                "landscape" => format!(
                    r#"[{{"id":5,"type":3,"layer":2,"title":"","package":"com.android.systemui","active":false,"focused":false,"bounds":[1184,0,1280,720]}},{}]"#,
                    app(true).replace("[0,0,720,1280]", "[0,0,1280,720]")
                ),
                "none" => "[]".to_owned(),
                "unsettled" => format!("[{bar},{}]", app(false)),
                _ => format!("[{bar},{}]", app(true)),
            };
            self.put("windows.json", &list);
        }

        fn dump(&self, name: &str, window: &str) {
            let attr = if window.is_empty() {
                String::new()
            } else {
                format!(" window=\"{window}\"")
            };
            self.put(
                name,
                &format!(
                    r#"<hierarchy{attr}><node text="OK" class="android.widget.TextView" content-desc="" hint="" showing-hint="false" checkable="false" checked="false" clickable="true" long-clickable="false" editable="false" password="false" enabled="true" focused="false" selected="false" scrollable="false" visible="true" path="0.0" bounds="[100,200][300,300]" /></hierarchy>"#
                ),
            );
        }
    }

    fn saved(points: Vec<(i32, i32)>) -> Saved {
        Saved {
            window: String::new(),
            screen: (720, 1280),
            points,
        }
    }

    #[cfg(unix)]
    fn script(path: &std::path::Path, body: &str) {
        let made = std::process::Command::new("/bin/sh")
            .args(["-c", r#"printf '%s' "$1" > "$0" && chmod 755 "$0""#])
            .arg(path)
            .arg(body)
            .status()
            .unwrap();
        assert!(made.success(), "write {}", path.display());
    }

    #[cfg(unix)]
    async fn failing_ui_server(log: &str, stderr: &str, status: u8) -> String {
        let dir = tempfile::tempdir().unwrap();
        let adb = dir.path().join("adb");
        script(
            &adb,
            &format!(
                "#!/bin/sh\nDIR=$(dirname \"$0\")\ncase \"$*\" in\n*logcat*) cat \"$DIR/crash.log\";;\n*app_process*) echo '{stderr}' >&2; exit {status};;\nesac\n"
            ),
        );
        std::fs::write(dir.path().join("crash.log"), log).unwrap();
        let android = android_at(adb);
        android.windows().await.unwrap_err()
    }

    #[cfg(unix)]
    fn log(fake: &Fake, name: &str) -> String {
        std::fs::read_to_string(fake.dir.path().join(name)).unwrap_or_default()
    }

    #[cfg(unix)]
    fn list(fake: &Fake, name: &str, target: &str) {
        fake.put(
            name,
            &format!(
                r#"<hierarchy window="7"><node text="" class="androidx.recyclerview.widget.RecyclerView" content-desc="" hint="" showing-hint="false" checkable="false" checked="false" clickable="false" long-clickable="false" editable="false" password="false" enabled="true" focused="false" selected="false" scrollable="true" visible="true" path="0.0" bounds="[0,160][720,1184]" /><node text="Target" class="android.widget.TextView" content-desc="" hint="" showing-hint="false" checkable="false" checked="false" clickable="true" long-clickable="false" editable="false" password="false" enabled="true" focused="false" selected="false" scrollable="false" visible="true" path="0.0.1" bounds="{target}" /></hierarchy>"#
            ),
        );
    }

    #[cfg(unix)]
    async fn press_target(fake: &Fake) -> Value {
        let out = crate::desk::run(
            &fake.android,
            "desk_press",
            serde_json::json!({"id": "com.android.settings:0.0.1:button:Target"}),
        )
        .await
        .unwrap();
        serde_json::from_str(&out.text).unwrap()
    }

    #[cfg(unix)]
    fn tap_element(session: &str, element: usize) -> ActArgs {
        serde_json::from_value(
            serde_json::json!({"session": session, "action": "tap", "element": element}),
        )
        .unwrap()
    }

    #[cfg(unix)]
    async fn desk(fake: &Fake, verb: &str, args: Value) -> Result<Value, String> {
        let out = crate::desk::run(&fake.android, verb, args).await?;
        Ok(serde_json::from_str(&out.text).unwrap())
    }

    #[cfg(unix)]
    async fn session(adb: &str) -> (Android, TcpStream) {
        let android = android_at(adb);
        let peer = controlled(&android).await;
        (android, peer)
    }

    #[cfg(unix)]
    async fn controlled(android: &Android) -> TcpStream {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (peer, _) = listener.accept().await.unwrap();
        *android.control.lock().await = Some(Control {
            stream,
            sequence: 0,
            pending: false,
            filled: false,
            _server: Command::new("sleep")
                .arg("30")
                .kill_on_drop(true)
                .spawn()
                .unwrap(),
            _forward: Forward {
                port: String::new(),
                remove: Command::new("true"),
            },
        });
        peer
    }

    fn ack(sequence: u64) -> Vec<u8> {
        let mut msg = vec![ACK_CLIPBOARD];
        msg.extend_from_slice(&sequence.to_be_bytes());
        msg
    }

    #[cfg(unix)]
    async fn paste_through(
        android: &Android,
        mut peer: TcpStream,
        mut ui: Command,
    ) -> Result<bool, String> {
        peer.write_all(&ack(1)).await.unwrap();
        let device = tokio::spawn(async move {
            let mut buf = [0; 64];
            let n = peer.read(&mut buf).await.unwrap();
            (buf[..n].to_vec(), peer)
        });
        let mut ui = ui
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        *android.ui.lock().await = Some(UiServer {
            stdin: ui.stdin.take().unwrap(),
            stdout: BufReader::new(ui.stdout.take().unwrap()),
            child: ui,
        });
        let pasted = android.paste("hi").await;
        assert_eq!(device.await.unwrap().0, clipboard_message("hi", 1));
        pasted
    }

    fn secret(text: &str) -> Focus {
        Focus {
            password: true,
            hash: 0,
            tail: String::new(),
            ..field(text)
        }
    }

    fn field(text: &str) -> Focus {
        let n = text.chars().count();
        Focus {
            editable: true,
            window: true,
            id: Some(7),
            bounds: [100, 80, 620, 180],
            len: text.encode_utf16().count(),
            hash: java_hash(text),
            tail: text.chars().skip(n.saturating_sub(32)).collect(),
            ..Focus::default()
        }
    }

    fn java_hash(text: &str) -> i32 {
        text.encode_utf16()
            .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)))
    }

    async fn typed(
        line: &str,
        at: Option<(i32, i32)>,
        before: &str,
        after: &[&str],
    ) -> (Option<bool>, usize) {
        let pastes = std::cell::Cell::new(0);
        let out = type_with(
            line,
            at,
            None,
            true,
            &mut None,
            |_| {
                let text = match pastes.get() {
                    0 => before,
                    n => after[(n - 1).min(after.len() - 1)],
                };
                let f = field(text);
                async move { Ok(Some(f)) }
            },
            || {
                pastes.set(pastes.get() + 1);
                async { Ok(true) }
            },
        )
        .await
        .unwrap();
        (out, pastes.get())
    }

    fn android_at(adb: impl Into<PathBuf>) -> Android {
        Android::new(Device {
            adb: adb.into(),
            serial: None,
            ui_jar: "/u.jar".into(),
            scrcpy_jar: "/s.jar".into(),
            su: None,
        })
    }
}
