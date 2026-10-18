//! shuttle: a local computer-use host that agents call over MCP stdio to drive this desktop.

pub mod cli;
pub mod daemon;
pub mod driver;
#[cfg(any(target_os = "macos", windows, test))]
pub mod hotkey;
#[cfg(target_os = "macos")]
pub mod mac;
pub mod mcp;
pub mod proxy;
pub mod runner;
pub mod tools;
#[cfg(windows)]
pub mod win;

/// The program name in usage lines, endpoints and the MCP server info.
pub const NAME: &str = "cocoon-shuttle";
/// The release version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The answer to every call while the user has stopped shuttle.
pub const STOPPED: &str = "stopped by the user: run `cocoon-shuttle resume` to go on";
