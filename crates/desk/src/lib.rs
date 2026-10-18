//! The desk verbs on macOS and Windows: the toolhost wire's desk calls over each OS's accessibility tree, input through cua-driver.

mod allow;
#[cfg(target_os = "macos")]
pub mod ax;
pub mod ax_roles;
pub mod desk;
pub mod driver;
pub mod keys;
pub mod model;
mod press;
pub mod server;
mod shot;
#[cfg(windows)]
pub mod uia;
pub mod uia_roles;
pub mod worker;
