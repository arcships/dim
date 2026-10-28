use super::*;
use crate::tree_protocol as wire;
use dim_agent_runtime::BranchOptions;
use dim_agent_types::BranchSource;

impl Connection {
    pub(super) async fn submit(&self, rpc: &RpcService, params: Submit) -> Result<Reply, RpcError> {
        if params.text.is_some() && params.user.is_some() {
            return Err(RpcError::invalid("text and user are mutually exclusive"));
        }
        let session = self.session(rpc, &params.session_id)?;
        let branch = params.branch_id.as_deref().unwrap_or("main");
        let publication = session.publication(branch).map_err(RpcError::from)?;
        let user = params.user.or_else(|| {
            params.text.map(|text| UserInput {
                content: vec![ContentBlock::Text { text }],
            })
        });
        let event = session
            .input(
                InputMessageData {
                    submission_id: params.submission_id,
                    user,
                    mode: params.mode,
                    ..InputMessageData::default()
                },
                branch,
            )
            .await
            .map_err(RpcError::from)?;
        if self.protocol_version() == 3 {
            let mut published = publication.watch();
            tokio::select! {
                biased;
                _ = self.closed.cancelled() => {},
                _ = rpc.unloading.cancelled() => {},
                result = published.wait_for(|&(sequence, closed)| sequence >= event.sequence || closed) => {
                    let _ = result;
                }
            }
            let failure = publication.failure();
            if publication.sequence() < event.sequence {
                if let Some(error) = failure {
                    return Err(RpcError::from(error));
                }
                return Err(RpcError::app(
                    -32000,
                    "submission_unknown",
                    "Input was queued but publication could not be confirmed",
                ));
            }
        }
        Ok(Reply::Submit(SubmitResult { event }))
    }

    pub(super) async fn input_lookup(
        &self,
        rpc: &RpcService,
        params: InputLookup,
    ) -> Result<Reply, RpcError> {
        let session = self.session(rpc, &params.session_id)?;
        let branch = params.branch_id.as_deref().expect("validated branchId");
        Ok(Reply::InputLookup(
            match session
                .lookup(branch, &params.submission_id)
                .await
                .map_err(RpcError::from)?
            {
                Some(event) => InputLookupResult::Accepted { event },
                None => InputLookupResult::Unknown,
            },
        ))
    }

    pub(super) async fn history(
        &self,
        rpc: &RpcService,
        params: History,
    ) -> Result<Reply, RpcError> {
        let session = self.session(rpc, &params.session_id)?;
        let branch = params.branch_id.as_deref().expect("validated branchId");
        let mut dispatched = session.dispatch_watch(branch).map_err(RpcError::from)?;
        let through = match params.through {
            Some(through) => {
                if through > dispatched.borrow().sequence {
                    return Err(RpcError::invalid("Future dispatched history position"));
                }
                through
            }
            None => session.sequence(branch).map_err(RpcError::from)?,
        };
        if params.after > through {
            return Err(RpcError::invalid("History cursor exceeds fixed position"));
        }
        loop {
            let state = dispatched.borrow().clone();
            if let Some(error) = state.failure {
                return Err(RpcError::from(error));
            }
            if state.sequence >= through {
                break;
            }
            tokio::select! {
                biased;
                _ = self.closed.cancelled() => return Err(RpcError::wire(-32000, "Connection closed")),
                _ = rpc.unloading.cancelled() => return Err(RpcError::reloading()),
                changed = dispatched.changed() => changed.map_err(|_| RpcError::closed())?,
            }
        }
        let events = session
            .local_page(branch, params.after, params.limit, through)
            .await
            .map_err(|error| match error {
                AgentError::InvalidInput(message) => RpcError::invalid(message),
                error => RpcError::from(error),
            })?;
        let next_after = events.last().map_or(params.after, |event| event.sequence);
        Ok(Reply::History(wire::HistoryResult {
            events: events
                .into_iter()
                .filter_map(|event| wire::ProductEvent::new(event, self.protocol_version()))
                .collect(),
            through,
            next_after,
            done: next_after == through,
        }))
    }

    pub(super) async fn create_branch(
        &self,
        rpc: &RpcService,
        params: BranchCreate,
    ) -> Result<Reply, RpcError> {
        let session = self.session(rpc, &params.session_id)?;
        session
            .state(&params.parent_branch_id)
            .map_err(RpcError::from)?;
        if params.prompt.trim().is_empty() {
            return Err(RpcError::invalid("Branch prompt must not be empty"));
        }
        if !session
            .config()
            .tools
            .as_ref()
            .is_none_or(|tools| tools.iter().any(|name| name == "branch"))
        {
            return Err(RpcError::invalid("Branch tool is not enabled"));
        }
        let prefix = session
            .context(&params.parent_branch_id, params.at)
            .await
            .map_err(RpcError::from)?;
        dim_agent_capabilities::history::validate_context_boundary(&prefix)
            .map_err(RpcError::invalid)?;
        let id = session
            .branch(
                params.parent_branch_id,
                params.at,
                BranchOptions {
                    prompt: params.prompt,
                    source: BranchSource::Host {
                        operation_id: uuid::Uuid::new_v4().to_string(),
                    },
                },
            )
            .await
            .map_err(RpcError::from)?;
        let branch = session
            .branches()
            .into_iter()
            .find(|branch| branch.id == id)
            .expect("successful creation is published");
        Ok(Reply::BranchCreate(wire::CreateResult {
            branch: branch.into(),
        }))
    }
}
