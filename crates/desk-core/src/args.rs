//! The arguments of the desk calls, one struct per verb.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Decodes the arguments of one call.
pub fn parse<T: DeserializeOwned>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))
}

#[derive(Deserialize)]
pub struct SnapArgs {
    #[serde(default)]
    pub window: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub text: bool,
    #[serde(default)]
    pub keys: bool,
    #[serde(default)]
    pub bounds: Option<bool>,
    #[serde(default)]
    pub max_chars: Option<usize>,
}

#[derive(Deserialize)]
pub struct FindArgs {
    pub name: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub window: Option<String>,
    #[serde(default)]
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
    #[serde(default)]
    pub window: Option<String>,
    #[serde(default)]
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
    #[serde(default)]
    pub button: Option<String>,
    #[serde(default)]
    pub count: Option<u32>,
}

#[derive(Deserialize)]
pub struct WindowArgs {
    pub window: String,
    #[serde(default)]
    pub pid: Option<u32>,
}

#[derive(Deserialize)]
pub struct LaunchArgs {
    pub app: String,
}

#[derive(Deserialize)]
pub struct ShotArgs {
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
    #[serde(default)]
    pub w: Option<f64>,
    #[serde(default)]
    pub h: Option<f64>,
    #[serde(default)]
    pub path: Option<String>,
}
