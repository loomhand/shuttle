use std::process::ExitCode;

use cocoonbox_desk::desk::cli_args;
use cocoonbox_desk::rpc::Call;
use cocoonbox_desk::server;

const USAGE: &str = "usage: cocoonbox-desk serve [ENDPOINT] | cocoonbox-desk VERB [key=value ...|JSON]; verbs: windows, snapshot, find, read, press, act, at, key, type, click, focus, close, launch, shot, screen";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((verb, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cocoonbox-desk: start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    if verb == "serve" {
        let endpoint = rest
            .first()
            .cloned()
            .unwrap_or_else(server::default_endpoint);
        return match runtime.block_on(serve(&endpoint)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("cocoonbox-desk: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let call = match cli_args(rest) {
        Ok(args) => Call {
            agent_id: "desk".to_owned(),
            tool: format!("desk_{verb}"),
            args,
        },
        Err(e) => {
            eprintln!("cocoonbox-desk: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match runtime.block_on(server::call(&server::default_endpoint(), &call)) {
        Ok(out) if !out.is_error => {
            println!("{}", out.text);
            ExitCode::SUCCESS
        }
        Ok(out) => {
            eprintln!("desk {verb}: {}", out.text);
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("desk {verb}: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "macos")]
async fn serve(endpoint: &str) -> Result<(), String> {
    use cocoonbox_desk::{ax::Ax, desk::Desk, driver::Driver, keys::Os};
    let driver = Driver::new(Driver::default_endpoint());
    server::serve(
        std::sync::Arc::new(Desk::new(Ax::open, driver, Os::Mac)),
        endpoint,
    )
    .await
}

#[cfg(windows)]
async fn serve(endpoint: &str) -> Result<(), String> {
    use cocoonbox_desk::{desk::Desk, driver::Driver, keys::Os, uia::Uia};
    let driver = Driver::new(Driver::default_endpoint());
    server::serve(
        std::sync::Arc::new(Desk::new(Uia::open, driver, Os::Windows)),
        endpoint,
    )
    .await
}

#[cfg(not(any(windows, target_os = "macos")))]
async fn serve(_: &str) -> Result<(), String> {
    Err("no desk backend for this OS; the Linux toolhost serves the desk there".to_owned())
}
