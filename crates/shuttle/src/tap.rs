//! A listen-only event tap on macOS: the clicks and keys the user sends while a recording runs.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::thread::JoinHandle;

use shuttle_desk::ax;
use tokio::sync::mpsc::UnboundedSender;

use crate::lock;
use crate::record::{Field, Hit, Input};

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
const KEYBOARD_TYPE: u32 = 10;
const KEY_ACTION_DOWN: u16 = 0;
const NO_DEAD_KEYS: u32 = 1;
const TURN_S: f64 = 0.1;
const DELIVER_IMMEDIATELY: isize = 4;
const CMD: u64 = 1 << 20;
const CTRL: u64 = 1 << 18;
const MODIFIERS: [(u64, &str); 4] = [
    (CMD, "cmd"),
    (CTRL, "ctrl"),
    (1 << 19, "alt"),
    (1 << 17, "shift"),
];

static LAYOUT: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static WATCHED: Once = Once::new();

type Callback = extern "C" fn(*mut c_void, u32, *mut c_void, *mut c_void) -> *mut c_void;
type Notified =
    extern "C" fn(*mut c_void, *const c_void, *const c_void, *const c_void, *const c_void);

/// A running tap; dropping it stops it.
pub struct Tap {
    done: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Tap {
    /// Starts a tap on its own thread that sends each click and key to `tx`, but none while `acting` counts shuttle's own actions; also answers whether secure input hides the keys as it starts.
    pub fn start(
        tx: UnboundedSender<Input>,
        acting: Arc<AtomicUsize>,
    ) -> Result<(Self, bool), String> {
        let (ready, started) = std::sync::mpsc::channel();
        let done = Arc::new(AtomicBool::new(false));
        let ended = Arc::clone(&done);
        let thread = std::thread::spawn(move || {
            let seen = Box::into_raw(Box::new(Seen {
                tx,
                acting,
                port: std::ptr::null_mut(),
                scale: ax::display_scale(),
                fresh: Cell::new(true),
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
                ax::bound_waits();
                let mut secure = ax::secure_input() && !ax::focused().is_some_and(|f| f.secure);
                let _ = ready.send(Ok(secure));
                while !ended.load(Ordering::Acquire) {
                    CFRunLoopRunInMode(kCFRunLoopDefaultMode, TURN_S, false);
                    let now = ax::secure_input();
                    if now && !secure && (*seen).acting.load(Ordering::Relaxed) == 0 {
                        let (pid, _) = ax::typing();
                        let _ = (*seen).tx.send(Input::Password {
                            pid,
                            focus: focused(),
                        });
                    }
                    secure = now;
                }
                CGEventTapEnable(port, false);
                CFMachPortInvalidate(port);
                CFRelease(source);
                CFRelease(port);
                drop(Box::from_raw(seen));
            }
        });
        let held = started
            .recv()
            .map_err(|_| "the event tap thread ended".to_owned())??;
        Ok((
            Self {
                done,
                thread: Some(thread),
            },
            held,
        ))
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

struct Seen {
    tx: UnboundedSender<Input>,
    acting: Arc<AtomicUsize>,
    port: *mut c_void,
    scale: f64,
    /// Whether the next key starts a run of typing, whose target the tap reads.
    fresh: Cell<bool>,
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

    fn text(&self, flags: u64, layout: &Mutex<Vec<u32>>) -> String {
        let mut buf = [0u16; 8];
        let mut len = 0usize;
        // SAFETY: the event is live for the callback that wraps it, `layout` holds a whole keyboard layout, and each call writes at most `buf.len()` units into `buf`.
        unsafe {
            if let Some(layout) = (flags & (CMD | CTRL) != 0)
                .then(|| lock(layout))
                .filter(|l| !l.is_empty())
            {
                let mut dead = 0u32;
                UCKeyTranslate(
                    layout.as_ptr().cast(),
                    self.field(KEY_CODE) as u16,
                    KEY_ACTION_DOWN,
                    0,
                    self.field(KEYBOARD_TYPE) as u32,
                    NO_DEAD_KEYS,
                    &mut dead,
                    buf.len(),
                    &mut len,
                    buf.as_mut_ptr(),
                );
            }
            if len == 0 {
                CGEventKeyboardGetUnicodeString(self.0, buf.len(), &mut len, buf.as_mut_ptr());
            }
        }
        String::from_utf16_lossy(&buf[..len.min(buf.len())])
    }
}

#[repr(C)]
struct Point {
    x: f64,
    y: f64,
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
    if seen.acting.load(Ordering::Relaxed) > 0 {
        return event;
    }
    if let Some(input) = read(&Event(event), kind, seen) {
        let _ = seen.tx.send(input);
    }
    event
}

pub(crate) fn watch() {
    *lock(&LAYOUT) = layout();
}

fn layout() -> Vec<u32> {
    // SAFETY: the observer is a function that lives as long as the process; the source is released after its layout data, which it owns, is copied out whole.
    unsafe {
        WATCHED.call_once(|| {
            CFNotificationCenterAddObserver(
                CFNotificationCenterGetDistributedCenter(),
                std::ptr::null(),
                switched,
                kTISNotifySelectedKeyboardInputSourceChanged,
                std::ptr::null(),
                DELIVER_IMMEDIATELY,
            );
        });
        let source = TISCopyCurrentKeyboardLayoutInputSource();
        if source.is_null() {
            return Vec::new();
        }
        let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
        let mut words = Vec::new();
        if !data.is_null() {
            let len = usize::try_from(CFDataGetLength(data)).unwrap_or_default();
            words = vec![0u32; len.div_ceil(4)];
            std::ptr::copy_nonoverlapping(CFDataGetBytePtr(data), words.as_mut_ptr().cast(), len);
        }
        CFRelease(source);
        words
    }
}

extern "C" fn switched(
    _: *mut c_void,
    _: *const c_void,
    _: *const c_void,
    _: *const c_void,
    _: *const c_void,
) {
    watch();
}

fn focused() -> Option<Field> {
    ax::focused().map(|f| {
        let (url, id) = f
            .page
            .map(|p| (p.url, p.dom_id.unwrap_or_default()))
            .unwrap_or_default();
        Field {
            role: f.role,
            name: f.name,
            frame: f.frame,
            window: f.window,
            secure: f.secure,
            url,
            id,
        }
    })
}

fn read(e: &Event, kind: u32, seen: &Seen) -> Option<Input> {
    let scale = seen.scale;
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
                role: s.role,
                name: s.name,
                description: s.description,
                pid: s.pid,
                path: s.path,
                secret: s.secret,
                page: s.page.map(|p| (p.url, p.dom_id)),
            });
            seen.fresh.set(true);
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
            let read = seen.fresh.get();
            let focus = if read { focused() } else { None };
            let text = if secret {
                String::new()
            } else {
                e.text(flags, &LAYOUT)
            };
            let plain = !text.is_empty()
                && !text.chars().any(char::is_control)
                && flags & (CMD | CTRL) == 0;
            seen.fresh.set(!plain);
            Some(Input::Key {
                code: e.field(KEY_CODE),
                text,
                mods,
                secret,
                pid,
                focus,
                read,
            })
        }
        _ => None,
    }
}

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
    fn CFDataGetLength(data: *const c_void) -> isize;
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFNotificationCenterGetDistributedCenter() -> *mut c_void;
    fn CFNotificationCenterAddObserver(
        center: *mut c_void,
        observer: *const c_void,
        callback: Notified,
        name: *const c_void,
        object: *const c_void,
        suspension: isize,
    );
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    static kTISPropertyUnicodeKeyLayoutData: *const c_void;
    static kTISNotifySelectedKeyboardInputSourceChanged: *const c_void;
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(source: *mut c_void, key: *const c_void) -> *const c_void;
    fn UCKeyTranslate(
        layout: *const c_void,
        code: u16,
        action: u16,
        mods: u32,
        keyboard: u32,
        options: u32,
        dead: *mut u32,
        max: usize,
        actual: *mut usize,
        chars: *mut u16,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    const UTF8: u32 = 0x0800_0100;

    static TIS: Mutex<()> = Mutex::new(());

    #[test]
    fn a_chord_records_the_key_it_is_on_and_plain_typing_the_character_it_makes() {
        let _tis = lock(&TIS);
        let layout = Mutex::new(layout());
        let press = |code: u16, text: &str, flags: u64| {
            let units: Vec<u16> = text.encode_utf16().collect();
            // SAFETY: the event is created here, read while it lives and released after.
            unsafe {
                let event = CGEventCreateKeyboardEvent(std::ptr::null(), code, true);
                CGEventKeyboardSetUnicodeString(event, units.len(), units.as_ptr());
                CGEventSetFlags(event, flags);
                let text = Event(event).text(flags, &layout);
                CFRelease(event);
                text
            }
        };
        assert_eq!(
            [
                press(8, "\u{3}", CTRL),
                press(8, "ç", CMD | (1 << 19)),
                press(33, "\u{1b}", CTRL | (1 << 17)),
                press(8, "ç", 1 << 19),
                press(53, "\u{1b}", 0),
                press(48, "\t", CTRL),
            ],
            ["c", "c", "[", "ç", "\u{1b}", "\t"]
        );
    }

    #[test]
    fn a_chord_after_an_input_source_switch_is_named_by_the_layout_it_switched_to() {
        let _tis = lock(&TIS);
        let (tx, _inputs) = tokio::sync::mpsc::unbounded_channel();
        let seen = Seen {
            tx,
            acting: Arc::default(),
            port: std::ptr::null_mut(),
            scale: 1.0,
            fresh: Cell::new(false),
        };
        let mut named = Vec::new();
        // SAFETY: each layout is copied whole out of a source its list owns before the list, query and id are released, and each event is read while it lives and released after.
        unsafe {
            for id in [c"com.apple.keylayout.US", c"com.apple.keylayout.German"] {
                let id = CFStringCreateWithCString(std::ptr::null(), id.as_ptr(), UTF8);
                let key = kTISPropertyInputSourceID;
                let query = CFDictionaryCreate(
                    std::ptr::null(),
                    &key,
                    &id,
                    1,
                    (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
                    (&raw const kCFTypeDictionaryValueCallBacks).cast(),
                );
                let list = TISCreateInputSourceList(query, true);
                assert!(!list.is_null() && CFArrayGetCount(list) > 0);
                let data = TISGetInputSourceProperty(
                    CFArrayGetValueAtIndex(list, 0).cast_mut(),
                    kTISPropertyUnicodeKeyLayoutData,
                );
                let len = usize::try_from(CFDataGetLength(data)).unwrap_or_default();
                let mut words = vec![0u32; len.div_ceil(4)];
                std::ptr::copy_nonoverlapping(
                    CFDataGetBytePtr(data),
                    words.as_mut_ptr().cast(),
                    len,
                );
                CFRelease(list);
                CFRelease(query);
                CFRelease(id);
                *lock(&LAYOUT) = words;
                let event = CGEventCreateKeyboardEvent(std::ptr::null(), 6, true);
                CGEventSetFlags(event, CTRL);
                if let Some(Input::Key { text, .. }) = read(&Event(event), KEY_DOWN, &seen) {
                    named.push(text);
                }
                CFRelease(event);
            }
        }
        assert_eq!(named, ["z", "y"]);
    }

    #[test]
    fn typing_names_the_app_in_front_when_no_focused_element_names_one() {
        assert!(ax::typing().0.is_some() || ax::front_app().is_none());
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreateKeyboardEvent(source: *const c_void, code: u16, down: bool) -> *mut c_void;
        fn CGEventKeyboardSetUnicodeString(event: *mut c_void, len: usize, chars: *const u16);
        fn CGEventSetFlags(event: *mut c_void, flags: u64);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFTypeDictionaryKeyCallBacks: [usize; 6];
        static kCFTypeDictionaryValueCallBacks: [usize; 5];
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            chars: *const std::ffi::c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFDictionaryCreate(
            alloc: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            count: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *const c_void;
        fn CFArrayGetCount(array: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    }

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        static kTISPropertyInputSourceID: *const c_void;
        fn TISCreateInputSourceList(query: *const c_void, all: bool) -> *const c_void;
    }
}
