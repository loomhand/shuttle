//! The command line: `mcp`, `serve`, `desk VERB` and `--version`.

use std::fmt::Display;
use std::io::Write as _;
use std::process::ExitCode;
use std::sync::Arc;

use desk_core::text::cli_args;
use desk_core::wire::Call;
use shuttle_desk::driver::Driver;
use shuttle_desk::server;
use shuttle_models::Models;
use shuttle_models::config::{Config, Safety, default_path};
use tokio::runtime::Builder;

#[cfg(any(target_os = "macos", windows))]
use crate::daemon::Ui;
use crate::daemon::{Asker, Controls, Daemon, Guard};
use crate::driver::{self, Bundled};
#[cfg(target_os = "macos")]
use crate::mac as controls;
use crate::runner::Runner;
use crate::tools::DESK_VERBS;
#[cfg(windows)]
use crate::win as controls;
use crate::{NAME, VERSION, log, mcp};

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
            let stdin = tokio::io::BufReader::new(tokio::io::stdin());
            exit(block_on(
                Builder::new_current_thread(),
                mcp::serve(stdin, tokio::io::stdout(), &endpoint),
            ))
        }
        "serve" => {
            let endpoint = rest.first().cloned().unwrap_or_else(desk_endpoint);
            #[cfg(any(target_os = "macos", windows))]
            return controls_serve(endpoint);
            #[cfg(not(any(target_os = "macos", windows)))]
            exit(block_on(
                Builder::new_multi_thread(),
                serve(&endpoint, None, |_, _| Ok(())),
            ))
        }
        "desk" => call_desk(rest),
        "stop" => safety("safety_stop"),
        "resume" => safety("safety_resume"),
        _ => usage(),
    }
}

fn desk_endpoint() -> String {
    server::cache_endpoint("SHUTTLE_SOCKET", &format!("{NAME}-desk"), NAME, "desk.sock")
}

fn safety(tool: &str) -> ExitCode {
    let call = Call {
        agent_id: "user".to_owned(),
        tool: tool.to_owned(),
        args: serde_json::json!({}),
    };
    match block_on(
        Builder::new_current_thread(),
        server::call(&desk_endpoint(), &call),
    ) {
        Ok(out) if !out.is_error => {
            let _ = writeln!(std::io::stdout(), "{}", out.text);
            ExitCode::SUCCESS
        }
        Ok(out) => fail(out.text),
        Err(e) => fail(format_args!("no daemon answers: {e}")),
    }
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
    let out = block_on(
        Builder::new_current_thread(),
        server::call(&desk_endpoint(), &call),
    );
    match out {
        Ok(out) if !out.is_error => {
            let _ = writeln!(std::io::stdout(), "{}", out.text);
            ExitCode::SUCCESS
        }
        Ok(out) => fail(format_args!("desk {verb}: {}", out.text)),
        Err(e) => fail(format_args!("desk {verb}: {e}")),
    }
}

/// The daemon on a worker thread, and this OS's controls on this, the main, thread.
#[cfg(any(target_os = "macos", windows))]
fn controls_serve(endpoint: String) -> ExitCode {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ready = move |controls, safety: &Safety| {
            let hotkey = crate::hotkey::parse(&safety.kill_hotkey)?;
            let _ = tx.send(Ui {
                controls,
                hotkey,
                hotkey_label: safety.kill_hotkey.clone(),
                indicator: safety.indicator,
            });
            Ok(())
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
    ready: impl FnOnce(Arc<dyn Controls>, &Safety) -> Result<(), String>,
) -> Result<(), String> {
    let path = default_path(NAME);
    let mut config = Config::load(&path);
    #[cfg(any(target_os = "macos", windows))]
    if let Ok(c) = &config
        && let Err(e) = crate::hotkey::parse(&c.safety.kill_hotkey)
    {
        config = Err(format!("{}: {e}", path.display()));
    }
    let mut key_vars = vec![Config::default().api_key_env];
    if let Ok(c) = &config
        && !key_vars.contains(&c.api_key_env)
    {
        key_vars.push(c.api_key_env.clone());
    }
    let mut safety = config
        .as_mut()
        .map(|c| std::mem::take(&mut c.safety))
        .unwrap_or_default();
    let apps = std::mem::take(&mut safety.apps);
    let guard = Guard {
        script_run: config.as_ref().is_ok_and(|c| c.tools.script_run),
        confirm: safety.confirm,
        asker,
    };
    let models = config.map(Models::new);
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("locate this program: {e}"))?;
    let listener = server::bind(endpoint).await?;
    let own = driver::endpoint(endpoint);
    let bundled = if cfg!(any(target_os = "macos", windows)) {
        Bundled::start(&exe, &own).await?
    } else {
        None
    };
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
    let daemon = Arc::new(Daemon::new(
        server::native(driver, apps),
        models,
        runner,
        guard,
    ));
    if let Err(e) = ready(daemon.clone(), &safety) {
        if let Some(b) = bundled {
            b.stop().await;
        }
        return Err(e);
    }
    let served = tokio::select! {
        r = server::serve(daemon, listener) => r,
        () = shutdown() => Ok(()),
    };
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
