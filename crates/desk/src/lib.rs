//! The desk host of a Windows or macOS box: the Linux toolhost's desk calls over the same framing.

#[cfg(target_os = "macos")]
pub mod ax;
pub mod ax_roles;
pub mod desk;
pub mod driver;
pub mod keys;
pub mod model;
pub mod rpc;
pub mod server;
#[cfg(windows)]
pub mod uia;
pub mod uia_roles;
pub mod worker;
