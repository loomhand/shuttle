//! shuttle: a local computer-use host that agents call over MCP stdio to drive this desktop.

pub mod cli;
pub mod daemon;
pub mod mcp;
pub mod proxy;
pub mod tools;

/// The program name in usage lines, endpoints and the MCP server info.
pub const NAME: &str = "cocoon-shuttle";
/// The release version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
