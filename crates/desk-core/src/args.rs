//! The arguments of the desk calls, one struct per verb.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

#[derive(Deserialize)]
pub struct SnapArgs {
    pub window: Option<String>,
    pub id: Option<String>,
    pub pid: Option<u32>,
    #[serde(default)]
    pub text: bool,
    #[serde(default)]
    pub keys: bool,
    #[serde(default)]
    pub menus: bool,
    pub bounds: Option<bool>,
    pub max_chars: Option<usize>,
}

#[derive(Deserialize)]
pub struct WaitArgs {
    /// The window list as `desk_windows` gave it.
    #[serde(default)]
    pub since: Value,
    pub timeout_ms: u64,
}

#[derive(Deserialize)]
pub struct FindArgs {
    pub name: String,
    pub role: Option<String>,
    pub window: Option<String>,
    pub pid: Option<u32>,
}

#[derive(Deserialize)]
pub struct ReadArgs {
    pub id: String,
    #[serde(default)]
    pub text: bool,
    /// Adds the page URL and DOM id of a web element.
    #[serde(default)]
    pub web: bool,
}

#[derive(Deserialize)]
pub struct IdArgs {
    pub id: String,
}

#[derive(Deserialize)]
pub struct ActArgs {
    pub id: String,
    pub action: String,
}

#[derive(Deserialize)]
pub struct AtArgs {
    pub x: i32,
    pub y: i32,
    pub window: Option<String>,
    pub pid: Option<u32>,
}

#[derive(Deserialize)]
pub struct KeyArgs {
    pub keys: String,
    /// The window that background input goes to, by title; foreground input on macOS brings it to the front first.
    pub window: Option<String>,
    pub pid: Option<u32>,
}

#[derive(Deserialize)]
pub struct TypeArgs {
    pub text: String,
    /// The window that background input goes to, by title; foreground input on macOS brings it to the front first.
    pub window: Option<String>,
    pub pid: Option<u32>,
    /// `unverified` types after a background click into a window whose keyboard focus cannot be read.
    pub focus: Option<Focus>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Focus {
    Unverified,
}

#[derive(Deserialize)]
pub struct ClickArgs {
    pub x: f64,
    pub y: f64,
    pub button: Option<String>,
    pub count: Option<u32>,
    /// Modifier keys held during the click: `cmd`, `ctrl`, `alt`, `shift`.
    pub modifiers: Option<Vec<String>>,
    /// The window that background input goes to, by title, and foreground input on macOS after it brings it to the front; `x` and `y` are then pixels of its `shot`.
    pub window: Option<String>,
    pub pid: Option<u32>,
}

#[derive(Deserialize)]
pub struct ScrollArgs {
    /// `up`, `down`, `left` or `right`.
    pub direction: String,
    pub amount: Option<u32>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    /// The window that background input goes to, by title; `x` and `y` are then pixels of its `shot`.
    pub window: Option<String>,
    pub pid: Option<u32>,
}

#[derive(Deserialize)]
pub struct DragArgs {
    pub x: f64,
    pub y: f64,
    pub to_x: f64,
    pub to_y: f64,
    /// The window that background input goes to, by title; the points are then pixels of its `shot`.
    pub window: Option<String>,
    pub pid: Option<u32>,
}

#[derive(Deserialize)]
pub struct StillArgs {
    pub timeout_ms: Option<u64>,
    pub hold_ms: Option<u64>,
}

#[derive(Deserialize)]
pub struct WindowArgs {
    pub window: String,
    pub pid: Option<u32>,
}

#[derive(Deserialize)]
pub struct LaunchArgs {
    pub app: String,
    /// Command-line arguments for a new instance of the app, such as a browser profile and a URL.
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Deserialize)]
pub struct WebArgs {
    pub pid: u32,
    pub on: bool,
    #[serde(default)]
    pub holder: String,
}

#[derive(Deserialize)]
pub struct ShotArgs {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub w: Option<f64>,
    pub h: Option<f64>,
    pub path: Option<String>,
    /// The window to capture by itself, on whichever display shows it; x, y, w, h then crop its image.
    pub window: Option<String>,
    pub pid: Option<u32>,
}

/// Decodes the arguments of one call.
pub fn parse<T: DeserializeOwned>(args: Value) -> Result<T, String> {
    let args = if args.is_null() {
        Value::Object(Map::new())
    } else {
        args
    };
    serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_call_without_args_takes_every_default() {
        let snap: SnapArgs = parse(Value::Null).unwrap();
        assert!(snap.window.is_none() && !snap.text);
        assert!(parse::<KeyArgs>(Value::Null).is_err());
        assert_eq!(
            parse::<KeyArgs>(json!({"keys": "cmd+s"})).unwrap().keys,
            "cmd+s"
        );
    }
}
