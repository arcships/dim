//! How much of a streaming Markdown buffer is closed.
//!
//! A block's rendering depends on all of it (a table's column widths, a list's
//! numbering, a fence's highlighting), so only text before the start of the
//! last block can be committed. This is the contract of the current
//! `splitStreamingMarkdown`, byte offsets instead of UTF-16 units.

/// Result of [`split_streaming_markdown`].
#[derive(Debug, PartialEq, Eq)]
pub struct StreamingSplit {
    /// Byte length of the prefix made of closed blocks.
    pub settled_len: usize,
    /// Info string of a fence that is still open at the end of the buffer.
    pub open_fence_info: Option<String>,
    /// That fence's marker run (```` ``` ````, `~~~~`): it closes the fence.
    pub open_fence: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    None,
    List,
    Quote,
    Table,
}

fn is_list_item(line: &str) -> bool {
    let body = line.trim_start_matches(' ');
    if line.len() - body.len() > 3 {
        return false;
    }
    let mut chars = body.chars();
    match chars.next() {
        Some('-' | '+' | '*') => chars.next().is_some_and(char::is_whitespace),
        Some(c) if c.is_ascii_digit() => {
            let digits = body.chars().take_while(char::is_ascii_digit).count();
            digits <= 9
                && matches!(body[digits..].chars().next(), Some('.' | ')'))
                && body[digits + 1..]
                    .chars()
                    .next()
                    .is_some_and(char::is_whitespace)
        }
        _ => false,
    }
}

/// Two or more leading whitespace characters followed by non-whitespace.
fn is_indented(line: &str) -> bool {
    let lead = line.chars().take_while(|c| c.is_whitespace()).count();
    lead >= 2 && line.chars().nth(lead).is_some_and(|c| !c.is_whitespace())
}

fn continues_block(line: &str, context: Context) -> bool {
    match context {
        Context::List => is_list_item(line) || is_indented(line),
        Context::Quote => line.starts_with('>'),
        Context::Table => line.contains('|'),
        Context::None => false,
    }
}

fn block_context(line: &str, current: Context) -> Context {
    if is_list_item(line) {
        return Context::List;
    }
    if line.starts_with('>') {
        return Context::Quote;
    }
    if current == Context::List && is_indented(line) {
        return Context::List;
    }
    if line.contains('|') {
        Context::Table
    } else {
        Context::None
    }
}

/// An opening fence: up to three spaces, then three or more backticks or
/// tildes; returns (fence run, info string).
fn opening_fence(line: &str) -> Option<(&str, &str)> {
    let body = line.trim_start_matches([' ', '\t']);
    if line.chars().take_while(|c| c.is_whitespace()).count() > 3 {
        return None;
    }
    let marker = body.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let run = body.chars().take_while(|c| *c == marker).count();
    (run >= 3).then(|| (&body[..run], body[run..].trim()))
}

fn closes_fence(line: &str, fence: &str) -> bool {
    if line.chars().take_while(|c| c.is_whitespace()).count() > 3 {
        return false;
    }
    let body = line.trim();
    let marker = fence.chars().next().unwrap_or('`');
    body.len() >= fence.len() && body.chars().all(|c| c == marker)
}

pub fn split_streaming_markdown(text: &str) -> StreamingSplit {
    let mut fence: Option<String> = None;
    let mut fence_info: Option<String> = None;
    let mut offset = 0usize;
    let mut last_start = 0usize;
    let mut previous_blank = true;
    let mut context = Context::None;

    for line in text.split('\n') {
        let line_start = offset;
        offset += line.len() + 1;

        if let Some(open) = &fence {
            if closes_fence(line, open) {
                fence = None;
                fence_info = None;
                previous_blank = false;
                context = Context::None;
            }
            continue;
        }
        if let Some((run, info)) = opening_fence(line) {
            last_start = line_start;
            fence = Some(run.to_owned());
            fence_info = Some(info.to_owned());
            previous_blank = false;
            context = Context::None;
            continue;
        }
        if line.trim().is_empty() {
            previous_blank = true;
            continue;
        }
        if previous_blank && !continues_block(line, context) {
            last_start = line_start;
        }
        previous_blank = false;
        context = block_context(line, context);
    }
    StreamingSplit {
        settled_len: last_start.min(text.len()),
        open_fence_info: fence_info,
        open_fence: fence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled(text: &str) -> &str {
        &text[..split_streaming_markdown(text).settled_len]
    }

    #[test]
    fn a_single_unfinished_paragraph_is_never_settled() {
        assert_eq!(settled("Hello wor"), "");
        assert_eq!(settled("Hello world.\n"), "");
    }

    #[test]
    fn a_blank_line_then_new_text_settles_the_previous_block() {
        assert_eq!(settled("First.\n\nSecond"), "First.\n\n");
        assert_eq!(
            settled("First.\n\nSecond.\n\nThird"),
            "First.\n\nSecond.\n\n"
        );
    }

    #[test]
    fn an_open_fence_holds_everything_from_its_opening_line() {
        let text = "Intro.\n\n```rust\nfn main() {\n\n    println!();\n";
        let split = split_streaming_markdown(text);
        assert_eq!(&text[..split.settled_len], "Intro.\n\n");
        assert_eq!(split.open_fence_info.as_deref(), Some("rust"));
    }

    #[test]
    fn a_closed_fence_settles_once_the_next_block_starts() {
        let text = "```sh\nls\n```\n\nNext";
        let split = split_streaming_markdown(text);
        assert_eq!(&text[..split.settled_len], "```sh\nls\n```\n\n");
        assert_eq!(split.open_fence_info, None);
    }

    #[test]
    fn loose_lists_quotes_and_tables_stay_one_block() {
        let list = "1. one\n\n2. two\n\n3. thr";
        assert_eq!(settled(list), "");
        let quote = "> a\n\n> b\n\nplain";
        assert_eq!(settled(quote), "> a\n\n> b\n\n");
        let table = "| a | b |\n|---|---|\n| 1 | 2 |\n\n| 3 | 4 |";
        assert_eq!(settled(table), "");
        let nested = "- item\n\n  continued\n\nNext paragraph";
        assert_eq!(settled(nested), "- item\n\n  continued\n\n");
    }

    #[test]
    fn tilde_fences_and_longer_closers() {
        let text = "~~~\ncode\n~~~~\n\nafter";
        assert_eq!(settled(text), "~~~\ncode\n~~~~\n\n");
        // A shorter fence line does not close.
        let open = "````\ncode\n```\nstill code";
        assert_eq!(settled(open), "");
        assert_eq!(
            split_streaming_markdown(open).open_fence_info.as_deref(),
            Some("")
        );
    }

    #[test]
    fn offsets_are_byte_offsets_on_char_boundaries() {
        let text = "中文段落。\n\n第二段";
        assert!(text.is_char_boundary(split_streaming_markdown(text).settled_len));
        assert_eq!(settled(text), "中文段落。\n\n");
    }
}
