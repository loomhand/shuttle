//! cua-driver client: one line of JSON each way over its daemon socket, a named pipe on Windows.

use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::server::Conn;

pub const WINDOW_SESSION: &str = "desk-window";
pub const CAPTURE_SESSION: &str = "desk-capture";
pub(crate) const CLICK_SPACING: Duration = Duration::from_millis(500);
pub(crate) const CONNECT_RETRY: Duration = Duration::from_millis(250);

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_WAIT: Duration = Duration::from_secs(10);
const SESSION: &str = "desk";
const ENDED: &str = "session has ended";
const CLICK_DISTANCE: f64 = 16.0;
const INPUT_TOOLS: [&str; 9] = [
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
const USER_IDLE: Duration = Duration::from_secs(1);
const OWN_INPUT_LAG: Duration = Duration::from_millis(600);
#[cfg(all(target_os = "macos", not(test)))]
const HID_SYSTEM_STATE: i32 = 1;
#[cfg(all(target_os = "macos", not(test)))]
const USER_EVENTS: [u32; 10] = [1, 3, 5, 6, 7, 10, 12, 22, 25, 27];

/// The driver calls the desk makes: input, windows, launch and capture.
pub trait Input {
    /// Calls driver tool `tool` and returns its result.
    fn call(&self, tool: &str, args: Value) -> impl Future<Output = Result<Value, String>> + Send;
}

/// A client of the cua-driver daemon at `endpoint`, in a desktop session, or the window session a call names; on Linux each call runs in a session of its own.
#[derive(Debug)]
pub struct Driver {
    endpoint: String,
    started: [AtomicBool; 3],
    calls: AtomicU64,
    last_click: Mutex<Option<(Instant, f64, f64)>>,
    last_input: std::sync::Mutex<Option<std::time::Instant>>,
}

impl Driver {
    pub fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            started: [
                AtomicBool::new(false),
                AtomicBool::new(false),
                AtomicBool::new(false),
            ],
            calls: AtomicU64::new(0),
            last_click: Mutex::new(None),
            last_input: std::sync::Mutex::new(None),
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

    /// Runs `click` at `(x, y)` once [`CLICK_SPACING`] has passed since a click within [`CLICK_DISTANCE`] of it, and records it before the next click may check, so two clicks never make a double click.
    pub(crate) async fn spaced<T>(&self, (x, y): (f64, f64), click: impl Future<Output = T>) -> T {
        let mut last = self.last_click.lock().await;
        let wait = last
            .filter(|(_, lx, ly)| (x - lx).hypot(y - ly) <= CLICK_DISTANCE)
            .map(|(at, _, _)| CLICK_SPACING.saturating_sub(at.elapsed()));
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
        }
        let done = click.await;
        *last = Some((Instant::now(), x, y));
        done
    }

    pub(crate) async fn send(&self, tool: &str, args: &Value) -> Result<Value, String> {
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
    pub(crate) async fn connect(&self) -> Result<Conn, String> {
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
                Err(e) => return Err(format!("connect cua-driver {}: {e}", self.endpoint)),
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
        *self
            .last_input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(std::time::Instant::now());
    }

    fn since_own_input(&self) -> Option<Duration> {
        self.last_input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .map(|at| at.elapsed())
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
            Err(e) if e.contains(ENDED) => {
                self.start(kind).await?;
                self.send(tool, &args).await
            }
            done => done,
        }
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
            let done = match point {
                Some(point) => self.spaced(point, self.send(tool, &args)).await,
                None => self.send(tool, &args).await,
            };
            let _ = self.send("end_session", &json!({"session": once})).await;
            return done;
        }
        let foreground = self.admit(tool, &args)?;
        let done = self.call_session(tool, args).await;
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

/// Whether HID input `since` seconds ago is the user's: it is within [`USER_IDLE`] and more than [`OWN_INPUT_LAG`] after the end of the driver's own last input, whose events reach the HID counter up to that late.
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
thread_local! {
    static USER_SINCE: std::cell::Cell<f64> = const { std::cell::Cell::new(f64::INFINITY) };
}

#[cfg(test)]
fn since_user_input() -> f64 {
    USER_SINCE.with(std::cell::Cell::get)
}

#[cfg(all(test, unix))]
pub(crate) fn user_input_since(seconds: f64) {
    USER_SINCE.with(|s| s.set(seconds));
}

#[cfg(all(target_os = "macos", not(test)))]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state: i32, kind: u32) -> f64;
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

    #[cfg(target_os = "macos")]
    fn answering(socket: &std::path::Path) -> tokio::task::JoinHandle<Vec<Value>> {
        let listener = tokio::net::UnixListener::bind(socket).unwrap();
        tokio::spawn(async move {
            let mut lines = Vec::new();
            while let Ok(Ok((s, _))) =
                tokio::time::timeout(Duration::from_millis(500), listener.accept()).await
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
}
