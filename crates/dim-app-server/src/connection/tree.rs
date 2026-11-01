use super::*;
use crate::tree_protocol as wire;
use dim_agent_capabilities::input_queue::{
    CheckInterrupted, SubmitCheckReply, SubmitCheckRequest, SubmitChecks, SubmitEntry,
    append_blocks,
};
use dim_agent_runtime::BranchOptions;
use dim_agent_types::{BranchSource, InputMode};

impl Connection {
    /// Runs the Session's submit checks before writing operator input (Hooks
    /// A.6.2); `Err` is the error the call answers with.
    async fn submit_check(
        &self,
        rpc: &RpcService,
        session: &Session,
        request: SubmitCheckRequest,
    ) -> Result<SubmitCheckReply, RpcError> {
        let checks = session
            .session_service::<SubmitChecks>()
            .map_err(RpcError::from)?;
        checks
            .check(&request, &rpc.unloading.child_token())
            .await
            .map_err(|CheckInterrupted| RpcError::reloading())
    }

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
        let mut data = InputMessageData {
            submission_id: params.submission_id,
            user,
            mode: params.mode,
            ..InputMessageData::default()
        };
        match self
            .submit_check(
                rpc,
                &session,
                SubmitCheckRequest {
                    entry: SubmitEntry::Submit,
                    branch_id: branch.to_owned(),
                    user: data.user.clone(),
                    mode: data.effective_mode(),
                },
            )
            .await?
        {
            SubmitCheckReply::Block { message } => {
                return Err(RpcError::app(-32000, "input_blocked", message));
            }
            SubmitCheckReply::Proceed { blocks } => append_blocks(&mut data.user, blocks),
        }
        let event = session.input(data, branch).await.map_err(RpcError::from)?;
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
        if params.latest_turn && params.after.is_some() {
            return Err(RpcError::invalid(
                "latestTurn and after are mutually exclusive",
            ));
        }
        if params.before.is_some() && params.after.is_some() {
            return Err(RpcError::invalid("before and after are mutually exclusive"));
        }
        if params.before == Some(0) {
            return Err(RpcError::invalid("before must be at least 1"));
        }
        if params.latest_turn && params.limit.is_some_and(|limit| limit > HISTORY_TURN_LIMIT) {
            return Err(RpcError::invalid(format!(
                "latestTurn limit must be between 1 and {HISTORY_TURN_LIMIT}"
            )));
        }
        let limit = if params.latest_turn {
            params.limit.unwrap_or(HISTORY_TURN_LIMIT)
        } else {
            params.limit.unwrap_or(HISTORY_PAGE_LIMIT)
        };
        let window = params.latest_turn || params.before.is_some();
        if window && !rpc.index.has_session_store() {
            return Err(RpcError::invalid(
                "history windows require the native Session store",
            ));
        }
        let session = self.session(rpc, &params.session_id)?;
        let branch = params.branch_id.as_deref().expect("validated branchId");
        let mut dispatched = match session.dispatch_watch(branch) {
            Ok(dispatched) => Some(dispatched),
            Err(AgentError::BranchEnded(_)) => None,
            Err(error) => return Err(RpcError::from(error)),
        };
        let head = session.sequence(branch).map_err(RpcError::from)?;
        let position = dispatched
            .as_ref()
            .map_or(head, |dispatched| dispatched.borrow().sequence);
        let through = match params.through {
            Some(through) => {
                if through > position {
                    return Err(RpcError::invalid("Future dispatched history position"));
                }
                through
            }
            None => head,
        };
        let after = params.after.unwrap_or(0);
        if after > through {
            return Err(RpcError::invalid("History cursor exceeds fixed position"));
        }
        if params.before.is_some_and(|before| before > through) {
            return Err(RpcError::invalid("History cursor exceeds fixed position"));
        }
        if let Some(dispatched) = &mut dispatched {
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
                    changed = dispatched.changed() => {
                        if changed.is_err() {
                            // The line ended while the call waited: its stored
                            // head is complete and a read of it serves that.
                            if session.ended_head(branch).is_none() {
                                return Err(RpcError::closed());
                            }
                            break;
                        }
                    }
                }
            }
        }
        if window {
            let turn = if params.latest_turn {
                rpc.index
                    .turn_window(&params.session_id, branch, through)
                    .await
                    .map_err(history_error)?
            } else {
                crate::storage::TurnWindow {
                    turn_start: None,
                    window_start: 1,
                }
            };
            let before = params.before.unwrap_or_else(|| through.saturating_add(1));
            let stored = rpc
                .index
                .read_page_before(
                    &params.session_id,
                    branch,
                    before,
                    limit,
                    through,
                    turn.window_start,
                )
                .await
                .map_err(history_error)?;
            return Ok(Reply::History(history_window(
                stored,
                through,
                turn,
                self.protocol_version(),
            )?));
        }
        let events = session
            .local_page(branch, after, limit, through)
            .await
            .map_err(history_error)?;
        let next_after = events.last().map_or(after, |event| event.sequence);
        Ok(Reply::History(wire::HistoryResult {
            events: events
                .into_iter()
                .filter_map(|event| wire::ProductEvent::new(event, self.protocol_version()))
                .collect(),
            through,
            next_after,
            done: next_after == through,
            next_before: None,
            turn_start: None,
        }))
    }

    pub(super) async fn end_branch(
        &self,
        rpc: &RpcService,
        params: BranchEnd,
    ) -> Result<Reply, RpcError> {
        let session = self.session(rpc, &params.session_id)?;
        session
            .end_branch(
                &params.branch_id,
                BranchSource::Host {
                    operation_id: uuid::Uuid::new_v4().to_string(),
                },
            )
            .await
            .map_err(RpcError::from)?;
        Ok(Reply::empty())
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
        // A blocked prompt creates no branch (Hooks A.6.2). A branch's first
        // input is built from the prompt alone, so the checks' contexts are
        // dropped.
        match self
            .submit_check(
                rpc,
                &session,
                SubmitCheckRequest {
                    entry: SubmitEntry::Branch,
                    branch_id: params.parent_branch_id.clone(),
                    user: Some(UserInput {
                        content: vec![ContentBlock::Text {
                            text: params.prompt.clone(),
                        }],
                    }),
                    mode: InputMode::Queue,
                },
            )
            .await?
        {
            SubmitCheckReply::Block { message } => {
                return Err(RpcError::app(-32000, "input_blocked", message));
            }
            SubmitCheckReply::Proceed { blocks } if !blocks.is_empty() => {
                tracing::warn!(
                    contexts = blocks.len(),
                    "submit check contexts dropped: a branch's first input is its prompt"
                );
            }
            SubmitCheckReply::Proceed { .. } => {}
        }
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

/// The default page of a `session.history` read without `latestTurn`.
const HISTORY_PAGE_LIMIT: usize = 256;
/// The default and largest page of a `latestTurn` read.
const HISTORY_TURN_LIMIT: usize = 4096;
/// Encoded `ProductEvent` bytes a `latestTurn` reply aims for. A Timeline
/// batch is the smallest unit the window can trim, so a batch above this is
/// returned whole rather than split.
const HISTORY_TURN_BYTES: usize = 4 * 1024 * 1024;
/// The largest reply a `latestTurn` page may produce: below the transport's
/// 16 MiB frame, with room for the envelope. A batch above it cannot be sent
/// and the read fails `history_too_large`.
const HISTORY_FRAME_BYTES: usize = crate::outbound::MAX_FRAME_BYTES - 64 * 1024;

fn history_error(error: AgentError) -> RpcError {
    match error {
        AgentError::InvalidInput(message) => RpcError::invalid(message),
        error => RpcError::from(error),
    }
}

/// Builds a newest-first window reply: the newest whole batches within the
/// event and byte limits. The newest batch is always kept; one above the byte
/// limit fails `history_too_large` rather than produce a frame the transport
/// could drop.
fn history_window(
    stored: Vec<dim_agent_runtime::StoredEvent>,
    through: u64,
    turn: crate::storage::TurnWindow,
    version: u32,
) -> Result<wire::HistoryResult, RpcError> {
    let mut items = Vec::with_capacity(stored.len());
    for record in stored {
        let sequence = record.event.sequence;
        let batch_end = record.batch_end;
        let visible = wire::ProductEvent::new(record.event, version);
        let bytes = match &visible {
            Some(product) => serde_json::to_vec(product)
                .map_err(|error| RpcError::wire(-32603, error.to_string()))?
                .len(),
            None => 0,
        };
        items.push((sequence, batch_end, bytes, visible));
    }
    let mut keep_from = items.len();
    let mut end = items.len();
    let mut total = 0;
    let mut newest = true;
    while end > 0 {
        let batch_end = items[end - 1].1;
        let mut begin = end;
        while begin > 0 && items[begin - 1].1 == batch_end {
            begin -= 1;
        }
        let bytes: usize = items[begin..end].iter().map(|item| item.2).sum();
        if newest {
            if bytes > HISTORY_FRAME_BYTES {
                return Err(RpcError::app(
                    -32000,
                    "history_too_large",
                    format!(
                        "The newest history batch is {bytes} bytes; a reply frame allows {HISTORY_FRAME_BYTES}"
                    ),
                ));
            }
            total = bytes;
            keep_from = begin;
            newest = false;
        } else if total + bytes <= HISTORY_TURN_BYTES {
            total += bytes;
            keep_from = begin;
        } else {
            break;
        }
        end = begin;
    }
    let kept = items.split_off(keep_from);
    let next_before = kept.first().map(|item| item.0);
    let next_after = kept.last().map_or(through, |item| item.0);
    let done = next_before.is_none_or(|sequence| sequence <= turn.window_start);
    Ok(wire::HistoryResult {
        events: kept.into_iter().filter_map(|item| item.3).collect(),
        through,
        next_after,
        done,
        next_before,
        turn_start: turn.turn_start,
    })
}
