//! The files an `@` mention offers in a Session's workspace, for the
//! `session.files.search` method: the index and ranking of the TUI's mention
//! picker (`crates/dim-cli/src/ui/composer/mentions.rs`), so both clients offer
//! the same files in the same order. Only paths under the workspace root are
//! offered; a query that names a path outside it finds nothing.

use super::FileStorageError;
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 50;
const MAX_DEPTH: usize = 8;
const MAX_ENTRIES: usize = 50_000;
const WALK_BUDGET: Duration = Duration::from_secs(2);
/// A walked index is reused this long before the workspace is walked again.
const INDEX_TTL: Duration = Duration::from_secs(10);

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

#[derive(Debug)]
struct Entry {
    /// Relative to the root, `/`-separated.
    rel: String,
    /// `rel` lowercased once, as every query compares it.
    lower: String,
    dir: bool,
    depth: usize,
}

/// One file or directory the query finds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// Relative to the workspace root, `/`-separated, with no trailing `/` on a directory.
    pub relative_path: String,
    pub absolute_path: PathBuf,
    pub dir: bool,
}

type Walked = HashMap<PathBuf, (Instant, Arc<Vec<Entry>>)>;

/// The walked indexes of the workspace roots, each reused for [`INDEX_TTL`].
/// Both WorkspaceTools search entry points use the file owner’s index.
pub struct MentionIndex {
    walked: Mutex<Walked>,
    ttl: Duration,
}

impl Default for MentionIndex {
    fn default() -> Self {
        Self {
            walked: Mutex::default(),
            ttl: INDEX_TTL,
        }
    }
}

impl MentionIndex {
    /// The index with `ttl` in place of the product's ten seconds, for tests
    /// that must observe a rewalk without waiting that long. No product path
    /// sets it.
    #[cfg(any(test, feature = "testing"))]
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            walked: Mutex::default(),
            ttl,
        }
    }

    /// The roots the index holds: what the eviction every search performs
    /// left. Test-only; the product reads no count.
    #[cfg(any(test, feature = "testing"))]
    pub fn walked_roots(&self) -> usize {
        self.walked
            .lock()
            .map(|walked| walked.len())
            .unwrap_or_default()
    }

    /// `root`'s entries, reused while the walk is younger than the TTL. Every
    /// call first drops the roots whose walk is past it, so an index that
    /// lives as long as the process keeps only the roots still being
    /// searched.
    fn entries(
        &self,
        root: &Path,
        check: &impl Fn() -> Result<(), FileStorageError>,
    ) -> Result<Arc<Vec<Entry>>, FileStorageError> {
        if let Ok(mut walked) = self.walked.lock() {
            walked.retain(|_, (at, _)| at.elapsed() < self.ttl);
            if let Some((_, entries)) = walked.get(root) {
                return Ok(entries.clone());
            }
        }
        let entries = Arc::new(walk(root, check)?);
        if let Ok(mut walked) = self.walked.lock() {
            walked.insert(root.to_path_buf(), (Instant::now(), entries.clone()));
        }
        Ok(entries)
    }

    /// The best `limit` matches of `query` under `root`, best first.
    pub fn search(
        &self,
        root: &Path,
        query: &str,
        limit: usize,
        check: impl Fn() -> Result<(), FileStorageError>,
    ) -> Result<Vec<Candidate>, FileStorageError> {
        check()?;
        if query.starts_with('~')
            || query.starts_with('/')
            || query.split('/').any(|part| part == "..")
        {
            return Ok(Vec::new());
        }
        let (prefix, name) = match query.rfind('/') {
            Some(at) => query.split_at(at + 1),
            None => ("", query),
        };
        let (prefix, name) = (prefix.to_lowercase(), name.to_lowercase());
        let hidden = name.starts_with('.');
        let entries = self.entries(root, &check)?;
        let mut ranked: Vec<(i64, &Entry)> = Vec::new();
        for entry in entries.iter() {
            check()?;
            let Some(rest) = entry.lower.strip_prefix(prefix.as_str()) else {
                continue;
            };
            if rest.is_empty() || (!hidden && (rest.starts_with('.') || rest.contains("/."))) {
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
        ranked.sort_by(|(a, x), (b, y)| {
            b.cmp(a)
                .then_with(|| x.depth.cmp(&y.depth))
                .then_with(|| y.dir.cmp(&x.dir))
                .then_with(|| x.rel.cmp(&y.rel))
        });
        Ok(ranked
            .into_iter()
            .take(limit)
            .map(|(_, entry)| Candidate {
                relative_path: entry.rel.clone(),
                absolute_path: root.join(&entry.rel),
                dir: entry.dir,
            })
            .collect())
    }
}

/// The `limit` rule both file-search entry points share: `session.files.search`'s
/// existing one. Absent is [`DEFAULT_LIMIT`], and any given value clamps to
/// `1..=MAX_LIMIT`, so `0` answers one candidate. A negative value never
/// reaches here: it fails the parameter's deserialization.
pub fn parse_limit(limit: Option<usize>) -> usize {
    limit.map_or(DEFAULT_LIMIT, |limit| limit.clamp(1, MAX_LIMIT))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MentionsRequest {
    pub path: PathBuf,
    pub query: String,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct MentionsReply {
    pub candidates: Vec<Candidate>,
}

/// Walks `root` breadth first, honouring ignore files, within the depth,
/// entry and time budget.
fn walk(
    root: &Path,
    check: &impl Fn() -> Result<(), FileStorageError>,
) -> Result<Vec<Entry>, FileStorageError> {
    let started = Instant::now();
    let walker = WalkBuilder::new(root)
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
    let mut entries = Vec::new();
    for entry in walker {
        check()?;
        if started.elapsed() > WALK_BUDGET || entries.len() >= MAX_ENTRIES {
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
        let rel = rel.replace(std::path::MAIN_SEPARATOR, "/");
        entries.push(Entry {
            lower: rel.to_lowercase(),
            rel,
            dir: entry.file_type().is_some_and(|kind| kind.is_dir()),
            depth: entry.depth(),
        });
    }
    Ok(entries)
}

/// How well `query` matches `candidate`, both lowercased: an exact match, a
/// prefix, a substring, then the letters in order with few gaps. `None` for
/// no match; an empty query matches everything equally.
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
