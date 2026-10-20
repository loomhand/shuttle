//! The desk endpoint: a unix socket on macOS and Linux, a named pipe on Windows, one framed call per connection.

use std::sync::Arc;

use desk_core::wire::{Call, Output, read_body, read_frame, tool_of, write_frame};
use tokio::io::{AsyncRead, AsyncReadExt};
use zeroize::Zeroizing;

use crate::computer::Computer;
use crate::desk::COMPUTER_VERBS;
use crate::desk::Desk;
use crate::driver::{Driver, Input};
use crate::keys::Os;
use crate::model::Access;

#[cfg(unix)]
const BACKLOG: u32 = 1024;
#[cfg(target_os = "linux")]
const SUN_PATH: usize = 108;
#[cfg(all(unix, not(target_os = "linux")))]
const SUN_PATH: usize = 104;

const ALLOWLIST_REFUSED: &str = "the computer verbs see and click the whole screen, which an app allowlist does not bound: empty [safety] apps in config.toml to use them";
// a group may read and write the pipe but not create instances of it: FILE_GENERIC_READ and FILE_GENERIC_WRITE less FILE_CREATE_PIPE_INSTANCE
const GROUP_RIGHTS: &str = "0x12019b";
#[cfg(windows)]
const PIPE_BUSY: i32 = 231;
#[cfg(windows)]
const BUSY_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// One client connection to an endpoint.
#[cfg(unix)]
pub type Conn = tokio::net::UnixStream;
/// One client connection to an endpoint.
#[cfg(windows)]
pub type Conn = PipeClient;
/// One connection an endpoint accepted.
#[cfg(unix)]
pub type Accepted = tokio::net::UnixStream;
/// One connection an endpoint accepted.
#[cfg(windows)]
pub type Accepted = tokio::net::windows::named_pipe::NamedPipeServer;

/// The client end of a named pipe that cancels its pending I/O when dropped, so its server sees the hang-up at once: the runtime closes a pipe only after its writes finish.
#[cfg(windows)]
#[derive(Debug)]
pub struct PipeClient(tokio::net::windows::named_pipe::NamedPipeClient);

#[cfg(windows)]
impl AsyncRead for PipeClient {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

#[cfg(windows)]
impl tokio::io::AsyncWrite for PipeClient {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

#[cfg(windows)]
impl std::os::windows::io::AsRawHandle for PipeClient {
    fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
        self.0.as_raw_handle()
    }
}

#[cfg(windows)]
impl Drop for PipeClient {
    fn drop(&mut self) {
        use std::os::windows::io::AsRawHandle as _;
        // SAFETY: the handle stays open until the inner pipe drops after this; no OVERLAPPED is named, so every pending call on it is cancelled.
        let _ = unsafe {
            windows::Win32::System::IO::CancelIoEx(
                windows::Win32::Foundation::HANDLE(self.0.as_raw_handle()),
                None,
            )
        };
    }
}

/// What answers the calls of an endpoint.
pub trait Handler: Send + Sync + 'static {
    /// Answers `call`; a failure is an error output, never a dropped connection.
    fn answer(&self, call: Call) -> impl Future<Output = Output> + Send;

    /// Whether some tool takes its whole connection through [`Handler::splice`], which costs every call a read of its tool.
    fn splicing(&self) -> bool {
        false
    }

    /// Whether a call of `tool` takes its whole connection through [`Handler::splice`] instead of one answer.
    fn splices(&self, _tool: &str) -> bool {
        false
    }

    /// Serves the connection of a call of `tool` that [`Handler::splices`] names; `body` is the call's frame body as read.
    fn splice(
        &self,
        _tool: &str,
        _body: Zeroizing<Vec<u8>>,
        _stream: Accepted,
    ) -> impl Future<Output = ()> + Send {
        async {}
    }

    /// Whether `tool` answers only a caller that runs as this process's user.
    fn owner_only(&self, _tool: &str) -> bool {
        false
    }
}

/// A connection, which can say whether its peer runs as this process's user.
trait Peer {
    fn is_owner(&self) -> bool;
}

#[cfg(unix)]
impl Peer for tokio::net::UnixStream {
    fn is_owner(&self) -> bool {
        // SAFETY: geteuid takes no arguments and always succeeds.
        let own = unsafe { libc::geteuid() };
        self.peer_cred().is_ok_and(|c| c.uid() == own)
    }
}

#[cfg(windows)]
impl Peer for tokio::net::windows::named_pipe::NamedPipeServer {
    fn is_owner(&self) -> bool {
        use std::os::windows::io::AsRawHandle;
        crate::pipesec::client_is_owner(windows::Win32::Foundation::HANDLE(self.as_raw_handle()))
    }
}

impl<B: Access + 'static, D: Input + Send + Sync + 'static> Handler for Desk<B, D> {
    async fn answer(&self, call: Call) -> Output {
        self.run(&call.tool, call.args)
            .await
            .unwrap_or_else(Output::error)
    }
}

/// The handler of a host without a desk backend.
pub struct NoDesk;

impl Handler for NoDesk {
    async fn answer(&self, call: Call) -> Output {
        Output::error(format!("{}: no desk backend for this OS yet", call.tool))
    }
}

/// An endpoint this daemon holds: the unix socket with the lock file beside it that keeps a second daemon out, or the first instance of a Windows pipe.
pub struct Listener {
    #[cfg(unix)]
    socket: tokio::net::UnixListener,
    #[cfg(unix)]
    _lock: std::fs::File,
    #[cfg(windows)]
    first: tokio::net::windows::named_pipe::NamedPipeServer,
    #[cfg(windows)]
    security: crate::pipesec::Security,
    endpoint: String,
}

/// Takes the unix socket `endpoint`, readable by its user only, once no other daemon holds its lock.
#[cfg(unix)]
pub async fn bind(endpoint: &str) -> Result<Listener, String> {
    bind_at(endpoint, None)
}

/// Takes the unix socket `endpoint` like [`bind`], open to the members of `group`, a group name or number: mode 0660, owned by that group. Its directory must belong to this user and be writable by no one else.
#[cfg(unix)]
pub async fn bind_for(endpoint: &str, group: &str) -> Result<Listener, String> {
    bind_at(endpoint, Some(gid(group)?))
}

#[cfg(unix)]
fn bind_at(endpoint: &str, gid: Option<u32>) -> Result<Listener, String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let path = std::path::Path::new(endpoint);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        if gid.is_some() {
            private_dir(dir)?;
        }
    }
    let lock_path = format!("{endpoint}.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|e| format!("open {lock_path}: {e}"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(format!("another daemon serves {endpoint}"));
        }
        Err(std::fs::TryLockError::Error(e)) => return Err(format!("lock {lock_path}: {e}")),
    }
    let staged = format!("{endpoint}.n");
    if staged.len() >= SUN_PATH {
        return Err(format!(
            "{endpoint}: a unix socket path holds at most {} bytes; set SHUTTLE_SOCKET to a shorter path",
            SUN_PATH - 3
        ));
    }
    let _ = std::fs::remove_file(&staged);
    let unbound = tokio::net::UnixSocket::new_stream().map_err(|e| format!("socket: {e}"))?;
    unbound
        .bind(&staged)
        .map_err(|e| format!("bind {staged}: {e}"))?;
    if let Some(gid) = gid {
        std::os::unix::fs::lchown(&staged, None, Some(gid))
            .map_err(|e| format!("give {staged} to group {gid}: {e}"))?;
    }
    socket_at(&staged)?;
    let mode = if gid.is_some() { 0o660 } else { 0o600 };
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(mode))
        .map_err(|e| format!("chmod {staged}: {e}"))?;
    let socket = unbound
        .listen(BACKLOG)
        .map_err(|e| format!("listen {staged}: {e}"))?;
    std::fs::rename(&staged, path).map_err(|e| format!("move {staged} to {endpoint}: {e}"))?;
    Ok(Listener {
        socket,
        _lock: lock,
        endpoint: endpoint.to_owned(),
    })
}

/// Refuses a socket directory that is not this user's or that its group or others can write, a link anywhere on its path, and any directory above it owned by another user or writable by others without the sticky bit: from any of them a member could swap the socket.
#[cfg(unix)]
fn private_dir(dir: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: geteuid takes no arguments and always succeeds.
    let own = unsafe { libc::geteuid() };
    private_path(dir, own, |d| {
        std::fs::symlink_metadata(d).map(|m| Entry {
            uid: m.uid(),
            mode: m.mode(),
            link: m.file_type().is_symlink(),
        })
    })
}

/// One path component as `lstat` sees it.
#[cfg(unix)]
struct Entry {
    uid: u32,
    mode: u32,
    link: bool,
}

/// [`private_dir`] for the user `own`, with `stat` reading each component of `dir` as given, links included.
#[cfg(unix)]
fn private_path(
    dir: &std::path::Path,
    own: u32,
    stat: impl Fn(&std::path::Path) -> std::io::Result<Entry>,
) -> Result<(), String> {
    if !dir.is_absolute() {
        return Err(format!(
            "socket_group needs an absolute endpoint; {} is relative",
            dir.display()
        ));
    }
    for (i, d) in dir.ancestors().enumerate() {
        let e = stat(d).map_err(|e| format!("stat {}: {e}", d.display()))?;
        if e.link {
            return Err(format!(
                "socket_group needs a path without links; {} is a link",
                d.display()
            ));
        }
        let writable = e.mode & 0o022 != 0;
        if i == 0 && (e.uid != own || writable) {
            return Err(format!(
                "socket_group needs {} owned by this user (uid {own}) and writable by no one else; it is uid {} mode {:o}",
                d.display(),
                e.uid,
                e.mode & 0o7777
            ));
        }
        if i > 0 && ((e.uid != 0 && e.uid != own) || (writable && e.mode & 0o1000 == 0)) {
            return Err(format!(
                "socket_group needs every directory above {} owned by root or this user and writable by no one else unless sticky; {} is uid {} mode {:o}",
                dir.display(),
                d.display(),
                e.uid,
                e.mode & 0o7777
            ));
        }
    }
    Ok(())
}

/// Refuses `path` unless it is a socket itself, not a link to one.
#[cfg(unix)]
fn socket_at(path: &str) -> Result<(), String> {
    use std::os::unix::fs::FileTypeExt;

    let kind = std::fs::symlink_metadata(path)
        .map_err(|e| format!("stat {path}: {e}"))?
        .file_type();
    if !kind.is_socket() {
        return Err(format!("{path} is not the socket just bound"));
    }
    Ok(())
}

/// Serves `handler` at `listener`.
#[cfg(unix)]
pub async fn serve<H: Handler>(handler: Arc<H>, listener: Listener) -> Result<(), String> {
    loop {
        let (stream, _) = listener
            .socket
            .accept()
            .await
            .map_err(|e| format!("accept {}: {e}", listener.endpoint))?;
        tokio::spawn(handle(handler.clone(), stream));
    }
}

/// Takes the named pipe `endpoint`, open to SYSTEM and this user, local clients only, once no other daemon holds it.
#[cfg(windows)]
pub async fn bind(endpoint: &str) -> Result<Listener, String> {
    bind_at(endpoint, None)
}

/// Takes the named pipe `endpoint` like [`bind`], open also to `group`, an account name or SID string, to read and write but not to create instances.
#[cfg(windows)]
pub async fn bind_for(endpoint: &str, group: &str) -> Result<Listener, String> {
    bind_at(endpoint, Some(group))
}

#[cfg(windows)]
fn bind_at(endpoint: &str, group: Option<&str>) -> Result<Listener, String> {
    let security = crate::pipesec::Security::new(group)?;
    Ok(Listener {
        first: pipe(endpoint, true, &security)?,
        security,
        endpoint: endpoint.to_owned(),
    })
}

/// Serves `handler` at `listener`.
#[cfg(windows)]
pub async fn serve<H: Handler>(handler: Arc<H>, listener: Listener) -> Result<(), String> {
    let Listener {
        first: mut server,
        security,
        endpoint,
    } = listener;
    loop {
        server
            .connect()
            .await
            .map_err(|e| format!("accept {endpoint}: {e}"))?;
        let client = std::mem::replace(&mut server, pipe(&endpoint, false, &security)?);
        tokio::spawn(handle(handler.clone(), client));
    }
}

/// The SDDL of the daemon's pipe: full access for SYSTEM and the SID `user`, and read and write for the SID `group`.
pub fn pipe_sddl(user: &str, group: Option<&str>) -> String {
    let mut sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{user})");
    if let Some(group) = group {
        sddl.push_str(&format!("(A;;{GROUP_RIGHTS};;;{group})"));
    }
    sddl
}

/// Sends one call to the endpoint and returns its output.
pub async fn call(endpoint: &str, call: &Call) -> Result<Output, String> {
    let conn = connect(endpoint)
        .await
        .map_err(|e| format!("connect {endpoint}: {e}"))?;
    exchange(conn, call).await
}

pub fn cache_endpoint(var: &str, pipe: &str, dir: &str, file: &str) -> String {
    if let Some(e) = std::env::var(var).ok().filter(|e| !e.is_empty()) {
        return e;
    }
    if cfg!(windows) {
        return format!(r"\\.\pipe\{pipe}");
    }
    let home = std::env::var("HOME").unwrap_or_default();
    if cfg!(target_os = "linux") {
        let cache = std::env::var("XDG_CACHE_HOME")
            .ok()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| format!("{home}/.cache"));
        return format!("{cache}/{dir}/{file}");
    }
    format!("{home}/Library/Caches/{dir}/{file}")
}

#[cfg(unix)]
fn gid(group: &str) -> Result<u32, String> {
    if let Ok(n) = group.parse() {
        return Ok(n);
    }
    let name = std::ffi::CString::new(group).map_err(|_| format!("group {group:?}: not a name"))?;
    let mut buf = vec![0; 16 << 10];
    loop {
        // SAFETY: a zeroed `group` is plain integers and null pointers, which getgrnam_r overwrites.
        let mut entry: libc::group = unsafe { std::mem::zeroed() };
        let mut found = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call, and `buf.len()` is the length of `buf`.
        let rc = unsafe {
            libc::getgrnam_r(
                name.as_ptr(),
                &raw mut entry,
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut found,
            )
        };
        match rc {
            0 if found.is_null() => return Err(format!("no group {group}")),
            0 => return Ok(entry.gr_gid),
            libc::ERANGE if buf.len() < 1 << 20 => buf.resize(buf.len() * 2, 0),
            e => {
                return Err(format!(
                    "look up group {group}: {}",
                    std::io::Error::from_raw_os_error(e)
                ));
            }
        }
    }
}

/// Connects to the endpoint.
#[cfg(unix)]
pub async fn connect(endpoint: &str) -> std::io::Result<Conn> {
    tokio::net::UnixStream::connect(endpoint).await
}

/// Connects to the endpoint, waiting while a Windows pipe is busy.
#[cfg(windows)]
pub async fn connect(endpoint: &str) -> std::io::Result<Conn> {
    loop {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(endpoint) {
            Err(e) if e.raw_os_error() == Some(PIPE_BUSY) => tokio::time::sleep(BUSY_POLL).await,
            done => return done.map(PipeClient),
        }
    }
}

/// Sends `call` on `conn` and reads its output.
pub async fn exchange(mut conn: Conn, call: &Call) -> Result<Output, String> {
    write_frame(&mut conn, call).await?;
    read_frame(&mut conn).await
}

/// This OS's desk: its accessibility tree and the computer verbs, with input through the cua-driver at `driver`, limited to the windows of `apps` unless empty.
#[cfg(target_os = "macos")]
pub fn native(driver: String, apps: &[String], _overlay: bool, background: bool) -> impl Handler {
    let driver = Arc::new(Driver::new(driver));
    Native {
        desk: Desk::new(crate::ax::Ax::open, Arc::clone(&driver), Os::Mac)
            .allow(apps)
            .background(background),
        computer: Computer::new(driver, Os::Mac).background(background),
        open: apps.is_empty(),
    }
}

/// This OS's desk: its accessibility tree and the computer verbs, with input through the cua-driver at `driver`, limited to the windows of `apps` unless empty.
#[cfg(windows)]
pub fn native(driver: String, apps: &[String], _overlay: bool, background: bool) -> impl Handler {
    let driver = Arc::new(Driver::new(driver));
    Native {
        desk: Desk::new(crate::uia::Uia::open, Arc::clone(&driver), Os::Windows)
            .allow(apps)
            .background(background),
        computer: Computer::new(driver, Os::Windows).background(background),
        open: apps.is_empty(),
    }
}

/// This OS's desk: its accessibility tree and the computer verbs, with input through the cua-driver at `driver`, limited to the windows of `apps` unless empty; `overlay` says whether that driver may show its cursor overlay.
#[cfg(target_os = "linux")]
pub fn native(driver: String, apps: &[String], overlay: bool, background: bool) -> impl Handler {
    let driver = Arc::new(Driver::new(driver));
    Native {
        desk: Desk::new(crate::spi::Spi::open, Arc::clone(&driver), Os::Linux)
            .allow(apps)
            .overlay(overlay)
            .background(background),
        computer: Computer::new(driver, Os::Linux).background(background),
        open: apps.is_empty(),
    }
}

/// A desk with the computer verbs: the desk verbs and, with no app allowlist, the computer verbs.
struct Native<B> {
    desk: Desk<B, Arc<Driver>>,
    computer: Computer,
    open: bool,
}

impl<B: Access + 'static> Handler for Native<B> {
    async fn answer(&self, call: Call) -> Output {
        let tool = call.tool.as_str();
        if !COMPUTER_VERBS.contains(&tool) {
            return self.desk.answer(call).await;
        }
        if !self.open {
            return Output::error(format!("{tool}: {ALLOWLIST_REFUSED}"));
        }
        self.computer
            .run(&call.agent_id, tool, call.args)
            .await
            .unwrap_or_else(Output::error)
    }
}

/// This OS's desk: none yet.
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn native(_: String, _: &[String], _: bool, _: bool) -> impl Handler {
    NoDesk
}

#[cfg(windows)]
fn pipe(
    endpoint: &str,
    first: bool,
    security: &crate::pipesec::Security,
) -> Result<tokio::net::windows::named_pipe::NamedPipeServer, String> {
    let mut options = tokio::net::windows::named_pipe::ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true);
    // SAFETY: the attributes point into `security`, which outlives the call.
    unsafe { options.create_with_security_attributes_raw(endpoint, security.attributes()) }
        .map_err(|e| format!("create pipe {endpoint}: {e}"))
}

async fn handle<H: Handler>(handler: Arc<H>, mut stream: Accepted) {
    let call = match read_body(&mut stream).await {
        Ok(body) => {
            if handler.splicing()
                && let Some(tool) = tool_of(&body).filter(|t| handler.splices(t))
            {
                let tool = tool.into_owned();
                return handler.splice(&tool, body, stream).await;
            }
            serde_json::from_slice::<Call>(&body).map_err(|e| format!("decode frame: {e}"))
        }
        Err(e) => Err(e),
    };
    let out = match call {
        Ok(call) => match refusal(handler.as_ref(), &call.tool, &stream) {
            Some(refused) => refused,
            None => {
                let (mut rx, tx) = tokio::io::split(stream);
                tokio::select! {
                    out = handler.answer(call) => {
                        stream = rx.unsplit(tx);
                        out
                    }
                    () = closed(&mut rx) => return,
                }
            }
        },
        Err(e) => Output::error(e),
    };
    let _ = write_frame(&mut stream, &out).await;
}

/// The refusal of a call of `tool` that only this process's user may send, when `peer` runs as another user.
fn refusal<H: Handler, P: Peer>(handler: &H, tool: &str, peer: &P) -> Option<Output> {
    (handler.owner_only(tool) && !peer.is_owner())
        .then(|| Output::error(format!("{tool}: only the daemon's own user can send it")))
}

/// Returns once the client closes its side of the connection; bytes it sends after its call are ignored.
async fn closed<R: AsyncRead + Unpin>(rx: &mut R) {
    let mut buf = [0; 64];
    while let Ok(n) = rx.read(&mut buf).await {
        if n == 0 {
            return;
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::desk::tests::Stub;
    use crate::keys::Os;

    struct NoDriver;

    impl Input for NoDriver {
        async fn call(&self, tool: &str, _: Value) -> Result<Value, String> {
            Err(format!("{tool}: no driver"))
        }
    }

    async fn answer(endpoint: &str, ask: &Call) -> Result<Output, String> {
        let mut out = Err(String::new());
        for _ in 0..50 {
            out = call(endpoint, ask).await;
            if out.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        out
    }

    #[tokio::test]
    async fn a_daemon_still_starting_keeps_a_second_one_out_before_its_socket_exists() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let starting = std::fs::File::create(format!("{endpoint}.lock")).unwrap();
        starting.lock().unwrap();
        assert_eq!(
            bind(&endpoint).await.err(),
            Some(format!("another daemon serves {endpoint}"))
        );
        assert!(!std::path::Path::new(&endpoint).exists());
    }

    #[tokio::test]
    async fn a_shared_socket_is_0660_and_owned_by_the_group_and_a_private_one_0600() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let mode = |p: &str| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let private = tempfile::tempdir().unwrap();
        let endpoint = private
            .path()
            .join("desk.sock")
            .to_string_lossy()
            .into_owned();
        let _listener = bind(&endpoint).await.unwrap();
        assert_eq!(mode(&endpoint), 0o600);
        let shared = tempfile::tempdir().unwrap();
        let endpoint = real(&shared)
            .unwrap()
            .join("desk.sock")
            .to_string_lossy()
            .into_owned();
        let own = std::fs::metadata(shared.path()).unwrap().gid();
        let _listener = bind_for(&endpoint, &own.to_string()).await.unwrap();
        assert_eq!(mode(&endpoint), 0o660);
        assert_eq!(std::fs::metadata(&endpoint).unwrap().gid(), own);
        assert_eq!(mode(&format!("{endpoint}.lock")), 0o600);
        assert!(!std::path::Path::new(&format!("{endpoint}.n")).exists());
    }

    #[tokio::test]
    async fn a_shared_socket_in_a_directory_its_group_can_write_is_refused() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o770)).unwrap();
        let endpoint = real(&dir)
            .unwrap()
            .join("desk.sock")
            .to_string_lossy()
            .into_owned();
        let own = std::fs::metadata(dir.path()).unwrap().gid();
        let err = bind_for(&endpoint, &own.to_string()).await.err().unwrap();
        assert!(
            err.contains("writable by no one else") && err.contains("mode 770"),
            "{err}"
        );
        assert!(!std::path::Path::new(&endpoint).exists());
    }

    #[tokio::test]
    async fn a_shared_socket_under_a_writable_parent_is_refused_unless_the_parent_is_sticky() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let top = tempfile::tempdir().unwrap();
        let parent = real(&top).unwrap().join("shared");
        let dir = parent.join("sock");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o750)).unwrap();
        let own = std::fs::metadata(&dir).unwrap().gid().to_string();
        let endpoint = dir.join("desk.sock").to_string_lossy().into_owned();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o770)).unwrap();
        let err = bind_for(&endpoint, &own).await.err().unwrap();
        assert!(
            err.contains("every directory above") && err.contains("mode 770"),
            "{err}"
        );
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o1777)).unwrap();
        let _listener = bind_for(&endpoint, &own).await.unwrap();
    }

    fn real(dir: &tempfile::TempDir) -> std::io::Result<std::path::PathBuf> {
        std::fs::canonicalize(dir.path())
    }

    #[tokio::test]
    async fn a_shared_socket_with_a_link_in_its_path_is_refused() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let top = tempfile::tempdir().unwrap();
        let target = real(&top).unwrap().join("target");
        std::fs::create_dir_all(target.join("sock")).unwrap();
        std::fs::set_permissions(target.join("sock"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let own = std::fs::metadata(&target).unwrap().gid().to_string();
        let link = real(&top).unwrap().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let below = link
            .join("sock")
            .join("desk.sock")
            .to_string_lossy()
            .into_owned();
        let err = bind_for(&below, &own).await.err().unwrap();
        assert!(err.contains("is a link") && err.contains("link"), "{err}");
        let leaf = real(&top).unwrap().join("leaf");
        std::os::unix::fs::symlink(target.join("sock"), &leaf).unwrap();
        let at = leaf.join("desk.sock").to_string_lossy().into_owned();
        let err = bind_for(&at, &own).await.err().unwrap();
        assert!(err.contains("is a link"), "{err}");
    }

    #[test]
    fn a_directory_above_owned_by_another_user_is_refused() {
        let tree = |other: u32| {
            move |p: &std::path::Path| {
                let (uid, mode) = match p.to_str().unwrap() {
                    "/" | "/home" => (0, 0o755),
                    "/home/other" => (other, 0o755),
                    _ => (1000, 0o700),
                };
                Ok(Entry {
                    uid,
                    mode,
                    link: false,
                })
            }
        };
        let dir = std::path::Path::new("/home/other/sock");
        let err = private_path(dir, 1000, tree(1001)).unwrap_err();
        assert!(err.contains("/home/other is uid 1001"), "{err}");
        assert_eq!(private_path(dir, 1000, tree(1000)), Ok(()));
        assert!(private_path(std::path::Path::new("run/x"), 1000, tree(1000)).is_err());
    }

    #[tokio::test]
    async fn an_endpoint_too_long_for_a_unix_socket_names_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir
            .path()
            .join("d".repeat(SUN_PATH))
            .to_string_lossy()
            .into_owned();
        let err = bind(&endpoint).await.err().unwrap();
        assert!(
            err.contains(&format!("at most {} bytes", SUN_PATH - 3)),
            "{err}"
        );
    }

    #[test]
    fn a_link_where_the_socket_was_bound_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        std::fs::write(&target, "").unwrap();
        let link = dir.path().join("desk.sock.n");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = socket_at(&link.to_string_lossy()).unwrap_err();
        assert!(err.contains("is not the socket just bound"), "{err}");
    }

    struct Guarded;

    impl Handler for Guarded {
        async fn answer(&self, call: Call) -> Output {
            Output::text(format!("{} answered", call.tool))
        }

        fn owner_only(&self, tool: &str) -> bool {
            tool == "halt"
        }
    }

    struct Foreign;

    impl Peer for Foreign {
        fn is_owner(&self) -> bool {
            false
        }
    }

    #[test]
    fn an_owner_only_call_from_another_user_is_refused_and_the_rest_answered() {
        let refused = refusal(&Guarded, "halt", &Foreign).unwrap();
        assert!(refused.is_error);
        assert_eq!(refused.text, "halt: only the daemon's own user can send it");
        assert!(refusal(&Guarded, "look", &Foreign).is_none());
    }

    #[tokio::test]
    async fn an_owner_only_call_over_the_socket_from_this_user_is_answered() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let listener = bind(&endpoint).await.unwrap();
        let server = tokio::spawn(serve(Arc::new(Guarded), listener));
        let out = answer(
            &endpoint,
            &Call {
                agent_id: String::new(),
                tool: "halt".into(),
                args: Value::Null,
            },
        )
        .await
        .unwrap();
        assert_eq!((out.is_error, out.text.as_str()), (false, "halt answered"));
        server.abort();
    }

    struct Endless(Arc<std::sync::atomic::AtomicBool>);

    struct Dropped(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl Handler for Endless {
        async fn answer(&self, _: Call) -> Output {
            let _dropped = Dropped(self.0.clone());
            std::future::pending::<()>().await;
            Output::text(String::new())
        }
    }

    struct Spliced;

    impl Handler for Spliced {
        async fn answer(&self, _: Call) -> Output {
            std::future::pending::<Output>().await
        }

        fn splicing(&self) -> bool {
            true
        }

        fn splices(&self, tool: &str) -> bool {
            tool == "svc"
        }

        async fn splice(&self, _: &str, _: Zeroizing<Vec<u8>>, mut stream: Accepted) {
            use tokio::io::AsyncWriteExt;

            let mut upload = Vec::new();
            let _ = stream.read_to_end(&mut upload).await;
            let _ = stream.write_all(&upload).await;
        }
    }

    #[tokio::test]
    async fn a_spliced_call_takes_a_client_half_close_as_the_end_of_its_upload_not_a_cancel() {
        use tokio::io::AsyncWriteExt;

        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let server = tokio::spawn(serve(Arc::new(Spliced), bind(&endpoint).await.unwrap()));
        let mut conn = connect(&endpoint).await.unwrap();
        let ask = Call {
            agent_id: String::new(),
            tool: "svc".into(),
            args: Value::Null,
        };
        write_frame(&mut conn, &ask).await.unwrap();
        conn.write_all(b"upload").await.unwrap();
        conn.shutdown().await.unwrap();
        let mut back = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            conn.read_to_end(&mut back),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(back, b"upload");
        server.abort();
    }

    #[tokio::test]
    async fn a_client_that_closes_its_connection_cancels_its_call() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = tokio::spawn(serve(
            Arc::new(Endless(cancelled.clone())),
            bind(&endpoint).await.unwrap(),
        ));
        let mut conn = connect(&endpoint).await.unwrap();
        let ask = Call {
            agent_id: String::new(),
            tool: "wait".into(),
            args: Value::Null,
        };
        write_frame(&mut conn, &ask).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!cancelled.load(std::sync::atomic::Ordering::SeqCst));
        drop(conn);
        for _ in 0..100 {
            if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst));
        server.abort();
    }

    #[test]
    fn a_socket_group_is_a_name_or_a_number_and_an_unknown_one_is_named() {
        let root = if cfg!(target_os = "macos") {
            "wheel"
        } else {
            "root"
        };
        assert_eq!(gid(root), Ok(0));
        assert_eq!(gid("4242"), Ok(4242));
        assert_eq!(
            gid("no-such-group-shuttle"),
            Err("no group no-such-group-shuttle".to_owned())
        );
        assert!(gid("a\0b").unwrap_err().contains("not a name"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_daemons_started_at_once_on_one_endpoint_leave_exactly_one_serving() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let start = || {
            let endpoint = endpoint.clone();
            tokio::spawn(async move { serve(Arc::new(NoDesk), bind(&endpoint).await?).await })
        };
        let (mut a, mut b) = (start(), start());
        let loser = tokio::select! {
            r = &mut a => { assert!(!b.is_finished()); r }
            r = &mut b => { assert!(!a.is_finished()); r }
        };
        assert_eq!(
            loser.unwrap().unwrap_err(),
            format!("another daemon serves {endpoint}")
        );
        let ask = Call {
            agent_id: "desk".into(),
            tool: "desk_screen".into(),
            args: json!({}),
        };
        assert!(
            answer(&endpoint, &ask)
                .await
                .unwrap()
                .text
                .contains("no desk backend")
        );
        a.abort();
        b.abort();
    }

    #[tokio::test]
    async fn the_socket_answers_desk_calls_with_desk_frames() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let desk = Arc::new(Desk::new(|| Ok(Stub::default()), NoDriver, Os::Mac));
        let server = tokio::spawn({
            let endpoint = endpoint.clone();
            async move { serve(desk, bind(&endpoint).await?).await }
        });
        let ask = |tool: &str, args: Value| Call {
            agent_id: "desk".into(),
            tool: tool.into(),
            args,
        };
        assert_eq!(
            answer(&endpoint, &ask("desk_windows", json!({})))
                .await
                .unwrap()
                .text,
            "[]"
        );
        let missing = call(&endpoint, &ask("desk_snapshot", json!({})))
            .await
            .unwrap();
        assert!(missing.is_error);
        assert_eq!(missing.text, "no focused window");
        let other = call(&endpoint, &ask("shell", json!({}))).await.unwrap();
        assert_eq!(other.text, "unknown desk tool shell");
        let key = call(&endpoint, &ask("desk_key", json!({"keys": "cmd+s"})))
            .await
            .unwrap();
        assert_eq!(key.text, "hotkey: no driver");
        let look = call(&endpoint, &ask("desk_look", json!({}))).await.unwrap();
        assert_eq!(
            (look.is_error, look.text.as_str()),
            (true, "desk_look: this desk answers no computer verbs")
        );
        server.abort();
    }

    #[tokio::test]
    async fn the_native_desk_answers_the_computer_verbs_unless_an_app_allowlist_or_background_input_is_set()
     {
        let ask = |tool: &str, args: Value| Call {
            agent_id: "desk".into(),
            tool: tool.into(),
            args,
        };
        let open = native("/nonexistent/driver.sock".into(), &[], false, false);
        let title = open
            .answer(ask("desk_title_at", json!({"x": 10, "y": 10})))
            .await;
        assert_eq!(
            (title.is_error, title.text.as_str()),
            (false, r#"{"title":false}"#)
        );
        let scale = open.answer(ask("desk_computer_scale", json!({}))).await;
        assert_eq!(scale.text, r#"{"scale":1.0}"#);
        let listed = native(
            "/nonexistent/driver.sock".into(),
            &["mousepad".to_owned()],
            false,
            false,
        );
        for tool in COMPUTER_VERBS {
            let out = listed.answer(ask(tool, json!({}))).await;
            assert_eq!(
                (out.is_error, out.text),
                (true, format!("{tool}: {ALLOWLIST_REFUSED}"))
            );
        }
        let other = listed.answer(ask("shell", json!({}))).await;
        assert_eq!(other.text, "unknown desk tool shell");
        let background = native("/nonexistent/driver.sock".into(), &[], false, true);
        let act = background
            .answer(ask(
                "desk_computer_act",
                json!({"action": "click", "x": 10, "y": 10}),
            ))
            .await;
        assert!(
            act.is_error && act.text.contains("needs the foreground"),
            "{}",
            act.text
        );
    }
}

#[cfg(test)]
mod pipe_tests {
    use super::*;

    #[test]
    fn the_pipe_admits_system_and_its_user_and_a_group_reads_and_writes_without_making_instances() {
        assert_eq!(
            pipe_sddl("S-1-5-21-1", None),
            "D:P(A;;GA;;;SY)(A;;GA;;;S-1-5-21-1)"
        );
        assert_eq!(
            pipe_sddl("S-1-5-21-1", Some("S-1-5-32-545")),
            "D:P(A;;GA;;;SY)(A;;GA;;;S-1-5-21-1)(A;;0x12019b;;;S-1-5-32-545)"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_group_is_an_account_name_or_a_sid_and_an_unknown_one_is_named() {
        assert_eq!(
            crate::pipesec::sid_of("S-1-5-32-545").unwrap(),
            "S-1-5-32-545"
        );
        assert_eq!(
            crate::pipesec::sid_of(r"BUILTIN\Users").unwrap(),
            "S-1-5-32-545"
        );
        let err = crate::pipesec::sid_of("no-such-account-8d1f").unwrap_err();
        assert!(err.contains("no-such-account-8d1f"), "{err}");
    }

    #[cfg(windows)]
    struct Guarded;

    #[cfg(windows)]
    impl Handler for Guarded {
        async fn answer(&self, call: Call) -> Output {
            Output::text(format!("{} answered", call.tool))
        }

        fn owner_only(&self, tool: &str) -> bool {
            tool == "halt"
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_client_dropped_with_a_write_pending_hangs_up_at_once() {
        use tokio::io::AsyncWriteExt;

        let name = format!(r"\\.\pipe\shuttle-test-hangup-{}", std::process::id());
        let mut server = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .in_buffer_size(4096)
            .out_buffer_size(4096)
            .create(&name)
            .unwrap();
        let accepted = server.connect();
        let mut client = connect(&name).await.unwrap();
        accepted.await.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.write_all(&vec![7u8; 1 << 20]),
        )
        .await
        .expect("the pipe took no write without a reader")
        .unwrap();
        drop(client);
        let started = std::time::Instant::now();
        let mut hung_up = false;
        while started.elapsed() < std::time::Duration::from_secs(2) {
            if server.write(b"x").await.is_err() {
                hung_up = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            hung_up,
            "the server saw no hang-up within 2 s of the client's drop"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn an_owner_only_call_over_the_pipe_from_this_user_is_answered_with_a_group_admitted() {
        let endpoint = format!(r"\\.\pipe\shuttle-test-owner-{}", std::process::id());
        let listener = bind_for(&endpoint, "S-1-5-32-545").await.unwrap();
        let server = tokio::spawn(serve(Arc::new(Guarded), listener));
        let out = call(
            &endpoint,
            &Call {
                agent_id: String::new(),
                tool: "halt".into(),
                args: serde_json::Value::Null,
            },
        )
        .await
        .unwrap();
        assert_eq!((out.is_error, out.text.as_str()), (false, "halt answered"));
        server.abort();
    }
}
