#![cfg(unix)]

use std::io::Read;
use std::os::unix::net::UnixListener;
use std::process::Command;

#[test]
fn a_desk_call_says_no_daemon_answers_only_when_none_took_the_connection() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("desk.sock");
    let desk = |socket: &std::path::Path| {
        let out = Command::new(env!("CARGO_BIN_EXE_shuttle"))
            .args(["desk", "windows"])
            .env("SHUTTLE_SOCKET", socket)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let none = desk(&socket);
    let listener = UnixListener::bind(&socket).unwrap();
    let daemon = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut len = [0u8; 4];
        conn.read_exact(&mut len).unwrap();
        let mut call = vec![0u8; u32::from_be_bytes(len) as usize];
        conn.read_exact(&mut call).unwrap();
    });
    let dropped = desk(&socket);
    daemon.join().unwrap();
    assert!(
        none.starts_with("shuttle: desk windows: no daemon answers: connect "),
        "{none}"
    );
    assert!(
        dropped.starts_with("shuttle: desk windows: no answer to desk_windows could be read")
            && !dropped.contains("no daemon answers"),
        "{dropped}"
    );
}
