//! shuttle: a local computer-use host that agents call over MCP stdio to drive this desktop.

pub mod android;
pub mod cli;
pub mod daemon;
pub mod driver;
#[cfg(any(target_os = "macos", windows, test))]
pub mod hotkey;
#[cfg(target_os = "macos")]
pub mod mac;
pub mod mcp;
pub mod plugins;
pub mod proxy;
pub mod runner;
pub mod tools;
#[cfg(windows)]
pub mod win;

use std::fmt::Display;
use std::io::Write as _;

/// The program name in usage lines, endpoints and the MCP server info.
pub const NAME: &str = "cocoon-shuttle";
/// The release version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The answer to every call while the user has stopped shuttle.
pub const STOPPED: &str = "stopped by the user: run `cocoon-shuttle resume` to go on";

pub(crate) fn log(line: impl Display) {
    let _ = writeln!(std::io::stderr(), "{NAME}: {line}");
}

/// Kills the processes of the group `pid` leads that are still in this process's cgroup, sparing the apps the desk moved into scopes of their own.
#[cfg(target_os = "linux")]
pub(crate) fn kill_unscoped(group: u32) {
    let cgroup = |p: &str| std::fs::read_to_string(format!("/proc/{p}/cgroup")).ok();
    let own = cgroup("self");
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(p) = name
            .to_str()
            .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        let in_group = std::fs::read_to_string(format!("/proc/{p}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().nth(2))
                .is_some_and(|g| g == group.to_string())
        });
        if in_group
            && cgroup(p) == own
            && let Ok(pid) = p.parse::<libc::pid_t>()
        {
            // SAFETY: kill takes no pointers; `pid` names one process of the group.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

#[cfg(unix)]
pub(crate) fn kill_group(pid: u32) {
    if let Ok(pid) = libc::pid_t::try_from(pid) {
        // SAFETY: kill takes no pointers; the negative pid names the process group the child leads.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
}
