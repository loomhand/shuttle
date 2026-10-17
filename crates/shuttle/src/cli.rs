//! The command line: `mcp`, `serve`, `desk VERB` and `--version`.

use std::process::ExitCode;
use std::sync::Arc;

use desk_core::text::cli_args;
use desk_core::wire::Call;
use shuttle_desk::server;
use shuttle_models::Models;
use shuttle_models::config::{Config, default_path};
use tokio::runtime::Builder;

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
            println!("{}", out.text);
            ExitCode::SUCCESS
        }
        Ok(out) => fail(&format!("desk {verb}: {}", out.text)),
        Err(e) => fail(&format!("desk {verb}: {e}")),
    }
}

async fn serve(endpoint: &str) -> Result<(), String> {
    let config = Config::load(&default_path(NAME));
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
    let runner = Runner::beside(&exe, endpoint.to_owned(), key_vars);
    server::serve(
        Arc::new(Daemon::new(server::native(), models, runner)),
        endpoint,
    )
    .await
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
