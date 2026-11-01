//! Native Rust samples for verification against the frozen TypeScript types.
use dim_agent_types::*;
use std::sync::Arc;

fn native_events() -> Vec<AgentEvent> {
    let turn = || "rust-turn".to_owned();
    let request_id = || "rust-request".to_owned();
    let call = ToolCall {
        id: "rust-call".into(),
        name: "echo".into(),
        input: JsonObject::new(),
    };
    let request = Arc::new(ModelRequest {
        provider_id: "rust-provider".into(),
        model_id: "rust-model".into(),
        capability_snapshot: None,
        messages: vec![Arc::new(Message::text(
            Role::User,
            "Rust -> TypeScript 中文",
        ))],
        tools: Some(
            vec![ToolDefinition {
                name: "echo".into(),
                description: "Echo".into(),
                source: ToolSource::Builtin,
                input_schema: JsonSchema {
                    schema_type: Some(SchemaType::Object),
                    properties: None,
                    items: None,
                    required: None,
                    enum_values: None,
                    description: None,
                    extensions: JsonObject::new(),
                },
                risk: None,
                metadata: None,
                file_changes: None,
            }]
            .into(),
        ),
        system: Some(
            SystemPrompt::Parts(vec![SystemPromptPart::Text {
                text: "System".into(),
                cache_control: Some(CacheControl::Ephemeral),
            }])
            .into(),
        ),
        config: Some(ModelConfig {
            reasoning: Some(ProviderReasoningConfig {
                enabled: Some(EnabledReasoning),
                ..Default::default()
            }),
            ..Default::default()
        }),
        metadata: None,
    });
    let result = ToolResult {
        content: ToolContent::Blocks(vec![
            ContentBlock::Text {
                text: "Done".into(),
            },
            ContentBlock::Image {
                source: ImageSource {
                    source_type: ImageSourceType::Base64,
                    media_type: "image/png".into(),
                    data: Some("AA==".into()),
                    url: None,
                },
            },
        ]),
        context_messages: Some(vec![Message {
            role: Role::ToolResult,
            content: MessageContent::Parts(vec![MessagePart::ToolResult {
                tool_use_id: call.id.clone(),
                content: ToolContent::Text("ok".into()),
                structured_content: None,
                is_error: Some(false),
            }]),
        }]),
        ..Default::default()
    };
    let kinds = vec![
        EventKind::InputMessage(InputMessageData::text("hello", None)),
        EventKind::InputApplied(InputAppliedData {
            input_id: "rust-1".into(),
            mode: Some(InputMode::Queue),
        }),
        EventKind::TurnStart(TurnStartData { turn_id: turn() }),
        EventKind::ModelStart(StepData {
            turn_id: turn(),
            request_id: request_id(),
        }),
        EventKind::ContextPrepare(ContextPrepareData {
            turn_id: turn(),
            request_id: request_id(),
            through: 4,
            request: request.clone(),
            configuration: None,
            prompt: None,
            recovery: None,
        }),
        EventKind::ContextCompaction(ContextCompactionData {
            turn_id: Some(turn()),
            request_id: Some(request_id()),
            command: None,
            start: None,
            through: 4,
            outcome: ContextCompactionResult::Failed {
                error: "fixture".into(),
                diagnostics: None,
            },
        }),
        EventKind::ContextBudget(ContextBudgetData {
            turn_id: turn(),
            request_id: request_id(),
            boundary: None,
            estimated_input_tokens: 42.5,
            baseline_source: BaselineSource::FullEstimate,
            baseline_prompt_tokens: None,
            delta_tokens: None,
            context_window: 1000.0,
            output_tokens: 100.0,
        }),
        EventKind::ModelCall(ModelCallData {
            turn_id: turn(),
            request_id: request_id(),
            request,
            configuration: None,
            prompt: None,
        }),
        EventKind::ModelUsage(ModelUsageData {
            turn_id: turn(),
            request_id: request_id(),
            usage: Usage {
                prompt_tokens: 42.0,
                completion_tokens: 5.0,
                total_tokens: 47.0,
                cache_read_tokens: None,
                cache_write_tokens: None,
                cost: None,
                currency: None,
            },
        }),
        EventKind::Thinking(DeltaData {
            turn_id: turn(),
            request_id: request_id(),
            delta: "reason".into(),
        }),
        EventKind::Text(DeltaData {
            turn_id: turn(),
            request_id: request_id(),
            delta: "response".into(),
        }),
        EventKind::ToolUse(ToolUseData {
            turn_id: turn(),
            request_id: request_id(),
            call: call.clone(),
        }),
        EventKind::ModelOutputEnd(ModelOutputEndData {
            turn_id: turn(),
            request_id: request_id(),
            outcome: ModelOutputOutcome::Completed {
                finish_reason: FinishReason::ToolUse,
            },
        }),
        EventKind::ToolCall(ToolCallData {
            turn_id: turn(),
            request_id: request_id(),
            calls: vec![call.clone()],
        }),
        EventKind::ToolStart(ToolStartData {
            turn_id: turn(),
            request_id: request_id(),
            call_event_id: "rust-14".into(),
            call_id: call.id.clone(),
        }),
        EventKind::ToolCompleted(ToolCompletedData {
            turn_id: turn(),
            request_id: request_id(),
            call_event_id: "rust-14".into(),
            call_id: call.id.clone(),
            result: result.clone(),
        }),
        EventKind::ToolResult(ToolResultData {
            turn_id: turn(),
            request_id: request_id(),
            call_event_id: Some("rust-14".into()),
            results: vec![ToolCallResult {
                call_id: call.id.clone(),
                result,
            }],
        }),
        EventKind::TurnEnd(TurnEndData {
            turn_id: turn(),
            outcome: TurnOutcome::Completed {
                reason: "done".into(),
            },
        }),
        EventKind::BranchCreate(BranchCreateData {
            at: BranchCreateAt::Create {},
            source: BranchSource::Tool {
                turn_id: turn(),
                request_id: request_id(),
                call_id: call.id,
            },
            branch: "child".into(),
            prompt: "Inspect".into(),
        }),
        EventKind::BranchStart(BranchStartData {
            parent: Some("main".into()),
            create_event: Some(21),
        }),
    ];
    kinds
        .into_iter()
        .enumerate()
        .map(|(i, kind)| AgentEvent {
            id: format!("rust-{}", i + 1),
            sequence: i as u64 + 1,
            create_at: 1_790_000_001_234,
            tags: None,
            kind,
        })
        .collect()
}

/// Rust-only extensions beyond the frozen union (ADR agent-029); the v1 boundary filters them.
fn extension_events() -> Vec<AgentEvent> {
    let kinds = vec![
        EventKind::ToolUseStart(ToolUseStartData {
            turn_id: "rust-turn".into(),
            request_id: "rust-request".into(),
            call_id: "rust-call".into(),
            name: "echo".into(),
        }),
        EventKind::ToolResult(ToolResultData {
            turn_id: "rust-turn".into(),
            request_id: "rust-request".into(),
            call_event_id: None,
            results: vec![ToolCallResult {
                call_id: "rust-call".into(),
                result: ToolResult::error(
                    "Tool was not executed: the model output ended before its arguments were complete",
                ),
            }],
        }),
    ];
    kinds
        .into_iter()
        .enumerate()
        .map(|(i, kind)| AgentEvent {
            id: format!("rust-extension-{}", i + 1),
            sequence: i as u64 + 1,
            create_at: 1_790_000_001_234,
            tags: None,
            kind,
        })
        .collect()
}

fn request_extension_event() -> AgentEvent {
    let mut event = native_events()
        .into_iter()
        .find(|event| matches!(event.kind, EventKind::ModelCall(_)))
        .expect("native model call sample");
    let EventKind::ModelCall(data) = &mut event.kind else {
        unreachable!()
    };
    let request = Arc::make_mut(&mut data.request);
    request.capability_snapshot = Some(ModelCapabilitySnapshot {
        target: ModelId {
            provider_id: request.provider_id.clone(),
            model_id: request.model_id.clone(),
        },
        capabilities: ModelCapabilities::with_limits(128_000, 16_384),
        reasoning: Some(ModelReasoning {
            supported: true,
            effort_options: Some(vec!["low".into(), "high".into()]),
            ..Default::default()
        }),
        service_tier: Some(ModelServiceTier {
            supported: true,
            default: Some("default".into()),
            options: vec!["default".into(), "flex".into()],
        }),
    });
    event.id = "rust-request-extension".into();
    event
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--emit" => {
            let events = native_events();
            let wire: Vec<_> = events.iter().filter_map(v1::EventRef::new).collect();
            serde_json::to_writer_pretty(std::io::stdout().lock(), &wire)?;
        }
        [flag] if flag == "--emit-extensions" => {
            let mut output = Vec::new();
            for event in extension_events() {
                let encoded = serde_json::to_value(&event)?;
                let decoded: AgentEvent = serde_json::from_value(encoded.clone())?;
                if decoded != event {
                    return Err(format!("{} did not round-trip", event.kind.key()).into());
                }
                output.push(serde_json::json!({
                    "event": encoded,
                    "v1": v1::EventRef::new(&event).is_some(),
                }));
            }
            serde_json::to_writer_pretty(std::io::stdout().lock(), &output)?;
        }
        [flag] if flag == "--emit-request-extensions" => {
            let event = request_extension_event();
            let native = serde_json::to_value(&event)?;
            let decoded: AgentEvent = serde_json::from_value(native.clone())?;
            let projected =
                serde_json::to_value(v1::EventRef::new(&event).expect("v1 model call"))?;
            serde_json::to_writer_pretty(
                std::io::stdout().lock(),
                &serde_json::json!({
                    "event": native,
                    "projectedV1": projected,
                    "roundTrips": decoded == event,
                }),
            )?;
        }
        [flag, path] if flag == "--roundtrip" => {
            let events: Vec<v1::AgentEvent> = serde_json::from_slice(&std::fs::read(path)?)?;
            serde_json::to_writer_pretty(std::io::stdout().lock(), &events)?;
        }
        _ => return Err(
            "Expected --emit, --emit-extensions, --emit-request-extensions or --roundtrip <path>"
                .into(),
        ),
    }
    Ok(())
}
