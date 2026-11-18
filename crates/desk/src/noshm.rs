//! The MIT-SHM grabber off Linux: there is none, so the computer verbs take their cua-driver paths.

use std::sync::Arc;

use crate::model::Modal;
pub use crate::screen::{Placement, block_hashes, clip, visible};

/// No grabber exists off Linux: [`Grabber::open`] always fails, so no other method can run.
pub enum Grabber {}

impl Grabber {
    pub fn open() -> Result<Self, String> {
        Err("no MIT-SHM capture on this OS".to_owned())
    }

    pub fn pixels(&mut self) -> Result<&[u8], String> {
        match *self {}
    }
}

macro_rules! never {
    ($($name:ident($($arg:ty),*) -> $out:ty;)*) => {
        impl Grabber {
            $(pub fn $name(&self, $(_: $arg),*) -> $out {
                match *self {}
            })*
        }
    };
}

never! {
    overlay_shown() -> Result<bool, String>;
    rgb() -> Result<(u32, u32, Vec<u8>), String>;
    region([i32; 4]) -> Result<Vec<u8>, String>;
    blocks() -> Result<Vec<u64>, String>;
    current() -> bool;
    docks() -> Arc<[bool]>;
    shell_windows() -> Vec<u32>;
    size() -> (usize, usize);
    mapped() -> Result<Vec<Placement>, String>;
    title(u32) -> Option<String>;
    active() -> Option<u32>;
    wm() -> Result<bool, String>;
    dialog(u32) -> bool;
    modals() -> Result<Vec<Modal>, String>;
    class(u32) -> String;
    keyboard(u32, u32) -> Result<bool, String>;
    grabbed(bool) -> Result<bool, String>;
    top_at(i32, i32) -> Result<Option<Placement>, String>;
    title_at(i32, i32) -> Result<bool, String>;
    top_level(u32) -> Result<u32, String>;
    pid(u32) -> Option<u32>;
}
