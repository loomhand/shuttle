//! Service plugins: the daemon runs each as its child, starts it again when it exits, and splices the connections of its tools' calls to its socket.

use std::collections::HashMap;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use shuttle_desk::server;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use zeroize::{Zeroize, Zeroizing};

use crate::plugins::Service;
use crate::{Running, log};

const SOCKET_VAR: &str = "SHUTTLE_SERVICE_SOCKET";
#[cfg(unix)]
const DIR: &str = "services";
const FIRST_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(30);
const STOP_WAIT: Duration = Duration::from_secs(3);
const LISTEN_WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(50);
const SPLICE_BUF: usize = 64 << 10;

/// The services this daemon runs.
pub struct Services {
    stop: Vec<oneshot::Sender<()>>,
    tasks: JoinSet<()>,
}

impl Services {
    /// Starts each of `services` with the socket beside the desk endpoint `desk` in its environment, and without the `key_vars`.
    pub fn start(services: &[Service], desk: &str, key_vars: &[String]) -> Result<Self, String> {
        let mut running = Self {
            stop: Vec::new(),
            tasks: JoinSet::new(),
        };
        if services.is_empty() {
            return Ok(running);
        }
        #[cfg(unix)]
        private_dir(&dir(desk))?;
        for s in services {
            let proc = Proc {
                name: s.service.clone(),
                bin: s.bin.clone(),
                args: s.args.clone(),
                socket: socket(desk, &s.service),
                desk: desk.to_owned(),
                key_vars: key_vars.to_vec(),
            };
            let (stop, stopped) = oneshot::channel();
            running.stop.push(stop);
            running.tasks.spawn(proc.supervise(stopped));
        }
        Ok(running)
    }

    /// Asks every service to exit and waits for them, killing those that do not.
    pub async fn stop(mut self) {
        for s in self.stop.drain(..) {
            let _ = s.send(());
        }
        while self.tasks.join_next().await.is_some() {}
    }
}

// only the used bytes are zeroized, so a short call does not wipe the whole buffer
struct Buf {
    bytes: Vec<u8>,
    used: usize,
}

impl Drop for Buf {
    fn drop(&mut self) {
        self.bytes[..self.used].zeroize();
    }
}

struct Proc {
    name: String,
    bin: PathBuf,
    args: Vec<String>,
    socket: String,
    desk: String,
    key_vars: Vec<String>,
}

impl Proc {
    fn spawn(&self) -> Result<Running, String> {
        #[cfg(unix)]
        if let Err(e) = std::fs::remove_file(&self.socket)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log(format_args!(
                "service {}: remove {}: {e}",
                self.name, self.socket
            ));
        }
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map_or_else(|| PathBuf::from("/"), PathBuf::from);
        let mut cmd = Command::new(&self.bin);
        cmd.args(&self.args)
            .current_dir(home)
            .env(SOCKET_VAR, &self.socket)
            .env("SHUTTLE_SOCKET", &self.desk)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        for var in &self.key_vars {
            cmd.env_remove(var);
        }
        #[cfg(unix)]
        cmd.process_group(0);
        let running = cmd
            .spawn()
            .map_err(|e| format!("start {}: {e}", self.bin.display()))
            .and_then(Running::jobbed)?;
        #[cfg(windows)]
        if let Some(job) = &running.job {
            servers().insert(self.socket.clone(), std::sync::Arc::downgrade(job));
        }
        Ok(running)
    }

    async fn supervise(self, mut stopped: oneshot::Receiver<()>) {
        let mut delay = FIRST_DELAY;
        loop {
            let started = Instant::now();
            match self.spawn() {
                Ok(mut running) => {
                    tokio::select! {
                        _ = &mut stopped => {
                            terminate(&mut running).await;
                            return;
                        }
                        status = running.child.wait() => log(format_args!("service {} exited with {status:?}; starting it again", self.name)),
                    }
                }
                Err(e) => log(format_args!("service {}: {e}", self.name)),
            }
            if started.elapsed() >= MAX_DELAY {
                delay = FIRST_DELAY;
            }
            tokio::select! {
                _ = &mut stopped => return,
                () = tokio::time::sleep(delay) => {}
            }
            delay = (delay * 2).min(MAX_DELAY);
        }
    }
}

/// The socket of each tool of `services`, by tool name, for the daemon at `desk`.
pub fn routes(services: &[Service], desk: &str) -> HashMap<String, String> {
    services
        .iter()
        .flat_map(|s| {
            let socket = socket(desk, &s.service);
            s.tools
                .iter()
                .map(move |t| (t.name.clone(), socket.clone()))
        })
        .collect()
}

/// Connects to the service at `socket`, waiting while it starts, writes the frame `body` and splices both ways until the service is done; an error comes back only while nothing has reached the service.
pub async fn route<C>(socket: &str, body: Zeroizing<Vec<u8>>, client: &mut C) -> Result<(), String>
where
    C: AsyncRead + AsyncWrite + Unpin,
{
    let mut service = listening(socket).await?;
    #[cfg(windows)]
    serves(socket, &service)?;
    let len = u32::try_from(body.len()).map_err(|_| "frame too large".to_owned())?;
    let sent = async {
        service.write_all(&len.to_be_bytes()).await?;
        service.write_all(&body).await
    };
    sent.await
        .map_err(|e| format!("write to the service at {socket}: {e}"))?;
    drop(body);
    splice(client, service).await;
    Ok(())
}

// the service gets TERM first, so it can close the apps it started
async fn terminate(running: &mut Running) {
    #[cfg(unix)]
    if let Some(pid) = running.child.id() {
        crate::term(pid);
    }
    #[cfg(windows)]
    match &running.job {
        Some(job) => job.kill(),
        None => {
            let _ = running.child.start_kill();
        }
    }
    if tokio::time::timeout(STOP_WAIT, running.child.wait())
        .await
        .is_err()
    {
        let _ = running.child.kill().await;
    }
}

async fn listening(socket: &str) -> Result<server::Conn, String> {
    let deadline = tokio::time::Instant::now() + LISTEN_WAIT;
    loop {
        let e = match tokio::time::timeout_at(deadline, server::connect(socket)).await {
            Ok(Ok(conn)) => return Ok(conn),
            Ok(Err(e)) => e.to_string(),
            Err(_) => "the pipe stays busy".to_owned(),
        };
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("no service listens at {socket}: {e}"));
        }
        tokio::time::sleep(POLL).await;
    }
}

// an end of the data on either side closes only the other's input, or both on Windows where a pipe has no half-close; the first failure ends both
async fn splice<C, S>(client: C, service: S)
where
    C: AsyncRead + AsyncWrite + Unpin,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (client_r, client_w) = tokio::io::split(client);
    let (service_r, service_w) = tokio::io::split(service);
    #[cfg(unix)]
    let _ = tokio::try_join!(pump(client_r, service_w), pump(service_r, client_w));
    #[cfg(windows)]
    tokio::select! {
        _ = pump(client_r, service_w) => {}
        _ = pump(service_r, client_w) => {}
    }
}

async fn pump<R, W>(mut r: R, mut w: W) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = Buf {
        bytes: vec![0u8; SPLICE_BUF],
        used: 0,
    };
    loop {
        let n = r.read(&mut buf.bytes).await?;
        if n == 0 {
            return w.shutdown().await;
        }
        buf.used = buf.used.max(n);
        w.write_all(&buf.bytes[..n]).await?;
    }
}

#[cfg(unix)]
fn dir(desk: &str) -> PathBuf {
    Path::new(desk).with_file_name(DIR)
}

#[cfg(unix)]
fn socket(desk: &str, service: &str) -> String {
    dir(desk)
        .join(format!("{service}.sock"))
        .to_string_lossy()
        .into_owned()
}

#[cfg(windows)]
fn socket(desk: &str, service: &str) -> String {
    format!("{desk}-svc-{service}")
}

/// The job of the service last started on each pipe, which alone may serve it.
#[cfg(windows)]
fn servers() -> std::sync::MutexGuard<'static, HashMap<String, std::sync::Weak<crate::job::Job>>> {
    static SERVERS: std::sync::LazyLock<
        std::sync::Mutex<HashMap<String, std::sync::Weak<crate::job::Job>>>,
    > = std::sync::LazyLock::new(Default::default);
    SERVERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// anyone can create a pipe of any free name, so a call goes only to a server inside the service's job
#[cfg(windows)]
fn serves(socket: &str, service: &server::Conn) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;

    let job = servers()
        .get(socket)
        .and_then(std::sync::Weak::upgrade)
        .ok_or_else(|| format!("no service runs for {socket}"))?;
    job.serves(windows::Win32::Foundation::HANDLE(service.as_raw_handle()))
        .map_err(|e| format!("{socket}: {e}"))
}

#[cfg(unix)]
fn private_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("chmod {}: {e}", dir.display()))
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    use desk_core::wire::{Call, Output, read_body, read_frame, write_frame};
    use serde_json::{Value, json};
    use shuttle_desk::server::{Conn, Handler, NoDesk};
    use shuttle_models::Models;
    use shuttle_models::config::Config;

    use super::*;
    use crate::daemon::{Daemon, Guard};
    use crate::mcp;
    use crate::plugins::Plugins;
    use crate::runner::Runner;

    const FAKE: &str = r#"#!/usr/bin/env python3
import json, os, socket, socketserver, struct, sys
state = sys.argv[1]

def exact(f, n):
    b = f.read(n)
    if len(b) < n:
        raise EOFError
    return b

def frame(f):
    return exact(f, struct.unpack(">I", exact(f, 4))[0])

def send(f, data):
    f.write(struct.pack(">I", len(data)) + data)
    f.flush()

def reply(f, text):
    send(f, json.dumps({"text": text, "is_error": False}).encode())

def mark(name, text=""):
    with open(os.path.join(state, name + ".tmp"), "w") as f:
        f.write(text)
    os.rename(os.path.join(state, name + ".tmp"), os.path.join(state, name))

class Conn(socketserver.StreamRequestHandler):
    def handle(self):
        raw = frame(self.rfile)
        call = json.loads(raw)
        tool = call["tool"]
        if tool == "demo_echo":
            with open(os.path.join(state, "frame"), "wb") as f:
                f.write(raw)
            env = {k: os.environ.get(k) for k in ("SHUTTLE_SOCKET", "CARGO_PKG_NAME")}
            reply(self.wfile, json.dumps({"agent_id": call["agent_id"], "args": call["args"], "env": env}))
        elif tool == "demo_stream":
            reply(self.wfile, "started")
            while True:
                head = self.rfile.read(5)
                if len(head) < 5:
                    mark("cancelled")
                    return
                n = struct.unpack(">I", head[1:])[0]
                data = exact(self.rfile, n)
                if head[0] == 5:
                    self.wfile.write(bytes([3]) + struct.pack(">I", 4) + struct.pack(">i", 0))
                    return
                self.wfile.write(bytes([1]) + struct.pack(">I", n) + data.upper())
        elif tool == "demo_blob":
            chunks = []
            while True:
                n = struct.unpack(">I", exact(self.rfile, 4))[0]
                if n == 0:
                    break
                chunks.append(exact(self.rfile, n))
            for d in chunks:
                send(self.wfile, d)
            send(self.wfile, b"")
            reply(self.wfile, str(sum(len(d) for d in chunks)))
        elif tool == "demo_early":
            reply(self.wfile, "early")
            self.request.shutdown(socket.SHUT_WR)
            total = 0
            while True:
                n = struct.unpack(">I", exact(self.rfile, 4))[0]
                if n == 0:
                    break
                total += len(exact(self.rfile, n))
            mark("received", str(total))
        elif tool == "demo_hold":
            mark("held")
            self.rfile.read(1)
            mark("dropped")

sock = os.environ["SHUTTLE_SERVICE_SOCKET"]
if os.path.exists(sock) and not os.path.exists(os.path.join(state, "keep-socket")):
    os.remove(sock)
server = socketserver.ThreadingUnixStreamServer(sock, Conn)
server.daemon_threads = True
mark("pid", str(os.getpid()))
server.serve_forever()
"#;

    struct Served {
        dir: tempfile::TempDir,
        endpoint: String,
        daemon: Arc<Daemon<NoDesk>>,
        services: Option<Services>,
        plugins: Plugins,
    }

    impl Served {
        fn state(&self, name: &str) -> PathBuf {
            self.dir.path().join("state").join(name)
        }

        fn pid(&self) -> Option<i32> {
            std::fs::read_to_string(self.state("pid"))
                .ok()?
                .parse()
                .ok()
        }

        async fn open(&self, tool: &str, args: Value) -> Conn {
            let mut conn = server::connect(&self.endpoint).await.unwrap();
            let call = Call {
                agent_id: "agent-1".into(),
                tool: tool.into(),
                args,
            };
            write_frame(&mut conn, &call).await.unwrap();
            conn
        }

        async fn echo(&self) -> Output {
            read_frame(&mut self.open("demo_echo", json!({"say": "hi"})).await)
                .await
                .unwrap()
        }

        async fn stop(mut self) {
            if let Some(s) = self.services.take() {
                s.stop().await;
            }
        }
    }

    async fn served(guard: Guard) -> Served {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let bin = dir.path().join("demo-service");
        crate::script(&bin, FAKE).unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let tool = |name: &str, mcp: bool| json!({"name": name, "description": name, "input_schema": {"type": "object", "properties": {"say": {"type": "string"}}}, "mcp": mcp});
        let plugins = Plugins {
            runs: Vec::new(),
            services: vec![
                serde_json::from_value(json!({
                    "service": "demo",
                    "bin": bin,
                    "args": [state],
                    "tools": [tool("demo_echo", true), tool("demo_stream", false), tool("demo_blob", false), tool("demo_hold", false), tool("demo_early", false)],
                }))
                .unwrap(),
            ],
        };
        let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/lib");
        let runner = Runner::new(
            "node".into(),
            runtime,
            endpoint.clone(),
            dir.path().join("flows"),
            Vec::new(),
        );
        let daemon = Arc::new(
            Daemon::new(NoDesk, Ok(Models::new(Config::default())), runner, guard)
                .with_services(routes(&plugins.services, &endpoint)),
        );
        let listener = server::bind(&endpoint).await.unwrap();
        let services =
            Services::start(&plugins.services, &endpoint, &["CARGO_PKG_NAME".to_owned()]).unwrap();
        tokio::spawn(server::serve(Arc::clone(&daemon), listener));
        let served = Served {
            dir,
            endpoint,
            daemon,
            services: Some(services),
            plugins,
        };
        for _ in 0..100 {
            if served.pid().is_some() {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        served
    }

    fn control(tool: &str) -> Call {
        Call {
            agent_id: String::new(),
            tool: tool.into(),
            args: json!({}),
        }
    }

    fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    async fn appears(path: &Path) -> bool {
        for _ in 0..100 {
            if path.exists() {
                return true;
            }
            tokio::time::sleep(POLL).await;
        }
        false
    }

    #[tokio::test]
    async fn a_call_reaches_the_service_byte_for_byte_and_the_service_gets_its_environment() {
        let s = served(Guard::default()).await;
        let mut conn = server::connect(&s.endpoint).await.unwrap();
        let raw = br#"{"tool":"demo_echo",  "args":{"z":1,"a":[1, 2]},"agent_id":"agent-7"}"#;
        conn.write_all(&u32::try_from(raw.len()).unwrap().to_be_bytes())
            .await
            .unwrap();
        conn.write_all(raw).await.unwrap();
        let out: Output = read_frame(&mut conn).await.unwrap();
        let got: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(std::fs::read(s.state("frame")).unwrap(), raw);
        assert_eq!(got["agent_id"], "agent-7");
        assert_eq!(got["env"]["SHUTTLE_SOCKET"], s.endpoint.as_str());
        assert_eq!(got["env"]["CARGO_PKG_NAME"], Value::Null);
        let dir = std::fs::metadata(s.dir.path().join(DIR)).unwrap();
        assert_eq!(dir.permissions().mode() & 0o777, 0o700);
        s.stop().await;
    }

    #[tokio::test]
    async fn tagged_frames_stream_both_ways_and_a_client_hangup_reaches_the_service() {
        let s = served(Guard::default()).await;
        let mut conn = s.open("demo_stream", json!({})).await;
        let started: Output = read_frame(&mut conn).await.unwrap();
        assert_eq!(started.text, "started");
        for word in [&b"one"[..], b"two"] {
            conn.write_all(&[4, 0, 0, 0, 3]).await.unwrap();
            conn.write_all(word).await.unwrap();
            let mut got = [0u8; 8];
            conn.read_exact(&mut got).await.unwrap();
            assert_eq!(got[..5], [1, 0, 0, 0, 3]);
            assert_eq!(got[5..], word.to_ascii_uppercase());
        }
        conn.write_all(&[5, 0, 0, 0, 0]).await.unwrap();
        let mut exit = Vec::new();
        conn.read_to_end(&mut exit).await.unwrap();
        assert_eq!(exit, [3, 0, 0, 0, 4, 0, 0, 0, 0]);
        let mut hung = s.open("demo_stream", json!({})).await;
        let _: Output = read_frame(&mut hung).await.unwrap();
        drop(hung);
        assert!(appears(&s.state("cancelled")).await);
        s.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_64_mib_raw_transfer_passes_both_ways_unchanged() {
        let s = served(Guard::default()).await;
        let mut conn = s.open("demo_blob", json!({})).await;
        let block: Vec<u8> = (0..1u32 << 20).map(|i| (i % 251) as u8).collect();
        let (mut r, mut w) = conn.split();
        let send = async {
            for i in 0..64u8 {
                let mut b = block.clone();
                b[0] = i;
                w.write_all(&(1u32 << 20).to_be_bytes()).await.unwrap();
                w.write_all(&b).await.unwrap();
            }
            w.write_all(&[0, 0, 0, 0]).await.unwrap();
        };
        let receive = async {
            let mut n = 0u8;
            loop {
                let mut len = [0u8; 4];
                r.read_exact(&mut len).await.unwrap();
                if len == [0; 4] {
                    break;
                }
                let mut b = vec![0u8; u32::from_be_bytes(len) as usize];
                r.read_exact(&mut b).await.unwrap();
                assert_eq!((b[0], &b[1..]), (n, &block[1..]));
                n += 1;
            }
            let out: Output = read_frame(&mut r).await.unwrap();
            (n, out.text)
        };
        let ((), (blocks, text)) = tokio::join!(send, receive);
        assert_eq!((blocks, text.as_str()), (64, "67108864"));
        s.stop().await;
    }

    #[tokio::test]
    async fn a_service_that_replies_and_closes_its_output_early_still_gets_the_whole_upload() {
        let s = served(Guard::default()).await;
        let mut conn = s.open("demo_early", json!({})).await;
        let early: Output = read_frame(&mut conn).await.unwrap();
        assert_eq!(early.text, "early");
        let block = vec![9u8; 1 << 20];
        for _ in 0..8 {
            conn.write_all(&(1u32 << 20).to_be_bytes()).await.unwrap();
            conn.write_all(&block).await.unwrap();
        }
        conn.write_all(&[0, 0, 0, 0]).await.unwrap();
        assert!(
            appears(&s.state("received")).await,
            "the upload was cut off"
        );
        assert_eq!(
            std::fs::read_to_string(s.state("received")).unwrap(),
            (8u32 << 20).to_string()
        );
        s.stop().await;
    }

    #[tokio::test]
    async fn a_service_that_dies_comes_back_and_answers_again() {
        let s = served(Guard::default()).await;
        assert!(!s.echo().await.is_error);
        let first = s.pid().unwrap();
        // SAFETY: the pid is the fake service this test started.
        unsafe { libc::kill(first, libc::SIGKILL) };
        let mut second = first;
        for _ in 0..100 {
            tokio::time::sleep(POLL).await;
            second = s.pid().unwrap_or(first);
            if second != first {
                break;
            }
        }
        assert_ne!(second, first, "the service came back");
        assert!(!s.echo().await.is_error);
        s.stop().await;
        assert!(!alive(second), "the service stopped with the daemon");
    }

    #[tokio::test]
    async fn a_service_killed_with_its_socket_left_behind_binds_again_on_restart() {
        let s = served(Guard::default()).await;
        std::fs::write(s.state("keep-socket"), "").unwrap();
        let first = s.pid().unwrap();
        // SAFETY: the pid is the fake service this test started.
        unsafe { libc::kill(first, libc::SIGKILL) };
        let mut second = first;
        for _ in 0..100 {
            tokio::time::sleep(POLL).await;
            second = s.pid().unwrap_or(first);
            if second != first {
                break;
            }
        }
        assert_ne!(second, first, "the restarted service did not bind");
        assert!(!s.echo().await.is_error);
        s.stop().await;
    }

    #[tokio::test]
    async fn a_stop_drops_live_splices_refuses_new_calls_and_keeps_the_service() {
        let s = served(Guard::default()).await;
        let mut held = s.open("demo_hold", json!({})).await;
        assert!(appears(&s.state("held")).await);
        let started = Instant::now();
        assert!(!s.daemon.answer(control("safety_stop")).await.is_error);
        let mut rest = Vec::new();
        held.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty() && started.elapsed() < Duration::from_secs(1));
        assert!(appears(&s.state("dropped")).await);
        let pid = s.pid().unwrap();
        assert!(alive(pid));
        let refused = s.echo().await;
        assert_eq!(
            (refused.is_error, refused.text.as_str()),
            (true, crate::STOPPED)
        );
        s.daemon.answer(control("safety_resume")).await;
        assert!(!s.echo().await.is_error);
        assert_eq!(s.pid(), Some(pid));
        s.stop().await;
    }

    #[tokio::test]
    async fn a_stop_and_a_resume_back_to_back_still_drop_a_live_splice() {
        let s = served(Guard::default()).await;
        let mut held = s.open("demo_hold", json!({})).await;
        assert!(appears(&s.state("held")).await);
        for tool in ["safety_stop", "safety_resume"] {
            s.daemon.answer(control(tool)).await;
        }
        let mut rest = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(2), held.read_to_end(&mut rest)).await;
        assert!(read.is_ok(), "the splice outlived the stop");
        assert!(!s.echo().await.is_error);
        s.stop().await;
    }

    #[tokio::test]
    async fn script_run_off_leaves_a_service_running_and_routed() {
        let s = served(Guard {
            script_run: false,
            ..Guard::default()
        })
        .await;
        let out = s.echo().await;
        assert!(!out.is_error, "{}", out.text);
        s.stop().await;
    }

    #[tokio::test]
    async fn the_mcp_server_lists_a_service_tool_with_its_schema_and_forwards_its_call() {
        let s = served(Guard::default()).await;
        let lines = [
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "demo_echo", "arguments": {"say": "hi"}}}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "demo_hold", "arguments": {}}}),
        ];
        let input: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let mut output = Vec::new();
        mcp::serve(input.as_bytes(), &mut output, &s.endpoint, &s.plugins)
            .await
            .unwrap();
        let out: Vec<Value> = output
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_slice(l).unwrap())
            .collect();
        let listed: Vec<&Value> = out[0]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["name"].as_str().is_some_and(|n| n.starts_with("demo_")))
            .collect();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0]["inputSchema"]["properties"]["say"]["type"],
            "string"
        );
        let echoed: Value =
            serde_json::from_str(out[1]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(echoed["args"], json!({"say": "hi"}));
        assert_eq!(echoed["agent_id"], crate::NAME);
        assert_eq!(out[2]["error"]["message"], "unknown tool demo_hold");
        s.stop().await;
    }

    async fn bench_service(socket: String) {
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        loop {
            let (mut conn, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let call: Call =
                    serde_json::from_slice(&read_body(&mut conn).await.unwrap()).unwrap();
                if call.tool == "bench_sink" {
                    let mut buf = vec![0u8; 1 << 20];
                    loop {
                        let mut len = [0u8; 4];
                        conn.read_exact(&mut len).await.unwrap();
                        let n = u32::from_be_bytes(len) as usize;
                        if n == 0 {
                            break;
                        }
                        conn.read_exact(&mut buf[..n]).await.unwrap();
                    }
                }
                write_frame(&mut conn, &Output::text("ok")).await.unwrap();
            });
        }
    }

    async fn bench_call(endpoint: &str, tool: &str, blocks: usize) -> Duration {
        let started = Instant::now();
        let mut conn = server::connect(endpoint).await.unwrap();
        let call = Call {
            agent_id: "bench".into(),
            tool: tool.into(),
            args: json!({"pad": "x".repeat(1000)}),
        };
        write_frame(&mut conn, &call).await.unwrap();
        let block = vec![7u8; 1 << 20];
        for _ in 0..blocks {
            conn.write_all(&(1u32 << 20).to_be_bytes()).await.unwrap();
            conn.write_all(&block).await.unwrap();
        }
        if tool == "bench_sink" {
            conn.write_all(&[0, 0, 0, 0]).await.unwrap();
        }
        let out: Output = read_frame(&mut conn).await.unwrap();
        assert_eq!(out.text, "ok");
        started.elapsed()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "a microbench: cargo test -p shuttle --release splice_cost -- --ignored --nocapture"]
    async fn splice_cost() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        private_dir(&super::dir(&endpoint)).unwrap();
        let socket = socket(&endpoint, "bench");
        tokio::spawn(bench_service(socket.clone()));
        let runner = Runner::new(
            "node".into(),
            dir.path().into(),
            endpoint.clone(),
            dir.path().join("flows"),
            Vec::new(),
        );
        let routes = ["bench_echo", "bench_sink"]
            .map(|t| (t.to_owned(), socket.clone()))
            .into();
        let daemon = Arc::new(
            Daemon::new(
                NoDesk,
                Ok(Models::new(Config::default())),
                runner,
                Guard::default(),
            )
            .with_services(routes),
        );
        tokio::spawn(server::serve(
            daemon,
            server::bind(&endpoint).await.unwrap(),
        ));
        listening(&socket).await.unwrap();
        let (mut direct, mut spliced) = (Vec::new(), Vec::new());
        for round in 0..40 {
            let order = if round % 2 == 0 {
                [&socket, &endpoint]
            } else {
                [&endpoint, &socket]
            };
            for at in order {
                for _ in 0..50 {
                    let d = bench_call(at, "bench_echo", 0).await;
                    if at == &socket {
                        direct.push(d)
                    } else {
                        spliced.push(d)
                    }
                }
            }
        }
        let p = |v: &mut Vec<Duration>, q: usize| {
            v.sort();
            v[v.len() * q / 100].as_secs_f64() * 1e6
        };
        let (d50, d90, s50, s90) = (
            p(&mut direct, 50),
            p(&mut direct, 90),
            p(&mut spliced, 50),
            p(&mut spliced, 90),
        );
        println!(
            "1 KB call, {} each, ABBA: direct p50 {d50:.1} us p90 {d90:.1} us; spliced p50 {s50:.1} us p90 {s90:.1} us; added p50 {:.1} us",
            direct.len(),
            s50 - d50
        );
        let mut big = [Vec::new(), Vec::new()];
        for round in 0..6 {
            let order = if round % 2 == 0 { [0, 1] } else { [1, 0] };
            for arm in order {
                let at = if arm == 0 { &socket } else { &endpoint };
                big[arm].push(bench_call(at, "bench_sink", 64).await);
            }
        }
        for (arm, name) in [(0, "direct"), (1, "spliced")] {
            let best = big[arm].iter().min().unwrap().as_secs_f64();
            println!(
                "64 MiB {name}: best {:.1} ms, {:.0} MiB/s",
                best * 1e3,
                64.0 / best
            );
        }
    }
}

#[cfg(test)]
#[cfg(windows)]
mod pipe_tests {
    use desk_core::wire::{Call, Output, read_frame};
    use serde_json::json;

    use super::*;
    use crate::plugins::ServiceTool;

    const POWERSHELL: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    const PING: &str = r"C:\Windows\System32\PING.EXE";
    const FAKE: &str = r#"
$child = Start-Process -FilePath ping -ArgumentList '-n','60','127.0.0.1' -WindowStyle Hidden -PassThru
Set-Content -Path $args[0] -Value $child.Id
$name = $env:SHUTTLE_SERVICE_SOCKET -replace '^\\\\\.\\pipe\\', ''
while ($true) {
  $p = New-Object System.IO.Pipes.NamedPipeServerStream($name, [System.IO.Pipes.PipeDirection]::InOut, 10)
  $p.WaitForConnection()
  $head = New-Object byte[] 4; [void]$p.Read($head, 0, 4); [Array]::Reverse($head)
  $n = [BitConverter]::ToInt32($head, 0); $body = New-Object byte[] $n; $got = 0
  while ($got -lt $n) { $got += $p.Read($body, $got, $n - $got) }
  $out = [Text.Encoding]::UTF8.GetBytes('{"text":"pong","is_error":false}')
  $len = [BitConverter]::GetBytes([int]$out.Length); [Array]::Reverse($len)
  $p.Write($len, 0, 4); $p.Write($out, 0, $out.Length); $p.Flush(); $p.WaitForPipeDrain(); $p.Dispose()
}
"#;

    fn service(bin: &str, args: Vec<String>) -> Service {
        Service {
            service: "demo".into(),
            bin: bin.into(),
            args,
            tools: vec![ServiceTool {
                name: "demo_ping".into(),
                description: "d".into(),
                input_schema: json!({"type": "object"}),
                mcp: true,
            }],
        }
    }

    fn body() -> Zeroizing<Vec<u8>> {
        let call = Call {
            agent_id: "a".into(),
            tool: "demo_ping".into(),
            args: json!({}),
        };
        Zeroizing::new(serde_json::to_vec(&call).unwrap())
    }

    fn alive(pid: &str) -> bool {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).contains(pid)
    }

    #[tokio::test]
    async fn a_service_serves_its_pipe_and_a_stop_ends_it_with_the_process_it_started() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("service.ps1");
        std::fs::write(&script, FAKE).unwrap();
        let pidfile = dir.path().join("child.pid");
        let args = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]
            .into_iter()
            .map(str::to_owned)
            .chain([script.display().to_string(), pidfile.display().to_string()])
            .collect();
        let desk = format!(r"\\.\pipe\shuttle-test-serve-{}", std::process::id());
        let running = Services::start(&[service(POWERSHELL, args)], &desk, &[]).unwrap();
        let (mut client, mut far) = tokio::io::duplex(1 << 16);
        let routed =
            tokio::spawn(async move { route(&socket(&desk, "demo"), body(), &mut far).await });
        let out: Output = read_frame(&mut client).await.unwrap();
        assert_eq!((out.text.as_str(), out.is_error), ("pong", false));
        drop(client);
        routed.await.unwrap().unwrap();
        let mut child = String::new();
        for _ in 0..100 {
            child = std::fs::read_to_string(&pidfile)
                .unwrap_or_default()
                .trim()
                .to_owned();
            if !child.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(alive(&child), "{child} never started");
        running.stop().await;
        let mut gone = false;
        for _ in 0..40 {
            gone = !alive(&child);
            if gone {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(gone, "{child} outlived the service's stop");
    }

    #[tokio::test]
    async fn a_call_is_refused_by_a_pipe_served_outside_the_services_tree() {
        let desk = format!(r"\\.\pipe\shuttle-test-squat-{}", std::process::id());
        let args = ["-n", "60", "127.0.0.1"].map(str::to_owned).to_vec();
        let running = Services::start(&[service(PING, args)], &desk, &[]).unwrap();
        let pipe = socket(&desk, "demo");
        let squatter = tokio::net::windows::named_pipe::ServerOptions::new()
            .create(&pipe)
            .unwrap();
        let held = tokio::spawn(async move {
            let _ = squatter.connect().await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        for _ in 0..100 {
            if servers().contains_key(&pipe) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let (_client, mut far) = tokio::io::duplex(1 << 16);
        let err = route(&pipe, body(), &mut far).await.unwrap_err();
        assert!(
            err.contains(&format!("process {}", std::process::id()))
                && err.contains("outside the service's process tree"),
            "{err}"
        );
        held.abort();
        running.stop().await;
    }

    #[tokio::test]
    async fn a_client_that_closes_mid_call_ends_the_call_and_the_service_sees_its_pipe_close() {
        let pipe = format!(r"\\.\pipe\shuttle-test-close-{}", std::process::id());
        let mut service = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe)
            .unwrap();
        let near = server::connect(&pipe).await.unwrap();
        service.connect().await.unwrap();
        let (client, far) = tokio::io::duplex(1 << 16);
        let spliced = tokio::spawn(splice(far, near));
        drop(client);
        let read = tokio::time::timeout(Duration::from_secs(5), service.read(&mut [0u8; 1]))
            .await
            .expect("the service never saw the call end");
        assert!(!matches!(read, Ok(n) if n > 0), "{read:?}");
        tokio::time::timeout(Duration::from_secs(5), spliced)
            .await
            .expect("the splice outlived the call")
            .unwrap();
    }

    async fn reply_through(size: usize, run: usize) -> Result<(), String> {
        use tokio::net::windows::named_pipe::ServerOptions;

        let reply: Vec<u8> = (0..size)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let desk = format!(
            r"\\.\pipe\shuttle-test-reply-{}-{size}-{run}",
            std::process::id()
        );
        let accepted = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&desk)
            .unwrap();
        let mut caller = server::connect(&desk).await.unwrap();
        accepted.connect().await.unwrap();
        let pipe = format!("{desk}-svc");
        let mut service = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe)
            .unwrap();
        let near = server::connect(&pipe).await.unwrap();
        service.connect().await.unwrap();
        let spliced = tokio::spawn(splice(accepted, near));
        let sent = reply.clone();
        tokio::spawn(async move {
            for frame in sent.chunks(16 << 10) {
                service.write_all(frame).await.unwrap();
            }
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        let mut got = Vec::new();
        caller.read_to_end(&mut got).await.unwrap();
        spliced.await.unwrap();
        if got == reply {
            Ok(())
        } else {
            Err(format!("{size} bytes, run {run}: {} arrived", got.len()))
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reply_the_service_sends_before_it_closes_reaches_a_caller_that_reads_late() {
        let mut runs = JoinSet::new();
        for size in [64 << 10, 1 << 20] {
            for run in 0..20 {
                runs.spawn(reply_through(size, run));
            }
        }
        let lost: Vec<String> = runs
            .join_all()
            .await
            .into_iter()
            .filter_map(Result::err)
            .collect();
        assert!(lost.is_empty(), "{lost:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_pipe_that_stays_busy_fails_the_call_after_the_listen_wait() {
        let pipe = format!(r"\\.\pipe\shuttle-test-busy-{}", std::process::id());
        let _service = tokio::net::windows::named_pipe::ServerOptions::new()
            .max_instances(1)
            .create(&pipe)
            .unwrap();
        let _held = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&pipe)
            .unwrap();
        let start = tokio::time::Instant::now();
        let err = listening(&pipe).await.unwrap_err();
        assert!(
            err.contains("no service listens") && start.elapsed() >= LISTEN_WAIT,
            "{err} after {:?}",
            start.elapsed()
        );
    }
}
