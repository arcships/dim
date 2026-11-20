use super::{HistoryEntry, HistoryRenderer, StreamedContext};
use crate::{port_faces, timeline_tree::TimelineTree};
use dim_agent_runtime::{ContextHistory, Names};
use dim_agent_types::{AgentEvent, ProjectionCheckpoint, RecoverySnapshot};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis_bridge::session::{HostDispatch, Reply, Value};
use serde::Deserialize;
use std::sync::Arc;

const NAME: &str = "historyRenderer";

pub(crate) struct HistoryFacePlugin {
    renderer: Key<HistoryRenderer>,
    tree: Key<TimelineTree>,
}

impl HistoryFacePlugin {
    pub(crate) fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            renderer: names.key()?,
            tree: names.key()?,
        })
    }
}

impl DynPlugin for HistoryFacePlugin {
    fn name(&self) -> &str {
        "HistoryRenderer"
    }

    fn injects(&self) -> Vec<TypeKey> {
        vec![self.renderer.erase(), self.tree.erase()]
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let host = Arc::new(HistoryFace {
                renderer: ctx.require_at(&self.renderer)?,
                tree: ctx.require_at(&self.tree)?,
            });
            let instance = self
                .renderer
                .erase()
                .instance_id()
                .expect("Session renderer");
            ctx.provide_at(Key::<HistoryFace>::of().instance(instance), host.clone())?;
            ctx.provide_at(
                rutis_bridge::session::host_key_in(
                    NAME,
                    self.renderer
                        .erase()
                        .instance_id()
                        .expect("Session renderer"),
                ),
                host as Arc<dyn HostDispatch>,
            )?;
            Ok(Effect::Done)
        })
    }
}

#[derive(Clone)]
pub(crate) struct HistoryFace {
    renderer: Arc<HistoryRenderer>,
    tree: Arc<TimelineTree>,
}

impl HistoryFace {
    pub(crate) fn key(names: &Names<'_>) -> rutis::Result<Key<Self>> {
        Ok(Key::of().instance(names.instance(dim_agent_runtime::SESSION_GROUP)?))
    }
    pub(crate) async fn rendered_context(
        &self,
        branch: &str,
        through: u64,
    ) -> dim_agent_runtime::Result<(ContextHistory, Vec<HistoryEntry>)> {
        self.renderer
            .rendered_context(&self.tree.history_timeline(branch)?, through)
            .await
    }
    pub(crate) async fn rendered_stream(
        &self,
        branch: &str,
        through: u64,
        batch: u64,
    ) -> dim_agent_runtime::Result<StreamedContext> {
        self.renderer
            .rendered_stream(&self.tree.history_timeline(branch)?, through, batch)
            .await
    }
    pub(crate) async fn stream_checkpoint(
        &self,
        branch: &str,
        through: u64,
        retain_from: &str,
        batch: u64,
    ) -> dim_agent_runtime::Result<ProjectionCheckpoint> {
        self.renderer
            .stream_checkpoint(
                &self.tree.history_timeline(branch)?,
                through,
                retain_from,
                batch,
            )
            .await
    }
    pub(crate) fn snapshot(
        &self,
        context: &ContextHistory,
        retain_from: &str,
        anchor: &AgentEvent,
    ) -> Result<RecoverySnapshot, String> {
        self.renderer.snapshot(context, retain_from, anchor)
    }
    pub(crate) fn snapshot_from_checkpoint(
        &self,
        checkpoint: &ProjectionCheckpoint,
        anchor: &AgentEvent,
    ) -> Result<RecoverySnapshot, String> {
        self.renderer.snapshot_from_checkpoint(checkpoint, anchor)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Prefix {
    branch_id: String,
    through: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Checkpoint {
    branch_id: String,
    through: u64,
    retain_from: String,
    batch: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Snapshot {
    context: ContextHistory,
    retain_from: String,
    anchor: AgentEvent,
}

impl HostDispatch for HistoryFace {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(
            serde_json::json!({"context":"async","renderedContext":"async","streamCheckpoint":"async","snapshot":"async"}),
        )
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        let owner = self.clone();
        match method {
            "context" => {
                let Prefix { branch_id, through } = port_faces::request(NAME, args)?;
                port_faces::serving(NAME, async move {
                    let timeline = owner
                        .tree
                        .history_timeline(&branch_id)
                        .map_err(|e| e.to_string())?;
                    owner
                        .renderer
                        .context(&timeline, through)
                        .await
                        .map_err(|e| e.to_string())
                })
            }
            "renderedContext" => {
                #[derive(serde::Serialize)]
                struct Rendered {
                    context: ContextHistory,
                    history: Vec<super::HistoryEntry>,
                }
                let Prefix { branch_id, through } = port_faces::request(NAME, args)?;
                port_faces::serving(NAME, async move {
                    let (context, history) = owner
                        .rendered_context(&branch_id, through)
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok::<_, String>(Rendered { context, history })
                })
            }
            "snapshot" => {
                let Snapshot {
                    context,
                    retain_from,
                    anchor,
                } = port_faces::request(NAME, args)?;
                port_faces::serving(NAME, async move {
                    owner.snapshot(&context, &retain_from, &anchor)
                })
            }
            "streamCheckpoint" => {
                let Checkpoint {
                    branch_id,
                    through,
                    retain_from,
                    batch,
                } = port_faces::request(NAME, args)?;
                port_faces::serving(NAME, async move {
                    owner
                        .stream_checkpoint(&branch_id, through, &retain_from, batch)
                        .await
                        .map_err(|e| e.to_string())
                })
            }
            _ => Err(port_faces::unknown_method(NAME, method)),
        }
    }
}
