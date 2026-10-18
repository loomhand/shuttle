//! Scripts and GUI plans on the bundled Node runtime, each run in its own scratch directory with the desk API at hand.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use desk_core::args::parse;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

const DEFAULT_TIMEOUT_S: u64 = 90;
const MAX_TIMEOUT_S: u64 = 110;
const MAX_OUTPUT: usize = 32 << 10;
const PIPE_GRACE: Duration = Duration::from_millis(500);

type Captured = Arc<Mutex<Vec<u8>>>;

/// Runs modules on `node` with the runtime in `runtime`, calling back to `endpoint`, saving flows in `flows`, without the `key_vars`.
pub struct Runner {
    node: PathBuf,
    runtime: PathBuf,
    endpoint: String,
    flows: PathBuf,
    key_vars: Vec<String>,
    stop: tokio::sync::watch::Sender<bool>,
}

impl Runner {
    pub fn new(
        node: PathBuf,
        runtime: PathBuf,
        endpoint: String,
        flows: PathBuf,
        key_vars: Vec<String>,
    ) -> Self {
        Self {
            node,
            runtime,
            endpoint,
            flows,
            key_vars,
            stop: tokio::sync::watch::Sender::new(false),
        }
    }

    /// Stops the run in flight and every run after it, until `halt(false)`.
    pub fn halt(&self, stopped: bool) {
        self.stop.send_replace(stopped);
    }

    /// The runner of a release install: Node and the runtime next to `exe`, or in the app bundle's Resources.
    pub fn beside(exe: &Path, endpoint: String, flows: PathBuf, key_vars: Vec<String>) -> Self {
        let dir = exe.parent().unwrap_or(Path::new("."));
        let node = dir.join(if cfg!(windows) { "node.exe" } else { "node" });
        let runtime = [dir.join("runtime"), dir.join("../Resources/runtime")]
            .into_iter()
            .find(|p| p.is_dir())
            .unwrap_or_else(|| dir.join("runtime"));
        Self::new(node, runtime, endpoint, flows, key_vars)
    }

    /// `script_run` for `agent`: one module, `code` or the absolute file `script`; its `main(params)` value comes back as `result`.
    pub async fn script(&self, args: Value, agent: &str) -> Result<Value, String> {
        let a: ScriptArgs = parse(args)?;
        let scratch = scratch()?;
        let script = match (a.code, a.script) {
            (Some(code), None) => {
                let path = scratch.path().join("script.mts");
                write_private(&path, code.as_bytes())
                    .await
                    .map_err(|e| format!("write the script: {e}"))?;
                path
            }
            (None, Some(path)) if path.is_absolute() => path,
            (None, Some(path)) => {
                return Err(format!("script {} is not an absolute path", path.display()));
            }
            _ => return Err("pass code or script".to_owned()),
        };
        let params = a.params.unwrap_or_else(|| json!({}));
        self.run(&script, &params, a.timeout_s, scratch.path(), agent)
            .await
    }

    /// `gui_plan` for `agent`: the plan executor of the runtime with `args`; its result comes back as is.
    pub async fn plan(&self, args: Value, agent: &str) -> Result<Value, String> {
        let timeout_s = args.get("timeout_s").and_then(Value::as_u64);
        let scratch = scratch()?;
        let mut report = self
            .run(
                &self.runtime.join("gui_plan.mts"),
                &args,
                timeout_s,
                scratch.path(),
                agent,
            )
            .await?;
        match report["error"]["message"].as_str() {
            Some(e) => Err(format!("gui_plan: {e}")),
            None => Ok(report["result"].take()),
        }
    }

    async fn run(
        &self,
        script: &Path,
        params: &Value,
        timeout_s: Option<u64>,
        scratch: &Path,
        agent: &str,
    ) -> Result<Value, String> {
        write_private(&scratch.join("params.json"), params.to_string().as_bytes())
            .await
            .map_err(|e| format!("write params: {e}"))?;
        let timeout =
            Duration::from_secs(timeout_s.unwrap_or(DEFAULT_TIMEOUT_S).min(MAX_TIMEOUT_S));
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
        let mut cmd = Command::new(&self.node);
        cmd.arg(self.runtime.join("entry.mts"))
            .current_dir(home.map_or_else(|| scratch.to_path_buf(), PathBuf::from))
            .env("TOOLHOST_SOCKET", &self.endpoint)
            .env("SHUTTLE_SCRIPT", script)
            .env("SHUTTLE_RUN", scratch)
            .env("SHUTTLE_FLOWS", &self.flows)
            .env("SHUTTLE_AGENT", agent)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for var in &self.key_vars {
            cmd.env_remove(var);
        }
        #[cfg(unix)]
        cmd.process_group(0);
        let started = Instant::now();
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("start {}: {e}", self.node.display()))?;
        let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
            return Err("the script's pipes are missing".to_owned());
        };
        let (logs, stderr) = (Captured::default(), Captured::default());
        let mut pipes = [
            tokio::spawn(capture(out, Arc::clone(&logs))),
            tokio::spawn(capture(err, Arc::clone(&stderr))),
        ];
        let mut stopped = self.stop.subscribe();
        let status = tokio::select! {
            s = tokio::time::timeout(timeout, child.wait()) => s.map_err(|_| Ended::Timeout),
            _ = stopped.wait_for(|s| *s) => Err(Ended::Stopped),
        };
        if status.is_err() {
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                kill_group(pid);
            }
            let _ = child.kill().await;
        }
        let drained = async {
            for p in &mut pipes {
                let _ = p.await;
            }
        };
        let _ = tokio::time::timeout(PIPE_GRACE, drained).await;
        for p in &pipes {
            p.abort();
        }
        let mut report = json!({
            "duration_ms": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        });
        report["logs"] = text(&logs).into();
        report["stderr"] = text(&stderr).into();
        match status {
            Err(Ended::Stopped) => {
                report["error"] = json!({"kind": "stopped", "message": crate::STOPPED});
            }
            Err(Ended::Timeout) => {
                report["error"] = json!({"kind": "timeout", "message": format!("the run passed {}s and was stopped", timeout.as_secs())});
            }
            Ok(status) => {
                let status = status.map_err(|e| format!("wait for the script: {e}"))?;
                report["exit_code"] = status.code().into();
                if !status.success() {
                    let thrown = tokio::fs::read(scratch.join("error.json")).await;
                    report["error"] = thrown
                        .ok()
                        .and_then(|raw| serde_json::from_slice(&raw).ok())
                        .unwrap_or_else(|| json!({"kind": "exit", "message": format!("node exited with {status}")}));
                }
            }
        }
        report["result"] = match tokio::fs::read(scratch.join("result.json")).await {
            Ok(raw) => serde_json::from_slice(&raw)
                .unwrap_or_else(|_| String::from_utf8_lossy(&raw).into_owned().into()),
            Err(_) => Value::Null,
        };
        Ok(report)
    }
}

#[derive(Deserialize)]
struct ScriptArgs {
    code: Option<String>,
    script: Option<PathBuf>,
    params: Option<Value>,
    timeout_s: Option<u64>,
}

enum Ended {
    Timeout,
    Stopped,
}

#[cfg(unix)]
pub(crate) fn kill_group(pid: u32) {
    if let Ok(pid) = libc::pid_t::try_from(pid) {
        // SAFETY: kill takes no pointers; the negative pid names the process group the child leads.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
}

fn scratch() -> Result<tempfile::TempDir, String> {
    #[cfg(unix)]
    let made = tempfile::Builder::new()
        .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .tempdir();
    #[cfg(not(unix))]
    let made = tempfile::tempdir();
    made.map_err(|e| format!("create a scratch directory: {e}"))
}

async fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut open = tokio::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    open.mode(0o600);
    open.open(path).await?.write_all(bytes).await
}

async fn capture(mut r: impl AsyncRead + Unpin, into: Captured) {
    let mut chunk = [0u8; 8192];
    while let Ok(n) = r.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        let mut buf = into.lock().unwrap_or_else(PoisonError::into_inner);
        let room = MAX_OUTPUT.saturating_sub(buf.len());
        buf.extend_from_slice(&chunk[..n.min(room)]);
    }
}

fn text(captured: &Captured) -> String {
    let bytes = std::mem::take(&mut *captured.lock().unwrap_or_else(PoisonError::into_inner));
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner(dir: &Path) -> Runner {
        let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/lib/entry.mts");
        std::fs::copy(lib, dir.join("entry.mts")).unwrap();
        std::fs::write(dir.join("desk.mts"), "export const here = true\n").unwrap();
        std::fs::write(
            dir.join("gui_plan.mts"),
            "export async function main(p) { return { ok: true, steps: p.steps.length, socket: process.env.TOOLHOST_SOCKET, home: process.env.HOME === undefined && process.env.CARGO_PKG_NAME === undefined } }\n",
        )
        .unwrap();
        Runner::new(
            "node".into(),
            dir.to_path_buf(),
            "/tmp/desk.sock".into(),
            dir.join("flows"),
            vec!["HOME".into(), "CARGO_PKG_NAME".into()],
        )
    }

    #[tokio::test]
    async fn a_script_main_returns_its_value_with_the_logs_and_the_desk_api_at_hand() {
        let dir = tempfile::tempdir().unwrap();
        let r = runner(dir.path());
        let code = "console.log('hi')\nexport function main({ n = 0 }) { return { n: n + 1, desk: globalThis.desk.here } }";
        let report = r
            .script(json!({"code": code, "params": {"n": 1}}), "run-test")
            .await
            .unwrap();
        assert_eq!(report["result"], json!({"n": 2, "desk": true}), "{report}");
        assert_eq!(report["logs"], "hi\n");
        assert_eq!(report["exit_code"], 0);
        assert!(report.get("error").is_none());
        let defaulted = r.script(json!({"code": code}), "run-test").await.unwrap();
        assert_eq!(defaulted["result"]["n"], 1, "{defaulted}");
    }

    #[tokio::test]
    async fn a_throwing_script_reports_its_error_and_a_slow_one_the_timeout_with_its_logs() {
        let dir = tempfile::tempdir().unwrap();
        let r = runner(dir.path());
        let thrown = r
            .script(
                json!({"code": "export function main() { throw new TypeError('nope') }"}),
                "run-test",
            )
            .await
            .unwrap();
        assert_eq!(thrown["error"]["kind"], "exception");
        let stack = thrown["error"]["message"].as_str().unwrap();
        assert!(stack.starts_with("TypeError: nope\n    at main"), "{stack}");
        let no_main = r
            .script(json!({"code": "export const x = 1"}), "run-test")
            .await
            .unwrap();
        assert_eq!(no_main["error"]["kind"], "no_main", "{no_main}");
        let broken = r
            .script(json!({"code": "export function main( {"}), "run-test")
            .await
            .unwrap();
        assert_eq!(broken["error"]["kind"], "import", "{broken}");
        assert!(broken["result"].is_null());
        let slow = r.script(json!({"code": "console.log('started')\nexport async function main() { await new Promise((r) => setTimeout(r, 5000)) }", "timeout_s": 1}), "run-test")
            .await
            .unwrap();
        assert_eq!(slow["error"]["kind"], "timeout");
        assert_eq!(slow["logs"], "started\n");
        let relative = r
            .script(json!({"script": "x.mts"}), "run-test")
            .await
            .unwrap_err();
        assert_eq!(relative, "script x.mts is not an absolute path");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_timeout_stops_the_whole_process_group_and_keeps_the_logs_a_stray_pipe_holder_blocks()
    {
        let dir = tempfile::tempdir().unwrap();
        let r = runner(dir.path());
        let code = "import { spawn } from 'node:child_process'\nconst same = spawn('sleep', ['30'], { stdio: 'inherit' })\nconst away = spawn('sleep', ['30'], { stdio: 'inherit', detached: true })\nconsole.log(`${same.pid} ${away.pid}`)\nexport async function main() { await new Promise((r) => setTimeout(r, 5000)) }";
        let report = r
            .script(json!({"code": code, "timeout_s": 1}), "run-test")
            .await
            .unwrap();
        assert_eq!(report["error"]["kind"], "timeout", "{report}");
        let logs = report["logs"].as_str().unwrap().trim().to_owned();
        let (same, away) = logs.split_once(' ').unwrap();
        let alive = |pid: &str| {
            std::process::Command::new("kill")
                .args(["-0", pid])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        let mut gone = false;
        for _ in 0..40 {
            gone = !alive(same);
            if gone {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let stray = alive(away);
        let _ = std::process::Command::new("kill")
            .args(["-9", away])
            .status();
        assert!(gone, "{same} outlived the timeout");
        assert!(
            stray,
            "the detached pipe holder is gone, so the test proves nothing"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_scratch_directory_and_the_params_are_the_users_alone() {
        let dir = tempfile::tempdir().unwrap();
        let r = runner(dir.path());
        let code = "import { statSync } from 'node:fs'\nexport function main() { const run = process.env.SHUTTLE_RUN; return [statSync(run).mode & 0o777, statSync(`${run}/params.json`).mode & 0o777, statSync(`${run}/script.mts`).mode & 0o777] }";
        let report = r.script(json!({"code": code}), "run-test").await.unwrap();
        let modes: Vec<u32> = serde_json::from_value(report["result"].clone()).unwrap();
        assert_eq!(modes, [0o700, 0o600, 0o600], "{report}");
    }

    #[tokio::test]
    async fn a_plan_runs_the_runtime_plan_executor_with_the_daemon_socket_and_no_hidden_variables()
    {
        let dir = tempfile::tempdir().unwrap();
        let r = runner(dir.path());
        let out = r
            .plan(
                json!({"steps": [{"app": "TextEdit"}, {"key": "cmd+n"}]}),
                "run-test",
            )
            .await
            .unwrap();
        assert_eq!(
            out,
            json!({"ok": true, "steps": 2, "socket": "/tmp/desk.sock", "home": true})
        );
    }
}
