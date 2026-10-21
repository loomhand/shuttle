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

    pub fn overlay_shown(&self) -> Result<bool, String> {
        match *self {}
    }

    pub fn rgb(&self) -> Result<(u32, u32, Vec<u8>), String> {
        match *self {}
    }

    pub fn region(&self, _: [i32; 4]) -> Result<Vec<u8>, String> {
        match *self {}
    }

    pub fn blocks(&self) -> Result<Vec<u64>, String> {
        match *self {}
    }

    pub fn pixels(&self) -> Result<&[u8], String> {
        match *self {}
    }

    pub fn docks(&self) -> Arc<[bool]> {
        match *self {}
    }

    pub fn shell_windows(&self) -> Vec<u32> {
        match *self {}
    }

    pub fn size(&self) -> (usize, usize) {
        match *self {}
    }

    pub fn mapped(&self) -> Result<Vec<Placement>, String> {
        match *self {}
    }

    pub fn title(&self, _: u32) -> Option<String> {
        match *self {}
    }

    pub fn active(&self) -> Option<u32> {
        match *self {}
    }

    pub fn wm(&self) -> Result<bool, String> {
        match *self {}
    }

    pub fn dialog(&self, _: u32) -> bool {
        match *self {}
    }

    pub fn modals(&self) -> Result<Vec<Modal>, String> {
        match *self {}
    }

    pub fn class(&self, _: u32) -> String {
        match *self {}
    }

    pub fn keyboard(&self, _: u32, _: u32) -> Result<bool, String> {
        match *self {}
    }

    pub fn grabbed(&self, _: bool) -> Result<bool, String> {
        match *self {}
    }

    pub fn top_at(&self, _: i32, _: i32) -> Result<Option<Placement>, String> {
        match *self {}
    }

    pub fn title_at(&self, _: i32, _: i32) -> Result<bool, String> {
        match *self {}
    }

    pub fn top_level(&self, _: u32) -> Result<u32, String> {
        match *self {}
    }

    pub fn client_at(&self, _: i32, _: i32) -> Result<Option<(String, Option<u32>)>, String> {
        match *self {}
    }

    pub fn pid(&self, _: u32) -> Option<u32> {
        match *self {}
    }
}
