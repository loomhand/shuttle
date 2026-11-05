//! shuttle: a local computer-use host that agents call over MCP, on stdio or HTTP, to drive this desktop.

pub mod android;
pub mod cli;
pub mod daemon;
pub mod driver;
#[cfg(any(target_os = "macos", windows, test))]
pub mod hotkey;
pub mod http;
#[cfg(windows)]
mod job;
#[cfg(target_os = "macos")]
pub mod mac;
pub mod mcp;
pub mod plugins;
pub mod proxy;
pub mod record;
pub mod runner;
pub mod services;
#[cfg(target_os = "macos")]
mod tap;
pub mod tools;
#[cfg(windows)]
pub mod win;

use std::fmt::Display;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
#[cfg(unix)]
use std::sync::atomic::Ordering;
#[cfg(unix)]
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use tokio::process::Child;

/// The program name in usage lines, endpoints and the MCP server info.
pub const NAME: &str = "shuttle";
/// The namespace of shuttle's directories and Windows global names, so another `shuttle` does not collide.
pub const VENDOR: &str = "loomhand";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The answer to every call while the user has stopped shuttle.
pub const STOPPED: &str = "stopped by the user: run `shuttle resume` to go on";
/// The environment variable whose value `1` tells `serve` and `mcp` that a host app owns the daemon, its driver and their permissions.
pub const EMBEDDED: &str = "SHUTTLE_EMBEDDED";

const FIRST_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(30);
#[cfg(unix)]
const GROUP_POLL: Duration = Duration::from_millis(20);

pub(crate) static ENDING: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
static GRACED: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// A child that leads its own process group, whose other members die with it.
pub(crate) struct Running {
    pub(crate) child: Child,
    #[cfg(unix)]
    group: Option<u32>,
    #[cfg(windows)]
    pub(crate) job: Option<std::sync::Arc<crate::job::Job>>,
}

impl Running {
    pub(crate) fn new(child: Child) -> Self {
        Self {
            #[cfg(unix)]
            group: child.id(),
            #[cfg(windows)]
            job: None,
            child,
        }
    }

    /// Like [`Running::new`]; on Windows the child also goes into a job that ends its whole tree with it, or with `alone` the child but not the apps it starts.
    pub(crate) fn jobbed(child: Child, alone: bool) -> Result<Self, String> {
        #[cfg(windows)]
        {
            let job = crate::job::Job::hold(child.raw_handle(), alone)?;
            Ok(Self {
                job: Some(std::sync::Arc::new(job)),
                ..Self::new(child)
            })
        }
        #[cfg(not(windows))]
        {
            let _ = alone;
            Ok(Self::new(child))
        }
    }
}

#[cfg(unix)]
impl Drop for Running {
    fn drop(&mut self) {
        if let Some(g) = self.group {
            crate::signal_group(g, libc::SIGKILL);
        }
    }
}

pub(crate) struct Backoff(Duration);

impl Default for Backoff {
    fn default() -> Self {
        Self(FIRST_DELAY)
    }
}

impl Backoff {
    pub(crate) fn after(&mut self, ran: Duration) -> Duration {
        if ran >= MAX_DELAY {
            self.0 = FIRST_DELAY;
        }
        let wait = self.0;
        self.0 = (self.0 * 2).min(MAX_DELAY);
        wait
    }
}

/// Shuttle's directory under a config or cache root: `loomhand/shuttle`.
pub fn home() -> PathBuf {
    Path::new(VENDOR).join(NAME)
}

pub fn embedded() -> bool {
    std::env::var(EMBEDDED).is_ok_and(|v| v == "1")
}

pub(crate) fn log(line: impl Display) {
    let _ = writeln!(std::io::stderr(), "{NAME}: {line}");
}

/// Sends `sig` to the live members of the group `group` leads that share this process's cgroup; how many it reached.
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

#[cfg(unix)]
pub(crate) fn kill_later(rt: &tokio::runtime::Handle, group: u32, grace: Duration) {
    {
        let mut graced = GRACED.lock().unwrap_or_else(PoisonError::into_inner);
        if ENDING.load(Ordering::Relaxed) {
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

#[cfg(all(test, unix))]
pub(crate) fn script(path: &Path, body: &str) -> std::io::Result<()> {
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
