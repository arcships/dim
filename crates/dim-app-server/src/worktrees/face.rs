use super::{CreatedWorktreeReply, ListParams, ManagedWorktrees, RemoveParams, WorktreeError};
use rutis_bridge::session::{Error, HostDispatch, Reply, Value};
use serde::Deserialize;
use serde_json::json;
use std::{path::PathBuf, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Create {
    source: PathBuf,
    base: Option<String>,
    branch: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Prune {
    older_than_ms: Option<u64>,
    keep: Option<usize>,
    force: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", tag = "method", content = "request")]
enum Operation {
    Create(Create),
    Discard(CreatedWorktreeReply),
    List(ListParams),
    PruneSelection(Prune),
    Remove(RemoveParams),
}

fn failure(error: WorktreeError) -> Error {
    rutis_bridge::exports::failure(
        "WorktreeError",
        error.code(),
        error.message().to_owned(),
        None,
    )
}

impl HostDispatch for ManagedWorktrees {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(
            json!({"create":"async","discard":"async","list":"async","pruneSelection":"async","remove":"async"}),
        )
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        let [request] = <[_; 1]>::try_from(args.list()?).map_err(|_| {
            failure(WorktreeError::Invalid(
                "expected one worktree request".into(),
            ))
        })?;
        let operation: Operation =
            serde_json::from_value(json!({"method":method,"request":request.json()?}))
                .map_err(|error| failure(WorktreeError::Invalid(error.to_string())))?;
        let owner = self.clone();
        let cancel = CancellationToken::new();
        let guard = cancel.clone().drop_guard();
        Ok(Value::cancellable_future(async move {
            let _guard = guard;
            tokio::spawn(async move {
                match operation {
                    Operation::Create(p) => {
                        let created = owner
                            .create(&p.source, p.base.as_deref(), p.branch.as_deref(), &cancel)
                            .await
                            .map_err(failure)?;
                        rutis_bridge::session::arg(&created.reply())
                    }
                    Operation::Discard(created) => {
                        let root = PathBuf::from(&created.root);
                        let managed = super::canonical(owner.root.clone())
                            .await
                            .map_err(failure)?;
                        if !super::valid_task_id(&created.task_id)
                            || root.parent() != Some(managed.join(&created.task_id).as_path())
                        {
                            return Err(failure(WorktreeError::Invalid(
                                "invalid managed worktree directory".into(),
                            )));
                        }
                        super::check_recorded_root(&root, &managed)
                            .await
                            .map_err(failure)?;
                        owner
                            .rollback_add(
                                std::path::Path::new(&created.repository),
                                &root,
                                &created.branch,
                            )
                            .await;
                        Ok(Value::Undefined)
                    }
                    Operation::List(_) => {
                        rutis_bridge::session::arg(&owner.list(&cancel).await.map_err(failure)?)
                    }
                    Operation::PruneSelection(p) => {
                        if p.older_than_ms.is_some() == p.keep.is_some() {
                            return Err(failure(WorktreeError::Invalid(
                                "exactly one of olderThanMs or keep is required".into(),
                            )));
                        }
                        rutis_bridge::session::arg(
                            &owner
                                .prune_selection(
                                    p.older_than_ms.map(Duration::from_millis),
                                    p.keep,
                                    p.force,
                                    &cancel,
                                )
                                .await
                                .map_err(failure)?,
                        )
                    }
                    Operation::Remove(p) => rutis_bridge::session::arg(
                        &owner
                            .remove(&p.task_id, p.force, &cancel)
                            .await
                            .map_err(failure)?,
                    ),
                }
            })
            .await
            .map_err(|error| failure(WorktreeError::Store(error.to_string())))?
        }))
    }
}
