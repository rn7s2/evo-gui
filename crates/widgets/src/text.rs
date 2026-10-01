//! The design's medium weight in the current system-font stack.
//!
//! On macOS, `.SystemUIFont` at 500 rasterises like 400 in our captures.
//! Use the lightest visibly heavier face until the stack exposes a medium face.

use gpui_kit::FontWeight;

pub const MEDIUM: FontWeight = FontWeight::SEMIBOLD;
