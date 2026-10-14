//! Terminal modes the process turns on and must hand back.
//!
//! Everything pushed into the terminal outlives the process: nothing resets it
//! when we die. The set of live modes is therefore tracked in one atomic word
//! so the exit path, the panic hook and the signal handler can all write the
//! same restore sequence without locks or allocation.

use std::sync::atomic::{AtomicU8, Ordering};

/// One terminal-owned mode this process may have enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Kitty keyboard protocol flags pushed with `CSI > flags u`.
    KittyKeyboard,
    /// xterm modifyOtherKeys level 2.
    ModifyOtherKeys,
    /// Bracketed paste (`?2004`).
    BracketedPaste,
    /// Hardware cursor hidden (`?25l`).
    CursorHidden,
    /// An OSC 9;4 progress indicator is showing.
    Progress,
    /// The alternate screen of the fullscreen interface (`?1049`).
    AltScreen,
    /// Button, drag and wheel reports in SGR encoding (`?1002` + `?1006`).
    Mouse,
}

impl Mode {
    const fn bit(self) -> u8 {
        match self {
            Self::KittyKeyboard => 1,
            Self::ModifyOtherKeys => 2,
            Self::BracketedPaste => 4,
            Self::CursorHidden => 8,
            Self::Progress => 16,
            Self::AltScreen => 32,
            Self::Mouse => 64,
        }
    }

    /// The sequence that turns the mode on.
    pub const fn enable_sequence(self) -> &'static [u8] {
        match self {
            // flags 1 (disambiguate) | 4 (alternate keys)
            Self::KittyKeyboard => b"\x1b[>5u",
            Self::ModifyOtherKeys => b"\x1b[>4;2m",
            Self::BracketedPaste => b"\x1b[?2004h",
            Self::CursorHidden => b"\x1b[?25l",
            Self::Progress => b"",
            Self::AltScreen => b"\x1b[?1049h",
            Self::Mouse => b"\x1b[?1002h\x1b[?1006h",
        }
    }

    /// The sequence that undoes the mode.
    pub const fn restore_sequence(self) -> &'static [u8] {
        match self {
            Self::KittyKeyboard => b"\x1b[<u",
            Self::ModifyOtherKeys => b"\x1b[>4;0m",
            Self::BracketedPaste => b"\x1b[?2004l",
            Self::CursorHidden => b"\x1b[?25h",
            Self::Progress => b"\x1b]9;4;0\x07",
            Self::AltScreen => b"\x1b[?1049l",
            Self::Mouse => b"\x1b[?1006l\x1b[?1002l",
        }
    }
}

/// The modes currently on, shared with signal handlers.
static LIVE: AtomicU8 = AtomicU8::new(0);

pub fn mark(mode: Mode) {
    LIVE.fetch_or(mode.bit(), Ordering::SeqCst);
}

pub fn clear(mode: Mode) {
    LIVE.fetch_and(!mode.bit(), Ordering::SeqCst);
}

/// Forgets every live mode: the terminal has been handed back.
pub fn clear_all() {
    LIVE.store(0, Ordering::SeqCst);
}

pub fn is_live(mode: Mode) -> bool {
    LIVE.load(Ordering::SeqCst) & mode.bit() != 0
}

/// Longest restore sequence: every mode plus the trailing CRLF.
pub const MAX_RESTORE_LEN: usize = 128;

/// Writes the sequence that undoes every live mode into `out` and returns its
/// length. Order matters: the keyboard protocols and mouse reports go first so
/// a report emitted while we tear down is not read by the shell as text; the
/// alternate screen is left before the cursor and CRLF reach the shell's
/// screen. Always ends
/// with showing the cursor and bracketed paste off (cheap and idempotent), then
/// a CRLF so the shell prompt starts on a fresh line.
pub fn restore_into(live: u8, out: &mut [u8; MAX_RESTORE_LEN]) -> usize {
    let mut len = 0;
    // A signal can land inside a synchronized frame or mid-SGR: close the
    // synchronized update (the terminal would otherwise hold its display) and
    // drop any styling before the shell prompt appears.
    const PREAMBLE: &[u8] = b"\x1b[?2026l\x1b[0m";
    let mut put = |bytes: &[u8], out: &mut [u8; MAX_RESTORE_LEN]| {
        out[len..len + bytes.len()].copy_from_slice(bytes);
        len += bytes.len();
    };
    put(PREAMBLE, out);
    for mode in [
        Mode::KittyKeyboard,
        Mode::ModifyOtherKeys,
        Mode::Mouse,
        Mode::Progress,
        Mode::AltScreen,
    ] {
        if live & mode.bit() != 0 {
            put(mode.restore_sequence(), out);
        }
    }
    put(Mode::BracketedPaste.restore_sequence(), out);
    put(Mode::CursorHidden.restore_sequence(), out);
    put(b"\r\n", out);
    len
}

/// Snapshot of the live set for `restore_into`.
pub fn live_bits() -> u8 {
    LIVE.load(Ordering::SeqCst)
}

#[cfg(test)]
pub fn live_bits_for(modes: &[Mode]) -> u8 {
    modes.iter().fold(0, |bits, mode| bits | mode.bit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restore(modes: &[Mode]) -> Vec<u8> {
        let mut out = [0u8; MAX_RESTORE_LEN];
        let len = restore_into(live_bits_for(modes), &mut out);
        out[..len].to_vec()
    }

    #[test]
    fn restore_always_resets_paste_and_cursor() {
        assert_eq!(
            restore(&[]),
            b"\x1b[?2026l\x1b[0m\x1b[?2004l\x1b[?25h\r\n".to_vec()
        );
    }

    #[test]
    fn keyboard_protocols_are_popped_first() {
        let bytes = restore(&[Mode::KittyKeyboard, Mode::ModifyOtherKeys, Mode::Progress]);
        assert_eq!(
            bytes,
            b"\x1b[?2026l\x1b[0m\x1b[<u\x1b[>4;0m\x1b]9;4;0\x07\x1b[?2004l\x1b[?25h\r\n".to_vec()
        );
    }

    #[test]
    fn worst_case_fits_the_buffer() {
        let all = [
            Mode::KittyKeyboard,
            Mode::ModifyOtherKeys,
            Mode::BracketedPaste,
            Mode::CursorHidden,
            Mode::Progress,
            Mode::AltScreen,
            Mode::Mouse,
        ];
        assert!(restore(&all).len() <= MAX_RESTORE_LEN);
    }
}
