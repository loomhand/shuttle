//! shuttle: a local computer-use host that agents call over MCP stdio to drive this desktop.

pub mod android;
pub mod cli;
pub mod daemon;
pub mod driver;
#[cfg(any(target_os = "macos", windows, test))]
pub mod hotkey;
#[cfg(windows)]
mod job;
#[cfg(target_os = "macos")]
pub mod mac;
pub mod mcp;
pub mod plugins;
pub mod proxy;
pub mod runner;
pub mod services;
pub mod tools;
#[cfg(windows)]
pub mod win;

use std::fmt::Display;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
#[cfg(unix)]
use std::sync::{Mutex, PoisonError};
#[cfg(unix)]
use std::time::Duration;

/// The program name in usage lines, endpoints and the MCP server info.
pub const NAME: &str = "shuttle";
/// The namespace of shuttle's directories and Windows global names, so another `shuttle` does not collide.
pub const VENDOR: &str = "loomhand";
/// The release version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The answer to every call while the user has stopped shuttle.
pub const STOPPED: &str = "stopped by the user: run `shuttle resume` to go on";

#[cfg(unix)]
const GROUP_POLL: Duration = Duration::from_millis(20);

pub(crate) static ENDING: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
static GRACED: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Shuttle's directory under a config or cache root: `loomhand/shuttle`.
pub fn home() -> PathBuf {
    Path::new(VENDOR).join(NAME)
}

pub(crate) fn log(line: impl Display) {
    let _ = writeln!(std::io::stderr(), "{NAME}: {line}");
}

/// Sends `sig` to the live processes of the group `group` leads that are still in this process's cgroup, sparing the apps the desk moved into scopes of their own; how many it reached, so signal 0 counts them.
#[cfg(target_os = "linux")]
pub(crate) fn signal_group(group: u32, sig: libc::c_int) -> usize {
    let cgroup = |p: &str| std::fs::read_to_string(format!("/proc/{p}/cgroup")).ok();
    let own = cgroup("self");
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return 0;
    };
    let mut reached = 0;
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(p) = name
            .to_str()
            .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        let in_group = std::fs::read_to_string(format!("/proc/{p}/stat")).is_ok_and(|stat| {
            let mut fields = stat
                .rsplit_once(')')
                .map(|(_, rest)| rest.split_whitespace())
                .into_iter()
                .flatten();
            fields.next() != Some("Z")
                && fields.nth(1).and_then(|g| g.parse::<u32>().ok()) == Some(group)
        });
        if in_group
            && cgroup(p) == own
            && let Ok(pid) = p.parse::<libc::pid_t>()
        {
            // SAFETY: kill takes no pointers; `pid` names one process of the group.
            if unsafe { libc::kill(pid, sig) } == 0 {
                reached += 1;
            }
        }
    }
    reached
}

/// Sends `sig` to the group `group` leads; 1 when it reached a process, so signal 0 tells whether one is left.
#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn signal_group(group: u32, sig: libc::c_int) -> usize {
    let Ok(pid) = libc::pid_t::try_from(group) else {
        return 0;
    };
    // SAFETY: kill takes no pointers; the negative pid names the process group the child leads.
    usize::from(unsafe { libc::kill(-pid, sig) } == 0)
}

/// Ends the group `group` leads: TERM, then KILL whatever is left after `grace`.
#[cfg(unix)]
pub(crate) async fn end_group(group: u32, grace: Duration) {
    if signal_group(group, libc::SIGTERM) == 0 {
        return;
    }
    kill_after(group, grace).await;
}

/// KILLs whatever is left of the group `group` leads once `grace` has passed, or nothing once the group is gone.
#[cfg(unix)]
async fn kill_after(group: u32, grace: Duration) {
    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(GROUP_POLL).await;
        if signal_group(group, 0) == 0 {
            return;
        }
    }
    signal_group(group, libc::SIGKILL);
}

#[cfg(unix)]
pub(crate) fn kill_later(rt: &tokio::runtime::Handle, group: u32, grace: Duration) {
    {
        let mut graced = GRACED.lock().unwrap_or_else(PoisonError::into_inner);
        if ENDING.load(std::sync::atomic::Ordering::Relaxed) {
            drop(graced);
            signal_group(group, libc::SIGKILL);
            return;
        }
        graced.push(group);
    }
    rt.spawn(async move {
        kill_after(group, grace).await;
        GRACED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|g| *g != group);
    });
}

#[cfg(unix)]
pub(crate) fn kill_graced() {
    let graced = std::mem::take(&mut *GRACED.lock().unwrap_or_else(PoisonError::into_inner));
    for group in graced {
        signal_group(group, libc::SIGKILL);
    }
}

#[cfg(unix)]
pub(crate) fn term(pid: u32) {
    if let Ok(pid) = libc::pid_t::try_from(pid) {
        // SAFETY: kill takes no pointers; `pid` names the child this daemon started.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
}

#[cfg(all(test, unix))]
pub(crate) fn script(path: &std::path::Path, body: &str) -> std::io::Result<()> {
    let made = std::process::Command::new("/bin/sh")
        .args(["-c", r#"printf '%s' "$1" > "$0" && chmod 755 "$0""#])
        .arg(path)
        .arg(body)
        .status()?;
    if made.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "write {}: {made}",
            path.display()
        )))
    }
}
