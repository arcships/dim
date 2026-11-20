use super::HistoryRenderer;
use crate::{port_faces, timeline_tree::TimelineTree};
use dim_agent_runtime::{ContextHistory, Names};
use dim_agent_types::AgentEvent;
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
            let host = HistoryFace {
                renderer: ctx.require_at(&self.renderer)?,
                tree: ctx.require_at(&self.tree)?,
            };
            ctx.provide_at(
                rutis_bridge::session::host_key_in(
                    NAME,
                    self.renderer
                        .erase()
                        .instance_id()
                        .expect("Session renderer"),
                ),
                Arc::new(host) as Arc<dyn HostDispatch>,
            )?;
            Ok(Effect::Done)
        })
    }
}

#[derive(Clone)]
struct HistoryFace {
    renderer: Arc<HistoryRenderer>,
    tree: Arc<TimelineTree>,
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
                    let timeline = owner
                        .tree
                        .history_timeline(&branch_id)
                        .map_err(|e| e.to_string())?;
                    let (context, history) = owner
                        .renderer
                        .rendered_context(&timeline, through)
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
                    owner.renderer.snapshot(&context, &retain_from, &anchor)
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
                    let timeline = owner
                        .tree
                        .history_timeline(&branch_id)
                        .map_err(|e| e.to_string())?;
                    owner
                        .renderer
                        .stream_checkpoint(&timeline, through, &retain_from, batch)
                        .await
                        .map_err(|e| e.to_string())
                })
            }
            _ => Err(port_faces::unknown_method(NAME, method)),
        }
    }
}
