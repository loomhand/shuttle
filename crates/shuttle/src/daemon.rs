//! The daemon's handler: model calls with the configured providers, scripts and plans on the runtime, desk calls by this OS's desk.

use std::hash::{BuildHasher, RandomState};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use desk_core::args::parse;
use desk_core::wire::{Call, Output};
use serde::Serialize;
use serde_json::Value;
use shuttle_desk::desk::{ACTIONS, TURN_WAIT};
use shuttle_desk::server::Handler;
use shuttle_models::Models;

use crate::STOPPED;
use crate::runner::Runner;

const BUSY: &str = "desk busy: another run or action is still going";
const SCRIPT_RUN_OFF: &str =
    "script_run is turned off: set script_run = true under [tools] in config.toml";

/// Answers `model_*` calls with `models`, `gui_plan` and `script_run` with `runner`, and the rest with `desk`; `safety_stop` halts every call until `safety_resume`.
pub struct Daemon<H> {
    desk: H,
    models: Result<Models, String>,
    runner: Runner,
    script_run: bool,
    halted: tokio::sync::watch::Sender<bool>,
    turn: tokio::sync::Mutex<()>,
    running: Mutex<Option<String>>,
    nonce: u64,
    runs: AtomicU64,
    wait: Duration,
}

impl<H: Handler> Daemon<H> {
    /// A daemon whose every call fails with the configuration's error when it does not load, since the safety controls are in it.
    pub fn new(desk: H, models: Result<Models, String>, runner: Runner, script_run: bool) -> Self {
        Self {
            desk,
            models,
            runner,
            script_run,
            halted: tokio::sync::watch::Sender::new(false),
            turn: tokio::sync::Mutex::new(()),
            running: Mutex::new(None),
            nonce: RandomState::new().hash_one(std::process::id()),
            runs: AtomicU64::new(1),
            wait: TURN_WAIT,
        }
    }

    async fn run(&self, tool: &str, args: Value) -> Result<String, String> {
        if tool == "script_run" && !self.script_run {
            return Err(SCRIPT_RUN_OFF.to_owned());
        }
        let _turn = tokio::time::timeout(self.wait, self.turn.lock())
            .await
            .map_err(|_| BUSY.to_owned())?;
        let n = self.runs.fetch_add(1, Ordering::Relaxed);
        let agent = format!("run-{:x}-{n}", self.nonce);
        *self.running() = Some(agent.clone());
        let report = if tool == "gui_plan" {
            self.runner.plan(args, &agent).await
        } else {
            self.runner.script(args, &agent).await
        };
        *self.running() = None;
        encode(&report?)
    }

    async fn act(&self, call: Call) -> Output {
        if self.running().as_deref() == Some(call.agent_id.as_str()) {
            return self.desk.answer(call).await;
        }
        match tokio::time::timeout(self.wait, self.turn.lock()).await {
            Ok(_turn) => self.desk.answer(call).await,
            Err(_) => Output::error(BUSY),
        }
    }

    fn halt(&self, stopped: bool) -> Output {
        self.halted.send_replace(stopped);
        self.runner.halt(stopped);
        Output::text(serde_json::json!({"ok": true, "stopped": stopped}).to_string())
    }

    fn running(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn model(&self, tool: &str, args: Value) -> Result<String, String> {
        let models = self.models.as_ref().map_err(String::clone)?;
        match tool {
            "model_decide" => encode(&models.decide(parse(args)?).await?),
            "model_ask" => encode(&models.ask(parse(args)?).await?),
            "model_ground" => encode(&models.ground(parse(args)?).await?),
            other => Err(format!("unknown tool {other}")),
        }
    }
}

impl<H: Handler> Handler for Daemon<H> {
    async fn answer(&self, call: Call) -> Output {
        match call.tool.as_str() {
            "safety_stop" => return self.halt(true),
            "safety_resume" => return self.halt(false),
            _ => {}
        }
        if *self.halted.borrow() {
            return Output::error(STOPPED);
        }
        if let Err(e) = &self.models {
            return Output::error(e);
        }
        if let t @ ("gui_plan" | "script_run") = call.tool.as_str() {
            return self
                .run(t, call.args)
                .await
                .map_or_else(Output::error, Output::text);
        }
        let mut halted = self.halted.subscribe();
        let out = async {
            match call.tool.as_str() {
                t if t.starts_with("model_") => self
                    .model(t, call.args)
                    .await
                    .map_or_else(Output::error, Output::text),
                t if ACTIONS.contains(&t) => self.act(call).await,
                _ => self.desk.answer(call).await,
            }
        };
        tokio::select! {
            out = out => out,
            _ = halted.wait_for(|h| *h) => Output::error(STOPPED),
        }
    }
}

fn encode<T: Serialize>(v: &T) -> Result<String, String> {
    serde_json::to_string(v).map_err(|e| format!("encode reply: {e}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use shuttle_models::config::Config;

    use super::*;

    struct Echo;

    impl Handler for Echo {
        async fn answer(&self, call: Call) -> Output {
            Output::text(format!("desk answered {}", call.tool))
        }
    }

    struct Slow;

    impl Handler for Slow {
        async fn answer(&self, _: Call) -> Output {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Output::text("too late")
        }
    }

    #[tokio::test]
    async fn stop_ends_a_desk_call_in_flight() {
        let d = std::sync::Arc::new(Daemon::new(
            Slow,
            Ok(Models::new(Config::default())),
            runner(),
            true,
        ));
        let call = |tool: &str| Call {
            agent_id: String::new(),
            tool: tool.into(),
            args: json!({}),
        };
        let click = tokio::spawn({
            let d = d.clone();
            let call = call("desk_click");
            async move { d.answer(call).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = std::time::Instant::now();
        d.answer(call("safety_stop")).await;
        let out = click.await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!((out.is_error, out.text.as_str()), (true, crate::STOPPED));
    }

    fn runner() -> Runner {
        let runtime = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/lib");
        Runner::new(
            "node".into(),
            runtime,
            "unused".into(),
            std::env::temp_dir().join("shuttle-test-flows"),
            Vec::new(),
        )
    }

    #[tokio::test]
    async fn model_calls_go_to_the_providers_and_the_rest_to_the_desk() {
        let closed = Config {
            base_url: "http://127.0.0.1:9/v1".to_owned(),
            ..Config::default()
        };
        let d = Daemon::new(Echo, Ok(Models::new(closed)), runner(), true);
        let ask = |tool: &str, args: Value| Call {
            agent_id: "desk".into(),
            tool: tool.into(),
            args,
        };
        let desk = d.answer(ask("desk_windows", json!({}))).await;
        assert_eq!(
            (desk.is_error, desk.text.as_str()),
            (false, "desk answered desk_windows")
        );
        let model = d.answer(ask("model_ask", json!({"prompt": "x"}))).await;
        assert!(
            model.is_error && model.text.starts_with("anthropic/claude-sonnet-5: "),
            "{}",
            model.text
        );
        let bad = d.answer(ask("model_ground", json!({"target": "x"}))).await;
        assert!(
            bad.is_error && bad.text.starts_with("invalid arguments"),
            "{}",
            bad.text
        );
        let unknown = d.answer(ask("model_plan", json!({}))).await;
        assert_eq!(unknown.text, "unknown tool model_plan");
    }

    #[test]
    fn each_daemon_names_its_runs_apart_from_any_earlier_daemon() {
        let a = Daemon::new(Echo, Err(String::new()), runner(), true);
        let b = Daemon::new(Echo, Err(String::new()), runner(), true);
        assert_ne!(a.nonce, b.nonce);
    }

    #[tokio::test]
    async fn a_broken_config_fails_every_call_but_stop_since_the_safety_controls_are_in_it() {
        let d = Daemon::new(
            Echo,
            Err("config.toml line 3: invalid type".to_owned()),
            runner(),
            true,
        );
        let call = |tool: &str| Call {
            agent_id: String::new(),
            tool: tool.into(),
            args: json!({"prompt": "x"}),
        };
        for tool in ["model_ask", "script_run", "gui_plan", "desk_screen"] {
            let out = d.answer(call(tool)).await;
            assert_eq!(
                (out.is_error, out.text.as_str()),
                (true, "config.toml line 3: invalid type"),
                "{tool}"
            );
        }
        assert!(!d.answer(call("safety_stop")).await.is_error);
    }

    #[tokio::test]
    async fn stop_fails_every_call_and_ends_the_run_in_flight_until_resume() {
        let d = std::sync::Arc::new(Daemon::new(
            Echo,
            Ok(Models::new(Config::default())),
            runner(),
            true,
        ));
        let call = |tool: &str, args: Value| Call {
            agent_id: String::new(),
            tool: tool.into(),
            args,
        };
        let slow = json!({"code": "export async function main() { await new Promise((r) => setTimeout(r, 20000)) }"});
        let run = tokio::spawn({
            let d = d.clone();
            let call = call("script_run", slow.clone());
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
        for tool in ["desk_screen", "desk_click", "model_ask", "script_run"] {
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
    async fn script_run_turned_off_answers_how_to_turn_it_on() {
        let d = Daemon::new(Echo, Ok(Models::new(Config::default())), runner(), false);
        let out = d
            .answer(Call {
                agent_id: String::new(),
                tool: "script_run".into(),
                args: json!({"code": "export function main() { return 1 }"}),
            })
            .await;
        assert_eq!((out.is_error, out.text.as_str()), (true, SCRIPT_RUN_OFF));
    }

    #[tokio::test]
    async fn script_run_goes_to_the_runner() {
        let d = Daemon::new(Echo, Ok(Models::new(Config::default())), runner(), true);
        let out = d
            .answer(Call {
                agent_id: String::new(),
                tool: "script_run".into(),
                args: json!({"code": "export function main() { return 42 }"}),
            })
            .await;
        let report: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(report["result"], 42, "{report}");
    }

    #[cfg(unix)]
    #[derive(Clone, Default)]
    struct Keys(std::sync::Arc<Mutex<Vec<String>>>);

    #[cfg(unix)]
    impl Handler for Keys {
        async fn answer(&self, call: Call) -> Output {
            if let Some(k) = call.args["keys"].as_str() {
                self.0.lock().unwrap().push(k.to_owned());
            }
            Output::text("{}")
        }
    }

    #[cfg(unix)]
    async fn served(wait: Duration) -> (std::sync::Arc<Daemon<Keys>>, Keys, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("d.sock").to_string_lossy().into_owned();
        let runtime = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/lib");
        let keys = Keys::default();
        let runner = Runner::new(
            "node".into(),
            runtime,
            endpoint.clone(),
            dir.path().join("flows"),
            Vec::new(),
        );
        let mut d = Daemon::new(
            keys.clone(),
            Ok(Models::new(Config::default())),
            runner,
            true,
        );
        d.wait = wait;
        let d = std::sync::Arc::new(d);
        tokio::spawn({
            let (d, endpoint) = (std::sync::Arc::clone(&d), endpoint.clone());
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
}
