//! Screenshot, UI elements and input on an Android box through adb, with text pasted by a scrcpy control session.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use super::rpc::{self, Output};

const ADB_TIMEOUT: Duration = Duration::from_secs(30);
const SETTLE: Duration = Duration::from_millis(500);
const MAX_WAIT: f64 = 10.0;
const MAX_ELEMENTS: usize = 200;
const LONG_PRESS_MS: u32 = 800;
const SWIPE_MS: u32 = 300;
const SCROLL_FRACTION: i32 = 3;
const SCREEN: (i32, i32) = (720, 1280);
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
const KEYS: [(&str, u32); 14] = [
    ("back", 4),
    ("home", 3),
    ("enter", 66),
    ("return", 66),
    ("tab", 61),
    ("delete", 67),
    ("backspace", 67),
    ("escape", 111),
    ("recents", 187),
    ("menu", 82),
    ("up", 19),
    ("down", 20),
    ("left", 21),
    ("right", 22),
];

/// adb on the box's Android, with the element rects of each session's latest computer_state.
pub struct Android {
    adb: PathBuf,
    rects: Mutex<HashMap<String, Vec<Rect>>>,
}

impl Android {
    /// Drives Android with the adb program `adb`, which finds the device by `ANDROID_SERIAL`.
    pub fn new(adb: PathBuf) -> Self {
        Self {
            adb,
            rects: Mutex::new(HashMap::new()),
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
        let xml = self.dump().await?;
        let mut elements = elements(&xml);
        if let Some(q) = args.query.as_deref().map(str::to_lowercase) {
            elements.retain(|e| e.label().to_lowercase().contains(&q));
        }
        elements.truncate(MAX_ELEMENTS);
        let text = elements
            .iter()
            .enumerate()
            .map(|(i, e)| e.line(i))
            .collect::<Vec<_>>()
            .join("\n");
        self.rects
            .lock()
            .await
            .insert(args.session, elements.into_iter().map(|e| e.rect).collect());
        let mut out = self.look().await?;
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
        let point = self.point(&args).await;
        let report = match args.action.as_str() {
            "tap" | "click" => {
                let (x, y) = point?;
                self.input(&["tap", &x.to_string(), &y.to_string()]).await?;
                format!("tapped {x},{y}")
            }
            "double_tap" | "double_click" => {
                let (x, y) = point?;
                for _ in 0..2 {
                    self.input(&["tap", &x.to_string(), &y.to_string()]).await?;
                }
                format!("double-tapped {x},{y}")
            }
            "long_press" => {
                let (x, y) = point?;
                self.swipe((x, y), (x, y), LONG_PRESS_MS).await?;
                format!("long-pressed {x},{y}")
            }
            "swipe" | "drag" => {
                let from = point?;
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
                if let Ok((x, y)) = point {
                    self.input(&["tap", &x.to_string(), &y.to_string()]).await?;
                }
                self.paste(text).await?;
                format!("typed {} characters", text.chars().count())
            }
            "key" => {
                let key = args.key.as_deref().ok_or("key needs key")?;
                let code = keycode(key).ok_or_else(|| format!("unknown key {key}"))?;
                self.input(&["keyevent", &code.to_string()]).await?;
                format!("pressed {key}")
            }
            "launch_app" => {
                let app = args.app.as_deref().ok_or("launch_app needs app")?;
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
                format!("launched {component}")
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

    /// Forgets the element rects of a session.
    pub async fn end(&self, args: &Value) -> Result<String, String> {
        if let Some(session) = args["session"].as_str() {
            self.rects.lock().await.remove(session);
        }
        Ok("ended".to_owned())
    }

    async fn point(&self, args: &ActArgs) -> Result<(i32, i32), String> {
        if let Some(i) = args.element {
            let rects = self.rects.lock().await;
            let rect = rects
                .get(&args.session)
                .and_then(|r| r.get(i))
                .ok_or_else(|| format!("no element [{i}] in the latest computer_state"))?;
            return Ok(rect.center());
        }
        match (args.x, args.y) {
            (Some(x), Some(y)) => Ok((x as i32, y as i32)),
            _ => Err(format!("{} needs element or x,y", args.action)),
        }
    }

    async fn swipe(&self, from: (i32, i32), to: (i32, i32), ms: u32) -> Result<(), String> {
        let args = [from.0, from.1, to.0, to.1].map(|v| v.to_string());
        let ms = ms.to_string();
        self.input(&["swipe", &args[0], &args[1], &args[2], &args[3], &ms])
            .await
    }

    async fn input(&self, args: &[&str]) -> Result<(), String> {
        let mut argv = vec!["shell", "input"];
        argv.extend_from_slice(args);
        self.adb(&argv).await.map(drop)
    }

    /// `uiautomator dump` of the screen, retried once while the screen animates.
    async fn dump(&self) -> Result<String, String> {
        let mut last = String::new();
        for _ in 0..2 {
            let out = self
                .adb(&["exec-out", "uiautomator", "dump", "/dev/tty"])
                .await?;
            let text = String::from_utf8_lossy(&out);
            if let (Some(start), Some(end)) = (text.find("<hierarchy"), text.rfind("</hierarchy>"))
            {
                return Ok(text[start..end].to_owned());
            }
            last = text.trim().to_owned();
        }
        Err(format!("uiautomator dump: {last}"))
    }

    /// Pastes `text` into the focused field through the clipboard, which takes any Unicode, unlike key injection.
    async fn paste(&self, text: &str) -> Result<(), String> {
        self.adb(&["push", SCRCPY_JAR, DEVICE_JAR]).await?;
        self.adb(&["push", SCRCPY_SU, DEVICE_SU]).await?;
        let socket = format!("localabstract:scrcpy_{SCRCPY_SCID}");
        let port = String::from_utf8_lossy(&self.adb(&["forward", "tcp:0", &socket]).await?)
            .trim()
            .to_owned();
        let server = self.scrcpy_control();
        let pasted = match server {
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

    async fn adb(&self, args: &[&str]) -> Result<Vec<u8>, String> {
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

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Rect {
    fn center(self) -> (i32, i32) {
        ((self.left + self.right) / 2, (self.top + self.bottom) / 2)
    }
}

/// One node of a uiautomator dump that shows text or takes input.
#[derive(Debug, PartialEq)]
struct Element {
    class: String,
    text: String,
    desc: String,
    id: String,
    clickable: bool,
    editable: bool,
    checked: Option<bool>,
    rect: Rect,
}

impl Element {
    fn label(&self) -> &str {
        if self.text.is_empty() {
            &self.desc
        } else {
            &self.text
        }
    }

    fn line(&self, i: usize) -> String {
        let class = self.class.rsplit('.').next().unwrap_or_default();
        let (x, y) = self.rect.center();
        let mut line = format!("[{i}] {class} {:?} at {x},{y}", self.label());
        if !self.id.is_empty() {
            line.push_str(&format!(
                " id={}",
                self.id.rsplit('/').next().unwrap_or_default()
            ));
        }
        if self.clickable {
            line.push_str(" clickable");
        }
        if self.editable {
            line.push_str(" editable");
        }
        if let Some(checked) = self.checked {
            line.push_str(if checked { " checked" } else { " unchecked" });
        }
        line
    }
}

/// The elements of a uiautomator dump in document order: nodes with text, a description, or input.
fn elements(xml: &str) -> Vec<Element> {
    xml.split("<node ")
        .skip(1)
        .filter_map(|node| {
            let attr = |key: &str| attribute(node, key).unwrap_or_default();
            let text = attr("text");
            let desc = attr("content-desc");
            let clickable = attr("clickable") == "true" || attr("long-clickable") == "true";
            let class = attr("class");
            let editable = class.ends_with("EditText");
            if text.is_empty() && desc.is_empty() && !clickable && !editable {
                return None;
            }
            let rect = bounds(&attr("bounds"))?;
            (rect.right > rect.left && rect.bottom > rect.top).then(|| Element {
                text,
                desc,
                id: attr("resource-id"),
                clickable,
                editable,
                checked: (attr("checkable") == "true").then(|| attr("checked") == "true"),
                rect,
                class,
            })
        })
        .collect()
}

/// The unescaped value of `key="..."` in the attributes of one node.
fn attribute(node: &str, key: &str) -> Option<String> {
    let head = node.split('>').next()?;
    let start = head
        .find(&format!(" {key}=\""))
        .map(|i| i + key.len() + 3)
        .or_else(|| {
            head.strip_prefix(&format!("{key}=\""))
                .map(|_| key.len() + 2)
        })?;
    let len = head[start..].find('"')?;
    Some(unescape(&head[start..start + len]))
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';') else { break };
        let entity = &rest[1..end];
        let ch = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `[l,t][r,b]` of a uiautomator node.
fn bounds(value: &str) -> Option<Rect> {
    let nums: Vec<i32> = value
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .filter(|s| !s.is_empty())
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    match nums[..] {
        [left, top, right, bottom] => Some(Rect {
            left,
            top,
            right,
            bottom,
        }),
        _ => None,
    }
}

/// The `package/activity` line of `cmd package resolve-activity --brief` for `app`.
fn launcher(out: &str, app: &str) -> Option<String> {
    out.lines()
        .map(str::trim)
        .find(|l| l.starts_with(&format!("{app}/")))
        .map(str::to_owned)
}

fn keycode(key: &str) -> Option<u32> {
    let key = key.to_lowercase();
    KEYS.iter()
        .find(|(name, _)| *name == key)
        .map(|(_, code)| *code)
        .or_else(|| key.parse().ok())
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

    const DUMP: &str = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation="0"><node index="0" text="" resource-id="" class="android.widget.FrameLayout" package="p" content-desc="" checkable="false" checked="false" clickable="false" long-clickable="false" bounds="[0,0][720,1280]"><node index="0" text="Wi&#8209;Fi &amp; more" resource-id="android:id/title" class="android.widget.TextView" package="p" content-desc="" checkable="false" checked="false" clickable="false" long-clickable="false" bounds="[160,470][490,510]" /><node index="1" text="" resource-id="com.android.settings:id/switch_widget" class="android.widget.Switch" package="p" content-desc="Dark theme" checkable="true" checked="true" clickable="true" long-clickable="false" bounds="[600,500][680,560]" /><node index="2" text="" resource-id="android:id/search_src_text" class="android.widget.EditText" package="p" content-desc="" checkable="false" checked="false" clickable="false" long-clickable="false" bounds="[40,100][680,180]" /><node index="3" text="hidden" resource-id="" class="android.widget.TextView" package="p" content-desc="" checkable="false" checked="false" clickable="false" long-clickable="false" bounds="[0,0][0,0]" /></node></hierarchy>"#;

    #[test]
    fn elements_keep_labelled_and_input_nodes_with_their_centers() {
        let got = elements(DUMP);
        let lines: Vec<String> = got.iter().enumerate().map(|(i, e)| e.line(i)).collect();
        assert_eq!(
            lines,
            [
                "[0] TextView \"Wi\u{2011}Fi & more\" at 325,490 id=title",
                "[1] Switch \"Dark theme\" at 640,530 id=switch_widget clickable checked",
                "[2] EditText \"\" at 360,140 id=search_src_text editable",
            ]
        );
    }

    #[test]
    fn bounds_and_entities_parse() {
        assert_eq!(
            bounds("[1,2][3,4]"),
            Some(Rect {
                left: 1,
                top: 2,
                right: 3,
                bottom: 4
            })
        );
        assert_eq!(bounds("[1,2]"), None);
        assert_eq!(
            unescape("a&lt;b&gt;&quot;&apos;&#20320;&#x597D;&bogus"),
            "a<b>\"'你好&bogus"
        );
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
    fn keys_map_to_android_keycodes() {
        assert_eq!(keycode("Back"), Some(4));
        assert_eq!(keycode("enter"), Some(66));
        assert_eq!(keycode("82"), Some(82));
        assert_eq!(keycode("hyper"), None);
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
