//! shuttle: a local computer-use host that agents call over MCP stdio to drive this desktop.

pub mod cli;
pub mod mcp;
pub mod tools;

/// The program name in usage lines, endpoints and the MCP server info.
pub const NAME: &str = "shuttle";
/// The release version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
