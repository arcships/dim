//! Local input both clients attach to a message: the limits a draft keeps
//! to, and the files a paste names.

use std::path::{Path, PathBuf};

/// A paste longer than this is stored as a session attachment
/// (`attachment.put`) instead of being sent inline.
pub const INLINE_PASTE: usize = 16 * 1024;
/// Longest paste a draft accepts.
pub const MAX_PASTE: usize = 10 * 1024 * 1024;
/// Images one submission accepts.
pub const MAX_IMAGES: usize = 20;

/// The local path a `file:` URL names, if it is local.
pub fn file_url(value: &str) -> Option<PathBuf> {
    let url = url::Url::parse(value).ok()?;
    if url.scheme() != "file" || !matches!(url.host_str(), None | Some("localhost")) {
        return None;
    }
    let path = url.to_file_path().ok()?;
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::{NSString, NSURL};
        objc2::rc::autoreleasepool(|_| {
            let native = NSURL::URLWithString(&NSString::from_str(url.as_str()))?;
            if native.isFileReferenceURL() {
                return native
                    .filePathURL()?
                    .path()
                    .map(|path| PathBuf::from(path.to_string()));
            }
            Some(path)
        })
    }
    #[cfg(not(target_os = "macos"))]
    Some(path)
}

fn local_path(value: &str, base: &Path) -> Option<PathBuf> {
    if value.starts_with("file:") {
        file_url(value)
    } else if value.starts_with("http://") || value.starts_with("https://") {
        None
    } else {
        let path = base.join(value);
        #[cfg(target_os = "macos")]
        if path.starts_with("/.file") {
            return file_url(url::Url::from_file_path(path).ok()?.as_str());
        }
        Some(path)
    }
}

/// The local files a paste names: one path, or several quoted or
/// backslash-escaped ones, `file:` URLs included; `None` unless every one
/// is an existing file.
pub fn pasted_paths(text: &str, base: &Path) -> Option<Vec<PathBuf>> {
    pasted(text, base, Path::is_file)
}

/// The local files and directories a paste names, as a terminal pastes a
/// drag: one path or several per line, one line per path or group;
/// `None` unless every one exists.
pub fn pasted_entries(text: &str, base: &Path) -> Option<Vec<PathBuf>> {
    pasted(text, base, Path::exists)
}

fn pasted(text: &str, base: &Path, exists: fn(&Path) -> bool) -> Option<Vec<PathBuf>> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        return None;
    }
    let mut paths = Vec::new();
    for line in lines {
        paths.extend(pasted_line(line, base, exists)?);
    }
    Some(paths)
}

/// One line: the whole line as a path (raw spaces included), else its
/// shell words, each of which must be a path.
fn pasted_line(value: &str, base: &Path, exists: fn(&Path) -> bool) -> Option<Vec<PathBuf>> {
    let whole = strip_quotes(value);
    if let Some(path) = local_path(whole, base).filter(|path| exists(path)) {
        return Some(vec![path]);
    }
    let tokens = shell_words(value)?;
    if tokens.is_empty() {
        return None;
    }
    tokens
        .iter()
        .map(|token| local_path(token, base).filter(|path| exists(path)))
        .collect()
}

/// `value` without one pair of matching quotes or backticks around it.
fn strip_quotes(value: &str) -> &str {
    for quote in ['\'', '"', '`'] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// Words split at unquoted whitespace, with quotes and escapes removed. A
/// backslash escapes any character, as a POSIX shell's drag does, except
/// on Windows, where it separates path components and escapes only
/// whitespace or a quote.
fn shell_words(value: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if quote != Some('\'')
                && chars.peek().is_some_and(|next| {
                    !cfg!(windows) || next.is_whitespace() || matches!(next, '\'' | '"')
                }) =>
            {
                token.push(chars.next()?)
            }
            '\'' | '"' if quote == Some(ch) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(ch),
            ch if ch.is_whitespace() && quote.is_none() => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
            }
            ch => token.push(ch),
        }
    }
    if quote.is_some() {
        return None;
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    Some(tokens)
}

/// A Markdown link to a local file: `[label](path)`, the path in angle
/// brackets when it has spaces or parentheses.
pub fn file_link(label: &str, path: &Path) -> String {
    let mut text = String::with_capacity(label.len());
    for ch in label.chars() {
        if matches!(ch, '[' | ']' | '\\') {
            text.push('\\');
        }
        text.push(ch);
    }
    let target = path.to_string_lossy();
    if target.contains(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>')) {
        let target = target.replace('<', "%3C").replace('>', "%3E");
        format!("[{text}](<{target}>)")
    } else {
        format!("[{text}]({target})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_entries_read_drags_as_terminals_paste_them() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let spaced = base.join("My File (1).png");
        let plain = base.join("plain.txt");
        let folder = base.join("folder");
        std::fs::write(&spaced, "").unwrap();
        std::fs::write(&plain, "").unwrap();
        std::fs::create_dir(&folder).unwrap();
        let raw = spaced.display().to_string();
        assert_eq!(pasted_entries(&raw, base), Some(vec![spaced.clone()]));
        assert_eq!(
            pasted_entries(&format!("'{raw}'"), base),
            Some(vec![spaced.clone()])
        );
        let url = url::Url::from_file_path(&spaced).unwrap();
        assert_eq!(
            pasted_entries(url.as_str(), base),
            Some(vec![spaced.clone()])
        );
        #[cfg(unix)]
        {
            let escaped = raw
                .replace(' ', "\\ ")
                .replace('(', "\\(")
                .replace(')', "\\)");
            assert_eq!(
                pasted_entries(&format!("{escaped} {}", plain.display()), base),
                Some(vec![spaced.clone(), plain.clone()])
            );
        }
        assert_eq!(
            pasted_entries(
                &format!("{}\n{}\n", plain.display(), folder.display()),
                base
            ),
            Some(vec![plain.clone(), folder.clone()])
        );
        assert_eq!(
            pasted_entries(&format!("see {}", plain.display()), base),
            None
        );
        assert_eq!(pasted_paths(&folder.display().to_string(), base), None);
        assert_eq!(pasted_entries("https://example.com/a.png", base), None);
    }

    #[test]
    fn file_links_escape_their_label_and_wrap_awkward_paths() {
        assert_eq!(file_link("a.rs", Path::new("/x/a.rs")), "[a.rs](/x/a.rs)");
        assert_eq!(
            file_link("[x] #2", Path::new("/my dir/(x).rs")),
            "[\\[x\\] #2](</my dir/(x).rs>)"
        );
    }
}
