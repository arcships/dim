//! Keyboard protocol negotiation and the teardown fence.
//!
//! Startup asks the terminal whether it speaks the kitty keyboard protocol
//! (`CSI ? u`) and follows with a DA1 query (`CSI c`) as a sentinel: every
//! terminal answers DA1, and replies arrive in order, so a kitty answer that
//! is not there when DA1 arrives means "unsupported". Teardown pops the flags
//! and then uses DA1 again as a fence, so key-release reports the terminal
//! emitted before it applied the pop are drained instead of reaching the
//! user's shell (fish reads `ESC [ 100 ; 5 : 3 u` as Ctrl-D and exits).

/// Which key encoding the terminal was switched to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardProtocol {
    /// Kitty flags 1|4 pushed; modified keys arrive as `CSI u`.
    Kitty,
    /// xterm modifyOtherKeys level 2.
    ModifyOtherKeys,
    /// Neither answered; legacy encoding only.
    Legacy,
}

/// Queries sent at startup: kitty flags, the background colour (OSC 11), then
/// the DA1 sentinel. Replies arrive in order, so DA1 proves the others are
/// either here or never coming.
pub const PROBE_QUERY: &[u8] = b"\x1b[?u\x1b]11;?\x07\x1b[c";
/// The DA1 sentinel alone, used as the teardown fence.
pub const FENCE_QUERY: &[u8] = b"\x1b[c";

/// Result of scanning the bytes read after [`PROBE_QUERY`].
#[derive(Debug, PartialEq, Eq)]
pub struct ProbeScan {
    /// A `CSI ? <flags> u` report was seen.
    pub kitty: bool,
    /// The DA1 reply arrived; nothing more is coming.
    pub complete: bool,
    /// The body of an OSC 11 report (`rgb:RRRR/GGGG/BBBB`), if one arrived.
    pub background_report: Option<String>,
    /// Bytes before the DA1 reply that are neither reply: user typeahead.
    pub typeahead: Vec<u8>,
    /// Bytes after the DA1 reply.
    pub rest: Vec<u8>,
}

pub fn scan_probe(bytes: &[u8]) -> ProbeScan {
    let mut kitty = false;
    let mut background_report = None;
    let mut typeahead = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'[') {
            if let Some((len, kind)) = reply_at(&bytes[index..]) {
                match kind {
                    ReplyKind::KittyFlags => kitty = true,
                    ReplyKind::Da1 => {
                        return ProbeScan {
                            kitty,
                            complete: true,
                            background_report,
                            typeahead,
                            rest: bytes[index + len..].to_vec(),
                        };
                    }
                    ReplyKind::Other => {}
                }
                index += len;
                continue;
            }
        }
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b']') {
            if let Some((len, body)) = osc_at(&bytes[index..]) {
                if let Some(report) = body.strip_prefix("11;") {
                    background_report = Some(report.to_owned());
                }
                index += len;
                continue;
            }
        }
        typeahead.push(bytes[index]);
        index += 1;
    }
    ProbeScan {
        kitty,
        complete: false,
        background_report,
        typeahead,
        rest: Vec::new(),
    }
}

/// A complete OSC string at the start of `bytes`: its length and body.
fn osc_at(bytes: &[u8]) -> Option<(usize, String)> {
    let body = &bytes[2..];
    for (i, &b) in body.iter().enumerate() {
        if b == 0x07 {
            return Some((2 + i + 1, String::from_utf8_lossy(&body[..i]).into_owned()));
        }
        if b == 0x1b && body.get(i + 1) == Some(&b'\\') {
            return Some((2 + i + 2, String::from_utf8_lossy(&body[..i]).into_owned()));
        }
    }
    None
}

enum ReplyKind {
    KittyFlags,
    Da1,
    Other,
}

/// A complete private reply at the start of `bytes`: `ESC [ ? params final`.
fn reply_at(bytes: &[u8]) -> Option<(usize, ReplyKind)> {
    if bytes.get(2) != Some(&b'?') {
        return None;
    }
    let mut index = 3;
    while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b';') {
        index += 1;
    }
    let final_byte = *bytes.get(index)?;
    let kind = match final_byte {
        b'u' => ReplyKind::KittyFlags,
        b'c' => ReplyKind::Da1,
        _ => ReplyKind::Other,
    };
    Some((index + 1, kind))
}

/// `Some(len)` when `tail` ends with a complete DA1 reply `ESC [ ? [0-9;]* c`.
/// DA2 (`CSI >`), kitty reports and key events never match, so a key report
/// cannot end the drain early.
pub fn da1_reply_len(tail: &[u8]) -> Option<usize> {
    let (&last, before) = tail.split_last()?;
    if last != b'c' {
        return None;
    }
    let params = before
        .iter()
        .rev()
        .take_while(|b| b.is_ascii_digit() || **b == b';')
        .count();
    let intro = &before[..before.len() - params];
    intro.ends_with(b"\x1b[?").then_some(3 + params + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_reply_before_da1_means_supported() {
        let scan = scan_probe(b"\x1b[?0u\x1b[?62;4c");
        assert!(scan.kitty && scan.complete);
        assert!(scan.typeahead.is_empty() && scan.rest.is_empty());
    }

    #[test]
    fn da1_alone_means_unsupported() {
        let scan = scan_probe(b"\x1b[?64;1;2c");
        assert!(!scan.kitty && scan.complete);
    }

    #[test]
    fn typeahead_is_preserved_and_order_does_not_matter() {
        let scan = scan_probe(b"ab\x1b[?1u\x1b[?62cde");
        assert!(scan.kitty && scan.complete);
        assert_eq!(scan.typeahead, b"ab");
        assert_eq!(scan.rest, b"de");
    }

    #[test]
    fn background_report_is_captured_and_never_leaks_as_typeahead() {
        let scan = scan_probe(b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07\x1b[?62c");
        assert_eq!(
            scan.background_report.as_deref(),
            Some("rgb:1e1e/1e1e/1e1e")
        );
        assert!(scan.typeahead.is_empty() && scan.complete);
        // ST-terminated, and with the kitty report ahead of it.
        let scan = scan_probe(b"\x1b[?0u\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1b[?62c");
        assert!(scan.kitty);
        assert_eq!(
            scan.background_report.as_deref(),
            Some("rgb:ffff/ffff/ffff")
        );
        assert!(scan.typeahead.is_empty());
    }

    #[test]
    fn incomplete_reply_is_not_complete() {
        let scan = scan_probe(b"\x1b[?62;");
        assert!(!scan.complete);
    }

    #[test]
    fn fence_matches_da1_only() {
        assert_eq!(da1_reply_len(b"\x1b[?62;c"), Some(7));
        assert_eq!(da1_reply_len(b"\x1b[>1;10;0c"), None);
        assert_eq!(da1_reply_len(b"\x1b[99;5:3u"), None);
        assert_eq!(da1_reply_len(b"zz\x1b[?6c"), Some(5));
    }
}
