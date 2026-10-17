//! The desk endpoint: a unix socket on macOS and Linux, a named pipe on Windows, one framed call per connection.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::desk::Desk;
use crate::driver::Input;
use crate::model::Access;
use crate::rpc::{Call, Output, read_frame, write_frame};

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
        if !call.tool.starts_with("desk_") {
            return Output::error(format!("unknown tool {}", call.tool));
        }
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

/// Serves `handler` at the unix socket `endpoint`, readable by its user only.
#[cfg(unix)]
pub async fn serve<H: Handler>(handler: Arc<H>, endpoint: &str) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let path = std::path::Path::new(endpoint);
    if connect(endpoint).await.is_ok() {
        return Err(format!("another daemon serves {endpoint}"));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let _ = std::fs::remove_file(path);
    let listener =
        tokio::net::UnixListener::bind(path).map_err(|e| format!("bind {endpoint}: {e}"))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod {endpoint}: {e}"))?;
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|e| format!("accept {endpoint}: {e}"))?;
        tokio::spawn(handle(handler.clone(), stream));
    }
}

/// Serves `handler` at the named pipe `endpoint`, local clients only.
#[cfg(windows)]
pub async fn serve<H: Handler>(handler: Arc<H>, endpoint: &str) -> Result<(), String> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let create = |first: bool| {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create(endpoint)
            .map_err(|e| format!("create pipe {endpoint}: {e}"))
    };
    let mut server = create(true)?;
    loop {
        server
            .connect()
            .await
            .map_err(|e| format!("accept {endpoint}: {e}"))?;
        let client = std::mem::replace(&mut server, create(false)?);
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

/// Connects to the endpoint, waiting while a Windows pipe is busy.
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

/// This OS's desk: its accessibility tree, with input through cua-driver at its default endpoint.
#[cfg(target_os = "macos")]
pub fn native() -> impl Handler {
    use crate::{ax::Ax, driver::Driver, keys::Os};
    Desk::new(Ax::open, Driver::new(Driver::default_endpoint()), Os::Mac)
}

/// This OS's desk: its accessibility tree, with input through cua-driver at its default endpoint.
#[cfg(windows)]
pub fn native() -> impl Handler {
    use crate::{driver::Driver, keys::Os, uia::Uia};
    Desk::new(
        Uia::open,
        Driver::new(Driver::default_endpoint()),
        Os::Windows,
    )
}

/// This OS's desk: none yet.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn native() -> impl Handler {
    NoDesk
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
    use crate::keys::Os;
    use crate::model::{Act, Extras, Frame, Node, Tree};

    struct Empty;

    impl Access for Empty {
        fn frames(&mut self) -> Result<Vec<Frame>, String> {
            Ok(Vec::new())
        }

        fn tree(&mut self, _: &Frame, _: Extras) -> Result<Tree, String> {
            Err("no tree".into())
        }

        fn node(&mut self, _: &str, _: Extras, _: usize) -> Result<Node, String> {
            Err("no node".into())
        }

        fn ancestors(&mut self, _: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }

        fn at(&mut self, _: &str, _: i32, _: i32) -> Result<Option<String>, String> {
            Ok(None)
        }

        fn act(&mut self, _: &str, _: Act) -> Result<bool, String> {
            Ok(false)
        }

        fn close(&mut self, _: &Frame) -> Result<(), String> {
            Ok(())
        }
    }

    struct NoDriver;

    impl Input for NoDriver {
        async fn call(&self, tool: &str, _: Value) -> Result<Value, String> {
            Err(format!("{tool}: no driver"))
        }
    }

    #[tokio::test]
    async fn the_socket_answers_desk_calls_with_toolhost_frames() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let desk = Arc::new(Desk::new(|| Ok(Empty), NoDriver, Os::Mac));
        let server = tokio::spawn({
            let endpoint = endpoint.clone();
            async move { serve(desk, &endpoint).await }
        });
        let ask = |tool: &str, args: Value| Call {
            agent_id: "desk".into(),
            tool: tool.into(),
            args,
        };
        let mut out = Err(String::new());
        for _ in 0..50 {
            out = call(&endpoint, &ask("desk_windows", json!({}))).await;
            if out.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(out.unwrap().text, "[]");
        let missing = call(&endpoint, &ask("desk_snapshot", json!({})))
            .await
            .unwrap();
        assert!(missing.is_error);
        assert_eq!(missing.text, "no focused window");
        let other = call(&endpoint, &ask("shell", json!({}))).await.unwrap();
        assert_eq!(other.text, "unknown tool shell");
        let key = call(&endpoint, &ask("desk_key", json!({"keys": "cmd+s"})))
            .await
            .unwrap();
        assert_eq!(key.text, "hotkey: no driver");
        server.abort();
    }
}
