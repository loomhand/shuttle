//! Recordings of what the user does: each click and key with what the desk saw then, for a draft of replayable steps.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use desk_core::wire::Call;
use serde::Deserialize;
use serde_json::{Value, json};
use shuttle_desk::server::Handler;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio::time::Instant;

#[cfg(target_os = "macos")]
use crate::tap::{self, Tap};

const SETTLE: Duration = Duration::from_millis(700);
const IDLE: Duration = Duration::from_secs(3600);
const CROP_PT: [f64; 2] = [200.0, 100.0];
const ROOT_ROLES: [&str; 5] = ["window", "frame", "application", "dialog", "document"];
const BLOCKED: [&str; 10] = [
    "1Password",
    "1Password 7",
    "Bitwarden",
    "Dashlane",
    "KeePassXC",
    "Keychain Access",
    "LastPass",
    "Passwords",
    "Shuttle",
    "System Settings",
];
const AGENT: &str = "record";
#[cfg(target_os = "macos")]
const INPUT_MONITORING: &str = "recording needs Input Monitoring: allow Shuttle under System Settings > Privacy & Security > Input Monitoring, then start again";
#[cfg(not(target_os = "macos"))]
const OTHER_OS: &str = "recording runs only on macOS";
const RECORDING: &str = "a recording is already running: stop it first";
const NOTHING: &str = "no recording is running, and none was made since the daemon started";
const GONE: &str = "the recording worker is gone";

/// One thing the user did, in desk coordinates, or a note.
#[derive(Debug)]
pub enum Input {
    /// A click, with the element under it as read before its app handled it, where the tap could read it.
    Click {
        x: f64,
        y: f64,
        right: bool,
        count: i64,
        mods: Vec<&'static str>,
        found: Option<Hit>,
    },
    /// A key and the process it went to; `secret` when it went to a password field, and then `text` is empty. The first key of a run of typing was `read` for its `focus`.
    Key {
        code: i64,
        text: String,
        mods: Vec<&'static str>,
        secret: bool,
        pid: Option<u32>,
        focus: Option<Field>,
        read: bool,
    },
    /// A password field of process `pid` took the keyboard focus, so the keys that follow reach no listener.
    Password {
        pid: Option<u32>,
        focus: Option<Field>,
    },
    Note(String),
}

/// The element with the keyboard focus, as a snapshot names it, the role of its window or sheet, the title of its window, whether it takes a password, and the address of its web page with its DOM id.
#[derive(Debug, Default, serde::Serialize)]
pub struct Field {
    pub role: &'static str,
    pub name: String,
    pub frame: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub window: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub secure: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub id: String,
}

/// An element under a click: its role and name as a snapshot names them, its help, its process, the role and name of its ancestors up to its window, innermost first, and the address of its web page with its DOM id.
#[derive(Debug, Default)]
pub struct Hit {
    pub role: &'static str,
    pub name: String,
    pub description: String,
    pub pid: Option<u32>,
    pub path: Vec<(&'static str, String)>,
    pub secret: bool,
    pub page: Option<(String, Option<String>)>,
}

/// The recordings of this daemon: at most one running, and the last one made.
pub struct Recorder {
    dir: PathBuf,
    queue: UnboundedSender<Session>,
    sessions: Mutex<Option<UnboundedReceiver<Session>>>,
    live: Mutex<Option<Live>>,
    ended: Mutex<Option<Ended>>,
}

impl Recorder {
    /// A recorder that saves each recording in `dir`, readable by this user only.
    pub fn new(dir: PathBuf) -> Self {
        let (queue, sessions) = unbounded_channel();
        Self {
            dir,
            queue,
            sessions: Mutex::new(Some(sessions)),
            live: Mutex::new(None),
            ended: Mutex::new(None),
        }
    }

    /// The sessions for the one worker that runs them; `None` after the first call.
    pub fn sessions(&self) -> Option<UnboundedReceiver<Session>> {
        lock(&self.sessions).take()
    }

    pub fn recording(&self) -> bool {
        lock(&self.live).is_some()
    }

    /// Starts recording the user's input, in `apps` alone when it names any; answers the recording's id.
    pub fn start(&self, apps: Vec<String>) -> Result<String, String> {
        let mut live = lock(&self.live);
        if live.is_some() {
            return Err(RECORDING.to_owned());
        }
        let (notes, inputs) = unbounded_channel();
        let tap = listen(notes.clone())?;
        let id = format!("rec-{}", now_ms());
        let path = self.path(&id);
        let (done, made) = oneshot::channel();
        self.queue
            .send(Session {
                id: id.clone(),
                apps,
                per_point: per_point(),
                path: path.clone(),
                inputs,
                done,
            })
            .map_err(|_| GONE.to_owned())?;
        *live = Some(Live {
            path,
            notes,
            made,
            _tap: tap,
        });
        Ok(id)
    }

    /// Adds the user's note to the running recording, where it is now.
    pub fn note(&self, text: String) -> Result<(), String> {
        lock(&self.live)
            .as_ref()
            .ok_or_else(|| NOTHING.to_owned())?
            .notes
            .send(Input::Note(text))
            .map_err(|_| GONE.to_owned())
    }

    /// Ends the running recording; the worker saves it once it has read the last input.
    pub fn end(&self) {
        let live = lock(&self.live).take();
        if let Some(Live { path, made, .. }) = live {
            *lock(&self.ended) = Some(Ended {
                path,
                made: Some(made),
            });
        }
    }

    pub fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Ends the running recording, or takes the last one, and answers it as saved.
    pub async fn stop(&self) -> Result<Value, String> {
        self.end();
        let made = {
            let mut ended = lock(&self.ended);
            let e = ended.as_mut().ok_or_else(|| NOTHING.to_owned())?;
            e.made.take().ok_or_else(|| e.path.clone())
        };
        match made {
            Ok(made) => made.await.map_err(|_| GONE.to_owned()),
            Err(path) => std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
                .map_err(|e| format!("read {}: {e}", path.display())),
        }
    }
}

/// A recording for the worker: its inputs until they end, where to save it and where to send it.
pub struct Session {
    id: String,
    apps: Vec<String>,
    per_point: f64,
    path: PathBuf,
    inputs: UnboundedReceiver<Input>,
    done: oneshot::Sender<Value>,
}

struct Live {
    path: PathBuf,
    notes: UnboundedSender<Input>,
    made: oneshot::Receiver<Value>,
    _tap: Listening,
}

struct Ended {
    path: PathBuf,
    made: Option<oneshot::Receiver<Value>>,
}

struct Frame {
    window: String,
    png: Vec<u8>,
    factor: f64,
    bounds: [f64; 4],
    at: Instant,
}

#[cfg(target_os = "macos")]
type Listening = Tap;
#[cfg(not(target_os = "macos"))]
type Listening = std::convert::Infallible;

/// Runs `s` against `desk` until its inputs end, then saves it and sends it on; input while `acting` says shuttle itself acts is no user's.
pub async fn work<H: Handler>(desk: &H, s: Session, acting: impl Fn() -> bool) {
    let Session {
        id,
        apps,
        per_point,
        path,
        mut inputs,
        done,
    } = s;
    let started = now_ms();
    let t0 = Instant::now();
    let mut windows = list(desk).await;
    let mut asked = Vec::new();
    light(desk, &windows, &apps, &mut asked).await;
    let mut front = front_of(&windows);
    let mut frame = framed(desk, &windows, front.as_ref(), &apps).await;
    let mut acted = t0;
    let mut settle: Option<Instant> = None;
    let mut events = Vec::new();
    loop {
        let wake = settle.unwrap_or_else(|| Instant::now() + IDLE);
        tokio::select! {
            input = inputs.recv() => {
                let Some(input) = input else { break };
                let t = t0.elapsed().as_millis();
                let event = match input {
                    Input::Note(text) => {
                        let mut e = json!({"t": t, "kind": "note"});
                        e["text"] = text.into();
                        events.push(e);
                        continue;
                    }
                    _ if acting() => continue,
                    Input::Click { x, y, right, count, mods, found } => {
                        let mut at = match found {
                            Some(h) => {
                                let mut at = json!({
                                    "found": {"role": h.role, "name": h.name, "description": h.description},
                                    "path": h.path.into_iter().map(|(role, name)| json!({"role": role, "name": name})).collect::<Vec<_>>(),
                                    "pid": h.pid,
                                    "secret": h.secret,
                                });
                                if let Some((url, id)) = h.page {
                                    at["web"] = json!({"url": url});
                                    if let Some(id) = id {
                                        at["web"]["id"] = id.into();
                                    }
                                }
                                at
                            }
                            None => call(desk, "desk_at", json!({"x": x.round() as i32, "y": y.round() as i32})).await.unwrap_or_default(),
                        };
                        let pid = at["pid"].as_u64();
                        if pid == Some(u64::from(std::process::id())) {
                            continue;
                        }
                        windows = list(desk).await;
                        light(desk, &windows, &apps, &mut asked).await;
                        let win = window_of(&windows, &at);
                        let app = win.and_then(|w| w["app"].as_str()).map(str::to_owned).or_else(|| app_of(&windows, pid));
                        if kept(app.as_deref(), &apps) {
                            let late = frame.as_ref().is_none_or(|f| f.at < acted || win.is_none_or(|w| w["id"] != f.window.as_str()));
                            if late {
                                frame = framed(desk, &windows, win, &apps).await;
                            }
                            let mut e = json!({
                                "t": t, "kind": "click", "x": x, "y": y, "button": if right { "right" } else { "left" }, "count": count, "mods": mods,
                                "app": app, "element": at["found"], "path": at["path"], "root": rooted(&at["found"]), "late": late,
                            });
                            if at["secret"] == true {
                                e["secret"] = true.into();
                            }
                            if at["web"].is_object() {
                                e["web"] = at["web"].take();
                            }
                            if let Some(w) = win {
                                e["window"] = json!({"title": w["title"], "bounds": w["bounds"]});
                            }
                            let mine = frame.as_ref().filter(|f| win.is_some_and(|w| w["id"] == f.window.as_str()));
                            if let Some((png, factor, offset)) = mine.and_then(|f| crop(f, x, y, per_point)) {
                                e["crop"] = png.into();
                                e["crop_scale"] = factor.into();
                                e["crop_offset"] = json!(offset);
                            }
                            e
                        } else {
                            json!({"t": t, "kind": "skipped"})
                        }
                    }
                    Input::Password { pid, focus } => {
                        windows = list(desk).await;
                        let app = app_of(&windows, pid.map(u64::from));
                        if app.is_some() && kept(app.as_deref(), &apps) {
                            json!({"t": t, "kind": "password", "app": app, "focus": focus})
                        } else {
                            json!({"t": t, "kind": "skipped"})
                        }
                    }
                    Input::Key {
                        code,
                        text,
                        mods,
                        secret,
                        pid,
                        focus,
                        read,
                    } => {
                        let pid = pid.map(u64::from);
                        if pid.is_some_and(|p| !windows.iter().any(|w| w["pid"].as_u64() == Some(p))) {
                            windows = list(desk).await;
                        }
                        let app = app_of(&windows, pid).or_else(|| front.as_ref().and_then(|w| w["app"].as_str()).map(str::to_owned));
                        if !kept(app.as_deref(), &apps) {
                            json!({"t": t, "kind": "skipped"})
                        } else if secret {
                            json!({"t": t, "kind": "key", "secret": true, "app": app})
                        } else {
                            let mut e = json!({"t": t, "kind": "key", "code": code, "text": text, "mods": mods, "app": app});
                            if read {
                                e["focus"] = json!(focus);
                            }
                            e
                        }
                    }
                };
                events.push(event);
                acted = Instant::now();
                settle = Some(acted + SETTLE);
            }
            () = tokio::time::sleep_until(wake), if settle.is_some() => {
                settle = None;
                windows = list(desk).await;
                light(desk, &windows, &apps, &mut asked).await;
                let now = front_of(&windows);
                events.extend(came(now.as_ref(), front.as_ref(), &apps, t0));
                frame = framed(desk, &windows, now.as_ref(), &apps).await;
                front = now;
            }
        }
    }
    events.extend(came(
        front_of(&list(desk).await).as_ref(),
        front.as_ref(),
        &apps,
        t0,
    ));
    desk.release(AGENT);
    let recording = json!({"id": id, "started_ms": started, "stopped_ms": now_ms(), "apps": apps, "events": events});
    if let Err(e) = save(&path, &recording) {
        crate::log(e);
    }
    let _ = done.send(recording);
}

#[cfg(target_os = "macos")]
fn listen(tx: UnboundedSender<Input>) -> Result<Listening, String> {
    if !tap::allowed() {
        tap::ask();
        return Err(INPUT_MONITORING.to_owned());
    }
    Tap::start(tx)
}

#[cfg(not(target_os = "macos"))]
fn listen(_: UnboundedSender<Input>) -> Result<Listening, String> {
    Err(OTHER_OS.to_owned())
}

fn rooted(found: &Value) -> bool {
    found.is_null()
        || found["role"]
            .as_str()
            .is_some_and(|r| ROOT_ROLES.contains(&r))
}

// the crop is CROP_PT points around the point, cut to the window, with the point's offset from its centre in desk units
fn crop(f: &Frame, x: f64, y: f64, per_point: f64) -> Option<(String, f64, [f64; 2])> {
    let [w, h] = CROP_PT.map(|v| v * per_point);
    let [x, y] = [x - f.bounds[0], y - f.bounds[1]];
    let [left, top] = [(x - w / 2.0).max(0.0), (y - h / 2.0).max(0.0)];
    let [right, bottom] = [
        (x + w / 2.0).min(f.bounds[2]),
        (y + h / 2.0).min(f.bounds[3]),
    ];
    let (_, _, png) = shuttle_desk::crop_png(
        &f.png,
        [left, top, right - left, bottom - top],
        f.factor,
        None,
    )
    .ok()?;
    let offset =
        [x - (left + right) / 2.0, y - (top + bottom) / 2.0].map(|v| (v * 10.0).round() / 10.0);
    Some((B64.encode(png), f.factor, offset))
}

#[cfg(target_os = "macos")]
fn per_point() -> f64 {
    shuttle_desk::ax::display_scale()
}

#[cfg(not(target_os = "macos"))]
fn per_point() -> f64 {
    1.0
}

fn kept(app: Option<&str>, apps: &[String]) -> bool {
    let Some(app) = app else {
        return apps.is_empty();
    };
    !BLOCKED.contains(&app) && (apps.is_empty() || apps.iter().any(|a| a.eq_ignore_ascii_case(app)))
}

// a path from the tap names its window by role and title, one from desk_at by id
fn window_of<'a>(windows: &'a [Value], at: &Value) -> Option<&'a Value> {
    let path = at["path"].as_array()?;
    let pid = at["pid"].as_u64();
    path.iter().find_map(|n| {
        windows.iter().find(|w| match n["id"].as_str() {
            Some(id) => w["id"].as_str() == Some(id),
            None => n["role"] == "window" && w["title"] == n["name"] && w["pid"].as_u64() == pid,
        })
    })
}

fn app_of(windows: &[Value], pid: Option<u64>) -> Option<String> {
    let pid = pid?;
    windows
        .iter()
        .find(|w| w["pid"].as_u64() == Some(pid))
        .and_then(|w| w["app"].as_str())
        .map(str::to_owned)
}

fn front_of(windows: &[Value]) -> Option<Value> {
    windows
        .iter()
        .find(|w| w["active"] == true && (w["title"] != "" || w["role"] != "window"))
        .cloned()
}

fn came(now: Option<&Value>, was: Option<&Value>, apps: &[String], t0: Instant) -> Option<Value> {
    let w = now?;
    (Some(&w["title"]) != was.map(|w| &w["title"]) && kept(w["app"].as_str(), apps)).then(|| {
        json!({"t": t0.elapsed().as_millis(), "kind": "after", "window": w["title"], "role": w["role"], "app": w["app"]})
    })
}

// a Chromium browser builds its page tree only for an assistive app that asks: each app the recording keeps is asked once, and the desk lets the trees go when the recording ends
async fn light<H: Handler>(desk: &H, windows: &[Value], apps: &[String], asked: &mut Vec<u64>) {
    for w in windows {
        let Some(pid) = w["pid"].as_u64().filter(|p| !asked.contains(p)) else {
            continue;
        };
        if !kept(w["app"].as_str(), apps) {
            continue;
        }
        asked.push(pid);
        let _ = call(desk, "desk_web", json!({"pid": pid, "on": true})).await;
    }
}

async fn list<H: Handler>(desk: &H) -> Vec<Value> {
    call(desk, "desk_windows", json!({}))
        .await
        .ok()
        .and_then(|v| match v {
            Value::Array(a) => Some(a),
            _ => None,
        })
        .unwrap_or_default()
}

async fn framed<H: Handler>(
    desk: &H,
    windows: &[Value],
    w: Option<&Value>,
    apps: &[String],
) -> Option<Frame> {
    let mut w = w?;
    for _ in 0..windows.len() {
        let Some(owner) = w["owner"].as_str() else {
            break;
        };
        w = windows.iter().find(|x| x["id"].as_str() == Some(owner))?;
    }
    if !kept(w["app"].as_str(), apps) {
        return None;
    }
    let bounds = <[f64; 4]>::deserialize(&w["bounds"]).ok()?;
    let out = desk
        .answer(asked("desk_shot", json!({"window": w["title"]})))
        .await;
    let png = B64.decode(out.image_b64?).ok()?;
    let width = u32::from_be_bytes(png.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(png.get(20..24)?.try_into().ok()?);
    let factor = f64::from(width) / bounds[2].max(1.0);
    if (f64::from(height) / factor - bounds[3]).abs() > 2.0 {
        return None;
    }
    Some(Frame {
        window: w["id"].as_str()?.to_owned(),
        factor,
        bounds,
        png,
        at: Instant::now(),
    })
}

async fn call<H: Handler>(desk: &H, tool: &str, args: Value) -> Result<Value, String> {
    let out = desk.answer(asked(tool, args)).await;
    if out.is_error {
        return Err(out.text);
    }
    serde_json::from_str(&out.text).map_err(|e| e.to_string())
}

fn asked(tool: &str, args: Value) -> Call {
    Call {
        agent_id: AGENT.to_owned(),
        tool: tool.to_owned(),
        args,
    }
}

fn save(path: &Path, recording: &Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        private_dir(dir)?;
    }
    let body = serde_json::to_vec(recording).map_err(|e| e.to_string())?;
    write_private(path, &body).map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(unix)]
fn private_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .and_then(|()| std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)))
        .map_err(|e| format!("create {}: {e}", dir.display()))
}

#[cfg(not(unix))]
fn private_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))
}

#[cfg(unix)]
fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?
        .write_all(body)
}

#[cfg(not(unix))]
fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, body)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use desk_core::wire::Output;

    use super::*;

    struct Desk {
        front: Mutex<&'static str>,
        hit: Value,
        shots: Mutex<Vec<Value>>,
        webs: Mutex<Vec<Value>>,
        released: Mutex<Vec<String>>,
    }

    impl Desk {
        fn new(hit: Value) -> Self {
            Self {
                front: Mutex::new("Untitled"),
                hit,
                shots: Mutex::default(),
                webs: Mutex::default(),
                released: Mutex::default(),
            }
        }
    }

    impl Handler for Desk {
        async fn answer(&self, call: Call) -> Output {
            let front = *lock(&self.front);
            match call.tool.as_str() {
                "desk_windows" => Output::text(
                    json!([
                        {"id": "7/1", "title": front, "role": "window", "pid": 7, "active": true, "app": "TextEdit", "bounds": [10, 20, 300, 200]},
                        {"id": "9/1", "title": "Vault", "role": "window", "pid": 9, "active": false, "app": "1Password"},
                    ])
                    .to_string(),
                ),
                "desk_at" if call.args["x"].is_i64() && call.args["y"].is_i64() => {
                    Output::text(self.hit.to_string())
                }
                "desk_web" if call.args["pid"] == 7 => {
                    lock(&self.webs).push(call.args);
                    Output::text(json!({"ok": true, "was": false}).to_string())
                }
                "desk_shot" => {
                    lock(&self.shots).push(call.args);
                    Output {
                        text: r#"{"ok":true}"#.into(),
                        image_b64: Some(B64.encode(gray([300, 200]))),
                        ..Output::default()
                    }
                }
                other => Output::error(format!("{other} is no desk call here")),
            }
        }

        fn release(&self, holder: &str) {
            lock(&self.released).push(holder.to_owned());
        }
    }

    fn gray([w, h]: [u32; 2]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut png = enc.write_header().unwrap();
        png.write_image_data(&vec![128; (w * h * 3) as usize])
            .unwrap();
        drop(png);
        out
    }

    async fn run(desk: &Desk, apps: &[&str], inputs: Vec<Input>) -> Value {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = unbounded_channel();
        let (done, made) = oneshot::channel();
        let path = dir.path().join("rec/r.json");
        let s = Session {
            id: "r".into(),
            apps: apps.iter().map(|a| (*a).to_owned()).collect(),
            per_point: 1.0,
            path: path.clone(),
            inputs: rx,
            done,
        };
        for i in inputs {
            tx.send(i).unwrap();
        }
        drop(tx);
        work(desk, s, || false).await;
        let r = made.await.unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        r
    }

    fn click(x: f64, y: f64) -> Input {
        Input::Click {
            x,
            y,
            right: false,
            count: 1,
            mods: Vec::new(),
            found: None,
        }
    }

    fn key(text: &str, secret: bool) -> Input {
        Input::Key {
            code: 0,
            text: text.into(),
            mods: Vec::new(),
            secret,
            pid: None,
            focus: None,
            read: false,
        }
    }

    #[tokio::test]
    async fn a_click_keeps_the_element_under_it_its_window_and_a_crop_around_it() {
        let desk = Desk::new(
            json!({"found": {"role": "button", "name": "Save"}, "path": [{"id": "7/1", "role": "window", "name": "Untitled"}], "pid": 7}),
        );
        let r = run(&desk, &[], vec![click(100.0, 80.0)]).await;
        let e = &r["events"][0];
        assert_eq!(
            (&e["kind"], &e["element"]["name"], &e["app"], &e["root"]),
            (
                &json!("click"),
                &json!("Save"),
                &json!("TextEdit"),
                &json!(false)
            ),
            "{e}"
        );
        assert_eq!(
            e["window"],
            json!({"title": "Untitled", "bounds": [10, 20, 300, 200]})
        );
        let crop = B64.decode(e["crop"].as_str().unwrap()).unwrap();
        assert_eq!(&crop[1..4], b"PNG");
        assert_eq!(u32::from_be_bytes(crop[16..20].try_into().unwrap()), 190);
        assert_eq!(e["crop_offset"], json!([-5.0, 0.0]));
        let r = run(&desk, &[], vec![click(30.0, 20.0)]).await;
        let e = &r["events"][0];
        let crop = B64.decode(e["crop"].as_str().unwrap()).unwrap();
        assert_eq!(u32::from_be_bytes(crop[16..20].try_into().unwrap()), 120);
        assert_eq!(e["crop_offset"], json!([-40.0, -25.0]), "{e}");
        assert!(
            lock(&desk.shots)
                .iter()
                .all(|a| *a == json!({"window": "Untitled"})),
            "{:?}",
            lock(&desk.shots)
        );
    }

    #[tokio::test]
    async fn a_click_the_tap_read_keeps_that_element_and_not_what_the_desk_finds_later() {
        let desk = Desk::new(
            json!({"found": {"role": "generic", "name": "Sidebar"}, "path": [], "pid": 3}),
        );
        let menu = Input::Click {
            x: 160.0,
            y: 48.0,
            right: false,
            count: 1,
            mods: Vec::new(),
            found: Some(Hit {
                role: "menuitem",
                name: "New".into(),
                description: String::new(),
                pid: Some(7),
                path: vec![("menu", "File".into()), ("window", "Untitled".into())],
                secret: false,
                page: None,
            }),
        };
        let r = run(&desk, &[], vec![menu]).await;
        let e = &r["events"][0];
        assert_eq!(
            (&e["element"]["name"], &e["app"], &e["window"]["title"]),
            (&json!("New"), &json!("TextEdit"), &json!("Untitled")),
            "{e}"
        );
        assert!(e.get("web").is_none(), "{e}");
    }

    #[tokio::test]
    async fn a_recording_holds_the_page_tree_of_each_app_it_keeps_and_lets_go_at_its_end() {
        let desk = Desk::new(Value::Null);
        run(&desk, &[], vec![click(5.0, 5.0)]).await;
        assert_eq!(*lock(&desk.webs), [json!({"pid": 7, "on": true})]);
        assert_eq!(*lock(&desk.released), ["record"]);
        let other = Desk::new(Value::Null);
        run(&other, &["Notes"], vec![click(5.0, 5.0)]).await;
        assert!(lock(&other.webs).is_empty());
    }

    #[tokio::test]
    async fn a_click_in_a_web_page_keeps_the_page_address_and_the_dom_id() {
        let desk = Desk::new(Value::Null);
        let field = |page| Input::Click {
            x: 120.0,
            y: 80.0,
            right: false,
            count: 1,
            mods: Vec::new(),
            found: Some(Hit {
                role: "button",
                name: "Pay".into(),
                pid: Some(7),
                path: vec![("window", "Untitled".into())],
                page,
                ..Hit::default()
            }),
        };
        let r = run(
            &desk,
            &[],
            vec![
                field(Some(("https://shop.test/cart".into(), Some("pay".into())))),
                field(Some(("https://shop.test/cart".into(), None))),
            ],
        )
        .await;
        let e = &r["events"];
        assert_eq!(
            (&e[0]["web"], &e[1]["web"]),
            (
                &json!({"url": "https://shop.test/cart", "id": "pay"}),
                &json!({"url": "https://shop.test/cart"})
            ),
            "{e}"
        );
    }

    #[tokio::test]
    async fn a_click_in_a_window_newer_than_the_last_list_finds_it() {
        let desk = Desk::new(
            json!({"found": {"role": "button", "name": "Save"}, "path": [{"id": "7/1", "role": "window", "name": "report.rtf"}], "pid": 7}),
        );
        let (tx, rx) = unbounded_channel();
        let (done, made) = oneshot::channel();
        let dir = tempfile::tempdir().unwrap();
        let s = Session {
            id: "r".into(),
            apps: Vec::new(),
            per_point: 1.0,
            path: dir.path().join("r.json"),
            inputs: rx,
            done,
        };
        let worker = async { work(&desk, s, || false).await };
        let user = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            *lock(&desk.front) = "report.rtf";
            tx.send(click(10.0, 10.0)).unwrap();
            drop(tx);
        };
        tokio::join!(worker, user);
        let r = made.await.unwrap();
        assert_eq!(r["events"][0]["window"]["title"], "report.rtf", "{r}");
    }

    #[tokio::test]
    async fn a_password_field_clicked_or_focused_is_marked() {
        let desk = Desk::new(Value::Null);
        let field = Input::Click {
            x: 20.0,
            y: 20.0,
            right: false,
            count: 1,
            mods: Vec::new(),
            found: Some(Hit {
                role: "textbox",
                pid: Some(7),
                secret: true,
                ..Hit::default()
            }),
        };
        let r = run(
            &desk,
            &[],
            vec![
                field,
                click(5.0, 5.0),
                Input::Password {
                    pid: Some(7),
                    focus: Some(Field {
                        role: "textbox",
                        secure: true,
                        ..Field::default()
                    }),
                },
            ],
        )
        .await;
        assert_eq!(
            (&r["events"][0]["secret"], r["events"][1].get("secret")),
            (&json!(true), None),
            "{r}"
        );
        assert_eq!(
            (
                &r["events"][2]["kind"],
                &r["events"][2]["app"],
                &r["events"][2]["focus"]
            ),
            (
                &json!("password"),
                &json!("TextEdit"),
                &json!({"role": "textbox", "name": "", "frame": "", "secure": true})
            ),
            "{r}"
        );
    }

    #[tokio::test]
    async fn a_password_focus_counts_for_the_app_that_has_it_and_not_the_front_one() {
        let desk = Desk::new(Value::Null);
        let focus = |pid| Input::Password {
            pid,
            focus: Some(Field {
                secure: true,
                ..Field::default()
            }),
        };
        let r = run(
            &desk,
            &[],
            vec![focus(Some(9)), focus(None), focus(Some(7))],
        )
        .await;
        let seen: Vec<(&Value, &Value)> = r["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (&e["kind"], &e["app"]))
            .collect();
        assert_eq!(
            seen,
            [
                (&json!("skipped"), &Value::Null),
                (&json!("skipped"), &Value::Null),
                (&json!("password"), &json!("TextEdit")),
            ]
        );
        let r = run(&desk, &["Notes"], vec![focus(Some(7))]).await;
        assert_eq!(r["events"][0]["kind"], "skipped");
    }

    #[tokio::test]
    async fn a_click_on_a_document_root_is_marked_root() {
        let desk = Desk::new(
            json!({"found": {"role": "document", "name": ""}, "path": [{"id": "7/1", "role": "window", "name": "Untitled"}], "pid": 7}),
        );
        let r = run(&desk, &[], vec![click(100.0, 80.0)]).await;
        assert_eq!(r["events"][0]["root"], true);
    }

    #[tokio::test]
    async fn the_first_key_of_a_run_keeps_the_field_it_went_to_or_that_none_was_readable() {
        let desk = Desk::new(Value::Null);
        let into = |focus: Option<Field>, read: bool| Input::Key {
            code: 0,
            text: "a".into(),
            mods: Vec::new(),
            secret: false,
            pid: None,
            focus,
            read,
        };
        let field = Field {
            role: "textbox",
            name: "Save As:".into(),
            frame: "dialog",
            window: "Untitled".into(),
            ..Field::default()
        };
        let r = run(
            &desk,
            &[],
            vec![into(Some(field), true), into(None, false), into(None, true)],
        )
        .await;
        let e = &r["events"];
        assert_eq!(
            e[0]["focus"],
            json!({"role": "textbox", "name": "Save As:", "frame": "dialog", "window": "Untitled"}),
            "{e}"
        );
        assert!(e[1].get("focus").is_none(), "{e}");
        assert_eq!(e[2]["focus"], Value::Null, "{e}");
        assert!(e[2].as_object().unwrap().contains_key("focus"), "{e}");
    }

    #[tokio::test]
    async fn a_key_into_a_password_field_keeps_no_text() {
        let desk = Desk::new(Value::Null);
        let r = run(&desk, &[], vec![key("a", false), key("", true)]).await;
        let e = &r["events"];
        assert_eq!(
            (&e[0]["text"], &e[1]["secret"]),
            (&json!("a"), &json!(true)),
            "{e}"
        );
        assert!(e[1].get("text").is_none(), "{e}");
    }

    #[tokio::test]
    async fn input_in_a_blocked_or_unnamed_app_is_skipped_without_details() {
        let desk = Desk::new(
            json!({"found": {"role": "textbox", "name": "Master password"}, "path": [{"id": "9/1", "role": "window", "name": "Vault"}], "pid": 9}),
        );
        let r = run(&desk, &[], vec![click(5.0, 5.0)]).await;
        assert_eq!(
            r["events"][0],
            json!({"t": r["events"][0]["t"], "kind": "skipped"})
        );
        assert!(
            lock(&desk.shots).iter().all(|a| a["window"] != "Vault"),
            "{:?}",
            lock(&desk.shots)
        );
        let only = Desk::new(
            json!({"found": {"role": "button", "name": "Save"}, "path": [{"id": "7/1", "role": "window", "name": "Untitled"}], "pid": 7}),
        );
        let r = run(&only, &["Notes"], vec![click(5.0, 5.0), key("x", false)]).await;
        let kinds: Vec<&Value> = r["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| &e["kind"])
            .collect();
        assert_eq!(kinds, [&json!("skipped"), &json!("skipped")]);
        assert!(lock(&only.shots).is_empty());
        let vault = Input::Key {
            code: 0,
            text: "p".into(),
            mods: Vec::new(),
            secret: false,
            pid: Some(9),
            focus: None,
            read: false,
        };
        let r = run(&only, &[], vec![key("a", false), vault]).await;
        let kinds: Vec<&Value> = r["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| &e["kind"])
            .collect();
        assert_eq!(kinds, [&json!("key"), &json!("skipped")], "{r}");
    }

    #[tokio::test]
    async fn a_window_that_came_after_the_last_input_settled_is_kept_when_the_recording_stops() {
        let desk = Desk::new(Value::Null);
        let (tx, rx) = unbounded_channel();
        let (done, made) = oneshot::channel();
        let dir = tempfile::tempdir().unwrap();
        let s = Session {
            id: "r".into(),
            apps: Vec::new(),
            per_point: 1.0,
            path: dir.path().join("r.json"),
            inputs: rx,
            done,
        };
        let worker = async { work(&desk, s, || false).await };
        let user = async {
            tx.send(key("s", false)).unwrap();
            tokio::time::sleep(SETTLE + Duration::from_millis(300)).await;
            *lock(&desk.front) = "Paid";
            drop(tx);
        };
        tokio::join!(worker, user);
        let r = made.await.unwrap();
        let last = &r["events"][1];
        assert_eq!(
            (&last["kind"], &last["window"]),
            (&json!("after"), &json!("Paid")),
            "{r}"
        );
    }

    #[tokio::test]
    async fn a_new_front_window_after_input_is_kept_with_notes_in_order() {
        let desk = Desk::new(Value::Null);
        let (tx, rx) = unbounded_channel();
        let (done, made) = oneshot::channel();
        let dir = tempfile::tempdir().unwrap();
        let s = Session {
            id: "r".into(),
            apps: Vec::new(),
            per_point: 1.0,
            path: dir.path().join("r.json"),
            inputs: rx,
            done,
        };
        let worker = async { work(&desk, s, || false).await };
        let user = async {
            tx.send(Input::Note("the name is a parameter".into()))
                .unwrap();
            tx.send(key("s", false)).unwrap();
            *lock(&desk.front) = "report.rtf";
            tokio::time::sleep(SETTLE + Duration::from_millis(300)).await;
            drop(tx);
        };
        tokio::join!(worker, user);
        let r = made.await.unwrap();
        let kinds: Vec<&Value> = r["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| &e["kind"])
            .collect();
        assert_eq!(
            kinds,
            [&json!("note"), &json!("key"), &json!("after")],
            "{r}"
        );
        assert_eq!(r["events"][2]["window"], "report.rtf");
    }
}
