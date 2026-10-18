//! The desk calls: snapshots and element actions through the OS backend, input, windows, launch and
//! capture through cua-driver; the same verbs and JSON as the Linux toolhost.

use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use desk_core::args::{
    ActArgs, AtArgs, ClickArgs, FindArgs, IdArgs, KeyArgs, LaunchArgs, ReadArgs, ShotArgs,
    SnapArgs, TypeArgs, WindowArgs, parse,
};
use desk_core::press::{MENU_POLL, MENU_POLLS, SELECTED_ROLES};
use desk_core::text::{MAX_FOUND, NAME_CHARS, matches, ms, quote};
use desk_core::window;
use desk_core::wire::Output;
use serde_json::{Map, Value, json};

use crate::driver::{Input, WINDOW_SESSION, desktop, text_of};
use crate::keys::{Os, key_names};
use crate::model::{Access, Act, Extras, Frame, Node};
use crate::worker::Worker;

pub const TURN_WAIT: Duration = Duration::from_secs(15);
pub const ACTIONS: [&str; 8] = [
    "desk_press",
    "desk_act",
    "desk_key",
    "desk_type",
    "desk_click",
    "desk_focus",
    "desk_close",
    "desk_launch",
];

const CALL_LIMIT: Duration = Duration::from_secs(3);
const TREE_LIMIT: Duration = Duration::from_secs(10);
const READ_CHARS: usize = 64 << 10;
const OVERLAY_TITLE: &str = "Cua.AgentCursorOverlay";
const NO_WINDOW_UNDER: &str =
    "no window under the point, and the app allowlist admits only windows";

type Turn = Arc<tokio::sync::OwnedMutexGuard<()>>;

/// The desk of one OS: `B` reads and acts on elements, `D` sends input.
pub struct Desk<B, D> {
    access: Worker<B>,
    driver: D,
    os: Os,
    acting: Arc<tokio::sync::Mutex<()>>,
    turn_wait: Duration,
    scale: OnceLock<f64>,
    read: std::sync::Mutex<Option<Read>>,
    allow: Allow,
}

impl<B: Access + 'static, D: Input + Sync> Desk<B, D> {
    /// A desk whose backend `open` makes on its worker thread.
    pub fn new(open: fn() -> Result<B, String>, driver: D, os: Os) -> Self {
        Self {
            access: Worker::new(open),
            driver,
            os,
            acting: Arc::new(tokio::sync::Mutex::new(())),
            turn_wait: TURN_WAIT,
            scale: OnceLock::new(),
            read: std::sync::Mutex::new(None),
            allow: Allow::default(),
        }
    }

    /// Limits the desk calls to windows of `apps`, matched by app name or executable, case-insensitive; empty allows every app.
    pub fn allow(mut self, apps: Vec<String>) -> Self {
        self.allow = Allow(apps.iter().map(|a| app_key(a)).collect());
        self
    }

    /// Runs desk call `tool`, a `desk_<verb>`; actions run one at a time, until their backend calls end, reads alongside them.
    pub async fn run(&self, tool: &str, args: Value) -> Result<Output, String> {
        let turn = if ACTIONS.contains(&tool) {
            let turn = tokio::time::timeout(self.turn_wait, self.acting.clone().lock_owned())
                .await
                .map_err(|_| "desk busy: a previous action is still running".to_owned())?;
            Some(Arc::new(turn))
        } else {
            None
        };
        if !self.allow.is_open() {
            self.admit(tool, &args).await?;
        }
        let text = match tool {
            "desk_windows" => self.windows().await?,
            "desk_snapshot" => self.snapshot(parse(args)?).await?,
            "desk_find" => self.find(parse(args)?).await?,
            "desk_read" => self.read(parse(args)?).await?,
            "desk_press" => self.press(parse::<IdArgs>(args)?.id, turn.clone()).await?,
            "desk_act" => self.act(parse(args)?, turn.clone()).await?,
            "desk_at" => self.at(parse(args)?).await?,
            "desk_key" => self.key(&parse::<KeyArgs>(args)?.keys).await?,
            "desk_type" => self.type_text(&parse::<TypeArgs>(args)?.text).await?,
            "desk_click" => self.click(parse(args)?).await?,
            "desk_focus" => self.focus(parse(args)?).await?,
            "desk_close" => self.close(parse(args)?, turn.clone()).await?,
            "desk_launch" => {
                let app = parse::<LaunchArgs>(args)?.app;
                let v = self.driver.call("launch_app", json!({"name": app})).await?;
                json!({"ok": true, "launched": text_of(&v)}).to_string()
            }
            "desk_shot" => return self.shot(parse(args)?).await,
            "desk_screen" => {
                let v = self.driver.call("get_screen_size", json!({})).await?;
                v["structuredContent"].to_string()
            }
            other => return Err(format!("unknown desk tool {other}")),
        };
        Ok(Output::text(text))
    }

    async fn windows(&self) -> Result<String, String> {
        let mut frames = self.access.run(CALL_LIMIT, (), |a| a.frames()).await?;
        frames.retain(|f| self.allow.admits(f.app.as_deref()));
        frames.sort_by_key(|f| !f.active);
        serde_json::to_string(&frames).map_err(|e| format!("encode windows: {e}"))
    }

    async fn admit(&self, tool: &str, args: &Value) -> Result<(), String> {
        let target = match tool {
            "desk_launch" => return self.allow.check(args["app"].as_str()),
            "desk_read" | "desk_press" | "desk_act" => {
                Target::Element(args["id"].as_str().ok_or("no element id")?.to_owned())
            }
            "desk_key" | "desk_type" => Target::Focused,
            _ => return Ok(()),
        };
        let allow = self.allow.clone();
        self.access
            .run(CALL_LIMIT, (), move |b| match target {
                Target::Element(id) => {
                    let pid = b.pid_of(&id)?;
                    allow.check_pid(&b.frames()?, pid)
                }
                Target::Focused => allow.pick(b.frames()?, None, None).map(drop),
            })
            .await
    }

    async fn snapshot(&self, a: SnapArgs) -> Result<String, String> {
        let started = Instant::now();
        let extras = Extras {
            values: true,
            bounds: a.bounds.unwrap_or(true),
            keys: a.keys,
            menus: a.menus,
        };
        let (window, pid) = (a.window, a.pid);
        let allow = self.allow.clone();
        let (tree, found) = self
            .access
            .run(TREE_LIMIT, (), move |b| {
                let frame = allow.pick(b.frames()?, window.as_deref(), pid)?;
                let found = started.elapsed().as_micros();
                Ok((b.tree(&frame, extras)?, found))
            })
            .await?;
        let mut phases = Map::new();
        phases.insert("frames".into(), ms(found));
        for (name, us) in &tree.phases {
            phases.insert((*name).into(), ms(*us));
        }
        let mut out = json!({
            "window": tree.frame,
            "ms": ms(started.elapsed().as_micros()),
            "truncated": tree.truncated,
        });
        out["phases"] = Value::Object(phases);
        if a.text {
            out["text"] = outline(&tree.nodes, a.max_chars.unwrap_or(NAME_CHARS)).into();
        } else {
            out["nodes"] = tree
                .nodes
                .iter()
                .enumerate()
                .map(|(i, n)| n.json(i))
                .collect();
        }
        Ok(out.to_string())
    }

    async fn find(&self, a: FindArgs) -> Result<String, String> {
        let extras = Extras {
            values: true,
            bounds: true,
            keys: false,
            menus: true,
        };
        let (window, pid) = (a.window, a.pid);
        let allow = self.allow.clone();
        let tree = self
            .access
            .run(TREE_LIMIT, (), move |b| {
                let frame = allow.pick(b.frames()?, window.as_deref(), pid)?;
                b.tree(&frame, extras)
            })
            .await?;
        let found: Vec<Value> = matches(&tree.nodes, &a.name, a.role.as_deref())
            .into_iter()
            .take(MAX_FOUND)
            .map(|i| tree.nodes[i].json(i))
            .collect();
        let mut out = json!({"window": tree.frame});
        out["found"] = Value::Array(found);
        Ok(out.to_string())
    }

    async fn read(&self, a: ReadArgs) -> Result<String, String> {
        let extras = Extras {
            values: true,
            bounds: true,
            keys: true,
            menus: false,
        };
        let chars = if a.text { READ_CHARS } else { NAME_CHARS };
        let node = self
            .access
            .run(CALL_LIMIT, (), move |b| b.node(&a.id, extras, chars))
            .await?;
        Ok(node.json(0).to_string())
    }

    async fn press(&self, id: String, turn: Option<Turn>) -> Result<String, String> {
        match self
            .access
            .run(TREE_LIMIT, turn, move |b| plan_press(b, &id))
            .await?
        {
            Press::Selected => Ok(json!({"ok": true, "how": "select"}).to_string()),
            Press::Acted(ok) => Ok(json!({"ok": ok, "how": "action"}).to_string()),
            Press::Click(x, y) => {
                self.click_at(json!({"x": x, "y": y})).await?;
                Ok(json!({"ok": true, "how": "click", "x": x, "y": y}).to_string())
            }
        }
    }

    async fn act(&self, a: ActArgs, turn: Option<Turn>) -> Result<String, String> {
        let act = match a.action.as_str() {
            "action" => Act::Action,
            "select" => Act::Select,
            "focus" => Act::Focus,
            "click" => {
                let n = self
                    .access
                    .run(CALL_LIMIT, turn, move |b| b.node(&a.id, Extras::BOUNDS, 0))
                    .await?;
                let [x, y, w, h] = n.bounds.ok_or("the element has no screen position")?;
                self.click_at(json!({"x": x + w / 2, "y": y + h / 2}))
                    .await?;
                return Ok(json!({"ok": true}).to_string());
            }
            other => {
                return Err(format!(
                    "unknown action {other}; use click, action, select or focus"
                ));
            }
        };
        let ok = self
            .access
            .run(CALL_LIMIT, turn, move |b| b.act(&a.id, act))
            .await?;
        Ok(json!({"ok": ok}).to_string())
    }

    async fn at(&self, a: AtArgs) -> Result<String, String> {
        let allow = self.allow.clone();
        self.access
            .run(CALL_LIMIT, (), move |b| {
                let frames = b.frames()?;
                let frame = allow.pick(frames.clone(), a.window.as_deref(), a.pid)?;
                let Some(hit) = b.at(&frame.id, a.x, a.y)? else {
                    return Ok(json!({"found": null, "window": frame}).to_string());
                };
                if !allow.is_open() {
                    let pid = b.pid_of(&hit)?;
                    allow.check_pid(&frames, pid)?;
                }
                let node = b.node(&hit, Extras::default(), 0)?;
                let path: Vec<Value> = b
                    .ancestors(&hit)?
                    .into_iter()
                    .filter_map(|id| {
                        let n = b.node(&id, Extras::default(), 0).ok()?;
                        Some(json!({"id": id, "role": n.role, "name": n.name}))
                    })
                    .collect();
                let mut out = json!({});
                out["found"] = node.json(0);
                out["path"] = Value::Array(path);
                Ok(out.to_string())
            })
            .await
    }

    async fn close(&self, a: WindowArgs, turn: Option<Turn>) -> Result<String, String> {
        let allow = self.allow.clone();
        self.access
            .run(CALL_LIMIT, turn, move |b| {
                let frame = allow.pick(b.frames()?, Some(&a.window), a.pid)?;
                b.close(&frame)?;
                Ok(json!({"ok": true, "title": frame.title}).to_string())
            })
            .await
    }

    async fn key(&self, combo: &str) -> Result<String, String> {
        let keys = key_names(combo, self.os)?;
        match keys.as_slice() {
            [one] => {
                self.driver
                    .call("press_key", desktop(json!({"key": one})))
                    .await?
            }
            _ => {
                self.driver
                    .call("hotkey", desktop(json!({"keys": keys})))
                    .await?
            }
        };
        Ok(json!({"ok": true}).to_string())
    }

    async fn type_text(&self, text: &str) -> Result<String, String> {
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                self.driver
                    .call("press_key", desktop(json!({"key": "return"})))
                    .await?;
            }
            let line = line.trim_end_matches('\r');
            if self.os == Os::Mac {
                if !line.is_empty() {
                    self.driver
                        .call("type_text", desktop(json!({"text": line})))
                        .await?;
                }
                continue;
            }
            for (i, c) in line.char_indices() {
                let req = match keystroke(c) {
                    Some(keys) if keys.len() == 1 => ("press_key", json!({"key": keys[0]})),
                    Some(keys) => ("hotkey", json!({"keys": keys})),
                    None => ("type_text", json!({"text": &line[i..i + c.len_utf8()]})),
                };
                self.driver.call(req.0, desktop(req.1)).await?;
            }
        }
        Ok(json!({"ok": true, "chars": text.chars().count()}).to_string())
    }

    async fn click(&self, a: ClickArgs) -> Result<String, String> {
        let mut req = json!({"x": a.x, "y": a.y});
        if let Some(button) = a.button {
            req["button"] = button.into();
        }
        if let Some(count) = a.count {
            req["count"] = count.into();
        }
        self.click_at(req).await?;
        Ok(json!({"ok": true}).to_string())
    }

    async fn click_at(&self, req: Value) -> Result<(), String> {
        let point = req["x"].as_f64().zip(req["y"].as_f64());
        if self.os == Os::Mac
            && let Some((x, y)) = point
        {
            let (px, py) = (x as i32, y as i32);
            let allow = self.allow.clone();
            let pressed = self
                .access
                .run(CALL_LIMIT, (), move |b| {
                    let Some(item) = b.menu_at(px, py)? else {
                        return Ok(false);
                    };
                    if !allow.is_open() {
                        let pid = b.pid_of(&item)?;
                        allow.check_pid(&b.frames()?, pid)?;
                    }
                    if b.act(&item, Act::Action)? {
                        Ok(true)
                    } else {
                        Err(format!("menu item {item} took no action"))
                    }
                })
                .await?;
            if pressed {
                return Ok(());
            }
        }
        let under = match point {
            Some((x, y)) if self.os == Os::Mac || !self.allow.is_open() => {
                self.window_at(x, y).await?
            }
            _ => None,
        };
        if !self.allow.is_open() {
            let pid = u32::try_from(under.as_ref().ok_or(NO_WINDOW_UNDER)?.pid).ok();
            let allow = self.allow.clone();
            self.access
                .run(CALL_LIMIT, (), move |b| allow.check_pid(&b.frames()?, pid))
                .await?;
        }
        if self.os == Os::Mac
            && let Some(w) = under
        {
            return self.click_window(req, &w).await;
        }
        self.driver.call("click", desktop(req)).await?;
        Ok(())
    }

    async fn click_window(&self, req: Value, w: &Under) -> Result<(), String> {
        let front = json!({"pid": w.pid, "window_id": w.id});
        self.driver.call("bring_to_front", front).await?;
        let cached = self
            .reads()
            .filter(|r| (r.pid, r.id, r.bounds) == (w.pid, w.id, w.bounds))
            .map(|r| r.k);
        let k = match cached {
            Some(k) => Some(k),
            None => self.pixels(w).await?,
        };
        let Some(k) = k else {
            self.driver.call("click", desktop(req)).await?;
            return Ok(());
        };
        let mut win = req.clone();
        win["session"] = WINDOW_SESSION.into();
        win["pid"] = w.pid.into();
        win["window_id"] = w.id.into();
        let at = |win: &mut Value, (kx, ky): (f64, f64)| {
            win["x"] = ((w.point.0 - w.bounds[0]) * kx).into();
            win["y"] = ((w.point.1 - w.bounds[1]) * ky).into();
        };
        at(&mut win, k);
        match self.driver.call("click", win.clone()).await {
            Err(e) if cached.is_some() && e.contains("window_never_read") => {
                match self.pixels(w).await? {
                    Some(k) => {
                        at(&mut win, k);
                        self.driver.call("click", win).await?;
                    }
                    None => {
                        self.driver.call("click", desktop(req)).await?;
                    }
                }
            }
            done => {
                done?;
            }
        }
        Ok(())
    }

    async fn scale(&self) -> Result<f64, String> {
        if let Some(s) = self.scale.get() {
            return Ok(*s);
        }
        let v = self.driver.call("get_screen_size", json!({})).await?;
        let s = v["structuredContent"]["scale_factor"]
            .as_f64()
            .unwrap_or(1.0);
        Ok(*self.scale.get_or_init(|| s))
    }

    async fn window_at(&self, x: f64, y: f64) -> Result<Option<Under>, String> {
        let scale = self.scale().await?;
        let listed = self.on_screen().await?;
        let point = (x / scale, y / scale);
        let top = listed
            .iter()
            .filter(|w| w["layer"] == 0)
            .filter(|w| {
                let [bx, by, bw, bh] = bounds(w);
                point.0 >= bx && point.0 < bx + bw && point.1 >= by && point.1 < by + bh
            })
            .max_by_key(|w| w["z_index"].as_i64().unwrap_or(i64::MIN));
        Ok(top.and_then(|w| {
            Some(Under {
                pid: w["pid"].as_i64()?,
                id: w["window_id"].as_i64()?,
                bounds: bounds(w),
                point,
            })
        }))
    }

    async fn on_screen(&self) -> Result<Vec<Value>, String> {
        let mut listed = self
            .driver
            .call("list_windows", json!({"on_screen_only": true}))
            .await?;
        let Value::Array(mut windows) = listed["structuredContent"]["windows"].take() else {
            return Ok(Vec::new());
        };
        windows.retain(|w| {
            !w["title"]
                .as_str()
                .is_some_and(|t| t.starts_with(OVERLAY_TITLE))
        });
        Ok(windows)
    }

    async fn pixels(&self, w: &Under) -> Result<Option<(f64, f64)>, String> {
        let read = json!({"session": WINDOW_SESSION, "pid": w.pid, "window_id": w.id});
        let v = self.driver.call("get_window_state", read).await?;
        let c = &v["structuredContent"];
        let refused = c["background_input"]["routes"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|r| r["route"] == "window_pointer" && r["status"] == "refused");
        if refused {
            return Ok(None);
        }
        let ratio = |px: &str, pt: &str| {
            c[px]
                .as_f64()
                .zip(c["window_bounds"][pt].as_f64())
                .filter(|&(_, pt)| pt > 0.0)
                .map(|(px, pt)| px / pt)
                .ok_or_else(|| format!("get_window_state of window {} has no {px}", w.id))
        };
        let k = (
            ratio("screenshot_width", "width")?,
            ratio("screenshot_height", "height")?,
        );
        *self.reads() = Some(Read {
            pid: w.pid,
            id: w.id,
            bounds: w.bounds,
            k,
        });
        Ok(Some(k))
    }

    fn reads(&self) -> std::sync::MutexGuard<'_, Option<Read>> {
        self.read
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn focus(&self, a: WindowArgs) -> Result<String, String> {
        let listed = self.driver.call("list_windows", json!({})).await?;
        let mut windows: Vec<Frame> = listed["structuredContent"]["windows"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|w| Frame {
                id: w["window_id"].to_string(),
                title: w["title"].as_str().unwrap_or_default().to_owned(),
                pid: w["pid"].as_u64().and_then(|p| u32::try_from(p).ok()),
                app: w["app_name"].as_str().map(str::to_owned),
                ..Frame::default()
            })
            .collect();
        if !self.allow.is_open() {
            let frames = self.access.run(CALL_LIMIT, (), |b| b.frames()).await?;
            for w in &mut windows {
                w.app = app_of(&frames, w.pid).map(str::to_owned);
            }
        }
        let w = self.allow.pick(windows, Some(&a.window), a.pid)?;
        let window_id: Value = serde_json::from_str(&w.id).unwrap_or(Value::Null);
        self.driver
            .call(
                "bring_to_front",
                json!({"pid": w.pid, "window_id": window_id}),
            )
            .await?;
        Ok(json!({"ok": true, "title": w.title, "pid": w.pid}).to_string())
    }

    async fn shot(&self, a: ShotArgs) -> Result<Output, String> {
        let v = self.driver.call("get_desktop_state", json!({})).await?;
        let png = v["content"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["type"] == "image")
            .and_then(|c| c["data"].as_str())
            .ok_or("the driver returned no screenshot")?;
        let png = B64
            .decode(png)
            .map_err(|e| format!("decode screenshot: {e}"))?;
        let s = &v["structuredContent"];
        let shot_w = s["screenshot_width"].as_f64().unwrap_or(0.0);
        let full_w = s["screenshot_original_width"].as_f64().unwrap_or(shot_w);
        let factor = if full_w > 0.0 { shot_w / full_w } else { 1.0 };
        let rect = [
            a.x.unwrap_or(0.0),
            a.y.unwrap_or(0.0),
            a.w.unwrap_or(f64::MAX / 4.0),
            a.h.unwrap_or(f64::MAX / 4.0),
        ];
        let mask = if self.allow.is_open() {
            None
        } else {
            Some(self.mask(factor).await?)
        };
        let (w, h, png) = crop_png(&png, rect, factor, mask.as_deref())?;
        Ok(Output {
            text: json!({"ok": true, "width": w, "height": h}).to_string(),
            image_b64: Some(B64.encode(&png)),
            ..Output::default()
        })
    }

    async fn mask(&self, factor: f64) -> Result<Vec<([f64; 4], bool)>, String> {
        let k = self.scale().await? * factor;
        let mut windows = self.on_screen().await?;
        let frames = self.access.run(CALL_LIMIT, (), |b| b.frames()).await?;
        windows.sort_by_key(|w| w["z_index"].as_i64().unwrap_or(i64::MIN));
        Ok(windows
            .into_iter()
            .map(|w| {
                let pid = w["pid"].as_u64().and_then(|p| u32::try_from(p).ok());
                (
                    bounds(&w).map(|v| v * k),
                    self.allow.admits(app_of(&frames, pid)),
                )
            })
            .collect())
    }
}

#[derive(Clone, Default)]
struct Allow(Arc<[String]>);

impl Allow {
    fn is_open(&self) -> bool {
        self.0.is_empty()
    }

    fn admits(&self, app: Option<&str>) -> bool {
        self.is_open() || app.is_some_and(|a| self.0.contains(&app_key(a)))
    }

    fn check(&self, app: Option<&str>) -> Result<(), String> {
        if self.admits(app) {
            Ok(())
        } else {
            Err(refused(app.unwrap_or("an unknown app")))
        }
    }

    fn check_pid(&self, frames: &[Frame], pid: Option<u32>) -> Result<(), String> {
        if self.is_open() {
            return Ok(());
        }
        self.check(app_of(frames, pid))
    }

    fn pick(
        &self,
        mut frames: Vec<Frame>,
        want: Option<&str>,
        pid: Option<u32>,
    ) -> Result<Frame, String> {
        if want.is_none() || self.is_open() {
            let frame = window::pick(frames, want, pid)?;
            self.check(frame.app.as_deref())?;
            return Ok(frame);
        }
        frames.retain(|f| self.admits(f.app.as_deref()));
        window::pick(frames, want, pid)
    }
}

enum Target {
    Element(String),
    Focused,
}

struct Under {
    pid: i64,
    id: i64,
    bounds: [f64; 4],
    point: (f64, f64),
}

#[derive(Clone, Copy)]
struct Read {
    pid: i64,
    id: i64,
    bounds: [f64; 4],
    k: (f64, f64),
}

/// What a press comes to after the backend's part of it.
#[derive(Debug, PartialEq)]
enum Press {
    Selected,
    Click(i32, i32),
    Acted(bool),
}

fn app_key(app: &str) -> String {
    let app = app.to_lowercase();
    app.strip_suffix(".exe").map(str::to_owned).unwrap_or(app)
}

fn app_of(frames: &[Frame], pid: Option<u32>) -> Option<&str> {
    frames
        .iter()
        .find(|f| pid.is_some() && f.pid == pid)
        .and_then(|f| f.app.as_deref())
}

fn bounds(w: &Value) -> [f64; 4] {
    let b = &w["bounds"];
    ["x", "y", "width", "height"].map(|k| b[k].as_f64().unwrap_or(0.0))
}

fn refused(app: &str) -> String {
    format!("{app} is not in the app allowlist ([safety] apps in config.toml)")
}

/// Presses `id` the way a user would; a menu entry runs its action, since a click right after its popup opens misses it.
fn plan_press<B: Access>(b: &mut B, id: &str) -> Result<Press, String> {
    let mut node = b.node(id, Extras::BOUNDS, 0)?;
    if SELECTED_ROLES.contains(&node.role) && b.act(id, Act::Select).unwrap_or(false) {
        return Ok(Press::Selected);
    }
    if node.role.starts_with("menuitem") {
        return Ok(Press::Acted(b.act(id, Act::Action)?));
    }
    let chain = b.ancestors(id)?;
    if node.role.starts_with("menu") && !node.has("showing") && open_menus(b, &chain) {
        for _ in 0..MENU_POLLS {
            node = b.node(id, Extras::BOUNDS, 0)?;
            if node.has("showing") {
                break;
            }
            std::thread::sleep(MENU_POLL);
        }
    }
    if let Some((x, y)) = reachable(b, id, &node, &chain) {
        return Ok(Press::Click(x, y));
    }
    Ok(Press::Acted(b.act(id, Act::Action)?))
}

fn open_menus<B: Access>(b: &mut B, chain: &[String]) -> bool {
    let roles: Vec<&'static str> = chain
        .iter()
        .map(|id| b.node(id, Extras::default(), 0).map_or("", |n| n.role))
        .collect();
    let start = roles
        .iter()
        .rposition(|r| *r != "menu")
        .map_or(0, |i| i + 1);
    for menu in &chain[start..] {
        let _ = b.act(menu, Act::Action);
    }
    start < chain.len()
}

fn reachable<B: Access>(b: &mut B, id: &str, node: &Node, chain: &[String]) -> Option<(i32, i32)> {
    let [x, y, w, h] = node.bounds?;
    if !node.has("showing") || w <= 0 || h <= 0 || x < 0 || y < 0 {
        return None;
    }
    let (cx, cy) = (x + w / 2, y + h / 2);
    if node.role.starts_with("menu") {
        return Some((cx, cy));
    }
    let inside = chain.iter().all(|a| {
        b.node(a, Extras::BOUNDS, 0).is_ok_and(|n| {
            n.role == "tab"
                || n.bounds.is_none_or(|[bx, by, bw, bh]| {
                    (bx..bx + bw).contains(&cx) && (by..by + bh).contains(&cy)
                })
        })
    });
    if !inside {
        return None;
    }
    let hit = b.at(chain.first()?, cx, cy).ok()??;
    let reaches =
        hit == id || chain.contains(&hit) || b.ancestors(&hit).ok()?.iter().any(|a| a == id);
    reaches.then_some((cx, cy))
}

/// One line per node: `- role "name" [disabled] [checked] [shortcut] [id=ID]: "value"`, indented by depth.
fn outline(nodes: &[Node], max_chars: usize) -> String {
    let mut out = String::new();
    for n in nodes {
        let _ = write!(out, "{}- {}", "  ".repeat(n.depth), n.role);
        if !n.name.is_empty() {
            let _ = write!(out, " \"{}\"", quote(&n.name, max_chars));
        }
        for state in ["disabled", "checked", "mixed"] {
            if n.has(state) {
                let _ = write!(out, " [{state}]");
            }
        }
        if let Some(s) = &n.shortcut {
            let _ = write!(out, " [{s}]");
        }
        if !n.id.is_empty() {
            let _ = write!(out, " [id={}]", n.id);
        }
        if let Some(value) = &n.value {
            let _ = write!(out, ": \"{}\"", quote(value, max_chars));
        }
        out.push('\n');
    }
    out
}

/// A crop of the screen rectangle `[x, y, w, h]` from a screenshot scaled by `factor`, as `(width, height, PNG)`.
fn crop_png(
    png: &[u8],
    [x, y, w, h]: [f64; 4],
    factor: f64,
    mask: Option<&[([f64; 4], bool)]>,
) -> Result<(u32, u32, Vec<u8>), String> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(png));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec
        .read_info()
        .map_err(|e| format!("decode screenshot: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("screenshot too large")?];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("decode screenshot: {e}"))?;
    let channels = info.color_type.samples();
    let clip = |v: f64, max: u32| (v * factor).floor().clamp(0.0, f64::from(max)) as u32;
    let (left, top) = (clip(x, info.width), clip(y, info.height));
    let (right, bottom) = (clip(x + w, info.width), clip(y + h, info.height));
    if right <= left || bottom <= top {
        return Err("the rectangle is off the screen".to_owned());
    }
    let (cw, ch) = (right - left, bottom - top);
    let keep = mask.map(|m| keep_map(m, [left, top, right, bottom]));
    let mut rgb = Vec::with_capacity((cw * ch * 3) as usize);
    for row in top..bottom {
        for col in left..right {
            let i = ((row - top) * cw + col - left) as usize;
            if keep.as_ref().is_some_and(|k| !k[i]) {
                rgb.extend([0; 3]);
                continue;
            }
            let at = (row as usize * info.width as usize + col as usize) * channels;
            let px = &buf[at..at + channels];
            match channels {
                1 | 2 => rgb.extend([px[0]; 3]),
                _ => rgb.extend(&px[..3]),
            }
        }
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, cw, ch);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .and_then(|mut w| w.write_image_data(&rgb))
        .map_err(|e| format!("encode PNG: {e}"))?;
    Ok((cw, ch, out))
}

fn keep_map(mask: &[([f64; 4], bool)], [left, top, right, bottom]: [u32; 4]) -> Vec<bool> {
    let cw = (right - left) as usize;
    let mut keep = vec![false; cw * (bottom - top) as usize];
    for &([x, y, w, h], admitted) in mask {
        let span = |a: f64, b: f64, min: u32, max: u32| {
            let (a, b) = if admitted {
                (a.ceil(), b.floor())
            } else {
                (a.floor(), b.ceil())
            };
            let clip = |v: f64| (v.clamp(f64::from(min), f64::from(max)) as u32 - min) as usize;
            clip(a)..clip(b)
        };
        let cols = span(x, x + w, left, right);
        for row in span(y, y + h, top, bottom) {
            keep[row * cw + cols.start..row * cw + cols.end.max(cols.start)].fill(admitted);
        }
    }
    keep
}

/// The US-layout keys that type `c`, shift first for a shifted character; none for a character Windows types as Unicode.
fn keystroke(c: char) -> Option<Vec<String>> {
    const SHIFTED: &str = "!@#$%^&*()_+:\"<>?~{}|";
    const BASE: &str = "1234567890-=;',./`[]\\";
    match c {
        'a'..='z' | '0'..='9' => Some(vec![c.to_string()]),
        ' ' => Some(vec!["space".to_owned()]),
        'A'..='Z' => Some(vec!["shift".to_owned(), c.to_ascii_lowercase().to_string()]),
        _ => {
            if let Some(i) = SHIFTED.find(c) {
                return BASE[i..]
                    .chars()
                    .next()
                    .map(|k| vec!["shift".to_owned(), k.to_string()]);
            }
            BASE.contains(c).then(|| vec![c.to_string()])
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::model::Tree;

    #[derive(Default)]
    struct Fake {
        nodes: HashMap<String, (Node, Option<String>)>,
        hits: HashMap<(i32, i32), String>,
        acts: Vec<(String, Act)>,
        opened: bool,
    }

    static ACTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    impl Fake {
        fn add(
            &mut self,
            id: &str,
            parent: Option<&str>,
            role: &'static str,
            states: &[&'static str],
            bounds: Option<[i32; 4]>,
        ) {
            let n = Node {
                id: id.into(),
                role,
                name: id.into(),
                states: states.to_vec(),
                bounds,
                ..Node::default()
            };
            self.nodes.insert(id.into(), (n, parent.map(str::to_owned)));
        }
    }

    impl Access for Fake {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            Ok(vec![Frame {
                id: "w".into(),
                title: "Untitled - Notepad".into(),
                role: "window",
                pid: Some(4),
                active: true,
                app: Some("Notepad".into()),
            }])
        }

        fn tree(&mut self, frame: &Frame, extras: Extras) -> Result<Tree, String> {
            MENUS.lock().unwrap().push(extras.menus);
            Ok(Tree {
                frame: frame.clone(),
                ..Tree::default()
            })
        }

        fn node(&mut self, id: &str, _: Extras, _: usize) -> Result<Node, String> {
            let (mut n, _) = self.nodes.get(id).cloned().ok_or("gone")?;
            if self.opened && n.role == "menuitem" {
                n.states.push("showing");
            }
            Ok(n)
        }

        fn ancestors(&mut self, id: &str) -> Result<Vec<String>, String> {
            let mut chain = Vec::new();
            let mut at = self.nodes.get(id).and_then(|(_, p)| p.clone());
            while let Some(p) = at {
                at = self.nodes.get(&p).and_then(|(_, p)| p.clone());
                chain.push(p);
            }
            chain.reverse();
            Ok(chain)
        }

        fn at(&mut self, _: &str, x: i32, y: i32) -> Result<Option<String>, String> {
            Ok(self.hits.get(&(x, y)).cloned())
        }

        fn menu_at(&mut self, x: i32, y: i32) -> Result<Option<String>, String> {
            Ok(self
                .hits
                .get(&(x, y))
                .filter(|id| {
                    self.nodes
                        .get(*id)
                        .is_some_and(|(n, _)| n.role == "menuitem")
                })
                .cloned())
        }

        fn act(&mut self, id: &str, act: Act) -> Result<bool, String> {
            self.acts.push((id.into(), act));
            ACTED.lock().unwrap().push(id.into());
            if id == "dead" {
                return Ok(false);
            }
            if self.nodes.get(id).is_some_and(|(n, _)| n.role == "menu") {
                self.opened = true;
            }
            Ok(act != Act::Select || self.nodes.get(id).is_some_and(|(n, _)| n.role == "tab"))
        }

        fn close(&mut self, _: &Frame) -> Result<(), String> {
            Ok(())
        }
    }

    fn notepad() -> Fake {
        let mut f = Fake::default();
        f.add("w", None, "window", &["showing"], Some([0, 0, 800, 600]));
        f.add(
            "ok",
            Some("w"),
            "button",
            &["showing"],
            Some([100, 100, 80, 20]),
        );
        f.add(
            "hidden",
            Some("w"),
            "button",
            &["showing"],
            Some([300, 100, 80, 20]),
        );
        f.add("tab", Some("w"), "tab", &["showing"], Some([0, 30, 60, 20]));
        f.add(
            "file",
            Some("w"),
            "menu",
            &["showing"],
            Some([0, 0, 40, 20]),
        );
        f.add("save", Some("file"), "menuitem", &[], None);
        f.hits.insert((140, 110), "ok".into());
        f.hits.insert((340, 110), "w".into());
        f
    }

    #[test]
    fn a_shown_control_that_a_hit_test_finds_takes_a_click() {
        let mut f = notepad();
        assert_eq!(plan_press(&mut f, "ok").unwrap(), Press::Click(140, 110));
        assert!(f.acts.is_empty());
    }

    #[test]
    fn a_covered_control_runs_its_action() {
        let mut f = notepad();
        f.hits.insert((340, 110), "elsewhere".into());
        f.add("elsewhere", None, "button", &[], None);
        assert_eq!(plan_press(&mut f, "hidden").unwrap(), Press::Acted(true));
        assert_eq!(f.acts, [("hidden".to_owned(), Act::Action)]);
    }

    #[test]
    fn a_tab_is_selected() {
        let mut f = notepad();
        assert_eq!(plan_press(&mut f, "tab").unwrap(), Press::Selected);
    }

    #[test]
    fn a_macos_menu_entry_runs_its_action_without_opening_its_menu() {
        let mut f = notepad();
        assert_eq!(plan_press(&mut f, "save").unwrap(), Press::Acted(true));
        assert_eq!(f.acts, [("save".to_owned(), Act::Action)]);
    }

    #[test]
    fn a_windows_menu_entry_runs_its_action() {
        let mut f = notepad();
        assert_eq!(plan_press(&mut f, "save").unwrap(), Press::Acted(true));
        assert_eq!(f.acts, [("save".to_owned(), Act::Action)]);
    }

    #[test]
    fn a_window_is_picked_by_exact_then_trimmed_then_contained_title() {
        let frame = |title: &str, active: bool, pid: u32| Frame {
            id: title.into(),
            title: title.into(),
            role: "window",
            pid: Some(pid),
            active,
            app: None,
        };
        let frames = vec![
            frame("Untitled - Notepad", false, 7),
            frame("Save As", true, 7),
            frame("Settings", false, 9),
        ];
        assert_eq!(
            window::pick(frames.clone(), None, None).unwrap().title,
            "Save As"
        );
        assert_eq!(
            window::pick(frames.clone(), Some("settings"), None)
                .unwrap()
                .title,
            "Settings"
        );
        assert_eq!(
            window::pick(frames.clone(), Some("notepad"), Some(7))
                .unwrap()
                .title,
            "Untitled - Notepad"
        );
        assert!(window::pick(frames, Some("Notepad"), Some(9)).is_err());
    }

    #[test]
    fn the_outline_shows_states_shortcuts_ids_and_values() {
        let nodes = vec![
            Node {
                id: "42-1".into(),
                role: "dialog",
                name: "Font".into(),
                ..Node::default()
            },
            Node {
                id: "42-1-7".into(),
                role: "checkbox",
                name: "Bold \"b\"".into(),
                states: vec!["checked"],
                depth: 1,
                parent: Some(0),
                shortcut: Some("ctrl+b".into()),
                ..Node::default()
            },
            Node {
                role: "checkbox",
                name: "Mixed".into(),
                states: vec!["mixed", "disabled"],
                depth: 1,
                parent: Some(0),
                value: Some("12".into()),
                ..Node::default()
            },
        ];
        assert_eq!(
            outline(&nodes, 80),
            "- dialog \"Font\" [id=42-1]\n  - checkbox \"Bold \\\"b\\\"\" [checked] [ctrl+b] [id=42-1-7]\n  - checkbox \"Mixed\" [disabled] [mixed]: \"12\"\n"
        );
    }

    #[test]
    fn a_crop_scales_screen_pixels_to_the_screenshot() {
        let mut png = Vec::new();
        let mut enc = png::Encoder::new(&mut png, 4, 2);
        enc.set_color(png::ColorType::Rgba);
        let mut px = Vec::new();
        for i in 0..8u8 {
            px.extend([i, i, i, 255]);
        }
        enc.write_header().unwrap().write_image_data(&px).unwrap();
        let (w, h, _) = crop_png(&png, [2.0, 0.0, 4.0, 4.0], 0.5, None).unwrap();
        assert_eq!((w, h), (2, 2));
        let (w, h, _) =
            crop_png(&png, [0.0, 0.0, f64::MAX / 4.0, f64::MAX / 4.0], 1.0, None).unwrap();
        assert_eq!((w, h), (4, 2));
        assert!(crop_png(&png, [100.0, 0.0, 4.0, 4.0], 1.0, None).is_err());
    }

    struct Recorder(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Recorder {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(json!({}))
        }
    }

    struct Slow(Arc<Mutex<Vec<String>>>);

    impl Input for Slow {
        async fn call(&self, _: &str, args: Value) -> Result<Value, String> {
            self.0
                .lock()
                .unwrap()
                .push(args["key"].as_str().unwrap_or_default().to_owned());
            tokio::time::sleep(Duration::from_millis(5)).await;
            Ok(json!({}))
        }
    }

    static CLOSED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static MENUS: Mutex<Vec<bool>> = Mutex::new(Vec::new());

    struct Twins;

    impl Access for Twins {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            let frame = |id: &str, active: bool| Frame {
                id: id.into(),
                title: "Untitled - Notepad".into(),
                role: "window",
                pid: Some(4),
                active,
                app: None,
            };
            Ok(vec![frame("front", true), frame("back", false)])
        }

        fn tree(&mut self, _: &Frame, _: Extras) -> Result<Tree, String> {
            Err("no tree".into())
        }

        fn node(&mut self, _: &str, _: Extras, _: usize) -> Result<Node, String> {
            Err("no node".into())
        }

        fn ancestors(&mut self, _: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }

        fn at(&mut self, _: &str, _: i32, _: i32) -> Result<Option<String>, String> {
            Ok(None)
        }

        fn act(&mut self, _: &str, _: Act) -> Result<bool, String> {
            Ok(false)
        }

        fn close(&mut self, frame: &Frame) -> Result<(), String> {
            CLOSED.lock().unwrap().push(frame.id.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn closing_one_of_two_same_titled_windows_closes_the_active_one() {
        let desk = Desk::new(|| Ok(Twins), Slow(Arc::default()), Os::Mac);
        desk.run("desk_close", json!({"window": "Untitled - Notepad"}))
            .await
            .unwrap();
        assert_eq!(*CLOSED.lock().unwrap(), ["front"]);
    }

    #[tokio::test]
    async fn a_snapshot_takes_the_menu_bar_only_when_asked_and_find_always_searches_it() {
        let desk = Desk::new(|| Ok(notepad()), Slow(Arc::default()), Os::Mac);
        desk.run("desk_snapshot", json!({})).await.unwrap();
        desk.run("desk_snapshot", json!({"menus": true}))
            .await
            .unwrap();
        desk.run("desk_find", json!({"name": "Save"}))
            .await
            .unwrap();
        assert_eq!(*MENUS.lock().unwrap(), [false, true, true]);
    }

    #[tokio::test]
    async fn two_typing_calls_at_once_type_one_after_the_other() {
        let keys = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(notepad()), Slow(keys.clone()), Os::Windows);
        let (a, b) = tokio::join!(
            desk.run("desk_type", json!({"text": "aaa"})),
            desk.run("desk_type", json!({"text": "bbb"}))
        );
        a.unwrap();
        b.unwrap();
        let got = keys.lock().unwrap().join("");
        assert!(got == "aaabbb" || got == "bbbaaa", "{got}");
    }

    #[tokio::test]
    async fn an_action_waits_a_bounded_time_for_a_running_one_then_says_the_desk_is_busy() {
        let keys = Arc::new(Mutex::new(Vec::new()));
        let mut desk = Desk::new(|| Ok(notepad()), Slow(keys.clone()), Os::Windows);
        desk.turn_wait = Duration::from_millis(50);
        let long = "a".repeat(100);
        let (typed, busy) = tokio::join!(desk.run("desk_type", json!({"text": long})), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            desk.run("desk_key", json!({"keys": "ctrl+s"})).await
        });
        typed.unwrap();
        assert_eq!(
            busy.unwrap_err(),
            "desk busy: a previous action is still running"
        );
        assert_eq!(keys.lock().unwrap().join(""), long);
    }

    #[tokio::test]
    async fn keys_and_text_go_to_the_desktop_in_driver_names() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(notepad()), Recorder(calls.clone()), Os::Mac);
        desk.run("desk_key", json!({"keys": "Super+Shift+S"}))
            .await
            .unwrap();
        desk.run("desk_type", json!({"text": "a\nb"}))
            .await
            .unwrap();
        let got = calls.lock().unwrap().clone();
        assert_eq!(
            got,
            [
                (
                    "hotkey".to_owned(),
                    json!({"keys": ["cmd", "shift", "s"], "scope": "desktop"})
                ),
                (
                    "type_text".to_owned(),
                    json!({"text": "a", "scope": "desktop"})
                ),
                (
                    "press_key".to_owned(),
                    json!({"key": "return", "scope": "desktop"})
                ),
                (
                    "type_text".to_owned(),
                    json!({"text": "b", "scope": "desktop"})
                ),
            ]
        );
    }

    #[tokio::test]
    async fn a_press_click_goes_through_the_driver() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(notepad()), Recorder(calls.clone()), Os::Windows);
        let out = desk.run("desk_press", json!({"id": "ok"})).await.unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out.text).unwrap()["how"],
            "click"
        );
        assert_eq!(
            calls.lock().unwrap().clone(),
            [(
                "click".to_owned(),
                json!({"x": 140, "y": 110, "scope": "desktop"})
            )]
        );
    }

    struct Screen(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Screen {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args.clone()));
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 2.0}}),
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 7, "window_id": 40, "layer": 0, "z_index": 5, "bounds": {"x": 100.0, "y": 50.0, "width": 300.0, "height": 200.0}},
                    {"pid": 8, "window_id": 41, "layer": 0, "z_index": 9, "bounds": {"x": 140.0, "y": 60.0, "width": 50.0, "height": 50.0}},
                    {"pid": 9, "window_id": 42, "layer": 25, "z_index": 30, "bounds": {"x": 0.0, "y": 0.0, "width": 2000.0, "height": 30.0}}
                ]}}),
                "get_window_state" if args["window_id"] == 40 => json!({"structuredContent": {
                    "screenshot_width": 450, "screenshot_height": 300,
                    "window_bounds": {"x": 100.0, "y": 50.0, "width": 300.0, "height": 200.0}
                }}),
                "get_window_state" => json!({"structuredContent": {
                    "screenshot_width": 100, "screenshot_height": 100,
                    "window_bounds": {"x": 140.0, "y": 60.0, "width": 50.0, "height": 50.0}
                }}),
                _ => json!({}),
            })
        }
    }

    #[derive(Default)]
    struct Reads {
        reads: Mutex<usize>,
        clicks: Mutex<usize>,
        forget: Mutex<bool>,
    }

    impl Input for Arc<Reads> {
        async fn call(&self, tool: &str, _: Value) -> Result<Value, String> {
            Ok(match tool {
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 7, "window_id": 40, "layer": 0, "z_index": 5, "bounds": {"x": 0.0, "y": 0.0, "width": 300.0, "height": 200.0}}
                ]}}),
                "get_window_state" => {
                    *self.reads.lock().unwrap() += 1;
                    json!({"structuredContent": {
                        "screenshot_width": 300, "screenshot_height": 200,
                        "window_bounds": {"x": 0.0, "y": 0.0, "width": 300.0, "height": 200.0}
                    }})
                }
                "click" => {
                    *self.clicks.lock().unwrap() += 1;
                    if std::mem::take(&mut *self.forget.lock().unwrap()) {
                        return Err("screenshot_context_missing: window_never_read".into());
                    }
                    json!({})
                }
                _ => json!({}),
            })
        }
    }

    struct Sheet(Arc<Mutex<Vec<Value>>>);

    impl Input for Sheet {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            Ok(match tool {
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 7, "window_id": 491, "layer": 0, "z_index": 7, "bounds": {"x": 0.0, "y": 0.0, "width": 390.0, "height": 332.0}}
                ]}}),
                "get_window_state" => json!({"structuredContent": {
                    "screenshot_width": 390, "screenshot_height": 332,
                    "window_bounds": {"x": 0.0, "y": 0.0, "width": 390.0, "height": 332.0},
                    "background_input": {"routes": [
                        {"route": "accessibility", "status": "refused"},
                        {"route": "window_pointer", "status": "refused"}
                    ]}
                }}),
                "click" => {
                    self.0.lock().unwrap().push(args);
                    json!({})
                }
                _ => json!({}),
            })
        }
    }

    #[tokio::test]
    async fn a_macos_click_into_a_window_the_driver_refuses_pointer_input_for_goes_to_the_desktop()
    {
        let clicks = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(notepad()), Sheet(clicks.clone()), Os::Mac);
        desk.run("desk_click", json!({"x": 20.0, "y": 30.0}))
            .await
            .unwrap();
        assert_eq!(
            *clicks.lock().unwrap(),
            [json!({"x": 20.0, "y": 30.0, "scope": "desktop"})]
        );
    }

    #[tokio::test]
    async fn macos_clicks_into_one_unmoved_window_read_it_once_and_again_when_the_driver_forgot_it()
    {
        let driver = Arc::new(Reads::default());
        let desk = Desk::new(|| Ok(notepad()), driver.clone(), Os::Mac);
        let click = || desk.run("desk_click", json!({"x": 10.0, "y": 10.0}));
        click().await.unwrap();
        click().await.unwrap();
        *driver.forget.lock().unwrap() = true;
        click().await.unwrap();
        assert_eq!(
            (
                *driver.reads.lock().unwrap(),
                *driver.clicks.lock().unwrap()
            ),
            (2, 4)
        );
    }

    #[tokio::test]
    async fn a_macos_click_on_an_open_menu_item_runs_the_item_instead_of_clicking_the_window_below()
    {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let open = || {
            let mut f = notepad();
            f.add("wrap", Some("file"), "menuitem", &["showing"], None);
            f.hits.insert((300, 200), "wrap".into());
            Ok(f)
        };
        let desk = Desk::new(open, Screen(calls.clone()), Os::Mac);
        desk.run("desk_click", json!({"x": 300.0, "y": 200.0}))
            .await
            .unwrap();
        assert!(ACTED.lock().unwrap().iter().any(|id| id == "wrap"));
        assert!(!calls.lock().unwrap().iter().any(|(t, _)| t == "click"));
    }

    #[tokio::test]
    async fn a_macos_click_on_a_menu_item_that_takes_no_action_fails() {
        let open = || {
            let mut f = notepad();
            f.add("dead", Some("file"), "menuitem", &["showing"], None);
            f.hits.insert((300, 200), "dead".into());
            Ok(f)
        };
        let desk = Desk::new(open, Screen(Arc::default()), Os::Mac);
        let out = desk
            .run("desk_click", json!({"x": 300.0, "y": 200.0}))
            .await;
        assert!(out.is_err(), "{out:?}");
    }

    struct Apps;

    impl Access for Apps {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            let frame = |id: &str, title: &str, pid: u32, active: bool, app: &str| Frame {
                id: id.into(),
                title: title.into(),
                role: "window",
                pid: Some(pid),
                active,
                app: Some(app.into()),
            };
            Ok(vec![
                frame("4/1", "Untitled - Notepad", 4, true, "Notepad.exe"),
                frame("8/1", "Inbox", 8, false, "Mail"),
            ])
        }

        fn tree(&mut self, frame: &Frame, _: Extras) -> Result<Tree, String> {
            Ok(Tree {
                frame: frame.clone(),
                nodes: Vec::new(),
                truncated: false,
                phases: Vec::new(),
            })
        }

        fn node(&mut self, id: &str, _: Extras, _: usize) -> Result<Node, String> {
            match id {
                "4/5" => Ok(Node {
                    id: id.into(),
                    role: "button",
                    states: vec!["showing"],
                    bounds: Some([680, 190, 40, 20]),
                    ..Node::default()
                }),
                _ => Err(format!("no node {id}")),
            }
        }

        fn ancestors(&mut self, _: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }

        fn at(&mut self, _: &str, x: i32, y: i32) -> Result<Option<String>, String> {
            Ok(((x, y) == (5, 5)).then(|| "8/3".into()))
        }

        fn menu_at(&mut self, x: i32, y: i32) -> Result<Option<String>, String> {
            Ok(((x, y) == (100, 800)).then(|| "8/9".into()))
        }

        fn pid_of(&mut self, id: &str) -> Result<Option<u32>, String> {
            Ok(id.split('/').next().and_then(|p| p.parse().ok()))
        }

        fn act(&mut self, _: &str, _: Act) -> Result<bool, String> {
            Ok(true)
        }

        fn close(&mut self, _: &Frame) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn an_app_allowlist_hides_and_refuses_the_windows_of_other_apps() {
        let desk = Desk::new(|| Ok(Apps), Screen(Arc::default()), Os::Mac)
            .allow(vec!["NOTEPAD.EXE".into()]);
        let refused = |r: Result<Output, String>| {
            r.err()
                .is_some_and(|e| e.contains("not in the app allowlist"))
        };
        let listed = desk.run("desk_windows", json!({})).await.unwrap().text;
        assert!(
            listed.contains("Notepad") && !listed.contains("Inbox"),
            "{listed}"
        );
        assert!(
            desk.run("desk_snapshot", json!({"window": "Notepad"}))
                .await
                .is_ok()
        );
        assert!(
            desk.run("desk_snapshot", json!({"window": "Inbox"}))
                .await
                .is_err_and(|e| e.ends_with("windows: [\"Untitled - Notepad\"]"))
        );
        assert!(refused(desk.run("desk_press", json!({"id": "8/5"})).await));
        assert!(!refused(desk.run("desk_press", json!({"id": "4/5"})).await));
        assert!(refused(
            desk.run("desk_click", json!({"x": 300.0, "y": 200.0}))
                .await
        ));
        assert!(refused(
            desk.run("desk_launch", json!({"app": "Mail"})).await
        ));
        assert!(
            desk.run("desk_key", json!({"keys": "ctrl+s"}))
                .await
                .is_ok()
        );
        let mail =
            Desk::new(|| Ok(Apps), Screen(Arc::default()), Os::Mac).allow(vec!["mail".into()]);
        assert!(refused(
            mail.run("desk_key", json!({"keys": "ctrl+s"})).await
        ));
        assert!(refused(mail.run("desk_type", json!({"text": "x"})).await));
    }

    struct Desktop(Arc<Mutex<Vec<String>>>);

    impl Input for Desktop {
        async fn call(&self, tool: &str, _: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push(tool.to_owned());
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 1.0}}),
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 99, "window_id": 3, "layer": 0, "z_index": 3, "title": "Cua.AgentCursorOverlay.default", "bounds": {"x": 0.0, "y": 0.0, "width": 1000.0, "height": 1000.0}},
                    {"pid": 4, "window_id": 1, "layer": 0, "z_index": 1, "bounds": {"x": 0.0, "y": 0.0, "width": 1000.0, "height": 1000.0}},
                    {"pid": 8, "window_id": 2, "layer": 0, "z_index": 2, "bounds": {"x": 500.0, "y": 0.0, "width": 500.0, "height": 500.0}}
                ]}}),
                "get_desktop_state" => {
                    let mut png = Vec::new();
                    let mut enc = png::Encoder::new(&mut png, 10, 10);
                    enc.set_color(png::ColorType::Rgb);
                    enc.write_header()
                        .unwrap()
                        .write_image_data(&[255; 300])
                        .unwrap();
                    let mut v = json!({"structuredContent": {"screenshot_width": 10, "screenshot_original_width": 1000}});
                    v["content"] = json!([{"type": "image", "data": B64.encode(&png)}]);
                    v
                }
                _ => json!({}),
            })
        }
    }

    fn pixels(out: &Output) -> Vec<[u8; 3]> {
        let png = B64.decode(out.image_b64.as_deref().unwrap()).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf[..info.buffer_size()]
            .chunks(3)
            .map(|p| [p[0], p[1], p[2]])
            .collect()
    }

    #[tokio::test]
    async fn an_app_allowlist_gates_every_point_and_element_a_call_reaches() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk =
            Desk::new(|| Ok(Apps), Desktop(calls.clone()), Os::Mac).allow(vec!["notepad".into()]);
        let refused = |r: Result<Output, String>| {
            r.err()
                .is_some_and(|e| e.contains("not in the app allowlist"))
        };
        assert!(refused(desk.run("desk_at", json!({"x": 5, "y": 5})).await));
        assert!(refused(
            desk.run("desk_act", json!({"id": "4/5", "action": "click"}))
                .await
        ));
        assert!(refused(
            desk.run("desk_click", json!({"x": 100.0, "y": 800.0}))
                .await
        ));
        assert!(!calls.lock().unwrap().iter().any(|t| t == "click"));
        let windows = Desk::new(|| Ok(Apps), Desktop(calls.clone()), Os::Windows)
            .allow(vec!["notepad".into()]);
        windows
            .run("desk_click", json!({"x": 100.0, "y": 800.0}))
            .await
            .unwrap();
        assert!(calls.lock().unwrap().iter().any(|t| t == "click"));
        let missed = desk
            .run("desk_snapshot", json!({"window": "Nope"}))
            .await
            .unwrap_err();
        assert!(
            missed.contains("Untitled - Notepad") && !missed.contains("Inbox"),
            "{missed}"
        );
    }

    #[tokio::test]
    async fn an_app_allowlist_blacks_out_the_screen_outside_its_windows_and_costs_nothing_without_one()
     {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let open = Desk::new(|| Ok(Apps), Desktop(calls.clone()), Os::Windows);
        let all = pixels(&open.run("desk_shot", json!({})).await.unwrap());
        assert!(all.iter().all(|p| *p == [255; 3]));
        assert_eq!(*calls.lock().unwrap(), ["get_desktop_state"]);
        let masked = Desk::new(|| Ok(Apps), Desktop(Arc::default()), Os::Windows)
            .allow(vec!["notepad".into()]);
        let shot = pixels(&masked.run("desk_shot", json!({})).await.unwrap());
        assert_eq!((shot[2], shot[7], shot[97]), ([255; 3], [0; 3], [255; 3]));
        let crop = masked
            .run(
                "desk_shot",
                json!({"x": 600.0, "y": 0.0, "w": 400.0, "h": 400.0}),
            )
            .await
            .unwrap();
        assert!(pixels(&crop).iter().all(|p| *p == [0; 3]));
    }

    #[tokio::test]
    async fn a_macos_click_targets_the_top_window_under_the_point_in_the_pixels_of_its_screenshot()
    {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(notepad()), Screen(calls.clone()), Os::Mac);
        desk.run("desk_click", json!({"x": 300.0, "y": 200.0}))
            .await
            .unwrap();
        desk.run("desk_click", json!({"x": 260.0, "y": 300.0, "count": 2}))
            .await
            .unwrap();
        desk.run("desk_click", json!({"x": 5000.0, "y": 5000.0}))
            .await
            .unwrap();
        let sent: Vec<(String, Value)> = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _)| t == "click" || t == "get_window_state" || t == "bring_to_front")
            .cloned()
            .collect();
        let window =
            |pid: u32, id: u32| json!({"session": WINDOW_SESSION, "pid": pid, "window_id": id});
        assert_eq!(
            sent,
            [
                (
                    "bring_to_front".to_owned(),
                    json!({"pid": 8, "window_id": 41})
                ),
                ("get_window_state".to_owned(), window(8, 41)),
                (
                    "click".to_owned(),
                    json!({"x": 20.0, "y": 80.0, "session": WINDOW_SESSION, "pid": 8, "window_id": 41})
                ),
                (
                    "bring_to_front".to_owned(),
                    json!({"pid": 7, "window_id": 40})
                ),
                ("get_window_state".to_owned(), window(7, 40)),
                (
                    "click".to_owned(),
                    json!({"x": 45.0, "y": 150.0, "count": 2, "session": WINDOW_SESSION, "pid": 7, "window_id": 40})
                ),
                (
                    "click".to_owned(),
                    json!({"x": 5000.0, "y": 5000.0, "scope": "desktop"})
                ),
            ]
        );
        let sizes = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _)| t == "get_screen_size")
            .count();
        assert_eq!(sizes, 1);
    }

    #[tokio::test]
    async fn a_window_names_its_app_when_the_backend_knows_it() {
        let desk = Desk::new(|| Ok(notepad()), Screen(Arc::default()), Os::Mac);
        let out = desk.run("desk_windows", json!({})).await.unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out.text).unwrap(),
            json!([{"id": "w", "title": "Untitled - Notepad", "role": "window", "pid": 4, "active": true, "app": "Notepad"}])
        );
    }

    #[test]
    fn windows_characters_map_to_us_layout_keys() {
        let k = |c| keystroke(c).map(|v| v.join("+"));
        assert_eq!(k('a').as_deref(), Some("a"));
        assert_eq!(k('Q').as_deref(), Some("shift+q"));
        assert_eq!(k(' ').as_deref(), Some("space"));
        assert_eq!(k('!').as_deref(), Some("shift+1"));
        assert_eq!(k(':').as_deref(), Some("shift+;"));
        assert_eq!(k('|').as_deref(), Some("shift+\\"));
        assert_eq!(k('/').as_deref(), Some("/"));
        assert_eq!(k('\\').as_deref(), Some("\\"));
        assert_eq!(k('é'), None);
    }
}
