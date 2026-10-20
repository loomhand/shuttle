//! The desk verbs on macOS, Windows and Linux: the desk wire's desk calls over each OS's accessibility tree, input through cua-driver, and on Linux the computer verbs.

#[cfg(target_os = "linux")]
pub mod atspi;
#[cfg(target_os = "macos")]
pub mod ax;
pub mod ax_roles;
#[cfg(target_os = "linux")]
pub mod computer;
pub mod desk;
pub mod driver;
pub mod keys;
pub mod model;
#[cfg(windows)]
pub mod pipesec;
#[cfg(target_os = "linux")]
pub mod scope;
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
#[cfg(target_os = "linux")]
mod volatile;
