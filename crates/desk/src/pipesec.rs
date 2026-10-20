//! Who may open the daemon's named pipe, and whether a client runs as this process's user.

use std::ffi::c_void;
use std::sync::OnceLock;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    ConvertStringSidToSidW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, LookupAccountNameW, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    SID_NAME_USE, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::core::{HSTRING, PWSTR};

use crate::server::pipe_sddl;

/// The security descriptor every instance of the daemon's pipe carries.
pub struct Security {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

// SAFETY: the descriptor is a LocalAlloc block this value alone owns and only reads after it is built.
unsafe impl Send for Security {}
// SAFETY: as above; no method writes through the pointers.
unsafe impl Sync for Security {}

impl Security {
    /// Admits SYSTEM and this process's user, and `group`, an account name or SID string, when set.
    pub fn new(group: Option<&str>) -> Result<Self, String> {
        let group = group.map(sid_of).transpose()?;
        let sddl = HSTRING::from(pipe_sddl(own_sid()?, group.as_deref()));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` is a NUL-terminated string that outlives the call; `descriptor` receives a LocalAlloc block.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &sddl,
                SDDL_REVISION_1,
                &raw mut descriptor,
                None,
            )
        }
        .map_err(|e| format!("pipe security {sddl}: {e}"))?;
        Ok(Self {
            descriptor,
            attributes: SECURITY_ATTRIBUTES {
                nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            },
        })
    }

    /// The `SECURITY_ATTRIBUTES` to create a pipe instance with, valid while `self` lives.
    pub fn attributes(&self) -> *mut c_void {
        (&raw const self.attributes).cast_mut().cast()
    }
}

impl Drop for Security {
    fn drop(&mut self) {
        // SAFETY: `descriptor` came from ConvertStringSecurityDescriptorToSecurityDescriptorW and is freed once.
        unsafe { LocalFree(Some(HLOCAL(self.descriptor.0))) };
    }
}

/// Whether the client at the far end of `pipe`, a server handle, runs as this process's user.
pub fn client_is_owner(pipe: HANDLE) -> bool {
    let mut pid = 0;
    // SAFETY: `pipe` is a live server handle of a connected pipe; `pid` receives the client's id.
    if unsafe { GetNamedPipeClientProcessId(pipe, &raw mut pid) }.is_err() {
        return false;
    }
    // SAFETY: OpenProcess takes plain values; the handle is closed below.
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return false;
    };
    let user = user_of(process);
    // SAFETY: `process` is the handle opened above, closed once.
    let _ = unsafe { CloseHandle(process) };
    match (user, own_sid()) {
        (Ok(user), Ok(own)) => user == own,
        _ => false,
    }
}

/// The SID string of `account`, an account name or a SID string.
pub fn sid_of(account: &str) -> Result<String, String> {
    if account.starts_with("S-1-") {
        let mut sid = PSID::default();
        // SAFETY: the string outlives the call; `sid` receives a LocalAlloc block freed in `sid_string`.
        unsafe { ConvertStringSidToSidW(&HSTRING::from(account), &raw mut sid) }
            .map_err(|e| format!("socket_group {account}: not a SID: {e}"))?;
        return sid_string(sid, true);
    }
    let name = HSTRING::from(account);
    let (mut sid_len, mut domain_len, mut use_) = (0u32, 0u32, SID_NAME_USE::default());
    // SAFETY: a first call with no buffers only reports their sizes.
    let _ = unsafe {
        LookupAccountNameW(
            None,
            &name,
            None,
            &raw mut sid_len,
            None,
            &raw mut domain_len,
            &raw mut use_,
        )
    };
    if sid_len == 0 {
        return Err(format!("socket_group {account}: no such account"));
    }
    let mut sid = vec![0u8; sid_len as usize];
    let mut domain = vec![0u16; domain_len.max(1) as usize];
    // SAFETY: both buffers are as large as the first call reported.
    unsafe {
        LookupAccountNameW(
            None,
            &name,
            Some(PSID(sid.as_mut_ptr().cast())),
            &raw mut sid_len,
            Some(PWSTR(domain.as_mut_ptr())),
            &raw mut domain_len,
            &raw mut use_,
        )
    }
    .map_err(|e| format!("socket_group {account}: {e}"))?;
    sid_string(PSID(sid.as_mut_ptr().cast()), false)
}

fn own_sid() -> Result<&'static str, String> {
    static OWN: OnceLock<Result<String, String>> = OnceLock::new();
    // SAFETY: GetCurrentProcess returns a pseudo handle that needs no closing.
    OWN.get_or_init(|| user_of(unsafe { GetCurrentProcess() }))
        .as_deref()
        .map_err(Clone::clone)
}

fn user_of(process: HANDLE) -> Result<String, String> {
    let mut token = HANDLE::default();
    // SAFETY: `process` is a live process handle; `token` is closed below.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) }
        .map_err(|e| format!("open the process token: {e}"))?;
    let mut len = 0;
    // SAFETY: a first call with no buffer only reports its size.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &raw mut len) };
    let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
    // SAFETY: `buf` holds at least `len` bytes, aligned for TOKEN_USER.
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            len,
            &raw mut len,
        )
    };
    // SAFETY: `token` was opened above and is closed once.
    let _ = unsafe { CloseHandle(token) };
    read.map_err(|e| format!("read the token user: {e}"))?;
    // SAFETY: GetTokenInformation filled `buf` with a TOKEN_USER whose SID points into `buf`.
    let user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
    sid_string(user.User.Sid, false)
}

fn sid_string(sid: PSID, owned: bool) -> Result<String, String> {
    let mut text = PWSTR::null();
    // SAFETY: `sid` is a valid SID; `text` receives a LocalAlloc string freed below.
    let made = unsafe { ConvertSidToStringSidW(sid, &raw mut text) };
    if owned {
        // SAFETY: an owned `sid` came from ConvertStringSidToSidW and is freed once.
        unsafe { LocalFree(Some(HLOCAL(sid.0))) };
    }
    made.map_err(|e| format!("format a SID: {e}"))?;
    // SAFETY: `text` is a NUL-terminated string from ConvertSidToStringSidW.
    let s = unsafe { text.to_string() }.map_err(|e| format!("format a SID: {e}"));
    // SAFETY: `text` came from ConvertSidToStringSidW and is freed once.
    unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    s
}
