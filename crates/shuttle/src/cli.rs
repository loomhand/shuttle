//! The command line: `mcp`, `serve`, `desk VERB` and `--version`.

use std::process::ExitCode;
use std::sync::Arc;

use desk_core::text::cli_args;
use desk_core::wire::Call;
use shuttle_desk::server;
use shuttle_models::Models;
use shuttle_models::config::{Config, default_path};

use crate::daemon::Daemon;
use crate::runner::Runner;
use crate::tools::DESK_VERBS;
use crate::{NAME, VERSION, mcp};

/// Runs the command in `args` (the program name excluded) and returns its exit code.
pub fn run(args: &[String]) -> ExitCode {
    let Some((cmd, rest)) = args.split_first() else {
        return usage();
    };
    match cmd.as_str() {
        "--version" | "-V" => {
            println!("{NAME} {VERSION}");
            ExitCode::SUCCESS
        }
        "mcp" => {
            let endpoint = desk_endpoint();
            let served = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("start runtime: {e}"))
                .and_then(|r| {
                    r.block_on(mcp::serve(
                        tokio::io::BufReader::new(tokio::io::stdin()),
                        tokio::io::stdout(),
                        &endpoint,
                    ))
                });
            match served {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(&e),
            }
        }
        "serve" => {
            let endpoint = rest.first().cloned().unwrap_or_else(desk_endpoint);
            let served = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("start runtime: {e}"))
                .and_then(|r| r.block_on(serve(&endpoint)));
            match served {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(&e),
            }
        }
        "desk" => call_desk(rest),
        _ => usage(),
    }
}

fn desk_endpoint() -> String {
    if let Some(e) = std::env::var("SHUTTLE_SOCKET")
        .ok()
        .filter(|e| !e.is_empty())
    {
        return e;
    }
    if cfg!(windows) {
        return format!(r"\\.\pipe\{NAME}-desk");
    }
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/Library/Caches/{NAME}/desk.sock")
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
    let out = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("start runtime: {e}"))
        .and_then(|r| r.block_on(server::call(&desk_endpoint(), &call)));
    match out {
        Ok(out) if !out.is_error => {
            println!("{}", out.text);
            ExitCode::SUCCESS
        }
        Ok(out) => fail(&format!("desk {verb}: {}", out.text)),
        Err(e) => fail(&format!("desk {verb}: {e}")),
    }
}

async fn serve(endpoint: &str) -> Result<(), String> {
    let config = Config::load(&default_path(NAME));
    let key_var = config
        .as_ref()
        .map_or_else(|_| Config::default().api_key_env, |c| c.api_key_env.clone());
    let models = config.map(Models::new);
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("locate this program: {e}"))?;
    let runner = Runner::beside(&exe, endpoint.to_owned(), key_var);
    server::serve(
        Arc::new(Daemon::new(server::native(), models, runner)),
        endpoint,
    )
    .await
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
