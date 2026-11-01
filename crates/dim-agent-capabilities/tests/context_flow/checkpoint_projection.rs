use super::*;
use dim_agent_capabilities::{
    context_block,
    history::{
        context_configuration, context_messages, project_context_history, projection_checkpoint,
    },
    prompt_state::PromptSnapshot,
};

fn fixed(events: Vec<EventKind>) -> ContextHistory {
    let events: Vec<_> = events
        .into_iter()
        .enumerate()
        .map(|(index, kind)| {
            Arc::new(AgentEvent {
                id: format!("event-{index}"),
                sequence: index as u64 + 1,
                create_at: index as u64,
                tags: None,
                kind,
            })
        })
        .collect();
    ContextHistory {
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
    }
}

fn keyed(text: &str, key: &str) -> InputBlock {
    context_block::raw(text.into(), Some(key.into()))
}

#[tokio::test]
async fn keyed_blocks_replay_verbatim_after_repeated_compaction_until_retired() {
    let agent = MinimalHost::start(&builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context {
            threshold: Some(600.),
            ..Context::default()
        },
    ))
    .await
    .unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let inputs = [
        vec![keyed("<live>a</live>", "a"), keyed("<live>b</live>", "b")],
        vec![context_block::retire("<ended>b</ended>".into(), "b".into())],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ];
    for (blocks, text) in inputs.into_iter().zip([
        "x".repeat(500),
        "y".repeat(500),
        "z".repeat(500),
        "w".repeat(500),
        "next".into(),
    ]) {
        session
            .input(
                InputMessageData {
                    blocks,
                    ..InputMessageData::text(text, Some(InputMode::Queue))
                },
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
    }
    let events = session.history("main", None).await.unwrap();
    let compactions: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ContextCompaction(ContextCompactionData {
                outcome:
                    ContextCompactionResult::Completed {
                        context_baseline, ..
                    },
                ..
            }) => Some(context_baseline),
            _ => None,
        })
        .collect();
    assert!(compactions.len() >= 2);
    let context_baseline = compactions.last().unwrap();
    assert_eq!(
        context_baseline
            .iter()
            .map(|entry| (entry.key.as_str(), entry.message.as_ref().clone()))
            .collect::<Vec<_>>(),
        [("a", Message::text(Role::User, "<live>a</live>"))]
    );
    let request = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(&data.request),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        *request.messages[1],
        Message::text(Role::User, "<live>a</live>")
    );
    assert!(
        !request
            .messages
            .contains(&Arc::new(Message::text(Role::User, "<live>b</live>")))
    );
    agent.close().await.unwrap();
}

pub(crate) fn diagnostics() -> Box<CompactionDiagnosticsRecord> {
    Box::new(
        serde_json::from_value(serde_json::json!({
            "reason":"auto", "trigger":"threshold", "estimatedInputTokens":1,
            "entriesReplaced":1, "modelCallCount":1, "durationMs":1,
            "strategy":{"kind":"test"}
        }))
        .unwrap(),
    )
}

/// A completed compaction fact replacing everything before `retain_from`
/// with one summary message.
pub(crate) fn completed(
    retain_from: &str,
    summary: &str,
    through: u64,
    context_baseline: Vec<ContextBaselineEntry>,
) -> EventKind {
    EventKind::ContextCompaction(ContextCompactionData {
        turn_id: Some("turn".into()),
        request_id: Some(summary.into()),
        command: None,
        start: None,
        through,
        outcome: ContextCompactionResult::Completed {
            retain_from: retain_from.into(),
            replacement: vec![dim_agent_capabilities::history::summary_message(summary)],
            strategy: "test".into(),
            diagnostics: diagnostics(),
            context_baseline,
            replacement_parts: None,
            pinned_inputs: None,
            digest_entries: None,
        },
    })
}

/// A record written before `replacement_parts` existed still deserializes
/// and replays: with the field absent, the whole replacement is the
/// strategy part (ContextCompaction §4.3).
#[test]
fn a_record_without_replacement_parts_deserializes_and_replays() {
    use dim_agent_capabilities::history::{context_messages, summary_message};
    let record = completed("event-2", "recorded", 3, Vec::new());
    let EventKind::ContextCompaction(data) = &record else {
        unreachable!("completed builds a compaction record")
    };
    let data = data.clone();
    let json = serde_json::to_value(&data).unwrap();
    assert!(
        json["outcome"].get("replacementParts").is_none(),
        "the field is absent, as a record written before it has it"
    );
    assert!(
        json["outcome"].get("digest_entries").is_none(),
        "a record written before the digest has no entries"
    );
    let data: ContextCompactionData = serde_json::from_value(json).unwrap();
    let ContextCompactionResult::Completed {
        replacement,
        replacement_parts,
        digest_entries,
        ..
    } = &data.outcome
    else {
        panic!("a completed record")
    };
    assert_eq!(*replacement_parts, None);
    assert_eq!(*digest_entries, None);
    assert_eq!(replacement, &vec![summary_message("recorded")]);
    let context = fixed(vec![
        EventKind::InputMessage(InputMessageData::text("first", None)),
        EventKind::TurnStart(TurnStartData {
            turn_id: "turn".into(),
        }),
        EventKind::ContextCompaction(data),
        EventKind::InputMessage(InputMessageData::text("second", None)),
        EventKind::InputConsumed(InputConsumedData {
            input_id: "event-3".into(),
        }),
    ]);
    let history = project_context_history(&context).unwrap();
    assert_eq!(
        context_messages(&context, &history)
            .into_iter()
            .map(Arc::unwrap_or_clone)
            .collect::<Vec<_>>(),
        vec![
            summary_message("recorded"),
            Message::text(Role::User, "second")
        ],
        "the replacement replays unchanged, then the kept entry"
    );
}

#[tokio::test]
async fn a_streamed_context_from_a_snapshot_matches_the_full_render() {
    let store = Arc::new(MemoryTimelineStore::default());
    let key = TimelineKey {
        session_id: "stream-checkpoint".into(),
        branch_id: "main".into(),
    };
    store
        .create_session(&key.session_id, &SessionConfig::default())
        .await
        .unwrap();
    let timeline = Timeline::new(Ctx::root().unwrap(), key, vec![], store.clone());
    let input = timeline
        .append(EventKind::InputMessage(InputMessageData::text("original", None)).into())
        .unwrap();
    let consumed = timeline
        .append(
            EventKind::InputConsumed(InputConsumedData {
                input_id: input.id.clone(),
            })
            .into(),
        )
        .unwrap();
    let cut = timeline
        .append(
            EventKind::TurnStart(TurnStartData {
                turn_id: "next".into(),
            })
            .into(),
        )
        .unwrap();
    let through = timeline.sequence();
    let context = timeline.context(through).await.unwrap();
    let renderer = HistoryRenderer::default();
    let snapshot = renderer.snapshot(&context, &cut.id, &consumed).unwrap();
    timeline.save_snapshot(snapshot).await.unwrap();
    let fact = timeline
        .append(completed(&cut.id, "authoritative", through, vec![]).into())
        .unwrap();

    let (full, full_history) = renderer
        .rendered_context(&timeline, fact.sequence)
        .await
        .unwrap();
    let streamed = renderer
        .rendered_stream(&timeline, fact.sequence, 8)
        .await
        .unwrap();
    assert_eq!(streamed.history, full_history);
    assert_eq!(streamed.history, project_context_history(&full).unwrap());
    assert_eq!(
        context_messages(&full, &streamed.history)
            .into_iter()
            .map(Arc::unwrap_or_clone)
            .collect::<Vec<_>>(),
        context_messages(&full, &project_context_history(&full).unwrap())
            .into_iter()
            .map(Arc::unwrap_or_clone)
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn a_streamed_context_without_a_snapshot_matches_the_full_render() {
    let store = Arc::new(MemoryTimelineStore::default());
    let key = TimelineKey {
        session_id: "stream-filtered".into(),
        branch_id: "main".into(),
    };
    store
        .create_session(&key.session_id, &SessionConfig::default())
        .await
        .unwrap();
    let timeline = Timeline::new(Ctx::root().unwrap(), key, vec![], store.clone());
    let input = timeline
        .append(EventKind::InputMessage(InputMessageData::text("original", None)).into())
        .unwrap();
    timeline
        .append(
            EventKind::InputConsumed(InputConsumedData {
                input_id: input.id.clone(),
            })
            .into(),
        )
        .unwrap();
    timeline
        .append(
            EventKind::TurnStart(TurnStartData {
                turn_id: "next".into(),
            })
            .into(),
        )
        .unwrap();
    let through = timeline.sequence();
    let renderer = HistoryRenderer::default();
    let (full, full_history) = renderer.rendered_context(&timeline, through).await.unwrap();
    let streamed = renderer
        .rendered_stream(&timeline, through, 8)
        .await
        .unwrap();
    assert_eq!(streamed.history, full_history);
    assert_eq!(streamed.history, project_context_history(&full).unwrap());
}

#[tokio::test]
async fn incompatible_snapshot_is_rejected_before_custom_renderer_decoding() {
    let store = Arc::new(MemoryTimelineStore::default());
    let key = TimelineKey {
        session_id: "compatibility".into(),
        branch_id: "main".into(),
    };
    store
        .create_session(&key.session_id, &SessionConfig::default())
        .await
        .unwrap();
    let timeline = Timeline::new(Ctx::root().unwrap(), key.clone(), vec![], store.clone());
    let input = timeline
        .append(EventKind::InputMessage(InputMessageData::text("original", None)).into())
        .unwrap();
    let consumed = timeline
        .append(
            EventKind::InputConsumed(InputConsumedData {
                input_id: input.id.clone(),
            })
            .into(),
        )
        .unwrap();
    let cut = timeline
        .append(
            EventKind::TurnStart(TurnStartData {
                turn_id: "next".into(),
            })
            .into(),
        )
        .unwrap();
    let through = timeline.sequence();
    let checkpoint =
        projection_checkpoint(&timeline.context(through).await.unwrap(), &cut.id).unwrap();
    let fact = timeline
        .append(completed(&cut.id, "authoritative", through, vec![]).into())
        .unwrap();
    timeline
        .save_snapshot(RecoverySnapshot {
            sequence: fact.sequence,
            event_id: fact.id.clone(),
            interpreter: "foreign/v1".into(),
            state: "not this renderer's format".into(),
        })
        .await
        .unwrap();
    assert!(
        store
            .latest_snapshot(&key, fact.sequence, "foreign/v1")
            .await
            .unwrap()
            .is_some()
    );
    let renderer = HistoryRenderer {
        render: external_history,
        checkpoint_interpreter: "custom/v1",
        decode_checkpoint: |_, _| panic!("must not decode an incompatible interpreter"),
        ..HistoryRenderer::default()
    };
    let (replay, _) = renderer
        .rendered_context(&timeline, fact.sequence)
        .await
        .unwrap();
    assert!(replay.checkpoint.is_none());
    assert_eq!(replay.events.len(), 4);
    assert_eq!(
        dim_agent_capabilities::history::context_messages(
            &replay,
            &(renderer.render)(&replay).unwrap()
        )
        .into_iter()
        .map(Arc::unwrap_or_clone)
        .collect::<Vec<_>>(),
        vec![dim_agent_capabilities::history::summary_message(
            "authoritative"
        )]
    );

    let renderer = HistoryRenderer {
        encode_checkpoint: |_, checkpoint| {
            serde_json::to_string(checkpoint)
                .map(|payload| format!("custom:{payload}"))
                .map_err(|error| error.to_string())
        },
        decode_checkpoint: |snapshot, _| {
            assert_eq!(snapshot.interpreter, "custom/v1");
            serde_json::from_str(snapshot.state.strip_prefix("custom:")?).ok()
        },
        ..renderer
    };
    timeline
        .save_snapshot(
            renderer
                .snapshot(&timeline.context(through).await.unwrap(), &cut.id, &fact)
                .unwrap(),
        )
        .await
        .unwrap();
    let (accelerated, _) = renderer
        .rendered_context(&timeline, fact.sequence)
        .await
        .unwrap();
    assert_eq!(accelerated.checkpoint, Some(checkpoint.clone()));
    assert!(accelerated.events.iter().any(|event| event.id == fact.id));
    assert_eq!(
        (renderer.render)(&accelerated).unwrap(),
        (renderer.render)(&replay).unwrap()
    );
    let full = timeline.context(fact.sequence).await.unwrap();
    assert!(full.checkpoint.is_none());
    assert_eq!(full.events, replay.events);
    assert_eq!(full.positions, replay.positions);

    let renderer = HistoryRenderer::default();
    let saved = renderer
        .snapshot(&timeline.context(through).await.unwrap(), &cut.id, &fact)
        .unwrap();
    assert_eq!(
        (renderer.decode_checkpoint)(&saved, &fact),
        Some(checkpoint.clone())
    );
    for field in [
        "sequence",
        "event_id",
        "interpreter",
        "payload",
        "sha256",
        "missing",
        "raw",
    ] {
        let mut damaged = saved.clone();
        let mut envelope: serde_json::Value = serde_json::from_str(&damaged.state).unwrap();
        match field {
            "sequence" => damaged.sequence += 1,
            "event_id" => damaged.event_id.push('x'),
            "interpreter" => damaged.interpreter.push('x'),
            "payload" => {
                envelope["payload"] = format!("{} ", envelope["payload"].as_str().unwrap()).into()
            }
            "sha256" => envelope["sha256"] = "00".into(),
            "missing" => {
                envelope.as_object_mut().unwrap().remove("sha256");
            }
            "raw" => {
                envelope = serde_json::from_str(envelope["payload"].as_str().unwrap()).unwrap()
            }
            _ => unreachable!(),
        }
        damaged.state = envelope.to_string();
        assert!(
            (renderer.decode_checkpoint)(&damaged, &fact).is_none(),
            "{field}"
        );
    }
    // Anchor validity belongs to the owner (#130): the store keeps a snapshot
    // anchored at any existing event, and the owner's decode ignores one whose
    // anchor is not its completed compaction, or whose cut differs from the
    // fact; the context then falls back to fact replay with the same result.
    let at = fact.sequence;
    let fallback = |snapshot: RecoverySnapshot| {
        let (timeline, renderer, replay) = (&timeline, &renderer, &replay);
        async move {
            timeline.save_snapshot(snapshot).await.unwrap();
            let (context, _) = renderer.rendered_context(timeline, at).await.unwrap();
            assert!(context.checkpoint.is_none());
            assert_eq!(context.events, replay.events);
        }
    };
    fallback(
        renderer
            .snapshot(
                &timeline.context(through).await.unwrap(),
                &cut.id,
                &consumed,
            )
            .unwrap(),
    )
    .await;
    let mut moved = checkpoint.clone();
    moved.state.retain_from = input.id.clone();
    let mut mismatched = saved.clone();
    mismatched.state = (renderer.encode_checkpoint)(&mismatched, &moved).unwrap();
    assert!((renderer.decode_checkpoint)(&mismatched, &fact).is_none());
    fallback(mismatched).await;
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
}

/// A checkpoint whose render fails is a cache miss: the read falls back to the
/// full replay and returns that with its rendering.
#[tokio::test]
async fn a_checkpoint_whose_render_fails_falls_back_to_full_replay() {
    fn fail_on_checkpoint(
        context: &ContextHistory,
    ) -> std::result::Result<Vec<HistoryEntry>, String> {
        if context.checkpoint.is_some() {
            return Err("render failed".into());
        }
        project_context_history(context)
    }
    let store = Arc::new(MemoryTimelineStore::default());
    let key = TimelineKey {
        session_id: "fallback".into(),
        branch_id: "main".into(),
    };
    store
        .create_session(&key.session_id, &SessionConfig::default())
        .await
        .unwrap();
    let timeline = Timeline::new(Ctx::root().unwrap(), key, vec![], store);
    let input = timeline
        .append(EventKind::InputMessage(InputMessageData::text("original", None)).into())
        .unwrap();
    timeline
        .append(
            EventKind::InputConsumed(InputConsumedData {
                input_id: input.id.clone(),
            })
            .into(),
        )
        .unwrap();
    let cut = timeline
        .append(
            EventKind::TurnStart(TurnStartData {
                turn_id: "next".into(),
            })
            .into(),
        )
        .unwrap();
    let kept = timeline
        .append(EventKind::InputMessage(InputMessageData::text("kept", None)).into())
        .unwrap();
    timeline
        .append(
            EventKind::InputConsumed(InputConsumedData {
                input_id: kept.id.clone(),
            })
            .into(),
        )
        .unwrap();
    let through = timeline.sequence();
    let fact = timeline
        .append(completed(&cut.id, "authoritative", through, vec![]).into())
        .unwrap();
    let saved = HistoryRenderer::default()
        .snapshot(&timeline.context(through).await.unwrap(), &cut.id, &fact)
        .unwrap();
    timeline.save_snapshot(saved).await.unwrap();
    let full = timeline.context(fact.sequence).await.unwrap();
    let (accelerated, _) = HistoryRenderer::default()
        .rendered_context(&timeline, fact.sequence)
        .await
        .unwrap();
    assert!(
        accelerated.checkpoint.is_some(),
        "the read must use the checkpoint"
    );
    let renderer = HistoryRenderer {
        render: fail_on_checkpoint,
        ..HistoryRenderer::default()
    };
    let (context, history) = renderer
        .rendered_context(&timeline, fact.sequence)
        .await
        .unwrap();
    assert!(context.checkpoint.is_none());
    assert_eq!(context.events, full.events);
    assert_eq!(history, project_context_history(&full).unwrap());
    assert!(
        !history.is_empty(),
        "the full replay renders the kept input"
    );
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
}

/// Builds a recovery snapshot from the preparation prefix, before its
/// Completed fact, never from later input or compaction events.
fn resume(context: &ContextHistory, cut: &str) -> ContextHistory {
    let mut source = context.clone();
    if let Some(index) = context.events.iter().position(|event| {
        matches!(&event.kind, EventKind::ContextCompaction(data)
            if matches!(&data.outcome, ContextCompactionResult::Completed { retain_from, .. } if retain_from == cut))
    }) {
        source.events.truncate(index);
        source.positions.truncate(index);
    }
    let checkpoint = projection_checkpoint(&source, cut).unwrap();
    let index = context
        .positions
        .iter()
        .position(|position| position == &checkpoint.resume)
        .unwrap();
    let mut resumed = ContextHistory {
        events: context.events[index..].to_vec(),
        positions: context.positions[index..].to_vec(),
        ancestry: context.ancestry.clone(),
        checkpoint: Some(checkpoint),
    };
    if !resumed.events.iter().any(|event| {
        matches!(&event.kind,
        EventKind::ContextCompaction(data) if matches!(&data.outcome,
            ContextCompactionResult::Completed { retain_from, .. } if retain_from == cut))
    }) {
        let through = context.events.last().unwrap().sequence;
        resumed.events.push(Arc::new(AgentEvent {
            id: format!("checkpoint-anchor-{cut}"),
            sequence: through + 1,
            create_at: 0,
            tags: None,
            kind: completed(cut, "summary", through, Vec::new()),
        }));
        resumed.positions.push(TimelinePosition {
            branch_id: "main".into(),
            sequence: through + 1,
        });
    }
    resumed
}

/// Neither a missing snapshot nor an older usable one may roll the model view
/// back to A; expectations are literal, not derived by replaying the function
/// under test.
#[test]
fn completed_compaction_survives_missing_or_older_recovery_checkpoint() {
    use dim_agent_capabilities::history::{context_messages, summary_message};

    let compaction = |retain_from: &str, summary: &str, through, source: &str, text: &str| {
        completed(
            retain_from,
            summary,
            through,
            vec![ContextBaselineEntry {
                key: "rule".into(),
                source_event: source.into(),
                message: Arc::new(Message::text(Role::User, text)),
            }],
        )
    };
    let input = |text: &str, rule: &str| {
        EventKind::InputMessage(InputMessageData {
            blocks: vec![keyed(rule, "rule")],
            ..InputMessageData::text(text, None)
        })
    };
    let turn = || {
        EventKind::TurnStart(TurnStartData {
            turn_id: "turn".into(),
        })
    };
    let context = fixed(vec![
        input("first", "old rule"),
        EventKind::InputConsumed(InputConsumedData {
            input_id: "event-0".into(),
        }),
        turn(),
        compaction("event-2", "summary A", 3, "event-1", "old rule"),
        input("second", "new rule"),
        EventKind::InputConsumed(InputConsumedData {
            input_id: "event-4".into(),
        }),
        turn(),
        compaction("event-6", "summary B", 7, "event-5", "new rule"),
        EventKind::InputMessage(InputMessageData {
            blocks: vec![context_block::retire("rule ended".into(), "rule".into())],
            ..InputMessageData::text("tail", None)
        }),
        EventKind::InputConsumed(InputConsumedData {
            input_id: "event-8".into(),
        }),
        EventKind::ContextCompaction(ContextCompactionData {
            turn_id: Some("turn".into()),
            request_id: Some("failed".into()),
            command: None,
            start: None,
            through: 10,
            outcome: ContextCompactionResult::Failed {
                error: "unavailable".into(),
                diagnostics: None,
            },
        }),
    ]);
    let expected = vec![
        summary_message("summary B"),
        Message::text(Role::User, "new rule"),
        Message::text(Role::User, "rule ended"),
        Message::text(Role::User, "tail"),
    ];
    for cut in [None, Some(("event-2", 3)), Some(("event-6", 7))] {
        let candidate = match cut {
            None => context.clone(),
            Some((retain_from, fact_index)) => {
                let candidate = resume(&context, retain_from);
                assert!(
                    candidate
                        .events
                        .iter()
                        .any(|event| event.id == context.events[fact_index].id),
                    "the authoritative compaction fact must remain in the suffix"
                );
                candidate
            }
        };
        let history = project_context_history(&candidate).unwrap();
        assert_eq!(
            context_messages(&candidate, &history)
                .into_iter()
                .map(Arc::unwrap_or_clone)
                .collect::<Vec<_>>(),
            expected,
            "{cut:?}"
        );
        assert!(history.last().unwrap().baseline.is_empty());
    }
    assert_eq!(project_history(&context.events, "main").unwrap().len(), 6);
}

#[test]
fn notification_preview_is_bounded_and_utf8_safe() {
    let preview = context_block::preview(&["<&🦀".repeat(8 * 1024).as_str()]);
    assert!(preview.len() <= 16 * 1024);
    assert!(preview.ends_with("[Preview truncated at 16 KiB.]"));
    assert!(preview.starts_with("&lt;&amp;🦀"));
    assert!(!preview.contains("&\n"));
}

#[test]
fn repeated_checkpoint_retains_restored_pending_input() {
    let context = fixed(vec![
        EventKind::InputMessage(InputMessageData::text("pending", None)),
        EventKind::InputMessage(InputMessageData {
            blocks: vec![context_block::raw("<report/>".into(), None)],
            ..InputMessageData::default()
        }),
        EventKind::InputMessage(InputMessageData::text("first", None)),
        EventKind::InputConsumed(InputConsumedData {
            input_id: "event-2".into(),
        }),
    ]);
    let once = resume(&context, "event-3");
    let twice = resume(&once, "event-3");
    let checkpoint = twice.checkpoint.as_ref().unwrap();
    assert!(
        checkpoint
            .state
            .inputs
            .iter()
            .any(|(id, _)| id == "event-1")
    );
}

#[test]
fn checkpoint_replay_preserves_prompt_configuration_pending_input_and_internal_tool_cursor() {
    let scope = PromptScope::Branch {
        timeline: "main".into(),
    };
    let prompt = PromptEntry {
        source: "host".into(),
        version: "v1".into(),
        body: "rules".into(),
        scope: scope.clone(),
        delivery: PromptDelivery::Context,
        order: 0,
    };
    let configuration = ContextConfiguredData {
        timeline: "main".into(),
        revision: "config".into(),
        previous: None,
        prompt: None,
        settings: Box::new(ModelConfiguration {
            provider_id: "provider".into(),
            model_id: "model".into(),
            capability_snapshot: None,
            tools: None,
            tool_sources: Vec::new(),
            config: None,
            metadata: None,
        }),
        system: Some(SystemPrompt::Text("system".into())),
        system_source: None,
    };
    let input = |text: &str| InputMessageData {
        user: InputMessageData::text(text, None).user,
        blocks: vec![keyed(text, "selection")],
        mode: Some(InputMode::Queue),
        ..InputMessageData::default()
    };
    let mut context = fixed(vec![
        EventKind::ContextConfigured(configuration.clone()),
        EventKind::TurnStart(TurnStartData {
            turn_id: "turn".into(),
        }),
        EventKind::PromptChanged(PromptChangedData {
            timeline: "main".into(),
            revision: "prompt".into(),
            previous: None,
            reason: PromptChangeReason::Initialize,
            changes: vec![
                PromptChange::Set { entry: prompt },
                PromptChange::Set {
                    entry: PromptEntry {
                        source: "system".into(),
                        version: "s1".into(),
                        body: "system rules".into(),
                        scope: scope.clone(),
                        delivery: PromptDelivery::System,
                        order: 0,
                    },
                },
            ],
        }),
        EventKind::InputMessage(input("queued")),
        EventKind::InputMessage(input("first")),
        EventKind::InputConsumed(InputConsumedData {
            input_id: "event-4".into(),
        }),
        EventKind::ModelStart(ModelStartData {
            turn_id: "turn".into(),
            request_id: "request".into(),
        }),
        EventKind::ToolUseStart(ToolUseStartData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            call_id: "a".into(),
            name: "echo".into(),
        }),
        EventKind::ToolUseStart(ToolUseStartData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            call_id: "b".into(),
            name: "echo".into(),
        }),
        EventKind::ToolUse(ToolUseData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            call: ToolCall {
                id: "a".into(),
                name: "echo".into(),
                input: JsonObject::new(),
            },
        }),
        EventKind::ToolUse(ToolUseData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            call: ToolCall {
                id: "b".into(),
                name: "echo".into(),
                input: JsonObject::new(),
            },
        }),
        EventKind::PromptChanged(PromptChangedData {
            timeline: "main".into(),
            revision: "during-tools".into(),
            previous: None,
            reason: PromptChangeReason::Refresh,
            changes: vec![PromptChange::Set {
                entry: PromptEntry {
                    source: "host".into(),
                    version: "v2".into(),
                    body: "new rules".into(),
                    scope: scope.clone(),
                    delivery: PromptDelivery::Context,
                    order: 0,
                },
            }],
        }),
        EventKind::ModelOutputEnd(ModelOutputEndData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            outcome: ModelOutputOutcome::Completed {
                finish_reason: FinishReason::ToolUse,
            },
        }),
        EventKind::ToolResult(ToolResultData {
            call_event_id: Some("tool-call".into()),
            turn_id: "turn".into(),
            request_id: "request".into(),
            results: vec![
                ToolCallResult {
                    call_id: "a".into(),
                    result: ToolResult::text("first result"),
                },
                ToolCallResult {
                    call_id: "b".into(),
                    result: ToolResult::text("second result"),
                },
            ],
        }),
        EventKind::InputConsumed(InputConsumedData {
            input_id: "event-3".into(),
        }),
        EventKind::PromptChanged(PromptChangedData {
            timeline: "main".into(),
            revision: "revoke".into(),
            previous: None,
            reason: PromptChangeReason::Refresh,
            changes: vec![PromptChange::Revoke {
                source: "host".into(),
                scope,
            }],
        }),
    ]);
    let whole = project_context_history(&context).unwrap();
    for retain_from in ["event-12", "event-14"] {
        let resumed = resume(&context, retain_from);
        let kept = whole
            .iter()
            .position(|entry| entry.event_id == retain_from)
            .unwrap();
        assert_eq!(project_context_history(&resumed).unwrap(), whole[kept..]);
        assert_eq!(
            PromptSnapshot::from_context(&resumed),
            PromptSnapshot::from_context(&context)
        );
        assert_eq!(context_configuration(&resumed), Some(configuration.clone()));
        assert!(resumed.checkpoint.as_ref().is_some_and(|checkpoint| {
            checkpoint
                .state
                .inputs
                .iter()
                .any(|(id, _)| id == "event-3")
        }));
        context = resumed;
    }
}

/// Output facts of one request in turn `turn`, built as ModelProvider and Loop
/// commit them (ADR agent-029).
mod output {
    use super::*;

    pub fn start(request: &str) -> EventKind {
        EventKind::ModelStart(ModelStartData {
            turn_id: "turn".into(),
            request_id: request.into(),
        })
    }
    pub fn text(request: &str, delta: &str) -> EventKind {
        EventKind::Text(TextData {
            turn_id: "turn".into(),
            request_id: request.into(),
            delta: delta.into(),
        })
    }
    pub fn thinking(request: &str, delta: &str) -> EventKind {
        EventKind::Thinking(ThinkingData {
            turn_id: "turn".into(),
            request_id: request.into(),
            delta: delta.into(),
        })
    }
    pub fn tool_start(request: &str, call: &str) -> EventKind {
        EventKind::ToolUseStart(ToolUseStartData {
            turn_id: "turn".into(),
            request_id: request.into(),
            call_id: call.into(),
            name: "echo".into(),
        })
    }
    pub fn tool_input() -> JsonObject {
        serde_json::from_value(serde_json::json!({"text": "x"})).unwrap()
    }
    pub fn tool_use(request: &str, call: &str) -> EventKind {
        EventKind::ToolUse(ToolUseData {
            turn_id: "turn".into(),
            request_id: request.into(),
            call: ToolCall {
                id: call.into(),
                name: "echo".into(),
                input: tool_input(),
            },
        })
    }
    /// Closes calls that were never dispatched.
    pub fn closure(request: &str, results: &[(&str, &str)]) -> EventKind {
        EventKind::ToolResult(ToolResultData {
            turn_id: "turn".into(),
            request_id: request.into(),
            call_event_id: None,
            results: results
                .iter()
                .map(|(call, text)| ToolCallResult {
                    call_id: (*call).into(),
                    result: ToolResult::error(*text),
                })
                .collect(),
        })
    }
    pub fn end(request: &str, outcome: ModelOutputOutcome) -> EventKind {
        EventKind::ModelOutputEnd(ModelOutputEndData {
            turn_id: "turn".into(),
            request_id: request.into(),
            outcome,
        })
    }
    pub fn cancelled() -> ModelOutputOutcome {
        ModelOutputOutcome::Cancelled {
            reason: "stopped".into(),
        }
    }
    pub fn completed() -> ModelOutputOutcome {
        ModelOutputOutcome::Completed {
            finish_reason: FinishReason::ToolUse,
        }
    }
    pub fn turn_end() -> EventKind {
        EventKind::TurnEnd(TurnEndData {
            turn_id: "turn".into(),
            outcome: TurnOutcome::Cancelled {
                reason: "stopped".into(),
            },
        })
    }
    pub fn prompt(text: &str, consumed: &str) -> [EventKind; 2] {
        [
            EventKind::InputMessage(InputMessageData::text(text, None)),
            EventKind::InputConsumed(InputConsumedData {
                input_id: consumed.into(),
            }),
        ]
    }
}

fn assistant(parts: Vec<MessagePart>) -> Message {
    Message {
        role: Role::Assistant,
        content: MessageContent::Parts(parts),
    }
}

fn text_part(text: &str) -> MessagePart {
    MessagePart::Text {
        provider_metadata: None,
        text: text.into(),
        start_time: None,
        end_time: None,
    }
}

fn tool_part(id: &str, input: JsonObject) -> MessagePart {
    MessagePart::ToolUse {
        provider_metadata: None,
        id: id.into(),
        name: "echo".into(),
        input,
        start_time: None,
    }
}

fn result_of(entry: &HistoryEntry) -> (&str, &str) {
    match &entry.message.content {
        MessageContent::Parts(parts) => match parts.as_slice() {
            [
                MessagePart::ToolResult {
                    tool_use_id,
                    content,
                    is_error: Some(true),
                    ..
                },
            ] => match content {
                ToolContent::Text(text) => (tool_use_id, text),
                other => panic!("expected text, got {other:?}"),
            },
            other => panic!("expected one error result, got {other:?}"),
        },
        other => panic!("expected parts, got {other:?}"),
    }
}

/// Replaying from a checkpoint at every event boundary yields the rest of
/// the full replay, including cuts inside an open output and between its
/// calls.
fn assert_checkpoints_match(context: &ContextHistory) {
    let whole = project_context_history(context).unwrap();
    for (kept, entry) in whole.iter().enumerate().skip(1) {
        if entry.event_id == whole[kept - 1].event_id {
            continue;
        }
        let mut resumed = resume(context, &entry.event_id);
        let renderer = HistoryRenderer::default();
        let anchor = resumed.events.last().unwrap().clone();
        let mut snapshot = RecoverySnapshot {
            sequence: anchor.sequence,
            event_id: anchor.id.clone(),
            interpreter: renderer.checkpoint_interpreter.into(),
            state: String::new(),
        };
        snapshot.state =
            (renderer.encode_checkpoint)(&snapshot, resumed.checkpoint.as_ref().unwrap()).unwrap();
        resumed.checkpoint = Some((renderer.decode_checkpoint)(&snapshot, &anchor).unwrap());
        assert_eq!(
            project_context_history(&resumed).unwrap(),
            whole[kept..],
            "cut {}",
            entry.event_id
        );
    }
}

#[test]
fn an_interrupted_output_keeps_its_text_and_started_calls_and_drops_its_thinking() {
    use output::*;
    let [input, consumed] = prompt("go", "event-0");
    let [next, next_consumed] = prompt("next", "event-11");
    let context = fixed(vec![
        input,
        consumed,
        EventKind::TurnStart(TurnStartData {
            turn_id: "turn".into(),
        }),
        start("r1"),
        thinking("r1", "plan"),
        text("r1", "Hello"),
        tool_start("r1", "a"),
        tool_start("r1", "b"),
        tool_use("r1", "a"),
        closure(
            "r1",
            &[
                ("a", "Tool was not executed: the model output ended"),
                (
                    "b",
                    "Tool was not executed: the model output ended before its arguments were complete",
                ),
            ],
        ),
        end("r1", cancelled()),
        next,
        next_consumed,
        turn_end(),
    ]);
    let history = project_context_history(&context).unwrap();
    assert_eq!(
        history
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["event-1", "event-9", "event-9:0", "event-9:1", "event-12"]
    );
    assert_eq!(
        history[1].message.as_ref().clone(),
        assistant(vec![
            text_part("Hello"),
            tool_part("a", tool_input()),
            tool_part("b", JsonObject::new()),
        ])
    );
    assert_eq!(
        result_of(&history[2]),
        ("a", "Tool was not executed: the model output ended")
    );
    assert_eq!(result_of(&history[3]).0, "b");
    dim_agent_capabilities::history::validate_history_boundary(&context.events, "main").unwrap();
    assert_checkpoints_match(&context);

    let failed = ModelOutputOutcome::Failed {
        error: "aborted".into(),
        budget: None,
        presentation: None,
    };
    let text_only = fixed(vec![
        start("r1"),
        thinking("r1", "plan"),
        text("r1", "half"),
        end("r1", failed.clone()),
    ]);
    assert_eq!(
        project_context_history(&text_only).unwrap()[0]
            .message
            .as_ref()
            .clone(),
        assistant(vec![text_part("half")])
    );
    let thinking_only = fixed(vec![start("r1"), thinking("r1", "plan"), end("r1", failed)]);
    assert!(project_context_history(&thinking_only).unwrap().is_empty());
}

#[test]
fn a_completed_output_closed_by_the_loop_before_turn_end_keeps_its_thinking() {
    use output::*;
    let context = fixed(vec![
        start("r1"),
        thinking("r1", "plan"),
        tool_start("r1", "a"),
        tool_use("r1", "a"),
        end("r1", completed()),
        closure("r1", &[("a", "Tool was not executed: stopped")]),
        turn_end(),
    ]);
    let history = project_context_history(&context).unwrap();
    assert_eq!(
        history
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["event-4", "event-5:0"]
    );
    assert_eq!(
        history[0].message.as_ref().clone(),
        assistant(vec![
            MessagePart::Thinking {
                provider_metadata: None,
                thinking: "plan".into(),
                start_time: None,
                end_time: None,
            },
            tool_part("a", tool_input()),
        ])
    );
    assert_eq!(
        result_of(&history[1]),
        ("a", "Tool was not executed: stopped")
    );
    dim_agent_capabilities::history::validate_history_boundary(&context.events, "main").unwrap();
    assert_checkpoints_match(&context);
}

#[test]
fn an_unclosed_call_or_an_open_output_fails_the_next_request_context() {
    use output::*;
    let proposed = vec![
        start("r1"),
        tool_start("r1", "a"),
        tool_use("r1", "a"),
        end("r1", completed()),
    ];
    let pending = fixed(proposed.clone());
    assert_eq!(project_context_history(&pending).unwrap().len(), 1);
    assert!(dim_agent_capabilities::history::validate_context_boundary(&pending).is_err());
    for later in [turn_end(), start("r2")] {
        let mut events = proposed.clone();
        events.push(later);
        let error = project_context_history(&fixed(events)).unwrap_err();
        assert!(error.contains("no closing result"), "{error}");
    }

    let replaced = fixed(vec![
        start("r1"),
        end("r1", cancelled()),
        start("r2"),
        text("r2", "kept"),
    ]);
    assert!(project_context_history(&replaced).unwrap().is_empty());
    for open in [
        vec![],
        vec![text("r1", "late")],
        vec![tool_start("r1", "a")],
    ] {
        let mut events = vec![start("r1")];
        events.extend(open);
        events.push(start("r2"));
        let error = project_context_history(&fixed(events)).unwrap_err();
        assert!(error.contains("previous model output"), "{error}");
    }
}
