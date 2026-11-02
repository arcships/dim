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

/// Rows available to live controls, retaining history unless fewer than
/// eight control rows would remain; smaller terminals use their full height.
pub fn live_row_budget(rows: usize, history: usize) -> usize {
    rows.saturating_sub(history).max(rows.min(8)).max(1)
}
