//! The `<thinking>` compatibility scan of one text block.
//!
//! The managed openai-codex provider writes its reasoning into the text
//! stream itself, wrapped in `<thinking>`…`</thinking>`. Only a tag that opens
//! the block converts: the first non-whitespace content of the block must be
//! the open tag, which the model emits as literal text. A tag anywhere later
//! is the model talking about the tag and stays text.

/// The open tag of a codex reasoning section inside a text block.
const OPEN: &str = "<thinking>";
/// The close tag of a codex reasoning section inside a text block.
const CLOSE: &str = "</thinking>";

/// What one scan step decided, in the order it must reach the stream.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ScanOutput {
    /// Text that is not a tag: append it to the block's part and stream it.
    Text(String),
    /// Thinking content: append it to the converted part and stream it.
    Thinking(String),
    /// The block opened with the tag: convert its part to thinking.
    Opened,
    /// The thinking section is complete, at the close tag or the block end,
    /// with its whole content.
    ThinkingEnd(String),
}

enum Scan {
    /// Nothing decided yet; everything so far is buffered because the open
    /// tag may still complete across deltas.
    Undecided(String),
    /// Inside the tag; `held` is the tail that may start a close tag.
    Thinking(String),
    /// Decided: the block is plain text (no tag, or the tag already closed).
    Text,
}

/// The scan state of one text block. The machine is byte-oriented over
/// `String`s: both tags are ASCII, so a match never falls inside a multi-byte
/// character and every split point is a character boundary.
pub(super) struct TagScan {
    scan: Scan,
    total: String,
}

impl TagScan {
    pub(super) fn new() -> Self {
        Self {
            scan: Scan::Undecided(String::new()),
            total: String::new(),
        }
    }

    /// Feeds one delta. The outputs are in stream order; `Opened` precedes
    /// the first `Thinking` it belongs to, and `ThinkingEnd` precedes any
    /// text that follows the close tag in the same delta.
    pub(super) fn feed(&mut self, delta: &str) -> Vec<ScanOutput> {
        let mut out = Vec::new();
        let mut rest = delta.to_owned();
        loop {
            match &mut self.scan {
                Scan::Undecided(pending) => {
                    pending.push_str(&rest);
                    rest.clear();
                    let trimmed = pending.trim_start();
                    if trimmed.starts_with(OPEN) {
                        // The whitespace before the tag is part of the tag's
                        // formatting, not text: keeping it would put a
                        // whitespace text block before the thinking.
                        let after = trimmed[OPEN.len()..].to_owned();
                        self.scan = Scan::Thinking(String::new());
                        out.push(ScanOutput::Opened);
                        rest = after;
                        if rest.is_empty() {
                            return out;
                        }
                        continue;
                    }
                    if OPEN.starts_with(trimmed) {
                        // Nothing yet, or a strict prefix of the open tag; the
                        // decision waits for more of the block.
                        return out;
                    }
                    // Not the tag: everything buffered so far is text,
                    // whitespace included, and the block is text from here.
                    let text = std::mem::take(pending);
                    self.scan = Scan::Text;
                    if !text.is_empty() {
                        out.push(ScanOutput::Text(text));
                    }
                    return out;
                }
                Scan::Thinking(held) => {
                    held.push_str(&rest);
                    rest.clear();
                    if let Some(pos) = held.find(CLOSE) {
                        let before = held[..pos].to_owned();
                        let after = held[pos + CLOSE.len()..].to_owned();
                        if !before.is_empty() {
                            self.total.push_str(&before);
                            out.push(ScanOutput::Thinking(before));
                        }
                        let total = std::mem::take(&mut self.total);
                        out.push(ScanOutput::ThinkingEnd(total));
                        self.scan = Scan::Text;
                        rest = after;
                        if rest.is_empty() {
                            return out;
                        }
                        continue;
                    }
                    // Hold back the longest suffix that may still complete
                    // into the close tag; everything before it is thinking.
                    let hold = close_prefix_suffix(held);
                    let emit = held[..held.len() - hold].to_owned();
                    *held = held[held.len() - hold..].to_owned();
                    if !emit.is_empty() {
                        self.total.push_str(&emit);
                        out.push(ScanOutput::Thinking(emit));
                    }
                    return out;
                }
                Scan::Text => {
                    if !rest.is_empty() {
                        out.push(ScanOutput::Text(std::mem::take(&mut rest)));
                    }
                    return out;
                }
            }
        }
    }

    /// The block ended. A decision never reached is text; an unclosed tag
    /// keeps its content as thinking.
    pub(super) fn end(&mut self) -> Vec<ScanOutput> {
        let mut out = Vec::new();
        match &mut self.scan {
            Scan::Undecided(pending) => {
                let text = std::mem::take(pending);
                self.scan = Scan::Text;
                if !text.is_empty() {
                    out.push(ScanOutput::Text(text));
                }
            }
            Scan::Thinking(held) => {
                let rest = std::mem::take(held);
                if !rest.is_empty() {
                    self.total.push_str(&rest);
                    out.push(ScanOutput::Thinking(rest));
                }
                let total = std::mem::take(&mut self.total);
                out.push(ScanOutput::ThinkingEnd(total));
                self.scan = Scan::Text;
            }
            Scan::Text => {}
        }
        out
    }
}

/// The length of the longest suffix of `held` that is a strict prefix of the
/// close tag, which the next delta may complete.
fn close_prefix_suffix(held: &str) -> usize {
    let max = held.len().min(CLOSE.len() - 1);
    (1..=max)
        .rev()
        .find(|&len| {
            // The suffix starts with '<', so a match never begins inside a
            // multi-byte character; other lengths are not candidates.
            held.is_char_boundary(held.len() - len) && CLOSE.starts_with(&held[held.len() - len..])
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(parts: &[&str]) -> Vec<ScanOutput> {
        let mut scan = TagScan::new();
        let mut out = Vec::new();
        for part in parts {
            out.extend(scan.feed(part));
        }
        out.extend(scan.end());
        out
    }

    #[test]
    fn the_close_tag_never_splits_inside_a_character() {
        assert_eq!(close_prefix_suffix("héllo</th"), 4);
        assert_eq!(close_prefix_suffix("</thinking"), 10);
        assert_eq!(close_prefix_suffix("</thinking>"), 0);
        assert_eq!(close_prefix_suffix("plain"), 0);
    }

    #[test]
    fn machine_examples() {
        // The byte check above pins the boundary arithmetic; the stream-level
        // tests in stream/tests.rs cover the machine through convert, where
        // its outputs have their meaning.
        assert_eq!(
            scan(&["<thinking>thought</thinking>answer"]),
            vec![
                ScanOutput::Opened,
                ScanOutput::Thinking("thought".into()),
                ScanOutput::ThinkingEnd("thought".into()),
                ScanOutput::Text("answer".into()),
            ]
        );
    }
}
