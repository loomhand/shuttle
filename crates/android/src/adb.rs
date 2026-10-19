//! Screenshot, UI elements and input on an Android device through adb, with text pasted by a scrcpy control session.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
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
const DEVICE_JAR: &str = "/data/local/tmp/cb-scrcpy.jar";
const DEVICE_SU: &str = "/data/local/tmp/su1000";
const SCRCPY_VERSION: &str = "4.1";
const SCRCPY_SCID: &str = "0cb0c11b";
const SET_CLIPBOARD: u8 = 9;
const CONNECT_TRIES: u32 = 50;
const CONNECT_PAUSE: Duration = Duration::from_millis(100);
const ACK_CLIPBOARD: u8 = 1;
const ACK_TIMEOUT: Duration = Duration::from_secs(2);
const DEVICE_UI_JAR: &str = "/data/local/tmp/cb-ui.jar";
const UI_TIMEOUT: Duration = Duration::from_secs(10);
const UI_END: &str = "<<end>>";
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

/// adb on an Android device, with the element centres of each session's latest computer_state.
pub struct Android {
    device: Device,
    points: Mutex<HashMap<String, Vec<(i32, i32)>>>,
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
        let screen = self.screen().await?;
        let (xml, shot) = tokio::join!(self.tree(None), self.look());
        let mut elements = desk::elements(&xml?, screen);
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
        self.points
            .lock()
            .await
            .insert(args.session, elements.iter().map(Node::center).collect());
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
                self.paste(text).await?;
                format!("typed {} characters", text.chars().count())
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

    /// Pastes `text` into the focused field through the clipboard, which takes any Unicode, unlike key injection.
    pub(crate) async fn paste(&self, text: &str) -> Result<(), String> {
        let mut slot = self.control.lock().await;
        let mut control = match slot.take() {
            Some(control) if control.is_open() => control,
            _ => self.open_control().await?,
        };
        control.paste(text).await?;
        *slot = Some(control);
        Ok(())
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
        self.points
            .lock()
            .await
            .get(&args.session)
            .and_then(|p| p.get(i))
            .copied()
            .map(Some)
            .ok_or_else(|| format!("no element [{i}] in the latest computer_state"))
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
        let _ = self.adb(&["shell", "pkill", "-f", "cb.Ui"]).await;
        let mut cmd = self.command();
        cmd.args([
            "shell",
            &format!("CLASSPATH={DEVICE_UI_JAR}"),
            "app_process",
            "/",
            "cb.Ui",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| format!("run UI server: {e}"))?;
        let stdin = child.stdin.take().ok_or("take UI server stdin")?;
        let mut stdout = BufReader::new(child.stdout.take().ok_or("take UI server stdout")?);
        let mut ready = String::new();
        tokio::time::timeout(UI_TIMEOUT, stdout.read_line(&mut ready))
            .await
            .map_err(|_| "UI server start timed out".to_owned())?
            .map_err(|e| format!("read UI server: {e}"))?;
        if ready.trim() != "ready" {
            return Err(format!("UI server said {:?}", ready.trim()));
        }
        Ok(UiServer {
            _child: child,
            stdin,
            stdout,
        })
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

/// The image's UI server (`image/android/ui`) in Android, one answer per request line; dropping it stops the server.
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

    async fn paste(&mut self, text: &str) -> Result<(), String> {
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

/// A scrcpy SET_CLIPBOARD message that pastes `text` and asks for an acknowledgement of `sequence`.
fn clipboard_message(text: &str, sequence: u64) -> Vec<u8> {
    let mut msg = Vec::with_capacity(14 + text.len());
    msg.push(SET_CLIPBOARD);
    msg.extend_from_slice(&sequence.to_be_bytes());
    msg.push(1);
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
        let dir = std::env::temp_dir().join(format!("shuttle-adb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let adb = dir.join("adb");
        let argv = dir.join("argv");
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
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(args.lines().last(), Some("'com.example/.Main$Launcher'"));
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
            points.insert("cu-1".into(), vec![(10, 20)]);
            points.insert("cu-2".into(), vec![(30, 40)]);
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
        assert_eq!(points.get("cu-2"), Some(&vec![(30, 40)]));
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
            points.insert(String::new(), vec![(10, 20)]);
            points.insert("cu-2".into(), vec![(30, 40)]);
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
    fn clipboard_message_is_scrcpy_set_clipboard_with_paste() {
        let msg = clipboard_message("你好", 7);
        assert_eq!(msg[0], 9);
        assert_eq!(&msg[1..9], &7u64.to_be_bytes());
        assert_eq!(msg[9], 1);
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
}
