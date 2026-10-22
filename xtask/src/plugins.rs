//! Plugin documents under `docs/architecture/rust-agent/plugins/`.
//!
//! `graph` assembles the local execution profile, records its live rutis
//! diagnostics in `generated/local/services.json`, and rewrites the generated
//! block of every plugin document from them. `check` requires a document for
//! every business plugin in that profile and checks its generated block and
//! hand-written `## Dependencies` (or `## 依赖`) table against the diagnostics.
use dim_agent_capabilities::{
    defaults,
    file_storage::FileStoragePlugin,
    host::{
        PutSessionFile, SessionFileError, SessionFileErrorCode, SessionFilesHost, StoredSessionFile,
    },
    host_platform::{HostPlatform, HostPlatformConfig, HostPlatformPlugin, ShellStatus},
    model_provider::ModelProviderPlugin,
    permission::{CommandRulesHost, NoCommandRules},
    skill_settings::SkillSettingsHost,
    skills::{MemorySkillSettings, SkillsCatalogPlugin},
    workspace_tools::{FileWriteCoordinatorPlugin, WorkspaceToolsConfig},
};
use dim_agent_runtime::capability_plugin;
use dim_agent_runtime::*;
use rutis::{BoxFuture, CordisError, Ctx, Effect, TypeKey, TypedPlugin};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const BEGIN: &str = "<!-- generated:plugin ";
const END: &str = "<!-- /generated -->";
const TOOLS: [&str; 8] = [
    "read", "write", "edit", "glob", "grep", "exec", "wait", "skill",
];
// Runtime assembly and the scripted model adapter are not business plugins.
// Keep this exclusion list narrow: a newly assembled capability needs a page.
const INFRASTRUCTURE: [&str; 9] = [
    "rutis-loader",
    "session",
    "branch",
    "SessionScope",
    "BranchScope",
    "SessionBroadcast",
    "SessionBroadcastEnd",
    "PrepareModelHost",
    "SessionExtension",
];

/// Graph-only host facts. Keep real local-execution dependencies but never scan
/// the developer's home or make the graph depend on their installed skills.
struct GraphPlatform(PathBuf);
impl TypedPlugin for GraphPlatform {
    type Deps = <HostPlatformPlugin as TypedPlugin>::Deps;
    fn name(&self) -> &str {
        "HostPlatform"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        _: Self::Deps,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            let mut platform = HostPlatform::current(ShellStatus::Unavailable {
                code: "GRAPH_ONLY",
                reason: "Graph generation does not execute commands".into(),
            })
            .map_err(|error| CordisError::PluginFailed(error.into()))?;
            platform.home = self.0.clone();
            ctx.provide_as(TypeKey::of::<HostPlatform>(), Arc::new(platform))?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for GraphPlatform {
    fn keys(&self) -> KeysOf<Self> {
        HostPlatformPlugin::new(HostPlatformConfig {
            exec_shell_file: Some((0, "exec-shell.json".into())),
            isolation_file: Some((0, "host-isolation.json".into())),
        })
        .keys()
    }
}

/// Graph-only `sessionFiles`, which SessionStore provides in a host with
/// native storage. The graph stores no files.
struct GraphFiles;
impl PutSessionFile for GraphFiles {
    fn put(
        &self,
        _: String,
        _: String,
        _: Vec<u8>,
        _: tokio_util::sync::CancellationToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = std::result::Result<StoredSessionFile, SessionFileError>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async {
            Err(SessionFileError {
                code: SessionFileErrorCode::NodeUnavailable,
                message: "Graph generation stores no files".into(),
            })
        })
    }
}

fn docs_dir(root: &Path) -> PathBuf {
    root.join("docs/architecture/rust-agent/plugins")
}
fn graph_file(root: &Path) -> PathBuf {
    root.join("docs/architecture/rust-agent/generated/local/services.json")
}

/// Assembles the local execution profile as the product composition root
/// does, with scripted model hosts, and returns its diagnostics per scope.
pub async fn local_graph() -> Result<BTreeMap<&'static str, Value>> {
    let dir = std::env::temp_dir().join(format!("dim-xtask-local-{}", std::process::id()));
    let (storage, work) = (dir.join("storage"), dir.join("work"));
    std::fs::create_dir_all(&storage)?;
    std::fs::create_dir_all(&work)?;
    let skill = storage.join(".agents/skills/graph");
    std::fs::create_dir_all(&skill)?;
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: graph\ndescription: Architecture fixture\n---\nGraph fixture instructions.\n",
    )?;
    let process = rutis::Ctx::root()?;
    // FileStorage refuses symlinked components such as macOS `/var`.
    (&process.plugin(capability_plugin(FileStoragePlugin::new(vec![
        storage.canonicalize()?,
    ]))))
        .await?;
    (&process.plugin(capability_plugin(GraphPlatform(storage.canonicalize()?)))).await?;
    process.provide_as(
        TypeKey::of::<SkillSettingsHost>(),
        Arc::new(SkillSettingsHost(Arc::new(MemorySkillSettings::default()))),
    )?;
    process.provide_as(
        TypeKey::of::<SessionFilesHost>(),
        Arc::new(SessionFilesHost(Arc::new(GraphFiles))),
    )?;
    (&process.plugin(capability_plugin(SkillsCatalogPlugin::new()))).await?;
    (&process.plugin(capability_plugin(FileWriteCoordinatorPlugin::default()))).await?;

    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_model_hosts(&mut builder);
    builder.session_value(Arc::new(CommandRulesHost(Arc::new(NoCommandRules))));
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::new,
        WorkspaceToolsConfig {
            output_root: dir.join("outputs"),
        },
    );
    // `serve` adds Attachments where the host has native storage.
    dim_agent_attachments::install(&mut builder);
    // The product profiles add Questions.
    dim_agent_questions::install(&mut builder);
    let agent = builder.build_in(&process);
    let session = agent
        .create_session(SessionConfig {
            cwd: work.canonicalize()?.to_string_lossy().into_owned(),
            tools: TOOLS.map(String::from).to_vec(),
            ..SessionConfig::default()
        })
        .await?;
    let session_diagnostics = session.diagnostics()?;
    let main_diagnostics = session.branch_diagnostics("main")?;
    // Scoped keys carry the instance of the scope plugin's loader group.
    let instance = |d: &rutis::RuntimeDiagnostics, name: &str| {
        let scope = d
            .plugins
            .iter()
            .find(|p| p.name == name)
            .expect("scope plugin");
        d.plugins
            .iter()
            .find(|p| Some(p.id) == scope.parent)
            .map(|p| p.instance)
            .expect("scope group")
    };
    let scopes = [
        (instance(&session_diagnostics, "SessionScope"), "session"),
        (instance(&main_diagnostics, "BranchScope"), "main"),
    ];
    let mut snapshot = process.diagnostics();
    let root = process.root_view().expect("process root").id;
    snapshot
        .plugins
        .retain(|plugin| plugin.parent == Some(root) && plugin.instance != scopes[0].0);
    let members: Vec<_> = snapshot.plugins.iter().map(|plugin| plugin.id).collect();
    snapshot
        .bindings
        .retain(|binding| members.contains(&binding.provider));
    let graphs = BTreeMap::from([
        ("process", crate::diagnostics(snapshot, &scopes)),
        ("session", crate::diagnostics(session_diagnostics, &scopes)),
        ("main", crate::diagnostics(main_diagnostics, &scopes)),
    ]);
    agent.close().await?;
    process.shutdown().await?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(graphs)
}

/// Writes the local graph and rewrites every plugin document's block.
pub fn write(root: &Path, graphs: &BTreeMap<&'static str, Value>) -> Result<()> {
    let file = graph_file(root);
    std::fs::create_dir_all(file.parent().unwrap())?;
    std::fs::write(&file, serde_json::to_string_pretty(graphs)? + "\n")?;
    let mermaid = crate::mermaid(graphs);
    std::fs::write(file.with_file_name("services.mmd"), &mermaid)?;
    std::fs::write(
        file.with_file_name("services.md"),
        format!(
            "# 本地执行运行组合\n\n由真实 rutis 装配 `defaults::install_local_execution` 生成（模型为脚本化 mock）。重新生成：`cargo run -p xtask -- graph`。\n\n```mermaid\n{mermaid}```\n"
        ),
    )?;
    let facts = Facts::new(graphs);
    for (path, name) in documents(root)? {
        let text = std::fs::read_to_string(&path)?;
        let (before, _, after) = split_block(&text, &name, &path)?;
        let block = facts.block(&name)?;
        std::fs::write(&path, format!("{before}{block}{after}"))?;
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
    let documented: BTreeSet<_> = documents.iter().map(|(_, name)| name.as_str()).collect();
    for (_, plugin) in &facts.plugins {
        let name = plugin["name"].as_str().ok_or("plugin graph has no name")?;
        if !INFRASTRUCTURE.contains(&name) && !documented.contains(name) {
            errors.push(format!(
                "business plugin `{name}` has no document in {}",
                docs_dir(root).display()
            ));
        }
    }
    for (path, name) in &documents {
        let text = std::fs::read_to_string(path)?;
        let (_, block, _) = split_block(&text, name, path)?;
        match facts.block(name) {
            Err(error) => errors.push(error.to_string()),
            Ok(expected) if expected != block => errors.push(format!(
                "{}: generated block is stale; run `cargo run -p xtask -- graph`",
                path.display()
            )),
            Ok(_) => {}
        }
        let injected: BTreeSet<String> = facts
            .plugin(name)
            .map(|plugin| facts.injects(plugin).into_iter().collect())
            .unwrap_or_default();
        let explained = dependency_rows(&text);
        for missing in injected.difference(&explained) {
            errors.push(format!(
                "{}: dependency table does not explain injected `{missing}`",
                path.display()
            ));
        }
        for extra in explained.difference(&injected) {
            errors.push(format!(
                "{}: dependency table explains `{extra}`, which the plugin does not inject",
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

/// `(path, plugin name)` of every document; the file stem is the plugin name.
fn documents(root: &Path) -> Result<Vec<(PathBuf, String)>> {
    let dir = docs_dir(root);
    let entries = std::fs::read_dir(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let mut documents = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if path.extension().is_some_and(|ext| ext == "md") && stem != "README" {
            documents.push((path.clone(), stem.to_owned()));
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
        if line.starts_with("## ") {
            inside = matches!(line.trim(), "## Dependencies" | "## 依赖");
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

    #[test]
    fn codes_drop_module_paths_and_name_the_scope() {
        assert_eq!(
            code("dim_agent_capabilities::approval::Approval@main"),
            "B:Approval"
        );
        assert_eq!(
            code("dim_agent_capabilities::tool_registry::ToolContribution#branch@session"),
            "S:ToolContribution#branch"
        );
        assert_eq!(
            code("dim_agent_runtime::StateCell<dim_agent_types::LoopSnapshot>@main"),
            "B:StateCell<LoopSnapshot>"
        );
        assert_eq!(
            code("dim_agent_capabilities::host_platform::HostPlatform"),
            "P:HostPlatform"
        );
    }

    #[test]
    fn dependency_rows_read_only_the_dependency_table() {
        let text = "## 依赖\n\n| 服务 | 用途 |\n| --- | --- |\n| `B:Permission` | 裁决 |\n\n## 事件\n\n| `approval/request` | x |\n";
        assert_eq!(
            dependency_rows(text),
            BTreeSet::from(["B:Permission".to_owned()])
        );
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
            write(&root, &graphs)?;
            assert!(check(&root).is_err(), "empty document set must not pass");
            let path = docs_dir(&root).join("Example.md");
            let page = format!(
                "# Example\n\n## Dependencies\n\n| Service | Purpose |\n| --- | --- |\n| `B:Timeline` | Commit results |\n\n{BEGIN}Example -->\n{END}\n"
            );
            std::fs::write(&path, &page)?;
            write(&root, &graphs)?;
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
            write(&root, &graphs)?;
            assert!(
                check(&root).is_err(),
                "new business plugins require documentation"
            );
            graphs.get_mut("main").unwrap()["plugins"]
                .as_array_mut()
                .unwrap()
                .pop();
            write(&root, &graphs)?;
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
