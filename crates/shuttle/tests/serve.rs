#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

#[test]
fn a_call_during_the_teardown_is_answered_that_nothing_ran() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config/loomhand/shuttle");
    std::fs::create_dir_all(config.join("tools.d")).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "[android]\nadb = \"/nonexistent/adb\"\nui_jar = \"/u.jar\"\nscrcpy_jar = \"/s.jar\"\n",
    )
    .unwrap();
    let service = json!({
        "service": "slow",
        "bin": "/bin/sh",
        "args": ["-c", "trap '' TERM; while :; do sleep 1; done"],
        "tools": [{"name": "slow_echo", "description": "d", "input_schema": {"type": "object"}}],
    });
    std::fs::write(config.join("tools.d/slow.json"), service.to_string()).unwrap();
    let socket = home.path().join("desk.sock");
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_shuttle"))
        .arg("serve")
        .arg(&socket)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while ask(&socket).is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let up = ask(&socket);
    Command::new("/bin/sh")
        .args(["-c", &format!("kill -TERM {}", daemon.id())])
        .status()
        .unwrap();
    let mut during = None;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match ask(&socket) {
            Some(text) if text.starts_with("the daemon is stopping") => {
                during = Some(text);
                break;
            }
            Some(_) => std::thread::sleep(Duration::from_millis(10)),
            None => break,
        }
    }
    let after = during.as_ref().and_then(|_| ask(&socket));
    let deadline = Instant::now() + Duration::from_secs(10);
    while daemon.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = daemon.kill();
    let _ = daemon.wait();
    assert!(up.is_some(), "the daemon never answered");
    let stopping = "the daemon is stopping, so nothing ran: call again in a few seconds";
    assert_eq!(during.as_deref(), Some(stopping));
    assert_eq!(
        after.as_deref(),
        Some(stopping),
        "a call made after the shutdown began was not accepted"
    );
}

fn ask(socket: &Path) -> Option<String> {
    let mut conn = UnixStream::connect(socket).ok()?;
    conn.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
    let call = json!({"agent_id": "t", "tool": "desk_windows", "args": {}}).to_string();
    let len = u32::try_from(call.len()).ok()?.to_be_bytes();
    conn.write_all(&len).ok()?;
    conn.write_all(call.as_bytes()).ok()?;
    let mut len = [0u8; 4];
    conn.read_exact(&mut len).ok()?;
    let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
    conn.read_exact(&mut body).ok()?;
    let out: Value = serde_json::from_slice(&body).ok()?;
    out["text"].as_str().map(str::to_owned)
}
