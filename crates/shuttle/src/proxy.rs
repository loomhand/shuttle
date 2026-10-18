//! The way from the MCP server to the daemon: one call per connection, starting the daemon when nothing listens.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use desk_core::wire::{Call, Output};
use shuttle_desk::server::{self, Conn};
use tokio::process::{Child, Command};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const START_WAIT: Duration = Duration::from_secs(10);
const START_POLL: Duration = Duration::from_millis(100);

/// Sends `call` to the daemon at `endpoint`, starting it when nothing listens; a call that may have reached it is never resent.
pub async fn call(endpoint: &str, call: &Call) -> Result<Output, String> {
    let conn = match server::connect(endpoint).await {
        Ok(conn) => conn,
        Err(_) => started(endpoint).await?,
    };
    server::exchange(conn, call).await
}

async fn started(endpoint: &str) -> Result<Conn, String> {
    let mut child = start(endpoint).await?;
    let mut waited = Duration::ZERO;
    loop {
        if let Ok(conn) = server::connect(endpoint).await {
            return Ok(conn);
        }
        if let Some(Ok(status)) = child
            .as_mut()
            .map(Child::try_wait)
            .and_then(Result::transpose)
        {
            return Err(format!(
                "the daemon exited with {status}; run `{} serve` to see why",
                crate::NAME
            ));
        }
        if waited >= START_WAIT {
            return Err(format!(
                "the daemon did not start at {endpoint} within {START_WAIT:?}"
            ));
        }
        tokio::time::sleep(START_POLL).await;
        waited += START_POLL;
    }
}

/// Starts the daemon; returns the child to watch when it runs as one.
async fn start(endpoint: &str) -> Result<Option<Child>, String> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("locate this program: {e}"))?;
    if let Some(app) = app_bundle(&exe) {
        // TCC attributes a daemon LaunchServices starts to its app, not to the terminal or agent that runs `mcp`
        let status = Command::new("open")
            .args(["-n", "-g", "-a"])
            .arg(app)
            .args(["--args", "serve", endpoint])
            .status()
            .await
            .map_err(|e| format!("start the daemon: {e}"))?;
        if !status.success() {
            return Err(format!("start the daemon: open exited with {status}"));
        }
        return Ok(None);
    }
    let mut cmd = Command::new(&exe);
    cmd.args(["serve", endpoint])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
        crate::win::disinherit_stdio();
    }
    cmd.spawn()
        .map(Some)
        .map_err(|e| format!("start the daemon: {e}"))
}

fn app_bundle(exe: &Path) -> Option<&Path> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::sync::Arc;

    #[cfg(unix)]
    use desk_core::wire::read_frame;

    use super::*;

    #[test]
    fn only_a_macos_binary_inside_an_app_bundle_starts_through_its_app() {
        let inside =
            Path::new("/Users/u/Applications/Cocoon Shuttle.app/Contents/MacOS/cocoon-shuttle");
        let bare = Path::new("/Users/u/.local/bin/cocoon-shuttle");
        let want = cfg!(target_os = "macos")
            .then_some(Path::new("/Users/u/Applications/Cocoon Shuttle.app"));
        assert_eq!(app_bundle(inside), want);
        assert_eq!(app_bundle(bare), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_call_the_daemon_dropped_is_an_error_and_never_sent_again() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let listener = tokio::net::UnixListener::bind(&endpoint).unwrap();
        let seen = Arc::new(std::sync::Mutex::new(0));
        let count = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                let _: Call = read_frame(&mut s).await.unwrap();
                *count.lock().unwrap() += 1;
            }
        });
        let ask = Call {
            agent_id: "t".into(),
            tool: "desk_type".into(),
            args: serde_json::json!({"text": "once"}),
        };
        assert!(call(&endpoint, &ask).await.is_err());
        assert_eq!(*seen.lock().unwrap(), 1);
    }
}
