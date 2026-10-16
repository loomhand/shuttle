//! The toolhost wire format: one big-endian u32 length and a JSON body each way, one call per connection.

pub use desk_core::wire::{Call, Output, read_frame, write_frame};
