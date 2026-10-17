//! The daemon's handler: model calls answered with the configured providers, every other call by this OS's desk.

use desk_core::args::parse;
use desk_core::wire::{Call, Output};
use serde::Serialize;
use serde_json::Value;
use shuttle_desk::server::Handler;
use shuttle_models::Models;

/// Answers `model_*` calls with `models` and the rest with `desk`.
pub struct Daemon<H> {
    desk: H,
    models: Result<Models, String>,
}

impl<H> Daemon<H> {
    /// A daemon whose model calls fail with the configuration's error while desk calls still work.
    pub fn new(desk: H, models: Result<Models, String>) -> Self {
        Self { desk, models }
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
        if !call.tool.starts_with("model_") {
            return self.desk.answer(call).await;
        }
        self.model(&call.tool, call.args)
            .await
            .map_or_else(Output::error, Output::text)
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

    #[tokio::test]
    async fn model_calls_go_to_the_providers_and_the_rest_to_the_desk() {
        let closed = Config {
            base_url: "http://127.0.0.1:9/v1".to_owned(),
            ..Config::default()
        };
        let d = Daemon::new(Echo, Ok(Models::new(closed)));
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
        let d = Daemon::new(Echo, Err("config.toml line 3: invalid type".to_owned()));
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
}
