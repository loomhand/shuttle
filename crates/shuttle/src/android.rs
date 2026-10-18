//! The daemon's desk on an Android device: the adb backend in place of this desktop's accessibility tree.

use desk_core::wire::{Call, Output};
use shuttle_android::adb::{Android, Device};
use shuttle_android::desk;
use shuttle_desk::server::Handler;
use shuttle_models::config::AndroidSettings;

/// The desk a daemon serves: this desktop's, or an Android device's when the config names one.
pub enum Desks<N> {
    Native(N),
    Android(Box<Android>),
}

impl<N: Handler> Handler for Desks<N> {
    async fn answer(&self, call: Call) -> Output {
        match self {
            Self::Native(native) => native.answer(call).await,
            Self::Android(android) => answer(android, call).await,
        }
    }
}

/// The Android device of `settings`.
pub fn device(settings: AndroidSettings) -> Android {
    Android::new(Device {
        adb: settings.adb,
        serial: settings.serial,
        ui_jar: settings.ui_jar,
        scrcpy_jar: settings.scrcpy_jar,
        su: settings.su,
    })
}

/// Whether the daemon starts this desktop's own driver, indicator and stop hotkey: never for an Android device.
pub fn local_controls(android: Option<&AndroidSettings>) -> bool {
    android.is_none()
}

async fn answer(android: &Android, call: Call) -> Output {
    let answered = match call.tool.as_str() {
        "desk_computer_state" => android.state(call.args).await,
        "desk_computer_act" => android.act(call.args).await,
        "desk_look" => android.look().await,
        verb => desk::run(android, verb, call.args).await,
    };
    answered.unwrap_or_else(Output::error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_android_device_takes_no_local_driver_indicator_or_hotkey() {
        let settings = AndroidSettings {
            adb: "adb".into(),
            serial: Some("127.0.0.1:5555".to_owned()),
            ui_jar: "/u.jar".into(),
            scrcpy_jar: "/s.jar".into(),
            su: "/su".into(),
        };
        assert!(!local_controls(Some(&settings)));
        assert!(local_controls(None));
    }
}
