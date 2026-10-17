//! The daemon's handler: model calls with the configured providers, scripts and plans on the runtime, desk calls by this OS's desk.

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

use crate::runner::Runner;

const BUSY: &str = "desk busy: another run or action is still going";

/// Answers `model_*` calls with `models`, `gui_plan` and `script_run` with `runner`, and the rest with `desk`.
pub struct Daemon<H> {
    desk: H,
    models: Result<Models, String>,
    runner: Runner,
    turn: tokio::sync::Mutex<()>,
    running: Mutex<Option<String>>,
    runs: AtomicU64,
    wait: Duration,
}

impl<H: Handler> Daemon<H> {
    /// A daemon whose model calls fail with the configuration's error while desk calls still work.
    pub fn new(desk: H, models: Result<Models, String>, runner: Runner) -> Self {
        Self {
            desk,
            models,
            runner,
            turn: tokio::sync::Mutex::new(()),
            running: Mutex::new(None),
            runs: AtomicU64::new(1),
            wait: TURN_WAIT,
        }
    }

    async fn run(&self, tool: &str, args: Value) -> Result<String, String> {
        let _turn = tokio::time::timeout(self.wait, self.turn.lock())
            .await
            .map_err(|_| BUSY.to_owned())?;
        let agent = format!("run-{}", self.runs.fetch_add(1, Ordering::Relaxed));
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
        let out = match call.tool.as_str() {
            t @ ("gui_plan" | "script_run") => self.run(t, call.args).await,
            t if t.starts_with("model_") => self.model(t, call.args).await,
            t if ACTIONS.contains(&t) => return self.act(call).await,
            _ => return self.desk.answer(call).await,
        };
        out.map_or_else(Output::error, Output::text)
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

    fn runner() -> Runner {
        let runtime = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/lib");
        Runner::new("node".into(), runtime, "unused".into(), Vec::new())
    }

    #[tokio::test]
    async fn model_calls_go_to_the_providers_and_the_rest_to_the_desk() {
        let closed = Config {
            base_url: "http://127.0.0.1:9/v1".to_owned(),
            ..Config::default()
        };
        let d = Daemon::new(Echo, Ok(Models::new(closed)), runner());
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

    #[tokio::test]
    async fn a_broken_config_fails_model_calls_and_leaves_the_desk_working() {
        let d = Daemon::new(
            Echo,
            Err("config.toml line 3: invalid type".to_owned()),
            runner(),
        );
        let call = |tool: &str| Call {
            agent_id: String::new(),
            tool: tool.into(),
            args: json!({"prompt": "x"}),
        };
        let model = d.answer(call("model_ask")).await;
        assert_eq!(
            (model.is_error, model.text.as_str()),
            (true, "config.toml line 3: invalid type")
        );
        assert!(!d.answer(call("desk_screen")).await.is_error);
    }

    #[tokio::test]
    async fn script_run_goes_to_the_runner() {
        let d = Daemon::new(Echo, Ok(Models::new(Config::default())), runner());
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
        let runner = Runner::new("node".into(), runtime, endpoint.clone(), Vec::new());
        let mut d = Daemon::new(keys.clone(), Err("no models".to_owned()), runner);
        d.wait = wait;
        let d = std::sync::Arc::new(d);
        tokio::spawn({
            let (d, endpoint) = (std::sync::Arc::clone(&d), endpoint.clone());
            async move { shuttle_desk::server::serve(d, &endpoint).await }
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
