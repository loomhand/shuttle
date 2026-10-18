//! The desk endpoint: a unix socket on macOS and Linux, a named pipe on Windows, one framed call per connection.

use std::sync::Arc;

use desk_core::wire::{Call, Output, read_frame, write_frame};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::desk::Desk;
use crate::driver::Input;
use crate::model::Access;

#[cfg(windows)]
const PIPE_BUSY: i32 = 231;
#[cfg(windows)]
const BUSY_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// One client connection to an endpoint.
#[cfg(unix)]
pub type Conn = tokio::net::UnixStream;
/// One client connection to an endpoint.
#[cfg(windows)]
pub type Conn = tokio::net::windows::named_pipe::NamedPipeClient;

/// What answers the calls of an endpoint.
pub trait Handler: Send + Sync + 'static {
    /// Answers `call`; a failure is an error output, never a dropped connection.
    fn answer(&self, call: Call) -> impl Future<Output = Output> + Send;
}

impl<B: Access + 'static, D: Input + Send + Sync + 'static> Handler for Desk<B, D> {
    async fn answer(&self, call: Call) -> Output {
        self.run(&call.tool, call.args)
            .await
            .unwrap_or_else(Output::error)
    }
}

/// The handler of a host without a desk backend.
pub struct NoDesk;

impl Handler for NoDesk {
    async fn answer(&self, call: Call) -> Output {
        Output::error(format!("{}: no desk backend for this OS yet", call.tool))
    }
}

/// An endpoint this daemon holds: the unix socket with the lock file beside it that keeps a second daemon out, or the first instance of a Windows pipe.
pub struct Listener {
    #[cfg(unix)]
    socket: tokio::net::UnixListener,
    #[cfg(unix)]
    _lock: std::fs::File,
    #[cfg(windows)]
    first: tokio::net::windows::named_pipe::NamedPipeServer,
    endpoint: String,
}

/// Takes the unix socket `endpoint`, readable by its user only, once no other daemon holds its lock.
#[cfg(unix)]
pub async fn bind(endpoint: &str) -> Result<Listener, String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let path = std::path::Path::new(endpoint);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let lock_path = format!("{endpoint}.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|e| format!("open {lock_path}: {e}"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(format!("another daemon serves {endpoint}"));
        }
        Err(std::fs::TryLockError::Error(e)) => return Err(format!("lock {lock_path}: {e}")),
    }
    let _ = std::fs::remove_file(path);
    let socket =
        tokio::net::UnixListener::bind(path).map_err(|e| format!("bind {endpoint}: {e}"))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod {endpoint}: {e}"))?;
    Ok(Listener {
        socket,
        _lock: lock,
        endpoint: endpoint.to_owned(),
    })
}

/// Serves `handler` at `listener`.
#[cfg(unix)]
pub async fn serve<H: Handler>(handler: Arc<H>, listener: Listener) -> Result<(), String> {
    loop {
        let (stream, _) = listener
            .socket
            .accept()
            .await
            .map_err(|e| format!("accept {}: {e}", listener.endpoint))?;
        tokio::spawn(handle(handler.clone(), stream));
    }
}

/// Takes the named pipe `endpoint`, local clients only, once no other daemon holds it.
#[cfg(windows)]
pub async fn bind(endpoint: &str) -> Result<Listener, String> {
    Ok(Listener {
        first: pipe(endpoint, true)?,
        endpoint: endpoint.to_owned(),
    })
}

/// Serves `handler` at `listener`.
#[cfg(windows)]
pub async fn serve<H: Handler>(handler: Arc<H>, listener: Listener) -> Result<(), String> {
    let endpoint = listener.endpoint;
    let mut server = listener.first;
    loop {
        server
            .connect()
            .await
            .map_err(|e| format!("accept {endpoint}: {e}"))?;
        let client = std::mem::replace(&mut server, pipe(&endpoint, false)?);
        tokio::spawn(handle(handler.clone(), client));
    }
}

/// Sends one call to the endpoint and returns its output.
pub async fn call(endpoint: &str, call: &Call) -> Result<Output, String> {
    let conn = connect(endpoint)
        .await
        .map_err(|e| format!("connect {endpoint}: {e}"))?;
    exchange(conn, call).await
}

pub fn cache_endpoint(var: &str, pipe: &str, dir: &str, file: &str) -> String {
    if let Some(e) = std::env::var(var).ok().filter(|e| !e.is_empty()) {
        return e;
    }
    if cfg!(windows) {
        return format!(r"\\.\pipe\{pipe}");
    }
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/Library/Caches/{dir}/{file}")
}

/// Connects to the endpoint.
#[cfg(unix)]
pub async fn connect(endpoint: &str) -> std::io::Result<Conn> {
    tokio::net::UnixStream::connect(endpoint).await
}

/// Connects to the endpoint, waiting while a Windows pipe is busy.
#[cfg(windows)]
pub async fn connect(endpoint: &str) -> std::io::Result<Conn> {
    loop {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(endpoint) {
            Err(e) if e.raw_os_error() == Some(PIPE_BUSY) => tokio::time::sleep(BUSY_POLL).await,
            done => return done,
        }
    }
}

/// Sends `call` on `conn` and reads its output.
pub async fn exchange(mut conn: Conn, call: &Call) -> Result<Output, String> {
    write_frame(&mut conn, call).await?;
    read_frame(&mut conn).await
}

/// This OS's desk: its accessibility tree, with input through the cua-driver at `driver`, limited to the windows of `apps` unless empty.
#[cfg(target_os = "macos")]
pub fn native(driver: String, apps: Vec<String>) -> impl Handler {
    use crate::{ax::Ax, driver::Driver, keys::Os};
    Desk::new(Ax::open, Driver::new(driver), Os::Mac).allow(apps)
}

/// This OS's desk: its accessibility tree, with input through the cua-driver at `driver`, limited to the windows of `apps` unless empty.
#[cfg(windows)]
pub fn native(driver: String, apps: Vec<String>) -> impl Handler {
    use crate::{driver::Driver, keys::Os, uia::Uia};
    Desk::new(Uia::open, Driver::new(driver), Os::Windows).allow(apps)
}

/// This OS's desk: none yet.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn native(_: String, _: Vec<String>) -> impl Handler {
    NoDesk
}

#[cfg(windows)]
fn pipe(
    endpoint: &str,
    first: bool,
) -> Result<tokio::net::windows::named_pipe::NamedPipeServer, String> {
    tokio::net::windows::named_pipe::ServerOptions::new()
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .create(endpoint)
        .map_err(|e| format!("create pipe {endpoint}: {e}"))
}

async fn handle<H: Handler, S: AsyncRead + AsyncWrite + Unpin>(handler: Arc<H>, mut stream: S) {
    let out = match read_frame::<_, Call>(&mut stream).await {
        Ok(call) => handler.answer(call).await,
        Err(e) => Output::error(e),
    };
    let _ = write_frame(&mut stream, &out).await;
}

#[cfg(all(test, unix))]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::desk::tests::Stub;
    use crate::keys::Os;

    struct NoDriver;

    impl Input for NoDriver {
        async fn call(&self, tool: &str, _: Value) -> Result<Value, String> {
            Err(format!("{tool}: no driver"))
        }
    }

    async fn answer(endpoint: &str, ask: &Call) -> Result<Output, String> {
        let mut out = Err(String::new());
        for _ in 0..50 {
            out = call(endpoint, ask).await;
            if out.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        out
    }

    #[tokio::test]
    async fn a_daemon_still_starting_keeps_a_second_one_out_before_its_socket_exists() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let starting = std::fs::File::create(format!("{endpoint}.lock")).unwrap();
        starting.lock().unwrap();
        assert_eq!(
            bind(&endpoint).await.err(),
            Some(format!("another daemon serves {endpoint}"))
        );
        assert!(!std::path::Path::new(&endpoint).exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_daemons_started_at_once_on_one_endpoint_leave_exactly_one_serving() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let start = || {
            let endpoint = endpoint.clone();
            tokio::spawn(async move { serve(Arc::new(NoDesk), bind(&endpoint).await?).await })
        };
        let (mut a, mut b) = (start(), start());
        let loser = tokio::select! {
            r = &mut a => { assert!(!b.is_finished()); r }
            r = &mut b => { assert!(!a.is_finished()); r }
        };
        assert_eq!(
            loser.unwrap().unwrap_err(),
            format!("another daemon serves {endpoint}")
        );
        let ask = Call {
            agent_id: "desk".into(),
            tool: "desk_screen".into(),
            args: json!({}),
        };
        assert!(
            answer(&endpoint, &ask)
                .await
                .unwrap()
                .text
                .contains("no desk backend")
        );
        a.abort();
        b.abort();
    }

    #[tokio::test]
    async fn the_socket_answers_desk_calls_with_toolhost_frames() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let desk = Arc::new(Desk::new(|| Ok(Stub::default()), NoDriver, Os::Mac));
        let server = tokio::spawn({
            let endpoint = endpoint.clone();
            async move { serve(desk, bind(&endpoint).await?).await }
        });
        let ask = |tool: &str, args: Value| Call {
            agent_id: "desk".into(),
            tool: tool.into(),
            args,
        };
        assert_eq!(
            answer(&endpoint, &ask("desk_windows", json!({})))
                .await
                .unwrap()
                .text,
            "[]"
        );
        let missing = call(&endpoint, &ask("desk_snapshot", json!({})))
            .await
            .unwrap();
        assert!(missing.is_error);
        assert_eq!(missing.text, "no focused window");
        let other = call(&endpoint, &ask("shell", json!({}))).await.unwrap();
        assert_eq!(other.text, "unknown desk tool shell");
        let key = call(&endpoint, &ask("desk_key", json!({"keys": "cmd+s"})))
            .await
            .unwrap();
        assert_eq!(key.text, "hotkey: no driver");
        server.abort();
    }
}
