//! The desk calls: snapshots and element actions through the OS backend, input, windows, launch and
//! capture through cua-driver; the same verbs and JSON as the Linux toolhost.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use desk_core::args::{
    ActArgs, AtArgs, ClickArgs, DragArgs, FindArgs, IdArgs, KeyArgs, LaunchArgs, ReadArgs,
    ScrollArgs, ShotArgs, SnapArgs, StillArgs, TypeArgs, WaitArgs, WindowArgs, parse,
};
use desk_core::text::{MAX_FOUND, NAME_CHARS, matches, ms, quote};
use desk_core::wire::Output;
use serde_json::{Map, Value, json};

use crate::allow::{Allow, app_key, app_of};
use crate::driver::{Input, WINDOW_SESSION, desktop, text_of};
use crate::keys::{Os, key_names, keystroke};
use crate::model::{Access, Act, Extras, Frame, Node};
use crate::press::{Press, plan_press};
use crate::shot::crop_png;
use crate::still::{Frame as StillFrame, Watch};
use crate::worker::Worker;

pub const TURN_WAIT: Duration = Duration::from_secs(15);
pub const ACTIONS: [&str; 10] = [
    "desk_press",
    "desk_act",
    "desk_key",
    "desk_type",
    "desk_click",
    "desk_scroll",
    "desk_drag",
    "desk_focus",
    "desk_close",
    "desk_launch",
];

const CALL_LIMIT: Duration = Duration::from_secs(3);
const TREE_LIMIT: Duration = Duration::from_secs(10);
const READ_CHARS: usize = 64 << 10;
const WAIT_MAX: Duration = Duration::from_secs(5);
const WAIT_POLL: Duration = Duration::from_millis(10);
const WAIT_CHECK: Duration = Duration::from_millis(100);
const NO_WINDOW_UNDER: &str =
    "no window under the point, and the app allowlist admits only windows";
const NO_WINDOWSERVER_CONNECTION: &str = "SLPSPostEventRecordTo returned 1002";
const WINDOW_NEVER_READ: &str = "window_never_read";
const OK: &str = r#"{"ok":true}"#;
const SCROLL_AMOUNT: u32 = 3;
const STILL_TIMEOUT: Duration = Duration::from_millis(600);
const STILL_MAX: Duration = Duration::from_secs(5);
const STILL_HOLD: Duration = Duration::from_millis(40);
const STILL_POLL: Duration = Duration::from_millis(10);
const STILL_EDGE: u32 = 320;

type Turn = Arc<tokio::sync::OwnedMutexGuard<()>>;

/// The desk of one OS: `B` reads and acts on elements, `D` sends input.
pub struct Desk<B, D> {
    access: Worker<B>,
    driver: D,
    os: Os,
    acting: Arc<tokio::sync::Mutex<()>>,
    turn_wait: Duration,
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
            read: std::sync::Mutex::new(None),
            allow: Allow::default(),
        }
    }

    /// Limits the desk calls to windows of `apps`, matched by app name or executable, case-insensitive; empty allows every app.
    pub fn allow(mut self, apps: &[String]) -> Self {
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
            "desk_wait" => self.wait(parse(args)?).await?,
            "desk_snapshot" => self.snapshot(parse(args)?).await?,
            "desk_find" => self.find(parse(args)?).await?,
            "desk_read" => self.read(parse(args)?).await?,
            "desk_press" => self.press(parse::<IdArgs>(args)?.id, turn.clone()).await?,
            "desk_act" => self.act(parse(args)?, turn.clone()).await?,
            "desk_at" => self.at(parse(args)?).await?,
            "desk_key" => self.key(&parse::<KeyArgs>(args)?.keys).await?,
            "desk_type" => self.type_text(&parse::<TypeArgs>(args)?.text).await?,
            "desk_click" => self.click(parse(args)?).await?,
            "desk_scroll" => self.scroll(parse(args)?).await?,
            "desk_drag" => self.drag(parse(args)?).await?,
            "desk_still" => self.still(parse(args)?).await?,
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
        let frames = self.listed().await?;
        serde_json::to_string(&frames).map_err(|e| format!("encode windows: {e}"))
    }

    async fn listed(&self) -> Result<Vec<Frame>, String> {
        let allow = self.allow.clone();
        self.access
            .run(CALL_LIMIT, (), move |b| {
                let mut frames = b.frames()?;
                frames.retain(|f| allow.admits(f.app.as_deref()));
                frames.sort_by_key(|f| !f.active);
                Ok(frames)
            })
            .await
    }

    /// Waits until the window list differs from `since` or `timeout_ms` passes.
    async fn wait(&self, a: WaitArgs) -> Result<String, String> {
        let limit = Duration::from_millis(a.timeout_ms).min(WAIT_MAX);
        let since = match a.since {
            Value::Array(ws) => Some(
                ws.into_iter()
                    .filter(|w| self.allow.admits(w["app"].as_str()))
                    .collect::<Value>(),
            ),
            _ => None,
        };
        let started = Instant::now();
        let mut last = Duration::ZERO;
        let mut hint = self.access.run(CALL_LIMIT, (), |b| Ok(b.hint())).await?;
        let mut checked = Instant::now();
        loop {
            tokio::time::sleep(WAIT_POLL).await;
            let now = self.access.run(CALL_LIMIT, (), |b| Ok(b.hint())).await?;
            let over = started.elapsed() >= limit;
            let moved = now.is_some() && now != hint;
            let due = checked.elapsed()
                >= if now.is_some() {
                    last.max(WAIT_CHECK)
                } else {
                    last
                };
            if !over && !moved && !due {
                continue;
            }
            hint = now;
            let at = Instant::now();
            let frames = self.listed().await?;
            last = at.elapsed();
            checked = Instant::now();
            let windows =
                serde_json::to_value(&frames).map_err(|e| format!("encode windows: {e}"))?;
            let same = since.as_ref() == Some(&windows);
            if !same || over {
                let mut out = json!({"changed": !same});
                out["windows"] = windows;
                return Ok(out.to_string());
            }
        }
    }

    async fn admit(&self, tool: &str, args: &Value) -> Result<(), String> {
        let id = match tool {
            "desk_launch" => return self.allow.check(args["app"].as_str()),
            "desk_read" | "desk_press" | "desk_act" => {
                Some(args["id"].as_str().ok_or("no element id")?.to_owned())
            }
            "desk_key" | "desk_type" => None,
            _ => return Ok(()),
        };
        let allow = self.allow.clone();
        self.access
            .run(CALL_LIMIT, (), move |b| match id {
                Some(id) => {
                    let pid = b.pid_of(&id)?;
                    check_process(b, &allow, pid)
                }
                None => allow.pick(b.frames()?, None, None).map(drop),
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
                let frame = live(allow.pick(frames(b, pid, &allow)?, window.as_deref(), pid)?)?;
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
                let frame = live(allow.pick(frames(b, pid, &allow)?, window.as_deref(), pid)?)?;
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
                return Ok(OK.to_owned());
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
                let frames = frames(b, a.pid, &allow)?;
                let kept = (!allow.is_open()).then(|| frames.clone());
                let frame = live(allow.pick(frames, a.window.as_deref(), a.pid)?)?;
                let Some(hit) = b.at(&frame.id, a.x, a.y)? else {
                    return Ok(json!({"found": null, "window": frame}).to_string());
                };
                if let Some(frames) = &kept {
                    let pid = b.pid_of(&hit)?;
                    allow.check(app_of(frames, pid))?;
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
                let frame = live(allow.pick(frames(b, a.pid, &allow)?, Some(&a.window), a.pid)?)?;
                b.close(&frame)?;
                Ok(json!({"ok": true, "title": frame.title}).to_string())
            })
            .await
    }

    async fn key(&self, combo: &str) -> Result<String, String> {
        let (tool, req) = key_request(key_names(combo, self.os)?);
        self.driver.call(tool, desktop(req)).await?;
        Ok(OK.to_owned())
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
            for (at, c) in line.char_indices() {
                let (tool, req) = match keystroke(c) {
                    Some(keys) => key_request(keys),
                    None => ("type_text", json!({"text": &line[at..at + c.len_utf8()]})),
                };
                self.driver.call(tool, desktop(req)).await?;
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
        Ok(OK.to_owned())
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
                        check_process(b, &allow, pid)?;
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
        if !self.allow.is_open()
            && let Some((x, y)) = point
        {
            self.check_point(x, y, under.as_ref()).await?;
        }
        if self.os == Os::Mac
            && let Some(w) = under
        {
            return self.click_window(req, &w).await;
        }
        self.driver.call("click", desktop(req)).await?;
        Ok(())
    }

    /// Refuses a point whose window, or the process on top there, the allowlist does not admit.
    async fn check_point(&self, x: f64, y: f64, under: Option<&Under>) -> Result<(), String> {
        let pid = u32::try_from(under.ok_or(NO_WINDOW_UNDER)?.pid).ok();
        let allow = self.allow.clone();
        self.access
            .run(CALL_LIMIT, (), move |b| {
                check_process(b, &allow, pid)?;
                match b.pid_at(x as i32, y as i32)? {
                    Some(top) if Some(top) != pid => check_process(b, &allow, Some(top)),
                    _ => Ok(()),
                }
            })
            .await
    }

    async fn admit_point(&self, x: f64, y: f64) -> Result<(), String> {
        if self.allow.is_open() {
            return Ok(());
        }
        let under = self.window_at(x, y).await?;
        self.check_point(x, y, under.as_ref()).await
    }

    /// Wheel notches at a screen point; without one, scroll keys to the front app window (the driver's topmost, or the one an allowlist picks and checks), or on Windows, whose apps drop posted scroll keys, wheel notches at its center.
    async fn scroll(&self, a: ScrollArgs) -> Result<String, String> {
        let mut req =
            json!({"direction": a.direction, "amount": a.amount.unwrap_or(SCROLL_AMOUNT)});
        let (x, y) = match (a.x, a.y) {
            (Some(x), Some(y)) => (x, y),
            (None, None) => {
                let target = self.front_window().await?;
                if self.os != Os::Windows {
                    req["pid"] = target["pid"].clone();
                    req["window_id"] = target["window_id"].clone();
                    req["session"] = WINDOW_SESSION.into();
                    self.driver.call("scroll", req).await?;
                    return Ok(OK.to_owned());
                }
                let [bx, by, bw, bh] = bounds(&target);
                let (x, y) = (bx + bw / 2.0, by + bh / 2.0);
                let under = self.window_at(x, y).await?;
                if under.is_none_or(|u| {
                    (Some(u.pid), Some(u.id))
                        != (target["pid"].as_i64(), target["window_id"].as_i64())
                }) {
                    return Err("another window covers the front window's center".to_owned());
                }
                (x, y)
            }
            _ => return Err("scroll needs both x and y, or neither".to_owned()),
        };
        self.admit_point(x, y).await?;
        req["x"] = x.into();
        req["y"] = y.into();
        self.driver.call("scroll", desktop(req)).await?;
        Ok(OK.to_owned())
    }

    async fn front_window(&self) -> Result<Value, String> {
        let picked = if self.allow.is_open() {
            None
        } else {
            let allow = self.allow.clone();
            let frame = self
                .access
                .run(CALL_LIMIT, (), move |b| allow.pick(b.frames()?, None, None))
                .await?;
            Some((frame.pid, frame.title))
        };
        self.on_screen()
            .await?
            .into_iter()
            .filter(|w| w["layer"] == 0)
            .filter(|w| {
                picked.as_ref().is_none_or(|(pid, _)| {
                    pid.is_some_and(|p| w["pid"].as_u64() == Some(u64::from(p)))
                })
            })
            .max_by_key(|w| {
                let titled = picked
                    .as_ref()
                    .is_some_and(|(_, t)| w["title"] == t.as_str());
                (titled, w["z_index"].as_i64().unwrap_or(i64::MIN))
            })
            .ok_or_else(|| "no app window is in front to scroll".to_owned())
    }

    /// A press-drag-release from (x, y) to (to_x, to_y) in desk coordinates.
    async fn drag(&self, a: DragArgs) -> Result<String, String> {
        self.admit_point(a.x, a.y).await?;
        self.admit_point(a.to_x, a.to_y).await?;
        let req = json!({"from_x": a.x, "from_y": a.y, "to_x": a.to_x, "to_y": a.to_y});
        self.driver.call("drag", desktop(req)).await?;
        Ok(OK.to_owned())
    }

    /// Waits until no block of the screen changes in a way that counts (see [`Watch`]) for `hold_ms`, at most `timeout_ms`; says whether one changed meanwhile.
    async fn still(&self, a: StillArgs) -> Result<String, String> {
        let limit = a
            .timeout_ms
            .map_or(STILL_TIMEOUT, Duration::from_millis)
            .min(STILL_MAX);
        let hold = a.hold_ms.map_or(STILL_HOLD, Duration::from_millis);
        let started = Instant::now();
        let mut last = self.still_frame().await?;
        let mut watch = Watch::default();
        let (mut since, mut changed) = (Instant::now(), false);
        loop {
            let still = since.elapsed() >= hold;
            if still || started.elapsed() >= limit {
                let waited = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                return Ok(json!({"still": still, "changed": changed, "ms": waited, "volatile": watch.volatile()}).to_string());
            }
            tokio::time::sleep(STILL_POLL).await;
            let now = self.still_frame().await?;
            if watch.counts(&last, &now) {
                (changed, since) = (true, Instant::now());
            }
            last = now;
        }
    }

    async fn still_frame(&self) -> Result<StillFrame, String> {
        let v = self
            .driver
            .call(
                "get_desktop_state",
                json!({"max_image_dimension": STILL_EDGE}),
            )
            .await?;
        let png = v["content"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["type"] == "image")
            .and_then(|c| c["data"].as_str())
            .ok_or("the driver returned no screenshot")?;
        StillFrame::decode(
            &B64.decode(png)
                .map_err(|e| format!("decode capture: {e}"))?,
        )
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
            Err(e) if e.contains(NO_WINDOWSERVER_CONNECTION) => {
                self.driver.call("click", desktop(req)).await?;
            }
            Err(e) if cached.is_some() && e.contains(WINDOW_NEVER_READ) => {
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
        if self.os == Os::Windows {
            return Ok(1.0);
        }
        let v = self.driver.call("get_screen_size", json!({})).await?;
        Ok(v["structuredContent"]["scale_factor"]
            .as_f64()
            .unwrap_or(1.0))
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
        let Value::Array(windows) = listed["structuredContent"]["windows"].take() else {
            return Ok(Vec::new());
        };
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
        let window_id: i64 =
            w.id.parse()
                .map_err(|_| format!("window id {:?} is not a number", w.id))?;
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

/// The windows a call picks from: the app of `pid` alone, unless the allowlist needs every window to judge the call.
fn frames<B: Access>(b: &mut B, pid: Option<u32>, allow: &Allow) -> Result<Vec<Frame>, String> {
    match pid {
        Some(p) if allow.is_open() => b.frames_of(p),
        _ => b.frames(),
    }
}

fn check_process<B: Access>(b: &mut B, allow: &Allow, pid: Option<u32>) -> Result<(), String> {
    let frames = pid.map_or(Ok(Vec::new()), |p| b.frames_of(p))?;
    allow.check(app_of(&frames, pid))
}

fn live(frame: Frame) -> Result<Frame, String> {
    if frame.stale {
        return Err(format!(
            "window list is stale: {:?} did not answer; take a new snapshot",
            frame.title
        ));
    }
    Ok(frame)
}

fn key_request(mut keys: Vec<String>) -> (&'static str, Value) {
    let mut req = json!({});
    if keys.len() == 1
        && let Some(key) = keys.pop()
    {
        req["key"] = key.into();
        return ("press_key", req);
    }
    req["keys"] = keys.into();
    ("hotkey", req)
}

fn bounds(w: &Value) -> [f64; 4] {
    let b = &w["bounds"];
    ["x", "y", "width", "height"].map(|k| b[k].as_f64().unwrap_or(0.0))
}

/// One line per node: `- role "name" [disabled] [checked] [shortcut] [id=ID]: "value"`, indented by depth.
fn outline(nodes: &[Node], max_chars: usize) -> String {
    let mut out = String::new();
    for n in nodes {
        let _ = write!(out, "{:indent$}- {}", "", n.role, indent = n.depth * 2);
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

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::model::Tree;

    const PID_CALLS: [(&str, &str); 4] = [
        ("desk_snapshot", r#"{"pid": 4}"#),
        ("desk_find", r#"{"name": "Save", "pid": 4}"#),
        ("desk_at", r#"{"x": 1, "y": 1, "pid": 4}"#),
        ("desk_close", r#"{"window": "Notepad", "pid": 4}"#),
    ];

    static ACTED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static MENUS: Mutex<Vec<bool>> = Mutex::new(Vec::new());

    #[derive(Default)]
    pub(crate) struct Fake {
        nodes: HashMap<String, (Node, Option<String>)>,
        pub(crate) hits: HashMap<(i32, i32), String>,
        pub(crate) acts: Vec<(String, Act)>,
        opened: bool,
    }

    impl Fake {
        pub(crate) fn add(
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
            Ok(vec![frame(
                "w",
                "Untitled - Notepad",
                4,
                true,
                Some("Notepad"),
            )])
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

    pub(crate) fn notepad() -> Fake {
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

    #[derive(Default)]
    pub(crate) struct Stub {
        frames: Vec<Frame>,
        scoped: bool,
        closed: Vec<String>,
        opens: Option<Opens>,
    }

    impl Stub {
        fn opens(at: usize, hinted: bool, list_ms: u64) -> Self {
            Self {
                opens: Some(Opens {
                    at,
                    hinted,
                    list_ms,
                    ..Opens::default()
                }),
                ..Self::default()
            }
        }
    }

    impl Access for Stub {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            if let Some(o) = &mut self.opens {
                return Ok(o.frames());
            }
            if self.scoped {
                return Err("every app was read".into());
            }
            Ok(self.frames.clone())
        }

        fn frames_of(&mut self, pid: u32) -> Result<Vec<Frame>, String> {
            let frames = if self.scoped {
                self.frames.clone()
            } else {
                self.frames()?
            };
            Ok(frames.into_iter().filter(|f| f.pid == Some(pid)).collect())
        }

        fn hint(&mut self) -> Option<u64> {
            self.opens.as_mut().and_then(Opens::hint)
        }

        fn tree(&mut self, frame: &Frame, _: Extras) -> Result<Tree, String> {
            Ok(Tree {
                frame: frame.clone(),
                ..Tree::default()
            })
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
            self.closed.push(frame.id.clone());
            Ok(())
        }
    }

    #[derive(Default)]
    struct Opens {
        at: usize,
        hinted: bool,
        list_ms: u64,
        t: usize,
        lists: usize,
        seen: Option<usize>,
    }

    impl Opens {
        fn frames(&mut self) -> Vec<Frame> {
            self.lists += 1;
            std::thread::sleep(Duration::from_millis(self.list_ms));
            if !self.hinted {
                self.t += 1;
            }
            if self.t >= self.at {
                self.seen.get_or_insert(self.t);
            }
            if self.t >= self.at {
                vec![
                    frame("save", "Save", 4, true, None),
                    frame("main", "Untitled", 4, false, None),
                ]
            } else {
                vec![frame("main", "Untitled", 4, true, None)]
            }
        }

        fn hint(&mut self) -> Option<u64> {
            self.hinted.then(|| {
                self.t += 1;
                u64::from(self.t >= self.at)
            })
        }
    }

    fn frame(id: &str, title: &str, pid: u32, active: bool, app: Option<&str>) -> Frame {
        Frame {
            id: id.into(),
            title: title.into(),
            role: "window",
            pid: Some(pid),
            active,
            app: app.map(str::to_owned),
            stale: false,
        }
    }

    fn one_app() -> Stub {
        Stub {
            frames: vec![frame("w", "Untitled - Notepad", 4, true, Some("Notepad"))],
            scoped: true,
            ..Stub::default()
        }
    }

    async fn closed<D: Input + Sync>(desk: &Desk<Stub, D>) -> Vec<String> {
        desk.access
            .run(CALL_LIMIT, (), |b| Ok(b.closed.clone()))
            .await
            .unwrap()
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
        scale: Mutex<Option<f64>>,
    }

    impl Input for Arc<Reads> {
        async fn call(&self, tool: &str, _: Value) -> Result<Value, String> {
            Ok(match tool {
                "get_screen_size" => {
                    json!({"structuredContent": {"scale_factor": *self.scale.lock().unwrap()}})
                }
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

    struct NoConnection(Arc<Mutex<Vec<Value>>>);

    impl Input for NoConnection {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            Ok(match tool {
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 7, "window_id": 40, "layer": 0, "z_index": 5, "bounds": {"x": 0.0, "y": 0.0, "width": 300.0, "height": 200.0}}
                ]}}),
                "get_window_state" => json!({"structuredContent": {
                    "screenshot_width": 300, "screenshot_height": 200,
                    "window_bounds": {"x": 0.0, "y": 0.0, "width": 300.0, "height": 200.0}
                }}),
                "click" if args.get("window_id").is_some() => {
                    return Err(
                        "click: Background input is not available for pid 7: the WindowServer \
                        connection is invalid (SLPSPostEventRecordTo returned 1002). Run the \
                        app-bundle daemon: `open -n -g -a CuaDriver --args serve`."
                            .into(),
                    );
                }
                "click" => {
                    self.0.lock().unwrap().push(args);
                    json!({})
                }
                _ => json!({}),
            })
        }
    }

    struct Apps;

    impl Access for Apps {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            Ok(vec![
                frame("4/1", "Untitled - Notepad", 4, true, Some("Notepad.exe")),
                frame("8/1", "Inbox", 8, false, Some("Mail")),
            ])
        }

        fn tree(&mut self, frame: &Frame, _: Extras) -> Result<Tree, String> {
            Ok(Tree {
                frame: frame.clone(),
                ..Tree::default()
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

        fn pid_at(&mut self, x: i32, y: i32) -> Result<Option<u32>, String> {
            Ok(Some(if (x, y) == (50, 600) { 12 } else { 4 }))
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

    struct Desktop(Arc<Mutex<Vec<String>>>);

    impl Input for Desktop {
        async fn call(&self, tool: &str, _: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push(tool.to_owned());
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 1.0}}),
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 98, "window_id": 4, "layer": 0, "z_index": 4, "title": "Cua.AgentCursorOverlay.default", "bounds": {"x": 0.0, "y": 900.0, "width": 100.0, "height": 100.0}},
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

    async fn wait_on<B: Access + 'static>(
        desk: &Desk<B, Slow>,
        timeout_ms: u64,
    ) -> (Value, Duration) {
        let since: Value =
            serde_json::from_str(&desk.run("desk_windows", json!({})).await.unwrap().text).unwrap();
        let started = Instant::now();
        let out = desk
            .run(
                "desk_wait",
                json!({"since": since, "timeout_ms": timeout_ms}),
            )
            .await
            .unwrap();
        (serde_json::from_str(&out.text).unwrap(), started.elapsed())
    }

    async fn lists(desk: &Desk<Stub, Slow>) -> usize {
        desk.access
            .run(CALL_LIMIT, (), |b| {
                Ok(b.opens.as_ref().map_or(0, |o| o.lists))
            })
            .await
            .unwrap()
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

    #[tokio::test]
    async fn closing_one_of_two_same_titled_windows_closes_the_active_one() {
        let twins = || {
            Ok(Stub {
                frames: vec![
                    frame("front", "Untitled - Notepad", 4, true, None),
                    frame("back", "Untitled - Notepad", 4, false, None),
                ],
                ..Stub::default()
            })
        };
        let desk = Desk::new(twins, Slow(Arc::default()), Os::Mac);
        desk.run("desk_close", json!({"window": "Untitled - Notepad"}))
            .await
            .unwrap();
        assert_eq!(closed(&desk).await, ["front"]);
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
    async fn a_window_kept_from_an_earlier_list_is_listed_but_never_acted_on() {
        let busy = || {
            Ok(Stub {
                frames: vec![Frame {
                    stale: true,
                    ..frame("w", "Draft", 4, true, None)
                }],
                ..Stub::default()
            })
        };
        let desk = Desk::new(busy, Slow(Arc::default()), Os::Mac);
        let listed = desk.run("desk_windows", json!({})).await.unwrap();
        assert!(listed.text.contains("Draft"), "{}", listed.text);
        for (tool, args) in [
            ("desk_close", json!({"window": "Draft"})),
            ("desk_snapshot", json!({"window": "Draft"})),
            ("desk_find", json!({"name": "Save", "window": "Draft"})),
        ] {
            let err = desk.run(tool, args).await.unwrap_err();
            assert!(err.contains("stale"), "{tool}: {err}");
        }
        assert!(closed(&desk).await.is_empty());
    }

    #[tokio::test]
    async fn a_call_that_names_a_process_reads_only_that_apps_windows() {
        let desk = Desk::new(|| Ok(one_app()), Slow(Arc::default()), Os::Mac);
        for (tool, args) in PID_CALLS {
            desk.run(tool, serde_json::from_str(args).unwrap())
                .await
                .unwrap();
        }
        assert!(desk.run("desk_snapshot", json!({})).await.is_err());
    }

    #[tokio::test]
    async fn with_an_allowlist_a_call_that_names_a_process_still_reads_every_window() {
        let desk =
            Desk::new(|| Ok(one_app()), Slow(Arc::default()), Os::Mac).allow(&["Notepad".into()]);
        for (tool, args) in PID_CALLS {
            let err = desk
                .run(tool, serde_json::from_str(args).unwrap())
                .await
                .unwrap_err();
            assert_eq!(err, "every app was read", "{tool}");
        }
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

    #[tokio::test]
    async fn a_macos_window_click_without_a_windowserver_connection_goes_to_the_desktop() {
        let clicks = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(notepad()), NoConnection(clicks.clone()), Os::Mac);
        desk.run("desk_click", json!({"x": 20.0, "y": 30.0}))
            .await
            .unwrap();
        assert_eq!(
            *clicks.lock().unwrap(),
            [json!({"x": 20.0, "y": 30.0, "scope": "desktop"})]
        );
    }

    #[tokio::test]
    async fn a_macos_click_into_a_window_refusing_pointer_input_goes_to_the_desktop() {
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
    async fn macos_clicks_read_an_unmoved_window_once_and_again_when_the_driver_forgot_it() {
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
    async fn a_macos_click_takes_the_display_scale_of_the_moment() {
        let driver = Arc::new(Reads::default());
        let desk = Desk::new(|| Ok(notepad()), driver.clone(), Os::Mac);
        let click = || desk.run("desk_click", json!({"x": 500.0, "y": 300.0}));
        click().await.unwrap();
        assert_eq!(*driver.reads.lock().unwrap(), 0);
        *driver.scale.lock().unwrap() = Some(2.0);
        click().await.unwrap();
        assert_eq!(*driver.reads.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn a_macos_click_on_an_open_menu_item_runs_it_instead_of_clicking() {
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

    #[tokio::test]
    async fn an_app_allowlist_hides_and_refuses_the_windows_of_other_apps() {
        let desk =
            Desk::new(|| Ok(Apps), Screen(Arc::default()), Os::Mac).allow(&["NOTEPAD.EXE".into()]);
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
        let mail = Desk::new(|| Ok(Apps), Screen(Arc::default()), Os::Mac).allow(&["mail".into()]);
        assert!(refused(
            mail.run("desk_key", json!({"keys": "ctrl+s"})).await
        ));
        assert!(refused(mail.run("desk_type", json!({"text": "x"})).await));
    }

    #[tokio::test]
    async fn an_app_allowlist_gates_every_point_and_element_a_call_reaches() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk =
            Desk::new(|| Ok(Apps), Desktop(calls.clone()), Os::Mac).allow(&["notepad".into()]);
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
        let windows =
            Desk::new(|| Ok(Apps), Desktop(calls.clone()), Os::Windows).allow(&["notepad".into()]);
        windows
            .run("desk_click", json!({"x": 100.0, "y": 800.0}))
            .await
            .unwrap();
        assert!(refused(
            windows
                .run("desk_click", json!({"x": 50.0, "y": 950.0}))
                .await
        ));
        assert!(refused(
            windows
                .run("desk_click", json!({"x": 50.0, "y": 600.0}))
                .await
        ));
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
    async fn an_app_allowlist_masks_shots_outside_its_windows_and_costs_nothing_without_one() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let open = Desk::new(|| Ok(Apps), Desktop(calls.clone()), Os::Windows);
        let all = pixels(&open.run("desk_shot", json!({})).await.unwrap());
        assert!(all.iter().all(|p| *p == [255; 3]));
        assert_eq!(*calls.lock().unwrap(), ["get_desktop_state"]);
        let masked =
            Desk::new(|| Ok(Apps), Desktop(Arc::default()), Os::Windows).allow(&["notepad".into()]);
        let shot = pixels(&masked.run("desk_shot", json!({})).await.unwrap());
        assert_eq!(
            (shot[2], shot[7], shot[97], shot[90]),
            ([255; 3], [0; 3], [255; 3], [0; 3])
        );
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
    async fn a_macos_click_targets_the_top_window_under_the_point_in_its_pixels() {
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
        assert_eq!(sizes, 3);
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

    #[tokio::test]
    async fn a_wait_returns_the_new_window_list_once_a_window_opens() {
        let desk = Desk::new(
            || Ok(Stub::opens(3, false, 0)),
            Slow(Arc::default()),
            Os::Mac,
        );
        let (out, _) = wait_on(&desk, 2000).await;
        assert_eq!(out["changed"], true);
        assert_eq!(out["windows"][0]["title"], "Save");
        assert_eq!(lists(&desk).await, 3);
    }

    #[tokio::test]
    async fn a_wait_without_a_change_returns_the_same_list_at_its_timeout() {
        let desk = Desk::new(
            || Ok(Stub::opens(usize::MAX, false, 0)),
            Slow(Arc::default()),
            Os::Mac,
        );
        let (out, took) = wait_on(&desk, 150).await;
        assert_eq!(out["changed"], false);
        assert_eq!(out["windows"][0]["title"], "Untitled");
        assert!(took >= Duration::from_millis(150), "{took:?}");
    }

    #[tokio::test]
    async fn slow_window_lists_count_against_a_waits_timeout() {
        let desk = Desk::new(
            || Ok(Stub::opens(usize::MAX, false, 200)),
            Slow(Arc::default()),
            Os::Mac,
        );
        let (out, took) = wait_on(&desk, 300).await;
        assert_eq!(out["changed"], false);
        assert!(took >= Duration::from_millis(300), "{took:?}");
        assert_eq!(lists(&desk).await, 3);
    }

    #[tokio::test]
    async fn a_wait_ended_early_frees_the_desk_at_once() {
        let desk = Arc::new(Desk::new(
            || Ok(Stub::opens(usize::MAX, true, 0)),
            Slow(Arc::default()),
            Os::Mac,
        ));
        let wait = tokio::spawn({
            let desk = desk.clone();
            async move { wait_on(&desk, 5000).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        wait.abort();
        let started = Instant::now();
        desk.run("desk_windows", json!({})).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_hint_change_right_after_a_slow_list_reads_the_list_at_once() {
        let desk = Desk::new(
            || Ok(Stub::opens(12, true, 150)),
            Slow(Arc::default()),
            Os::Mac,
        );
        let (out, _) = wait_on(&desk, 2000).await;
        assert_eq!(out["changed"], true);
        let seen = desk
            .access
            .run(CALL_LIMIT, (), |b| {
                Ok(b.opens.as_ref().and_then(|o| o.seen))
            })
            .await
            .unwrap();
        assert_eq!(seen, Some(12));
    }

    #[tokio::test]
    async fn a_wait_lists_and_compares_only_the_windows_the_allowlist_admits() {
        let desk =
            Desk::new(|| Ok(Apps), Screen(Arc::default()), Os::Mac).allow(&["notepad.exe".into()]);
        let mut since: Value =
            serde_json::from_str(&desk.run("desk_windows", json!({})).await.unwrap().text).unwrap();
        since.as_array_mut().unwrap().push(json!({"id": "8/1", "title": "Inbox", "role": "window", "pid": 8, "active": false, "app": "Mail"}));
        let out = desk
            .run("desk_wait", json!({"since": since, "timeout_ms": 50}))
            .await
            .unwrap();
        let out: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(out["changed"], false);
        assert!(!out.to_string().contains("Inbox"), "{out}");
    }

    #[tokio::test]
    async fn a_steady_hint_spares_full_window_lists() {
        let desk = Desk::new(
            || Ok(Stub::opens(usize::MAX, true, 0)),
            Slow(Arc::default()),
            Os::Mac,
        );
        let (out, _) = wait_on(&desk, 350).await;
        assert_eq!(out["changed"], false);
        assert!(lists(&desk).await <= 5);
    }

    #[tokio::test]
    async fn a_hint_change_reads_the_window_list_at_once() {
        let desk = Desk::new(
            || Ok(Stub::opens(3, true, 0)),
            Slow(Arc::default()),
            Os::Mac,
        );
        let (out, took) = wait_on(&desk, 2000).await;
        assert_eq!(out["changed"], true);
        assert_eq!(lists(&desk).await, 2);
        assert!(took < WAIT_CHECK, "{took:?}");
    }

    struct Wheel(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Wheel {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 2.0}}),
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 4, "window_id": 1, "layer": 0, "z_index": 1, "title": "Untitled - Notepad", "bounds": {"x": 0.0, "y": 0.0, "width": 400.0, "height": 300.0}},
                    {"pid": 8, "window_id": 2, "layer": 0, "z_index": 2, "title": "Inbox", "bounds": {"x": 500.0, "y": 0.0, "width": 300.0, "height": 300.0}},
                    {"pid": 4, "window_id": 3, "layer": 0, "z_index": 3, "title": "Find", "bounds": {"x": 100.0, "y": 400.0, "width": 200.0, "height": 100.0}},
                    {"pid": 98, "window_id": 9, "layer": 25, "z_index": 9, "title": "Menu"}
                ]}}),
                _ => json!({}),
            })
        }
    }

    struct Renamed(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Renamed {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "get_screen_size" => json!({"structuredContent": {"scale_factor": 2.0}}),
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 4, "window_id": 1, "layer": 0, "z_index": 1, "title": "Notepad", "bounds": {"x": 0.0, "y": 0.0, "width": 400.0, "height": 300.0}},
                    {"pid": 4, "window_id": 3, "layer": 0, "z_index": 3, "title": "Find", "bounds": {"x": 100.0, "y": 400.0, "width": 200.0, "height": 100.0}},
                    {"pid": 8, "window_id": 2, "layer": 0, "z_index": 5, "title": "Inbox", "bounds": {"x": 500.0, "y": 0.0, "width": 300.0, "height": 300.0}}
                ]}}),
                _ => json!({}),
            })
        }
    }

    #[tokio::test]
    async fn a_checked_window_the_driver_titles_differently_falls_back_to_its_apps_topmost_window()
    {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk =
            Desk::new(|| Ok(Apps), Renamed(calls.clone()), Os::Windows).allow(&["notepad".into()]);
        desk.run("desk_scroll", json!({"direction": "down"}))
            .await
            .unwrap();
        assert_eq!(
            calls.lock().unwrap().last().cloned().unwrap(),
            (
                "scroll".to_owned(),
                json!({"direction": "down", "amount": 3, "x": 200.0, "y": 450.0, "scope": "desktop"})
            )
        );
    }

    struct Covered(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Covered {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(match tool {
                "list_windows" => json!({"structuredContent": {"windows": [
                    {"pid": 4, "window_id": 1, "layer": 0, "z_index": 1, "title": "Untitled - Notepad", "bounds": {"x": 0.0, "y": 0.0, "width": 400.0, "height": 300.0}},
                    {"pid": 8, "window_id": 2, "layer": 0, "z_index": 2, "title": "Inbox", "bounds": {"x": 150.0, "y": 100.0, "width": 100.0, "height": 100.0}}
                ]}}),
                _ => json!({}),
            })
        }
    }

    #[tokio::test]
    async fn a_pointless_windows_scroll_refuses_a_front_window_another_window_covers() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(|| Ok(Apps), Covered(calls.clone()), Os::Windows)
            .allow(&["notepad".into(), "mail".into()]);
        let err = desk
            .run("desk_scroll", json!({"direction": "down"}))
            .await
            .unwrap_err();
        assert!(err.contains("covers the front window's center"), "{err}");
        assert!(!calls.lock().unwrap().iter().any(|(t, _)| t == "scroll"));
    }

    #[tokio::test]
    async fn a_scroll_wheels_at_its_point_or_at_the_front_window_and_a_drag_spans_two_points() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk = Desk::new(
            || Err::<Fake, _>("no accessibility".to_owned()),
            Wheel(calls.clone()),
            Os::Windows,
        );
        let last = || calls.lock().unwrap().last().cloned().unwrap();
        desk.run(
            "desk_scroll",
            json!({"direction": "down", "x": 10.0, "y": 20.0}),
        )
        .await
        .unwrap();
        assert_eq!(
            last(),
            (
                "scroll".to_owned(),
                json!({"direction": "down", "amount": 3, "x": 10.0, "y": 20.0, "scope": "desktop"})
            )
        );
        desk.run("desk_scroll", json!({"direction": "up", "amount": 5}))
            .await
            .unwrap();
        assert_eq!(
            last(),
            (
                "scroll".to_owned(),
                json!({"direction": "up", "amount": 5, "x": 200.0, "y": 450.0, "scope": "desktop"})
            )
        );
        Desk::new(
            || Err::<Fake, _>("no accessibility".to_owned()),
            Wheel(calls.clone()),
            Os::Mac,
        )
        .run("desk_scroll", json!({"direction": "up", "amount": 5}))
        .await
        .unwrap();
        assert_eq!(
            last(),
            (
                "scroll".to_owned(),
                json!({"direction": "up", "amount": 5, "pid": 4, "window_id": 3, "session": WINDOW_SESSION})
            )
        );
        let allowed =
            Desk::new(|| Ok(Apps), Wheel(calls.clone()), Os::Windows).allow(&["notepad".into()]);
        allowed
            .run("desk_scroll", json!({"direction": "down"}))
            .await
            .unwrap();
        assert_eq!(
            last(),
            (
                "scroll".to_owned(),
                json!({"direction": "down", "amount": 3, "x": 200.0, "y": 150.0, "scope": "desktop"})
            )
        );
        let half = desk
            .run("desk_scroll", json!({"direction": "up", "x": 1.0}))
            .await
            .unwrap_err();
        assert!(half.contains("both x and y"), "{half}");
        desk.run(
            "desk_drag",
            json!({"x": 1.0, "y": 2.0, "to_x": 300.0, "to_y": 400.0}),
        )
        .await
        .unwrap();
        assert_eq!(
            last(),
            (
                "drag".to_owned(),
                json!({"from_x": 1.0, "from_y": 2.0, "to_x": 300.0, "to_y": 400.0, "scope": "desktop"})
            )
        );
    }

    #[tokio::test]
    async fn an_app_allowlist_gates_both_ends_of_a_drag_and_the_point_of_a_scroll() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let desk =
            Desk::new(|| Ok(Apps), Desktop(calls.clone()), Os::Windows).allow(&["notepad".into()]);
        let refused = |r: Result<Output, String>| {
            r.err()
                .is_some_and(|e| e.contains("not in the app allowlist"))
        };
        let drag = |to: (f64, f64)| json!({"x": 100.0, "y": 800.0, "to_x": to.0, "to_y": to.1});
        assert!(refused(desk.run("desk_drag", drag((50.0, 950.0))).await));
        assert!(refused(desk.run("desk_drag", drag((50.0, 600.0))).await));
        assert!(refused(
            desk.run(
                "desk_scroll",
                json!({"direction": "down", "x": 50.0, "y": 950.0})
            )
            .await
        ));
        assert!(
            !calls
                .lock()
                .unwrap()
                .iter()
                .any(|t| t == "drag" || t == "scroll")
        );
        let mail =
            Desk::new(|| Ok(Apps), Desktop(calls.clone()), Os::Windows).allow(&["mail".into()]);
        assert!(refused(
            mail.run("desk_scroll", json!({"direction": "down"})).await
        ));
        desk.run("desk_drag", drag((200.0, 300.0))).await.unwrap();
        desk.run(
            "desk_scroll",
            json!({"direction": "down", "x": 100.0, "y": 800.0}),
        )
        .await
        .unwrap();
        let done = calls.lock().unwrap().clone();
        assert!(done.contains(&"drag".to_owned()) && done.contains(&"scroll".to_owned()));
    }

    struct Frames(Mutex<usize>, fn(usize) -> Vec<(usize, u8)>);

    impl Input for Frames {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            assert_eq!(
                (tool, &args),
                ("get_desktop_state", &json!({"max_image_dimension": 320}))
            );
            let mut n = self.0.lock().unwrap();
            *n += 1;
            let (w, h) = (32, 16);
            let mut rgb = vec![0u8; w * h * 3];
            for (block, tone) in (self.1)(*n) {
                for y in 0..8 {
                    for x in 0..8 {
                        let at = ((y + block / 4 * 8) * w + x + block % 4 * 8) * 3;
                        rgb[at..at + 3].fill(tone);
                    }
                }
            }
            let mut png = Vec::new();
            let mut enc = png::Encoder::new(&mut png, w as u32, h as u32);
            enc.set_color(png::ColorType::Rgb);
            enc.write_header().unwrap().write_image_data(&rgb).unwrap();
            Ok(json!({"content": [{"type": "image", "data": B64.encode(&png)}]}))
        }
    }

    #[tokio::test]
    async fn still_answers_once_the_screen_holds_and_a_churning_region_does_not_hold_it_back() {
        let run = |frames: fn(usize) -> Vec<(usize, u8)>, args: Value| async move {
            let desk = Desk::new(|| Ok(notepad()), Frames(Mutex::new(0), frames), Os::Mac);
            let out = desk.run("desk_still", args).await.unwrap();
            serde_json::from_str::<Value>(&out.text).unwrap()
        };
        let pair = |v: &Value| (v["still"].clone(), v["changed"].clone());
        let calm = run(|_| vec![], json!({"hold_ms": 30})).await;
        assert_eq!(pair(&calm), (json!(true), json!(false)));
        let settles = run(
            |n| vec![(0, if n < 3 { 0 } else { 200 })],
            json!({"hold_ms": 30}),
        )
        .await;
        assert_eq!(pair(&settles), (json!(true), json!(true)));
        let video = run(
            |n| vec![(5, u8::try_from(n * 40 % 250).unwrap())],
            json!({"hold_ms": 30, "timeout_ms": 2000}),
        )
        .await;
        assert_eq!(pair(&video), (json!(true), json!(true)), "{video}");
        assert_eq!(video["volatile"], 1, "{video}");
        assert!(video["ms"].as_u64().unwrap() < 2000, "{video}");
        let short = run(
            |n| vec![(0, if n < 3 { 0 } else { 200 })],
            json!({"hold_ms": 1000, "timeout_ms": 150}),
        )
        .await;
        assert_eq!(pair(&short), (json!(false), json!(true)), "{short}");
        assert!(
            (150..1000).contains(&short["ms"].as_u64().unwrap()),
            "{short}"
        );
    }
}
