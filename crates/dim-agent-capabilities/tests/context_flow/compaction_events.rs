use super::*;
use rutis::host::CtxHost;
use std::sync::Mutex;

#[tokio::test]
async fn snapshot_codecs_preserve_completed_compaction_even_when_saving_fails() {
    for mode in ["builder-failure", "encoder-failure", "custom"] {
        let store = Arc::new(MemoryTimelineStore::default());
        let mut renderer = HistoryRenderer::default();
        match mode {
            "builder-failure" => {
                renderer.checkpoint =
                    CheckpointBuilder::Custom(|_, _| Err("snapshot builder unavailable".into()))
            }
            "encoder-failure" => {
                renderer.encode_checkpoint = |_, _| Err("snapshot encoder unavailable".into())
            }
            "custom" => {
                renderer.checkpoint_interpreter = "test/custom/v1";
                renderer.encode_checkpoint = |_, checkpoint| {
                    serde_json::to_string(checkpoint)
                        .map(|payload| format!("custom:{payload}"))
                        .map_err(|error| error.to_string())
                };
                renderer.decode_checkpoint = |snapshot, _| {
                    serde_json::from_str(snapshot.state.strip_prefix("custom:")?).ok()
                };
            }
            _ => unreachable!(),
        }
        let interpreter = renderer.checkpoint_interpreter;
        let decode = renderer.decode_checkpoint;
        let mut builder = builder_with_context(
            Arc::new(EchoModel),
            renderer,
            Context {
                threshold: Some(900.),
                ..Context::default()
            },
        );
        builder.timeline_store(store.clone());
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let original = "x".repeat(1_500);
        for input in [&original, "next", "third"] {
            session
                .submit(input.into(), InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
        }
        let events = session.history("main", None).await.unwrap();
        let compactions: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::ContextCompaction(data) => Some(&data.outcome),
                _ => None,
            })
            .collect();
        // One compaction replaces everything: the summary, then the running
        // turn's pinned input (ContextCompaction §5.3, §5.9). A later
        // preparation does not compact again on the regrowth rule (§5.6).
        assert!(matches!(compactions.as_slice(), [
                ContextCompactionResult::Completed {
                    replacement,
                    replacement_parts,
                    ..
                },
            ] if replacement.len() == 2
                && replacement[0] == dim_agent_capabilities::history::summary_message("short")
                && *replacement_parts == Some(ReplacementParts { strategy: 1, digest: 0 })));
        let saved = store
            .latest_snapshot(
                &TimelineKey {
                    session_id: session.id().into(),
                    branch_id: "main".into(),
                },
                events.last().unwrap().sequence,
                interpreter,
            )
            .await
            .unwrap();
        if mode == "custom" {
            let saved = saved.unwrap();
            assert!(saved.state.starts_with("custom:"));
            let timeline = Timeline::restore(
                Ctx::root().unwrap(),
                TimelineKey {
                    session_id: session.id().into(),
                    branch_id: "main".into(),
                },
                vec![],
                store.clone(),
            )
            .await
            .unwrap();
            let accelerated = timeline
                .context_with_checkpoint(timeline.sequence(), interpreter, decode)
                .await
                .unwrap();
            assert!(accelerated.checkpoint.is_some());
            assert_eq!(
                project_context_history(&accelerated).unwrap(),
                project_context_history(&timeline.context(timeline.sequence()).await.unwrap())
                    .unwrap()
            );
            dim_agent_runtime::core::close_timeline(&timeline)
                .await
                .unwrap();
        } else {
            assert!(saved.is_none());
        }
        let requests: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::ModelCall(data) => Some(&data.request),
                _ => None,
            })
            .collect();
        assert_eq!(requests.len(), 3);
        let request = requests.last().unwrap();
        assert_eq!(
            request.messages[0].as_ref(),
            &dim_agent_capabilities::history::summary_message("short")
        );
        assert!(
            !request
                .messages
                .contains(&Arc::new(Message::text(Role::User, &original)))
        );
        assert!(
            request
                .messages
                .contains(&Arc::new(Message::text(Role::User, "next")))
        );
        agent.close().await.unwrap();
    }
}

#[derive(Clone, Copy, Debug)]
enum Edit {
    None,
    Summary,
    FirstCursor,
    Failed,
    Skipped,
    Completed,
    BrokenCursor,
}

struct RewriteCompaction {
    edit: Arc<Mutex<Edit>>,
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
}
impl RewriteCompaction {
    fn new(names: &Names<'_>, edit: Arc<Mutex<Edit>>) -> rutis::Result<Self> {
        Ok(Self {
            edit,
            timeline: names.key()?,
            dispatcher: names.key()?,
        })
    }
}
fn rewrite(names: &Names<'_>, edit: Edit) -> rutis::Result<RewriteCompaction> {
    RewriteCompaction::new(names, Arc::new(Mutex::new(edit)))
}
impl DynPlugin for RewriteCompaction {
    fn name(&self) -> &str {
        "test.rewrite-compaction"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase(), self.dispatcher.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let source = Arc::new(Mutex::new(Vec::<Arc<AgentEvent>>::new()));
            let observed = source.clone();
            dispatcher.subscribe_all(ctx, "test.compaction-history", move |event| {
                observed.lock().unwrap().push(event);
                Box::pin(async { Ok(()) })
            })?;
            let current = self.edit.clone();
            timeline.before(
                ctx,
                self.name(),
                &["context/compaction"],
                move |mut draft| {
                    let edit = *current.lock().unwrap();
                    let EventKind::ContextCompaction(data) = &mut draft.kind else {
                        unreachable!()
                    };
                    let events: Vec<_> = source
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|event| event.sequence <= data.through)
                        .cloned()
                        .collect();
                    let context = ContextHistory {
                        positions: events
                            .iter()
                            .map(|event| TimelinePosition {
                                branch_id: "main".into(),
                                sequence: event.sequence,
                            })
                            .collect(),
                        events,
                        checkpoint: None,
                        ancestry: Vec::new(),
                    };
                    let history =
                        project_context_history(&context).map_err(AgentError::InvalidInput)?;
                    let diagnostics = match &data.outcome {
                        ContextCompactionResult::Completed { diagnostics, .. }
                        | ContextCompactionResult::Skipped { diagnostics, .. } => {
                            diagnostics.clone()
                        }
                        ContextCompactionResult::Failed { diagnostics, .. } => diagnostics
                            .clone()
                            .unwrap_or_else(checkpoint_projection::diagnostics),
                        ContextCompactionResult::Cancelled { .. } => {
                            checkpoint_projection::diagnostics()
                        }
                    };
                    match edit {
                        Edit::None => {}
                        Edit::Summary | Edit::FirstCursor | Edit::Completed => {
                            let retain_from = if matches!(edit, Edit::FirstCursor) {
                                history[1].source.clone()
                            } else {
                                context.events.last().unwrap().id.clone()
                            };
                            data.outcome = ContextCompactionResult::Completed {
                                retain_from,
                                replacement: vec![
                                    dim_agent_capabilities::history::summary_message(
                                        "committed summary",
                                    ),
                                ],
                                strategy: "test".into(),
                                diagnostics,
                                context_baseline: Vec::new(),
                                replacement_parts: None,
                                pinned_inputs: None,
                                digest_entries: None,
                            };
                        }
                        Edit::Failed => {
                            data.outcome = ContextCompactionResult::Failed {
                                error: "committed failure".into(),
                                diagnostics: Some(diagnostics),
                            }
                        }
                        Edit::Skipped => {
                            data.outcome = ContextCompactionResult::Skipped {
                                reason: "committed skip".into(),
                                diagnostics,
                            }
                        }
                        Edit::BrokenCursor => {
                            data.outcome = ContextCompactionResult::Completed {
                                retain_from: "outside-this-history".into(),
                                replacement: vec![
                                    dim_agent_capabilities::history::summary_message("broken"),
                                ],
                                strategy: "test".into(),
                                diagnostics,
                                context_baseline: Vec::new(),
                                replacement_parts: None,
                                pinned_inputs: None,
                                digest_entries: None,
                            };
                        }
                    }
                    Ok(draft)
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test]
async fn committed_summary_drives_current_and_following_model_requests() {
    let mut builder = builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context {
            threshold: Some(900.),
            ..Context::default()
        },
    );
    builder.require_feature(
        Feature::new("test.rewrite-compaction").branch("test.rewrite-compaction", |names| {
            Ok(Box::new(rewrite(names, Edit::Summary)?))
        }),
    );
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let original = "x".repeat(1_500);
    session
        .submit(original.clone(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    session
        .submit("next".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = session.history("main", None).await.unwrap();
    let requests: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(&data.request),
            _ => None,
        })
        .collect();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ContextCompaction(_)))
            .count(),
        1
    );
    for request in requests {
        assert_eq!(
            request.messages[0].as_ref(),
            &dim_agent_capabilities::history::summary_message("committed summary")
        );
        assert!(
            !request
                .messages
                .contains(&Arc::new(Message::text(Role::User, &original)))
        );
    }
    assert!(events.iter().any(
        |event| matches!(&event.kind, EventKind::InputMessage(data) if matches!(
            data.user.as_ref().map(|user| user.content.as_slice()),
            Some([ContentBlock::Text { text }]) if text == &original
        ))
    ));
    assert!(
        events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::Text(data)
        if data.delta.contains("<dim-summary>") && data.delta.contains("committed summary")))
    );
    agent.close().await.unwrap();
}

#[tokio::test]
async fn committed_cursor_preserves_the_history_after_its_cutoff() {
    let mut builder = builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context {
            threshold: Some(800.),
            ..Context::default()
        },
    );
    builder.require_feature(
        Feature::new("test.rewrite-compaction").branch("test.rewrite-compaction", |names| {
            Ok(Box::new(rewrite(names, Edit::FirstCursor)?))
        }),
    );
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit("x".repeat(500), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    session
        .submit("next".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = session.history("main", None).await.unwrap();
    let compaction = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ContextCompaction(data) => Some(data),
            _ => None,
        })
        .unwrap();
    let history = project_history(
        &session
            .history("main", Some(compaction.through))
            .await
            .unwrap(),
    )
    .unwrap();
    let ContextCompactionResult::Completed {
        retain_from,
        replacement,
        ..
    } = &compaction.outcome
    else {
        panic!("expected compaction")
    };
    assert_eq!(retain_from, &history[1].source);
    let expected: Vec<_> = replacement
        .iter()
        .cloned()
        .chain(
            history[1..]
                .iter()
                .map(|entry| entry.message.as_ref().clone()),
        )
        .collect();
    let request = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(&data.request),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        request.messages.iter().map(Arc::as_ref).collect::<Vec<_>>(),
        expected.iter().collect::<Vec<_>>()
    );
    assert_eq!(request.messages.len(), 3);
    agent.close().await.unwrap();
}

#[tokio::test]
async fn final_outcome_controls_normal_preparation_and_budget_recovery() {
    for recovery in [false, true] {
        for edit in [Edit::Failed, Edit::Skipped, Edit::Completed] {
            let calls = Arc::new(AtomicUsize::new(0));
            let model: Arc<dyn ModelDriver> = if recovery {
                Arc::new(BudgetFailure(calls.clone()))
            } else {
                Arc::new(EchoModel)
            };
            let mut builder = builder_with_context(
                model,
                HistoryRenderer::default(),
                Context {
                    foreign_cursor: matches!(edit, Edit::Completed),
                    threshold: (!recovery).then_some(300.),
                    ..Context::default()
                },
            );
            builder.require_feature(
                Feature::new("test.rewrite-compaction")
                    .branch("test.rewrite-compaction", move |names| {
                        Ok(Box::new(rewrite(names, edit)?))
                    }),
            );
            let agent = MinimalHost::start(&builder).await.unwrap();
            let session = agent
                .create_session(SessionConfig::default())
                .await
                .unwrap();
            let original = "x".repeat(500);
            session
                .submit(original.clone(), InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
            let events = session.history("main", None).await.unwrap();
            let requests: Vec<_> = events
                .iter()
                .filter_map(|event| match &event.kind {
                    EventKind::ModelCall(data) => Some(&data.request),
                    _ => None,
                })
                .collect();
            let compaction = events
                .iter()
                .find_map(|event| match &event.kind {
                    EventKind::ContextCompaction(data) => Some(&data.outcome),
                    _ => None,
                })
                .unwrap();
            if matches!(edit, Edit::Completed) {
                assert!(matches!(
                    compaction,
                    ContextCompactionResult::Completed { .. }
                ));
                assert_eq!(
                    requests.last().unwrap().messages[0].as_ref(),
                    &dim_agent_capabilities::history::summary_message("committed summary")
                );
            } else if recovery {
                assert!(events.iter().any(|event| matches!(&event.kind, EventKind::TurnEnd(data)
                    if matches!(&data.outcome, TurnOutcome::Failed { error } if error.contains("committed")))));
            } else {
                assert_eq!(
                    requests[0].messages,
                    vec![Arc::new(Message::text(Role::User, &original))]
                );
            }
            let expected_calls = if recovery && matches!(edit, Edit::Completed) {
                2
            } else {
                1
            };
            assert_eq!(
                requests.len(),
                expected_calls,
                "{edit:?}, recovery={recovery}"
            );
            if recovery {
                assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
            }
            agent.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn a_damaged_compaction_fails_replay_without_silently_regenerating_history() {
    let edit = Arc::new(Mutex::new(Edit::BrokenCursor));
    let mut builder = builder_with_context(
        std::sync::Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context {
            threshold: Some(300.),
            ..Context::default()
        },
    );
    let plugin_edit = edit.clone();
    builder.require_feature(Feature::new("test.rewrite-compaction").branch(
        "test.rewrite-compaction",
        move |names| {
            Ok(Box::new(RewriteCompaction::new(
                names,
                plugin_edit.clone(),
            )?))
        },
    ));
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit("x".repeat(500), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = session.history("main", None).await.unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        EventKind::ContextCompaction(ContextCompactionData {
            outcome: ContextCompactionResult::Completed { retain_from, .. },
            ..
        }) if retain_from == "outside-this-history"
    )));
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        EventKind::TurnEnd(TurnEndData {
            outcome: TurnOutcome::Failed { .. },
            ..
        })
    )));
    *edit.lock().unwrap() = Edit::None;
    session
        .submit(
            "after the broken compaction".into(),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = session.history("main", None).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ContextCompaction(_)))
            .count(),
        1
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ModelCall(_)))
    );
    let ends: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::TurnEnd(data) => Some(&data.outcome),
            _ => None,
        })
        .collect();
    assert_eq!(ends.len(), 2);
    assert!(matches!(ends[1], TurnOutcome::Failed { error }
        if error.contains("Compaction cut is outside")));
    agent.close().await.unwrap();
}
