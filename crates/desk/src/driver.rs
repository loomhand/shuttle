//! cua-driver client: one line of JSON each way over its daemon socket, a named pipe on Windows.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SESSION: &str = "desk";
const ENDED: &str = "session has ended";

/// The driver calls the desk makes: input, windows, launch and capture.
pub trait Input {
    /// Calls driver tool `tool` and returns its result.
    fn call(&self, tool: &str, args: Value) -> impl Future<Output = Result<Value, String>> + Send;
}

/// A client of the cua-driver daemon at `endpoint`, in one driver session with desktop capture scope.
#[derive(Debug)]
pub struct Driver {
    endpoint: String,
    started: AtomicBool,
}

impl Driver {
    pub fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            started: AtomicBool::new(false),
        }
    }

    /// The daemon's endpoint: `CUA_DRIVER_SOCKET`, else the driver's default for this OS.
    pub fn default_endpoint() -> String {
        if let Some(e) = std::env::var("CUA_DRIVER_SOCKET")
            .ok()
            .filter(|e| !e.is_empty())
        {
            return e;
        }
        if cfg!(windows) {
            return r"\\.\pipe\cua-driver".to_owned();
        }
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/Library/Caches/cua-driver/cua-driver.sock")
    }

    async fn start(&self) -> Result<(), String> {
        self.send("start_session", json!({"capture_scope": "desktop"}))
            .await?;
        self.started.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn send(&self, tool: &str, mut args: Value) -> Result<Value, String> {
        args["session"] = SESSION.into();
        let mut line = serde_json::to_vec(&json!({"method": "call", "name": tool, "args": args}))
            .map_err(|e| format!("encode: {e}"))?;
        line.push(b'\n');
        let reply = tokio::time::timeout(REQUEST_TIMEOUT, self.exchange(line))
            .await
            .map_err(|_| format!("cua-driver {tool} timed out"))??;
        result(tool, &reply)
    }

    #[cfg(unix)]
    async fn exchange(&self, line: Vec<u8>) -> Result<String, String> {
        let stream = tokio::net::UnixStream::connect(&self.endpoint)
            .await
            .map_err(|e| format!("connect cua-driver {}: {e}", self.endpoint))?;
        roundtrip(stream, line).await
    }

    #[cfg(windows)]
    async fn exchange(&self, line: Vec<u8>) -> Result<String, String> {
        use tokio::net::windows::named_pipe::ClientOptions;
        const PIPE_BUSY: i32 = 231;
        let pipe = loop {
            match ClientOptions::new().open(&self.endpoint) {
                Ok(pipe) => break pipe,
                Err(e) if e.raw_os_error() == Some(PIPE_BUSY) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => return Err(format!("connect cua-driver {}: {e}", self.endpoint)),
            }
        };
        roundtrip(pipe, line).await
    }
}

impl Input for Driver {
    async fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
        if !self.started.load(Ordering::Relaxed) {
            self.start().await?;
        }
        match self.send(tool, args.clone()).await {
            Err(e) if e.contains(ENDED) => {
                self.start().await?;
                self.send(tool, args).await
            }
            done => done,
        }
    }
}

/// `req` addressed to the screen, not to a window.
pub fn desktop(mut req: Value) -> Value {
    req["scope"] = "desktop".into();
    req
}

/// The text blocks of a driver result.
pub fn text_of(v: &Value) -> String {
    v["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn roundtrip<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    line: Vec<u8>,
) -> Result<String, String> {
    let mut stream = BufReader::new(stream);
    stream
        .get_mut()
        .write_all(&line)
        .await
        .map_err(|e| format!("write cua-driver: {e}"))?;
    let mut reply = String::new();
    stream
        .read_line(&mut reply)
        .await
        .map_err(|e| format!("read cua-driver: {e}"))?;
    Ok(reply)
}

fn result(tool: &str, reply: &str) -> Result<Value, String> {
    let mut v: Value =
        serde_json::from_str(reply).map_err(|e| format!("decode cua-driver reply: {e}"))?;
    if v["ok"] != true {
        return Err(format!(
            "{tool}: {}",
            v["error"].as_str().unwrap_or("cua-driver call failed")
        ));
    }
    let out = v["result"].take();
    if out["isError"] == true {
        return Err(format!("{tool}: {}", text_of(&out)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_become_results_or_errors() {
        let ok = result(
            "list_windows",
            r#"{"ok":true,"result":{"structuredContent":{"windows":[]}}}"#,
        );
        assert_eq!(ok.unwrap()["structuredContent"]["windows"], json!([]));
        let refused = result(
            "click",
            r#"{"ok":false,"error":"no permission","exit_code":1}"#,
        );
        assert_eq!(refused.unwrap_err(), "click: no permission");
        let failed = result(
            "click",
            r#"{"ok":true,"result":{"isError":true,"content":[{"type":"text","text":"off screen"}]}}"#,
        );
        assert_eq!(failed.unwrap_err(), "click: off screen");
        assert!(result("click", "").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_first_call_starts_a_desktop_session_and_an_ended_one_starts_again() {
        use tokio::io::AsyncBufReadExt;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("cua.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let ok = b"{\"ok\":true,\"result\":{\"structuredContent\":{\"width\":1280}}}\n";
        let ended =
            b"{\"ok\":false,\"error\":\"session has ended; tool call 'press_key' was rejected\"}\n";
        let daemon = tokio::spawn(async move {
            let mut lines = Vec::new();
            for i in 0..5 {
                let (s, _) = listener.accept().await.unwrap();
                let mut s = BufReader::new(s);
                let mut line = String::new();
                s.read_line(&mut line).await.unwrap();
                let reply: &[u8] = if i == 2 { ended } else { ok };
                s.get_mut().write_all(reply).await.unwrap();
                lines.push(serde_json::from_str::<Value>(&line).unwrap()["name"].clone());
            }
            lines
        });
        let d = Driver::new(socket.to_string_lossy().into_owned());
        let got = d.call("get_screen_size", json!({})).await.unwrap();
        assert_eq!(got["structuredContent"]["width"], 1280);
        d.call("press_key", json!({"key": "a"})).await.unwrap();
        assert_eq!(
            daemon.await.unwrap(),
            [
                "start_session",
                "get_screen_size",
                "press_key",
                "start_session",
                "press_key"
            ]
        );
    }
}
