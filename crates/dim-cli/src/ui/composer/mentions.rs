//! `@` file mentions: the trigger before the cursor, the file index it
//! searches, the ranking and the picker panel (ports of
//! `detectMentionTrigger`, `path-data.ts` and the inline path panel).
//!
//! The index never blocks the interface: a project directory is walked on
//! a thread of its own, within a depth, entry and time budget, and the
//! picker ranks whatever has been found so far. The home directory and the
//! filesystem root are never walked; there, as for a typed `~/`, `/` or
//! `../` prefix, the picker lists only the directory being typed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::ui::i18n::t;
use crate::ui::render::panel::bordered;
use crate::ui::rows::{Columns, Kind, Row, footer, panel_row, title, title_room};
use crate::ui::text::{display_width, truncate_cells, truncate_cells_start};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::fg;

/// Rows the picker shows at most.
pub const PAGE_SIZE: usize = 8;
const RESULTS: usize = 20;
const MAX_DEPTH: usize = 8;
const MAX_ENTRIES: usize = 50_000;
const WALK_BUDGET: Duration = Duration::from_secs(2);
/// A finished index is reused this long before it is walked again.
const INDEX_TTL: Duration = Duration::from_secs(10);
/// Entries one listed directory contributes at most.
const MAX_LISTED: usize = 2_000;
const LISTING_TTL: Duration = Duration::from_secs(2);
const HINT: &str = "↑↓ select · enter/tab insert · esc close";

/// Directories never walked, wherever they are.
const SKIPPED: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "dist",
    "build",
    "out",
    "target",
    "vendor",
    "venv",
    ".venv",
    "__pycache__",
    "coverage",
    ".next",
    ".turbo",
    ".cache",
    "DerivedData",
    "Pods",
    "Library",
    ".Trash",
];

/// An `@` being typed: the byte where the `@` is and the query after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub start: usize,
    pub query: String,
}

fn is_query_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '.' | '/' | '~' | '\\' | ':' | '-')
}

fn is_boundary(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            ',' | ';'
                | ':'
                | '!'
                | '?'
                | '，'
                | '。'
                | '！'
                | '？'
                | '、'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '<'
                | '>'
        )
}

/// The `@` mention the cursor is in: an `@` at the start of the draft or
/// after a space or punctuation, then path characters up to the cursor
/// (`a@b` is not one).
pub fn trigger(text: &str, cursor: usize) -> Option<Trigger> {
    let before = text.get(..cursor)?;
    for (at, ch) in before.char_indices().rev() {
        if ch == '@' {
            let lead = before[..at].chars().next_back();
            if lead.is_some_and(|ch| !is_boundary(ch)) {
                return None;
            }
            return Some(Trigger {
                start: at,
                query: before[at + 1..].to_owned(),
            });
        }
        if !is_query_char(ch) {
            return None;
        }
    }
    None
}

/// One file or directory the picker offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// What the mention reads after its `@`, as the query would type it
    /// (`src/main.rs`, `~/notes.txt`); a directory ends in `/`.
    pub text: String,
    pub path: PathBuf,
    pub dir: bool,
}

#[derive(Debug, Clone)]
struct Entry {
    /// Relative to the root, `/`-separated.
    rel: String,
    /// `rel` lowercased once, as every query compares it.
    lower: String,
    dir: bool,
    depth: usize,
}

#[derive(Default)]
struct Found {
    entries: Vec<Entry>,
    done: bool,
}

/// A walk's generation, entry count and whether it finished: what a frame
/// ranked from it depends on.
type IndexState = (u64, usize, bool);

/// The walk of one project directory, filled from a thread of its own.
struct Walk {
    /// Which walk this is: a refresh can find exactly what the last one
    /// did, and must still count as new.
    generation: u64,
    started: Instant,
    found: Arc<Mutex<Found>>,
    stop: Arc<AtomicBool>,
}

impl Walk {
    fn start(root: PathBuf, generation: u64) -> Self {
        let found = Arc::new(Mutex::new(Found::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let walk = Self {
            generation,
            started: Instant::now(),
            found: found.clone(),
            stop: stop.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name("dim-mentions".into())
            .spawn(move || index(&root, &found, &stop));
        if spawned.is_err() {
            walk.found.lock().map(|mut f| f.done = true).ok();
        }
        walk
    }

    fn done(&self) -> bool {
        self.found.lock().map(|f| f.done).unwrap_or(true)
    }

    fn state(&self) -> IndexState {
        self.found
            .lock()
            .map(|f| (self.generation, f.entries.len(), f.done))
            .unwrap_or((self.generation, 0, true))
    }
}

impl Drop for Walk {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Walks `root` breadth first, honouring `.gitignore` files, within the
/// depth, entry and time budget; batches reach `found` as they are read.
fn index(root: &Path, found: &Mutex<Found>, stop: &AtomicBool) {
    let started = Instant::now();
    let walker = ignore::WalkBuilder::new(root)
        .max_depth(Some(MAX_DEPTH))
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .parents(true)
        .follow_links(false)
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| SKIPPED.contains(&name))
        })
        .build();
    let mut batch = Vec::new();
    let mut total = 0;
    let flush = |batch: &mut Vec<Entry>| {
        if let Ok(mut found) = found.lock() {
            found.entries.append(batch);
        }
    };
    for entry in walker {
        if stop.load(Ordering::Relaxed) || started.elapsed() > WALK_BUDGET || total >= MAX_ENTRIES {
            break;
        }
        let Ok(entry) = entry else { continue };
        if entry.depth() == 0 {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let Some(rel) = rel.to_str() else { continue };
        let rel = rel.replace('\\', "/");
        batch.push(Entry {
            lower: rel.to_lowercase(),
            rel,
            dir: entry.file_type().is_some_and(|kind| kind.is_dir()),
            depth: entry.depth(),
        });
        total += 1;
        if batch.len() >= 512 {
            flush(&mut batch);
        }
    }
    flush(&mut batch);
    if let Ok(mut found) = found.lock() {
        found.done = true;
    }
}

/// One directory read for a listing.
struct Listing {
    dir: PathBuf,
    read: Instant,
    entries: Vec<(String, bool)>,
}

fn list_dir(dir: &Path) -> Vec<(String, bool)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.take(MAX_LISTED)
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            let dir = entry.file_type().ok()?.is_dir()
                || (entry.file_type().ok()?.is_symlink() && entry.path().is_dir());
            Some((name, dir))
        })
        .collect()
}

/// The files `@` mentions are found among, for one working directory.
pub struct Mentions {
    root: PathBuf,
    /// The home directory and `/` are only listed, never walked.
    shallow: bool,
    /// The index the picker ranks.
    walk: Option<Walk>,
    /// A refresh of a stale index; it replaces `walk` once finished, so
    /// the picker keeps its results meanwhile.
    refresh: Option<Walk>,
    walks: u64,
    /// How long a finished index is reused.
    ttl: Duration,
    listing: Option<Listing>,
    /// The index state the last frame ranked, to redraw as it changes.
    seen: Option<IndexState>,
    /// The last index ranking: its query, the index state it saw, and the
    /// result, so a frame and a key with one query rank once.
    ranked: Option<(String, IndexState, Vec<Candidate>)>,
}

impl Mentions {
    pub fn new(root: PathBuf) -> Self {
        Self::new_with_home(root, home())
    }

    fn new_with_home(root: PathBuf, home: Option<PathBuf>) -> Self {
        let shallow = root.parent().is_none() || home.is_some_and(|home| same_dir(&home, &root));
        Self {
            root,
            shallow,
            walk: None,
            refresh: None,
            walks: 0,
            ttl: INDEX_TTL,
            listing: None,
            seen: None,
            ranked: None,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn start_walk(&mut self) -> Walk {
        self.walks += 1;
        Walk::start(self.root.clone(), self.walks)
    }

    /// A finished refresh takes the index's place.
    fn adopt_refresh(&mut self) {
        if self.refresh.as_ref().is_some_and(Walk::done) {
            self.walk = self.refresh.take();
        }
    }

    /// Starts the walk when a project query first needs it, and a refresh
    /// once the finished index is stale.
    fn walk(&mut self) {
        self.adopt_refresh();
        let stale = self
            .walk
            .as_ref()
            .is_some_and(|walk| walk.done() && walk.started.elapsed() > self.ttl);
        if self.walk.is_none() {
            self.walk = Some(self.start_walk());
        } else if stale && self.refresh.is_none() {
            self.refresh = Some(self.start_walk());
        }
    }

    fn listing(&mut self, dir: &Path) -> &[(String, bool)] {
        let fresh = self
            .listing
            .as_ref()
            .is_some_and(|l| l.dir == dir && l.read.elapsed() < LISTING_TTL);
        if !fresh {
            self.listing = Some(Listing {
                dir: dir.to_owned(),
                read: Instant::now(),
                entries: list_dir(dir),
            });
        }
        &self.listing.as_ref().expect("listing").entries
    }

    /// True once the index changed since the last time this was asked:
    /// more entries arrived, the walk finished, or a refresh replaced it.
    pub fn changed(&mut self) -> bool {
        self.adopt_refresh();
        let Some(walk) = &self.walk else { return false };
        let now = Some(walk.state());
        if now != self.seen {
            self.seen = now;
            return true;
        }
        false
    }

    /// Whether the walk is still filling the index.
    pub fn searching(&self) -> bool {
        self.walk.as_ref().is_some_and(|walk| !walk.done())
    }

    /// The best candidates for a query, best first.
    pub fn candidates(&mut self, query: &str) -> Vec<Candidate> {
        let query = if query == "~" { "~/" } else { query };
        let (prefix, name) = match query.rfind('/') {
            Some(at) => query.split_at(at + 1),
            None => ("", query),
        };
        let outside = prefix.starts_with('~')
            || prefix.starts_with('/')
            || prefix.split('/').any(|part| part == "..")
            || Path::new(prefix).is_absolute();
        if outside || self.shallow {
            let Some(dir) = self.resolve(prefix) else {
                return Vec::new();
            };
            let listed: Vec<(String, bool)> = self.listing(&dir).to_vec();
            let mut ranked: Vec<(i64, Candidate)> = listed
                .into_iter()
                .filter(|(entry, _)| name.starts_with('.') || !entry.starts_with('.'))
                .filter_map(|(entry, dir_kind)| {
                    let score = fuzzy(name, &entry)?;
                    Some((
                        score,
                        Candidate {
                            text: format!("{prefix}{entry}{}", if dir_kind { "/" } else { "" }),
                            path: dir.join(&entry),
                            dir: dir_kind,
                        },
                    ))
                })
                .collect();
            ranked.sort_by(|(a, x), (b, y)| {
                b.cmp(a)
                    .then_with(|| y.dir.cmp(&x.dir))
                    .then_with(|| x.text.cmp(&y.text))
            });
            return ranked.into_iter().take(RESULTS).map(|(_, c)| c).collect();
        }
        let root = self.root.clone();
        self.walk();
        let Some(walk) = &self.walk else {
            return Vec::new();
        };
        let Ok(found) = walk.found.lock() else {
            return Vec::new();
        };
        let size = (walk.generation, found.entries.len(), found.done);
        if let Some((last, seen, ranked)) = &self.ranked
            && last == query
            && *seen == size
        {
            return ranked.clone();
        }
        let prefix = prefix.to_lowercase();
        let name = name.to_lowercase();
        let hidden = name.starts_with('.');
        let mut ranked: Vec<(i64, &Entry)> = Vec::new();
        for entry in &found.entries {
            let Some(rest) = entry.lower.strip_prefix(prefix.as_str()) else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            if !hidden && (rest.starts_with('.') || rest.contains("/.")) {
                continue;
            }
            let base = rest.rsplit('/').next().unwrap_or(rest);
            let score = match (score(&name, base), score(&name, rest)) {
                (Some(base), Some(path)) => (base + 50).max(path),
                (Some(base), None) => base + 50,
                (None, Some(path)) => path,
                (None, None) => continue,
            };
            ranked.push((score, entry));
        }
        let order = |(a, x): &(i64, &Entry), (b, y): &(i64, &Entry)| {
            b.cmp(a)
                .then_with(|| x.depth.cmp(&y.depth))
                .then_with(|| y.dir.cmp(&x.dir))
                .then_with(|| x.rel.cmp(&y.rel))
        };
        if ranked.len() > RESULTS {
            ranked.select_nth_unstable_by(RESULTS, order);
            ranked.truncate(RESULTS);
        }
        ranked.sort_by(order);
        let result: Vec<Candidate> = ranked
            .into_iter()
            .map(|(_, entry)| Candidate {
                text: format!("{}{}", entry.rel, if entry.dir { "/" } else { "" }),
                path: root.join(&entry.rel),
                dir: entry.dir,
            })
            .collect();
        drop(found);
        self.ranked = Some((query.to_owned(), size, result.clone()));
        result
    }

    /// The directory a typed prefix names: `~` is the home directory, an
    /// absolute prefix itself, anything else under the root.
    fn resolve(&self, prefix: &str) -> Option<PathBuf> {
        let dir = if let Some(rest) = prefix.strip_prefix("~/") {
            home()?.join(rest)
        } else if prefix.starts_with('~') {
            home()?
        } else if Path::new(prefix).is_absolute() {
            PathBuf::from(prefix)
        } else {
            self.root.join(prefix)
        };
        dir.is_dir().then_some(dir)
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// How well `query` matches `candidate`, case-insensitively: an exact
/// match, a prefix, a substring, then the letters in order with few gaps;
/// `None` for no match. An empty query matches everything equally.
fn fuzzy(query: &str, candidate: &str) -> Option<i64> {
    score(&query.to_lowercase(), &candidate.to_lowercase())
}

/// [`fuzzy`] over already lowercased text, without allocating: it runs
/// for every indexed entry on every keystroke.
fn score(query: &str, candidate: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    if candidate == query {
        return Some(2000);
    }
    if candidate.len() < query.len() {
        return None;
    }
    if let Some(rest) = candidate.strip_prefix(query) {
        return Some(1500 - rest.chars().count() as i64);
    }
    if let Some(at) = candidate.find(query) {
        return Some(1200 - candidate[..at].chars().count() as i64);
    }
    let mut wanted = query.chars();
    let mut next = wanted.next();
    let mut count = 0i64;
    let mut gaps = 0i64;
    let mut last: Option<i64> = None;
    for (at, ch) in candidate.chars().enumerate() {
        let Some(want) = next else { break };
        if ch == want {
            if let Some(last) = last {
                gaps += at as i64 - last - 1;
            }
            last = Some(at as i64);
            count += 1;
            next = wanted.next();
        }
    }
    if next.is_some() || gaps > (count * 2).max(4) {
        return None;
    }
    Some(800 - gaps)
}

/// The selection over the candidates of one trigger.
#[derive(Debug, Default)]
pub struct Picker {
    pub selected: usize,
    /// The trigger Esc closed; it stays closed until it changes.
    pub dismissed: Option<Trigger>,
    query: String,
}

impl Picker {
    pub fn follow(&mut self, query: &str) {
        if self.query != query {
            self.query = query.to_owned();
            self.selected = 0;
        }
    }

    /// Moves the selection, wrapping at either end.
    pub fn step(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(len as isize) as usize;
    }
}

/// The picker's title row: `Files`, or `Files · <dir>` when the screen
/// shows no other directory the candidates come from. A path too long for
/// the row keeps its tail behind a leading ellipsis, as the border's does;
/// too narrow even for a tail, the title stays plain rather than show an
/// ellipsis that names nothing.
fn picker_title(root: Option<&str>, inner: usize) -> String {
    /// Fewest path cells worth reading; the border uses the same idea for
    /// the tail it keeps.
    const TAIL: usize = 4;
    let files = t("Files");
    let Some(root) = root else {
        return files.to_owned();
    };
    let separator = " · ";
    let Some(path) = title_room(inner).checked_sub(display_width(files) + display_width(separator))
    else {
        return files.to_owned();
    };
    if path < TAIL {
        return files.to_owned();
    }
    format!("{files}{separator}{}", truncate_cells_start(root, path))
}

/// The bordered picker panel, `width` columns wide including borders.
/// `root` names the directory the candidates come from, for a screen whose
/// border names another workspace; `None` titles the panel `Files`.
pub fn render(
    picker: &Picker,
    candidates: &[Candidate],
    searching: bool,
    root: Option<&str>,
    width: usize,
    p: &Palette,
) -> Vec<String> {
    let inner = width.saturating_sub(4).max(1);
    let rows: Vec<Row> = candidates
        .iter()
        .map(|candidate| Row {
            kind: Kind::Item,
            label: format!("@{}", candidate.text),
            detail: String::new(),
            key: None,
            unavailable: false,
        })
        .collect();
    let mut content = vec![title(&picker_title(root, inner), inner, p)];
    if rows.is_empty() {
        let empty = if searching {
            t("Searching…")
        } else {
            t("No matches")
        };
        content.push(fg(&truncate_cells(empty, inner), &p.muted));
    } else {
        let columns = Columns::of(&rows, inner);
        let selected = picker.selected.min(rows.len() - 1);
        let start = (selected + 1).saturating_sub(PAGE_SIZE);
        content.extend(
            rows.iter()
                .enumerate()
                .skip(start)
                .take(PAGE_SIZE)
                .map(|(at, row)| panel_row(row, at == selected, inner, columns, p)),
        );
    }
    let position = if rows.is_empty() {
        "0 / 0".to_owned()
    } else {
        format!(
            "{} / {}",
            picker.selected.min(rows.len() - 1) + 1,
            rows.len()
        )
    };
    content.push(footer(&position, t(HINT), inner, p));
    bordered(&content, width, None, &p.border, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_end(text: &str) -> Option<Trigger> {
        trigger(text, text.len())
    }

    #[test]
    fn an_at_sign_after_a_boundary_starts_a_mention() {
        assert_eq!(
            at_end("open @src/ma"),
            Some(Trigger {
                start: 5,
                query: "src/ma".into()
            })
        );
        assert_eq!(at_end("@").map(|t| t.query), Some(String::new()));
        assert_eq!(at_end("看看（@文件").map(|t| t.query), None);
        assert_eq!(at_end("看看 @文件").map(|t| t.query), Some("文件".into()));
        assert_eq!(at_end("(@a").map(|t| t.start), Some(1));
        assert_eq!(at_end("mail a@b"), None);
        assert_eq!(at_end("@src done"), None);
        assert_eq!(at_end("plain"), None);
    }

    #[test]
    fn ranking_prefers_exact_then_prefix_then_substring_then_letters_in_order() {
        assert_eq!(fuzzy("main.rs", "main.rs"), Some(2000));
        assert!(fuzzy("ma", "main.rs") > fuzzy("ai", "main.rs"));
        assert!(fuzzy("ai", "main.rs") > fuzzy("mrs", "main.rs"));
        assert_eq!(fuzzy("zz", "main.rs"), None);
        assert_eq!(fuzzy("mx", "m-------------x"), None);
        assert_eq!(fuzzy("", "anything"), Some(0));
    }

    fn wait(mentions: &mut Mentions) {
        let started = Instant::now();
        while (mentions.searching() || mentions.refresh.is_some())
            && started.elapsed() < Duration::from_secs(10)
        {
            mentions.adopt_refresh();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_refresh_keeps_the_results_and_redraws_when_it_lands() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("first.txt"), "").unwrap();
        let mut mentions = Mentions::new(dir.path().to_owned());
        mentions.ttl = Duration::MAX;
        mentions.candidates("");
        wait(&mut mentions);
        assert!(mentions.changed());
        assert!(!mentions.changed());
        let texts = |m: &mut Mentions| -> Vec<String> {
            m.candidates("").into_iter().map(|c| c.text).collect()
        };
        assert_eq!(texts(&mut mentions), ["first.txt"]);
        std::fs::write(dir.path().join("second.txt"), "").unwrap();
        mentions.ttl = Duration::ZERO;
        assert_eq!(texts(&mut mentions), ["first.txt"]);
        mentions.ttl = Duration::MAX;
        assert!(!mentions.searching());
        wait(&mut mentions);
        assert!(mentions.changed());
        assert_eq!(texts(&mut mentions), ["first.txt", "second.txt"]);
        std::fs::remove_file(dir.path().join("second.txt")).unwrap();
        std::fs::write(dir.path().join("third.txt"), "").unwrap();
        mentions.changed();
        mentions.ttl = Duration::ZERO;
        texts(&mut mentions);
        mentions.ttl = Duration::MAX;
        wait(&mut mentions);
        assert!(mentions.changed(), "a same-size refresh still redraws");
        assert_eq!(texts(&mut mentions), ["first.txt", "third.txt"]);
    }

    #[test]
    fn the_index_honours_gitignore_and_skipped_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/deep/er")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(root.join("ignored")).unwrap();
        std::fs::write(root.join(".gitignore"), "ignored/\n*.log\n").unwrap();
        std::fs::write(root.join("src/deep/er/main.rs"), "").unwrap();
        std::fs::write(root.join("node_modules/pkg/main.rs"), "").unwrap();
        std::fs::write(root.join("ignored/main.rs"), "").unwrap();
        std::fs::write(root.join("run.log"), "").unwrap();
        std::fs::write(root.join(".env"), "").unwrap();
        let mut mentions = Mentions::new(root.to_owned());
        mentions.candidates("");
        wait(&mut mentions);
        let found: Vec<String> = mentions
            .candidates("main")
            .into_iter()
            .map(|c| c.text)
            .collect();
        assert_eq!(found, ["src/deep/er/main.rs"]);
        assert!(mentions.candidates("run").is_empty());
        assert!(!mentions.candidates("").iter().any(|c| c.text == ".env"));
        assert_eq!(mentions.candidates(".en")[0].text, ".env");
        let under: Vec<String> = mentions
            .candidates("src/")
            .into_iter()
            .map(|c| c.text)
            .collect();
        assert_eq!(under[0], "src/deep/");
        assert!(under.contains(&"src/deep/er/main.rs".to_owned()));
    }

    #[test]
    fn the_home_directory_is_listed_one_level_and_never_walked() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("notes.txt"), "").unwrap();
        std::fs::create_dir(home.path().join("projects")).unwrap();
        std::fs::write(home.path().join("projects/hidden.txt"), "").unwrap();
        let home = home.path().to_owned();
        let mut mentions = Mentions::new_with_home(home.clone(), Some(home));
        let found: Vec<String> = mentions
            .candidates("")
            .into_iter()
            .map(|candidate| candidate.text)
            .collect();
        assert!(mentions.walk.is_none(), "home must not be walked");
        assert_eq!(
            found,
            ["projects/", "notes.txt"],
            "only the home's own entries are listed"
        );
    }
}
