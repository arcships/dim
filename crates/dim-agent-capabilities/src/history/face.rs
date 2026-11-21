use super::{HistoryEntry, HistoryRenderer, StreamedContext};
use crate::{port_faces, timeline_tree::TimelineTree};
use dim_agent_runtime::ContextHistory;
use dim_agent_types::{AgentEvent, ProjectionCheckpoint};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis_bridge::session::{HostDispatch, Reply, Value};
use serde::Deserialize;
use std::sync::Arc;

const NAME: &str = "historyRenderer";

pub(crate) type SessionHistoryRenderer = HistoryRenderer<Arc<TimelineTree>>;

impl HistoryRenderer {
    pub fn feature(self) -> dim_agent_runtime::Feature {
        dim_agent_runtime::Feature::new("HistoryRenderer").session(
            "HistoryRenderer",
            move |names| {
                Ok(Box::new(HistoryRendererPlugin {
                    renderer: self.clone(),
                    tree: names.key()?,
                    service: names.key()?,
                }))
            },
        )
    }
}

struct HistoryRendererPlugin {
    renderer: HistoryRenderer,
    tree: Key<TimelineTree>,
    service: Key<SessionHistoryRenderer>,
}

impl DynPlugin for HistoryRendererPlugin {
    fn name(&self) -> &str {
        "HistoryRenderer"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tree.erase()]
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let renderer = Arc::new(HistoryRenderer {
                tree: ctx.require_at(&self.tree)?,
                render: self.renderer.render,
                checkpoint: self.renderer.checkpoint,
                checkpoint_interpreter: self.renderer.checkpoint_interpreter,
                encode_checkpoint: self.renderer.encode_checkpoint,
                decode_checkpoint: self.renderer.decode_checkpoint,
            });
            let instance = self
                .service
                .erase()
                .instance_id()
                .expect("Session renderer");
            ctx.provide_at(self.service.clone(), renderer.clone())?;
            ctx.provide_at(
                rutis_bridge::session::host_key_in(NAME, instance),
                renderer as Arc<dyn HostDispatch>,
            )?;
            Ok(Effect::Done)
        })
    }
}

impl SessionHistoryRenderer {
    pub(crate) async fn branch_rendered_context(
        &self,
        branch: &str,
        through: u64,
    ) -> dim_agent_runtime::Result<(ContextHistory, Vec<HistoryEntry>)> {
        self.rendered_context(&self.tree.history_timeline(branch)?, through)
            .await
    }
    pub(crate) async fn branch_rendered_stream(
        &self,
        branch: &str,
        through: u64,
        batch: u64,
    ) -> dim_agent_runtime::Result<StreamedContext> {
        self.rendered_stream(&self.tree.history_timeline(branch)?, through, batch)
            .await
    }
    pub(crate) async fn branch_stream_checkpoint(
        &self,
        branch: &str,
        through: u64,
        retain_from: &str,
        batch: u64,
    ) -> dim_agent_runtime::Result<ProjectionCheckpoint> {
        self.stream_checkpoint(
            &self.tree.history_timeline(branch)?,
            through,
            retain_from,
            batch,
        )
        .await
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

impl HostDispatch for SessionHistoryRenderer {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(
            serde_json::json!({"context":"async","renderedContext":"async","renderedStream":"async","snapshotFromCheckpoint":"async","streamCheckpoint":"async","snapshot":"async"}),
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
                        .branch_rendered_context(&branch_id, through)
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok::<_, String>(Rendered { context, history })
                })
            }
            "renderedStream" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                struct Stream {
                    branch_id: String,
                    through: u64,
                    batch: u64,
                }
                let Stream {
                    branch_id,
                    through,
                    batch,
                } = port_faces::request(NAME, args)?;
                port_faces::serving(NAME, async move {
                    owner
                        .branch_rendered_stream(&branch_id, through, batch)
                        .await
                        .map_err(|error| error.to_string())
                })
            }
            "snapshotFromCheckpoint" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Save {
                    checkpoint: ProjectionCheckpoint,
                    anchor: AgentEvent,
                }
                let Save { checkpoint, anchor } = port_faces::request(NAME, args)?;
                port_faces::serving(NAME, async move {
                    owner.snapshot_from_checkpoint(&checkpoint, &anchor)
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
                        .branch_stream_checkpoint(&branch_id, through, &retain_from, batch)
                        .await
                        .map_err(|e| e.to_string())
                })
            }
            _ => Err(port_faces::unknown_method(NAME, method)),
        }
    }
}
