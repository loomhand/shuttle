//! The bundled cua-driver: the daemon starts it on its own socket once it holds its endpoint, restarts it when it exits, and stops it when the daemon exits; on Linux the apps it launched into scopes of their own outlive it.

#[cfg(unix)]
use std::os::unix::fs::FileTypeExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use shuttle_desk::server;
use tokio::process::Command;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::{Running, home, log};

const TELEMETRY: &str = "CUA_DRIVER_RS_TELEMETRY_ENABLED";
const DRIVER_EMBEDDED: &str = "CUA_DRIVER_EMBEDDED";
const WINDOW_WATCH: &str = "CUA_DRIVER_WINDOW_CHANGE_TIMEOUT_MS";
const WINDOW_WATCH_MS: &str = "50";
const GLIDE_MS: &str = "1";
const LISTEN_WAIT: Duration = Duration::from_secs(10);
const STOP_WAIT: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(100);
const RESTART_DELAY: Duration = Duration::from_secs(1);

/// A cua-driver this daemon runs.
pub struct Bundled {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Bundled {
    /// Starts the cua-driver beside `exe` at `endpoint`, after stopping one that a killed daemon left there; none when no driver is bundled. Off macOS `overlay = false` starts it without its agent cursor; `embedded` starts it in cua-driver's embedded mode; on macOS `foreground` input bounds its window watch after an action, which foreground input holds no focus lease on and nobody reads.
    pub async fn start(
        exe: &Path,
        endpoint: &str,
        overlay: bool,
        embedded: bool,
        foreground: bool,
    ) -> Result<Option<Self>, String> {
        let bin = exe.with_file_name(if cfg!(windows) {
            "cua-driver.exe"
        } else {
            "cua-driver"
        });
        if !bin.is_file() {
            return Ok(None);
        }
        if !overlay && cfg!(target_os = "macos") {
            log(
                "[driver] overlay = false has no effect on macOS: front-window tracking needs the overlay",
            );
        }
        let proc = Proc {
            bin,
            endpoint: endpoint.to_owned(),
            pid_file: pid_file(endpoint)?,
            overlay: overlay || cfg!(target_os = "macos"),
            embedded,
            watch: foreground && cfg!(target_os = "macos"),
        };
        if server::connect(endpoint).await.is_ok() {
            proc.stop_stale().await?;
        }
        let running = proc.spawn().await?;
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(proc.supervise(running, stopped));
        Ok(Some(Self { stop, task }))
    }

    /// Asks the driver to exit and waits for it, killing it when it does not.
    pub async fn stop(self) {
        let _ = self.stop.send(());
        let _ = self.task.await;
    }
}

struct Proc {
    bin: PathBuf,
    endpoint: String,
    pid_file: PathBuf,
    overlay: bool,
    embedded: bool,
    watch: bool,
}

impl Proc {
    async fn spawn(&self) -> Result<Running, String> {
        #[cfg(unix)]
        clear_stale(&self.endpoint).await;
        let mut cmd = Command::new(&self.bin);
        cmd.args(["serve", "--socket", &self.endpoint, "--pid-file"])
            .arg(&self.pid_file)
            .args(["--glide-ms", GLIDE_MS])
            .args((!self.overlay).then_some("--no-overlay"))
            .env(TELEMETRY, "false")
            .envs(self.embedded.then_some((DRIVER_EMBEDDED, "1")))
            .envs(self.watch.then_some((WINDOW_WATCH, WINDOW_WATCH_MS)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut running = cmd
            .spawn()
            .map_err(|e| format!("start {}: {e}", self.bin.display()))
            .and_then(Running::alone)?;
        let mut waited = Duration::ZERO;
        while server::connect(&self.endpoint).await.is_err() {
            if let Ok(Some(status)) = running.child.try_wait() {
                return Err(format!(
                    "cua-driver exited with {status} before it listened at {}",
                    self.endpoint
                ));
            }
            if waited >= LISTEN_WAIT {
                return Err(format!(
                    "cua-driver did not listen at {} within {LISTEN_WAIT:?}",
                    self.endpoint
                ));
            }
            tokio::time::sleep(POLL).await;
            waited += POLL;
        }
        Ok(running)
    }

    async fn supervise(self, mut running: Running, mut stopped: oneshot::Receiver<()>) {
        loop {
            tokio::select! {
                _ = &mut stopped => break,
                status = running.child.wait() => log(format_args!("cua-driver exited with {status:?}; starting it again")),
            }
            tokio::select! {
                _ = &mut stopped => {
                    self.request_stop().await;
                    return;
                }
                spawned = async {
                    tokio::time::sleep(RESTART_DELAY).await;
                    self.spawn().await
                } => match spawned {
                    Ok(r) => running = r,
                    Err(e) => log(e),
                },
            }
        }
        self.request_stop().await;
        if tokio::time::timeout(STOP_WAIT, running.child.wait())
            .await
            .is_err()
        {
            let _ = running.child.kill().await;
        }
    }

    async fn request_stop(&self) {
        let stop = Command::new(&self.bin)
            .args(["stop", "--socket", &self.endpoint])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status();
        let _ = tokio::time::timeout(STOP_WAIT, stop).await;
    }

    async fn stop_stale(&self) -> Result<(), String> {
        self.request_stop().await;
        let mut waited = Duration::ZERO;
        while server::connect(&self.endpoint).await.is_ok() {
            if waited >= STOP_WAIT {
                return Err(format!(
                    "a cua-driver an earlier daemon left at {} does not stop",
                    self.endpoint
                ));
            }
            tokio::time::sleep(POLL).await;
            waited += POLL;
        }
        Ok(())
    }
}

/// The socket of the driver the daemon at `desk` runs: beside the desk socket, or a pipe named after the desk pipe on Windows.
pub fn endpoint(desk: &str) -> String {
    if cfg!(windows) {
        format!("{desk}-driver")
    } else {
        Path::new(desk)
            .with_file_name("driver.sock")
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(unix)]
async fn clear_stale(endpoint: &str) {
    let socket = std::fs::symlink_metadata(endpoint).is_ok_and(|m| m.file_type().is_socket());
    if socket
        && server::connect(endpoint)
            .await
            .is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused)
    {
        let _ = std::fs::remove_file(endpoint);
    }
}

fn pid_file(endpoint: &str) -> Result<PathBuf, String> {
    if !cfg!(windows) {
        return Ok(Path::new(endpoint).with_extension("pid"));
    }
    let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join(home());
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let pipe = endpoint.rsplit('\\').next().unwrap_or(endpoint);
    Ok(dir.join(format!("{pipe}.pid")))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::NAME;

    const FAKE: &str = r#"#!/usr/bin/env python3
import os, socket, sys, time
args = dict(zip(sys.argv[2::2], sys.argv[3::2]))
if sys.argv[1] == "stop":
    if os.path.exists(args["--socket"] + ".hang"):
        time.sleep(30)
    pid = int(open(args["--socket"] + ".fake-pid").read())
    if args.get("--expected-pid", str(pid)) != str(pid):
        sys.exit(1)
    os.kill(pid, 15)
    sys.exit(0)
child = os.fork()
if child:
    os.waitpid(child, 0)
    sys.exit(0)
open(args["--socket"] + ".fake-pid", "w").write(str(os.getpid()))
open(args["--socket"] + ".argv", "w").write(" ".join(sys.argv[1:]))
open(args["--pid-file"], "w").write(str(os.getpid()))
if os.path.exists(args["--socket"] + ".slow"):
    time.sleep(3)
if os.environ.get("CUA_DRIVER_EMBEDDED") == "1" and os.path.exists(args["--socket"]):
    sys.exit(4)
open(args["--socket"] + ".env", "w").write(os.environ.get("CUA_DRIVER_EMBEDDED", ""))
open(args["--socket"] + ".watch", "w").write(os.environ.get("CUA_DRIVER_WINDOW_CHANGE_TIMEOUT_MS", ""))
if os.path.exists(args["--socket"]):
    os.remove(args["--socket"])
s = socket.socket(socket.AF_UNIX)
s.bind(args["--socket"])
s.listen(8)
while True:
    s.accept()[0].close()
"#;

    fn fake(dir: &Path, script: &str) -> std::io::Result<PathBuf> {
        crate::script(&dir.join("cua-driver"), script)?;
        Ok(dir.join(NAME))
    }

    fn pid(endpoint: &str) -> Option<i32> {
        std::fs::read_to_string(format!("{endpoint}.fake-pid"))
            .ok()?
            .parse()
            .ok()
    }

    fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    async fn gone(pid: i32) -> bool {
        for _ in 0..50 {
            if !alive(pid) {
                return true;
            }
            tokio::time::sleep(POLL).await;
        }
        false
    }

    #[tokio::test]
    async fn no_bundled_driver_starts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = endpoint(&dir.path().join("desk.sock").to_string_lossy());
        let started = Bundled::start(&dir.path().join(NAME), &endpoint, true, false, false).await;
        assert!(matches!(started, Ok(None)));
    }

    #[tokio::test]
    async fn the_driver_listens_on_its_own_socket_comes_back_after_it_dies_and_stops_with_the_daemon()
     {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake(dir.path(), FAKE).unwrap();
        let endpoint = endpoint(&dir.path().join("desk.sock").to_string_lossy());
        let driver = Bundled::start(&exe, &endpoint, true, false, false)
            .await
            .unwrap()
            .unwrap();
        assert!(server::connect(&endpoint).await.is_ok());
        let first = pid(&endpoint).unwrap();
        // SAFETY: the pid is the fake driver this test started.
        unsafe { libc::kill(first, libc::SIGKILL) };
        assert!(gone(first).await);
        let mut second = first;
        for _ in 0..100 {
            tokio::time::sleep(POLL).await;
            second = pid(&endpoint).unwrap_or(first);
            if second != first && server::connect(&endpoint).await.is_ok() {
                break;
            }
        }
        driver.stop().await;
        assert_ne!(second, first, "the driver came back");
        assert!(gone(second).await, "the driver stopped with the daemon");
    }

    #[tokio::test]
    async fn the_driver_draws_its_cursor_and_glides_it_in_one_millisecond() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake(dir.path(), FAKE).unwrap();
        let endpoint = endpoint(&dir.path().join("desk.sock").to_string_lossy());
        let driver = Bundled::start(&exe, &endpoint, true, false, false)
            .await
            .unwrap()
            .unwrap();
        let argv = std::fs::read_to_string(format!("{endpoint}.argv")).unwrap_or_default();
        driver.stop().await;
        assert!(argv.contains("--glide-ms 1"), "{argv}");
        assert!(!argv.contains("--no-overlay"), "{argv}");
    }

    #[tokio::test]
    async fn with_the_overlay_off_the_driver_starts_without_its_cursor_except_on_macos() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake(dir.path(), FAKE).unwrap();
        let endpoint = endpoint(&dir.path().join("desk.sock").to_string_lossy());
        let driver = Bundled::start(&exe, &endpoint, false, false, false)
            .await
            .unwrap()
            .unwrap();
        let argv = std::fs::read_to_string(format!("{endpoint}.argv")).unwrap_or_default();
        driver.stop().await;
        assert_eq!(
            argv.contains("--no-overlay"),
            !cfg!(target_os = "macos"),
            "{argv}"
        );
    }

    #[tokio::test]
    async fn foreground_input_bounds_the_window_watch_of_the_macos_driver() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake(dir.path(), FAKE).unwrap();
        let bounded = if cfg!(target_os = "macos") { "50" } else { "" };
        for (foreground, want) in [(true, bounded), (false, "")] {
            let sock = dir.path().join(format!("{foreground}.sock"));
            let endpoint = endpoint(&sock.to_string_lossy());
            let driver = Bundled::start(&exe, &endpoint, true, false, foreground)
                .await
                .unwrap()
                .unwrap();
            let watch = std::fs::read_to_string(format!("{endpoint}.watch")).unwrap_or_default();
            driver.stop().await;
            assert_eq!(watch, want, "foreground {foreground}");
        }
    }

    #[tokio::test]
    async fn a_stop_the_driver_never_answers_still_leaves_no_driver() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake(dir.path(), FAKE).unwrap();
        let endpoint = endpoint(&dir.path().join("desk.sock").to_string_lossy());
        let driver = Bundled::start(&exe, &endpoint, true, false, false)
            .await
            .unwrap()
            .unwrap();
        let server = pid(&endpoint).unwrap();
        std::fs::write(format!("{endpoint}.hang"), "").unwrap();
        driver.stop().await;
        assert!(gone(server).await, "the forked driver outlived the daemon");
    }

    #[tokio::test]
    async fn a_stop_while_the_driver_restarts_leaves_no_half_started_driver() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake(dir.path(), FAKE).unwrap();
        let endpoint = endpoint(&dir.path().join("desk.sock").to_string_lossy());
        let driver = Bundled::start(&exe, &endpoint, true, false, false)
            .await
            .unwrap()
            .unwrap();
        let first = pid(&endpoint).unwrap();
        std::fs::write(format!("{endpoint}.slow"), "").unwrap();
        std::fs::write(format!("{endpoint}.hang"), "").unwrap();
        // SAFETY: the pid is the fake driver this test started.
        unsafe { libc::kill(first, libc::SIGKILL) };
        let mut second = first;
        for _ in 0..50 {
            tokio::time::sleep(POLL).await;
            second = pid(&endpoint).unwrap_or(first);
            if second != first {
                break;
            }
        }
        driver.stop().await;
        assert_ne!(second, first, "the driver was restarting");
        assert!(
            gone(second).await,
            "the half-started driver outlived the daemon"
        );
    }

    #[tokio::test]
    async fn a_driver_a_killed_daemon_left_is_stopped_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake(dir.path(), FAKE).unwrap();
        let endpoint = endpoint(&dir.path().join("desk.sock").to_string_lossy());
        let mut left = std::process::Command::new(exe.with_file_name("cua-driver"))
            .args(["serve", "--socket", &endpoint, "--pid-file"])
            .arg(pid_file(&endpoint).unwrap())
            .spawn()
            .unwrap();
        while server::connect(&endpoint).await.is_err() {
            tokio::time::sleep(POLL).await;
        }
        let stale = pid(&endpoint).unwrap();
        let driver = Bundled::start(&exe, &endpoint, true, false, false)
            .await
            .unwrap()
            .unwrap();
        let _ = left.wait();
        let stopped = gone(stale).await;
        let replaced = pid(&endpoint) != Some(stale) && server::connect(&endpoint).await.is_ok();
        driver.stop().await;
        assert!(
            stopped && replaced,
            "stopped {stopped}, replaced {replaced}"
        );
    }

    #[tokio::test]
    async fn a_driver_that_exits_before_it_listens_fails_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake(dir.path(), "#!/bin/sh\nexit 3\n").unwrap();
        let endpoint = endpoint(&dir.path().join("desk.sock").to_string_lossy());
        let err = Bundled::start(&exe, &endpoint, true, false, false)
            .await
            .err()
            .unwrap_or_default();
        assert!(
            err.contains("exited with") && err.contains("before it listened"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn an_embedded_driver_starts_and_restarts_over_the_socket_a_dead_driver_left() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake(dir.path(), FAKE).unwrap();
        let endpoint = endpoint(&dir.path().join("desk.sock").to_string_lossy());
        drop(std::os::unix::net::UnixListener::bind(&endpoint).unwrap());
        let driver = Bundled::start(&exe, &endpoint, true, true, false)
            .await
            .unwrap()
            .unwrap();
        let env = std::fs::read_to_string(format!("{endpoint}.env")).unwrap_or_default();
        let first = pid(&endpoint).unwrap();
        // SAFETY: the pid is the fake driver this test started.
        unsafe { libc::kill(first, libc::SIGKILL) };
        assert!(gone(first).await);
        let mut back = false;
        for _ in 0..100 {
            tokio::time::sleep(POLL).await;
            if pid(&endpoint).is_some_and(|p| p != first)
                && server::connect(&endpoint).await.is_ok()
            {
                back = true;
                break;
            }
        }
        driver.stop().await;
        assert_eq!(env, "1");
        assert!(
            back,
            "the embedded driver came back over the socket it left"
        );
    }

    #[tokio::test]
    async fn only_a_socket_that_refuses_a_connect_is_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.sock").to_string_lossy().into_owned();
        let dead = dir.path().join("dead.sock").to_string_lossy().into_owned();
        let missing = dir
            .path()
            .join("missing.sock")
            .to_string_lossy()
            .into_owned();
        let file = dir.path().join("file.sock").to_string_lossy().into_owned();
        let link = dir.path().join("link.sock").to_string_lossy().into_owned();
        let _listener = std::os::unix::net::UnixListener::bind(&live).unwrap();
        drop(std::os::unix::net::UnixListener::bind(&dead).unwrap());
        std::fs::write(&file, "").unwrap();
        std::os::unix::fs::symlink(&file, &link).unwrap();
        for socket in [&live, &dead, &missing, &file, &link] {
            clear_stale(socket).await;
        }
        assert!(Path::new(&live).exists());
        assert!(!Path::new(&dead).exists());
        assert!(!Path::new(&missing).exists());
        assert!(Path::new(&file).exists());
        assert!(std::fs::symlink_metadata(&link).is_ok());
    }
}
