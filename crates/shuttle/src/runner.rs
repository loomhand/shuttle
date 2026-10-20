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
#[cfg(unix)]
const END_GRACE: Duration = Duration::from_secs(2);

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
        self.script_on(&self.node, args, agent).await
    }

    /// A plugin tool's run for `agent`: `script_run`'s contract with the plugin's program `bin` in place of Node.
    pub async fn plugin(&self, bin: &Path, args: Value, agent: &str) -> Result<Value, String> {
        self.script_on(bin, args, agent).await
    }

    async fn script_on(&self, program: &Path, args: Value, agent: &str) -> Result<Value, String> {
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
        self.run(
            program,
            &script,
            &params,
            a.timeout_s,
            scratch.path(),
            agent,
        )
        .await
    }

    /// `gui_plan` for `agent`: the plan executor of the runtime with `args`; its result comes back as is.
    pub async fn plan(&self, args: Value, agent: &str) -> Result<Value, String> {
        let timeout_s = args.get("timeout_s").and_then(Value::as_u64);
        let scratch = scratch()?;
        let mut report = self
            .run(
                &self.node,
                &self.module("gui_plan"),
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
        program: &Path,
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
        let mut cmd = Command::new(program);
        cmd.arg(self.module("entry"))
            .current_dir(home.map_or_else(|| scratch.to_path_buf(), PathBuf::from))
            .env("SHUTTLE_SOCKET", &self.endpoint)
            .env("SHUTTLE_SCRIPT", script)
            .env("SHUTTLE_RUN", scratch)
            .env("SHUTTLE_FLOWS", &self.flows)
            .env("SHUTTLE_AGENT", agent)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(cfg!(windows));
        for var in &self.key_vars {
            cmd.env_remove(var);
        }
        #[cfg(unix)]
        cmd.process_group(0);
        let started = Instant::now();
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("start {}: {e}", program.display()))?;
        #[cfg(unix)]
        let mut group = Group {
            pid: child.id(),
            termed: false,
        };
        #[cfg(windows)]
        let job = crate::job::Job::hold(child.raw_handle())?;
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
                group.termed = true;
                let _ = tokio::join!(crate::end_group(pid, END_GRACE), child.wait());
            }
            #[cfg(windows)]
            job.kill();
            let _ = child.kill().await;
        } else {
            #[cfg(windows)]
            job.release();
        }
        #[cfg(unix)]
        {
            group.pid = None;
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
                        .unwrap_or_else(|| json!({"kind": "exit", "message": format!("{} exited with {status}", program.file_name().unwrap_or_default().to_string_lossy())}));
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

    fn module(&self, name: &str) -> PathBuf {
        let ext = if self.runtime.join("entry.mjs").is_file() {
            "mjs"
        } else {
            "mts"
        };
        self.runtime.join(format!("{name}.{ext}"))
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

/// A run's process group, ended when the run is dropped before it ends, as when its client closes the connection.
#[cfg(unix)]
struct Group {
    pid: Option<u32>,
    termed: bool,
}

#[cfg(unix)]
impl Drop for Group {
    fn drop(&mut self) {
        let Some(pid) = self.pid else {
            return;
        };
        if !self.termed {
            crate::signal_group(pid, libc::SIGTERM);
        }
        match tokio::runtime::Handle::try_current() {
            Ok(rt) if !self.termed => {
                crate::kill_later(&rt, pid, END_GRACE);
            }
            _ => {
                crate::signal_group(pid, libc::SIGKILL);
            }
        }
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
            "export async function main(p) { return { ok: true, steps: p.steps.length, socket: process.env.SHUTTLE_SOCKET, home: process.env.HOME === undefined && process.env.CARGO_PKG_NAME === undefined } }\n",
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
    fn alive(pid: &str) -> bool {
        let out = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", pid.trim()])
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&out.stdout);
        !stat.trim().is_empty() && !stat.trim_start().starts_with('Z')
    }

    #[cfg(unix)]
    const ON_TERM: &str = "import { writeFileSync } from 'node:fs'\nexport async function main({ marker }) { process.on('SIGTERM', () => { writeFileSync(marker, 'term'); process.exit(0) }); writeFileSync(`${marker}.ready`, ''); await new Promise((r) => setTimeout(r, 30000)) }";

    #[cfg(unix)]
    #[tokio::test]
    async fn a_timeout_or_a_stop_sends_term_first_so_the_scripts_handler_runs() {
        let dir = tempfile::tempdir().unwrap();
        let r = runner(dir.path());
        let marker = dir.path().join("timeout");
        let report = r
            .script(
                json!({"code": ON_TERM, "params": {"marker": marker}, "timeout_s": 2}),
                "run-test",
            )
            .await
            .unwrap();
        assert_eq!(report["error"]["kind"], "timeout", "{report}");
        assert!(
            dir.path().join("timeout.ready").exists(),
            "the script was not ready before its timeout: {report}"
        );
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "term");
        let marker = dir.path().join("stop");
        let ready = dir.path().join("stop.ready");
        let args = json!({"code": ON_TERM, "params": {"marker": marker}, "timeout_s": 30});
        let (report, ()) = tokio::join!(r.script(args, "run-test"), async {
            tokio::time::timeout(Duration::from_secs(10), async {
                while !ready.exists() {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("the script never got ready");
            r.halt(true);
        });
        r.halt(false);
        let report = report.unwrap();
        assert_eq!(report["error"]["kind"], "stopped", "{report}");
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "term");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_run_that_ignores_term_is_killed_once_the_grace_has_passed() {
        let dir = tempfile::tempdir().unwrap();
        let r = runner(dir.path());
        let code = "process.on('SIGTERM', () => {})\nconsole.log(process.pid)\nexport async function main() { await new Promise((r) => setTimeout(r, 30000)) }";
        let started = Instant::now();
        let report = r
            .script(json!({"code": code, "timeout_s": 2}), "run-test")
            .await
            .unwrap();
        let took = started.elapsed();
        assert_eq!(report["error"]["kind"], "timeout", "{report}");
        let limit = Duration::from_secs(2) + END_GRACE;
        assert!(
            took >= limit - Duration::from_millis(100)
                && took < limit + PIPE_GRACE + Duration::from_millis(500),
            "{took:?}"
        );
        let pid = report["logs"].as_str().unwrap().trim().to_owned();
        assert!(!alive(&pid), "{pid} outlived the grace");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_run_dropped_during_its_grace_still_kills_a_script_that_ignores_term() {
        let dir = tempfile::tempdir().unwrap();
        let r = runner(dir.path());
        let (pid_file, termed) = (dir.path().join("pid"), dir.path().join("termed"));
        let code = "import { writeFileSync } from 'node:fs'\nexport async function main({ pid, termed }) { process.on('SIGTERM', () => writeFileSync(termed, '')); writeFileSync(pid, String(process.pid)); await new Promise((r) => setTimeout(r, 30000)) }";
        let args =
            json!({"code": code, "params": {"pid": pid_file, "termed": termed}, "timeout_s": 2});
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::select! {
                report = r.script(args, "run-test") => panic!("the run ended: {report:?}"),
                () = async {
                    while !termed.exists() {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                } => {}
            }
        })
        .await
        .expect("the run got no TERM");
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        let mut gone = false;
        for _ in 0..20 {
            gone = !alive(&pid);
            if gone {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if !gone {
            let _ = std::process::Command::new("kill")
                .args(["-9", pid.trim()])
                .status();
        }
        assert!(gone, "{pid} outlived the run dropped during its grace");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_run_whose_client_goes_away_gets_term_first() {
        let dir = tempfile::tempdir().unwrap();
        let r = runner(dir.path());
        let marker = dir.path().join("dropped");
        let ready = dir.path().join("dropped.ready");
        let args = json!({"code": ON_TERM, "params": {"marker": marker}, "timeout_s": 30});
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::select! {
                report = r.script(args, "run-test") => panic!("the run ended: {report:?}"),
                () = async {
                    while !ready.exists() {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                } => {}
            }
        })
        .await
        .expect("the script never got ready");
        let mut termed = false;
        for _ in 0..40 {
            termed = std::fs::read_to_string(&marker).is_ok_and(|m| m == "term");
            if termed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(termed, "the dropped run got no TERM");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "needs a writable cgroupfs"]
    async fn a_timeout_spares_an_app_in_a_cgroup_of_its_own() {
        let own = std::fs::read_to_string("/proc/self/cgroup").unwrap();
        let own = own.trim().strip_prefix("0::").expect("cgroup v2");
        let scope = Path::new("/sys/fs/cgroup")
            .join(own.trim_start_matches('/'))
            .join(format!("shuttle-test-{}", std::process::id()));
        std::fs::create_dir(&scope).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let r = runner(dir.path());
        let pids = dir.path().join("pids");
        let code = "import { spawn } from 'node:child_process'\nimport { writeFileSync } from 'node:fs'\nexport async function main({ pids }) { const app = spawn('sleep', ['30'], { stdio: 'ignore' }); const helper = spawn('sleep', ['30'], { stdio: 'ignore' }); writeFileSync(pids, `${app.pid} ${helper.pid}`); await new Promise((r) => setTimeout(r, 30000)) }";
        let args = json!({"code": code, "params": {"pids": pids}, "timeout_s": 2});
        let (report, moved) = tokio::join!(r.script(args, "run-test"), async {
            for _ in 0..150 {
                if let Some((app, helper)) = std::fs::read_to_string(&pids)
                    .ok()
                    .and_then(|p| p.split_once(' ').map(|(a, h)| (a.to_owned(), h.to_owned())))
                {
                    return std::fs::write(scope.join("cgroup.procs"), &app)
                        .map(|()| (app, helper))
                        .map_err(|e| e.to_string());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err("the script wrote no pids".to_owned())
        });
        let report = report.unwrap();
        let mut checked = Err(String::new());
        if let Ok((app, helper)) = &moved {
            let mut gone = false;
            for _ in 0..40 {
                gone = !alive(helper);
                if gone {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            checked = Ok((alive(app), gone));
            let _ = std::process::Command::new("kill")
                .args(["-9", app])
                .status();
        }
        for _ in 0..40 {
            if std::fs::remove_dir(&scope).is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let (kept, gone) = moved.and(checked).unwrap();
        assert_eq!(report["error"]["kind"], "timeout", "{report}");
        assert!(gone, "the helper in the run's cgroup outlived the timeout");
        assert!(kept, "the app in its own cgroup died with the run");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_timeout_kills_the_run_with_the_process_it_started_first_and_a_finished_run_leaves_its_processes()
     {
        let dir = tempfile::tempdir().unwrap();
        let r = runner(dir.path());
        let alive = |pid: &str| {
            let out = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).contains(pid)
        };
        let started = "import { spawn } from 'node:child_process'\nconst ping = spawn('ping', ['-n', '60', '127.0.0.1'], { stdio: 'ignore', detached: true })\nconsole.log(ping.pid)\n";
        let hung = format!(
            "{started}export async function main() {{ await new Promise((r) => setTimeout(r, 5000)) }}"
        );
        let report = r
            .script(json!({"code": hung, "timeout_s": 2}), "run-test")
            .await
            .unwrap();
        assert_eq!(report["error"]["kind"], "timeout", "{report}");
        let pid = report["logs"].as_str().unwrap().trim().to_owned();
        assert!(
            !pid.is_empty(),
            "the script printed no pid before its timeout: {report}"
        );
        let mut gone = false;
        for _ in 0..40 {
            gone = !alive(&pid);
            if gone {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(gone, "{pid} outlived the timeout");
        let done = format!("{started}export function main() {{ return 1 }}");
        let report = r.script(json!({"code": done}), "run-test").await.unwrap();
        assert_eq!(report["result"], 1, "{report}");
        let pid = report["logs"].as_str().unwrap().trim().to_owned();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let kept = alive(&pid);
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid])
            .output();
        assert!(kept, "{pid} died with a run that finished");
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
    async fn a_packaged_runtime_runs_its_stripped_modules_and_keeps_the_sources_importable() {
        let dir = tempfile::tempdir().unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for e in std::fs::read_dir(root.join("runtime/lib")).unwrap() {
            let e = e.unwrap();
            std::fs::copy(e.path(), dir.path().join(e.file_name())).unwrap();
        }
        let built = std::process::Command::new("node")
            .arg(root.join("packaging/runtime.mjs"))
            .arg(dir.path())
            .arg(dir.path())
            .status()
            .unwrap();
        assert!(built.success());
        let mut names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "desk.mjs",
                "desk.mts",
                "entry.mjs",
                "entry.mts",
                "gui_plan.mjs",
                "gui_plan.mts",
                "png.mjs",
                "png.mts"
            ]
        );
        let r = Runner::new(
            "node".into(),
            dir.path().to_path_buf(),
            "unused".into(),
            dir.path().join("flows"),
            Vec::new(),
        );
        let code = "import { basename } from 'node:path'\nimport { pathToFileURL } from 'node:url'\nexport async function main(p: { n: number }): Promise<unknown[]> {\n  const own = await import(pathToFileURL(process.argv[1]!.replace(/entry\\.mjs$/, 'desk.mts')).href)\n  return [p.n, basename(process.argv[1]!), typeof globalThis.desk.windows, typeof own.windows]\n}";
        let report = r
            .script(json!({"code": code, "params": {"n": 1}}), "run-test")
            .await
            .unwrap();
        assert_eq!(
            report["result"],
            json!([1, "entry.mjs", "function", "function"]),
            "{report}"
        );
        let plan = r.plan(json!({"steps": []}), "run-test").await.unwrap();
        assert_eq!(plan["error"], "pass steps, flow or find", "{plan}");
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
