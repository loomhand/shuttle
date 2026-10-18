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
}

#[derive(Deserialize)]
pub struct TypeArgs {
    pub text: String,
}

#[derive(Deserialize)]
pub struct ClickArgs {
    pub x: f64,
    pub y: f64,
    pub button: Option<String>,
    pub count: Option<u32>,
}

#[derive(Deserialize)]
pub struct WindowArgs {
    pub window: String,
    pub pid: Option<u32>,
}

#[derive(Deserialize)]
pub struct LaunchArgs {
    pub app: String,
}

#[derive(Deserialize)]
pub struct ShotArgs {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub w: Option<f64>,
    pub h: Option<f64>,
    pub path: Option<String>,
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
