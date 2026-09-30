//! cua-driver client: line-delimited JSON over its unix socket.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use super::rpc::Output;

const STATE_ELEMENTS: u64 = 300;
const TREE_CHARS: usize = 15_000;
const MIN_WINDOW: i64 = 10;
const SHELL_APPS: [&str; 2] = ["Xfce4-panel", "Xfdesktop"];
const SETTLE: Duration = Duration::from_millis(600);
const MAX_WAIT_SECS: f64 = 10.0;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_SESSION: &str = "cocoonbox";

/// Screen executor client; remembers the element tokens of the latest window state.
pub struct Cua {
    socket: PathBuf,
    last: Mutex<Option<Snapshot>>,
}

impl Cua {
    /// Creates a client for the driver socket at `socket`.
    pub fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            last: Mutex::new(None),
        }
    }

    /// `computer_state`: window list, accessibility elements and a desktop screenshot.
    pub async fn state(&self, args: Value) -> Result<Output, String> {
        let session = session_of(&args);
        let a: StateArgs =
            serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))?;
        let desktop = self
            .request(&session, "get_desktop_state", json!({}))
            .await?;
        let listed = window_list(&desktop);
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
                    let (tree, tokens) = element_tree(&state);
                    let window_id = state["structuredContent"]["window_id"]
                        .as_i64()
                        .or(window_id);
                    let _ = writeln!(text, "\nElements of pid={pid} (center = screen pixels):");
                    text.push_str(&tree);
                    *self.last.lock().await = Some(Snapshot {
                        pid,
                        window_id,
                        tokens,
                    });
                }
                Err(e) => {
                    let _ = writeln!(text, "\nElements of pid={pid} unavailable: {e}");
                }
            }
        }
        Ok(Output {
            text,
            is_error: false,
            image_b64: content_image(&desktop),
        })
    }

    /// `computer_act`: one input action followed by a fresh desktop screenshot.
    pub async fn act(&self, args: Value) -> Result<Output, String> {
        let session = session_of(&args);
        let a: ActArgs =
            serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))?;
        if a.action == "wait" {
            let secs = a.seconds.unwrap_or(1.0).clamp(0.1, MAX_WAIT_SECS);
            tokio::time::sleep(Duration::from_secs_f64(secs)).await;
            return Ok(Output {
                text: format!("waited {secs:.1}s"),
                is_error: false,
                image_b64: self.screenshot(&session).await?,
            });
        }
        let (tool, req) = self.action_request(&session, &a).await?;
        let result = self.request(&session, tool, req).await;
        tokio::time::sleep(SETTLE).await;
        let shot = self.screenshot(&session).await?;
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

    /// Base64 PNG of the whole desktop.
    pub async fn screenshot(&self, session: &str) -> Result<Option<String>, String> {
        let v = self
            .request(session, "get_desktop_state", json!({}))
            .await?;
        Ok(content_image(&v))
    }

    /// Ends a driver session, releasing its input devices.
    pub async fn end(&self, args: Value) -> Result<String, String> {
        let session = session_of(&args);
        self.request(&session, "end_session", json!({})).await?;
        *self.last.lock().await = None;
        Ok(format!("ended session {session}"))
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
                if let Some(b) = &a.button {
                    req["button"] = b.as_str().into();
                }
                match a.action.as_str() {
                    "double_click" => "double_click",
                    "right_click" => "right_click",
                    _ => "click",
                }
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
            "launch_app" => {
                req["name"] = required(&a.app, "app")?.into();
                return Ok(("launch_app", req));
            }
            other => return Err(format!("unknown action {other}")),
        };
        if let Some(index) = a.element {
            let last = self.last.lock().await;
            let snap = last
                .as_ref()
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
        } else {
            let windows = self
                .request(session, "list_windows", json!({"on_screen_only": true}))
                .await?;
            if let Some(w) = window_list(&windows)
                .iter()
                .find(|w| !SHELL_APPS.contains(&w.app.as_str()))
            {
                req["pid"] = w.pid.into();
                req["window_id"] = w.window_id.into();
            }
        }
        if a.foreground {
            req["delivery_mode"] = "foreground".into();
        }
        Ok((tool, req))
    }

    async fn request(&self, session: &str, name: &str, args: Value) -> Result<Value, String> {
        let exchange = async {
            let mut stream = UnixStream::connect(&self.socket)
                .await
                .map_err(|e| format!("connect cua-driver {}: {e}", self.socket.display()))?;
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
            serde_json::from_str::<Value>(&reply)
                .map_err(|e| format!("decode cua-driver reply: {e}"))
        };
        let mut v = tokio::time::timeout(REQUEST_TIMEOUT, exchange)
            .await
            .map_err(|_| format!("cua-driver {name} timed out"))??;
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

/// The driver session label carried in `args`.
pub fn session_of(args: &Value) -> String {
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

fn element_tree(result: &Value) -> (String, HashMap<i64, String>) {
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
            let _ = write!(tree, " center=({:.0},{:.0})", x + w / 2.0, y + h / 2.0);
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

fn required<'a>(v: &'a Option<String>, name: &str) -> Result<&'a str, String> {
    v.as_deref().ok_or_else(|| format!("missing {name}"))
}
