//! A listen-only event tap on macOS: the clicks and keys the user sends while a recording runs.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use tokio::sync::mpsc::UnboundedSender;

use shuttle_desk::ax;

use crate::record::{Hit, Input};

const SESSION_TAP: u32 = 1;
const HEAD_INSERT: u32 = 0;
const LISTEN_ONLY: u32 = 1;
const LEFT_DOWN: u32 = 1;
const RIGHT_DOWN: u32 = 3;
const KEY_DOWN: u32 = 10;
const DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const DISABLED_BY_INPUT: u32 = 0xFFFF_FFFF;
const CLICK_STATE: u32 = 1;
const KEY_CODE: u32 = 9;
const TURN_S: f64 = 0.1;
const MODIFIERS: [(u64, &str); 4] = [
    (1 << 20, "cmd"),
    (1 << 18, "ctrl"),
    (1 << 19, "alt"),
    (1 << 17, "shift"),
];

/// A running tap; dropping it stops it.
pub struct Tap {
    done: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

struct Seen {
    tx: UnboundedSender<Input>,
    port: *mut c_void,
    scale: f64,
}

/// Whether this process may listen to the keyboard (Input Monitoring).
pub fn allowed() -> bool {
    // SAFETY: the call only reads this process's TCC state.
    unsafe { CGPreflightListenEventAccess() }
}

/// Lists this app under Input Monitoring and asks the user once.
pub fn ask() {
    // SAFETY: the call reads and may register this process's TCC state.
    unsafe { CGRequestListenEventAccess() };
}

impl Tap {
    /// Starts a tap on its own thread that sends each click and key to `tx`.
    pub fn start(tx: UnboundedSender<Input>) -> Result<Self, String> {
        let (ready, started) = std::sync::mpsc::channel();
        let done = Arc::new(AtomicBool::new(false));
        let ended = Arc::clone(&done);
        let thread = std::thread::spawn(move || {
            let seen = Box::into_raw(Box::new(Seen {
                tx,
                port: std::ptr::null_mut(),
                scale: ax::display_scale(),
            }));
            let mask = (1u64 << LEFT_DOWN) | (1u64 << RIGHT_DOWN) | (1u64 << KEY_DOWN);
            // SAFETY: `seen` outlives the tap: it is freed below, after the run loop that calls `heard` ends; the source and port are released after use.
            unsafe {
                let port = CGEventTapCreate(
                    SESSION_TAP,
                    HEAD_INSERT,
                    LISTEN_ONLY,
                    mask,
                    heard,
                    seen.cast(),
                );
                if port.is_null() {
                    drop(Box::from_raw(seen));
                    let _ = ready.send(Err("no event tap: allow Shuttle under System Settings > Privacy & Security > Input Monitoring".to_owned()));
                    return;
                }
                (*seen).port = port;
                let source = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
                CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
                CGEventTapEnable(port, true);
                let _ = ready.send(Ok(()));
                while !ended.load(Ordering::Acquire) {
                    CFRunLoopRunInMode(kCFRunLoopDefaultMode, TURN_S, false);
                }
                CGEventTapEnable(port, false);
                CFMachPortInvalidate(port);
                CFRelease(source);
                CFRelease(port);
                drop(Box::from_raw(seen));
            }
        });
        started
            .recv()
            .map_err(|_| "the event tap thread ended".to_owned())??;
        Ok(Self {
            done,
            thread: Some(thread),
        })
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

extern "C" fn heard(
    _proxy: *mut c_void,
    kind: u32,
    event: *mut c_void,
    user: *mut c_void,
) -> *mut c_void {
    // SAFETY: `user` is the `Seen` box the tap thread owns while its run loop runs this callback.
    let seen = unsafe { &*(user as *const Seen) };
    if kind == DISABLED_BY_TIMEOUT || kind == DISABLED_BY_INPUT {
        // SAFETY: the port is the live tap that was disabled.
        unsafe { CGEventTapEnable(seen.port, true) };
        return event;
    }
    if let Some(input) = read(&Event(event), kind, seen.scale) {
        let _ = seen.tx.send(input);
    }
    event
}

/// The live event a tap callback gets, for the length of the call.
struct Event(*mut c_void);

impl Event {
    fn flags(&self) -> u64 {
        // SAFETY: the event is live for the callback that wraps it.
        unsafe { CGEventGetFlags(self.0) }
    }

    fn at(&self) -> Point {
        // SAFETY: the event is live for the callback that wraps it.
        unsafe { CGEventGetLocation(self.0) }
    }

    fn field(&self, f: u32) -> i64 {
        // SAFETY: the event is live for the callback that wraps it.
        unsafe { CGEventGetIntegerValueField(self.0, f) }
    }

    fn text(&self) -> String {
        let mut buf = [0u16; 8];
        let mut len = 0usize;
        // SAFETY: the event is live for the callback that wraps it, and the call writes at most `buf.len()` units into `buf`.
        unsafe { CGEventKeyboardGetUnicodeString(self.0, buf.len(), &mut len, buf.as_mut_ptr()) };
        String::from_utf16_lossy(&buf[..len.min(buf.len())])
    }
}

fn read(e: &Event, kind: u32, scale: f64) -> Option<Input> {
    let flags = e.flags();
    let mods = MODIFIERS
        .iter()
        .filter(|(m, _)| flags & m != 0)
        .map(|(_, n)| *n)
        .collect();
    match kind {
        LEFT_DOWN | RIGHT_DOWN => {
            let p = e.at();
            let (x, y) = (p.x * scale, p.y * scale);
            let found = ax::seen_at(x.round() as i32, y.round() as i32).map(|s| Hit {
                role: s.role.to_owned(),
                name: s.name,
                description: s.description,
                pid: s.pid,
                path: s.path.into_iter().map(|(r, n)| (r.to_owned(), n)).collect(),
                secret: s.secret,
            });
            Some(Input::Click {
                x,
                y,
                right: kind == RIGHT_DOWN,
                count: e.field(CLICK_STATE),
                mods,
                found,
            })
        }
        KEY_DOWN => {
            let (pid, secret) = ax::typing();
            Some(Input::Key {
                code: e.field(KEY_CODE),
                text: if secret { String::new() } else { e.text() },
                mods,
                secret,
                pid,
            })
        }
        _ => None,
    }
}

#[repr(C)]
struct Point {
    x: f64,
    y: f64,
}

type Callback = extern "C" fn(*mut c_void, u32, *mut c_void, *mut c_void) -> *mut c_void;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        mask: u64,
        callback: Callback,
        user: *mut c_void,
    ) -> *mut c_void;
    fn CGEventTapEnable(tap: *mut c_void, enable: bool);
    fn CGEventGetLocation(event: *mut c_void) -> Point;
    fn CGEventGetFlags(event: *mut c_void) -> u64;
    fn CGEventGetIntegerValueField(event: *mut c_void, field: u32) -> i64;
    fn CGEventKeyboardGetUnicodeString(
        event: *mut c_void,
        max: usize,
        actual: *mut usize,
        chars: *mut u16,
    );
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: *const c_void;
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFMachPortCreateRunLoopSource(
        alloc: *const c_void,
        port: *mut c_void,
        order: isize,
    ) -> *mut c_void;
    fn CFMachPortInvalidate(port: *mut c_void);
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopAddSource(run_loop: *mut c_void, source: *mut c_void, mode: *const c_void);
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, once: bool) -> i32;
    fn CFRelease(cf: *const c_void);
}
