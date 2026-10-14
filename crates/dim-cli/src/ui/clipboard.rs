//! Copying text out of the fullscreen interface, which owns the mouse and
//! so cannot leave selection to the terminal.
//!
//! OSC 52 reaches the clipboard of the machine the terminal runs on, also
//! over SSH; terminals without it (Apple Terminal) are covered locally on
//! macOS by `pbcopy`.

/// The OSC 52 sequence that puts `text` on the terminal's clipboard.
pub fn osc52(text: &str) -> Vec<u8> {
    let mut out = b"\x1b]52;c;".to_vec();
    out.extend_from_slice(base64(text.as_bytes()).as_bytes());
    out.push(0x07);
    out
}

/// Also hands `text` to the local clipboard where one is reachable without
/// the terminal. Never blocks the caller.
pub fn copy_local(text: &str) {
    #[cfg(target_os = "macos")]
    if std::env::var_os("SSH_TTY").is_none() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let text = text.to_owned();
        std::thread::spawn(move || {
            // pbcopy reads its input in the locale's encoding.
            let Ok(mut child) = Command::new("pbcopy")
                .env("LANG", "en_US.UTF-8")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            else {
                return;
            };
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = text;
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_carries_base64_text() {
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(osc52("hi"), b"\x1b]52;c;aGk=\x07".to_vec());
    }
}
