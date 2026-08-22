//! Portable artifact rendering.

mod json;
mod markdown;

pub use json::{
    read_transcript as read_transcript_json, write_transcript as write_transcript_json,
};
#[allow(unused_imports)]
pub use markdown::write_transcript;
