//! The command line: `mcp [--http ADDR]`, `serve`, `stop`, `resume`, `desk VERB` and `--version`.

use std::fmt::Display;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use desk_core::wire::Call;
use serde_json::{Map, Value};
use shuttle_desk::driver::Driver;
use shuttle_desk::server;
use shuttle_models::Models;
#[cfg(any(target_os = "macos", windows))]
use shuttle_models::config::Safety;
use shuttle_models::config::{Config, DEFAULT_KEY_ENV, InputMode, default_path};
use tokio::runtime::Builder;

use crate::android::{self, Desks};
use crate::daemon::{Asker, Daemon, Guard};
#[cfg(any(target_os = "macos", windows))]
use crate::daemon::{Controls, Ui};
use crate::driver::{self, Bundled};
#[cfg(any(target_os = "macos", windows))]
use crate::hotkey::{self, Hotkey};
#[cfg(target_os = "macos")]
use crate::mac as controls;
use crate::runner::Runner;
use crate::services::{self, Services};
use crate::tools::{self, DESK_VERBS};
#[cfg(windows)]
use crate::win as controls;
use crate::{NAME, VENDOR, VERSION, home, http, log, mcp, plugins};

/// Runs the command in `args` (the program name excluded) and returns its exit code.
pub fn run(args: &[String]) -> ExitCode {
    let Some((cmd, rest)) = args.split_first() else {
        return usage();
    };
    match cmd.as_str() {
        "--version" | "-V" => {
            let _ = writeln!(std::io::stdout(), "{NAME} {VERSION}");
            ExitCode::SUCCESS
        }
        "mcp" => {
            let endpoint = desk_endpoint();
            let path = default_path(home());
            let config = Config::load(&path);
            let mut listed = plugins::configured(&config, &path, &tools::NAMES);
            if !config.as_ref().is_ok_and(|c| c.tools.script_run) {
                listed.runs.clear();
            }
            match rest {
                [] => {
                    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
                    exit(block_on(
                        Builder::new_current_thread(),
                        mcp::serve(stdin, tokio::io::stdout(), &endpoint, &listed),
                    ))
                }
                [flag, addr] if flag == "--http" => {
                    let token = std::env::var(http::TOKEN_ENV).unwrap_or_default();
                    exit(block_on(
                        Builder::new_multi_thread(),
                        http::Server::new(endpoint, listed, token).serve(addr),
                    ))
                }
                _ => usage(),
            }
        }
        "serve" => {
            let endpoint = rest.first().cloned().unwrap_or_else(desk_endpoint);
            #[cfg(any(target_os = "macos", windows))]
            return controls_serve(endpoint);
            #[cfg(not(any(target_os = "macos", windows)))]
            exit(block_on(
                Builder::new_multi_thread(),
                serve(&endpoint, None),
            ))
        }
        "desk" => call_desk(rest),
        "stop" | "resume" => safety(cmd),
        _ => usage(),
    }
}

fn desk_endpoint() -> String {
    server::cache_endpoint(
        "SHUTTLE_SOCKET",
        &format!("{VENDOR}-{NAME}-desk"),
        &home().to_string_lossy(),
        "desk.sock",
    )
}

fn safety(cmd: &str) -> ExitCode {
    let call = Call {
        agent_id: "user".to_owned(),
        tool: format!("safety_{cmd}"),
        args: serde_json::json!({}),
    };
    send(&call, cmd)
}

fn call_desk(rest: &[String]) -> ExitCode {
    let Some((verb, rest)) = rest.split_first() else {
        return usage();
    };
    let call = match cli_args(rest) {
        Ok(args) => Call {
            agent_id: "desk".to_owned(),
            tool: format!("desk_{verb}"),
            args,
        },
        Err(e) => {
            log(e);
            return usage();
        }
    };
    send(&call, format_args!("desk {verb}"))
}

/// One JSON object, or `key=value` pairs whose values parse as JSON when they can.
fn cli_args(rest: &[String]) -> Result<Value, String> {
    if let [one] = rest
        && one.trim_start().starts_with('{')
    {
        return serde_json::from_str(one).map_err(|e| format!("arguments: {e}"));
    }
    let mut args = Map::new();
    for pair in rest {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| format!("{pair:?} is not key=value"))?;
        let v = serde_json::from_str(v).unwrap_or_else(|_| Value::String(v.to_owned()));
        args.insert(k.to_owned(), v);
    }
    Ok(Value::Object(args))
}

fn send(call: &Call, what: impl Display) -> ExitCode {
    match block_on(
        Builder::new_current_thread(),
        server::call(&desk_endpoint(), call),
    ) {
        Ok(out) if !out.is_error => {
            let _ = writeln!(std::io::stdout(), "{}", out.text);
            if let Some(image) = &out.image_b64 {
                match save_image(image) {
                    Ok(path) => {
                        let _ = writeln!(std::io::stdout(), "image: {}", path.display());
                    }
                    Err(e) => return fail(format_args!("{what}: {e}")),
                }
            }
            ExitCode::SUCCESS
        }
        Ok(out) => fail(format_args!("{what}: {}", out.text)),
        Err(e) => fail(format_args!("{what}: no daemon answers: {e}")),
    }
}

fn save_image(b64: &str) -> Result<PathBuf, String> {
    let image = B64
        .decode(b64)
        .map_err(|e| format!("decode screenshot: {e}"))?;
    let suffix = match image.get(..12) {
        Some([0xff, 0xd8, ..]) => ".jpg",
        Some([b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P']) => ".webp",
        _ => ".png",
    };
    let mut file = tempfile::Builder::new()
        .prefix(&format!("{NAME}-"))
        .suffix(suffix)
        .tempfile()
        .map_err(|e| format!("create screenshot file: {e}"))?;
    file.write_all(&image)
        .map_err(|e| format!("write screenshot: {e}"))?;
    file.keep()
        .map(|(_, path)| path)
        .map_err(|e| format!("keep screenshot file: {e}"))
}

/// The daemon on a worker thread, and this OS's controls on this, the main, thread.
#[cfg(any(target_os = "macos", windows))]
fn controls_serve(endpoint: String) -> ExitCode {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ready = move |controls, safety: Safety, hotkey: Option<Hotkey>, input: InputMode| {
            let input = match input {
                InputMode::Foreground => "foreground",
                InputMode::Background => "background",
                InputMode::Auto => "auto",
            };
            let _ = tx.send(Ui {
                controls,
                hotkey,
                stop_label: match hotkey {
                    Some(_) => format!("Stop ({})", safety.kill_hotkey),
                    None => "Stop".to_owned(),
                },
                indicator: safety.indicator,
                about: format!("Shuttle {VERSION}, {input} input"),
            });
        };
        let done = block_on(
            Builder::new_multi_thread(),
            serve(&endpoint, Some(controls::asker()), ready),
        );
        if let Err(e) = &done {
            log(e);
        }
        #[cfg(windows)]
        controls::shutdown();
        std::process::exit(i32::from(done.is_err()));
    });
    match rx.recv() {
        Ok(ui) => controls::run(ui),
        Err(_) => loop {
            std::thread::park();
        },
    }
}

async fn serve(
    endpoint: &str,
    asker: Option<Asker>,
    #[cfg(any(target_os = "macos", windows))] ready: impl FnOnce(
        Arc<dyn Controls>,
        Safety,
        Option<Hotkey>,
        InputMode,
    ),
) -> Result<(), String> {
    let path = default_path(home());
    let mut config = Config::load(&path);
    let android = config.as_mut().ok().and_then(|c| c.android.take());
    let local = android.is_none();
    let mut safety = config
        .as_mut()
        .map(|c| std::mem::take(&mut c.safety))
        .unwrap_or_default();
    safety.indicator &= local;
    #[cfg(any(target_os = "macos", windows))]
    let hotkey = match hotkey::parse(&safety.kill_hotkey) {
        Ok(h) => h,
        Err(e) => {
            config = Err(format!("{}: {e}", path.display()));
            safety = Safety::default();
            hotkey::parse(&safety.kill_hotkey)?
        }
    };
    let mut key_vars = vec![DEFAULT_KEY_ENV.to_owned()];
    if let Ok(c) = &config
        && !key_vars.contains(&c.api_key_env)
    {
        key_vars.push(c.api_key_env.clone());
    }
    let apps = std::mem::take(&mut safety.apps);
    let input = config
        .as_ref()
        .map_or(InputMode::Foreground, |c| c.desk.input);
    let guard = Guard {
        script_run: config.as_ref().is_ok_and(|c| c.tools.script_run),
        confirm: safety.confirm,
        asker,
        background: input == InputMode::Background,
    };
    let auto = local && !cfg!(target_os = "linux") && input == InputMode::Auto;
    let overlay = config.as_ref().ok().is_none_or(|c| c.driver.overlay);
    let locked_input = config.as_ref().is_ok_and(|c| c.desk.locked_input);
    let group = config
        .as_mut()
        .ok()
        .and_then(|c| c.daemon.socket_group.take());
    let plugins = plugins::configured(&config, &path, &tools::NAMES);
    let routes = services::routes(&plugins.services, endpoint);
    let models = config.map(Models::new);
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("locate this program: {e}"))?;
    let listener = match &group {
        Some(group) => server::bind_for(endpoint, group).await?,
        None => server::bind(endpoint).await?,
    };
    let services = Services::start(&plugins.services, endpoint, &key_vars)?;
    let own = driver::endpoint(endpoint);
    let bundled = if local {
        Bundled::start(
            &exe,
            &own,
            overlay,
            crate::embedded(),
            input == InputMode::Foreground,
        )
        .await?
    } else {
        None
    };
    if local && bundled.is_none() && cfg!(target_os = "linux") {
        return Err(format!(
            "no cua-driver beside {}: the Linux desk runs only the bundled one",
            exe.display()
        ));
    }
    let driver = if bundled.is_some() {
        own
    } else {
        Driver::default_endpoint()
    };
    let runner = Runner::beside(
        &exe,
        endpoint.to_owned(),
        path.with_file_name("flows"),
        key_vars,
    )
    .input(if guard.background {
        "background"
    } else if auto {
        "auto"
    } else {
        "foreground"
    });
    let desk = match android {
        Some(settings) => Desks::Android(Box::new(android::device(settings, &exe)?)),
        None => Desks::Native(server::native(
            driver,
            &apps,
            overlay,
            guard.background,
            auto,
            locked_input,
        )),
    };
    let recordings = server::cache_endpoint(
        "SHUTTLE_RECORDINGS",
        "",
        &home().to_string_lossy(),
        "recordings",
    );
    let daemon = Arc::new(
        Daemon::new(desk, models, runner, guard)
            .with_plugins(plugins.runs)
            .with_services(routes)
            .with_recordings(recordings.into()),
    );
    let recorder = Arc::clone(&daemon);
    tokio::spawn(async move { recorder.recordings().await });
    #[cfg(any(target_os = "macos", windows))]
    ready(daemon.clone(), safety, hotkey.filter(|_| local), input);
    let ending = Arc::clone(&daemon);
    let mut serving = Box::pin(server::serve(daemon, listener));
    let served = tokio::select! {
        r = &mut serving => r,
        () = shutdown() => Ok(()),
    };
    ending.end_recording().await;
    #[cfg(unix)]
    crate::kill_graced();
    services.stop().await;
    if let Some(b) = bundled {
        b.stop().await;
    }
    served
}

#[cfg(unix)]
async fn shutdown() {
    use tokio::signal::unix::{SignalKind, signal};

    let (Ok(mut term), Ok(mut int), Ok(mut hup)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
        signal(SignalKind::hangup()),
    ) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
        _ = hup.recv() => {}
    }
}

#[cfg(windows)]
async fn shutdown() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

fn block_on<T>(
    mut runtime: Builder,
    f: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    runtime
        .enable_all()
        .build()
        .map_err(|e| format!("start runtime: {e}"))?
        .block_on(f)
}

fn exit(done: Result<(), String>) -> ExitCode {
    done.map_or_else(fail, |()| ExitCode::SUCCESS)
}

fn usage() -> ExitCode {
    let _ = writeln!(
        std::io::stderr(),
        "usage: {NAME} mcp [--http ADDR, with {} set] | {NAME} serve [ENDPOINT] | {NAME} stop | {NAME} resume | {NAME} desk VERB [key=value ...|JSON] | {NAME} --version; desk verbs: {}",
        http::TOKEN_ENV,
        DESK_VERBS.join(", ")
    );
    ExitCode::from(2)
}

fn fail(e: impl Display) -> ExitCode {
    log(e);
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_arguments_are_json_or_pairs_with_json_values() {
        let pairs =
            cli_args(&["window=Save As".into(), "text=true".into(), "pid=7".into()]).unwrap();
        assert_eq!(
            pairs,
            serde_json::json!({"window": "Save As", "text": true, "pid": 7})
        );
        assert_eq!(
            cli_args(&[r#"{"id": "1/2"}"#.into()]).unwrap(),
            serde_json::json!({"id": "1/2"})
        );
        assert!(cli_args(&["nope".into()]).is_err());
    }

    #[test]
    fn a_screenshot_lands_in_a_file_named_by_its_format() {
        for (image, ext) in [
            (&b"\x89PNG\r\n\x1a\n\0\0\0\r"[..], "png"),
            (b"\xff\xd8\xff\xe0\0\x10JFIF\0\x01\x01", "jpg"),
            (b"RIFF\x24\0\0\0WEBPVP8L", "webp"),
        ] {
            let path = save_image(&B64.encode(image)).unwrap();
            assert_eq!(path.extension().and_then(|e| e.to_str()), Some(ext));
            assert_eq!(std::fs::read(&path).unwrap(), image);
            std::fs::remove_file(path).unwrap();
        }
        assert!(save_image("not base64!").is_err());
    }
}
