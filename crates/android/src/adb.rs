//! Screenshot, UI elements and input on an Android box through adb, with text pasted by a scrcpy control session.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use super::android_desk::{self, Node};
use super::rpc::{self, Output};

pub(super) const SCREEN: (i32, i32) = (720, 1280);

const ADB_TIMEOUT: Duration = Duration::from_secs(30);
const SETTLE: Duration = Duration::from_millis(500);
const MAX_WAIT: f64 = 10.0;
const MAX_ELEMENTS: usize = 200;
const LONG_PRESS_MS: u32 = 800;
const SWIPE_MS: u32 = 300;
const SCROLL_FRACTION: i32 = 3;
const SCRCPY_JAR: &str = "/usr/local/share/scrcpy/scrcpy-server.jar";
const SCRCPY_SU: &str = "/usr/local/share/scrcpy/su1000";
const DEVICE_JAR: &str = "/data/local/tmp/cb-scrcpy.jar";
const DEVICE_SU: &str = "/data/local/tmp/su1000";
const SCRCPY_VERSION: &str = "4.1";
const SCRCPY_SCID: &str = "0cb0c11b";
const SET_CLIPBOARD: u8 = 9;
const CONNECT_TRIES: u32 = 50;
const CONNECT_PAUSE: Duration = Duration::from_millis(100);
const PASTE_SETTLE: Duration = Duration::from_millis(300);
const UI_JAR: &str = "/usr/local/share/cocoonbox/cb-ui.jar";
const DEVICE_UI_JAR: &str = "/data/local/tmp/cb-ui.jar";
const UI_CLASSPATH: &str = "CLASSPATH=/system/framework/uiautomator.jar:/data/local/tmp/cb-ui.jar";
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

/// adb on the box's Android, with the element centres of each session's latest computer_state.
pub struct Android {
    adb: PathBuf,
    points: Mutex<HashMap<String, Vec<(i32, i32)>>>,
    ui: Mutex<Option<UiServer>>,
}

impl Android {
    /// Drives Android with the adb program `adb`, which finds the device by `ANDROID_SERIAL`.
    pub fn new(adb: PathBuf) -> Self {
        Self {
            adb,
            points: Mutex::new(HashMap::new()),
            ui: Mutex::new(None),
        }
    }

    /// The screen as PNG.
    pub async fn look(&self) -> Result<Output, String> {
        let png = self.adb(&["exec-out", "screencap", "-p"]).await?;
        Ok(Output {
            text: format!("{}x{} screen", SCREEN.0, SCREEN.1),
            image_b64: Some(B64.encode(png)),
            ..Output::default()
        })
    }

    /// The UI elements of the screen, [index] for computer_act, and a screenshot.
    pub async fn state(&self, args: Value) -> Result<Output, String> {
        let args: StateArgs = rpc::parse(args)?;
        let (xml, shot) = tokio::join!(self.tree(None), self.look());
        let mut elements = android_desk::elements(&xml?);
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
        let args: ActArgs = rpc::parse(args)?;
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
                let (x, y) = point.unwrap_or((SCREEN.0 / 2, SCREEN.1 / 2));
                let (dx, dy) = match args.direction.as_deref().unwrap_or("down") {
                    "up" => (0, SCREEN.1 / SCROLL_FRACTION),
                    "down" => (0, -SCREEN.1 / SCROLL_FRACTION),
                    "left" => (SCREEN.0 / SCROLL_FRACTION, 0),
                    "right" => (-SCREEN.0 / SCROLL_FRACTION, 0),
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

    /// Forgets the element centres of a session.
    pub async fn end(&self, args: &Value) -> Result<String, String> {
        if let Some(session) = args["session"].as_str() {
            self.points.lock().await.remove(session);
        }
        Ok("ended".to_owned())
    }

    /// Starts the launcher activity of package `app`; its component.
    pub(super) async fn launch(&self, app: &str) -> Result<String, String> {
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
            .ok_or_else(|| format!("launch {app}: no launcher activity"))?;
        self.adb(&["shell", "am", "start", "-W", "-n", &component])
            .await?;
        Ok(component)
    }

    pub(super) async fn tap(&self, (x, y): (i32, i32)) -> Result<(), String> {
        self.input(&["tap", &x.to_string(), &y.to_string()]).await
    }

    /// Presses a key or a combination such as `ctrl+a`.
    pub(super) async fn keys(&self, keys: &str) -> Result<(), String> {
        let codes: Vec<String> = keycodes(keys)?.iter().map(u32::to_string).collect();
        let mut argv = vec![if codes.len() == 1 {
            "keyevent"
        } else {
            "keycombination"
        }];
        argv.extend(codes.iter().map(String::as_str));
        self.input(&argv).await
    }

    pub(super) async fn input(&self, args: &[&str]) -> Result<(), String> {
        let mut argv = vec!["shell", "input"];
        argv.extend_from_slice(args);
        self.adb(&argv).await.map(drop)
    }

    /// The UI tree of window `window`, else of the active one, from the image's UI server.
    pub(super) async fn tree(&self, window: Option<&str>) -> Result<String, String> {
        match window {
            Some(w) => self.served(&format!("dump {w}")).await,
            None => self.served("dump").await,
        }
    }

    /// The interactive windows as the UI server lists them, as JSON.
    pub(super) async fn windows(&self) -> Result<String, String> {
        self.served("windows").await
    }

    /// Pastes `text` into the focused field through the clipboard, which takes any Unicode, unlike key injection.
    pub(super) async fn paste(&self, text: &str) -> Result<(), String> {
        self.adb(&["push", SCRCPY_JAR, DEVICE_JAR]).await?;
        self.adb(&["push", SCRCPY_SU, DEVICE_SU]).await?;
        let socket = format!("localabstract:scrcpy_{SCRCPY_SCID}");
        let port = String::from_utf8_lossy(&self.adb(&["forward", "tcp:0", &socket]).await?)
            .trim()
            .to_owned();
        let pasted = match self.scrcpy_control() {
            Ok(child) => {
                let pasted = send_clipboard(&port, text).await;
                drop(child);
                pasted
            }
            Err(e) => Err(e),
        };
        let _ = self
            .adb(&["shell", "pkill", "-f", &format!("scid={SCRCPY_SCID}")])
            .await;
        let removed = self
            .adb(&["forward", "--remove", &format!("tcp:{port}")])
            .await;
        pasted?;
        removed.map(drop)
    }

    pub(super) async fn adb(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        let mut cmd = Command::new(&self.adb);
        cmd.args(args).stdin(Stdio::null()).kill_on_drop(true);
        crate::sys::unprotect(&mut cmd);
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
        self.adb(&["push", UI_JAR, DEVICE_UI_JAR]).await?;
        let _ = self.adb(&["shell", "pkill", "-f", "cb.Ui"]).await;
        let mut cmd = Command::new(&self.adb);
        cmd.args(["shell", UI_CLASSPATH, "app_process", "/", "cb.Ui"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        crate::sys::unprotect(&mut cmd);
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

    /// A control-only scrcpy server as the system uid, which may set the clipboard; dropping the child stops it.
    fn scrcpy_control(&self) -> Result<Child, String> {
        let mut cmd = Command::new(&self.adb);
        cmd.args([
            "shell",
            &format!("CLASSPATH={DEVICE_JAR}"),
            DEVICE_SU,
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
        crate::sys::unprotect(&mut cmd);
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

/// A scrcpy SET_CLIPBOARD message that pastes `text`.
fn clipboard_message(text: &str) -> Vec<u8> {
    let mut msg = Vec::with_capacity(14 + text.len());
    msg.push(SET_CLIPBOARD);
    msg.extend_from_slice(&0u64.to_be_bytes());
    msg.push(1);
    msg.extend_from_slice(&(text.len() as u32).to_be_bytes());
    msg.extend_from_slice(text.as_bytes());
    msg
}

/// Sends `text` to the scrcpy control socket that adb forwards to `port`, once the server's first byte shows it listens.
async fn send_clipboard(port: &str, text: &str) -> Result<(), String> {
    let addr = format!("127.0.0.1:{port}");
    for _ in 0..CONNECT_TRIES {
        if let Ok(mut stream) = TcpStream::connect(&addr).await {
            let mut ready = [0u8; 1];
            if matches!(stream.read(&mut ready).await, Ok(1)) {
                stream
                    .write_all(&clipboard_message(text))
                    .await
                    .map_err(|e| format!("send clipboard: {e}"))?;
                tokio::time::sleep(PASTE_SETTLE).await;
                return Ok(());
            }
        }
        tokio::time::sleep(CONNECT_PAUSE).await;
    }
    Err("scrcpy control did not start".to_owned())
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
    fn clipboard_message_is_scrcpy_set_clipboard_with_paste() {
        let msg = clipboard_message("你好");
        assert_eq!(msg[0], 9);
        assert_eq!(&msg[1..9], &[0; 8]);
        assert_eq!(msg[9], 1);
        assert_eq!(&msg[10..14], &6u32.to_be_bytes());
        assert_eq!(&msg[14..], "你好".as_bytes());
    }
}
