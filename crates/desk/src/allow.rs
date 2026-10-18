//! The app allowlist: which windows and elements the desk may list, read, shoot and act on.

use std::sync::Arc;

use desk_core::window;

use crate::model::Frame;

#[derive(Clone, Default)]
pub(crate) struct Allow(pub(crate) Arc<[String]>);

impl Allow {
    pub(crate) fn is_open(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn admits(&self, app: Option<&str>) -> bool {
        self.is_open() || app.is_some_and(|a| self.0.contains(&app_key(a)))
    }

    pub(crate) fn check(&self, app: Option<&str>) -> Result<(), String> {
        if self.admits(app) {
            Ok(())
        } else {
            Err(refused(app.unwrap_or("an unknown app")))
        }
    }

    pub(crate) fn pick(
        &self,
        mut frames: Vec<Frame>,
        want: Option<&str>,
        pid: Option<u32>,
    ) -> Result<Frame, String> {
        if want.is_none() || self.is_open() {
            let frame = window::pick(frames, want, pid)?;
            self.check(frame.app.as_deref())?;
            return Ok(frame);
        }
        frames.retain(|f| self.admits(f.app.as_deref()));
        window::pick(frames, want, pid)
    }
}

pub(crate) fn app_key(app: &str) -> String {
    let mut key = app.to_lowercase();
    if key.ends_with(".exe") {
        key.truncate(key.len() - 4);
    }
    key
}

pub(crate) fn app_of(frames: &[Frame], pid: Option<u32>) -> Option<&str> {
    frames
        .iter()
        .find(|f| pid.is_some() && f.pid == pid)
        .and_then(|f| f.app.as_deref())
}

fn refused(app: &str) -> String {
    format!("{app} is not in the app allowlist ([safety] apps in config.toml)")
}
