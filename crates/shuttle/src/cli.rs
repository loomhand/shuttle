//! The command line: `mcp`, `serve`, `desk VERB` and `--version`.

use std::io::Write as _;
use std::path::Path;
use std::process::{ExitCode, Stdio};
use std::sync::Arc;

use desk_core::text::cli_args;
use desk_core::wire::Call;
use shuttle_desk::driver::Driver;
use shuttle_desk::server;
use shuttle_models::Models;
use shuttle_models::config::{Config, default_path};
use tokio::runtime::Builder;

use crate::daemon::Daemon;
use crate::runner::Runner;
use crate::tools::DESK_VERBS;
use crate::{NAME, VERSION, mcp};

const DRIVER_TELEMETRY: &str = "CUA_DRIVER_RS_TELEMETRY_ENABLED";
const DRIVER_POLL: std::time::Duration = std::time::Duration::from_millis(100);
const DRIVER_POLLS: u32 = 100;

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
            exit(block_on(Builder::new_multi_thread(), serve(&endpoint)))
        }
        "desk" => call_desk(rest),
        _ => usage(),
    }
}

fn desk_endpoint() -> String {
    server::cache_endpoint("SHUTTLE_SOCKET", &format!("{NAME}-desk"), NAME, "desk.sock")
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
            eprintln!("{NAME}: {e}");
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
        Ok(out) => fail(&format!("desk {verb}: {}", out.text)),
        Err(e) => fail(&format!("desk {verb}: {e}")),
    }
}

async fn serve(endpoint: &str) -> Result<(), String> {
    let path = default_path(NAME);
    let config = Config::load(&path);
    let mut key_vars = vec![Config::default().api_key_env];
    if let Ok(c) = &config
        && !key_vars.contains(&c.api_key_env)
    {
        key_vars.push(c.api_key_env.clone());
    }
    let models = config.map(Models::new);
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("locate this program: {e}"))?;
    start_driver(&exe).await;
    let runner = Runner::beside(
        &exe,
        endpoint.to_owned(),
        path.with_file_name("flows"),
        key_vars,
    );
    server::serve(
        Arc::new(Daemon::new(server::native(), models, runner)),
        endpoint,
    )
    .await
}

async fn start_driver(exe: &Path) {
    let driver = exe.with_file_name(if cfg!(windows) {
        "cua-driver.exe"
    } else {
        "cua-driver"
    });
    let endpoint = Driver::default_endpoint();
    if !driver.is_file() || server::connect(&endpoint).await.is_ok() {
        return;
    }
    let mut cmd = std::process::Command::new(&driver);
    cmd.arg("serve")
        .env(DRIVER_TELEMETRY, "false")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, crate::DETACHED_PROCESS);
    let started = cmd.spawn();
    if let Err(e) = started {
        eprintln!("{NAME}: start {}: {e}", driver.display());
        return;
    }
    for _ in 0..DRIVER_POLLS {
        if server::connect(&endpoint).await.is_ok() {
            return;
        }
        tokio::time::sleep(DRIVER_POLL).await;
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
    done.map_or_else(|e| fail(&e), |()| ExitCode::SUCCESS)
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: {NAME} mcp | {NAME} serve [ENDPOINT] | {NAME} desk VERB [key=value ...|JSON] | {NAME} --version; desk verbs: {}",
        DESK_VERBS.join(", ")
    );
    ExitCode::from(2)
}

fn fail(e: &str) -> ExitCode {
    eprintln!("{NAME}: {e}");
    ExitCode::FAILURE
}
