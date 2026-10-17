//! The bundled cua-driver: the daemon starts it on its own socket once it holds its endpoint, restarts it when it exits, and stops it when the daemon exits.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use shuttle_desk::server;
use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::NAME;

const TELEMETRY: &str = "CUA_DRIVER_RS_TELEMETRY_ENABLED";
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
    /// Starts the cua-driver beside `exe` at `endpoint`, after stopping one that a killed daemon left there; none when no driver is bundled.
    pub async fn start(exe: &Path, endpoint: &str) -> Result<Option<Self>, String> {
        let bin = exe.with_file_name(if cfg!(windows) {
            "cua-driver.exe"
        } else {
            "cua-driver"
        });
        if !bin.is_file() {
            return Ok(None);
        }
        let proc = Proc {
            bin,
            endpoint: endpoint.to_owned(),
            pid_file: pid_file(endpoint)?,
        };
        if server::connect(endpoint).await.is_ok() {
            proc.stop_stale().await?;
        }
        let child = proc.spawn().await?;
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(proc.supervise(child, stopped));
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
}

impl Proc {
    async fn spawn(&self) -> Result<Child, String> {
        let mut child = Command::new(&self.bin)
            .args(["serve", "--socket", &self.endpoint, "--pid-file"])
            .arg(&self.pid_file)
            .env(TELEMETRY, "false")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("start {}: {e}", self.bin.display()))?;
        let mut waited = Duration::ZERO;
        while server::connect(&self.endpoint).await.is_err() {
            if let Ok(Some(status)) = child.try_wait() {
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
        Ok(child)
    }

    async fn supervise(self, mut child: Child, mut stopped: oneshot::Receiver<()>) {
        loop {
            tokio::select! {
                _ = &mut stopped => break,
                status = child.wait() => log(&format!("cua-driver exited with {status:?}; starting it again")),
            }
            tokio::select! {
                _ = &mut stopped => return,
                spawned = async {
                    tokio::time::sleep(RESTART_DELAY).await;
                    self.spawn().await
                } => match spawned {
                    Ok(c) => child = c,
                    Err(e) => log(&e),
                },
            }
        }
        self.request_stop().await;
        if tokio::time::timeout(STOP_WAIT, child.wait()).await.is_err() {
            let _ = child.kill().await;
        }
    }

    async fn request_stop(&self) {
        let stop = Command::new(&self.bin)
            .args(["stop", "--socket", &self.endpoint])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
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

fn pid_file(endpoint: &str) -> Result<PathBuf, String> {
    if !cfg!(windows) {
        return Ok(Path::new(endpoint).with_extension("pid"));
    }
    let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join(NAME);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let pipe = endpoint.rsplit('\\').next().unwrap_or(endpoint);
    Ok(dir.join(format!("{pipe}.pid")))
}

fn log(line: &str) {
    let _ = writeln!(std::io::stderr(), "{NAME}: {line}");
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    const FAKE: &str = r#"#!/usr/bin/env python3
import os, socket, sys
args = dict(zip(sys.argv[2::2], sys.argv[3::2]))
if sys.argv[1] == "stop":
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
open(args["--pid-file"], "w").write(str(os.getpid()))
if os.path.exists(args["--socket"]):
    os.remove(args["--socket"])
s = socket.socket(socket.AF_UNIX)
s.bind(args["--socket"])
s.listen(8)
while True:
    s.accept()[0].close()
"#;

    fn fake(dir: &Path, script: &str) -> std::io::Result<PathBuf> {
        let bin = dir.join("cua-driver");
        std::fs::write(&bin, script)?;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))?;
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
        let started = Bundled::start(&dir.path().join(NAME), &endpoint).await;
        assert!(matches!(started, Ok(None)));
    }

    #[tokio::test]
    async fn the_driver_listens_on_its_own_socket_comes_back_after_it_dies_and_stops_with_the_daemon()
     {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake(dir.path(), FAKE).unwrap();
        let endpoint = endpoint(&dir.path().join("desk.sock").to_string_lossy());
        let driver = Bundled::start(&exe, &endpoint).await.unwrap().unwrap();
        assert!(server::connect(&endpoint).await.is_ok());
        let first = pid(&endpoint).unwrap();
        // SAFETY: the pid is the fake driver this test started.
        unsafe { libc::kill(first, libc::SIGKILL) };
        assert!(gone(first).await);
        let mut second = first;
        for _ in 0..50 {
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
        let driver = Bundled::start(&exe, &endpoint).await.unwrap().unwrap();
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
        let err = Bundled::start(&exe, &endpoint)
            .await
            .err()
            .unwrap_or_default();
        assert!(
            err.contains("exited with") && err.contains("before it listened"),
            "{err}"
        );
    }
}
