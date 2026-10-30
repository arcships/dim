use super::network::{Diagnostic, WebError};
use encoding_rs::{Encoding, UTF_8};
use regex::Regex;
use std::sync::OnceLock;
use url::Url;

pub struct Converted {
    pub text: String,
    pub title: Option<String>,
}

pub fn plain_text(html: &str) -> String {
    let without_comments = comment_regex().replace_all(html, "");
    let without_tags = tag_regex().replace_all(&without_comments, "");
    let decoded = html_escape::decode_html_entities(&without_tags);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn redact(text: &str, key: Option<&str>) -> String {
    let Some(key) = key.filter(|k| !k.is_empty()) else {
        return text.to_owned();
    };
    let encoded = encode_uri_component(key);
    text.replace(key, "[redacted]")
        .replace(&encoded, "[redacted]")
}

fn encode_uri_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
        {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub fn convert(
    body: &[u8],
    content_type: &str,
    url: &Url,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Converted, WebError> {
    let (media, header_charset) = parse_mime(content_type).ok_or_else(|| {
        WebError::new(
            "unsupported_content_type",
            "The response has an invalid content type.",
        )
    })?;
    let html = media == "text/html" || media == "application/xhtml+xml";
    if !html
        && !matches!(
            media.as_str(),
            "text/plain"
                | "text/markdown"
                | "text/csv"
                | "application/json"
                | "application/xml"
                | "text/xml"
        )
        && !media.ends_with("+json")
        && !media.ends_with("+xml")
    {
        return Err(WebError::new(
            "unsupported_content_type",
            format!("Unsupported content type: {media}."),
        ));
    }
    let meta_charset = if html {
        let prefix_len = body.len().min(1024);
        let latin1: String = body[..prefix_len].iter().map(|&b| b as char).collect();
        meta_charset_regex()
            .captures(&latin1)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_owned())
    } else {
        None
    };
    let label = header_charset
        .as_deref()
        .or(meta_charset.as_deref())
        .unwrap_or("utf-8");
    let encoding = match Encoding::for_label(label.trim().as_bytes()) {
        Some(enc) => enc,
        None => {
            diagnostics.push(Diagnostic {
                code: "unsupported_charset",
                message: "An unsupported charset was decoded as UTF-8 with replacement.".into(),
            });
            UTF_8
        }
    };
    let (decoded_cow, _, _) = encoding.decode(body);
    let decoded = decoded_cow.into_owned();
    if !html {
        if (media == "application/json" || media.ends_with("+json"))
            && let Some(formatted) = format_json_preserving_numbers(&decoded)
        {
            return Ok(Converted {
                text: formatted,
                title: None,
            });
        }
        return Ok(Converted {
            text: decoded,
            title: None,
        });
    }
    Ok(convert_html(&decoded, url))
}

fn parse_mime(raw: &str) -> Option<(String, Option<String>)> {
    let mut parts = raw.split(';');
    let essence = parts.next()?.trim();
    let (ty, sub) = essence.split_once('/')?;
    let ty = ty.trim();
    let sub = sub.trim();
    if ty.is_empty()
        || sub.is_empty()
        || !is_mime_token(ty)
        || !is_mime_token(sub)
        || sub.contains('/')
    {
        return None;
    }
    let media = format!("{}/{}", ty.to_ascii_lowercase(), sub.to_ascii_lowercase());
    let mut charset = None;
    for param in parts {
        if let Some((k, v)) = param.split_once('=')
            && k.trim().eq_ignore_ascii_case("charset")
        {
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(v);
            if !v.is_empty() {
                charset = Some(v.to_owned());
            }
        }
    }
    Some((media, charset))
}

fn is_mime_token(s: &str) -> bool {
    s.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'|'
                    | b'~'
            )
    })
}

fn format_json_preserving_numbers(input: &str) -> Option<String> {
    if serde_json::from_str::<serde_json::Value>(input).is_err() {
        return None;
    }
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Container {
        Object,
        Array,
    }
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut out = String::with_capacity(len + len / 4);
    let mut stack: Vec<(Container, bool)> = Vec::new();
    let mut after_colon = false;
    let indent = |out: &mut String, depth: usize| {
        for _ in 0..depth {
            out.push_str("  ");
        }
    };
    while i < len {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let prepare_value =
            |out: &mut String, stack: &mut Vec<(Container, bool)>, after_colon: &mut bool| {
                if *after_colon {
                    out.push(' ');
                    *after_colon = false;
                } else if let Some((_, has_items)) = stack.last_mut()
                    && !*has_items
                {
                    *has_items = true;
                    out.push('\n');
                    indent(out, stack.len());
                }
            };
        match b {
            b'{' | b'[' => {
                prepare_value(&mut out, &mut stack, &mut after_colon);
                out.push(b as char);
                stack.push((
                    if b == b'{' {
                        Container::Object
                    } else {
                        Container::Array
                    },
                    false,
                ));
                i += 1;
            }
            b'}' | b']' => {
                let (_, has_items) = stack.pop()?;
                if has_items {
                    out.push('\n');
                    indent(&mut out, stack.len());
                }
                out.push(b as char);
                after_colon = false;
                i += 1;
            }
            b',' => {
                out.push_str(",\n");
                indent(&mut out, stack.len());
                after_colon = false;
                i += 1;
            }
            b':' => {
                out.push(':');
                after_colon = true;
                i += 1;
            }
            b'"' => {
                prepare_value(&mut out, &mut stack, &mut after_colon);
                let start = i;
                i += 1;
                while i < len {
                    if bytes[i] == b'\\' {
                        i += 2;
                    } else if bytes[i] == b'"' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
                let raw_str = &input[start..i];
                if let Ok(parsed) = serde_json::from_str::<String>(raw_str)
                    && let Ok(normalized) = serde_json::to_string(&parsed)
                {
                    out.push_str(&normalized);
                } else {
                    out.push_str(raw_str);
                }
            }
            _ => {
                prepare_value(&mut out, &mut stack, &mut after_colon);
                let start = i;
                while i < len
                    && !bytes[i].is_ascii_whitespace()
                    && !matches!(bytes[i], b',' | b'}' | b']' | b':')
                {
                    i += 1;
                }
                out.push_str(&input[start..i]);
            }
        }
    }
    Some(out)
}

fn convert_html(html: &str, url: &Url) -> Converted {
    let title = extract_tag_inner(html, "title")
        .map(|inner| plain_text(&inner))
        .filter(|t| !t.is_empty());
    let mut base = url.clone();
    if let Some(base_href) = extract_base_href(html)
        && let Ok(target) = base.join(&base_href)
        && matches!(target.scheme(), "http" | "https")
        && target.username().is_empty()
        && target.password().is_none()
    {
        base = target;
    }
    let cleaned = strip_noise_elements(html);
    let root = extract_tag_inner(&cleaned, "main")
        .or_else(|| extract_tag_inner(&cleaned, "article"))
        .or_else(|| extract_tag_inner(&cleaned, "body"))
        .unwrap_or(cleaned);
    let markdown = html_fragment_to_markdown(&root, &base);
    Converted {
        text: markdown,
        title,
    }
}

fn extract_tag_inner(html: &str, tag: &str) -> Option<String> {
    let pattern = format!(r"(?is)<{tag}\b[^>]*>(.*?)</{tag}\s*>");
    let re = Regex::new(&pattern).ok()?;
    re.captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_owned())
}

fn extract_base_href(html: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r#"(?is)<base\b[^>]*\bhref\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>"']+))[^>]*>"#)
            .unwrap()
    });
    let caps = re.captures(html)?;
    let raw = caps
        .get(1)
        .or_else(|| caps.get(2))
        .or_else(|| caps.get(3))?
        .as_str();
    Some(html_escape::decode_html_entities(raw).into_owned())
}

fn strip_noise_elements(html: &str) -> String {
    let mut current = comment_regex().replace_all(html, "").into_owned();
    for tag in [
        "script", "style", "noscript", "template", "svg", "iframe", "nav", "button", "select",
        "textarea",
    ] {
        let pattern = format!(r"(?is)<{tag}\b[^>]*>.*?</{tag}\s*>");
        if let Ok(re) = Regex::new(&pattern) {
            current = re.replace_all(&current, "").into_owned();
        }
    }
    static ROLE_NAV_RE: OnceLock<Regex> = OnceLock::new();
    let role_nav = ROLE_NAV_RE.get_or_init(|| {
        Regex::new(
            r#"(?is)<([a-z0-9]+)\b[^>]*\brole\s*=\s*(?:"navigation"|'navigation'|navigation)\b[^>]*>.*?</[a-z0-9]+\s*>"#,
        )
        .unwrap()
    });
    current = role_nav.replace_all(&current, "").into_owned();
    static VOID_NOISE_RE: OnceLock<Regex> = OnceLock::new();
    let void_noise = VOID_NOISE_RE.get_or_init(|| {
        Regex::new(r"(?is)<(?:input|img)\b[^>]*>(?:</(?:input|img)\s*>)?").unwrap()
    });
    void_noise.replace_all(&current, "").into_owned()
}

fn html_fragment_to_markdown(html: &str, base: &Url) -> String {
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    let bytes = html.as_bytes();
    let len = bytes.len();
    let mut in_pre = false;
    let mut in_code = false;
    let mut link_stack: Vec<(Option<String>, usize)> = Vec::new();
    let mut list_stack: Vec<Option<usize>> = Vec::new();

    while i < len {
        if bytes[i] == b'<'
            && let Some(end_rel) = html[i..].find('>')
        {
            let tag_end = i + end_rel;
            let raw_tag = &html[i + 1..tag_end];
            let trimmed = raw_tag.trim();
            if trimmed.starts_with('!') || trimmed.starts_with('?') {
                i = tag_end + 1;
                continue;
            }
            let is_closing = trimmed.starts_with('/');
            let tag_body = if is_closing {
                trimmed[1..].trim()
            } else {
                trimmed.trim_end_matches('/').trim()
            };
            let tag_name = tag_body
                .split(|c: char| c.is_ascii_whitespace() || c == '/')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !tag_name.is_empty()
                && tag_name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                match (is_closing, tag_name.as_str()) {
                    (false, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") => {
                        ensure_blank_line(&mut out);
                        let level = (tag_name.as_bytes()[1] - b'0') as usize;
                        for _ in 0..level {
                            out.push('#');
                        }
                        out.push(' ');
                    }
                    (true, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") => {
                        ensure_blank_line(&mut out);
                    }
                    (
                        false,
                        "p" | "div" | "section" | "article" | "header" | "footer" | "aside"
                        | "main" | "blockquote",
                    ) => {
                        ensure_blank_line(&mut out);
                        if tag_name == "blockquote" {
                            out.push_str("> ");
                        }
                    }
                    (
                        true,
                        "p" | "div" | "section" | "article" | "header" | "footer" | "aside"
                        | "main" | "blockquote",
                    ) => {
                        ensure_blank_line(&mut out);
                    }
                    (false, "br") => {
                        out.push('\n');
                    }
                    (false, "hr") => {
                        ensure_blank_line(&mut out);
                        out.push_str("---");
                        ensure_blank_line(&mut out);
                    }
                    (false, "pre") => {
                        ensure_blank_line(&mut out);
                        out.push_str("```\n");
                        in_pre = true;
                    }
                    (true, "pre") => {
                        if !out.ends_with('\n') {
                            out.push('\n');
                        }
                        out.push_str("```");
                        in_pre = false;
                        ensure_blank_line(&mut out);
                    }
                    (false, "code") => {
                        if !in_pre {
                            out.push('`');
                            in_code = true;
                        }
                    }
                    (true, "code") => {
                        if !in_pre && in_code {
                            out.push('`');
                            in_code = false;
                        }
                    }
                    (false, "strong" | "b") => {
                        out.push_str("**");
                    }
                    (true, "strong" | "b") => {
                        out.push_str("**");
                    }
                    (false, "em" | "i") => {
                        out.push('*');
                    }
                    (true, "em" | "i") => {
                        out.push('*');
                    }
                    (false, "ul") => {
                        ensure_blank_line(&mut out);
                        list_stack.push(None);
                    }
                    (true, "ul") => {
                        list_stack.pop();
                        ensure_blank_line(&mut out);
                    }
                    (false, "ol") => {
                        ensure_blank_line(&mut out);
                        list_stack.push(Some(1));
                    }
                    (true, "ol") => {
                        list_stack.pop();
                        ensure_blank_line(&mut out);
                    }
                    (false, "li") => {
                        ensure_newline(&mut out);
                        if let Some(Some(counter)) = list_stack.last_mut() {
                            out.push_str(&format!("{counter}. "));
                            *counter += 1;
                        } else {
                            out.push_str("* ");
                        }
                    }
                    (true, "li") => {
                        ensure_newline(&mut out);
                    }
                    (false, "a") => {
                        let resolved = extract_attr(tag_body, "href").and_then(|href| {
                            let target = base.join(&href).ok()?;
                            if matches!(target.scheme(), "http" | "https")
                                && target.username().is_empty()
                                && target.password().is_none()
                            {
                                Some(target.to_string())
                            } else {
                                None
                            }
                        });
                        if resolved.is_some() {
                            out.push('[');
                        }
                        link_stack.push((resolved, out.len()));
                    }
                    (true, "a") => {
                        if let Some((Some(href), _)) = link_stack.pop() {
                            out.push_str(&format!("]({href})"));
                        }
                    }
                    _ => {}
                }
                i = tag_end + 1;
                continue;
            }
        }
        let next_tag = html[i + 1..]
            .find('<')
            .map(|rel| i + 1 + rel)
            .unwrap_or(len);
        let text_slice = &html[i..next_tag];
        let decoded = html_escape::decode_html_entities(text_slice);
        if in_pre {
            out.push_str(&decoded);
        } else {
            append_collapsed_whitespace(&mut out, &decoded);
        }
        i = next_tag;
    }

    normalize_markdown(&out)
}

fn extract_attr(tag_body: &str, attr_name: &str) -> Option<String> {
    let pattern = format!(r#"(?is)\b{attr_name}\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>"']+))"#);
    let re = Regex::new(&pattern).ok()?;
    let caps = re.captures(tag_body)?;
    let val = caps
        .get(1)
        .or_else(|| caps.get(2))
        .or_else(|| caps.get(3))?
        .as_str();
    Some(html_escape::decode_html_entities(val).into_owned())
}

fn ensure_newline(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn ensure_blank_line(out: &mut String) {
    while out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
    if out.is_empty() {
        return;
    }
    if out.ends_with("\n\n") {
        return;
    }
    if out.ends_with('\n') {
        out.push('\n');
    } else {
        out.push_str("\n\n");
    }
}

fn append_collapsed_whitespace(out: &mut String, text: &str) {
    let mut prev_space = out.is_empty() || out.ends_with([' ', '\n']);
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
}

fn normalize_markdown(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let mut blank_run = 0usize;
    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        if line.is_empty() {
            blank_run += 1;
            if blank_run <= 1 && !lines.is_empty() {
                lines.push("");
            }
        } else {
            blank_run = 0;
            lines.push(line);
        }
    }
    while lines.last() == Some(&"") {
        lines.pop();
    }
    lines.join("\n")
}

fn comment_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)<!--.*?-->").unwrap())
}

fn tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)<[^>]*>").unwrap())
}

fn meta_charset_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)<meta\b[^>]*\bcharset\s*=\s*["']?([^\s;"'/>]+)"#).unwrap())
}
