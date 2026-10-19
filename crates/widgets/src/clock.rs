//! The clock the animated widgets read.
//!
//! `std::time::Instant::now` is worse than missing on `wasm32-unknown-unknown`: the
//! standard library's wasm backend compiles it to
//! `panic!("time not implemented on this platform")`, so a browser build that reaches
//! it aborts — and every transition in this crate reads it (the slider's first frame,
//! the transcript's scroll window). A browser has a clock, so the answer is not to
//! take the animation away but to ask the right one: `web-time`'s `Instant` is
//! `Performance.now()` there and a plain re-export of `std::time` everywhere else, so
//! on a desktop this is the standard clock, unchanged, and on the web it is the
//! browser's own.
//!
//! One module rather than an import per file: this is the single place that decides
//! which clock the UI reads, and a widget that needs the time says so by importing
//! from here.

#[cfg(not(target_family = "wasm"))]
pub use std::time::Instant;
#[cfg(target_family = "wasm")]
pub use web_time::Instant;
