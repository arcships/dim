use super::*;
use crate::tree_protocol as wire;
use dim_agent_capabilities::input_queue::{
    CheckInterrupted, SubmitCheckRequest, SubmitChecks, SubmitDecision, SubmitEntry, append_blocks,
};
use dim_agent_capabilities::timeline_tree::TreeCommands;
use dim_agent_runtime::{
    BranchOptions, InputCommit, QueryComparison, QueryFilter, QueryOperand, QueryOrder,
    QueryOrderKey, QueryOutput, QueryPage, QueryPick, QuerySelect, QuerySpec, QueryValue,
    TimelineQuery,
};
use dim_agent_types::{BranchSource, InputBlock, InputMode, TimelinePosition};

impl Connection {
    /// Runs the Session's submit checks before writing operator input (Hooks
    /// A.6.2); `Err` is the error the call answers with.
    async fn submit_check(
        &self,
        rpc: &RpcService,
        session: &Session,
        request: SubmitCheckRequest,
    ) -> Result<SubmitDecision, RpcError> {
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
        // The checks' contexts are appended to the operator's own content, so
        // an input that carries none must be refused before the checks run:
        // otherwise a check's context would be the only content, and the input
        // it made valid would be written and titled after all.
        data.validate()
            .map_err(|reason| RpcError::from(AgentError::InvalidInput(reason.into())))?;
        let checked = self
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
            .await?;
        // A warning is the caller's, never the model's: it names work the
        // checks did not do, and the input is written without it. Version 1
        // and 2 encodings are frozen, so they carry no warnings.
        let checked = match checked {
            SubmitDecision::Block { message } => {
                return Err(RpcError::app(-32000, "input_blocked", message));
            }
            SubmitDecision::Proceed { blocks, warnings } => {
                append_blocks(&mut data.user, blocks);
                warnings
            }
        };
        let (event, commit) = session
            .input_commit(data, branch)
            .await
            .map_err(RpcError::from)?;
        // A warning describes the write of this call. A resubmission of a
        // `submissionId` the branch already holds wrote nothing, so it answers
        // the first input's receipt with none of this attempt's warnings.
        let warnings = match commit {
            InputCommit::Written => checked,
            InputCommit::Existing => Vec::new(),
        }
        .into_iter()
        .filter(|_| self.protocol_version() >= 3)
        .collect();
        if self.protocol_version() >= 3 {
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
        Ok(Reply::Submit(SubmitResult { event, warnings }))
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
        display: bool,
        tail: bool,
    ) -> Result<Reply, RpcError> {
        if tail && (params.latest_turn || params.after.is_some()) {
            return Err(RpcError::invalid(
                "tail cannot be combined with latestTurn or after",
            ));
        }
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
        if (params.latest_turn || tail)
            && params.limit.is_some_and(|limit| limit > HISTORY_TURN_LIMIT)
        {
            return Err(RpcError::invalid(format!(
                "latestTurn limit must be between 1 and {HISTORY_TURN_LIMIT}"
            )));
        }
        let limit = if params.latest_turn || tail {
            params.limit.unwrap_or(HISTORY_TURN_LIMIT)
        } else {
            params.limit.unwrap_or(HISTORY_PAGE_LIMIT)
        };
        let window = tail || params.latest_turn || params.before.is_some();
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
            let tree = session
                .session_service::<TreeCommands>()
                .map_err(history_error)?;
            let turn = if params.latest_turn {
                turn_window(&tree, branch, through).await?
            } else {
                crate::storage::TurnWindow {
                    turn_start: None,
                    window_start: 1,
                }
            };
            let before = params.before.unwrap_or_else(|| through.saturating_add(1));
            let read = if session.ended_head(branch).is_some() {
                HistoryRead::Ended(&tree, self.protocol_version())
            } else {
                HistoryRead::Open(&session, self.protocol_version())
            };
            let reply = if tail {
                display_tail(&read, branch, before, limit, through).await
            } else {
                window_at(&read, branch, before, limit, through, turn, display).await
            };
            // The owner is chosen before the read: a line that ends in between
            // answers `BranchEnded`, and the tree serves the same request at its
            // fixed head.
            let reply = match reply {
                Err(error)
                    if error
                        .data
                        .as_ref()
                        .is_some_and(|data| data.app_error == "branch_ended") =>
                {
                    let ended = HistoryRead::Ended(&tree, self.protocol_version());
                    if tail {
                        display_tail(&ended, branch, before, limit, through).await
                    } else {
                        window_at(&ended, branch, before, limit, through, turn, display).await
                    }
                }
                other => other,
            };
            return reply.map(|page| {
                if display {
                    Reply::HistoryDisplay(page.into())
                } else {
                    Reply::History(page)
                }
            });
        }
        let tree = session
            .session_service::<TreeCommands>()
            .map_err(history_error)?;
        let read = if session.ended_head(branch).is_some() {
            HistoryRead::Ended(&tree, self.protocol_version())
        } else {
            HistoryRead::Open(&session, self.protocol_version())
        };
        let reply = forward_page(&read, branch, after, limit, through, display).await;
        let reply = match reply {
            Err(error)
                if error
                    .data
                    .as_ref()
                    .is_some_and(|data| data.app_error == "branch_ended") =>
            {
                forward_page(
                    &HistoryRead::Ended(&tree, self.protocol_version()),
                    branch,
                    after,
                    limit,
                    through,
                    display,
                )
                .await
            }
            other => other,
        };
        reply.map(|page| {
            if display {
                Reply::HistoryDisplay(page.into())
            } else {
                Reply::History(page)
            }
        })
    }

    pub(super) async fn history_detail(
        &self,
        rpc: &RpcService,
        params: HistoryDetail,
    ) -> Result<Reply, RpcError> {
        if params.event_id.is_empty() || params.sequence == 0 {
            return Err(RpcError::invalid(
                "A detail requires an event identity and positive sequence",
            ));
        }
        let session = self.session(rpc, &params.session_id)?;
        let branch = params.branch_id.as_deref().expect("validated branchId");
        if params.sequence > session.sequence(branch).map_err(history_error)? {
            return Err(RpcError::invalid("Future detail position"));
        }
        let query = TimelineQuery {
            scope: TimelineQueryScope::Branch,
            from: Some(dim_agent_types::TimelinePosition {
                branch_id: branch.into(),
                sequence: params.sequence,
            }),
            through: Some(params.sequence),
            include_publishing: false,
            query: QuerySpec {
                filter: Some(QueryFilter::And {
                    conditions: vec![
                        completed_compaction(),
                        QueryFilter::In {
                            value: QueryOperand::Field { path: "id".into() },
                            values: vec![params.event_id.clone().into()],
                        },
                    ],
                }),
                select: QuerySelect::Fields {
                    fields: vec!["data".into()],
                },
                group_by: vec![],
                pick: None,
                order_by: vec![],
            },
            page: Some(QueryPage {
                limit: std::num::NonZeroU64::MIN,
                after: None,
            }),
        };
        let reply = match session.query(branch, query.clone()).await {
            Err(AgentError::BranchEnded(_)) => {
                session
                    .session_service::<TreeCommands>()
                    .map_err(history_error)?
                    .query(branch.to_owned(), query)
                    .await
            }
            other => other,
        }
        .map_err(history_error)?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("detail selects data")
        };
        let Some(row) = rows.into_iter().next() else {
            return Err(RpcError::app(
                -32000,
                "detail_not_found",
                "The completed compaction is not visible at this position",
            ));
        };
        let [QueryValue::Value { value }]: [_; 1] =
            row.values.try_into().expect("compaction detail data")
        else {
            unreachable!("compaction data is present")
        };
        let detail = dim_agent_types::CompactionDetail {
            event_id: params.event_id,
            sequence: params.sequence,
            data: serde_json::from_value(value)
                .map_err(|error| RpcError::wire(-32603, error.to_string()))?,
        };
        let encoded = serde_json::value::to_raw_value(&detail)
            .map_err(|error| RpcError::wire(-32603, error.to_string()))?;
        if encoded.get().len() > REPLY_FRAME_BYTES {
            return Err(RpcError::app(
                -32000,
                "detail_too_large",
                "The compaction exceeds the display budget; export the Timeline to read the complete fact",
            ));
        }
        Ok(Reply::HistoryDetail(encoded))
    }

    /// The branch's live keyed blocks at `through` (keyed-blocks §2.2): the
    /// newest live block of each key on the branch's own segment, or the
    /// published head when `through` is absent. A position past the head is
    /// invalid input, not a clamp: the client's overlay was built from a
    /// different view of the branch. A serialized reply above
    /// `REPLY_FRAME_BYTES` fails `keys_too_large` rather than produce a frame
    /// the transport would drop.
    pub(super) async fn keys(&self, rpc: &RpcService, params: Keys) -> Result<Reply, RpcError> {
        let session = self.session(rpc, &params.session_id)?;
        let branch = params.branch_id.as_deref().expect("validated branchId");
        let head = session.sequence(branch).map_err(RpcError::from)?;
        let through = match params.through {
            Some(through) if through > head => {
                return Err(RpcError::invalid("Future keyed-block position"));
            }
            Some(through) => through,
            None => head,
        };
        let entries = if session.ended_head(branch).is_some() {
            let tree = session
                .session_service::<TreeCommands>()
                .map_err(history_error)?;
            scan_keys(
                &HistoryRead::Ended(&tree, self.protocol_version()),
                branch,
                through,
            )
            .await?
        } else {
            match scan_keys(
                &HistoryRead::Open(&session, self.protocol_version()),
                branch,
                through,
            )
            .await
            {
                // The line ended while the scan ran: the tree serves the same
                // read at its fixed head (ADR 021 D2).
                Err(error)
                    if error
                        .data
                        .as_ref()
                        .is_some_and(|data| data.app_error == "branch_ended") =>
                {
                    let tree = session
                        .session_service::<TreeCommands>()
                        .map_err(history_error)?;
                    scan_keys(
                        &HistoryRead::Ended(&tree, self.protocol_version()),
                        branch,
                        through,
                    )
                    .await?
                }
                other => other?,
            }
        };
        let result = wire::KeysResult { through, entries };
        let bytes = serde_json::to_vec(&result)
            .map_err(|error| RpcError::wire(-32603, error.to_string()))?
            .len();
        if bytes > REPLY_FRAME_BYTES {
            return Err(keys_too_large(bytes));
        }
        Ok(Reply::Keys(result))
    }

    pub(super) async fn branches(
        &self,
        rpc: &RpcService,
        params: Branches,
    ) -> Result<Reply, RpcError> {
        let session = self.session(rpc, &params.session_id)?;
        Ok(Reply::Branches(wire::BranchesResult {
            branches: session.directory().into_iter().map(Into::into).collect(),
        }))
    }

    pub(super) async fn branch_get(
        &self,
        rpc: &RpcService,
        params: BranchRequest,
    ) -> Result<Reply, RpcError> {
        let session = self.session(rpc, &params.session_id)?;
        let tree = session
            .session_service::<TreeCommands>()
            .map_err(RpcError::from)?;
        let read = tree
            .display(params.branch_id)
            .await
            .map_err(RpcError::from)?;
        let mut entry = wire::BranchDirectoryView::from(read.entry);
        entry.purpose = read.purpose;
        entry.cut_messages = read.cut_messages;
        entry.end = read.end;
        Ok(Reply::BranchGet(entry))
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
        let boundary = dim_agent_capabilities::history::validate_context_boundary_in(
            &session,
            &params.parent_branch_id,
            params.at,
        )
        .await
        .map_err(RpcError::from)?;
        boundary.map_err(RpcError::invalid)?;
        // A blocked prompt creates no branch (Hooks A.6.2). A branch's first
        // input is built from the prompt alone, so the checks' contexts are
        // dropped and their warnings ride on the reply.
        let checked = self
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
            .await?;
        let warnings = match checked {
            SubmitDecision::Block { message } => {
                return Err(RpcError::app(-32000, "input_blocked", message));
            }
            SubmitDecision::Proceed { blocks, warnings } => {
                if !blocks.is_empty() {
                    tracing::warn!(
                        contexts = blocks.len(),
                        "submit check contexts dropped: a branch's first input is its prompt"
                    );
                }
                // `session.branch_create` is served from protocol v2 on
                // (`connection.rs`'s `validate_call`), and the v2 encoding is
                // frozen, so only v3 and later carry the warnings.
                warnings
                    .into_iter()
                    .filter(|_| self.protocol_version() >= 3)
                    .collect()
            }
        };
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
            warnings,
        }))
    }
}

/// The default page of a `session.history` read without `latestTurn`.
const HISTORY_PAGE_LIMIT: usize = 256;
/// The default and largest page of a `latestTurn` read.
const HISTORY_TURN_LIMIT: usize = 4096;
/// Encoded `ProductEvent` bytes a history reply aims for, in a `latestTurn`
/// window and in forward paging alike. A Timeline batch is the smallest unit a
/// page can trim, so a batch above this is returned whole rather than split.
const HISTORY_TURN_BYTES: usize = 4 * 1024 * 1024;
/// The largest reply a built-in read may produce: below the transport's
/// 16 MiB frame, with room for the envelope. The first batch of any history
/// page above it fails `history_too_large`; a `session.keys` reply above it
/// fails `keys_too_large`.
const REPLY_FRAME_BYTES: usize = crate::outbound::MAX_FRAME_BYTES - 64 * 1024;
/// Facts one page of the `session.keys` scan keeps.
const KEYS_PAGE: u64 = 256;
/// The most live keyed blocks one `session.keys` reply carries. The key is
/// writer-chosen and unchecked (session-access A.2), so the entry count is
/// bounded here rather than by the producers' key sets; a reply above it
/// fails `too_many_keys` instead of truncating live state.
const KEYS_ENTRY_LIMIT: usize = 1024;

fn history_error(error: AgentError) -> RpcError {
    match error {
        AgentError::InvalidInput(message) => RpcError::invalid(message),
        error => RpcError::from(error),
    }
}

fn too_many_keys() -> RpcError {
    RpcError::app(
        -32000,
        "too_many_keys",
        format!(
            "The branch has more than {KEYS_ENTRY_LIMIT} live keyed blocks; the read refuses to truncate live state"
        ),
    )
}

fn keys_too_large(bytes: usize) -> RpcError {
    RpcError::app(
        -32000,
        "keys_too_large",
        format!("The reply is {bytes} bytes; a reply frame allows {REPLY_FRAME_BYTES}"),
    )
}

/// The newest turn at `through` and the first sequence a window over it
/// includes (session-history-window §4.2): the turn's start, and the messages
/// its first request consumed or, when it has yet to take one, the newest
/// message at or below the start.
async fn turn_window(
    tree: &TreeCommands,
    branch: &str,
    through: u64,
) -> Result<crate::storage::TurnWindow, RpcError> {
    if through == 0 {
        return Ok(crate::storage::TurnWindow {
            turn_start: None,
            window_start: 1,
        });
    }
    let query = |filter, pick| TimelineQuery {
        scope: TimelineQueryScope::Branch,
        from: None,
        through: Some(through),
        include_publishing: false,
        query: QuerySpec {
            filter: Some(filter),
            select: QuerySelect::Fields {
                fields: vec!["position.sequence".into()],
            },
            group_by: vec![],
            pick: Some(pick),
            order_by: vec![],
        },
        page: Some(QueryPage {
            limit: std::num::NonZeroU64::MIN,
            after: None,
        }),
    };
    let turned = tree
        .query(
            branch.to_owned(),
            query(
                QueryFilter::In {
                    value: QueryOperand::Field {
                        path: "kind".into(),
                    },
                    values: vec!["turn/start".into()],
                },
                QueryPick::Last,
            ),
        )
        .await
        .map_err(history_error)?;
    let QueryOutput::Rows { rows } = turned.result else {
        unreachable!("field selection returns rows")
    };
    let Some(row) = rows.first() else {
        return Ok(crate::storage::TurnWindow {
            turn_start: None,
            window_start: 1,
        });
    };
    let [QueryValue::Value { value: start }] = row.values.as_slice() else {
        unreachable!("turn start has a sequence")
    };
    let turn_start = start.as_u64().expect("event sequence is unsigned");
    let mut first_model = query(
        QueryFilter::In {
            value: QueryOperand::Field {
                path: "kind".into(),
            },
            values: vec!["step/model-start".into()],
        },
        QueryPick::First,
    );
    first_model.from = Some(TimelinePosition {
        branch_id: branch.into(),
        sequence: turn_start + 1,
    });
    let step = tree
        .query(branch.to_owned(), first_model)
        .await
        .map_err(history_error)?;
    let QueryOutput::Rows { rows } = step.result else {
        unreachable!("field selection returns rows")
    };
    let mut window_start = turn_start;
    let mut consumed = false;
    if let Some(row) = rows.first() {
        let [QueryValue::Value { value: first_step }] = row.values.as_slice() else {
            unreachable!("model start has a sequence")
        };
        let mut adopted = query(
            QueryFilter::In {
                value: QueryOperand::Field {
                    path: "kind".into(),
                },
                values: vec!["input/consumed".into()],
            },
            QueryPick::First,
        );
        adopted.from = Some(TimelinePosition {
            branch_id: branch.into(),
            sequence: turn_start + 1,
        });
        adopted.through = Some(first_step.as_u64().expect("model sequence is unsigned") - 1);
        adopted.query.pick = None;
        adopted.query.select = QuerySelect::Fields {
            fields: vec!["data.input_id".into()],
        };
        adopted.page.as_mut().unwrap().limit = std::num::NonZeroU64::new(128).unwrap();
        loop {
            let reply = tree
                .query(branch.to_owned(), adopted.clone())
                .await
                .map_err(history_error)?;
            let QueryOutput::Rows { rows } = reply.result else {
                unreachable!("consumed identities")
            };
            let ids = rows
                .into_iter()
                .map(|row| {
                    let [QueryValue::Value { value }] = <[_; 1]>::try_from(row.values).unwrap()
                    else {
                        unreachable!("input identity")
                    };
                    value
                })
                .collect::<Vec<_>>();
            if !ids.is_empty() {
                let inputs = tree
                    .query(
                        branch.to_owned(),
                        query(
                            QueryFilter::In {
                                value: QueryOperand::Field { path: "id".into() },
                                values: ids,
                            },
                            QueryPick::First,
                        ),
                    )
                    .await
                    .map_err(history_error)?;
                let QueryOutput::Rows { rows } = inputs.result else {
                    unreachable!("input position")
                };
                if let Some(row) = rows.first() {
                    let [QueryValue::Value { value: sequence }] = row.values.as_slice() else {
                        unreachable!("input sequence")
                    };
                    window_start =
                        window_start.min(sequence.as_u64().expect("input sequence is unsigned"));
                    consumed = true;
                }
            }
            let Some(next) = reply.next else { break };
            adopted.page.as_mut().unwrap().after = Some(next);
        }
    }
    if !consumed {
        let newest = tree
            .query(
                branch.to_owned(),
                query(
                    QueryFilter::And {
                        conditions: vec![
                            QueryFilter::In {
                                value: QueryOperand::Field {
                                    path: "kind".into(),
                                },
                                values: vec!["input/message".into()],
                            },
                            QueryFilter::Compare {
                                left: QueryOperand::Field {
                                    path: "position.sequence".into(),
                                },
                                comparison: QueryComparison::Le,
                                right: QueryOperand::Value {
                                    value: turn_start.into(),
                                },
                            },
                        ],
                    },
                    QueryPick::Last,
                ),
            )
            .await
            .map_err(history_error)?;
        let QueryOutput::Rows { rows } = newest.result else {
            unreachable!("field selection returns rows")
        };
        if let Some(row) = rows.first() {
            let [QueryValue::Value { value: sequence }] = row.values.as_slice() else {
                unreachable!("input has a sequence")
            };
            window_start = window_start.min(sequence.as_u64().expect("event sequence is unsigned"));
        }
    }
    Ok(crate::storage::TurnWindow {
        turn_start: Some(turn_start),
        window_start,
    })
}

/// Where a history read's facts and batch edges come from: a live line's own
/// Timeline, through its Session, or an ended line's stored history, through
/// the tree that owns its fixed head (ADR 021 D2).
enum HistoryRead<'a> {
    Open(&'a Session, u32),
    Ended(&'a TreeCommands, u32),
}

impl HistoryRead<'_> {
    async fn query(
        &self,
        branch: &str,
        query: TimelineQuery,
    ) -> Result<dim_agent_runtime::TimelineQueryReply, AgentError> {
        match self {
            Self::Open(session, _) => session.query(branch, query).await,
            Self::Ended(tree, _) => tree.query(branch.to_owned(), query).await,
        }
    }

    async fn batch_end(&self, branch: &str, sequence: u64) -> Result<(), AgentError> {
        match self {
            Self::Open(session, _) => session.batch_end(branch, sequence).await,
            Self::Ended(tree, _) => tree.batch_end(branch.to_owned(), sequence).await,
        }
    }
}

/// The latest live blocks in this branch's own segment, excluding deleted inputs.
/// The opening excludes the inherited prefix and a fork's copied facts.
async fn scan_keys(
    read: &HistoryRead<'_>,
    branch: &str,
    through: u64,
) -> Result<Vec<wire::KeyEntry>, RpcError> {
    let mut found: HashMap<String, (u64, usize, wire::KeyEntry)> = HashMap::new();
    let mut closed: HashSet<String> = HashSet::new();
    let opened = read
        .query(
            branch,
            TimelineQuery {
                scope: TimelineQueryScope::Branch,
                from: None,
                through: Some(through),
                include_publishing: false,
                query: QuerySpec {
                    filter: Some(QueryFilter::In {
                        value: QueryOperand::Field {
                            path: "kind".into(),
                        },
                        values: vec!["branch/start".into(), "session/forked".into()],
                    }),
                    select: QuerySelect::Fields {
                        fields: vec!["position.sequence".into()],
                    },
                    group_by: vec![],
                    pick: Some(QueryPick::Last),
                    order_by: vec![],
                },
                page: Some(QueryPage {
                    limit: std::num::NonZeroU64::MIN,
                    after: None,
                }),
            },
        )
        .await
        .map_err(history_error)?;
    let QueryOutput::Rows { rows } = opened.result else {
        unreachable!("field selection returns rows")
    };
    let from = rows.first().map(|row| {
        let [QueryValue::Value { value: sequence }] = row.values.as_slice() else {
            unreachable!("opening has a sequence")
        };
        dim_agent_types::TimelinePosition {
            branch_id: branch.into(),
            sequence: sequence.as_u64().expect("event sequence is unsigned"),
        }
    });
    let mut after = None;
    loop {
        let page = read
            .query(
                branch,
                TimelineQuery {
                    scope: TimelineQueryScope::Branch,
                    from: from.clone(),
                    through: Some(through),
                    include_publishing: false,
                    query: QuerySpec {
                        filter: Some(QueryFilter::And {
                            conditions: vec![
                                QueryFilter::In {
                                    value: QueryOperand::Field {
                                        path: "kind".into(),
                                    },
                                    values: vec!["input/message".into()],
                                },
                                QueryFilter::Any {
                                    field: "data.blocks".into(),
                                    condition: Box::new(QueryFilter::Present {
                                        value: QueryOperand::Element { path: "key".into() },
                                    }),
                                },
                                QueryFilter::Not {
                                    condition: Box::new(QueryFilter::Exists {
                                        condition: Box::new(QueryFilter::And {
                                            conditions: vec![
                                                QueryFilter::In {
                                                    value: QueryOperand::Field {
                                                        path: "kind".into(),
                                                    },
                                                    values: vec!["input/deleted".into()],
                                                },
                                                QueryFilter::Compare {
                                                    left: QueryOperand::Field {
                                                        path: "data.input_id".into(),
                                                    },
                                                    comparison: QueryComparison::Eq,
                                                    right: QueryOperand::Outer {
                                                        path: "id".into(),
                                                    },
                                                },
                                            ],
                                        }),
                                    }),
                                },
                            ],
                        }),
                        select: QuerySelect::Fields {
                            fields: vec![
                                "id".into(),
                                "position.sequence".into(),
                                "data.blocks".into(),
                            ],
                        },
                        group_by: vec![],
                        pick: None,
                        order_by: vec![QueryOrder {
                            key: QueryOrderKey::Position,
                            direction: TimelineQueryDirection::Descending,
                        }],
                    },
                    page: Some(QueryPage {
                        limit: std::num::NonZeroU64::new(KEYS_PAGE).unwrap(),
                        after,
                    }),
                },
            )
            .await
            .map_err(history_error)?;
        let QueryOutput::Rows { rows } = page.result else {
            unreachable!("field selection returns rows")
        };
        for row in rows {
            let [
                QueryValue::Value {
                    value: serde_json::Value::String(id),
                },
                QueryValue::Value { value: sequence },
                QueryValue::Value { value: blocks },
            ] = row.values.as_slice()
            else {
                unreachable!("keyed input has id, sequence and blocks")
            };
            let sequence = sequence.as_u64().expect("event sequence is unsigned");
            let blocks: Vec<InputBlock> = serde_json::from_value(blocks.clone())
                .expect("input blocks follow their event contract");
            for (index, block) in blocks.iter().enumerate().rev() {
                let InputBlock::Raw {
                    text,
                    key: Some(key),
                    retire,
                } = block
                else {
                    continue;
                };
                if found.contains_key(key) {
                    continue;
                }
                if *retire {
                    closed.insert(key.clone());
                    continue;
                }
                if closed.contains(key) {
                    continue;
                }
                found.insert(
                    key.clone(),
                    (
                        sequence,
                        index,
                        wire::KeyEntry {
                            key: key.clone(),
                            sequence,
                            input_id: id.clone(),
                            text: text.clone(),
                        },
                    ),
                );
                if found.len() > KEYS_ENTRY_LIMIT {
                    return Err(too_many_keys());
                }
            }
        }
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    let mut entries: Vec<_> = found.into_values().collect();
    entries.sort_by_key(|(sequence, index, _)| (*sequence, *index));
    Ok(entries.into_iter().map(|(_, _, entry)| entry).collect())
}

async fn window_at(
    read: &HistoryRead<'_>,
    branch: &str,
    before: u64,
    limit: usize,
    through: u64,
    turn: crate::storage::TurnWindow,
    display: bool,
) -> Result<wire::HistoryResult, RpcError> {
    let events = window_page(
        read,
        branch,
        before,
        limit,
        through,
        turn.window_start,
        display,
    )
    .await?;
    history_window(read, branch, events, through, turn).await
}

async fn display_tail(
    read: &HistoryRead<'_>,
    branch: &str,
    before: u64,
    limit: usize,
    through: u64,
) -> Result<wire::HistoryResult, RpcError> {
    let top = before.saturating_sub(1).min(through);
    let low = top.saturating_sub(limit.saturating_sub(1) as u64).max(1);
    let mut next = top;
    let mut bytes = 0;
    let mut kept = Vec::new();
    'page: while next >= low && next > 0 {
        let start = next.saturating_sub(7).max(low);
        let items = window_items(read, branch, start, next, true, false).await?;
        for item in items.into_iter().rev() {
            if item.bytes > REPLY_FRAME_BYTES {
                return Err(RpcError::app(
                    -32000,
                    "history_too_large",
                    "A display event exceeds the reply frame budget",
                ));
            }
            if bytes > 0 && bytes + item.bytes > HISTORY_TURN_BYTES {
                break 'page;
            }
            bytes += item.bytes;
            kept.push(item);
        }
        next = start - 1;
    }
    kept.reverse();
    let next_before = kept.first().map(|item| item.sequence);
    let next_after = kept.last().map_or(through, |item| item.sequence);
    Ok(wire::HistoryResult {
        events: kept.into_iter().filter_map(|item| item.event).collect(),
        through,
        next_after,
        done: next_before.is_none_or(|sequence| sequence <= 1),
        next_before,
        turn_start: None,
    })
}

/// The newest events below `before` inside `window_start..=through`
/// (session-history-window §4.2), read through the loaded line's query or the
/// tree's stored read. The page ends at the newest batch end at or below
/// `before - 1` and starts at a batch start; `batch_end` settles both ends
/// because a query page may begin and end inside an atomic batch (ADR 020 D4).
async fn window_page(
    read: &HistoryRead<'_>,
    branch: &str,
    before: u64,
    limit: usize,
    through: u64,
    window_start: u64,
    display: bool,
) -> Result<Vec<HistoryItem>, RpcError> {
    if through > 0 {
        read.batch_end(branch, through)
            .await
            .map_err(history_error)?;
    }
    let top = before.saturating_sub(1).min(through);
    if top == 0 || top < window_start || limit == 0 {
        return Ok(Vec::new());
    }
    let Some(end) = batch_end_at_or_below(read, branch, top, window_start).await? else {
        return Ok(Vec::new());
    };
    let low = end
        .saturating_sub(limit.saturating_sub(1) as u64)
        .max(1)
        .max(window_start);
    let low = batch_start(read, branch, low, window_start).await?;
    window_items(read, branch, low, end, display, true).await
}

/// The greatest atomic-batch end at or below `top`, searching no lower than
/// `floor`; `None` when the window holds none.
async fn batch_end_at_or_below(
    read: &HistoryRead<'_>,
    branch: &str,
    top: u64,
    floor: u64,
) -> Result<Option<u64>, RpcError> {
    let mut probe = top;
    while probe >= floor && probe > 0 {
        match read.batch_end(branch, probe).await {
            Ok(()) => return Ok(Some(probe)),
            Err(AgentError::InvalidInput(_)) => probe -= 1,
            Err(error) => return Err(history_error(error)),
        }
    }
    Ok(None)
}

/// The first sequence of the batch holding `sequence`, searching no lower
/// than `floor`, which is where a window that begins inside the batch starts.
async fn batch_start(
    read: &HistoryRead<'_>,
    branch: &str,
    sequence: u64,
    floor: u64,
) -> Result<u64, RpcError> {
    let mut probe = sequence.saturating_sub(1);
    while probe >= floor && probe > 0 {
        match read.batch_end(branch, probe).await {
            Ok(()) => return Ok(probe + 1),
            Err(AgentError::InvalidInput(_)) => probe -= 1,
            Err(error) => return Err(history_error(error)),
        }
    }
    Ok(floor)
}

/// `low..=end` through the source query.
async fn window_items(
    read: &HistoryRead<'_>,
    branch: &str,
    low: u64,
    end: u64,
    display: bool,
    configuration: bool,
) -> Result<Vec<HistoryItem>, RpcError> {
    let limit = end.saturating_sub(low).saturating_add(1).max(1);
    let filter = if display {
        QueryFilter::And {
            conditions: vec![
                wire::ProductEvent::query_filter(),
                QueryFilter::Not {
                    condition: Box::new(QueryFilter::In {
                        value: QueryOperand::Field {
                            path: "kind".into(),
                        },
                        values: if configuration {
                            vec!["context/compaction".into()]
                        } else {
                            vec!["context/compaction".into(), "context/configured".into()]
                        },
                    }),
                },
            ],
        }
    } else {
        wire::ProductEvent::query_filter()
    };
    let reply = read
        .query(
            branch,
            TimelineQuery {
                scope: TimelineQueryScope::Branch,
                from: Some(dim_agent_types::TimelinePosition {
                    branch_id: branch.into(),
                    sequence: low,
                }),
                through: Some(end),
                include_publishing: false,
                query: QuerySpec {
                    filter: Some(filter),
                    select: QuerySelect::Events,
                    group_by: vec![],
                    pick: None,
                    order_by: vec![],
                },
                page: Some(QueryPage {
                    limit: std::num::NonZeroU64::new(limit).unwrap(),
                    after: None,
                }),
            },
        )
        .await
        .map_err(history_error)?;
    let QueryOutput::Events { events } = reply.result else {
        unreachable!("event selection returns events")
    };
    let version = match read {
        HistoryRead::Open(_, version) | HistoryRead::Ended(_, version) => *version,
    };
    let mut visible = history_items(events.into_iter().map(|row| row.event).collect(), version)?;
    if display {
        let reply = read
            .query(
                branch,
                TimelineQuery {
                    scope: TimelineQueryScope::Branch,
                    from: Some(dim_agent_types::TimelinePosition {
                        branch_id: branch.into(),
                        sequence: low,
                    }),
                    through: Some(end),
                    include_publishing: false,
                    query: QuerySpec {
                        filter: Some(QueryFilter::In {
                            value: QueryOperand::Field {
                                path: "kind".into(),
                            },
                            values: vec!["context/compaction".into()],
                        }),
                        select: QuerySelect::Fields {
                            fields: [
                                "id",
                                "position.sequence",
                                "create_at",
                                "data.start",
                                "data.strategy",
                                "data.diagnostics.insufficientSpace",
                                "data.status",
                            ]
                            .map(String::from)
                            .to_vec(),
                        },
                        group_by: vec![],
                        pick: None,
                        order_by: vec![],
                    },
                    page: Some(QueryPage {
                        limit: std::num::NonZeroU64::new(limit).unwrap(),
                        after: None,
                    }),
                },
            )
            .await
            .map_err(history_error)?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("display selects summary fields")
        };
        let mut other = Vec::new();
        for row in rows {
            let [
                id,
                sequence,
                create_at,
                start,
                strategy,
                insufficient_space,
                status,
            ]: [_; 7] = row.values.try_into().expect("compaction summary fields");
            let value = |value| match value {
                QueryValue::Value { value } => value,
                _ => unreachable!("required compaction summary field"),
            };
            let optional = |value| match value {
                QueryValue::Missing => None,
                QueryValue::Value { value } => Some(value),
                _ => unreachable!("scalar compaction summary field"),
            };
            if value(status) != "completed" {
                other.push(value(id));
                continue;
            }
            let item = wire::CompactionItem {
                id: serde_json::from_value(value(id)).expect("event id"),
                sequence: serde_json::from_value(value(sequence)).expect("event sequence"),
                create_at: serde_json::from_value(value(create_at)).expect("event creation time"),
                data: dim_agent_types::CompactionSummary {
                    start: optional(start)
                        .map(|value| serde_json::from_value(value).expect("compaction start")),
                    strategy: serde_json::from_value(value(strategy)).expect("compaction strategy"),
                    insufficient_space: optional(insufficient_space)
                        .map(|value| serde_json::from_value(value).expect("insufficient space")),
                },
            };
            let encoded = serde_json::value::to_raw_value(&item)
                .map_err(|error| RpcError::wire(-32603, error.to_string()))?;
            visible.push(HistoryItem {
                sequence: item.sequence,
                bytes: encoded.get().len(),
                event: Some(encoded),
            });
        }
        if !other.is_empty() {
            let reply = read
                .query(
                    branch,
                    TimelineQuery {
                        scope: TimelineQueryScope::Branch,
                        from: Some(TimelinePosition {
                            branch_id: branch.into(),
                            sequence: low,
                        }),
                        through: Some(end),
                        include_publishing: false,
                        query: QuerySpec {
                            filter: Some(QueryFilter::In {
                                value: QueryOperand::Field { path: "id".into() },
                                values: other,
                            }),
                            select: QuerySelect::Events,
                            group_by: vec![],
                            pick: None,
                            order_by: vec![],
                        },
                        page: Some(QueryPage {
                            limit: std::num::NonZeroU64::new(limit).unwrap(),
                            after: None,
                        }),
                    },
                )
                .await
                .map_err(history_error)?;
            let QueryOutput::Events { events } = reply.result else {
                unreachable!("compaction events")
            };
            visible.extend(history_items(
                events.into_iter().map(|event| event.event).collect(),
                version,
            )?);
        }
        visible.sort_unstable_by_key(|item| item.sequence);
    }
    let mut visible = visible.into_iter().peekable();
    Ok((low..=end)
        .map(|sequence| {
            if visible.peek().is_some_and(|item| item.sequence == sequence) {
                visible.next().unwrap()
            } else {
                HistoryItem {
                    sequence,
                    bytes: 0,
                    event: None,
                }
            }
        })
        .collect())
}

fn completed_compaction() -> QueryFilter {
    QueryFilter::And {
        conditions: [("kind", "context/compaction"), ("data.status", "completed")]
            .into_iter()
            .map(|(path, value)| QueryFilter::In {
                value: QueryOperand::Field { path: path.into() },
                values: vec![value.into()],
            })
            .collect(),
    }
}

/// One event of a history page with its encoded form, built once, and size.
struct HistoryItem {
    sequence: u64,
    bytes: usize,
    event: Option<Box<serde_json::value::RawValue>>,
}

/// The page's events as reply items, each visible form encoded once to size it
/// and carried in that same form.
fn history_items(
    events: Vec<Arc<dim_agent_types::AgentEvent>>,
    version: u32,
) -> Result<Vec<HistoryItem>, RpcError> {
    let mut items = Vec::with_capacity(events.len());
    for event in events {
        let sequence = event.sequence;
        let encoded = match wire::ProductEvent::new(event, version) {
            Some(product) => Some(
                serde_json::value::to_raw_value(&product)
                    .map_err(|error| RpcError::wire(-32603, error.to_string()))?,
            ),
            None => None,
        };
        let bytes = encoded.as_ref().map_or(0, |raw| raw.get().len());
        items.push(HistoryItem {
            sequence,
            bytes,
            event: encoded,
        });
    }
    Ok(items)
}

/// Trims a page to whole batches within `HISTORY_TURN_BYTES`, walking the
/// batches the reply carries first: newest first for a window, oldest first
/// for a forward page. The first batch is always kept, and one above
/// `REPLY_FRAME_BYTES` fails `history_too_large` rather than produce a frame
/// the transport could drop. A page within the target asks for no batch edge.
///
/// The result is a suffix for a newest-first page and a prefix for a forward
/// one. `items` must be batch-complete at both ends for a forward read and at
/// its newer end for a window read. The byte budget names the last event that
/// fits; only the two batches around it are walked, so the store's `batch_end`
/// is asked for the batch edges the reply needs, not once per event.
async fn bounded_batches(
    read: &HistoryRead<'_>,
    branch: &str,
    items: &[HistoryItem],
    newest_first: bool,
) -> Result<std::ops::Range<usize>, RpcError> {
    if items.iter().map(|item| item.bytes).sum::<usize>() <= HISTORY_TURN_BYTES {
        return Ok(0..items.len());
    }
    let (first, first_bytes) = if newest_first {
        let begin = batch_begin(read, branch, items, items.len()).await?;
        (
            begin,
            items[begin..].iter().map(|item| item.bytes).sum::<usize>(),
        )
    } else {
        let end = batch_end_from(read, branch, items, 0).await?;
        (
            end,
            items[..end].iter().map(|item| item.bytes).sum::<usize>(),
        )
    };
    if first_bytes > REPLY_FRAME_BYTES {
        return Err(RpcError::app(
            -32000,
            "history_too_large",
            format!(
                "A history batch is {first_bytes} bytes; a reply frame allows {REPLY_FRAME_BYTES}"
            ),
        ));
    }
    if newest_first {
        // The smallest start whose suffix fits; then the batch started at or
        // above it. With no such batch the page starts at its first batch.
        let mut start = items.len();
        let mut bytes = 0;
        for index in (0..items.len()).rev() {
            if bytes + items[index].bytes > HISTORY_TURN_BYTES {
                break;
            }
            bytes += items[index].bytes;
            start = index;
        }
        let mut kept = start;
        while kept < items.len() {
            match read.batch_end(branch, items[kept - 1].sequence).await {
                Ok(()) => return Ok(kept..items.len()),
                Err(AgentError::InvalidInput(_)) => kept += 1,
                Err(error) => return Err(history_error(error)),
            }
        }
        return Ok(first..items.len());
    }
    // The most events whose prefix fits; then the batch ended at or below
    // them. Keeping fewer events than the first batch cannot fit starts at it.
    let mut end = 0;
    let mut bytes = 0;
    for item in items {
        if bytes + item.bytes > HISTORY_TURN_BYTES {
            break;
        }
        bytes += item.bytes;
        end += 1;
    }
    if end <= first {
        return Ok(0..first);
    }
    let mut kept = end - 1;
    loop {
        match read.batch_end(branch, items[kept].sequence).await {
            Ok(()) => return Ok(0..kept + 1),
            Err(AgentError::InvalidInput(_)) => kept -= 1,
            Err(error) => return Err(history_error(error)),
        }
    }
}

/// The end of the batch that starts at `items[start]`: the first event from
/// there whose `batch_end` check passes.
async fn batch_end_from(
    read: &HistoryRead<'_>,
    branch: &str,
    items: &[HistoryItem],
    start: usize,
) -> Result<usize, RpcError> {
    let mut end = start;
    loop {
        match read.batch_end(branch, items[end].sequence).await {
            Ok(()) => return Ok(end + 1),
            Err(AgentError::InvalidInput(_)) => end += 1,
            Err(error) => return Err(history_error(error)),
        }
    }
}

/// Builds a forward reply: the page's whole batches from its start within the
/// byte target. A trimmed page is a short page, not the end: `done` is false
/// and `nextAfter` is its last returned sequence, so the client continues as
/// it does today.
async fn forward_page(
    read: &HistoryRead<'_>,
    branch: &str,
    after: u64,
    limit: usize,
    through: u64,
    display: bool,
) -> Result<wire::HistoryResult, RpcError> {
    if after > 0 {
        read.batch_end(branch, after).await.map_err(history_error)?;
    }
    read.batch_end(branch, through)
        .await
        .map_err(history_error)?;
    let mut end = after.saturating_add(limit as u64).min(through);
    while end < through && end > after {
        match read.batch_end(branch, end).await {
            Ok(()) => break,
            Err(AgentError::InvalidInput(_)) => end += 1,
            Err(error) => return Err(history_error(error)),
        }
    }
    let items = if end > after {
        window_items(read, branch, after + 1, end, display, true).await?
    } else {
        Vec::new()
    };
    let keep = bounded_batches(read, branch, &items, false).await?;
    let next_after = keep
        .end
        .checked_sub(1)
        .map_or(after, |last| items[last].sequence);
    Ok(wire::HistoryResult {
        events: items
            .into_iter()
            .take(keep.end)
            .filter_map(|item| item.event)
            .collect(),
        through,
        next_after,
        done: next_after == through,
        next_before: None,
        turn_start: None,
    })
}

/// Builds a newest-first window reply: the newest whole batches within the
/// event and byte limits. A page within the byte target needs no batch edges
/// beyond its own ends; a larger one walks batches down from the newest, so
/// the store's `batch_end` is asked only for the events the trim inspects.
async fn history_window(
    read: &HistoryRead<'_>,
    branch: &str,
    mut items: Vec<HistoryItem>,
    through: u64,
    turn: crate::storage::TurnWindow,
) -> Result<wire::HistoryResult, RpcError> {
    let keep = bounded_batches(read, branch, &items, true).await?;
    let kept = items.split_off(keep.start);
    let next_before = kept.first().map(|item| item.sequence);
    let next_after = kept.last().map_or(through, |item| item.sequence);
    let done = next_before.is_none_or(|sequence| sequence <= turn.window_start);
    Ok(wire::HistoryResult {
        events: kept.into_iter().filter_map(|item| item.event).collect(),
        through,
        next_after,
        done,
        next_before,
        turn_start: turn.turn_start,
    })
}

/// Where the batch ending at `items[end - 1]` begins. `end - 1` ends a batch;
/// walking down one event at a time, the batch-end check says whether the
/// event below starts a batch of its own, and every event above it shares this
/// one.
async fn batch_begin(
    read: &HistoryRead<'_>,
    branch: &str,
    items: &[HistoryItem],
    end: usize,
) -> Result<usize, RpcError> {
    let mut begin = end - 1;
    while begin > 0 {
        match read.batch_end(branch, items[begin - 1].sequence).await {
            Ok(()) => break,
            Err(AgentError::InvalidInput(_)) => begin -= 1,
            Err(error) => return Err(history_error(error)),
        }
    }
    Ok(begin)
}
