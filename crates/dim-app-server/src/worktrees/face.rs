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
            if let Operation::Create(request) = operation {
                let created = owner
                    .create(
                        &request.source,
                        request.base.as_deref(),
                        request.branch.as_deref(),
                        &cancel,
                    )
                    .await
                    .map_err(failure)?;
                return rutis_bridge::session::arg(&created.reply());
            }
            tokio::spawn(async move {
                match operation {
                    Operation::Create(_) => unreachable!(),
                    Operation::Discard(reply) => {
                        owner.discard(&reply).await.map_err(failure)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use dim_agent_runtime::{AgentBuilder, AgentConfig};
    use std::{process::Command, sync::Arc};

    #[tokio::test]
    async fn abandoning_a_completed_checkout_before_handoff_removes_its_tree_and_branch() {
        let directory = tempfile::tempdir().unwrap();
        let repo = directory.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .current_dir(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ]);
        let mut builder = AgentBuilder::new(AgentConfig::default());
        dim_agent_capabilities::defaults::install(&mut builder);
        dim_agent_capabilities::defaults::mock_model_hosts(&mut builder);
        dim_agent_capabilities::defaults::tool_host(
            &mut builder,
            Arc::new(dim_agent_capabilities::adapters::MockTools),
        );
        let server = crate::serve(
            builder,
            crate::ServerConfig {
                database_path: Some(directory.path().join("store.sqlite")),
                product_home: Some(directory.path().into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let owner = server.root.get::<ManagedWorktrees>().unwrap();
        let cancel = CancellationToken::new();
        let mut handoff = Box::pin(owner.create(&repo, None, Some("abandoned"), &cancel));
        assert!(futures_util::poll!(&mut handoff).is_pending());
        let root = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(root) = owner
                    .preparations
                    .lock()
                    .unwrap()
                    .values()
                    .next()
                    .map(|p| p.prepared.directory.root.clone())
                {
                    break root;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(root.join(".git").is_file());
        drop(handoff);
        owner.preparing.close();
        tokio::time::timeout(Duration::from_secs(10), owner.preparing.wait())
            .await
            .unwrap();
        assert!(!root.exists());
        assert!(git(&["branch", "--list", "abandoned"]).is_empty());
        server.shutdown();
        server.join().await.unwrap();
    }
}
