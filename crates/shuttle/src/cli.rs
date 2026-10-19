//! The command line: `mcp`, `serve`, `stop`, `resume`, `desk VERB` and `--version`.

use std::fmt::Display;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use desk_core::text::cli_args;
use desk_core::wire::Call;
use shuttle_desk::driver::Driver;
use shuttle_desk::server;
use shuttle_models::Models;
#[cfg(any(target_os = "macos", windows))]
use shuttle_models::config::Safety;
use shuttle_models::config::{Config, DEFAULT_KEY_ENV, default_path};
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
use crate::{NAME, VERSION, log, mcp, plugins};

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
            let path = default_path(NAME);
            let config = Config::load(&path);
            let mut listed = plugins::configured(&config, &path, &tools::NAMES);
            if !config.as_ref().is_ok_and(|c| c.tools.script_run) {
                listed.runs.clear();
            }
            let stdin = tokio::io::BufReader::new(tokio::io::stdin());
            exit(block_on(
                Builder::new_current_thread(),
                mcp::serve(stdin, tokio::io::stdout(), &endpoint, &listed),
            ))
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
    server::cache_endpoint("SHUTTLE_SOCKET", &format!("{NAME}-desk"), NAME, "desk.sock")
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

fn send(call: &Call, what: impl Display) -> ExitCode {
    match block_on(
        Builder::new_current_thread(),
        server::call(&desk_endpoint(), call),
    ) {
        Ok(out) if !out.is_error => {
            let _ = writeln!(std::io::stdout(), "{}", out.text);
            if let Some(png) = &out.image_b64 {
                match save_png(png) {
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

/// Writes a call's screenshot to a new file in the temporary directory; its path.
fn save_png(b64: &str) -> Result<PathBuf, String> {
    let png = B64
        .decode(b64)
        .map_err(|e| format!("decode screenshot: {e}"))?;
    let mut file = tempfile::Builder::new()
        .prefix("cocoon-shuttle-")
        .suffix(".png")
        .tempfile()
        .map_err(|e| format!("create screenshot file: {e}"))?;
    file.write_all(&png)
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
        let ready = move |controls, safety: Safety, hotkey: Option<Hotkey>| {
            let _ = tx.send(Ui {
                controls,
                hotkey,
                stop_label: match hotkey {
                    Some(_) => format!("Stop ({})", safety.kill_hotkey),
                    None => "Stop".to_owned(),
                },
                indicator: safety.indicator,
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
    ),
) -> Result<(), String> {
    let path = default_path(NAME);
    let mut config = Config::load(&path);
    let android = config.as_mut().ok().and_then(|c| c.android.take());
    let local = android::local_controls(android.as_ref());
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
    let guard = Guard {
        script_run: config.as_ref().is_ok_and(|c| c.tools.script_run),
        confirm: safety.confirm,
        asker,
    };
    let overlay = !config.as_ref().is_ok_and(|c| !c.driver.overlay);
    let plugins = plugins::configured(&config, &path, &tools::NAMES);
    let routes = services::routes(&plugins.services, endpoint);
    let models = config.map(Models::new);
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("locate this program: {e}"))?;
    let listener = server::bind(endpoint).await?;
    let services = Services::start(&plugins.services, endpoint, &key_vars)?;
    let own = driver::endpoint(endpoint);
    let bundled = if local {
        Bundled::start(&exe, &own, overlay).await?
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
    );
    let desk = match android {
        Some(settings) => Desks::Android(Box::new(android::device(settings))),
        None => Desks::Native(server::native(driver, &apps)),
    };
    let daemon = Arc::new(
        Daemon::new(desk, models, runner, guard)
            .with_plugins(plugins.runs)
            .with_services(routes),
    );
    #[cfg(any(target_os = "macos", windows))]
    ready(daemon.clone(), safety, hotkey.filter(|_| local));
    let served = tokio::select! {
        r = server::serve(daemon, listener) => r,
        () = shutdown() => Ok(()),
    };
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
        "usage: {NAME} mcp | {NAME} serve [ENDPOINT] | {NAME} stop | {NAME} resume | {NAME} desk VERB [key=value ...|JSON] | {NAME} --version; desk verbs: {}",
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
    fn a_screenshot_lands_in_a_png_file() {
        let path = save_png(&B64.encode(b"\x89PNG")).unwrap();
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));
        assert_eq!(std::fs::read(&path).unwrap(), b"\x89PNG");
        std::fs::remove_file(path).unwrap();
        assert!(save_png("not base64!").is_err());
    }
}
