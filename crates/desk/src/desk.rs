//! The desk calls: snapshots and element actions through the OS backend, input, windows, launch and
//! capture through cua-driver; the same verbs and JSON as the Linux toolhost.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use desk_core::args::{
    ActArgs, AtArgs, ClickArgs, FindArgs, IdArgs, KeyArgs, LaunchArgs, ReadArgs, ShotArgs,
    SnapArgs, TypeArgs, WindowArgs, parse,
};
use desk_core::press::{MENU_POLL, MENU_POLLS, SELECTED_ROLES};
use desk_core::text::{MAX_FOUND, NAME_CHARS, matches, ms, quote};
use desk_core::window::{self, TITLE_TIERS, app_window, title_matches};
use serde_json::{Map, Value, json};

use crate::driver::{Input, desktop, text_of};
use crate::keys::{Os, key_names};
use crate::model::{Access, Act, Extras, Frame, Node};
use crate::rpc::Output;
use crate::worker::Worker;

const CALL_LIMIT: Duration = Duration::from_secs(3);
const TREE_LIMIT: Duration = Duration::from_secs(10);
const READ_CHARS: usize = 64 << 10;

/// The desk of one OS: `B` reads and acts on elements, `D` sends input.
pub struct Desk<B, D> {
    access: Worker<B>,
    driver: D,
    os: Os,
}

impl<B: Access + 'static, D: Input + Sync> Desk<B, D> {
    /// A desk whose backend `open` makes on its worker thread.
    pub fn new(open: fn() -> Result<B, String>, driver: D, os: Os) -> Self {
        Self {
            access: Worker::new(open),
            driver,
            os,
        }
    }

    /// Runs desk call `tool`, a `desk_<verb>`.
    pub async fn run(&self, tool: &str, args: Value) -> Result<Output, String> {
        let text = match tool {
            "desk_windows" => self.windows().await?,
            "desk_snapshot" => self.snapshot(parse(args)?).await?,
            "desk_find" => self.find(parse(args)?).await?,
            "desk_read" => self.read(parse(args)?).await?,
            "desk_press" => self.press(parse::<IdArgs>(args)?.id).await?,
            "desk_act" => self.act(parse(args)?).await?,
            "desk_at" => self.at(parse(args)?).await?,
            "desk_key" => self.key(&parse::<KeyArgs>(args)?.keys).await?,
            "desk_type" => self.type_text(&parse::<TypeArgs>(args)?.text).await?,
            "desk_click" => self.click(parse(args)?).await?,
            "desk_focus" => self.focus(parse(args)?).await?,
            "desk_close" => self.close(parse(args)?).await?,
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
        let mut frames = self.access.run(CALL_LIMIT, |a| a.frames()).await?;
        frames.sort_by_key(|f| !f.active);
        Ok(Value::Array(frames.iter().map(Frame::json).collect()).to_string())
    }

    async fn snapshot(&self, a: SnapArgs) -> Result<String, String> {
        let started = Instant::now();
        let extras = Extras {
            values: true,
            bounds: a.bounds.unwrap_or(true),
            keys: a.keys,
        };
        let (window, pid) = (a.window, a.pid);
        let (tree, found) = self
            .access
            .run(TREE_LIMIT, move |b| {
                let frame = window::pick(b.frames()?, window.as_deref(), pid)?;
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
            "window": tree.frame.json(),
            "ms": ms(started.elapsed().as_micros()),
            "phases": phases,
            "truncated": tree.truncated,
        });
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
        };
        let (window, pid) = (a.window, a.pid);
        let tree = self
            .access
            .run(TREE_LIMIT, move |b| {
                let frame = window::pick(b.frames()?, window.as_deref(), pid)?;
                b.tree(&frame, extras)
            })
            .await?;
        let found: Vec<Value> = matches(&tree.nodes, &a.name, a.role.as_deref())
            .into_iter()
            .take(MAX_FOUND)
            .map(|i| tree.nodes[i].json(i))
            .collect();
        Ok(json!({"window": tree.frame.json(), "found": found}).to_string())
    }

    async fn read(&self, a: ReadArgs) -> Result<String, String> {
        let extras = Extras {
            values: true,
            bounds: true,
            keys: true,
        };
        let chars = if a.text { READ_CHARS } else { NAME_CHARS };
        let node = self
            .access
            .run(CALL_LIMIT, move |b| b.node(&a.id, extras, chars))
            .await?;
        Ok(node.json(0).to_string())
    }

    async fn press(&self, id: String) -> Result<String, String> {
        let os = self.os;
        match self
            .access
            .run(TREE_LIMIT, move |b| plan_press(b, &id, os))
            .await?
        {
            Press::Selected => Ok(json!({"ok": true, "how": "select"}).to_string()),
            Press::Acted(ok) => Ok(json!({"ok": ok, "how": "action"}).to_string()),
            Press::Click(x, y) => {
                self.driver
                    .call("click", desktop(json!({"x": x, "y": y})))
                    .await?;
                Ok(json!({"ok": true, "how": "click", "x": x, "y": y}).to_string())
            }
        }
    }

    async fn act(&self, a: ActArgs) -> Result<String, String> {
        let act = match a.action.as_str() {
            "action" => Act::Action,
            "select" => Act::Select,
            "focus" => Act::Focus,
            "click" => {
                let shown = Extras {
                    bounds: true,
                    ..Extras::default()
                };
                let n = self
                    .access
                    .run(CALL_LIMIT, move |b| b.node(&a.id, shown, 0))
                    .await?;
                let [x, y, w, h] = n.bounds.ok_or("the element has no screen position")?;
                self.driver
                    .call("click", desktop(json!({"x": x + w / 2, "y": y + h / 2})))
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
            .run(CALL_LIMIT, move |b| b.act(&a.id, act))
            .await?;
        Ok(json!({"ok": ok}).to_string())
    }

    async fn at(&self, a: AtArgs) -> Result<String, String> {
        self.access
            .run(CALL_LIMIT, move |b| {
                let frame = window::pick(b.frames()?, a.window.as_deref(), a.pid)?;
                let Some(hit) = b.at(&frame.id, a.x, a.y)? else {
                    return Ok(json!({"found": null, "window": frame.json()}).to_string());
                };
                let node = b.node(&hit, Extras::default(), 0)?;
                let path: Vec<Value> = b
                    .ancestors(&hit)?
                    .into_iter()
                    .filter_map(|id| {
                        let n = b.node(&id, Extras::default(), 0).ok()?;
                        Some(json!({"id": id, "role": n.role, "name": n.name}))
                    })
                    .collect();
                Ok(json!({"found": node.json(0), "path": path}).to_string())
            })
            .await
    }

    async fn close(&self, a: WindowArgs) -> Result<String, String> {
        self.access
            .run(CALL_LIMIT, move |b| {
                let frame = titled(b.frames()?, &a.window, a.pid)?;
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
        self.driver.call("click", desktop(req)).await?;
        Ok(json!({"ok": true}).to_string())
    }

    async fn focus(&self, a: WindowArgs) -> Result<String, String> {
        let listed = self.driver.call("list_windows", json!({})).await?;
        let windows: Vec<Frame> = listed["structuredContent"]["windows"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|w| Frame {
                id: w["window_id"].to_string(),
                title: w["title"].as_str().unwrap_or_default().to_owned(),
                pid: w["pid"].as_u64().and_then(|p| u32::try_from(p).ok()),
                ..Frame::default()
            })
            .collect();
        let w = titled(windows, &a.window, a.pid)?;
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
        let (w, h, png) = crop_png(&png, rect, factor)?;
        let image = B64.encode(&png);
        let out = json!({"ok": true, "width": w, "height": h, "png_b64": image});
        Ok(Output {
            text: out.to_string(),
            image_b64: Some(image),
            ..Output::default()
        })
    }
}

/// What a press comes to after the backend's part of it.
#[derive(Debug, PartialEq)]
pub enum Press {
    Selected,
    Click(i32, i32),
    Acted(bool),
}

/// Presses `id` the way a user would: selects a tab or list row, opens the menus above a hidden menu
/// entry, clicks the element's centre when a hit test there finds it, else runs its action; a Windows
/// menu entry runs its action, since a click right after its popup opens only hovers it.
pub fn plan_press<B: Access>(b: &mut B, id: &str, os: Os) -> Result<Press, String> {
    let shown = Extras {
        bounds: true,
        ..Extras::default()
    };
    let mut node = b.node(id, shown, 0)?;
    if SELECTED_ROLES.contains(&node.role) && b.act(id, Act::Select).unwrap_or(false) {
        return Ok(Press::Selected);
    }
    if os == Os::Windows && node.role.starts_with("menuitem") {
        return Ok(Press::Acted(b.act(id, Act::Action)?));
    }
    let chain = b.ancestors(id)?;
    if node.role.starts_with("menu") && !node.has("showing") && open_menus(b, &chain) {
        for _ in 0..MENU_POLLS {
            node = b.node(id, shown, 0)?;
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
    let shown = Extras {
        bounds: true,
        ..Extras::default()
    };
    let inside = chain.iter().all(|a| {
        b.node(a, shown, 0).is_ok_and(|n| {
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
pub fn outline(nodes: &[Node], max_chars: usize) -> String {
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
pub fn crop_png(
    png: &[u8],
    [x, y, w, h]: [f64; 4],
    factor: f64,
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
    let mut rgb = Vec::with_capacity((cw * ch * 3) as usize);
    for row in top..bottom {
        for col in left..right {
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

fn titled(frames: Vec<Frame>, window: &str, pid: Option<u32>) -> Result<Frame, String> {
    let frames: Vec<Frame> = frames
        .into_iter()
        .filter(|f| pid.is_none_or(|p| f.pid == Some(p)))
        .collect();
    match (0..TITLE_TIERS).find_map(|tier| {
        frames
            .iter()
            .rfind(|f| title_matches(tier, &f.title, window))
    }) {
        Some(f) => Ok(f.clone()),
        None => app_window(&frames, window),
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
            }])
        }

        fn tree(&mut self, frame: &Frame, _: Extras) -> Result<Tree, String> {
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

        fn act(&mut self, id: &str, act: Act) -> Result<bool, String> {
            self.acts.push((id.into(), act));
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
        assert_eq!(
            plan_press(&mut f, "ok", Os::Mac).unwrap(),
            Press::Click(140, 110)
        );
        assert!(f.acts.is_empty());
    }

    #[test]
    fn a_covered_control_runs_its_action() {
        let mut f = notepad();
        f.hits.insert((340, 110), "elsewhere".into());
        f.add("elsewhere", None, "button", &[], None);
        assert_eq!(
            plan_press(&mut f, "hidden", Os::Mac).unwrap(),
            Press::Acted(true)
        );
        assert_eq!(f.acts, [("hidden".to_owned(), Act::Action)]);
    }

    #[test]
    fn a_tab_is_selected() {
        let mut f = notepad();
        assert_eq!(plan_press(&mut f, "tab", Os::Mac).unwrap(), Press::Selected);
    }

    #[test]
    fn a_hidden_menu_entry_opens_its_menu_first() {
        let mut f = notepad();
        f.nodes.get_mut("save").unwrap().0.bounds = Some([0, 20, 100, 20]);
        assert_eq!(
            plan_press(&mut f, "save", Os::Mac).unwrap(),
            Press::Click(50, 30)
        );
        assert_eq!(f.acts, [("file".to_owned(), Act::Action)]);
    }

    #[test]
    fn a_windows_menu_entry_runs_its_action() {
        let mut f = notepad();
        assert_eq!(
            plan_press(&mut f, "save", Os::Windows).unwrap(),
            Press::Acted(true)
        );
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
    fn a_stale_title_finds_the_window_of_its_app_unless_one_matches() {
        let frame = |title: &str, active: bool| Frame {
            id: title.into(),
            title: title.into(),
            role: "window",
            pid: Some(7),
            active,
        };
        let frames = vec![
            frame("a.txt - Notepad", false),
            frame("*b.txt - Notepad", true),
            frame("Save As", false),
        ];
        let picked = |want: &str| window::pick(frames.clone(), Some(want), None).map(|f| f.title);
        assert_eq!(picked("Untitled - Notepad").unwrap(), "*b.txt - Notepad");
        assert_eq!(picked("a.txt - Notepad").unwrap(), "a.txt - Notepad");
        assert!(picked("Untitled - Paint").is_err());
        let found = |want: &str| titled(frames.clone(), want, None).map(|f| f.title);
        assert_eq!(found("Untitled - Notepad").unwrap(), "*b.txt - Notepad");
        assert_eq!(found("a.txt - Notepad").unwrap(), "a.txt - Notepad");
        assert!(found("Untitled - Paint").is_err());
    }

    #[test]
    fn a_stale_title_takes_its_app_window_only_when_that_is_unambiguous() {
        let frame = |title: &str, active: bool| Frame {
            id: title.into(),
            title: title.into(),
            role: "window",
            pid: Some(7),
            active,
        };
        let one = vec![frame("a.txt - Notepad", false), frame("Save As", true)];
        assert_eq!(
            titled(one, "Untitled - Notepad", None).unwrap().title,
            "a.txt - Notepad"
        );
        let active = vec![
            frame("a.txt - Notepad", false),
            frame("b.txt - Notepad", true),
        ];
        assert_eq!(
            titled(active, "Untitled - Notepad", None).unwrap().title,
            "b.txt - Notepad"
        );
        let none_active = vec![
            frame("a.txt - Notepad", false),
            frame("b.txt - Notepad", false),
            frame("Save As", true),
        ];
        assert_eq!(
            window::pick(none_active, Some("Untitled - Notepad"), None).unwrap_err(),
            "no window \"Untitled - Notepad\"; windows of its app: [\"a.txt - Notepad\", \"b.txt - Notepad\"]"
        );
    }

    #[test]
    fn names_match_exactly_before_they_match_contained() {
        let n = |name: &str, role: &'static str| Node {
            name: name.into(),
            role,
            ..Node::default()
        };
        let nodes = vec![
            n("Save As…", "menuitem"),
            n("Save", "menuitem"),
            n("save", "button"),
        ];
        assert_eq!(matches(&nodes, "save", None), [1, 2]);
        assert_eq!(matches(&nodes, "save as", None), [0]);
        assert_eq!(matches(&nodes, "save", Some("button")), [2]);
        assert!(matches(&nodes, "", None).is_empty());
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
        let (w, h, _) = crop_png(&png, [2.0, 0.0, 4.0, 4.0], 0.5).unwrap();
        assert_eq!((w, h), (2, 2));
        let (w, h, _) = crop_png(&png, [0.0, 0.0, f64::MAX / 4.0, f64::MAX / 4.0], 1.0).unwrap();
        assert_eq!((w, h), (4, 2));
        assert!(crop_png(&png, [100.0, 0.0, 4.0, 4.0], 1.0).is_err());
    }

    struct Recorder(Arc<Mutex<Vec<(String, Value)>>>);

    impl Input for Recorder {
        async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push((tool.to_owned(), args));
            Ok(json!({}))
        }
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
