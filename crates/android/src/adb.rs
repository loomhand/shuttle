//! Screenshot, UI elements and input on an Android device through adb, with text pasted by a scrcpy control session.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, OnceCell};

use desk_core::args::parse;
use desk_core::wire::Output;

use crate::desk::{self, Node};

const ADB_TIMEOUT: Duration = Duration::from_secs(30);
const SETTLE: Duration = Duration::from_millis(500);
const MAX_WAIT: f64 = 10.0;
const MAX_ELEMENTS: usize = 200;
const LONG_PRESS_MS: u32 = 800;
const SWIPE_MS: u32 = 300;
const SCROLL_FRACTION: i32 = 3;
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
const TYPE_WAIT: Duration = Duration::from_secs(1);
const TYPE_DEADLINE: Duration = Duration::from_millis(1500);
const TYPE_FOCUS_POLL: Duration = Duration::from_millis(20);
const VERIFY_WAIT: Duration = Duration::from_millis(300);
const VERIFY_POLL: Duration = Duration::from_millis(10);
const VERIFY_TAIL: usize = 16;
const DEVICE_UI_JAR: &str = "/data/local/tmp/shuttle-ui.jar";
const UI_TIMEOUT: Duration = Duration::from_secs(10);
const STATE_TRIES: u32 = 2;
const UI_END: &str = "<<end>>";
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
    points: Mutex<HashMap<String, Saved>>,
    ui: Mutex<Option<UiServer>>,
    control: Mutex<Option<Control>>,
    screen: OnceCell<(i32, i32)>,
}

impl Android {
    /// Drives the Android device `device`.
    pub fn new(device: Device) -> Self {
        Self {
            device,
            points: Mutex::new(HashMap::new()),
            ui: Mutex::new(None),
            control: Mutex::new(None),
            screen: OnceCell::new(),
        }
    }

    /// The screen as PNG.
    pub async fn look(&self) -> Result<Output, String> {
        let png = self.adb(&["exec-out", "screencap", "-p"]).await?;
        let (w, h) = self.screen().await?;
        Ok(Output {
            text: format!("{w}x{h} screen"),
            image_b64: Some(B64.encode(png)),
            ..Output::default()
        })
    }

    /// The UI elements of the screen, [index] for computer_act, and a screenshot.
    pub async fn state(&self, args: Value) -> Result<Output, String> {
        let args: StateArgs = parse(args)?;
        let mut tries = 0;
        let (xml, shot, front) = loop {
            let (xml, shot, front) = tokio::join!(self.tree(None), self.look(), desk::front(self));
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
        let text = elements
            .iter()
            .enumerate()
            .map(|(i, e)| e.line(i))
            .collect::<Vec<_>>()
            .join("\n");
        let saved = Saved {
            window: front.window().to_owned(),
            points: elements.iter().map(Node::center).collect(),
        };
        self.points.lock().await.insert(args.session, saved);
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
                self.tap((x, y)).await?;
                self.tap((x, y)).await?;
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
                let (w, h) = self.screen().await?;
                let (x, y) = point.unwrap_or((w / 2, h / 2));
                let (dx, dy) = match args.direction.as_deref().unwrap_or("down") {
                    "up" => (0, h / SCROLL_FRACTION),
                    "down" => (0, -h / SCROLL_FRACTION),
                    "left" => (w / SCROLL_FRACTION, 0),
                    "right" => (-w / SCROLL_FRACTION, 0),
                    other => return Err(format!("unknown direction {other}")),
                };
                for _ in 0..args.amount.unwrap_or(1).max(1) {
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
                match self.type_text(text, point).await? {
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
        let mut out = self.look().await?;
        out.text = report;
        Ok(out)
    }

    /// Forgets the element centres of a session, the default one when none is named.
    pub async fn end(&self, args: &Value) -> Result<String, String> {
        let session = args["session"].as_str().unwrap_or_default();
        self.points.lock().await.remove(session);
        Ok("ended".to_owned())
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

    pub(crate) async fn tap(&self, (x, y): (i32, i32)) -> Result<(), String> {
        self.input(&["tap", &x.to_string(), &y.to_string()]).await
    }

    /// Presses a key or a combination such as `ctrl+a`.
    pub(crate) async fn keys(&self, keys: &str) -> Result<(), String> {
        let codes: Vec<String> = keycodes(keys)?.iter().map(u32::to_string).collect();
        let mut argv = vec![if codes.len() == 1 {
            "keyevent"
        } else {
            "keycombination"
        }];
        argv.extend(codes.iter().map(String::as_str));
        self.input(&argv).await
    }

    pub(crate) async fn input(&self, args: &[&str]) -> Result<(), String> {
        let mut argv = vec!["shell", "input"];
        argv.extend_from_slice(args);
        self.adb(&argv).await.map(drop)
    }

    /// The UI tree of window `window`, else of the active one, from the image's UI server.
    pub(crate) async fn tree(&self, window: Option<&str>) -> Result<String, String> {
        match window {
            Some(w) => self.served(&format!("dump {w}")).await,
            None => self.served("dump").await,
        }
    }

    /// The interactive windows as the UI server lists them, as JSON.
    pub(crate) async fn windows(&self) -> Result<String, String> {
        self.served("windows").await
    }

    /// Types `text` into the focused field, the first line under `at` when given: each line pasted and checked (see
    /// [`type_with`]), Return between lines.
    pub(crate) async fn type_text(
        &self,
        text: &str,
        at: Option<(i32, i32)>,
    ) -> Result<Option<bool>, String> {
        let mut typed = Some(true);
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                self.keys("return").await?;
            }
            if line.is_empty() {
                continue;
            }
            let shown = type_with(
                line,
                at.filter(|_| i == 0),
                |live| async move {
                    let answer = self
                        .served(if live { "focus live" } else { "focus" })
                        .await?;
                    serde_json::from_str(answer.trim()).map_err(|e| format!("decode focus: {e}"))
                },
                || self.paste(line),
            )
            .await?;
            typed = match (typed, shown) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (None, _) | (_, None) => None,
                _ => Some(true),
            };
        }
        Ok(typed)
    }

    /// Pastes `text` into the focused field through the clipboard, which takes any Unicode, unlike key injection: scrcpy sets the
    /// clipboard, and the UI server presses the paste key and answers once the focused window has handled it: false when the
    /// device refused the key, which then reached no window.
    pub(crate) async fn paste(&self, text: &str) -> Result<bool, String> {
        {
            let mut slot = self.control.lock().await;
            let mut control = match slot.take() {
                Some(control) if control.is_open() => control,
                _ => self.open_control().await?,
            };
            control.set_clipboard(text).await?;
            *slot = Some(control);
        }
        match tokio::time::timeout(PASTE_TIMEOUT, self.served("paste")).await {
            Ok(answer) => answer.map(|a| a.trim() == "true"),
            Err(_) => Err(format!(
                "{} did not handle the paste key within {} s; the paste may still land, so check the field before typing again",
                self.focused_window().await,
                PASTE_TIMEOUT.as_secs()
            )),
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

    /// The screen size `wm size` reports, read once per device.
    pub(crate) async fn screen(&self) -> Result<(i32, i32), String> {
        self.screen
            .get_or_try_init(|| async {
                let out = self.adb(&["shell", "wm", "size"]).await?;
                let out = String::from_utf8_lossy(&out);
                wm_size(&out).ok_or_else(|| format!("wm size said {:?}", out.trim()))
            })
            .await
            .copied()
    }

    pub(crate) async fn adb(&self, args: &[&str]) -> Result<Vec<u8>, String> {
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
        let (window, point) = {
            let saved = self.points.lock().await;
            let saved = saved.get(&args.session);
            let point = saved.and_then(|s| s.points.get(i)).copied();
            let point =
                point.ok_or_else(|| format!("no element [{i}] in the latest computer_state"))?;
            (saved.map(|s| s.window.clone()).unwrap_or_default(), point)
        };
        let front = desk::front(self).await?;
        if window.is_empty() || front.window().is_empty() {
            return Err(
                "the window list is unavailable; use x,y or call computer_state again".to_owned(),
            );
        }
        if front.window() != window {
            return Err("the screen changed since computer_state; call it again".to_owned());
        }
        if front.covered(point) {
            return Err(format!(
                "element [{i}] is covered now; call computer_state again"
            ));
        }
        Ok(Some(point))
    }

    async fn swipe(&self, from: (i32, i32), to: (i32, i32), ms: u32) -> Result<(), String> {
        let args = [from.0, from.1, to.0, to.1].map(|v| v.to_string());
        let ms = ms.to_string();
        self.input(&["swipe", &args[0], &args[1], &args[2], &args[3], &ms])
            .await
    }

    /// One answer of the resident UI server, started on first use; a server that fails or is cancelled mid-answer is dropped.
    async fn served(&self, cmd: &str) -> Result<String, String> {
        let mut slot = self.ui.lock().await;
        let mut server = match slot.take() {
            Some(server) => server,
            None => self.start_ui().await?,
        };
        let answer = tokio::time::timeout(UI_TIMEOUT, server.ask(cmd))
            .await
            .map_err(|_| "UI server: timed out".to_owned())??;
        *slot = Some(server);
        match answer.trim().strip_prefix("<error>") {
            Some(e) => Err(format!("UI server: {}", e.trim_end_matches("</error>"))),
            None => Ok(answer),
        }
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
            .map_err(|e| format!("read UI server: {e}"))?;
        if ready.trim() != "ready" {
            return Err(self.start_failure(&mut child, stderr, ready.trim()).await);
        }
        Ok(UiServer {
            _child: child,
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
            let end = (0..=UI_ABORT_KEEP.min(abort.len()))
                .rev()
                .find(|&i| abort.is_char_boundary(i))
                .unwrap_or(0);
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

struct Saved {
    window: String,
    points: Vec<(i32, i32)>,
}

/// The UI server (`android/ui`) on the device, one answer per request line; dropping it stops the server.
struct UiServer {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl UiServer {
    async fn ask(&mut self, cmd: &str) -> Result<String, String> {
        self.stdin
            .write_all(format!("{cmd}\n").as_bytes())
            .await
            .map_err(|e| format!("ask UI server: {e}"))?;
        let mut answer = String::new();
        let mut line = String::new();
        loop {
            line.clear();
            let n = self
                .stdout
                .read_line(&mut line)
                .await
                .map_err(|e| format!("read UI server: {e}"))?;
            if n == 0 {
                return Err("UI server exited".to_owned());
            }
            if line.trim_end() == UI_END {
                return Ok(answer);
            }
            answer.push_str(&line);
        }
    }
}

/// The resident control-only scrcpy session of the paste: its socket, server and adb forward, which a drop ends.
struct Control {
    stream: TcpStream,
    sequence: u64,
    _server: Child,
    _forward: Forward,
}

impl Control {
    /// Whether the server still holds the session; it sends nothing unasked, so anything readable is its end.
    fn is_open(&self) -> bool {
        matches!(self.stream.try_read(&mut [0; 1]), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
    }

    async fn set_clipboard(&mut self, text: &str) -> Result<(), String> {
        self.sequence += 1;
        self.stream
            .write_all(&clipboard_message(text, self.sequence))
            .await
            .map_err(|e| format!("send clipboard: {e}"))?;
        tokio::time::timeout(ACK_TIMEOUT, read_ack(&mut self.stream, self.sequence))
            .await
            .map_err(|_| "scrcpy control: no clipboard acknowledgement".to_owned())?
    }
}

struct Forward {
    port: String,
    remove: Option<Command>,
}

impl Forward {
    fn new(android: &Android, port: String) -> Self {
        let mut remove = android.command();
        remove
            .args(["forward", "--remove", &format!("tcp:{port}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Self {
            port,
            remove: Some(remove),
        }
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        if let Some(mut remove) = self.remove.take() {
            let _ = remove.spawn();
        }
    }
}

/// The node with input focus, as the UI server's `focus` names it: its text is a length, a hash and its last 32 code points.
#[derive(Debug, Default, Deserialize)]
struct Focus {
    editable: bool,
    password: bool,
    window: bool,
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
        let n = line.chars().count();
        let tail: String = line.chars().skip(n.saturating_sub(VERIFY_TAIL)).collect();
        self.tail.ends_with(&tail)
    }
}

#[derive(Deserialize)]
struct StateArgs {
    #[serde(default)]
    session: String,
    #[serde(default)]
    query: Option<String>,
}

#[derive(Deserialize)]
struct ActArgs {
    #[serde(default)]
    session: String,
    action: String,
    #[serde(default)]
    element: Option<usize>,
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
    direction: Option<String>,
    #[serde(default)]
    amount: Option<u32>,
    #[serde(default)]
    app: Option<String>,
    #[serde(default)]
    seconds: Option<f64>,
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

/// Pastes `line` through `paste` into the field `focus` names and checks the field shows it; again while the field did not
/// change.
///
/// It waits, within a second, for an editable node with input focus in the focused window, under `at` when given, and takes its
/// text live; an error when the focused field lies outside `at`. After a paste it asks `focus` at once and then every 10 ms, at
/// most 300 ms, until that text changed and ends with the line's last 16 characters, and reads it live once when the window
/// ends; a cached answer shows the paste only once it differs from the cached text at the snapshot. A paste key the device
/// refused reached no window, so it is pressed again at once. Some(true) is a paste that shows. Some(false) is one that does
/// not: the text stayed the same through every paste started within 1.5 s of the call. An error when no key was delivered by
/// then. None is a paste it does not check: no such field (a web view), a password field, which a second paste would double, or
/// text that changed otherwise (an insert inside the text, a filter or a format), which is not pasted again.
async fn type_with<F, Ff, P, Pf>(
    line: &str,
    at: Option<(i32, i32)>,
    mut focus: F,
    mut paste: P,
) -> Result<Option<bool>, String>
where
    F: FnMut(bool) -> Ff,
    Ff: Future<Output = Result<Option<Focus>, String>>,
    P: FnMut() -> Pf,
    Pf: Future<Output = Result<bool, String>>,
{
    let usable = |f: &Focus| f.editable && f.window && at.is_none_or(|p| f.holds(p));
    let start = tokio::time::Instant::now();
    let end = start + TYPE_WAIT;
    let before = loop {
        let cached = focus(false).await?;
        if cached.as_ref().is_some_and(usable)
            && let Some(f) = focus(true).await?.filter(usable)
        {
            break Some((f, cached));
        }
        if tokio::time::Instant::now() >= end {
            if let (Some((x, y)), Some(f)) = (at, cached)
                && f.editable
                && f.window
            {
                return Err(format!(
                    "the field at ({x},{y}) has no input focus; another field does"
                ));
            }
            break None;
        }
        tokio::time::sleep(TYPE_FOCUS_POLL).await;
    };
    let Some((before, cached)) = before.filter(|(f, _)| !f.password) else {
        return if paste().await? {
            Ok(None)
        } else {
            Err(NOT_INJECTED.to_owned())
        };
    };
    let shows = |f: &Focus| !f.same_text(&before) && f.ends_with(line);
    let mut delivered = false;
    loop {
        if paste().await? {
            delivered = true;
            let deadline = tokio::time::Instant::now() + VERIFY_WAIT;
            loop {
                if focus(false)
                    .await?
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
        let now = focus(true).await?.filter(usable);
        if now.as_ref().is_some_and(shows) {
            return Ok(Some(true));
        }
        if !now.is_some_and(|f| f.same_text(&before)) {
            return Ok(None);
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

/// Whether `s` is an Android package name, such as `com.android.settings`, which a device shell takes as it is.
pub(crate) fn is_package(s: &str) -> bool {
    s.split('.').count() >= 2
        && s.split('.').all(|part| {
            part.starts_with(|c: char| c.is_ascii_alphabetic())
                && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
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

/// A scrcpy SET_CLIPBOARD message that sets the clipboard to `text`, without scrcpy's own paste key, and asks for an
/// acknowledgement of `sequence`.
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

    fn saved(points: Vec<(i32, i32)>) -> Saved {
        Saved {
            window: String::new(),
            points,
        }
    }

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
        let android = Android::new(Device {
            adb: "/nonexistent/adb".into(),
            serial: None,
            ui_jar: "/u.jar".into(),
            scrcpy_jar: "/s.jar".into(),
            su: Some("/su".into()),
        });
        let err = android.launch("com.x; reboot").await.unwrap_err();
        assert!(err.contains("not an Android package"), "{err}");
        let err = android.launch("com.x").await.unwrap_err();
        assert!(err.contains("run adb"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_component_reaches_the_device_shell_quoted() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let adb = dir.path().join("adb");
        let argv = dir.path().join("argv");
        std::fs::write(
            &adb,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n", argv.display()),
        )
        .unwrap();
        std::fs::set_permissions(&adb, std::fs::Permissions::from_mode(0o755)).unwrap();
        let android = Android::new(Device {
            adb,
            serial: None,
            ui_jar: "/u.jar".into(),
            scrcpy_jar: "/s.jar".into(),
            su: Some("/su".into()),
        });
        android.launch("com.example/.Main$Launcher").await.unwrap();
        let args = std::fs::read_to_string(&argv).unwrap();
        assert_eq!(args.lines().last(), Some("'com.example/.Main$Launcher'"));
    }

    const CRASH_LOG: &str = "10-19 08:01:40.100  3200  3200 F DEBUG   : Cmdline: /system/bin/other\n10-19 08:01:40.100  3200  3200 F DEBUG   : Abort message: 'not ours'\n10-19 08:01:50.777  3318  3318 F DEBUG   : Cmdline: app_process / shuttle.Ui\n10-19 08:01:50.777  3318  3318 F DEBUG   : pid: 3308, tid: 3308, name: main  >>> app_process <<<\n10-19 08:01:50.777  3318  3318 F DEBUG   : Abort message: 'No pending exception expected: java.lang.ClassNotFoundException: shuttle.Ui\n10-19 08:01:50.777  3318  3318 F DEBUG   : (Throwable with empty stack trace)'\n10-19 08:01:51.000  3330  3330 F DEBUG   : Cmdline: /system/bin/other\n10-19 08:01:51.000  3330  3330 F DEBUG   : Abort message: 'later, not ours'\n";

    #[test]
    fn the_abort_cause_is_the_last_one_of_a_cb_ui_process() {
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
    async fn failing_ui_server(log: &str, stderr: &str, status: u8) -> String {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let adb = dir.path().join("adb");
        std::fs::write(&adb, format!("#!/bin/sh\nDIR=$(dirname \"$0\")\ncase \"$*\" in\n*logcat*) cat \"$DIR/crash.log\";;\n*app_process*) echo '{stderr}' >&2; exit {status};;\nesac\n")).unwrap();
        std::fs::write(dir.path().join("crash.log"), log).unwrap();
        std::fs::set_permissions(&adb, std::fs::Permissions::from_mode(0o755)).unwrap();
        let android = Android::new(Device {
            adb,
            serial: None,
            ui_jar: "/u.jar".into(),
            scrcpy_jar: "/s.jar".into(),
            su: None,
        });
        android.windows().await.unwrap_err()
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
    struct Fake {
        dir: tempfile::TempDir,
        android: Android,
    }

    #[cfg(unix)]
    impl Fake {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let adb = dir.path().join("adb");
            std::fs::write(
                &adb,
                "#!/bin/sh\nDIR=$(dirname \"$0\")\ncase \"$*\" in\n*\"wm size\"*) echo 'Physical size: 720x1280';;\n*screencap*) printf png;;\n*app_process*)\n  echo ready\n  while read l; do\n    case \"$l\" in\n      windows) cat \"$DIR/windows.json\";;\n      dump*) if [ -f \"$DIR/dump-first.xml\" ]; then cat \"$DIR/dump-first.xml\"; rm \"$DIR/dump-first.xml\"; else cat \"$DIR/dump.xml\"; fi;;\n    esac\n    echo\n    echo '<<end>>'\n  done;;\nesac\n",
            )
            .unwrap();
            std::fs::set_permissions(&adb, std::fs::Permissions::from_mode(0o755)).unwrap();
            let android = Android::new(Device {
                adb,
                serial: None,
                ui_jar: "/u.jar".into(),
                scrcpy_jar: "/s.jar".into(),
                su: None,
            });
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
                "none" => "[]".to_owned(),
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
                    r#"<hierarchy{attr}><node text="OK" class="android.widget.TextView" content-desc="" hint="" showing-hint="false" checkable="false" checked="false" clickable="true" long-clickable="false" editable="false" enabled="true" focused="false" selected="false" scrollable="false" visible="true" depth="1" path="0.0" bounds="[100,200][300,300]" /></hierarchy>"#
                ),
            );
        }
    }

    #[cfg(unix)]
    fn tap_element(session: &str, element: usize) -> ActArgs {
        serde_json::from_value(
            serde_json::json!({"session": session, "action": "tap", "element": element}),
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_indexed_point_is_refused_once_the_screen_it_came_from_changed() {
        let fake = Fake::new();
        fake.windows("");
        fake.android.points.lock().await.insert(
            "s".into(),
            Saved {
                window: "7".into(),
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
        let explicit: ActArgs = serde_json::from_value(
            serde_json::json!({"session": "s", "action": "tap", "x": 360.0, "y": 760.0}),
        )
        .unwrap();
        fake.windows("shade");
        assert_eq!(fake.android.point(&explicit).await, Ok(Some((360, 760))));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_indexed_point_is_refused_when_either_window_id_is_unknown() {
        let fake = Fake::new();
        let unavailable = "the window list is unavailable; use x,y or call computer_state again";
        fake.windows("none");
        fake.android.points.lock().await.insert(
            "known".into(),
            Saved {
                window: "7".into(),
                points: vec![(360, 760)],
            },
        );
        fake.android.points.lock().await.insert(
            "unknown".into(),
            Saved {
                window: String::new(),
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
            fake.android
                .points
                .lock()
                .await
                .get("")
                .map(|s| s.window.as_str()),
            Some("7")
        );
        fake.dump("dump.xml", "9");
        fake.dump("dump-first.xml", "9");
        let err = fake.android.state(serde_json::json!({})).await.unwrap_err();
        assert_eq!(err, "the screen is changing; call computer_state again");
    }

    #[tokio::test]
    async fn a_session_end_forgets_only_that_sessions_elements() {
        let android = Android::new(Device {
            adb: "/nonexistent/adb".into(),
            serial: None,
            ui_jar: "/u.jar".into(),
            scrcpy_jar: "/s.jar".into(),
            su: None,
        });
        {
            let mut points = android.points.lock().await;
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
        let points = android.points.lock().await;
        assert!(!points.contains_key("cu-1"));
        assert_eq!(points.get("cu-2").map(|s| &s.points), Some(&vec![(30, 40)]));
    }

    #[tokio::test]
    async fn an_end_without_a_session_forgets_the_default_one_and_ends_are_idempotent() {
        let android = Android::new(Device {
            adb: "/nonexistent/adb".into(),
            serial: None,
            ui_jar: "/u.jar".into(),
            scrcpy_jar: "/s.jar".into(),
            su: None,
        });
        {
            let mut points = android.points.lock().await;
            points.insert(String::new(), saved(vec![(10, 20)]));
            points.insert("cu-2".into(), saved(vec![(30, 40)]));
        }
        let end = |args| {
            let android = &android;
            async move { android.end(&args).await.unwrap() }
        };
        assert_eq!(end(serde_json::json!({})).await, "ended");
        assert!(!android.points.lock().await.contains_key(""));
        assert_eq!(
            end(serde_json::json!({"session": "unknown"})).await,
            "ended"
        );
        assert_eq!(end(serde_json::json!({"session": "cu-2"})).await, "ended");
        assert_eq!(end(serde_json::json!({"session": "cu-2"})).await, "ended");
        assert!(android.points.lock().await.is_empty());
    }

    #[test]
    fn clipboard_message_is_scrcpy_set_clipboard_without_its_paste_key() {
        let msg = clipboard_message("你好", 7);
        assert_eq!(msg[0], 9);
        assert_eq!(&msg[1..9], &7u64.to_be_bytes());
        assert_eq!(msg[9], 0);
        assert_eq!(&msg[10..14], &6u32.to_be_bytes());
        assert_eq!(&msg[14..], "你好".as_bytes());
    }

    #[tokio::test]
    async fn the_paste_takes_only_its_own_acknowledgement() {
        let ack = |sequence: u64| {
            let mut msg = vec![ACK_CLIPBOARD];
            msg.extend_from_slice(&sequence.to_be_bytes());
            msg
        };
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
    async fn session(adb: &str) -> (Android, TcpStream) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (peer, _) = listener.accept().await.unwrap();
        let android = Android::new(Device {
            adb: adb.into(),
            serial: None,
            ui_jar: "/u.jar".into(),
            scrcpy_jar: "/s.jar".into(),
            su: Some("/su".into()),
        });
        *android.control.lock().await = Some(Control {
            stream,
            sequence: 0,
            _server: Command::new("sleep")
                .arg("30")
                .kill_on_drop(true)
                .spawn()
                .unwrap(),
            _forward: Forward {
                port: String::new(),
                remove: None,
            },
        });
        (android, peer)
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
        tokio::time::sleep(Duration::from_millis(50)).await;
        let err = android.paste("hi").await.unwrap_err();
        assert!(err.contains("run adb"), "{err}");
        let mut sent = Vec::new();
        peer.read_to_end(&mut sent).await.unwrap();
        assert!(sent.is_empty(), "{sent:?}");
    }

    #[cfg(unix)]
    async fn paste_through(
        android: &Android,
        mut peer: TcpStream,
        mut ui: Command,
    ) -> Result<bool, String> {
        let mut ack = vec![ACK_CLIPBOARD];
        ack.extend_from_slice(&1u64.to_be_bytes());
        peer.write_all(&ack).await.unwrap();
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
            _child: ui,
        });
        let pasted = android.paste("hi").await;
        assert_eq!(device.await.unwrap().0, clipboard_message("hi", 1));
        pasted
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

    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn the_focused_window_diagnostic_gives_up_after_a_second() {
        let dir = tempfile::tempdir().unwrap();
        let adb = dir.path().join("adb");
        std::fs::write(&adb, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&adb, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
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
    async fn paste_keys_refused_until_the_deadline_fail_as_not_injected() {
        let pastes = std::cell::Cell::new(0);
        let err = type_with(
            "abc",
            None,
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
    async fn a_paste_that_fails_is_not_pasted_again() {
        let pastes = std::cell::Cell::new(0);
        let err = type_with(
            "abc",
            None,
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

    fn field(text: &str) -> Focus {
        let n = text.chars().count();
        Focus {
            editable: true,
            window: true,
            bounds: [100, 80, 620, 180],
            len: text.encode_utf16().count(),
            hash: text.len() as i32,
            tail: text.chars().skip(n.saturating_sub(32)).collect(),
            ..Focus::default()
        }
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

    #[test]
    fn the_ui_servers_focus_answer_decodes_and_null_is_no_focus() {
        let f: Option<Focus> = serde_json::from_str(
            r#"{"editable":true,"password":false,"window":true,"len":6,"hash":-1,"tail":"ab 你好","class":"android.widget.EditText","package":"com.android.settings","bounds":[100,80,620,180]}"#,
        )
        .unwrap();
        let f = f.unwrap();
        assert!(f.editable && f.window && !f.password && f.holds((360, 136)));
        assert!(
            !f.holds((360, 180))
                && f.ends_with("你好")
                && f.ends_with("b 你好")
                && !f.ends_with("x ab 你好")
        );
        let none: Option<Focus> = serde_json::from_str("null").unwrap();
        assert!(none.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_type_that_shows_is_pasted_once_and_verified() {
        assert_eq!(typed("你好", None, "", &["你好"]).await, (Some(true), 1));
        assert_eq!(typed("abc", None, "x ", &["x abc"]).await, (Some(true), 1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_paste_that_left_the_field_unchanged_is_pasted_again_until_the_deadline() {
        assert_eq!(typed("abc", None, "", &["", "abc"]).await, (Some(true), 2));
        assert_eq!(
            typed("abc", None, "", &["", "", "abc"]).await,
            (Some(true), 3)
        );
        assert_eq!(typed("abc", None, "", &[""]).await, (Some(false), 5));
    }

    #[tokio::test(start_paused = true)]
    async fn a_lagging_cached_answer_is_read_live_before_a_repeat() {
        let pastes = std::cell::Cell::new(0);
        let out = type_with(
            "abc",
            None,
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
        assert_eq!((out, pastes.get()), (Some(false), 5));
    }

    #[tokio::test(start_paused = true)]
    async fn a_field_that_already_ends_with_the_line_shows_the_paste_only_once_it_changes() {
        assert_eq!(typed("bc", None, "abc", &["abc"]).await, (Some(false), 5));
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
            |_| async {
                Ok(Some(Focus {
                    password: true,
                    ..field("")
                }))
            },
            || {
                pastes.set(pastes.get() + 1);
                async { Ok(true) }
            },
        )
        .await
        .unwrap();
        assert_eq!((password, pastes.get()), (None, 1));
        let started = tokio::time::Instant::now();
        let none = type_with("abc", None, |_| async { Ok(None) }, || async { Ok(true) })
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
}
