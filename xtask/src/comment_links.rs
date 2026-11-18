//! `check` fails when a comment links a document under `docs/` that is missing,
//! or an anchor that is not a heading of it. A comment link resolves against
//! the source file's own directory, so a file that moves leaves its inbound
//! links broken: rustdoc resolves intra-doc item links only, and the checks
//! under `docs/` do not read Rust sources. Intra-doc item links are not checked
//! here; they are rustdoc's (`rustdoc::broken_intra_doc_links`), which only
//! `cargo doc` runs.
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::{Component, Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const SOURCE_ROOTS: [&str; 3] = ["crates", "xtask", "tests"];
const SKIPPED: [&str; 3] = ["target", "vendor", "node_modules"];

/// Checks every comment link under `root`. Each failure names the file, the
/// line, the link and why it fails.
pub fn check(root: &Path) -> Result<()> {
    let mut errors = Vec::new();
    let mut checked = 0;
    for path in sources(root) {
        let text = fs::read_to_string(&path)?;
        for (line, comment) in comments(&text) {
            let comment = without_code_spans(&comment);
            for (offset, target) in link_targets(&comment) {
                let Some(link) = docs_link(&path, root, target) else {
                    continue;
                };
                checked += 1;
                if let Some(reason) = failure(&link) {
                    errors.push(format!(
                        "{}:{}: `{target}`: {reason}",
                        path.strip_prefix(root).unwrap_or(&path).display(),
                        line + offset
                    ));
                }
            }
        }
    }
    if errors.is_empty() {
        println!("comment links passed ({checked} links into docs)");
        Ok(())
    } else {
        Err(errors.join("\n").into())
    }
}

/// A link in a comment whose path is a document under `docs/`.
struct DocsLink {
    file: PathBuf,
    anchor: Option<String>,
}

fn sources(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root_name in SOURCE_ROOTS {
        let directory = root.join(root_name);
        if directory.is_dir() {
            sources_in(&directory, &mut files);
        }
    }
    files.sort();
    files
}

fn sources_in(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("a source directory reads") {
        let path = entry.expect("a source entry reads").path();
        if SKIPPED.contains(&path_name(&path).as_str()) {
            continue;
        }
        if path.is_dir() {
            sources_in(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn path_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Each comment in `text`, as its first line number and its text with the
/// `//`, `/*` and `*/` delimiters removed. A comment is either a `//` line or a
/// `/* */` block, and a block nests as Rust's does. Neither opens inside a
/// string or a character literal, where the same characters mean something
/// else: a glob such as `"**/*.rs"` is not a comment start.
fn comments(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut rest = text;
    let mut line = 1;
    while let Some(character) = rest.chars().next() {
        let skipped = if let Some(body) = rest.strip_prefix("//") {
            let end = body.find('\n').unwrap_or(body.len());
            found.push((line, body[..end].to_owned()));
            2 + end
        } else if let Some(body) = rest.strip_prefix("/*") {
            let (end, consumed) = block(body);
            found.push((line, body[..end].to_owned()));
            2 + consumed
        } else if let Some(length) = literal(rest) {
            length
        } else {
            character.len_utf8()
        };
        line += rest[..skipped].matches('\n').count();
        rest = &rest[skipped..];
    }
    found
}

/// The body of the block comment whose opening `/*` precedes `body`, as the
/// length of the body before its closing `*/` and the bytes through that
/// closing delimiter. `body` may hold nested comments, which Rust allows and
/// which therefore do not end the outer one early.
fn block(body: &str) -> (usize, usize) {
    let mut depth = 1;
    let mut index = 0;
    while index < body.len() {
        let rest = &body[index..];
        if rest.starts_with("/*") {
            depth += 1;
            index += 2;
        } else if rest.starts_with("*/") {
            depth -= 1;
            if depth == 0 {
                return (index, index + 2);
            }
            index += 2;
        } else {
            index += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    (body.len(), body.len())
}

/// The length of the string or character literal that starts `rest`, `None`
/// when the first character starts neither. The `b` and `b`+`r` prefixes are
/// left to the plain and raw string arms, which then begin at the quote.
fn literal(rest: &str) -> Option<usize> {
    if let Some(body) = rest.strip_prefix('"') {
        return Some(1 + quoted(body, '"'));
    }
    if let Some(body) = rest.strip_prefix('\'') {
        return character_literal(body).map(|length| 1 + length);
    }
    let body = rest.strip_prefix('r')?;
    let hashes = body.len() - body.trim_start_matches('#').len();
    let body = body[hashes..].strip_prefix('"')?;
    let terminator = format!("\"{}", "#".repeat(hashes));
    let end = body.find(&terminator).unwrap_or(body.len());
    Some(
        1 + hashes
            + 1
            + end
            + if end < body.len() {
                terminator.len()
            } else {
                0
            },
    )
}

/// The bytes from the start of `body` through its closing `quote`, escaped
/// characters skipped: the whole of `body` when it never closes.
fn quoted(body: &str, quote: char) -> usize {
    let mut characters = body.char_indices();
    while let Some((offset, character)) = characters.next() {
        if character == '\\' {
            characters.next();
        } else if character == quote {
            return offset + 1;
        }
    }
    body.len()
}

/// The bytes from the start of `body` through the closing quote of the
/// character literal whose opening quote precedes it, or `None` for a lifetime
/// such as `'a`, which has no closing quote of its own.
fn character_literal(body: &str) -> Option<usize> {
    let (_, character) = body.char_indices().next()?;
    if character == '\\' {
        return Some(quoted(body, '\''));
    }
    let end = character.len_utf8();
    body[end..].starts_with('\'').then_some(end + 1)
}

/// `comment` with the text of its code spans overwritten by spaces: Markdown
/// reads a link written inside a code span as syntax being documented, not as
/// a link. Newlines keep their place, so a link after a span reports its own
/// line.
fn without_code_spans(comment: &str) -> String {
    let mut masked = comment.as_bytes().to_vec();
    let mut offset = 0;
    while let Some(found) = comment[offset..].find('`') {
        let open = offset + found;
        let run = comment[open..].chars().take_while(|c| *c == '`').count();
        let Some(close) = closing_span(comment, open + run, run) else {
            offset = open + run;
            continue;
        };
        for byte in &mut masked[open..close + run] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
        offset = close + run;
    }
    String::from_utf8(masked).expect("masking a comment keeps it text")
}

/// Where the run of `length` backticks that closes the code span whose body
/// starts at `body` begins, `None` when the span never closes.
fn closing_span(text: &str, body: usize, length: usize) -> Option<usize> {
    let mut offset = body;
    while let Some(found) = text[offset..].find('`') {
        let found = offset + found;
        let run = text[found..].chars().take_while(|c| *c == '`').count();
        if run == length {
            return Some(found);
        }
        offset = found + run;
    }
    None
}

/// The targets of the Markdown links in `comment`, in source order, each with
/// the number of lines from the start of the comment: inline `[text](target)`
/// links and `[label]: target` reference definitions alike, so a definition
/// that moves with its document is checked like the reference that uses it.
fn link_targets(comment: &str) -> Vec<(usize, &str)> {
    let mut targets = Vec::new();
    let mut rest = comment;
    while let Some(open) = rest.find("](") {
        let at = comment.len() - rest.len() + open;
        rest = &rest[open + 2..];
        let Some(close) = rest.find(')') else { break };
        push_target(
            &rest[..close],
            comment[..at].matches('\n').count(),
            &mut targets,
        );
        rest = &rest[close + 1..];
    }
    for (line, text) in comment.lines().enumerate() {
        let text = text.trim_start_matches([' ', '\t', '*', '/', '!']);
        if let Some((_, target)) = text
            .strip_prefix('[')
            .and_then(|text| text.split_once("]:"))
        {
            push_target(target, line, &mut targets);
        }
    }
    targets
}

/// Pushes the link target in `text`, nothing for none of it: Markdown lets
/// either form wrap the path in angle brackets and follow it with a title.
fn push_target<'a>(text: &'a str, line: usize, targets: &mut Vec<(usize, &'a str)>) {
    let target = text.trim();
    let target = match target.strip_prefix('<') {
        Some(rest) => rest.split_once('>').map_or(rest, |(path, _)| path),
        None => target.split_whitespace().next().unwrap_or_default(),
    };
    if !target.is_empty() {
        targets.push((line, target));
    }
}

/// The link as a document under `docs/`, when it names one. A link to a URL, to
/// a path outside `docs/`, or an intra-doc item link is not checked, and
/// whether it names `docs/` is read from the path as written: a link one level
/// short still names `docs/`, which is the whole reason it is checked.
fn docs_link(source: &Path, root: &Path, target: &str) -> Option<DocsLink> {
    let (path, anchor) = match target.split_once('#') {
        Some((path, anchor)) => (path, Some(anchor.to_owned())),
        None => (target, None),
    };
    if path.is_empty()
        || !path.contains("docs/")
        || path.starts_with('/')
        || path.starts_with("http://")
        || path.starts_with("https://")
        || path.starts_with("mailto:")
        || path.contains("::")
    {
        return None;
    }
    let directory = source.parent().unwrap_or(root);
    Some(DocsLink {
        file: resolve(&directory.join(path)),
        anchor,
    })
}

/// `path` with its `.` and `..` components resolved, without requiring the
/// path to exist.
fn resolve(path: &Path) -> PathBuf {
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            other => parts.push(other.as_os_str()),
        }
    }
    parts.iter().collect()
}

fn failure(link: &DocsLink) -> Option<String> {
    let text = match fs::read_to_string(&link.file) {
        Ok(text) => text,
        Err(error) => return Some(read_failure(&error)),
    };
    let anchor = link.anchor.as_deref()?;
    if headings(&text).contains(anchor) {
        return None;
    }
    Some(format!("no heading `#{anchor}` in the document"))
}

/// Why the document a link names cannot be read: a directory, and a file the
/// filesystem refuses, are not missing, and the message says which happened.
fn read_failure(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "no such file".to_owned(),
        std::io::ErrorKind::IsADirectory => "is a directory, not a document".to_owned(),
        _ => format!("cannot be read: {error}"),
    }
}

/// The GitHub heading slugs of a Markdown document: an ATX heading (`## Title`)
/// and a Setext heading (a paragraph line underlined by `===` or `---`) alike,
/// since a link may name either. A fence hides the lines it holds, indented by
/// up to three spaces as GitHub allows. A repeated heading also gets `-1`, `-2`
/// variants, which is what a link to a duplicate must name.
fn headings(text: &str) -> BTreeSet<String> {
    let mut slugs = BTreeSet::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut fence: Option<char> = None;
    let mut paragraph: Option<&str> = None;
    for line in text.lines() {
        let body = line.trim_start_matches(' ');
        let indented = line.len() - body.len() <= 3;
        if let Some(marker) = fence {
            if indented && fence_marker(body).is_some() && body.trim_matches(marker).is_empty() {
                fence = None;
            }
            continue;
        }
        if indented && let Some(marker) = fence_marker(body) {
            fence = Some(marker);
            paragraph = None;
            continue;
        }
        if body.trim().is_empty() {
            paragraph = None;
            continue;
        }
        if let Some(heading) = atx(body) {
            push_heading(&mut slugs, &mut seen, heading);
            paragraph = None;
            continue;
        }
        if let Some(title) = paragraph.take().filter(|_| setext(body)) {
            push_heading(&mut slugs, &mut seen, title);
            continue;
        }
        paragraph = Some(body.trim());
    }
    slugs
}

/// The marker of the fence line `body` opens: a run of at least three backticks
/// or tildes, which GitHub also allows indented by up to three spaces.
fn fence_marker(body: &str) -> Option<char> {
    let marker = body.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    (body.chars().take_while(|c| *c == marker).count() >= 3).then_some(marker)
}

/// The text of an ATX heading line, `None` for a line of prose: GitHub takes
/// at most six `#`s and needs whitespace after them, so `#128 is not a
/// heading` is prose rather than an anchor.
fn atx(line: &str) -> Option<&str> {
    let hashes = line
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let heading = line[hashes..].strip_prefix([' ', '\t'])?.trim();
    (!heading.is_empty()).then_some(heading)
}

/// Whether `line` underlines a Setext heading: a run of `=` or of `-` alone on
/// its line, which titles the paragraph line above it.
fn setext(line: &str) -> bool {
    let underline = line.trim();
    !underline.is_empty()
        && (underline.chars().all(|character| character == '=')
            || underline.chars().all(|character| character == '-'))
}

fn push_heading(slugs: &mut BTreeSet<String>, seen: &mut BTreeMap<String, usize>, heading: &str) {
    let slug = slug(heading);
    let count = seen.entry(slug.clone()).or_insert(0);
    slugs.insert(if *count == 0 {
        slug
    } else {
        format!("{slug}-{count}")
    });
    *count += 1;
}

/// GitHub's heading slug: lowercase, code spans, emphasis and link markup
/// dropped, non-ASCII letters kept, spaces to `-`.
fn slug(heading: &str) -> String {
    let mut text = heading.to_lowercase();
    while let Some(open) = text.find("](") {
        match text[open..].find(')') {
            Some(close) => {
                text.replace_range(open..open + close + 1, "");
            }
            None => break,
        }
    }
    text.retain(|character| !matches!(character, '`' | '[' | ']' | '<' | '>' | '*' | '!'));
    text.chars()
        .filter(|character| character.is_alphanumeric() || matches!(character, '-' | ' ' | '_'))
        .collect::<String>()
        .trim()
        .replace(' ', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_comment_and_a_line_comment_are_both_read() {
        let text = "let x = 1;\n/*one\ntwo*/ let y = 2;\n// three\n";
        assert_eq!(
            comments(text),
            vec![(2, "one\ntwo".to_owned()), (4, " three".to_owned())]
        );
    }

    #[test]
    fn a_block_comment_ends_after_its_nested_comments() {
        let text = "/* outer /* inner */ [x](../../docs/a.md) */";
        assert_eq!(
            comments(text),
            vec![(1, " outer /* inner */ [x](../../docs/a.md) ".to_owned())]
        );
    }

    #[test]
    fn a_link_in_a_block_comment_reports_its_own_line() {
        let text = "/*\n * [a](../../docs/a.md)\n * [b](../../docs/b.md)\n */";
        let (line, comment) = comments(text).remove(0);
        let targets = link_targets(&comment);
        assert_eq!(
            targets,
            vec![(1, "../../docs/a.md"), (2, "../../docs/b.md")]
        );
        assert_eq!(line + targets[1].0, 3);
    }

    #[test]
    fn a_link_inside_a_code_span_is_not_a_link() {
        let comment =
            "Write `[a](../../docs/a.md)` or `[b](../../docs/b.md)`, not [c](../../docs/c.md)";
        assert_eq!(
            link_targets(&without_code_spans(comment)),
            vec![(0, "../../docs/c.md")]
        );
    }

    #[test]
    fn a_link_after_a_glob_string_literal_keeps_its_own_line() {
        let text = concat!(
            "let pattern = \"**/*.rs\";\n",
            "let quote = '\"';\n",
            "let lifetime: &'a str = r#\"**/*.ts\"#;\n",
            "/// [Prompt §6.1]: ../../../docs/design/plugins/Prompt.md#61-the-plugin-bridge\n",
        );
        let link = comments(text)
            .into_iter()
            .filter_map(|(line, comment)| {
                link_targets(&comment)
                    .into_iter()
                    .next()
                    .map(|(offset, target)| (line + offset, target.to_owned()))
            })
            .next()
            .expect("the doc line is a comment");
        assert_eq!(
            link,
            (
                4,
                "../../../docs/design/plugins/Prompt.md#61-the-plugin-bridge".to_owned()
            )
        );
    }

    #[test]
    fn a_reference_definition_is_read_like_a_link() {
        let text = "/// [PromptSources §3]: ../../../docs/design/plugins/PromptSources.md\n"
            .to_owned()
            + "// [Quoted]: <../../../docs/design/plugins/Prompt.md> \"title\"\n"
            + "// see [Prompt §6.1]: ../../../docs/design/plugins/Prompt.md\n";
        let mut targets = Vec::new();
        for (_, comment) in comments(&text) {
            targets.extend(
                link_targets(&comment)
                    .into_iter()
                    .map(|(_, target)| target.to_owned()),
            );
        }
        assert_eq!(
            targets,
            vec![
                "../../../docs/design/plugins/PromptSources.md",
                "../../../docs/design/plugins/Prompt.md",
            ]
        );
    }

    #[test]
    fn slugs_follow_github() {
        assert_eq!(
            slug("7.1.1 The parent block, `branch:parent`"),
            "711-the-parent-block-branchparent"
        );
        assert_eq!(
            slug("4.10 宿主端口：`ExecutionSettingsHost` 等五个"),
            "410-宿主端口executionsettingshost-等五个"
        );
        assert_eq!(slug("1. 规则"), "1-规则");
    }

    #[test]
    fn a_repeated_heading_is_suffixed_and_a_fenced_line_is_not_a_heading() {
        let text = "# One\n## One\n```\n# not a heading\n```\n";
        assert_eq!(
            headings(text),
            BTreeSet::from(["one".to_owned(), "one-1".to_owned()])
        );
    }

    #[test]
    fn a_heading_needs_whitespace_after_its_hashes() {
        let text = "#128 was fixed\n#[derive(Clone)]\n# Title\n";
        assert_eq!(headings(text), BTreeSet::from(["title".to_owned()]));
    }

    #[test]
    fn a_fence_indented_by_up_to_three_spaces_hides_its_lines() {
        let text = "# One\n   ```text\n# hidden\n   ```\n# Two\n    ```\n# visible\n";
        assert_eq!(
            headings(text),
            BTreeSet::from(["one".to_owned(), "two".to_owned(), "visible".to_owned()])
        );
    }

    #[test]
    fn a_setext_heading_titles_the_line_above_its_underline() {
        let text = "Setext one\n==========\n\nSetext two\n----------\n# Atx\n";
        assert_eq!(
            headings(text),
            BTreeSet::from([
                "setext-one".to_owned(),
                "setext-two".to_owned(),
                "atx".to_owned()
            ])
        );
    }

    /// A tree whose one source file is `source` and whose one document is
    /// `document` at `docs/design/example.md`, under a fresh directory.
    fn tree(name: &str, source: &str, document: Option<&[u8]>) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "dim-xtask-comment-links-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("crates/example/src")).unwrap();
        std::fs::write(root.join("crates/example/src/lib.rs"), source).unwrap();
        if let Some(document) = document {
            std::fs::create_dir_all(root.join("docs/design")).unwrap();
            std::fs::write(root.join("docs/design/example.md"), document).unwrap();
        }
        root
    }

    fn refusal(root: &Path) -> String {
        let error = check(root).unwrap_err().to_string();
        std::fs::remove_dir_all(root).unwrap();
        error
    }

    #[test]
    fn a_link_one_level_short_is_refused_with_its_line() {
        let root = tree(
            "short",
            "// a line of code\n/// [example](../../docs/design/example.md)\n",
            Some(b"# Present\n"),
        );
        let error = refusal(&root);
        assert!(
            error.contains(
                "crates/example/src/lib.rs:2: `../../docs/design/example.md`: no such file"
            ),
            "{error}"
        );
    }

    #[test]
    fn a_missing_anchor_is_refused_by_name() {
        let root = tree(
            "anchor",
            "/// [example](../../../docs/design/example.md#absent-heading)\n",
            Some(b"# Present\n"),
        );
        let error = refusal(&root);
        assert!(
            error.contains("no heading `#absent-heading` in the document"),
            "{error}"
        );
    }

    #[test]
    fn a_link_that_resolves_passes() {
        let root = tree(
            "clean",
            "/// [example](../../../docs/design/example.md#present)\n",
            Some(b"# Present\n"),
        );
        check(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_link_to_a_directory_is_not_a_missing_document() {
        let root = tree(
            "directory",
            "/// [example](../../../docs/design)\n",
            Some(b"# Present\n"),
        );
        let error = refusal(&root);
        assert!(error.contains("is a directory, not a document"), "{error}");
    }

    #[test]
    fn a_document_that_cannot_be_read_says_why() {
        let root = tree(
            "unreadable",
            "/// [example](../../../docs/design/example.md)\n",
            Some(b"\xff\xfe"),
        );
        let error = refusal(&root);
        assert!(error.contains("cannot be read:"), "{error}");
        assert!(!error.contains("no such file"), "{error}");
    }

    #[test]
    fn a_link_inside_a_code_span_does_not_fail_the_check() {
        let root = tree(
            "code-span",
            "// Write `[example](../../../docs/design/absent.md)` as a link.\n",
            None,
        );
        check(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
