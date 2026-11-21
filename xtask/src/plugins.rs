//! Local execution profile graph generation for plugin documents under
//! `docs/architecture/rust-agent/plugins/`.
//!
//! `graph` assembles the local execution profile, records its live rutis
//! diagnostics in `generated/local/services.json`, and delegates generated
//! block updates and static document checks to `xtask_check::plugin_docs`.
use dim_agent_capabilities::{
    defaults,
    execution_settings::ExecutionSettingsHost,
    file_storage::FileStorage,
    file_storage::FileStoragePlugin,
    hooks::{HookSourcesHost, NoHookSources},
    host::{
        SessionFileError, SessionFileErrorCode, SessionFiles, SessionFilesService,
        StoredSessionFile,
    },
    host_platform::{HostPlatform, ShellStatus},
    model_provider::ModelProviderPlugin,
    permission::{CommandRules, NoCommandRules},
    skill_settings::SkillSettingsHost,
    skills::{MemorySkillSettings, SkillsCatalogPlugin},
    toolchain::ToolchainSourceHost,
    workspace_tools::WorkspaceToolsConfig,
};
use dim_agent_runtime::{testing::MinimalHost, *};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const TOOLS: [&str; 8] = [
    "read", "write", "edit", "glob", "grep", "exec", "wait", "skill",
];

/// Graph-only host facts. Keep real local-execution dependencies but never scan
/// the developer's home or make the graph depend on their installed skills.
struct GraphPlatform {
    home: PathBuf,
    storage: Key<FileStorage>,
    execution: Key<ExecutionSettingsHost>,
    toolchain: Key<ToolchainSourceHost>,
}
impl DynPlugin for GraphPlatform {
    fn name(&self) -> &str {
        "HostPlatform"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.storage.erase(),
            self.execution.erase(),
            self.toolchain.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let execution = ctx.require_at(&self.execution)?;
            let mut platform = HostPlatform::current(ShellStatus::Unavailable {
                code: "GRAPH_ONLY",
                reason: "Graph generation does not execute commands".into(),
            })
            .map_err(|error| rutis::Error::from(ErrorKind::PluginFailed(error.into())))?
            .with_execution_settings((*execution).clone());
            let toolchain = ctx.require_at(&self.toolchain)?;
            platform = platform.with_toolchain_source((*toolchain).clone());
            platform.home = self.home.clone();
            ctx.provide_as::<HostPlatform>(Arc::new(platform))?;
            Ok(Effect::Done)
        })
    }
}

/// Graph-only `sessionFiles`, which SessionStore provides in a host with
/// native storage. The graph stores no files.
struct GraphFiles;
impl SessionFilesService for GraphFiles {
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

    fn register(
        &self,
        _: String,
        _: PathBuf,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = std::result::Result<(), SessionFileError>> + Send + '_,
        >,
    > {
        Box::pin(async {
            Err(SessionFileError {
                code: SessionFileErrorCode::NodeUnavailable,
                message: "Graph generation stores no output".into(),
            })
        })
    }

    fn owns(
        &self,
        _: String,
        _: PathBuf,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = std::result::Result<bool, SessionFileError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async { Ok(false) })
    }

    fn owned_paths(
        &self,
        _: String,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = std::result::Result<Vec<PathBuf>, SessionFileError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }
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
    let home = storage.canonicalize()?;
    let mut builder = AgentBuilder::new(AgentConfig::default());
    // The host's own plugins are rows of its loader, as the App Server places
    // them.
    let files = FileStoragePlugin::new(vec![home.clone()]);
    builder.plugin(move |_| Ok(Box::new(files.clone())));
    builder.plugin(move |_| {
        Ok(Box::new(GraphPlatform {
            home: home.clone(),
            storage: Key::of(),
            execution: Key::of(),
            toolchain: Key::of(),
        }))
    });
    defaults::mock_model_hosts(&mut builder);
    builder.session_data_value(Arc::new(CommandRules(Arc::new(NoCommandRules))));
    builder.session_value(Arc::new(HookSourcesHost(Arc::new(NoHookSources))));
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::new,
        WorkspaceToolsConfig {
            boundary: None,
            output_root: dir.join("outputs"),
        },
        dim_agent_capabilities::timeline_tree::BranchToolConfig::default(),
    );
    dim_agent_capabilities::prompt_suggestion::install_local(&mut builder);
    dim_agent_capabilities::hooks::install(&mut builder);
    dim_agent_attachments::install(&mut builder);
    dim_agent_questions::install(&mut builder);
    builder.plugin(|names| Ok(Box::new(SkillsCatalogPlugin::new(names)?)));
    let agent = MinimalHost::start_with(&builder, |root, _| {
        root.provide_as::<SkillSettingsHost>(Arc::new(SkillSettingsHost(Arc::new(
            MemorySkillSettings::default(),
        ))))?;
        root.provide_as::<SessionFiles>(Arc::new(SessionFiles(Arc::new(GraphFiles))))?;
        root.provide_as::<ExecutionSettingsHost>(Arc::new(ExecutionSettingsHost::fixed(
            Default::default(),
        )))?;
        root.provide_as::<ToolchainSourceHost>(Arc::new(ToolchainSourceHost::fixed(vec![])))?;
        Ok(())
    })
    .await?;
    let process = agent.root().clone();
    let session = agent
        .create_session(SessionConfig {
            cwd: work.canonicalize()?.to_string_lossy().into_owned(),
            tools: Some(TOOLS.map(String::from).to_vec()),
            ..SessionConfig::default()
        })
        .await?;
    let session_diagnostics = session.diagnostics()?;
    let main_diagnostics = session.branch_diagnostics("main")?;
    let instance = |d: &rutis::host::diagnostics::RuntimeDiagnostics, name: &str| {
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
    let loader = snapshot
        .plugins
        .iter()
        .find(|plugin| plugin.parent == Some(root) && plugin.name == "rutis-loader")
        .map(|plugin| plugin.id);
    let providers: BTreeSet<_> = snapshot
        .bindings
        .iter()
        .map(|binding| binding.provider)
        .collect();
    snapshot.plugins.retain(|plugin| {
        let own = plugin.parent.is_some()
            && plugin.parent == loader
            && providers.contains(&plugin.id)
            && plugin.name != "PartRegistry";
        (plugin.parent == Some(root) || own) && plugin.instance != scopes[0].0
    });
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
    let _ = std::fs::remove_dir_all(&dir);
    Ok(graphs)
}

/// Writes the local graph and rewrites every plugin document's block.
pub fn write(root: &Path, graphs: &BTreeMap<&'static str, Value>) -> Result<()> {
    let file = xtask_check::plugin_docs::graph_file(root);
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
    xtask_check::plugin_docs::rewrite(root, graphs)?;
    Ok(())
}
