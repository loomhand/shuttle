//! The macOS controls on the main thread's AppKit loop: the kill hotkey, the menu-bar indicator with Stop and Resume, and the confirm dialog.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::Arc;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertSecondButtonReturn, NSApplication, NSApplicationActivationPolicy, NSMenu,
    NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSObject, NSRunLoop, NSRunLoopCommonModes, NSString, NSTimer};
use tokio::sync::oneshot;

use crate::daemon::{Asker, Controls, Ui};
use crate::hotkey::Hotkey;
use crate::log;

const POLL_S: f64 = 0.25;
const KEYBOARD_CLASS: u32 = u32::from_be_bytes(*b"keyb");
const HOT_KEY_PRESSED: u32 = 5;
const SIGNATURE: u32 = u32::from_be_bytes(*b"cshl");

/// Runs the controls on this, the main, thread until the process exits.
pub fn run(ui: Ui) -> ! {
    #[allow(clippy::expect_used)]
    let mtm = MainThreadMarker::new().expect("the macOS controls run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    register(ui.hotkey, Arc::clone(&ui.controls));
    let _indicator = ui
        .indicator
        .then(|| indicator(mtm, &ui.controls, &ui.hotkey_label));
    app.run();
    std::process::exit(0)
}

/// The asker that shows each question as a dialog on the main thread.
pub fn asker() -> Asker {
    Arc::new(|question| {
        let (tx, rx) = oneshot::channel();
        DispatchQueue::main().exec_async(move || {
            if let Some(mtm) = MainThreadMarker::new() {
                ask(mtm, &question, tx);
            }
        });
        rx
    })
}

fn ask(mtm: MainThreadMarker, question: &str, tx: oneshot::Sender<bool>) {
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("Cocoon Shuttle"));
    alert.setInformativeText(&NSString::from_str(question));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    alert.addButtonWithTitle(&NSString::from_str("Allow"));
    let watch = Watch::new(mtm, tx);
    // SAFETY: `watch` is a `Watch`, whose `watch:` method takes the timer, and there is no user info; the timer joins this thread's run loop in the common modes, which include the dialog's modal mode.
    let timer = unsafe {
        let timer = NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
            POLL_S,
            &watch,
            sel!(watch:),
            None,
            true,
        );
        NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
        timer
    };
    let allowed = alert.runModal() == NSAlertSecondButtonReturn;
    timer.invalidate();
    if let Some(tx) = watch.ivars().take() {
        let _ = tx.send(allowed);
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CocoonShuttleConfirmWatch"]
    #[ivars = RefCell<Option<oneshot::Sender<bool>>>]
    struct Watch;

    impl Watch {
        #[unsafe(method(watch:))]
        fn watch(&self, _timer: Option<&AnyObject>) {
            if self
                .ivars()
                .borrow()
                .as_ref()
                .is_none_or(oneshot::Sender::is_closed)
            {
                NSApplication::sharedApplication(self.mtm()).abortModal();
            }
        }
    }
);

impl Watch {
    fn new(mtm: MainThreadMarker, tx: oneshot::Sender<bool>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RefCell::new(Some(tx)));
        // SAFETY: NSObject's init on a freshly allocated object with its ivars set.
        unsafe { msg_send![super(this), init] }
    }
}

struct Indicator {
    _target: Retained<Target>,
    _timer: Retained<NSTimer>,
}

fn indicator(mtm: MainThreadMarker, controls: &Arc<dyn Controls>, hotkey: &str) -> Indicator {
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    let target = Target::new(
        mtm,
        Shown {
            controls: Arc::clone(controls),
            item: item.clone(),
        },
    );
    let menu = NSMenu::new(mtm);
    for (title, action) in [
        (format!("Stop ({hotkey})"), sel!(stop:)),
        ("Resume".to_owned(), sel!(resume:)),
    ] {
        // SAFETY: the item is a fresh NSMenuItem; the action names a method `Target` defines.
        let entry = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(&title),
                Some(action),
                &NSString::from_str(""),
            )
        };
        // SAFETY: `target` outlives the menu: the indicator holds both until the process exits.
        unsafe { entry.setTarget(Some(&target)) };
        menu.addItem(&entry);
    }
    item.setMenu(Some(&menu));
    target.ivars().show(mtm);
    // SAFETY: `target` is a `Target`, whose `tick:` method takes the timer, and there is no user info; the timer joins this thread's run loop in the common modes, which include the confirm dialog's modal mode.
    let timer = unsafe {
        let timer = NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
            POLL_S,
            &target,
            sel!(tick:),
            None,
            true,
        );
        NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
        timer
    };
    Indicator {
        _target: target,
        _timer: timer,
    }
}

struct Shown {
    controls: Arc<dyn Controls>,
    item: Retained<NSStatusItem>,
}

impl Shown {
    fn show(&self, mtm: MainThreadMarker) {
        let title = if self.controls.halted() {
            "■ Shuttle stopped"
        } else if self.controls.acting() {
            "● Shuttle"
        } else {
            "Shuttle"
        };
        if let Some(button) = self.item.button(mtm) {
            button.setTitle(&NSString::from_str(title));
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CocoonShuttleControls"]
    #[ivars = Shown]
    struct Target;

    impl Target {
        #[unsafe(method(stop:))]
        fn stop(&self, _sender: Option<&AnyObject>) {
            self.ivars().controls.halt(true);
        }

        #[unsafe(method(resume:))]
        fn resume(&self, _sender: Option<&AnyObject>) {
            self.ivars().controls.halt(false);
        }

        #[unsafe(method(tick:))]
        fn tick(&self, _timer: Option<&AnyObject>) {
            self.ivars().show(self.mtm());
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker, shown: Shown) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(shown);
        // SAFETY: NSObject's init on a freshly allocated object with its ivars set.
        unsafe { msg_send![super(this), init] }
    }
}

#[repr(C)]
struct EventTypeSpec {
    class: u32,
    kind: u32,
}

#[repr(C)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

type Handler = extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i32;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> *mut c_void;
    fn InstallEventHandler(
        target: *mut c_void,
        handler: Handler,
        count: u32,
        types: *const EventTypeSpec,
        user: *mut c_void,
        out: *mut *mut c_void,
    ) -> i32;
    fn RegisterEventHotKey(
        code: u32,
        modifiers: u32,
        id: EventHotKeyId,
        target: *mut c_void,
        options: u32,
        out: *mut *mut c_void,
    ) -> i32;
}

extern "C" fn pressed(_: *mut c_void, _: *mut c_void, user: *mut c_void) -> i32 {
    // SAFETY: `user` is the leaked `Arc<dyn Controls>` box that `register` installs for the life of the process.
    let controls = unsafe { &*(user as *const Arc<dyn Controls>) };
    controls.halt(true);
    0
}

fn register(hotkey: Hotkey, controls: Arc<dyn Controls>) {
    let user = Box::into_raw(Box::new(controls)).cast::<c_void>();
    let spec = EventTypeSpec {
        class: KEYBOARD_CLASS,
        kind: HOT_KEY_PRESSED,
    };
    let mut handler = std::ptr::null_mut();
    let mut key = std::ptr::null_mut();
    // SAFETY: the application event target lives as long as the process; `spec` and the out pointers are valid for the calls; `user` is never freed.
    let (installed, registered) = unsafe {
        let target = GetApplicationEventTarget();
        (
            InstallEventHandler(target, pressed, 1, &spec, user, &mut handler),
            RegisterEventHotKey(
                hotkey.code,
                hotkey.modifiers,
                EventHotKeyId {
                    signature: SIGNATURE,
                    id: 1,
                },
                target,
                0,
                &mut key,
            ),
        )
    };
    if installed != 0 || registered != 0 {
        log(format_args!(
            "the kill hotkey did not register (handler {installed}, hotkey {registered}); cocoon-shuttle stop still works"
        ));
    }
}
