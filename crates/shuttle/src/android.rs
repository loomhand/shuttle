//! The daemon's desk on an Android device: the adb backend in place of this desktop's accessibility tree.

use std::path::{Path, PathBuf};

use desk_core::wire::{Call, Output};
use serde_json::Value;
use shuttle_android::adb::{Android, Device};
use shuttle_android::desk;
use shuttle_desk::server::Handler;
use shuttle_models::config::AndroidSettings;

const BUNDLED_JAR: &str = "android/shuttle-ui.jar";

/// The desk a daemon serves: this desktop's, or an Android device's when the config names one.
pub enum Desks<N> {
    Native(N),
    Android(Box<Android>),
}

impl<N: Handler> Handler for Desks<N> {
    async fn answer(&self, call: Call) -> Output {
        match self {
            Self::Native(native) => native.answer(call).await,
            Self::Android(android) => answer(android, call).await,
        }
    }

    fn release(&self, holder: &str) {
        if let Self::Native(native) = self {
            native.release(holder);
        }
    }

    fn allowed(&self) {
        if let Self::Native(native) = self {
            native.allowed();
        }
    }
}

pub fn device(settings: AndroidSettings, exe: &Path) -> Result<Android, String> {
    Ok(Android::new(Device {
        ui_jar: ui_jar(settings.ui_jar, exe)?,
        adb: settings.adb,
        serial: settings.serial,
        scrcpy_jar: settings.scrcpy_jar,
        su: settings.su,
    }))
}

/// The UI server jar: the configured one, else the one the archive installs beside the program `exe`.
fn ui_jar(configured: Option<PathBuf>, exe: &Path) -> Result<PathBuf, String> {
    if let Some(jar) = configured {
        return Ok(jar);
    }
    let bundled = exe.parent().unwrap_or(Path::new(".")).join(BUNDLED_JAR);
    if bundled.is_file() {
        Ok(bundled)
    } else {
        Err(format!(
            "[android] sets no ui_jar, and there is no {} beside the program",
            bundled.display()
        ))
    }
}

async fn answer(android: &Android, call: Call) -> Output {
    let Call {
        agent_id,
        tool,
        args,
    } = call;
    let answered = async {
        match tool.as_str() {
            "desk_computer_state" => android.state(with_session(&tool, agent_id, args)?).await,
            "desk_computer_act" => android.act(with_session(&tool, agent_id, args)?).await,
            "desk_computer_end" => android
                .end(&with_session(&tool, agent_id, args)?)
                .await
                .map(Output::text),
            "desk_look" => android.look().await,
            verb => desk::run(android, verb, args).await,
        }
    };
    answered.await.unwrap_or_else(Output::error)
}

fn with_session(tool: &str, agent: String, mut args: Value) -> Result<Value, String> {
    if !args.is_object() && !args.is_null() {
        return Err(format!("{tool}: args are not an object"));
    }
    match &args["session"] {
        Value::String(_) => {}
        Value::Number(n) => args["session"] = n.to_string().into(),
        Value::Null => args["session"] = agent.into(),
        _ => return Err(format!("{tool}: session takes a string; nothing was sent")),
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_computer_session_end_is_answered_on_the_device() {
        let android = fake();
        let out = answer(
            &android,
            Call {
                agent_id: "agent".into(),
                tool: "desk_computer_end".into(),
                args: serde_json::json!({"session": "cu-1"}),
            },
        )
        .await;
        assert_eq!((out.is_error, out.text.as_str()), (false, "ended"));
    }

    #[test]
    fn a_computer_verb_without_a_session_takes_its_callers_own() {
        let tool = "desk_computer_act";
        let a = with_session(tool, "agent-a".into(), serde_json::json!({"action": "tap"})).unwrap();
        let b = with_session(tool, "agent-b".into(), Value::Null).unwrap();
        assert_ne!(a["session"], b["session"]);
        assert_eq!(a["session"], "agent-a");
        let named = with_session(
            tool,
            "agent-a".into(),
            serde_json::json!({"session": "cu-1"}),
        );
        assert_eq!(named.unwrap()["session"], "cu-1");
    }

    #[tokio::test]
    async fn a_numbered_session_is_that_session_and_a_session_of_another_type_is_refused() {
        let tool = "desk_computer_end";
        let numbered = with_session(tool, "agent-a".into(), serde_json::json!({"session": 2}));
        assert_eq!(numbered.unwrap()["session"], "2");
        let android = fake();
        for session in [
            serde_json::json!({}),
            serde_json::json!([2]),
            serde_json::json!(true),
        ] {
            let out = answer(
                &android,
                Call {
                    agent_id: "agent".into(),
                    tool: tool.into(),
                    args: serde_json::json!({"session": session}),
                },
            )
            .await;
            assert_eq!(
                (out.is_error, out.text.as_str()),
                (
                    true,
                    "desk_computer_end: session takes a string; nothing was sent"
                )
            );
        }
    }

    #[tokio::test]
    async fn computer_verbs_on_a_device_refuse_args_that_are_not_an_object() {
        let android = fake();
        for tool in [
            "desk_computer_end",
            "desk_computer_state",
            "desk_computer_act",
        ] {
            for args in [serde_json::json!(["tap"]), serde_json::json!("tap")] {
                let out = answer(
                    &android,
                    Call {
                        agent_id: "agent".into(),
                        tool: tool.into(),
                        args,
                    },
                )
                .await;
                assert_eq!(
                    (out.is_error, out.text),
                    (true, format!("{tool}: args are not an object"))
                );
            }
        }
    }

    #[test]
    fn an_unset_ui_jar_is_the_bundled_one_and_a_set_one_wins() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("shuttle");
        let err = ui_jar(None, &exe).unwrap_err();
        assert!(err.contains("sets no ui_jar"), "{err}");
        let bundled = dir.path().join("android/shuttle-ui.jar");
        std::fs::create_dir_all(bundled.parent().unwrap()).unwrap();
        std::fs::write(&bundled, b"dex").unwrap();
        assert_eq!(ui_jar(None, &exe).unwrap(), bundled);
        assert_eq!(
            ui_jar(Some("/u.jar".into()), &exe).unwrap(),
            PathBuf::from("/u.jar")
        );
    }

    fn fake() -> Android {
        device(
            AndroidSettings {
                adb: "/nonexistent/adb".into(),
                serial: None,
                ui_jar: Some("/u.jar".into()),
                scrcpy_jar: "/s.jar".into(),
                su: None,
            },
            Path::new("/shuttle"),
        )
        .unwrap()
    }
}
