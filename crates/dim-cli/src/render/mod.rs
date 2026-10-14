//! Output side of the scrollback interface.

pub mod markdown;
pub mod osc8;
pub mod panel;
pub mod screen;
#[cfg(test)]
pub mod testterm;

pub use screen::{Frame, Screen};
