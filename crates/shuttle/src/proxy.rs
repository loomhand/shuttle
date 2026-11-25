//! The way from the MCP server to the daemon: one call per connection, starting the daemon when nothing listens.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use desk_core::wire::{Call, Output};
use shuttle_desk::server::{self, Conn};
use tokio::process::{Child, Command};

const START_WAIT: Duration = Duration::from_secs(10);
const START_POLL: Duration = Duration::from_millis(10);
const EXIT_WAIT: Duration = Duration::from_secs(1);

/// Sends `call` to the daemon at `endpoint`, starting it when nothing listens unless a host app embeds shuttle; a call that may have reached it is never resent.
pub async fn call(endpoint: &str, call: &Call) -> Result<Output, String> {
    reach(endpoint, call, crate::embedded()).await
}

async fn reach(endpoint: &str, call: &Call, embedded: bool) -> Result<Output, String> {
    match server::connect(endpoint).await {
        Ok(conn) => return server::exchange(conn, call).await,
        Err(e) if !matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
            return Err(format!("connect {endpoint}: {e}"));
        }
        Err(_) if embedded => {
            return Err(format!(
                "no {NAME} daemon answers at {endpoint}; with {EMBEDDED}=1 the app that embeds {NAME} starts `{NAME} serve` itself",
                NAME = crate::NAME,
                EMBEDDED = crate::EMBEDDED
            ));
        }
        Err(_) => {}
    }
    let (conn, child) = listening(endpoint, start(endpoint).await?).await?;
    answered(endpoint, conn, child, call).await
}

async fn answered(
    endpoint: &str,
    conn: Conn,
    child: Option<Child>,
    call: &Call,
) -> Result<Output, String> {
    match (server::exchange(conn, call).await, child) {
        (Err(e), Some(child)) => Err(daemon_error(child, e).await),
        (Err(e), None) => Err(opened_error(endpoint, e).await),
        (out, _) => out,
    }
}

async fn listening(
    endpoint: &str,
    mut child: Option<Child>,
) -> Result<(Conn, Option<Child>), String> {
    let mut waited = Duration::ZERO;
    loop {
        if let Ok(conn) = server::connect(endpoint).await {
            return Ok((conn, child));
        }
        match child.as_mut().map(Child::try_wait) {
            Some(Ok(Some(status))) => return Err(exited(status)),
            None => {
                if let Ok(e) = std::fs::read_to_string(failed(endpoint)) {
                    return Err(format!("the daemon did not start: {e}"));
                }
            }
            _ => {}
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

async fn daemon_error(mut child: Child, e: String) -> String {
    match tokio::time::timeout(EXIT_WAIT, child.wait()).await {
        Ok(Ok(status)) => format!("{e}; {}", exited(status)),
        _ => e,
    }
}

async fn opened_error(endpoint: &str, e: String) -> String {
    let mut waited = Duration::ZERO;
    while waited < EXIT_WAIT {
        if let Ok(why) = std::fs::read_to_string(failed(endpoint)) {
            return format!("{e}; the daemon did not start: {why}");
        }
        tokio::time::sleep(START_POLL).await;
        waited += START_POLL;
    }
    e
}

fn exited(status: ExitStatus) -> String {
    format!(
        "the daemon exited with {status}; run `{} serve` to see why",
        crate::NAME
    )
}

/// Starts the daemon; returns the child to watch when it runs as one.
async fn start(endpoint: &str) -> Result<Option<Child>, String> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("locate this program: {e}"))?;
    if let Some(app) = app_bundle(&exe) {
        let _ = std::fs::remove_file(failed(endpoint));
        // TCC attributes a daemon LaunchServices starts to its app, not to the terminal or agent that runs `mcp`
        let status = open_command(app, endpoint)
            .status()
            .await
            .map_err(|e| format!("start the daemon: {e}"))?;
        if !status.success() {
            return Err(format!("start the daemon: open exited with {status}"));
        }
        return Ok(None);
    }
    daemon(&exe, endpoint)
        .spawn()
        .map(Some)
        .map_err(|e| format!("start the daemon: {e}"))
}

fn open_command(app: &Path, endpoint: &str) -> Command {
    let mut cmd = Command::new("open");
    cmd.args(["-n", "-g", "-a"])
        .arg(app)
        .args(["--args", "serve", endpoint])
        .env_remove(crate::http::TOKEN_ENV);
    cmd
}

fn daemon(exe: &Path, endpoint: &str) -> Command {
    let mut cmd = Command::new(exe);
    cmd.args(["serve", endpoint])
        .env_remove(crate::http::TOKEN_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    {
        cmd.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
        crate::win::disinherit_stdio();
    }
    cmd
}

pub(crate) fn failed(endpoint: &str) -> PathBuf {
    format!("{endpoint}.err").into()
}

pub(crate) fn app_bundle(exe: &Path) -> Option<&Path> {
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
    fn the_daemon_starts_without_the_http_token_its_children_would_inherit() {
        let cmd = daemon(Path::new("/bin/true"), "desk.sock");
        let token = std::ffi::OsStr::new(crate::http::TOKEN_ENV);
        assert!(
            cmd.as_std()
                .get_envs()
                .any(|(k, v)| k == token && v.is_none()),
            "{:?}",
            cmd.as_std().get_envs().collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_app_bundle_daemon_starts_through_open_without_the_http_token() {
        let cmd = open_command(Path::new("/Applications/Shuttle.app"), "desk.sock");
        let token = std::ffi::OsStr::new(crate::http::TOKEN_ENV);
        assert!(
            cmd.as_std()
                .get_envs()
                .any(|(k, v)| k == token && v.is_none()),
            "{:?}",
            cmd.as_std().get_envs().collect::<Vec<_>>()
        );
    }

    #[test]
    fn only_a_macos_binary_inside_an_app_bundle_starts_through_its_app() {
        let inside = Path::new("/Users/u/Applications/Loomhand/Shuttle.app/Contents/MacOS/shuttle");
        let bare = Path::new("/Users/u/.local/bin/shuttle");
        let want = cfg!(target_os = "macos")
            .then_some(Path::new("/Users/u/Applications/Loomhand/Shuttle.app"));
        assert_eq!(app_bundle(inside), want);
        assert_eq!(app_bundle(bare), None);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn the_started_daemon_leads_a_process_group_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("serve"), "sleep 5\n").unwrap();
        let mut child = daemon(Path::new("/bin/sh"), "unused")
            .current_dir(dir.path())
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let group: u32 = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(2))
            .and_then(|g| g.parse().ok())
            .unwrap();
        child.kill().await.unwrap();
        assert_eq!(group, pid, "{stat}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_started_daemon_that_exits_before_it_answers_names_its_exit() {
        let gone = Command::new("sh").args(["-c", "exit 3"]).spawn().unwrap();
        let err = daemon_error(gone, "read frame length: early eof".to_owned()).await;
        assert!(
            err.starts_with("read frame length: early eof; ")
                && err.contains("exited with exit status: 3"),
            "{err}"
        );
        let alive = Command::new("sleep")
            .arg("5")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        assert_eq!(
            daemon_error(alive, "early eof".to_owned()).await,
            "early eof"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_daemon_started_through_its_app_that_fails_is_answered_with_its_own_error_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        std::fs::write(failed(&endpoint), "[android] sets no ui_jar").unwrap();
        let started = std::time::Instant::now();
        let Err(err) = listening(&endpoint, None).await else {
            panic!("a daemon that failed was reached");
        };
        assert_eq!(err, "the daemon did not start: [android] sets no ui_jar");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_daemon_started_through_its_app_that_drops_the_call_and_fails_names_its_own_error() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let eof = "no answer to desk_windows could be read".to_owned();
        tokio::time::pause();
        assert_eq!(opened_error(&endpoint, eof.clone()).await, eof);
        std::fs::write(failed(&endpoint), "[android] sets no ui_jar").unwrap();
        assert_eq!(
            opened_error(&endpoint, eof).await,
            "no answer to desk_windows could be read; the daemon did not start: [android] sets no ui_jar"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_call_an_app_started_daemon_drops_is_answered_with_its_start_error() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let listener = tokio::net::UnixListener::bind(&endpoint).unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let _: Call = read_frame(&mut s).await.unwrap();
        });
        std::fs::write(failed(&endpoint), "[android] sets no ui_jar").unwrap();
        let conn = server::connect(&endpoint).await.unwrap();
        let ask = Call {
            agent_id: "t".into(),
            tool: "desk_windows".into(),
            args: serde_json::json!({}),
        };
        let err = answered(&endpoint, conn, None, &ask).await.unwrap_err();
        assert!(
            err.starts_with("no answer to desk_windows could be read")
                && err.ends_with("; the daemon did not start: [android] sets no ui_jar"),
            "{err}"
        );
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
        let err = call(&endpoint, &ask).await.unwrap_err();
        assert!(
            err.contains("no answer to desk_type could be read")
                && err.contains("whether it ran is not known"),
            "{err}"
        );
        assert_eq!(*seen.lock().unwrap(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_embedded_mcp_with_no_daemon_fails_at_once_and_starts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let ask = Call {
            agent_id: "t".into(),
            tool: "desk_windows".into(),
            args: serde_json::json!({}),
        };
        let started = std::time::Instant::now();
        let err = reach(&endpoint, &ask, true).await.unwrap_err();
        assert!(err.contains("SHUTTLE_EMBEDDED=1"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert!(!Path::new(&endpoint).exists());
    }
}
