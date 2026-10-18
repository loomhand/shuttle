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
#[cfg(target_os = "linux")]
pub mod spi;
#[cfg(windows)]
pub mod uia;
pub mod uia_roles;
pub mod worker;
#[cfg(target_os = "linux")]
pub mod xshm;
#[cfg(target_os = "linux")]
pub mod xtest;

mod allow;
mod press;
mod shot;
mod still;
