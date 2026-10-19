//! The model configuration: one OpenAI-compatible endpoint, one key, and a model name per role.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const DEFAULT_KEY_ENV: &str = "OPENROUTER_API_KEY";

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
const DEFAULT_DECISIONS_URL: &str = "https://openrouter.ai/api/alpha/decisions";
const DEFAULT_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_KILL_HOTKEY: &str = if cfg!(target_os = "macos") {
    "ctrl+alt+cmd+escape"
} else {
    "ctrl+alt+shift+escape"
};

const DEFAULT_DECIDE: &str = "typesafe/jev-1.14";
const DEFAULT_GROUND: &str = "qwen/qwen3-vl-235b-a22b-instruct";
const DEFAULT_ASK: &str = "anthropic/claude-sonnet-5";

/// The model configuration; the default is OpenRouter with the key in `OPENROUTER_API_KEY`.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The API root that `/chat/completions` is appended to, such as `https://openrouter.ai/api/v1`.
    pub base_url: String,
    /// The whole URL of the decisions endpoint.
    pub decisions_url: String,
    pub api_key_env: String,
    pub api_key: Option<Secret>,
    pub timeout_ms: u64,
    pub decide_api: DecideApi,
    /// The builds a judgment may come from, such as `typesafe/jev-1.14-########` (`#` is one digit); unset takes any.
    pub decide_build: Option<String>,
    pub models: Roles,
    pub safety: Safety,
    pub tools: Tools,
    pub driver: DriverSettings,
    pub daemon: DaemonSettings,
    /// An Android device whose desk this daemon serves over adb, in place of this desktop's own.
    pub android: Option<AndroidSettings>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            decisions_url: DEFAULT_DECISIONS_URL.to_owned(),
            api_key_env: DEFAULT_KEY_ENV.to_owned(),
            api_key: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            decide_api: DecideApi::default(),
            decide_build: None,
            models: Roles::default(),
            safety: Safety::default(),
            tools: Tools::default(),
            driver: DriverSettings::default(),
            daemon: DaemonSettings::default(),
            android: None,
        }
    }
}

impl Config {
    /// Reads `path`, a missing file being the default; a file that holds an `api_key` must be readable by its owner only.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        };
        let config: Self = toml::from_str(&text).map_err(|e| {
            let line = e
                .span()
                .map_or(0, |at| text[..at.start].matches('\n').count() + 1);
            format!("{} line {line}: {}", path.display(), e.message())
        })?;
        if config.decide_build.as_deref() == Some("") {
            return Err(format!(
                "{}: decide_build is empty; remove it to take any build",
                path.display()
            ));
        }
        match config.daemon.socket_group.as_deref() {
            Some("") => {
                return Err(format!(
                    "{}: [daemon] socket_group is empty; remove it to keep the endpoint to its user",
                    path.display()
                ));
            }
            Some(_) if cfg!(windows) => {
                return Err(format!(
                    "{}: [daemon] socket_group needs a unix socket; the Windows pipe stays with its user",
                    path.display()
                ));
            }
            _ => {}
        }
        if config.android.is_some() && !config.safety.apps.is_empty() {
            return Err(format!(
                "{}: [safety] apps is not enforced on an [android] device, whose screenshots it does not mask either; remove apps or the [android] section",
                path.display()
            ));
        }
        if config.api_key.is_some() {
            owner_only(path)?;
        }
        Ok(config)
    }

    /// The key: `api_key` when set, else the `api_key_env` variable, else none.
    pub fn key(&self) -> Option<Secret> {
        self.api_key.clone().or_else(|| {
            std::env::var(&self.api_key_env)
                .ok()
                .filter(|k| !k.is_empty())
                .map(Secret)
        })
    }
}

/// How `decide` and `verify` reach their model: the decisions endpoint, or chat completions asked for the same answers as JSON.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DecideApi {
    #[default]
    Decisions,
    Chat,
}

/// The model name of each role; `verify` falls back to `decide`.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Roles {
    pub decide: String,
    pub verify: Option<String>,
    pub ground: String,
    pub ask: String,
}

impl Default for Roles {
    fn default() -> Self {
        Self {
            decide: DEFAULT_DECIDE.to_owned(),
            verify: None,
            ground: DEFAULT_GROUND.to_owned(),
            ask: DEFAULT_ASK.to_owned(),
        }
    }
}

/// The safety controls: `apps` limits the desk calls to windows of those apps, matched by app name or executable; empty allows every app.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Safety {
    pub apps: Vec<String>,
    /// The global key combination that stops shuttle, such as `ctrl+alt+cmd+escape`.
    pub kill_hotkey: String,
    /// The menu-bar item, or tray icon on Windows, that shows shuttle acting or stopped.
    pub indicator: bool,
    pub confirm: Confirm,
}

impl Default for Safety {
    fn default() -> Self {
        Self {
            apps: Vec::new(),
            kill_hotkey: DEFAULT_KILL_HOTKEY.to_owned(),
            indicator: true,
            confirm: Confirm::default(),
        }
    }
}

/// Which desk actions ask the user first: none, clicks at a screen point, or every action.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Confirm {
    #[default]
    Off,
    Coordinate,
    Every,
}

/// The tools a user can turn off, and the directory of plugin tools (default: `tools.d` next to the config file).
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tools {
    pub script_run: bool,
    pub plugins: Option<PathBuf>,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            script_run: true,
            plugins: None,
        }
    }
}

/// The bundled cua-driver: `overlay = false` starts it without its agent cursor, except on macOS, whose front-window tracking needs the overlay.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DriverSettings {
    pub overlay: bool,
}

impl Default for DriverSettings {
    fn default() -> Self {
        Self { overlay: true }
    }
}

/// The daemon's endpoint: `socket_group`, a group name or number, opens the unix socket to that group's members (mode 0660); unset keeps it to its user (0600).
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonSettings {
    pub socket_group: Option<String>,
}

/// The adb program, the device's serial, and the host files the UI server and the paste push to the device.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AndroidSettings {
    #[serde(default = "adb")]
    pub adb: PathBuf,
    pub serial: Option<String>,
    pub ui_jar: Option<PathBuf>,
    pub scrcpy_jar: PathBuf,
    pub su: Option<PathBuf>,
}

/// A key kept out of logs and error messages.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    /// The key itself, for the request header only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for Secret {
    fn from(key: String) -> Self {
        Self(key)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// The configuration file of program `name`: under `$XDG_CONFIG_HOME` or `~/.config`, `%APPDATA%` on Windows.
pub fn default_path(name: &str) -> PathBuf {
    let dir = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    dir.unwrap_or_default().join(name).join("config.toml")
}

#[cfg(unix)]
fn owner_only(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path)
        .map_err(|e| format!("stat {}: {e}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(format!(
            "{} holds an api_key and is readable by others (mode {:o}); run chmod 600 on it",
            path.display(),
            mode & 0o777
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn owner_only(_: &Path) -> Result<(), String> {
    Ok(())
}

fn adb() -> PathBuf {
    PathBuf::from("adb")
}
