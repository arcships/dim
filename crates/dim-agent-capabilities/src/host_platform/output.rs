//! Canonical command output rules shared by exec, the persistent shell and ShellInput:
//! one UTF-8 decoder, one sanitizer, one preview truncation and one pipe pump.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

pub const OUTPUT_PREVIEW_MAX_LINES: usize = 2_000;
pub const OUTPUT_PREVIEW_MAX_BYTES: usize = 50 * 1024;
pub const PREVIEW_BUFFER_HEADROOM_BYTES: usize = OUTPUT_PREVIEW_MAX_BYTES * 2;
pub const OUTPUT_FILE_BYTE_LIMIT: u64 = 64 * 1024 * 1024;
pub const EXIT_STDIO_GRACE: Duration = Duration::from_millis(100);

/// Incremental canonical UTF-8 decoding of one stream (ADR 018): invalid and
/// truncated bytes become `<0xNN>`, never U+FFFD.
#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
}
impl Decoder {
    pub fn write(&mut self, chunk: &[u8]) -> String {
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(chunk);
        self.decode(&input, false)
    }
    pub fn end(&mut self) -> String {
        let input = std::mem::take(&mut self.pending);
        self.decode(&input, true)
    }
    fn decode(&mut self, input: &[u8], last: bool) -> String {
        let mut output = String::new();
        let mut offset = 0;
        while offset < input.len() {
            let first = input[offset];
            if first <= 0x7f {
                output.push(first as char);
                offset += 1;
                continue;
            }
            let length = match first {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => 0,
            };
            if length == 0 {
                output.push_str(&escape_byte(first));
                offset += 1;
                continue;
            }
            if offset + length > input.len() {
                if !last {
                    self.pending = input[offset..].to_vec();
                    break;
                }
                output.push_str(&escape_byte(first));
                offset += 1;
                continue;
            }
            match std::str::from_utf8(&input[offset..offset + length]) {
                Ok(text) => {
                    output.push_str(text);
                    offset += length;
                }
                Err(_) => {
                    output.push_str(&escape_byte(first));
                    offset += 1;
                }
            }
        }
        output
    }
}

fn escape_byte(byte: u8) -> String {
    format!("<0x{byte:02X}>")
}

/// Removes control characters except `\t\n\r`, and U+FFF9–U+FFFB.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|&ch| {
            !(matches!(ch, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}')
                || matches!(ch, '\u{7f}'..='\u{9f}' | '\u{fff9}'..='\u{fffb}'))
        })
        .collect()
}

/// The head of `text` within `max` UTF-8 bytes, on a character boundary.
pub(crate) fn head_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The tail of `text` within `max` UTF-8 bytes, on a character boundary.
pub fn tail_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// TS `truncatePreview`: the last 2000 lines within 50 KiB.
pub fn truncate_preview(text: &str) -> String {
    let sanitized = sanitize(text);
    if sanitized.is_empty() {
        return sanitized;
    }
    let lines: Vec<&str> = sanitized.split('\n').collect();
    let tail = &lines[lines.len().saturating_sub(OUTPUT_PREVIEW_MAX_LINES)..];
    let joined = tail.join("\n");
    if joined.len() <= OUTPUT_PREVIEW_MAX_BYTES {
        return joined;
    }
    if tail.len() == 1 || (tail.len() == 2 && tail[1].is_empty()) {
        let suffix = if tail.len() == 2 { "\n" } else { "" };
        return format!(
            "{}{suffix}",
            tail_bytes(tail[0], OUTPUT_PREVIEW_MAX_BYTES - suffix.len())
        );
    }
    let (mut low, mut high) = (0, tail.len() - 1);
    while low < high {
        let middle = (low + high) / 2;
        if tail[middle..].join("\n").len() <= OUTPUT_PREVIEW_MAX_BYTES {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    let joined = tail[low..].join("\n");
    tail_bytes(&joined, OUTPUT_PREVIEW_MAX_BYTES).to_owned()
}

/// exec's preview bounds — the sanitized tail within 2,000 lines and 50 KiB —
/// and whether they left anything out: bytes outside the captured `preview`
/// buffer, or a sanitized tail the rule cut.
pub fn bounded_preview(preview: &str, decoded: usize) -> (String, bool) {
    let text = truncate_preview(preview);
    let truncated = decoded > preview.len() || text.len() < sanitize(preview).len();
    (text, truncated)
}

/// Appends `text` to the bounded output tail that [`truncate_preview`] reads:
/// past twice the preview budget, only the last part from a line start stays.
pub fn capture_preview(preview: &mut String, text: &str) {
    preview.push_str(text);
    if preview.len() > PREVIEW_BUFFER_HEADROOM_BYTES {
        let keep = tail_bytes(preview, PREVIEW_BUFFER_HEADROOM_BYTES);
        let keep = keep
            .find('\n')
            .filter(|index| index + 1 < keep.len())
            .map_or(keep, |index| &keep[index + 1..]);
        *preview = keep.to_owned();
    }
}

/// Where [`pump`] delivers the decoded text of a pipe, in read order.
pub trait OutputSink: Send + Sync {
    fn push(&self, text: &str) -> impl Future<Output = ()> + Send;
}

/// Reads one pipe through its own [`Decoder`] until EOF, a read error, or
/// [`EXIT_STDIO_GRACE`] of reading after `stop` (the root exited) fires.
pub async fn pump(
    mut pipe: impl tokio::io::AsyncRead + Unpin,
    output: Arc<impl OutputSink>,
    stop: CancellationToken,
) {
    let mut decoder = Decoder::default();
    let mut buffer = vec![0; 16 * 1024];
    let mut drain_budget = EXIT_STDIO_GRACE;
    loop {
        let draining = stop.is_cancelled();
        let started = Instant::now();
        let read = if draining {
            if drain_budget.is_zero() {
                break;
            }
            match tokio::time::timeout(drain_budget, pipe.read(&mut buffer)).await {
                Ok(read) => read,
                Err(_) => break,
            }
        } else {
            tokio::select! {
                read = pipe.read(&mut buffer) => read,
                _ = stop.cancelled() => continue,
            }
        };
        match read {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                let text = decoder.write(&buffer[..count]);
                if draining {
                    drain_budget = drain_budget.saturating_sub(started.elapsed());
                }
                output.push(&text).await;
            }
        }
    }
    output.push(&decoder.end()).await;
}
