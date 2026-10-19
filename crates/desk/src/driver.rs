//! cua-driver client: one line of JSON each way over its daemon socket, a named pipe on Windows.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::Instant;

pub const WINDOW_SESSION: &str = "desk-window";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SESSION: &str = "desk";
const ENDED: &str = "session has ended";
const CLICK_SPACING: Duration = Duration::from_millis(500);
const CLICK_DISTANCE: f64 = 16.0;

/// The driver calls the desk makes: input, windows, launch and capture.
pub trait Input {
    /// Calls driver tool `tool` and returns its result.
    fn call(&self, tool: &str, args: Value) -> impl Future<Output = Result<Value, String>> + Send;
}

/// A client of the cua-driver daemon at `endpoint`, in a desktop session, or the window session a call names; on Linux each call runs in a session of its own.
#[derive(Debug)]
pub struct Driver {
    endpoint: String,
    started: [AtomicBool; 2],
    calls: AtomicU64,
    last_click: Mutex<Option<(Instant, f64, f64)>>,
}

impl Driver {
    pub fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            started: [AtomicBool::new(false), AtomicBool::new(false)],
            calls: AtomicU64::new(0),
            last_click: Mutex::new(None),
        }
    }

    /// The daemon's endpoint: `CUA_DRIVER_SOCKET`, else the driver's default for this OS.
    pub fn default_endpoint() -> String {
        crate::server::cache_endpoint(
            "CUA_DRIVER_SOCKET",
            "cua-driver",
            "cua-driver",
            "cua-driver.sock",
        )
    }

    async fn start(&self, window: bool) -> Result<(), String> {
        let (scope, session) = if window {
            ("window", WINDOW_SESSION)
        } else {
            ("desktop", SESSION)
        };
        self.send(
            "start_session",
            &json!({"capture_scope": scope, "session": session}),
        )
        .await?;
        self.started[usize::from(window)].store(true, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Waits out [`CLICK_SPACING`] after a click within [`CLICK_DISTANCE`] of this one, so two clicks never make a double click.
    pub(crate) async fn space_click(&self, (x, y): (f64, f64)) {
        let wait = self
            .last_click()
            .filter(|(_, lx, ly)| (x - lx).hypot(y - ly) <= CLICK_DISTANCE)
            .map(|(at, _, _)| CLICK_SPACING.saturating_sub(at.elapsed()));
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
        }
    }

    /// Records a click at `point`, which the next nearby click waits out.
    pub(crate) fn clicked(&self, (x, y): (f64, f64)) {
        *self.last_click() = Some((Instant::now(), x, y));
    }

    fn last_click(&self) -> std::sync::MutexGuard<'_, Option<(Instant, f64, f64)>> {
        self.last_click
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn send(&self, tool: &str, args: &Value) -> Result<Value, String> {
        let request = Request {
            method: "call",
            name: tool,
            args,
        };
        let mut line =
            serde_json::to_vec(&request).map_err(|e| format!("encode cua-driver {tool}: {e}"))?;
        line.push(b'\n');
        let reply = tokio::time::timeout(REQUEST_TIMEOUT, self.exchange(line))
            .await
            .map_err(|_| format!("cua-driver {tool} timed out"))??;
        result(tool, &reply)
    }

    async fn exchange(&self, line: Vec<u8>) -> Result<String, String> {
        let stream = crate::server::connect(&self.endpoint)
            .await
            .map_err(|e| format!("connect cua-driver {}: {e}", self.endpoint))?;
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
}

impl Input for Driver {
    async fn call(&self, tool: &str, mut args: Value) -> Result<Value, String> {
        if cfg!(target_os = "linux") {
            // the Linux driver keeps a session's last capture scale and a master keyboard that Chrome's core-keyboard input loses to
            let once = format!(
                "desk-{}-{}",
                std::process::id(),
                self.calls.fetch_add(1, Ordering::Relaxed)
            );
            args["session"] = once.as_str().into();
            let point = (tool == "click")
                .then(|| args["x"].as_f64().zip(args["y"].as_f64()))
                .flatten();
            if let Some(point) = point {
                self.space_click(point).await;
            }
            let done = self.send(tool, &args).await;
            if let Some(point) = point {
                self.clicked(point);
            }
            let _ = self.send("end_session", &json!({"session": once})).await;
            return done;
        }
        let window = args["session"] == WINDOW_SESSION;
        if !window {
            args["session"] = SESSION.into();
        }
        if !self.started[usize::from(window)].load(Ordering::Relaxed) {
            self.start(window).await?;
        }
        match self.send(tool, &args).await {
            Err(e) if e.contains(ENDED) => {
                self.start(window).await?;
                self.send(tool, &args).await
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

#[derive(Serialize)]
struct Request<'a> {
    method: &'static str,
    name: &'a str,
    args: &'a Value,
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
        return Err(match out["structuredContent"]["reason"].as_str() {
            Some(reason) => format!("{tool}: {reason}: {}", text_of(&out)),
            None => format!("{tool}: {}", text_of(&out)),
        });
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
        let unread = result(
            "click",
            r#"{"ok":true,"result":{"isError":true,"content":[{"type":"text","text":"No screenshot of window 491"}],"structuredContent":{"code":"screenshot_context_missing","reason":"window_never_read"}}}"#,
        );
        assert_eq!(
            unread.unwrap_err(),
            "click: window_never_read: No screenshot of window 491"
        );
        assert!(result("click", "").is_err());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn the_first_call_starts_a_desktop_session_and_an_ended_one_starts_again() {
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
                lines.push(serde_json::from_str::<Value>(&line).unwrap());
            }
            lines
        });
        let d = Driver::new(socket.to_string_lossy().into_owned());
        let got = d.call("get_screen_size", json!({})).await.unwrap();
        assert_eq!(got["structuredContent"]["width"], 1280);
        d.call("press_key", json!({"key": "a"})).await.unwrap();
        let lines = daemon.await.unwrap();
        assert!(lines.iter().all(|l| l["args"]["session"] == SESSION));
        assert_eq!(
            lines.iter().map(|l| &l["name"]).collect::<Vec<_>>(),
            [
                "start_session",
                "get_screen_size",
                "press_key",
                "start_session",
                "press_key"
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn on_linux_each_call_runs_in_a_session_of_its_own_and_ends_it() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("cua.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let ok = b"{\"ok\":true,\"result\":{\"structuredContent\":{\"width\":1280}}}\n";
        let daemon = tokio::spawn(async move {
            let mut lines = Vec::new();
            for _ in 0..4 {
                let (s, _) = listener.accept().await.unwrap();
                let mut s = BufReader::new(s);
                let mut line = String::new();
                s.read_line(&mut line).await.unwrap();
                s.get_mut().write_all(ok).await.unwrap();
                lines.push(serde_json::from_str::<Value>(&line).unwrap());
            }
            lines
        });
        let d = Driver::new(socket.to_string_lossy().into_owned());
        d.call("get_screen_size", json!({})).await.unwrap();
        d.call("press_key", json!({"key": "a"})).await.unwrap();
        let lines = daemon.await.unwrap();
        let names: Vec<&Value> = lines.iter().map(|l| &l["name"]).collect();
        assert_eq!(
            names,
            ["get_screen_size", "end_session", "press_key", "end_session"]
        );
        let sessions: Vec<&Value> = lines.iter().map(|l| &l["args"]["session"]).collect();
        assert_eq!(sessions[0], sessions[1]);
        assert_eq!(sessions[2], sessions[3]);
        assert_ne!(sessions[0], sessions[2]);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(start_paused = true)]
    async fn a_click_near_the_last_one_waits_out_the_spacing() {
        let d = Driver::new(String::new());
        *d.last_click() = Some((Instant::now(), 100.0, 100.0));
        let started = Instant::now();
        d.space_click((110.0, 100.0)).await;
        assert_eq!(started.elapsed(), CLICK_SPACING);
        let started = Instant::now();
        d.space_click((300.0, 100.0)).await;
        assert_eq!(started.elapsed(), Duration::ZERO);
    }
}
