//! The Windows controls on their own message-loop thread: the kill hotkey, the tray icon with Stop and Resume, and the confirm dialog.

use std::os::windows::io::AsRawHandle;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use tokio::sync::oneshot;
use windows::Win32::Foundation::{
    HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, HWND, LPARAM, LRESULT, POINT, SetHandleInformation,
    WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFY_ICON_MESSAGE,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW,
    EnumThreadWindows, GetCursorPos, GetMessageW, IDI_APPLICATION, IDI_ERROR, IDI_INFORMATION,
    IDOK, LoadIconW, MB_DEFBUTTON2, MB_ICONQUESTION, MB_OKCANCEL, MB_SETFOREGROUND, MB_TOPMOST,
    MF_STRING, MSG, MessageBoxW, PostMessageW, RegisterClassW, RegisterWindowMessageW,
    SetForegroundWindow, SetTimer, TPM_NONOTIFY, TPM_RETURNCMD, TrackPopupMenu, TranslateMessage,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_HOTKEY, WM_LBUTTONUP, WM_RBUTTONUP,
    WM_TIMER, WNDCLASSW,
};
use windows::core::{BOOL, HSTRING, w};

use crate::daemon::{Asker, Controls, Ui};
use crate::log;

const POLL: Duration = Duration::from_millis(250);
const TRAY_MESSAGE: u32 = WM_APP + 1;
const STOP: usize = 1;
const RESUME: usize = 2;

static SHOWN: OnceLock<Shown> = OnceLock::new();
static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();
static TRAY: AtomicIsize = AtomicIsize::new(0);

struct Shown {
    controls: Arc<dyn Controls>,
    stop_label: String,
    title: Mutex<&'static str>,
}

impl Shown {
    fn tray(&self, window: HWND, action: NOTIFY_ICON_MESSAGE) {
        let (title, icon) = if self.controls.halted() {
            ("Shuttle stopped", IDI_ERROR)
        } else if self.controls.acting() {
            ("Shuttle acting", IDI_INFORMATION)
        } else {
            ("Shuttle", IDI_APPLICATION)
        };
        let mut last = self.title.lock().unwrap_or_else(PoisonError::into_inner);
        if action == NIM_MODIFY && *last == title {
            return;
        }
        *last = title;
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: window,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: TRAY_MESSAGE,
            // SAFETY: a stock icon needs no module and lives for the process.
            hIcon: unsafe { LoadIconW(None, icon) }.unwrap_or_default(),
            ..Default::default()
        };
        for (slot, unit) in data.szTip.iter_mut().zip(title.encode_utf16()) {
            *slot = unit;
        }
        // SAFETY: `data` is a filled NOTIFYICONDATAW for this thread's window.
        let _ = unsafe { Shell_NotifyIconW(action, &data) };
    }

    fn menu(&self, window: HWND) {
        let stop = HSTRING::from(self.stop_label.as_str());
        // SAFETY: the menu is created, shown and destroyed on this thread; both labels outlive it.
        let picked = unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                return;
            };
            let _ = AppendMenuW(menu, MF_STRING, STOP, &stop);
            let _ = AppendMenuW(menu, MF_STRING, RESUME, w!("Resume"));
            let mut at = POINT::default();
            let _ = GetCursorPos(&mut at);
            let _ = SetForegroundWindow(window);
            let picked = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY,
                at.x,
                at.y,
                None,
                window,
                None,
            );
            let _ = DestroyMenu(menu);
            picked.0 as usize
        };
        match picked {
            STOP => self.controls.halt(true),
            RESUME => self.controls.halt(false),
            _ => {}
        }
    }
}

/// Runs the controls on this thread until the process exits.
pub fn run(ui: Ui) -> ! {
    let shown = SHOWN.get_or_init(|| Shown {
        controls: ui.controls,
        stop_label: ui.stop_label,
        title: Mutex::new(""),
    });
    // SAFETY: the window class and window live until the process exits; `procedure` reads only `SHOWN`, set above.
    let window = unsafe {
        GetModuleHandleW(None).and_then(|module| {
            let class = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: module.into(),
                lpszClassName: w!("LoomhandShuttleControls"),
                ..Default::default()
            };
            RegisterClassW(&class);
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("LoomhandShuttleControls"),
                w!("Shuttle"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(module.into()),
                None,
            )
        })
    };
    let window = match window {
        Ok(w) => w,
        Err(e) => {
            log(format_args!(
                "the Windows controls did not start: {e}; shuttle stop still works"
            ));
            loop {
                std::thread::park();
            }
        }
    };
    if let Some(hotkey) = ui.hotkey {
        let modifiers = HOT_KEY_MODIFIERS(hotkey.modifiers) | MOD_NOREPEAT;
        // SAFETY: `window` is this thread's window; the hotkey id is unique to it.
        if let Err(e) = unsafe { RegisterHotKey(Some(window), 1, modifiers, hotkey.code) } {
            log(format_args!(
                "the kill hotkey did not register: {e}; shuttle stop still works"
            ));
        }
    }
    if ui.indicator {
        // SAFETY: the message name is a static string.
        TASKBAR_CREATED.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) });
        shown.tray(window, NIM_ADD);
        TRAY.store(window.0 as isize, Ordering::Release);
        // SAFETY: `window` is this thread's window; the timer has no callback, so it posts WM_TIMER.
        unsafe { SetTimer(Some(window), 1, POLL.as_millis() as u32, None) };
    }
    let mut msg = MSG::default();
    // SAFETY: `msg` is valid for each call; the loop runs on the thread that owns `window`.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        // SAFETY: `msg` came from GetMessageW just above.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    std::process::exit(0)
}

pub fn shutdown() {
    let window = TRAY.load(Ordering::Acquire);
    if window == 0 {
        return;
    }
    let data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(window as *mut _),
        uID: 1,
        ..Default::default()
    };
    // SAFETY: `data` names the icon `run` added for the controls window.
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
}

/// The asker that shows each question as a dialog on a thread of its own.
pub fn asker() -> Asker {
    Arc::new(|question| {
        let (tx, rx) = oneshot::channel();
        std::thread::spawn(move || ask(&question, tx));
        rx
    })
}

pub fn disinherit_stdio() {
    for raw in [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ] {
        // SAFETY: the handle is this process's own standard handle, valid for the call; clearing its inherit flag changes nothing else.
        let _ =
            unsafe { SetHandleInformation(HANDLE(raw), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) };
    }
}

fn ask(question: &str, tx: oneshot::Sender<bool>) {
    let call = Arc::new(Mutex::new(Some(tx)));
    // SAFETY: GetCurrentThreadId has no preconditions.
    let thread = unsafe { GetCurrentThreadId() };
    let watched = Arc::clone(&call);
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(POLL);
            let guard = watched.lock().unwrap_or_else(PoisonError::into_inner);
            let gone = match guard.as_ref() {
                None => return,
                Some(tx) => tx.is_closed(),
            };
            drop(guard);
            if gone {
                // SAFETY: `close` only posts WM_CLOSE to the windows of the dialog's thread.
                let _ = unsafe { EnumThreadWindows(thread, Some(close), LPARAM(0)) };
            }
        }
    });
    let text = HSTRING::from(format!("{question}\n\nOK allows it; Cancel refuses."));
    let style = MB_OKCANCEL | MB_DEFBUTTON2 | MB_ICONQUESTION | MB_TOPMOST | MB_SETFOREGROUND;
    // SAFETY: both strings outlive the call; the dialog has no owner window.
    let answer = unsafe { MessageBoxW(None, &text, w!("Shuttle"), style) };
    if let Some(tx) = call.lock().unwrap_or_else(PoisonError::into_inner).take() {
        let _ = tx.send(answer == IDOK);
    }
}

extern "system" fn close(window: HWND, _: LPARAM) -> BOOL {
    // SAFETY: `window` is a live top-level window the enumeration passed in.
    let _ = unsafe { PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    BOOL(1)
}

extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if let Some(shown) = SHOWN.get() {
        match message {
            WM_HOTKEY => shown.controls.halt(true),
            WM_TIMER => shown.tray(window, NIM_MODIFY),
            TRAY_MESSAGE if matches!(lparam.0 as u32 & 0xFFFF, WM_LBUTTONUP | WM_RBUTTONUP) => {
                shown.menu(window);
            }
            _ if TASKBAR_CREATED.get() == Some(&message) => shown.tray(window, NIM_ADD),
            _ => {}
        }
    }
    // SAFETY: the arguments are the ones Windows passed to this window procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}
