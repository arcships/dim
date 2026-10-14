//! Output side of the terminal interfaces: scrollback and fullscreen.

pub mod fullscreen;
pub mod markdown;
pub mod osc8;
pub mod panel;
pub mod screen;
#[cfg(test)]
pub mod testterm;

pub use fullscreen::FullScreen;
pub use screen::{Frame, Overlay, Screen};
