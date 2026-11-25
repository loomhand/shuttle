//! Which refusals of background input auto input takes to the foreground: those raised before any input was sent.

use serde_json::Value;

use crate::desk::{
    BACKGROUND_DESKTOP, BACKGROUND_ELEMENT, BACKGROUND_NO_WINDOW, BACKGROUND_TARGET,
    BACKGROUND_TERMINAL, BACKGROUND_UIA,
};
use crate::keys::Os;
use crate::press::BACKGROUND_NO_POINT;

/// The desk verbs auto input tries in the background first.
pub(crate) const VERBS: [&str; 7] = [
    "desk_key",
    "desk_type",
    "desk_click",
    "desk_scroll",
    "desk_drag",
    "desk_press",
    "desk_act",
];

const SHUTTLE: [&str; 7] = [
    BACKGROUND_TARGET,
    BACKGROUND_TERMINAL,
    BACKGROUND_DESKTOP,
    BACKGROUND_NO_WINDOW,
    BACKGROUND_ELEMENT,
    BACKGROUND_UIA,
    BACKGROUND_NO_POINT,
];
const CLASS: &str = "Background delivery is not available for target window class";
const CLASS_KIND: &str = "on this event kind (";
const XAML_HOTKEY: &str = "could not find a UIA AcceleratorKey";
const MAC_HIDDEN: &str = "is minimized or its application is hidden";
const MAC_OFF_SPACE: &str = "is not among the process's current AXWindows";
const MAC_OCCLUDED: &str = "Background pointer press refused at screen";
const MAC_MENU_BEHIND: &str = "invoke_menu does not activate a background app";
const MAC_CONSENT_PENDING: &str = "automation_consent_pending";

/// Whether error `e` of background `tool` on `os` came before anything was delivered, so the foreground may run it once.
pub(crate) fn pre_send(tool: &str, os: Os, args: &Value, e: &str) -> bool {
    if tool == "desk_press" && os != Os::Windows {
        return false;
    }
    if SHUTTLE.iter().any(|s| e.contains(s)) {
        return true;
    }
    if e.contains(CLASS) {
        let double = tool == "desk_click" && args["count"].as_u64().is_some_and(|c| c >= 2);
        return !double && class_only(e);
    }
    e.contains(XAML_HOTKEY)
        || (os == Os::Mac
            && (e.contains(MAC_HIDDEN)
                || e.contains(MAC_OFF_SPACE)
                || e.contains(MAC_OCCLUDED)
                || (tool == "desk_key"
                    && e.contains(MAC_MENU_BEHIND)
                    && !e.contains(MAC_CONSENT_PENDING))))
}

fn class_only(e: &str) -> bool {
    e.find(CLASS_KIND)
        .map(|i| &e[i + CLASS_KIND.len()..])
        .and_then(|rest| rest.find(')').map(|j| &rest[j + 1..]))
        .is_some_and(|after| after.starts_with(". Retry"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const REFUSED: &str = "hotkey: Background delivery is not available for target window class 'Chrome_WidgetWin_1' on this event kind (key_combo). Retry this action with delivery_mode:\"foreground\"; Cua Driver will activate the target for the action and restore the previous foreground afterward.";
    const TRIED: &str = "click: Background delivery is not available for target window class 'Foo' on this event kind (mouse_click): coordinate injection failed (x). Retry this action with delivery_mode:\"foreground\".";

    #[test]
    fn a_macos_key_whose_menu_item_is_disabled_in_the_background_goes_to_the_foreground() {
        let none = json!({});
        let refused = "hotkey: Select All is disabled. invoke_menu does not activate a background app: macOS keeps this menu item unavailable while the app is behind the user's window.";
        assert!(pre_send("desk_key", Os::Mac, &none, refused));
        for (tool, os) in [
            ("desk_key", Os::Windows),
            ("desk_key", Os::Linux),
            ("desk_click", Os::Mac),
            ("desk_type", Os::Mac),
            ("desk_press", Os::Mac),
            ("desk_act", Os::Mac),
        ] {
            assert!(!pre_send(tool, os, &none, refused), "{tool} {os:?}");
        }
    }

    #[test]
    fn a_macos_save_waiting_on_consent_is_not_retried_but_a_denied_one_is() {
        let none = json!({});
        let save = |cause: &str| {
            format!(
                "hotkey: cmd+s is the key equivalent of File > Save. Key events do not reach a background app's menus, so no keys were sent. invoke_menu: path segment 1 is disabled while the app is in the background, and saving through TextEdit's scripting interface did not complete ({cause}). Nothing was saved. invoke_menu does not activate a background app: macOS keeps this menu item unavailable while the app is behind the user's window."
            )
        };
        let pending = save(
            "automation_consent_pending: macOS may ask the user for Automation consent for TextEdit; nothing was sent, and the background route works once it is granted",
        );
        let denied = save(
            "automation_denied: macOS does not allow Cua Driver to control TextEdit. The user can allow it in System Settings > Privacy & Security > Automation",
        );
        assert!(!pre_send("desk_key", Os::Mac, &none, &pending));
        assert!(pre_send("desk_key", Os::Mac, &none, &denied));
    }

    #[test]
    fn a_refusal_raised_before_any_input_goes_to_the_foreground() {
        let none = json!({});
        for (tool, os, e) in [
            ("desk_key", Os::Windows, BACKGROUND_TARGET),
            ("desk_click", Os::Mac, BACKGROUND_NO_WINDOW),
            ("desk_act", Os::Windows, BACKGROUND_UIA),
            ("desk_type", Os::Windows, BACKGROUND_TERMINAL),
            ("desk_press", Os::Windows, BACKGROUND_NO_POINT),
            ("desk_press", Os::Windows, BACKGROUND_ELEMENT),
            ("desk_key", Os::Windows, REFUSED),
            (
                "desk_key",
                Os::Windows,
                "hotkey: hotkey on a modern XAML / UWP target (pid 4, hwnd 5) could not find a UIA AcceleratorKey or `(Ctrl+X)`-style name hint",
            ),
            (
                "desk_key",
                Os::Mac,
                "press_key: window 7 is minimized or its application is hidden (or that state could not be proven)",
            ),
            (
                "desk_key",
                Os::Mac,
                "press_key: window 7 is not among the process's current AXWindows (another Space, or its AX surface is unresolved); background input is refused so a sibling window cannot receive it. Observation (capture/list_windows) remains available",
            ),
            (
                "desk_click",
                Os::Mac,
                "click: Background pointer press refused at screen (412, 300): window 81 of pid 7 is drawn over window 80 there",
            ),
        ] {
            assert!(pre_send(tool, os, &none, e), "{tool} {e}");
        }
    }

    #[test]
    fn a_refusal_that_may_follow_delivered_input_stays_an_error() {
        let none = json!({});
        for (tool, os, args, e) in [
            ("desk_click", Os::Windows, &none, TRIED),
            ("desk_click", Os::Windows, &json!({"count": 2}), REFUSED),
            ("desk_press", Os::Mac, &none, BACKGROUND_ELEMENT),
            (
                "desk_press",
                Os::Windows,
                &none,
                crate::desk::BACKGROUND_TWINS,
            ),
            ("desk_act", Os::Mac, &none, crate::desk::BACKGROUND_TWINS),
            (
                "desk_click",
                Os::Windows,
                &none,
                "click: UIA pixel click timeout for pid 4. No fallback input was sent.",
            ),
            (
                "desk_key",
                Os::Windows,
                &none,
                "press_key: window 7 is minimized or its application is hidden",
            ),
            (
                "desk_key",
                Os::Mac,
                &none,
                "press_key: window 7 cannot receive input: the screen is locked",
            ),
        ] {
            assert!(!pre_send(tool, os, args, e), "{tool} {e}");
        }
    }
}
