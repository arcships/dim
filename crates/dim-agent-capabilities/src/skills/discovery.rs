//! Skill discovery and `SKILL.md` parsing, ported from TS
//! `packages/plugin/src/builtin/skill.ts` and `frontmatter.ts`.
use super::{SkillDefinition, SkillIssue};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

const SKILL_FILENAME: &str = "SKILL.md";
const MAX_SCAN_FILES: usize = 1000;
const MAX_DEPTH: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RootKind {
    User,
    Project,
    /// A plugin's skills root, registered under the plugin id.
    Plugin(String),
    /// A source a preview or an installation reads; never catalogued.
    Preview,
}

/// One scanned root: parsed skills in traversal order and per-file issues.
#[derive(Debug, Default)]
pub(crate) struct RootSnapshot {
    pub skills: Vec<Arc<SkillDefinition>>,
    pub issues: Vec<SkillIssue>,
}

/// Scans a whole root. A missing root is empty; an unreadable root fails.
pub(crate) fn scan_root(root: &Path, kind: RootKind) -> Result<RootSnapshot, SkillIssue> {
    let unreadable = |error: std::io::Error| SkillIssue::RootUnreadable {
        root: root.to_owned(),
        reason: error.kind().to_string(),
    };
    match std::fs::metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RootSnapshot::default());
        }
        Err(error) => return Err(unreadable(error)),
        Ok(_) => {}
    }
    std::fs::read_dir(root).map_err(unreadable)?;
    let mut scan = Scan {
        root,
        kind,
        count: 0,
        visited: HashSet::new(),
        snapshot: RootSnapshot::default(),
    };
    scan.walk(root, 0);
    if scan.count >= MAX_SCAN_FILES {
        scan.snapshot.issues.push(SkillIssue::ScanLimitReached {
            root: root.to_owned(),
        });
    }
    Ok(scan.snapshot)
}

struct Scan<'a> {
    root: &'a Path,
    kind: RootKind,
    count: usize,
    visited: HashSet<PathBuf>,
    snapshot: RootSnapshot,
}
impl Scan<'_> {
    fn walk(&mut self, dir: &Path, depth: usize) {
        if self.count >= MAX_SCAN_FILES || depth > MAX_DEPTH {
            return;
        }
        let Ok(real) = dir.canonicalize() else {
            return;
        };
        if !self.visited.insert(real) {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut names: Vec<_> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
            .collect();
        names.sort();
        for name in names {
            if self.count >= MAX_SCAN_FILES {
                return;
            }
            let path = dir.join(name);
            if !std::fs::metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                continue;
            }
            let manifest = path.join(SKILL_FILENAME);
            match std::fs::metadata(&manifest) {
                Ok(meta) if meta.is_file() => {
                    self.count += 1;
                    match parse_skill(&manifest, &path, self.root, &self.kind) {
                        Ok(skill) => self.snapshot.skills.push(Arc::new(skill)),
                        Err(issue) => self.snapshot.issues.push(issue),
                    }
                }
                Ok(_) => {}
                Err(_) => self.walk(&path, depth + 1),
            }
        }
    }
}

/// Mirrors TS `getSkillId`.
fn skill_id(skill_dir: &Path, root: &Path, kind: &RootKind) -> String {
    let rel = skill_dir
        .strip_prefix(root)
        .unwrap_or(skill_dir)
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let rel = if rel.trim().is_empty() {
        ".".into()
    } else {
        rel
    };
    match kind {
        RootKind::User => format!("user:{rel}"),
        RootKind::Project => format!(
            "workspace:{}:{rel}",
            root.to_string_lossy().replace('\\', "/")
        ),
        RootKind::Plugin(plugin) => format!("plugin:{plugin}:{rel}"),
        RootKind::Preview => format!("preview:{rel}"),
    }
}

fn parse_skill(
    manifest: &Path,
    skill_dir: &Path,
    root: &Path,
    kind: &RootKind,
) -> Result<SkillDefinition, SkillIssue> {
    let invalid = |reason: &str| SkillIssue::InvalidManifest {
        path: manifest.to_owned(),
        reason: reason.into(),
    };
    let bytes = std::fs::read(manifest).map_err(|error| SkillIssue::Unreadable {
        path: manifest.to_owned(),
        reason: error.kind().to_string(),
    })?;
    let text = String::from_utf8(bytes).map_err(|_| invalid("not valid UTF-8"))?;
    let (data, body) = parse_frontmatter(&text).ok_or_else(|| invalid("missing frontmatter"))?;
    let field = |key: &str| match data.get(key) {
        Some(Yaml::Str(value)) if !value.trim().is_empty() => Some(value.trim().to_owned()),
        _ => None,
    };
    let name = field("name").ok_or_else(|| invalid("missing name"))?;
    let description = field("description").ok_or_else(|| invalid("missing description"))?;
    if body.is_empty() {
        return Err(invalid("empty body"));
    }
    let body = body
        .replace("${SKILL_ROOT}", &skill_dir.to_string_lossy())
        .replace("${SKILLS_DIR}", &root.to_string_lossy());
    Ok(SkillDefinition {
        id: skill_id(skill_dir, root, kind),
        name,
        description,
        allowed_tools: normalize_allowed_tools(data.get("allowedTools")),
        file_path: manifest.to_owned(),
        body,
    })
}

/// Mirrors TS `normalizeAllowedTools`; descriptive only.
fn normalize_allowed_tools(raw: Option<&Yaml>) -> Vec<String> {
    let items: Vec<&str> = match raw {
        Some(Yaml::Str(item)) => vec![item],
        Some(Yaml::List(items)) => items.iter().map(String::as_str).collect(),
        _ => return Vec::new(),
    };
    let mut result = std::collections::BTreeSet::new();
    for item in items {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut name = trimmed;
        if let Some(open) = trimmed.find('(')
            && trimmed.ends_with(')')
            && open > 0
            && open + 2 < trimmed.len()
            && trimmed[..open]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            name = &trimmed[..open];
        }
        let name = name.to_lowercase();
        result.insert(match name.as_str() {
            "bash" | "shell" => "exec".to_owned(),
            _ => name,
        });
    }
    result.into_iter().collect()
}

#[derive(Debug, Clone, PartialEq)]
enum Yaml {
    Str(String),
    List(Vec<String>),
    Other,
}

/// Mirrors TS `parseSkillFile` over `parseFrontmatter`: a closed, non-empty
/// frontmatter block is required.
fn parse_frontmatter(input: &str) -> Option<(BTreeMap<String, Yaml>, String)> {
    let text = input.trim_start_matches('\u{feff}').trim_start();
    if !text.starts_with("---") {
        return None;
    }
    let after_open = 3 + text[3..].find('\n')?;
    let close = after_open + text[after_open..].find("\n---")?;
    let block = text.get(after_open + 1..close).unwrap_or("");
    let body = text[close + 4..].trim().to_owned();
    let data = parse_simple_yaml(block)?;
    (!data.is_empty()).then_some((data, body))
}

/// Mirrors TS `parseSimpleYaml`; `None` where TS throws.
fn parse_simple_yaml(block: &str) -> Option<BTreeMap<String, Yaml>> {
    let lines: Vec<&str> = block.split('\n').collect();
    let mut result = BTreeMap::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }
        let Some(colon) = trimmed.find(':') else {
            i += 1;
            continue;
        };
        let key = trimmed[..colon].trim().to_owned();
        let raw = trimmed[colon + 1..].trim();
        if matches!(raw, "|" | "|-" | ">" | ">-") {
            let (collected, next) = collect_indented_block(&lines, i + 1);
            let value = if raw.starts_with('|') {
                collected.join("\n")
            } else {
                collected
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let value = if raw.ends_with('-') {
                value
            } else {
                format!("{value}\n")
            };
            result.insert(key, Yaml::Str(value));
            i = next;
            continue;
        }
        if raw.is_empty() {
            let mut items = Vec::new();
            let mut j = i + 1;
            while j < lines.len() {
                let next = lines[j].trim();
                if let Some(item) = next.strip_prefix("- ") {
                    items.push(unquote(item.trim()).to_owned());
                    j += 1;
                } else if next.is_empty() || next.starts_with('#') {
                    j += 1;
                } else {
                    break;
                }
            }
            if items.is_empty() {
                result.insert(key, Yaml::Str(String::new()));
                i += 1;
            } else {
                result.insert(key, Yaml::List(items));
                i = j;
            }
            continue;
        }
        if raw.starts_with('[') {
            let inner = raw.strip_suffix(']')?;
            let inner = &inner[1..];
            let items = if inner.trim().is_empty() {
                Vec::new()
            } else {
                inner
                    .split(',')
                    .map(|item| unquote(item.trim()).to_owned())
                    .collect()
            };
            result.insert(key, Yaml::List(items));
            i += 1;
            continue;
        }
        result.insert(key, parse_scalar(raw));
        i += 1;
    }
    Some(result)
}

fn collect_indented_block(lines: &[&str], start: usize) -> (Vec<String>, usize) {
    let mut collected = Vec::new();
    let mut indent = None;
    let mut j = start;
    while j < lines.len() {
        let raw = lines[j];
        if raw.trim().is_empty() {
            collected.push(String::new());
            j += 1;
            continue;
        }
        let leading = raw.len() - raw.trim_start().len();
        let indent = *indent.get_or_insert(leading);
        if leading < indent {
            break;
        }
        collected.push(raw[indent..].to_owned());
        j += 1;
    }
    while collected.last().is_some_and(String::is_empty) {
        collected.pop();
    }
    (collected, j)
}

fn parse_scalar(raw: &str) -> Yaml {
    if matches!(raw, "true" | "false" | "null" | "~") {
        return Yaml::Other;
    }
    let unquoted = unquote(raw);
    if unquoted.len() != raw.len() {
        return Yaml::Str(unquoted.to_owned());
    }
    if raw.parse::<f64>().is_ok_and(|number| !number.is_nan()) {
        return Yaml::Other;
    }
    Yaml::Str(raw.to_owned())
}

fn unquote(s: &str) -> &str {
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}
