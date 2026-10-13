//! The desk endpoint: a unix socket on macOS, a named pipe on Windows, one framed call per connection.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::desk::Desk;
use crate::driver::Input;
use crate::model::Access;
use crate::rpc::{Call, Output, read_frame, write_frame};

/// The endpoint: `TOOLHOST_SOCKET`, else this OS's default.
pub fn default_endpoint() -> String {
    if let Some(e) = std::env::var("TOOLHOST_SOCKET")
        .ok()
        .filter(|e| !e.is_empty())
    {
        return e;
    }
    if cfg!(windows) {
        return r"\\.\pipe\cocoonbox-desk".to_owned();
    }
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/Library/Caches/cocoonbox/desk.sock")
}

/// Serves `desk` at the unix socket `endpoint`, readable by its user only.
#[cfg(unix)]
pub async fn serve<B, D>(desk: Arc<Desk<B, D>>, endpoint: &str) -> Result<(), String>
where
    B: Access + 'static,
    D: Input + Send + Sync + 'static,
{
    use std::os::unix::fs::PermissionsExt;

    let path = std::path::Path::new(endpoint);
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
        tokio::spawn(handle(desk.clone(), stream));
    }
}

/// Serves `desk` at the named pipe `endpoint`, local clients only.
#[cfg(windows)]
pub async fn serve<B, D>(desk: Arc<Desk<B, D>>, endpoint: &str) -> Result<(), String>
where
    B: Access + 'static,
    D: Input + Send + Sync + 'static,
{
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
        tokio::spawn(handle(desk.clone(), client));
    }
}

/// Sends one call to the desk at `endpoint` and returns its output.
pub async fn call(endpoint: &str, call: &Call) -> Result<Output, String> {
    #[cfg(unix)]
    let mut stream = tokio::net::UnixStream::connect(endpoint)
        .await
        .map_err(|e| format!("connect {endpoint}: {e}"))?;
    #[cfg(windows)]
    let mut stream = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(endpoint)
        .map_err(|e| format!("connect {endpoint}: {e}"))?;
    write_frame(&mut stream, call).await?;
    read_frame(&mut stream).await
}

async fn handle<B, D, S>(desk: Arc<Desk<B, D>>, mut stream: S)
where
    B: Access + 'static,
    D: Input + Send + Sync + 'static,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let out = match read_frame::<_, Call>(&mut stream).await {
        Ok(call) if call.tool.starts_with("desk_") => desk
            .run(&call.tool, call.args)
            .await
            .unwrap_or_else(Output::error),
        Ok(call) => Output::error(format!("unknown tool {}", call.tool)),
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
