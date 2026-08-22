//! Recording, mixing, and WAV writing.

mod level;
mod mic;
mod permission;
mod tap;
// The source-independent sink lands before the capture sources that consume it.
#[allow(dead_code)]
mod wav;
