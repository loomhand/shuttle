#![cfg(windows)]

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn the_daemon_an_mcp_session_starts_holds_none_of_its_stdio() {
    let home = tempfile::tempdir().unwrap();
    let pipe = format!("cocoon-shuttle-test-{}", std::process::id());
    let mut mcp = Command::new(env!("CARGO_BIN_EXE_cocoon-shuttle"))
        .arg("mcp")
        .env("SHUTTLE_SOCKET", format!(r"\\.\pipe\{pipe}"))
        .env("APPDATA", home.path())
        .env("USERPROFILE", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = mcp.stdin.take().unwrap();
    for line in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"desk","arguments":{"verb":"screen"}}}"#,
    ] {
        writeln!(stdin, "{line}").unwrap();
    }
    drop(stdin);
    let mut stdout = mcp.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        let _ = tx.send(out);
    });
    let out = rx.recv_timeout(Duration::from_secs(30));
    let status = mcp.wait().unwrap();
    let killed = Command::new("powershell")
        .args(["-NoProfile", "-Command"])
        .arg(format!(
            "(Get-CimInstance Win32_Process -Filter \"Name = 'cocoon-shuttle.exe' AND CommandLine LIKE '%{pipe}%'\" | Invoke-CimMethod -MethodName Terminate | Measure-Object).Count"
        ))
        .output()
        .unwrap();
    let out = out.expect("stdout closes when mcp exits, though the daemon it started still runs");
    assert!(status.success(), "mcp exited with {status}");
    assert_eq!(out.lines().count(), 2, "{out}");
    assert_eq!(
        String::from_utf8_lossy(&killed.stdout).trim(),
        "1",
        "one daemon ran"
    );
}
