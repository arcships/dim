use unicode_segmentation::UnicodeSegmentation;

use crate::sanitize::Sanitizer;

const FRAGMENT_BYTES: usize = 1024;

#[derive(Clone, Default)]
pub(super) struct Excerpt {
    sanitizer: Sanitizer,
    current: String,
    clipped: bool,
    complete: String,
    suffix: bool,
    dots: u8,
}

impl Excerpt {
    pub(super) fn push(&mut self, text: &str) {
        for raw in text.chars() {
            let Some(ch) = self.sanitizer.character(raw) else {
                continue;
            };
            if ch == '\n' {
                self.finish_unit();
                self.suffix = false;
            } else if ch.is_whitespace() {
                if self.dots == 1 {
                    self.finish_unit();
                }
                self.dots = 0;
                self.suffix = false;
                if !self.current.is_empty() && !self.current.ends_with(' ') {
                    self.append(' ');
                }
            } else if self.suffix
                && matches!(
                    ch,
                    '。' | '！'
                        | '？'
                        | '!'
                        | '?'
                        | '”'
                        | '’'
                        | '"'
                        | '\''
                        | ')'
                        | '）'
                        | ']'
                        | '」'
                        | '』'
                )
            {
                if self.complete.len() + ch.len_utf8() <= FRAGMENT_BYTES {
                    self.complete.push(ch);
                }
            } else {
                self.suffix = false;
                self.append(ch);
                match ch {
                    '。' | '！' | '？' | '!' | '?' => self.finish_unit(),
                    '.' => self.dots = self.dots.saturating_add(1),
                    '”' | '’' | '"' | '\'' | ')' | '）' | ']' | '」' | '』' => {}
                    _ => self.dots = 0,
                }
            }
        }
    }

    pub(super) fn finish(&mut self) {
        if self.dots == 1 {
            self.finish_unit();
        }
    }

    pub(super) fn text(&self) -> String {
        if !self.complete.is_empty() {
            self.complete.clone()
        } else if self.current.trim().is_empty() {
            String::new()
        } else {
            format!("{}…", self.current.trim_end())
        }
    }

    fn append(&mut self, ch: char) {
        if self.clipped {
            return;
        }
        if self.current.len() + ch.len_utf8() <= FRAGMENT_BYTES - '…'.len_utf8() {
            self.current.push(ch);
        } else {
            if let Some((at, _)) = self.current.grapheme_indices(true).next_back() {
                self.current.truncate(at);
            }
            self.clipped = true;
        }
    }

    fn finish_unit(&mut self) {
        if !self.current.trim().is_empty() {
            self.complete = self.current.trim().to_owned();
            if self.clipped {
                self.complete.push('…');
            }
            self.current.clear();
            self.clipped = false;
            self.suffix = true;
        }
        self.dots = 0;
    }
}
