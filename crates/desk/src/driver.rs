//! cua-driver client: one line of JSON each way over its daemon socket, a named pipe on Windows.

use std::fmt::Write as _;
use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::lock;
use crate::server::Conn;

pub const WINDOW_SESSION: &str = "desk-window";
pub const CAPTURE_SESSION: &str = "desk-capture";
pub(crate) const CLICK_SPACING: Duration = Duration::from_millis(500);
pub(crate) const CONNECT_RETRY: Duration = Duration::from_millis(250);
pub(crate) const ENDED: &str = "session has ended";
pub(crate) const ACTUAL_FRONT: &str = "; actual foreground window ";
pub(crate) const INPUT_TOOLS: [&str; 9] = [
    "click",
    "double_click",
    "right_click",
    "drag",
    "scroll",
    "move_cursor",
    "type_text",
    "press_key",
    "hotkey",
];

const DEFAULT_PIPE: &str = r"\\.\pipe\cua-driver";
const CONNECT_FAILED: &str = "connect cua-driver";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_WAIT: Duration = Duration::from_secs(10);
const SESSION: &str = "desk";
const CLICK_DISTANCE: f64 = 16.0;
const USER_IDLE: Duration = Duration::from_secs(1);
const OWN_INPUT_LAG: Duration = Duration::from_millis(600);
#[cfg(windows)]
const PER_TYPED_UNIT: Duration = Duration::from_secs(2);
#[cfg(not(windows))]
const PER_TYPED_UNIT: Duration = Duration::from_millis(40);
#[cfg(all(target_os = "macos", not(test)))]
const HID_SYSTEM_STATE: i32 = 1;
#[cfg(all(target_os = "macos", not(test)))]
const USER_EVENTS: [u32; 10] = [1, 3, 5, 6, 7, 10, 12, 22, 25, 27];

#[cfg(test)]
thread_local! {
    static USER_SINCE: std::cell::Cell<f64> = const { std::cell::Cell::new(f64::INFINITY) };
}

/// The driver calls the desk makes: input, windows, launch and capture.
pub trait Input {
    fn call(&self, tool: &str, args: Value) -> impl Future<Output = Result<Value, String>> + Send;
}

/// A client of the cua-driver daemon at `endpoint`, in a desktop session, or the window session a call names; on Linux each call runs in a session of its own, except window reads, whose session the next window input shares and ends.
#[derive(Debug)]
pub struct Driver {
    endpoint: String,
    started: [AtomicBool; 3],
    calls: AtomicU64,
    window_n: AtomicU64,
    last_click: Mutex<Option<(Instant, f64, f64)>>,
    last_input: std::sync::Mutex<Option<std::time::Instant>>,
}

impl Driver {
    pub fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            started: Default::default(),
            calls: AtomicU64::new(0),
            window_n: AtomicU64::new(0),
            last_click: Mutex::new(None),
            last_input: std::sync::Mutex::new(None),
        }
    }

    /// The daemon's endpoint: `CUA_DRIVER_SOCKET`, else the driver's default for this OS.
    pub fn default_endpoint() -> String {
        if cfg!(windows) {
            return std::env::var("CUA_DRIVER_SOCKET")
                .ok()
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| DEFAULT_PIPE.to_owned());
        }
        crate::server::cache_endpoint(
            "CUA_DRIVER_SOCKET",
            "cua-driver",
            "cua-driver",
            "cua-driver.sock",
        )
    }

    /// Runs `click` at `(x, y)` once [`CLICK_SPACING`] has passed since a click within [`CLICK_DISTANCE`] of it, so two clicks never make a double click.
    pub(crate) async fn spaced<T>(&self, (x, y): (f64, f64), click: impl Future<Output = T>) -> T {
        let mut last = self.last_click.lock().await;
        let wait = last
            .filter(|(_, lx, ly)| (x - lx).hypot(y - ly) <= CLICK_DISTANCE)
            .map(|(at, _, _)| CLICK_SPACING.saturating_sub(at.elapsed()));
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
        }
        *last = Some((Instant::now(), x, y));
        let done = click.await;
        *last = Some((Instant::now(), x, y));
        done
    }

    pub(crate) async fn send(&self, tool: &str, args: &Value) -> Result<Value, String> {
        let reply = self.line(tool, args, REQUEST_TIMEOUT).await?;
        result(tool, &reply)
    }

    /// The raw reply line to one call within `limit`, empty when the driver closed without one.
    pub(crate) async fn line(
        &self,
        tool: &str,
        args: &Value,
        limit: Duration,
    ) -> Result<String, String> {
        let request = Request {
            method: "call",
            name: tool,
            args,
        };
        let mut line =
            serde_json::to_vec(&request).map_err(|e| format!("encode cua-driver {tool}: {e}"))?;
        line.push(b'\n');
        tokio::time::timeout(call_limit(tool, args, limit), self.exchange(line))
            .await
            .map_err(|_| format!("cua-driver {tool} timed out"))?
    }

    pub(crate) fn admit(&self, tool: &str, args: &Value) -> Result<bool, String> {
        let foreground = INPUT_TOOLS.contains(&tool) && args["delivery_mode"] != "background";
        if foreground || tool == "bring_to_front" {
            let since = since_user_input();
            if user_busy(since, self.since_own_input()) {
                return Err(format!(
                    "{tool}: the user used the mouse or keyboard {since:.1} s ago, and foreground input would take them over; nothing was sent: try again once they stop"
                ));
            }
        }
        Ok(foreground)
    }

    pub(crate) fn stamp(&self) {
        *lock(&self.last_input) = Some(std::time::Instant::now());
    }

    async fn start(&self, kind: usize) -> Result<(), String> {
        let (scope, session) = match kind {
            1 => ("window", WINDOW_SESSION),
            2 => ("desktop", CAPTURE_SESSION),
            _ => ("desktop", SESSION),
        };
        self.send(
            "start_session",
            &json!({"capture_scope": scope, "session": session}),
        )
        .await?;
        self.started[kind].store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Connects to the daemon, waiting out a restart: the one each desktop session start triggers, or the supervisor's after the driver died.
    async fn connect(&self) -> Result<Conn, String> {
        let deadline = Instant::now() + CONNECT_WAIT;
        loop {
            match crate::server::connect(&self.endpoint).await {
                Ok(stream) => return Ok(stream),
                Err(e)
                    if matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound)
                        && Instant::now() < deadline =>
                {
                    tokio::time::sleep(CONNECT_RETRY).await;
                }
                Err(e) => return Err(format!("{CONNECT_FAILED} {}: {e}", self.endpoint)),
            }
        }
    }

    async fn exchange(&self, line: Vec<u8>) -> Result<String, String> {
        let mut stream = BufReader::new(self.connect().await?);
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

    fn since_own_input(&self) -> Option<Duration> {
        lock(&self.last_input).map(|at| at.elapsed())
    }

    async fn call_session(&self, tool: &str, mut args: Value) -> Result<Value, String> {
        let kind = match args["session"].as_str() {
            Some(WINDOW_SESSION) => 1,
            Some(CAPTURE_SESSION) => 2,
            _ => {
                args["session"] = SESSION.into();
                0
            }
        };
        if !self.started[kind].load(Ordering::Relaxed) {
            self.start(kind).await?;
        }
        match self.send(tool, &args).await {
            Err(e) if e.starts_with(&format!("{tool}: {ENDED}")) => {
                self.start(kind).await?;
                self.send(tool, &args).await
            }
            done => done,
        }
    }
}

impl Input for Driver {
    async fn call(&self, tool: &str, mut args: Value) -> Result<Value, String> {
        let point = (tool == "click")
            .then(|| args["x"].as_f64().zip(args["y"].as_f64()))
            .flatten();
        if cfg!(target_os = "linux") {
            let window = args["session"] == WINDOW_SESSION;
            let read = window && tool == "get_window_state";
            // a session keeps its capture scale and a master keyboard that Chrome's core input loses to
            let (kind, n) = match (window, read) {
                (true, true) => ("window-", self.window_n.load(Ordering::Relaxed)),
                (true, false) => ("window-", self.window_n.fetch_add(1, Ordering::Relaxed)),
                _ => ("", self.calls.fetch_add(1, Ordering::Relaxed)),
            };
            args["session"] = format!("desk-{}-{kind}{n}", std::process::id()).into();
            let done = match point {
                Some(point) => self.spaced(point, self.send(tool, &args)).await,
                None => self.send(tool, &args).await,
            };
            if !read && !done.as_ref().is_err_and(|e| e.starts_with(CONNECT_FAILED)) {
                let _ = self
                    .send("end_session", &json!({"session": args["session"]}))
                    .await;
            }
            return done;
        }
        let foreground = self.admit(tool, &args)?;
        let done = match point {
            Some(point) => self.spaced(point, self.call_session(tool, args)).await,
            None => self.call_session(tool, args).await,
        };
        if foreground {
            self.stamp();
        }
        done
    }
}

impl Input for std::sync::Arc<Driver> {
    fn call(&self, tool: &str, args: Value) -> impl Future<Output = Result<Value, String>> + Send {
        Driver::call(self, tool, args)
    }
}

#[derive(Serialize)]
struct Request<'a> {
    method: &'static str,
    name: &'a str,
    args: &'a Value,
}

/// `req` addressed to the screen, not to a window.
pub fn desktop(mut req: Value) -> Value {
    req["scope"] = "desktop".into();
    req
}

/// `req` delivered to window `window_id` of `pid` without raising it or moving the pointer.
pub fn background(mut req: Value, (pid, window_id): (i64, i64)) -> Value {
    req["target"] = json!({"kind": "window", "pid": pid, "window_id": window_id});
    req["delivery_mode"] = "background".into();
    req["session"] = WINDOW_SESSION.into();
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

/// The base64 data of a driver result's first image block.
pub fn image_of(v: &Value) -> Option<&str> {
    v["content"]
        .as_array()?
        .iter()
        .find(|c| c["type"] == "image")
        .and_then(|c| c["data"].as_str())
}

#[cfg(all(test, unix))]
pub(crate) fn user_input_since(seconds: f64) {
    USER_SINCE.with(|s| s.set(seconds));
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
        let mut e = match out["structuredContent"]["reason"].as_str() {
            Some(reason) => format!("{tool}: {reason}: {}", text_of(&out)),
            None => format!("{tool}: {}", text_of(&out)),
        };
        if let Some(id) = out["structuredContent"]["actual_foreground_window_id"].as_u64() {
            let _ = write!(e, "{ACTUAL_FRONT}{id}");
        }
        return Err(e);
    }
    Ok(out)
}

fn call_limit(tool: &str, args: &Value, limit: Duration) -> Duration {
    let units = args["text"]
        .as_str()
        .map_or(0, |t| t.encode_utf16().count());
    match tool {
        "type_text" => limit + PER_TYPED_UNIT * u32::try_from(units).unwrap_or(u32::MAX),
        _ => limit,
    }
}

/// Whether HID input `since` seconds ago is the user's: within [`USER_IDLE`], and over [`OWN_INPUT_LAG`] after the driver's own last input, whose events reach the HID counter that late.
fn user_busy(since: f64, own: Option<Duration>) -> bool {
    since < USER_IDLE.as_secs_f64()
        && own.is_none_or(|own| since < own.saturating_sub(OWN_INPUT_LAG).as_secs_f64())
}

#[cfg(all(target_os = "macos", not(test)))]
fn since_user_input() -> f64 {
    USER_EVENTS
        .iter()
        .map(|&kind| {
            // SAFETY: CGEventSourceSecondsSinceLastEventType reads a counter and takes two plain integers.
            unsafe { CGEventSourceSecondsSinceLastEventType(HID_SYSTEM_STATE, kind) }
        })
        .fold(f64::INFINITY, f64::min)
}

#[cfg(all(not(target_os = "macos"), not(test)))]
fn since_user_input() -> f64 {
    f64::INFINITY
}

#[cfg(test)]
fn since_user_input() -> f64 {
    USER_SINCE.with(std::cell::Cell::get)
}

#[cfg(all(target_os = "macos", not(test)))]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state: i32, kind: u32) -> f64;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_type_waits_its_driver_s_slowest_time_per_utf16_unit_and_other_calls_their_limit() {
        let text = json!({"text": "x".repeat(6000)});
        assert_eq!(
            call_limit("type_text", &text, REQUEST_TIMEOUT),
            REQUEST_TIMEOUT + PER_TYPED_UNIT * 6000
        );
        assert_eq!(
            call_limit(
                "type_text",
                &json!({"text": "a\u{4e2d}\u{1f600}"}),
                REQUEST_TIMEOUT
            ),
            REQUEST_TIMEOUT + PER_TYPED_UNIT * 4
        );
        assert_eq!(call_limit("click", &text, REQUEST_TIMEOUT), REQUEST_TIMEOUT);
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_type_waits_the_driver_s_two_second_stall_cap_per_unit() {
        let text = json!({"text": "x".repeat(1000)});
        assert_eq!(
            call_limit("type_text", &text, REQUEST_TIMEOUT),
            Duration::from_secs(2030)
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn elsewhere_a_type_waits_forty_ms_more_per_unit() {
        let text = json!({"text": "x".repeat(6000)});
        assert_eq!(
            call_limit("type_text", &text, REQUEST_TIMEOUT),
            Duration::from_secs(270)
        );
    }

    #[test]
    fn only_input_within_a_second_and_past_the_lag_of_the_drivers_own_is_the_users() {
        let past = OWN_INPUT_LAG + Duration::from_millis(100);
        assert!(user_busy(0.2, None));
        assert!(!user_busy(1.5, None));
        assert!(!user_busy(f64::INFINITY, None));
        assert!(!user_busy(0.2, Some(Duration::from_millis(100))));
        assert!(!user_busy(0.0, Some(Duration::from_millis(100))));
        assert!(!user_busy(0.0, Some(OWN_INPUT_LAG)));
        assert!(!user_busy(0.15, Some(past)));
        assert!(user_busy(0.05, Some(past)));
        assert!(!user_busy(1.2, Some(USER_IDLE + past)));
    }

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
        let refused_front = result(
            "bring_to_front",
            r#"{"ok":true,"result":{"isError":true,"content":[{"type":"text","text":"foreground activation failed"}],"structuredContent":{"code":"foreground_unavailable","actual_foreground_window_id":7}}}"#,
        );
        assert_eq!(
            refused_front.unwrap_err(),
            format!("bring_to_front: foreground activation failed{ACTUAL_FRONT}7")
        );
        assert!(result("click", "").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_call_waits_out_a_driver_restart_behind_its_stale_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("cua.sock");
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
        let restarted = socket.clone();
        let daemon = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            std::fs::remove_file(&restarted).unwrap();
            let listener = tokio::net::UnixListener::bind(&restarted).unwrap();
            let (s, _) = listener.accept().await.unwrap();
            let mut s = BufReader::new(s);
            let mut line = String::new();
            s.read_line(&mut line).await.unwrap();
            s.get_mut()
                .write_all(b"{\"ok\":true,\"result\":{\"width\":1280}}\n")
                .await
                .unwrap();
        });
        let d = Driver::new(socket.to_string_lossy().into_owned());
        let got = d.send("get_screen_size", &json!({})).await.unwrap();
        assert_eq!(got["width"], 1280);
        daemon.await.unwrap();
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

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_tool_error_that_only_mentions_an_ended_session_is_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("cua.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let ok = b"{\"ok\":true,\"result\":{}}\n";
        let failed = b"{\"ok\":true,\"result\":{\"isError\":true,\"content\":[{\"type\":\"text\",\"text\":\"no button named 'session has ended'\"}]}}\n";
        let daemon = tokio::spawn(async move {
            let mut lines = Vec::new();
            while let Ok(Ok((s, _))) =
                tokio::time::timeout(Duration::from_secs(1), listener.accept()).await
            {
                let mut s = BufReader::new(s);
                let mut line = String::new();
                s.read_line(&mut line).await.unwrap();
                let line = serde_json::from_str::<Value>(&line).unwrap();
                let reply: &[u8] = if line["name"] == "start_session" {
                    ok
                } else {
                    failed
                };
                s.get_mut().write_all(reply).await.unwrap();
                lines.push(line["name"].as_str().unwrap().to_owned());
            }
            lines
        });
        let d = Driver::new(socket.to_string_lossy().into_owned());
        let err = d.call("press_key", json!({"key": "a"})).await.unwrap_err();
        assert_eq!(err, "press_key: no button named 'session has ended'");
        drop(d);
        assert_eq!(daemon.await.unwrap(), ["start_session", "press_key"]);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_capture_runs_in_a_desktop_session_of_its_own_apart_from_input() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("cua.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let daemon = tokio::spawn(async move {
            let mut lines = Vec::new();
            for _ in 0..4 {
                let (s, _) = listener.accept().await.unwrap();
                let mut s = BufReader::new(s);
                let mut line = String::new();
                s.read_line(&mut line).await.unwrap();
                s.get_mut()
                    .write_all(b"{\"ok\":true,\"result\":{}}\n")
                    .await
                    .unwrap();
                lines.push(serde_json::from_str::<Value>(&line).unwrap());
            }
            lines
        });
        let d = Driver::new(socket.to_string_lossy().into_owned());
        d.call("get_desktop_state", json!({"session": CAPTURE_SESSION}))
            .await
            .unwrap();
        d.call("click", json!({"x": 10, "y": 10, "scope": "desktop"}))
            .await
            .unwrap();
        let lines = tokio::time::timeout(Duration::from_secs(5), daemon)
            .await
            .expect("the daemon got fewer calls than a capture and a click make")
            .unwrap();
        let got: Vec<_> = lines
            .iter()
            .map(|l| {
                (
                    l["name"].as_str().unwrap().to_owned(),
                    l["args"]["session"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let want = [
            ("start_session", CAPTURE_SESSION),
            ("get_desktop_state", CAPTURE_SESSION),
            ("start_session", SESSION),
            ("click", SESSION),
        ]
        .map(|(n, s)| (n.to_owned(), s.to_owned()));
        assert_eq!(got, want);
        assert_eq!(lines[0]["args"]["capture_scope"], "desktop");
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
    #[tokio::test]
    async fn on_linux_window_reads_share_the_session_of_the_next_window_input_which_ends_it() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("cua.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let daemon = tokio::spawn(async move {
            let (mut lines, mut ended) = (Vec::new(), std::collections::HashSet::new());
            for _ in 0..9 {
                let (s, _) = listener.accept().await.unwrap();
                let mut s = BufReader::new(s);
                let mut line = String::new();
                s.read_line(&mut line).await.unwrap();
                let l: Value = serde_json::from_str(&line).unwrap();
                let session = l["args"]["session"].to_string();
                let reply = if ended.contains(&session) {
                    "{\"ok\":false,\"error\":\"session has ended\"}\n"
                } else {
                    "{\"ok\":true,\"result\":{\"structuredContent\":{}}}\n"
                };
                if l["name"] == "end_session" {
                    ended.insert(session);
                }
                s.get_mut().write_all(reply.as_bytes()).await.unwrap();
                lines.push(l);
            }
            lines
        });
        let d = Driver::new(socket.to_string_lossy().into_owned());
        let read = json!({"session": WINDOW_SESSION, "pid": 4, "window_id": 9});
        let click =
            json!({"session": WINDOW_SESSION, "pid": 4, "window_id": 9, "x": 5.0, "y": 5.0});
        for _ in 0..2 {
            d.call("get_window_state", read.clone()).await.unwrap();
            d.call("click", click.clone()).await.unwrap();
        }
        d.call("get_window_state", read).await.unwrap();
        d.call("get_window_state", json!({"pid": 4, "window_id": 9}))
            .await
            .unwrap();
        let lines = daemon.await.unwrap();
        let calls: Vec<(&str, &str)> = lines
            .iter()
            .map(|l| {
                (
                    l["name"].as_str().unwrap(),
                    l["args"]["session"].as_str().unwrap(),
                )
            })
            .collect();
        let window = |n: u32| format!("desk-{}-window-{n}", std::process::id());
        let (w0, w1, w2) = (window(0), window(1), window(2));
        assert_eq!(
            calls[..7],
            [
                ("get_window_state", w0.as_str()),
                ("click", &w0),
                ("end_session", &w0),
                ("get_window_state", &w1),
                ("click", &w1),
                ("end_session", &w1),
                ("get_window_state", &w2),
            ]
        );
        assert_eq!(calls[7].0, "get_window_state");
        assert!(!calls[7].1.contains("window"), "{calls:?}");
        assert_eq!(calls[8], ("end_session", calls[7].1));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(start_paused = true)]
    async fn on_linux_a_call_that_never_reached_the_driver_ends_no_session() {
        let dir = tempfile::tempdir().unwrap();
        let d = Driver::new(dir.path().join("cua.sock").to_string_lossy().into_owned());
        let started = Instant::now();
        let err = d.call("press_key", json!({"key": "a"})).await.unwrap_err();
        assert!(err.starts_with(CONNECT_FAILED), "{err}");
        assert!(
            started.elapsed() < CONNECT_WAIT + CONNECT_RETRY,
            "{:?}",
            started.elapsed()
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn only_foreground_input_counts_as_the_drivers_own() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("cua.sock");
        let daemon = answering(&socket);
        let d = Driver::new(socket.to_string_lossy().into_owned());
        d.call(
            "click",
            json!({"x": 10, "y": 10, "delivery_mode": "background"}),
        )
        .await
        .unwrap();
        assert!(d.since_own_input().is_none());
        d.call("click", json!({"x": 10, "y": 10})).await.unwrap();
        assert!(d.since_own_input().is_some());
        drop(d);
        daemon.await.unwrap();
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn bring_to_front_refuses_while_the_user_is_busy_and_never_counts_as_input() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("cua.sock");
        let d = Driver::new(socket.to_string_lossy().into_owned());
        let front = json!({"pid": 4, "window_id": 1});
        user_input_since(0.2);
        let refused = d.call("bring_to_front", front.clone()).await.unwrap_err();
        assert!(
            refused.contains("the user used the mouse or keyboard"),
            "{refused}"
        );
        user_input_since(f64::INFINITY);
        let daemon = answering(&socket);
        d.call("bring_to_front", front).await.unwrap();
        assert!(d.since_own_input().is_none());
        drop(d);
        daemon.await.unwrap();
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_desk_click_near_the_last_one_waits_out_the_spacing_off_linux() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("cua.sock");
        let daemon = answering(&socket);
        let d = Driver::new(socket.to_string_lossy().into_owned());
        d.call("click", json!({"x": 10, "y": 10})).await.unwrap();
        let started = Instant::now();
        d.call("click", json!({"x": 12, "y": 10})).await.unwrap();
        assert!(
            started.elapsed() >= CLICK_SPACING,
            "{:?}",
            started.elapsed()
        );
        drop(d);
        daemon.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_click_near_the_last_one_waits_out_the_spacing() {
        let d = Driver::new(String::new());
        d.spaced((100.0, 100.0), async {}).await;
        let started = Instant::now();
        d.spaced((110.0, 100.0), async {}).await;
        assert_eq!(started.elapsed(), CLICK_SPACING);
        let started = Instant::now();
        d.spaced((300.0, 100.0), async {}).await;
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn a_click_dropped_while_it_runs_still_spaces_the_next_near_one() {
        let d = Driver::new(String::new());
        let dropped = Duration::from_millis(100);
        let _ = tokio::time::timeout(
            dropped,
            d.spaced((100.0, 100.0), std::future::pending::<()>()),
        )
        .await;
        let started = Instant::now();
        d.spaced((104.0, 100.0), async {}).await;
        assert_eq!(started.elapsed(), CLICK_SPACING - dropped);
    }

    #[tokio::test(start_paused = true)]
    async fn two_near_clicks_at_once_are_spaced_by_the_second_waiting_for_the_first() {
        let d = Driver::new(String::new());
        let started = Instant::now();
        let slow = async { tokio::time::sleep(Duration::from_millis(50)).await };
        tokio::join!(
            d.spaced((100.0, 100.0), slow),
            d.spaced((104.0, 100.0), async {})
        );
        assert_eq!(started.elapsed(), Duration::from_millis(50) + CLICK_SPACING);
    }

    #[cfg(target_os = "macos")]
    fn answering(socket: &std::path::Path) -> tokio::task::JoinHandle<Vec<Value>> {
        let listener = tokio::net::UnixListener::bind(socket).unwrap();
        tokio::spawn(async move {
            let mut lines = Vec::new();
            while let Ok(Ok((s, _))) =
                tokio::time::timeout(Duration::from_secs(1), listener.accept()).await
            {
                let mut s = BufReader::new(s);
                let mut line = String::new();
                s.read_line(&mut line).await.unwrap();
                s.get_mut()
                    .write_all(b"{\"ok\":true,\"result\":{}}\n")
                    .await
                    .unwrap();
                lines.push(serde_json::from_str::<Value>(&line).unwrap());
            }
            lines
        })
    }
}
