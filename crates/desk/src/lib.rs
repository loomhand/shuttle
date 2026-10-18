//! The desk verbs on macOS and Windows: the toolhost wire's desk calls over each OS's accessibility tree, input through cua-driver.

#[cfg(target_os = "linux")]
pub mod atspi;
#[cfg(target_os = "macos")]
pub mod ax;
pub mod ax_roles;
pub mod desk;
pub mod driver;
pub mod keys;
pub mod model;
pub mod server;
#[cfg(windows)]
pub mod uia;
pub mod uia_roles;
pub mod worker;

mod allow;
mod press;
mod shot;
mod still;
