//! Static checks and block rewriting for plugin documents under
//! `docs/architecture/rust-agent/plugins/` against the recorded local graph.

use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const BEGIN: &str = "<!-- generated:plugin ";
const END: &str = "<!-- /generated -->";
const INFRASTRUCTURE: [&str; 12] = [
    "rutis-loader",
    "session",
    "branch",
    "SessionScope",
    "BranchScope",
    // The instances' mounters (ADR 006 D3); the parts they mount have pages.
    "SessionParts",
    "SessionOptionalParts",
    "BranchParts",
    "BranchOptionalParts",
    "SessionBroadcast",
    "SessionBroadcastEnd",
    "PrepareModelHost",
];

fn docs_dir(root: &Path) -> PathBuf {
    root.join("docs/architecture/rust-agent/plugins")
}

pub fn graph_file(root: &Path) -> PathBuf {
    root.join("docs/architecture/rust-agent/generated/local/services.json")
}

/// Rewrites every plugin document's generated blocks from `graphs`.
pub fn rewrite<K: AsRef<str>>(root: &Path, graphs: &BTreeMap<K, Value>) -> Result<()> {
    let facts = Facts::new(graphs);
    for (path, names) in documents(root)? {
        let mut text = std::fs::read_to_string(&path)?;
        for name in names {
            let (before, _, after) = split_block(&text, &name, &path)?;
            text = format!("{before}{}{after}", facts.block(&name)?);
        }
        std::fs::write(&path, text)?;
    }
    Ok(())
}

/// Checks every plugin document against the recorded local graph.
pub fn check(root: &Path) -> Result<()> {
    let documents = documents(root)?;
    if documents.is_empty() {
        return Err(format!("{}: no plugin documents", docs_dir(root).display()).into());
    }
    let graphs: BTreeMap<String, Value> =
        serde_json::from_str(&std::fs::read_to_string(graph_file(root))?)?;
    let facts = Facts::new(&graphs);
    let mut errors = Vec::new();
    let documented: BTreeSet<_> = documents
        .iter()
        .flat_map(|(_, names)| names.iter().map(String::as_str))
        .collect();
    for (_, plugin) in &facts.plugins {
        let name = plugin["name"].as_str().ok_or("plugin graph has no name")?;
        if !INFRASTRUCTURE.contains(&name) && !documented.contains(name) {
            errors.push(format!(
                "business plugin `{name}` has no document in {}",
                docs_dir(root).display()
            ));
        }
    }
    for (path, name, section) in documents
        .iter()
        .map(|(path, names)| Ok((path, names, std::fs::read_to_string(path)?)))
        .collect::<Result<Vec<_>>>()?
        .iter()
        .flat_map(|(path, names, text)| {
            sections(text, names).map(move |(name, section)| (*path, name, section))
        })
    {
        let (_, block, _) = split_block(section, name, path)?;
        match facts.block(name) {
            Err(error) => errors.push(error.to_string()),
            Ok(expected) if expected != block => errors.push(format!(
                "{}: `{name}`: generated block is stale; run `cargo run -p xtask -- graph`",
                path.display()
            )),
            Ok(_) => {}
        }
        let injected: BTreeSet<String> = facts
            .plugin(name)
            .map(|plugin| facts.injects(plugin).into_iter().collect())
            .unwrap_or_default();
        let explained = dependency_rows(section);
        for missing in injected.difference(&explained) {
            errors.push(format!(
                "{}: `{name}`: dependency table does not explain injected `{missing}`",
                path.display()
            ));
        }
        for extra in explained.difference(&injected) {
            errors.push(format!(
                "{}: `{name}`: dependency table explains `{extra}`, which it does not inject",
                path.display()
            ));
        }
    }
    if errors.is_empty() {
        println!("plugin documents passed ({} documents)", documents.len());
        Ok(())
    } else {
        Err(errors.join("\n").into())
    }
}

/// Each part a page documents with the text that documents it: from the end
/// of the previous part's block, or the start, to the end of its own block.
fn sections<'a>(
    text: &'a str,
    names: &'a [String],
) -> impl Iterator<Item = (&'a String, &'a str)> + 'a {
    let mut start = 0;
    names.iter().map(move |name| {
        let marker = format!("{BEGIN}{name} ");
        let end = text[start..]
            .find(&marker)
            .and_then(|at| {
                text[start + at..]
                    .find(END)
                    .map(|end| start + at + end + END.len())
            })
            .unwrap_or(text.len());
        let section = &text[start..end];
        start = end;
        (name, section)
    })
}

/// The parts or process plugins a page documents: the names of its
/// generated blocks, in order.
fn documented(text: &str) -> Vec<String> {
    text.match_indices(BEGIN)
        .filter_map(|(at, _)| {
            text[at + BEGIN.len()..]
                .split_whitespace()
                .next()
                .map(str::to_owned)
        })
        .collect()
}

/// `(path, documented names)` of every page; a page documents at least one.
fn documents(root: &Path) -> Result<Vec<(PathBuf, Vec<String>)>> {
    let dir = docs_dir(root);
    let entries = std::fs::read_dir(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let mut documents = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if path.extension().is_some_and(|ext| ext == "md") && stem != "README" {
            let names = documented(&std::fs::read_to_string(&path)?);
            if names.is_empty() {
                return Err(format!("{}: no generated block", path.display()).into());
            }
            documents.push((path.clone(), names));
        }
    }
    documents.sort();
    Ok(documents)
}

/// `(before, block, after)`; the block spans both markers.
fn split_block<'a>(text: &'a str, name: &str, path: &Path) -> Result<(&'a str, &'a str, &'a str)> {
    let marker = format!("{BEGIN}{name} ");
    let start = text
        .find(&marker)
        .ok_or_else(|| format!("{}: missing `{marker}...` block", path.display()))?;
    let end = text[start..]
        .find(END)
        .map(|offset| start + offset + END.len())
        .ok_or_else(|| format!("{}: unterminated generated block", path.display()))?;
    Ok((&text[..start], &text[start..end], &text[end..]))
}

/// First-column codes of the hand-written dependency table.
fn dependency_rows(text: &str) -> BTreeSet<String> {
    let mut rows = BTreeSet::new();
    let mut inside = false;
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("##") {
            inside = matches!(
                heading.trim_start_matches('#').trim(),
                "Dependencies" | "依赖"
            );
            continue;
        }
        let Some(cell) = line
            .strip_prefix('|')
            .and_then(|rest| rest.split('|').next())
        else {
            continue;
        };
        let cell = cell.trim();
        if inside && cell.starts_with('`') && cell.ends_with('`') && cell.len() > 2 {
            rows.insert(cell.trim_matches('`').to_owned());
        }
    }
    rows
}

/// `dim_agent_x::m::Name<dim_agent_y::T>#key@main` → `B:Name<T>#key`.
fn code(service: &str) -> String {
    let (path, scope) = match service.rsplit_once('@') {
        Some((path, "main")) => (path, "B"),
        Some((path, "session")) => (path, "S"),
        Some((path, _)) => (path, "?"),
        None => (service, "P"),
    };
    let mut short = String::new();
    let mut segment = String::new();
    let mut chars = path.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == ':' && chars.peek() == Some(&':') {
            chars.next();
            segment.clear();
            continue;
        }
        if ch.is_alphanumeric() || ch == '_' {
            segment.push(ch);
            continue;
        }
        short.push_str(&segment);
        segment.clear();
        short.push(ch);
    }
    short.push_str(&segment);
    format!("{scope}:{short}")
}

struct Facts<'a> {
    /// `(scope, plugin)` of every plugin in the profile.
    plugins: Vec<(&'a str, &'a Value)>,
    bindings: Vec<&'a Value>,
}
impl<'a> Facts<'a> {
    fn new<K: AsRef<str>>(graphs: &'a BTreeMap<K, Value>) -> Self {
        let mut plugins = Vec::new();
        let mut bindings = Vec::new();
        for (scope, graph) in graphs {
            for plugin in graph["plugins"].as_array().into_iter().flatten() {
                plugins.push((scope.as_ref(), plugin));
            }
            bindings.extend(graph["bindings"].as_array().into_iter().flatten());
        }
        Self { plugins, bindings }
    }
    fn plugin(&self, name: &str) -> Option<&(&'a str, &'a Value)> {
        self.plugins
            .iter()
            .find(|(_, plugin)| plugin["name"] == name)
    }
    fn injects(&self, (_, plugin): &(&str, &Value)) -> Vec<String> {
        plugin["injects"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|inject| code(inject["service"].as_str().unwrap_or_default()))
            .collect()
    }
    fn block(&self, name: &str) -> Result<String> {
        let found = self
            .plugin(name)
            .ok_or_else(|| format!("plugin `{name}` is not in the local execution profile"))?;
        let (scope, plugin) = found;
        let id = &plugin["id"];
        let scope = match *scope {
            "process" => "进程",
            "session" => "Session",
            _ => "分支",
        };
        let list = |items: BTreeSet<String>| {
            if items.is_empty() {
                "—".to_owned()
            } else {
                items
                    .into_iter()
                    .map(|item| format!("`{item}`"))
                    .collect::<Vec<_>>()
                    .join("、")
            }
        };
        let injects = list(self.injects(found).into_iter().collect());
        let provides = list(
            self.bindings
                .iter()
                .filter(|binding| binding["provider"] == *id)
                .map(|binding| code(binding["service"].as_str().unwrap_or_default()))
                .collect(),
        );
        let dependents = list(
            self.plugins
                .iter()
                .filter(|(_, other)| {
                    other["resolved"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|edge| edge["provider"] == *id)
                })
                .map(|(_, other)| other["name"].as_str().unwrap_or_default().to_owned())
                .collect(),
        );
        Ok(format!(
            "{BEGIN}{name} — 由 `cargo run -p xtask -- graph` 从真实装配生成，勿手改 -->\n| 项 | 值 |\n| --- | --- |\n| 作用域 | {scope} |\n| 注入 | {injects} |\n| 提供 | {provides} |\n| 被注入于 | {dependents} |\n{END}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fixture(root: &Path, graphs: &BTreeMap<&'static str, Value>) -> Result<()> {
        let file = graph_file(root);
        std::fs::create_dir_all(file.parent().unwrap())?;
        std::fs::write(&file, serde_json::to_string_pretty(graphs)? + "\n")?;
        rewrite(root, graphs)
    }

    #[test]
    fn documentation_check_requires_complete_profile_and_exact_dependencies() {
        let root = std::env::temp_dir().join(format!(
            "dim-plugin-doc-check-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let result = (|| -> Result<()> {
            assert!(check(&root).is_err(), "missing directory must not pass");
            std::fs::create_dir_all(docs_dir(&root))?;
            let mut graphs = BTreeMap::from([(
                "main",
                serde_json::json!({
                    "plugins": [
                        {"id":1,"name":"branch","injects":[],"resolved":[]},
                        {"id":2,"name":"Example","injects":[{"service":"crate::Timeline@main"}],"resolved":[]}
                    ],
                    "bindings":[]
                }),
            )]);
            write_fixture(&root, &graphs)?;
            assert!(check(&root).is_err(), "empty document set must not pass");
            let path = docs_dir(&root).join("Example.md");
            let page = format!(
                "# Example\n\n## Dependencies\n\n| Service | Purpose |\n| --- | --- |\n| `B:Timeline` | Commit results |\n\n{BEGIN}Example -->\n{END}\n"
            );
            std::fs::write(&path, &page)?;
            write_fixture(&root, &graphs)?;
            check(&root)?;
            let valid = std::fs::read_to_string(&path)?;
            std::fs::write(
                &path,
                valid.replace("| `B:Timeline` | Commit results |", ""),
            )?;
            assert!(
                check(&root).is_err(),
                "a missing dependency explanation must fail"
            );
            std::fs::write(&path, &valid)?;
            graphs.get_mut("main").unwrap()["plugins"]
                .as_array_mut()
                .unwrap()
                .push(
                    serde_json::json!({"id":3,"name":"NewCapability","injects":[],"resolved":[]}),
                );
            write_fixture(&root, &graphs)?;
            assert!(
                check(&root).is_err(),
                "new business plugins require documentation"
            );
            graphs.get_mut("main").unwrap()["plugins"]
                .as_array_mut()
                .unwrap()
                .pop();
            write_fixture(&root, &graphs)?;
            std::fs::write(
                docs_dir(&root).join("Removed.md"),
                page.replace("Example", "Removed"),
            )?;
            assert!(
                check(&root).is_err(),
                "removed plugins must not retain orphan documents"
            );
            std::fs::remove_file(docs_dir(&root).join("Removed.md"))?;
            check(&root)
        })();
        std::fs::remove_dir_all(root).unwrap();
        result.unwrap();
    }
}
