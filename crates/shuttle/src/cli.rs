//! The command line: `mcp`, `serve`, `desk VERB` and `--version`.

use std::process::ExitCode;

use desk_core::text::cli_args;
use desk_core::wire::Call;
use shuttle_desk::server;

use crate::{NAME, VERSION, mcp};

/// Runs the command in `args` (the program name excluded) and returns its exit code.
pub fn run(args: &[String]) -> ExitCode {
    let Some((cmd, rest)) = args.split_first() else {
        eprintln!("{}", usage());
        return ExitCode::from(2);
    };
    match cmd.as_str() {
        "--version" | "-V" | "version" => {
            println!("{NAME} {VERSION}");
            ExitCode::SUCCESS
        }
        "mcp" => match mcp::serve(std::io::stdin().lock(), std::io::stdout().lock()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        },
        "serve" => {
            let endpoint = rest.first().cloned().unwrap_or_else(desk_endpoint);
            block_on(serve(&endpoint))
        }
        "desk" => desk(rest),
        _ => {
            eprintln!("{}", usage());
            ExitCode::from(2)
        }
    }
}

/// The desk endpoint: `SHUTTLE_SOCKET`, else this OS's default.
pub fn desk_endpoint() -> String {
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
    if cfg!(target_os = "macos") {
        return format!("{home}/Library/Caches/{NAME}/desk.sock");
    }
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) if !dir.is_empty() => format!("{dir}/{NAME}/desk.sock"),
        _ => format!("{home}/.cache/{NAME}/desk.sock"),
    }
}

fn desk(rest: &[String]) -> ExitCode {
    let Some((verb, rest)) = rest.split_first() else {
        eprintln!("{}", usage());
        return ExitCode::from(2);
    };
    let call = match cli_args(rest) {
        Ok(args) => Call {
            agent_id: "desk".to_owned(),
            tool: format!("desk_{verb}"),
            args,
        },
        Err(e) => {
            eprintln!("{NAME}: {e}\n{}", usage());
            return ExitCode::from(2);
        }
    };
    let out = match runtime().and_then(|r| r.block_on(server::call(&desk_endpoint(), &call))) {
        Ok(out) => out,
        Err(e) => return fail(&format!("desk {verb}: {e}")),
    };
    if out.is_error {
        return fail(&format!("desk {verb}: {}", out.text));
    }
    println!("{}", out.text);
    ExitCode::SUCCESS
}

fn usage() -> String {
    format!(
        "usage: {NAME} mcp | {NAME} serve [ENDPOINT] | {NAME} desk VERB [key=value ...|JSON] | {NAME} --version; desk verbs: windows, snapshot, find, read, press, act, at, key, type, click, focus, close, launch, shot, screen"
    )
}

fn block_on(work: impl Future<Output = Result<(), String>>) -> ExitCode {
    match runtime().and_then(|r| r.block_on(work)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("start runtime: {e}"))
}

fn fail(e: &str) -> ExitCode {
    eprintln!("{NAME}: {e}");
    ExitCode::FAILURE
}

#[cfg(target_os = "macos")]
async fn serve(endpoint: &str) -> Result<(), String> {
    use shuttle_desk::{ax::Ax, desk::Desk, driver::Driver, keys::Os};
    let driver = Driver::new(Driver::default_endpoint());
    server::serve(
        std::sync::Arc::new(Desk::new(Ax::open, driver, Os::Mac)),
        endpoint,
    )
    .await
}

#[cfg(windows)]
async fn serve(endpoint: &str) -> Result<(), String> {
    use shuttle_desk::{desk::Desk, driver::Driver, keys::Os, uia::Uia};
    let driver = Driver::new(Driver::default_endpoint());
    server::serve(
        std::sync::Arc::new(Desk::new(Uia::open, driver, Os::Windows)),
        endpoint,
    )
    .await
}

#[cfg(not(any(windows, target_os = "macos")))]
async fn serve(_: &str) -> Result<(), String> {
    Err("no desk backend for Linux yet; see docs/platforms.md".to_owned())
}
