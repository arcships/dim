use super::*;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, Uri},
    response::sse::{Event, Sse},
};
use dim_agent_capabilities::{
    adapters::DefaultPrepareModelPlugin,
    context_model::ProductContextModel,
    history::HistoryRenderer,
    host::{IdentityContext, PrepareContextHost},
    prompt_state::PromptSnapshot,
    summary_strategy::ProductSummaryStrategyConfig,
};
use std::{path::Path, sync::Mutex as StdMutex};

type Seen = Arc<StdMutex<Vec<(Uri, HeaderMap, Value)>>>;

async fn endpoint(
    State(seen): State<Seen>,
    uri: Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
    seen.lock()
        .unwrap()
        .push((uri.clone(), headers, body.clone()));
    let frames = if uri.path().ends_with("/responses") {
        vec![
            json!({"type":"response.created","response":{"id":"resp-1","created_at":1,"model":body["model"]}}),
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg-1","role":"assistant","content":[]}}),
            json!({"type":"response.content_part.added","item_id":"msg-1","output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}),
            json!({"type":"response.output_text.delta","item_id":"msg-1","output_index":0,"content_index":0,"delta":"startup reply"}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"msg-1","role":"assistant","content":[{"type":"output_text","text":"startup reply","annotations":[]}]}}),
            json!({"type":"response.completed","response":{"id":"resp-1","status":"completed","usage":{"input_tokens":7,"output_tokens":3,"total_tokens":10}}}),
        ]
    } else if uri.path().contains(":streamGenerateContent") {
        vec![
            json!({"candidates":[{"content":{"role":"model","parts":[{"text":"startup reply"}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3,"totalTokenCount":10}}),
        ]
    } else if uri.path().ends_with("/messages") {
        vec![
            json!({"type":"message_start","message":{"id":"msg-1","type":"message","role":"assistant","model":body["model"],"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":7,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"startup reply"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":3}}),
            json!({"type":"message_stop"}),
        ]
    } else if body["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .is_some_and(|message| {
            message["role"] == "user" && message.to_string().contains("delegate-startup")
        })
    {
        vec![
            json!({"id":"delegate","object":"chat.completion.chunk","created":0,"model":body["model"],
            "choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"startup-agent","type":"function",
                "function":{"name":"agent","arguments":json!({"action":"request","profile":{"name":"startup-worker",
                    "description":"startup fixture","systemPrompt":"work"},"prompt":"member startup prompt"}).to_string()}}]},"finish_reason":"tool_calls"}]}),
        ]
    } else {
        vec![
            json!({"id":"x","object":"chat.completion.chunk","created":0,"model":body["model"],"choices":[{"index":0,"delta":{"content":"startup reply"},"finish_reason":"stop"}]}),
        ]
    };
    Sse::new(futures_util::stream::iter(frames.into_iter().map(
        |frame| {
            let event = Event::default().data(frame.to_string());
            Ok(match frame["type"].as_str() {
                Some(kind) => event.event(kind),
                None => event,
            })
        },
    )))
}

async fn start(home: &Path) -> RunningServer {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_with_prompt_capability(
        &mut builder,
        dim_agent_capabilities::model_provider::ModelProviderPlugin::process,
    );
    builder
        .session_value(Arc::new(
            dim_agent_capabilities::permission::CommandRulesHost(Arc::new(
                dim_agent_capabilities::permission::NoCommandRules,
            )),
        ))
        .require_feature(
            Feature::new("PromptRegistry").session("PromptRegistry", |names| {
                Ok(Box::new(
                    dim_agent_capabilities::prompt_registry::PromptRegistryPlugin::new(names)?,
                ))
            }),
        )
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install_compaction(
        &mut builder,
        Arc::new(ProductContextModel),
        ProductSummaryStrategyConfig::default(),
    );
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    dim_app_server::collaboration::install(&mut builder);
    serve(
        builder,
        ServerConfig {
            unix_path: Some(std::path::PathBuf::from("/tmp").join(format!(
                "dim-startup-sock-{}/server.sock",
                uuid::Uuid::new_v4()
            ))),
            product_home: Some(home.to_owned()),
            target_home: Some(home.join("native")),
            idle_unload_after: Duration::from_secs(3600),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

fn startup(base: &str, format: &str, credential: &str) -> Value {
    json!({"connection":{"baseUrl":base,"apiFormat":format,"apiKey":credential,
        "headers":{"X-Marker":format!("private:marker:{credential}")}},
        "contextWindow":64000,"maxOutputTokens":2048,"autoCompactWindow":48000})
}

async fn submit(client: &mut Client, sid: &str, branch: &str) {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":sid,"branchId":branch,"text":"hello"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":sid,"branchId":branch}))
            .await,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn temporary_startup_connections_execute_all_formats_without_sharing_or_persisting_secrets() {
    let home = std::env::temp_dir().join(format!("dim-startup-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let seen = Seen::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/gateway/v1", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let stopped = stop.clone();
    let app = Router::new().fallback(endpoint).with_state(seen.clone());
    let fixture = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(stopped.cancelled_owned())
            .await
            .unwrap()
    });
    let server = start(&home).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let before = client.rpc("provider.list", json!({})).await;
    assert_ok(&before);
    let mut sessions = Vec::new();
    for (format, suffix, key_header) in [
        ("anthropic", "/messages", "x-api-key"),
        ("openai-chat", "/chat/completions", "authorization"),
        ("openai-compatible", "/chat/completions", "authorization"),
        ("openai-responses", "/responses", "authorization"),
        (
            "gemini",
            "/models/startup-fixture:streamGenerateContent",
            "x-goog-api-key",
        ),
        ("xai", "/chat/completions", "authorization"),
        ("xai-responses", "/responses", "authorization"),
    ] {
        for auth in ["key", "bearer", "anonymous"] {
            let bearer = auth == "bearer";
            let credential = format!("private-credential-{format}-{auth}");
            let mut input = startup(&base, format, &credential);
            if auth != "key" {
                input["connection"]
                    .as_object_mut()
                    .unwrap()
                    .remove("apiKey");
            }
            if bearer {
                input["connection"]["authToken"] = credential.clone().into();
            }
            let created = client
                .rpc(
                    "session.create",
                    json!({"cwd":home,"model":{"modelId":"startup-fixture"},"startup":input}),
                )
                .await;
            assert_ok(&created);
            let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
            dim_agent_aimux::recording::set_request_recording(Some(
                &home.join("startup-recordings"),
            ))
            .unwrap();
            submit(&mut client, &sid, "main").await;
            let history = client
                .rpc(
                    "session.history",
                    json!({"sessionId":sid,"branchId":"main"}),
                )
                .await;
            assert_ok(&history);
            assert!(
                history.to_string().contains("startup reply"),
                "{format}: {history}"
            );
            let (uri, headers, body) = seen
                .lock()
                .unwrap()
                .last()
                .cloned()
                .expect("the model endpoint was reached");
            assert_eq!(
                uri.path(),
                format!("/gateway/v1{suffix}"),
                "{format}: {uri}"
            );
            let auth_header = if bearer { "authorization" } else { key_header };
            let expected = if auth_header == "authorization" {
                format!("Bearer {credential}")
            } else {
                credential.clone()
            };
            if auth == "anonymous" {
                assert!(
                    !headers.contains_key("authorization"),
                    "{format}: {headers:?}"
                );
                assert!(!headers.contains_key("x-api-key"), "{format}: {headers:?}");
                assert!(
                    !headers.contains_key("x-goog-api-key"),
                    "{format}: {headers:?}"
                );
            } else {
                assert_eq!(headers[auth_header], expected, "{format}");
            }
            assert_eq!(headers["x-marker"], format!("private:marker:{credential}"));
            if bearer && key_header != "authorization" {
                assert!(!headers.contains_key(key_header), "{format}: {headers:?}");
            }
            if format == "gemini" {
                assert_eq!(body["generationConfig"]["maxOutputTokens"], 2048);
            } else {
                assert_eq!(body["model"], "startup-fixture");
                assert_eq!(
                    body.get("max_tokens")
                        .or_else(|| body.get("max_completion_tokens"))
                        .or_else(|| body.get("max_output_tokens")),
                    Some(&json!(2048)),
                    "{body}"
                );
            }
            let session = server.agent().unwrap().get_session(&sid).unwrap();
            let state = session.get::<PromptSnapshot>("main").unwrap().unwrap();
            let encoded = serde_json::to_string(&state.configuration).unwrap();
            assert!(!encoded.contains(&credential), "{encoded}");
            assert!(encoded.contains("headerNames"), "{encoded}");
            assert_eq!(
                state
                    .configuration
                    .unwrap()
                    .capability_snapshot
                    .unwrap()
                    .capabilities
                    .context_window,
                64000
            );
            sessions.push((sid, credential));
        }
    }
    for (format, model, effort_pointer) in [
        ("anthropic", "claude-opus-5-5", "/output_config/effort"),
        ("openai-chat", "gpt-5.4", "/reasoning_effort"),
    ] {
        let credential = format!("private-effort-{format}");
        let created = client
            .rpc(
                "session.create",
                json!({"cwd":home,"model":{"modelId":model},
                    "modelPreferences":{"reasoningEffort":"high"},
                    "startup":startup(&base, format, &credential)}),
            )
            .await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
        submit(&mut client, &sid, "main").await;
        let body = seen.lock().unwrap().last().unwrap().2.clone();
        assert_eq!(body.pointer(effort_pointer), Some(&json!("high")), "{body}");
        let invalid = client
            .rpc(
                "session.model.update",
                json!({"sessionId":sid,"branchId":"main",
                    "modelPreferences":{"reasoningEffort":"not-advertised"}}),
            )
            .await;
        assert!(invalid.get("error").is_some(), "{invalid}");
        let unsupported_tier = client
            .rpc(
                "session.model.update",
                json!({"sessionId":sid,"branchId":"main",
                    "modelPreferences":{"serviceTier":"priority"}}),
            )
            .await;
        assert!(
            unsupported_tier.get("error").is_some(),
            "{unsupported_tier}"
        );
        let updated = client
            .rpc(
                "session.model.update",
                json!({"sessionId":sid,"branchId":"main",
                    "modelPreferences":{"reasoningEffort":"low"}}),
            )
            .await;
        assert_ok(&updated);
        submit(&mut client, &sid, "main").await;
        let body = seen.lock().unwrap().last().unwrap().2.clone();
        assert_eq!(body.pointer(effort_pointer), Some(&json!("low")), "{body}");
        let attached = client.rpc("session.attach", json!({"sessionId":sid})).await;
        assert_ok(&attached);
        assert_eq!(
            attached["result"]["capabilitySnapshot"],
            created["result"]["capabilitySnapshot"]
        );
        assert_eq!(
            attached["result"]["capabilitySnapshot"]["capabilities"]["contextWindow"],
            64000
        );
        let cleared = client
            .rpc(
                "session.model.update",
                json!({"sessionId":sid,"branchId":"main","modelPreferences":{
                    "reasoningEffort":{"action":"clear"},"serviceTier":{"action":"clear"}}}),
            )
            .await;
        assert_ok(&cleared);
        submit(&mut client, &sid, "main").await;
        let body = seen.lock().unwrap().last().unwrap().2.clone();
        assert_eq!(
            body.pointer(effort_pointer),
            (format == "openai-chat").then_some(&json!("none")),
            "{body}"
        );
        assert!(body.get("service_tier").is_none(), "{body}");
        sessions.push((sid, credential));

        let credential = format!("private-default-{format}");
        let created = client
            .rpc(
                "session.create",
                json!({"cwd":home,"model":{"modelId":model},
                    "startup":startup(&base, format, &credential)}),
            )
            .await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
        submit(&mut client, &sid, "main").await;
        let body = seen.lock().unwrap().last().unwrap().2.clone();
        assert_eq!(
            body.pointer(effort_pointer),
            (format == "openai-chat").then_some(&json!("none")),
            "{body}"
        );
        sessions.push((sid, credential));
    }
    submit(&mut client, &sessions[0].0, "main").await;
    assert_eq!(
        seen.lock().unwrap().last().unwrap().1["x-api-key"],
        sessions[0].1
    );
    let after = client.rpc("provider.list", json!({})).await;
    assert_eq!(before["result"], after["result"]);
    server.shutdown();
    server.join().await.unwrap();
    dim_agent_aimux::recording::set_request_recording(None).unwrap();
    let recordings =
        std::fs::read_to_string(home.join("startup-recordings/recordings.jsonl")).unwrap();
    assert!(
        recordings.contains("startup-fixture"),
        "requests must still be recorded"
    );
    assert!(recordings.contains("[REDACTED]"));
    let db = std::fs::read(home.join("native/dimcode.sqlite")).unwrap();
    let stored = String::from_utf8_lossy(&db);
    for (_, credential) in sessions {
        assert!(
            !stored.contains(&credential),
            "startup credentials reached SQLite"
        );
        assert!(
            !recordings.contains(&credential),
            "startup credentials or custom header values reached model-log"
        );
    }
    stop.cancel();
    fixture.await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_admission_rejects_unsafe_connections_and_ambiguous_models_without_creating_sessions()
 {
    let home = std::env::temp_dir().join(format!("dim-admission-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let server = start(&home).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let mut remote = Client::new_version(&server, true, 3).await;
    let input = startup(
        "http://127.0.0.1:9/gateway/v1",
        "openai-compatible",
        "private-not-returned",
    );
    let rejected = remote
        .rpc(
            "session.create",
            json!({"cwd":home,"model":{"modelId":"startup-fixture"},"startup":input}),
        )
        .await;
    assert!(rejected.get("error").is_some(), "{rejected}");
    assert!(!rejected.to_string().contains("private-not-returned"));
    for (pointer, value) in [
        (
            "/connection/baseUrl",
            json!("https://user:private-not-returned@example.invalid/v1"),
        ),
        (
            "/connection/baseUrl",
            json!("https://example.invalid/v1?key=private-not-returned"),
        ),
        ("/connection/baseUrl", json!("file:///private-not-returned")),
        ("/connection/authToken", json!("private-not-returned")),
        ("/connection/apiKey", json!("private-not-returned\n")),
        ("/connection/apiFormat", json!("bedrock")),
        (
            "/connection/headers",
            json!({"X-Marker":"private-not-returned","x-marker":"private-not-returned"}),
        ),
        (
            "/connection/headers",
            json!({"AUTHORIZATION":"private-not-returned"}),
        ),
        (
            "/connection/headers",
            json!({"X-Marker":"private-not-returned\n"}),
        ),
        ("/contextWindow", json!(0)),
        ("/contextWindow", json!(9_007_199_254_740_992u64)),
        ("/maxOutputTokens", json!(64001)),
        ("/autoCompactWindow", json!(64001)),
        ("/autoCompactWindow", json!(0)),
    ] {
        let mut invalid = input.clone();
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        invalid.pointer_mut(parent).unwrap()[field] = value;
        let rejected = client
            .rpc(
                "session.create",
                json!({"cwd":home,"model":{"modelId":"startup-fixture"},"startup":invalid}),
            )
            .await;
        assert!(rejected.get("error").is_some(), "{pointer}: {rejected}");
        assert!(
            !rejected.to_string().contains("private-not-returned"),
            "{rejected}"
        );
    }
    let unsupported_effort = client
        .rpc(
            "session.create",
            json!({"cwd":home,"model":{"modelId":"startup-fixture"},"startup":input,
                "modelPreferences":{"reasoningEffort":"high"}}),
        )
        .await;
    assert!(
        unsupported_effort.get("error").is_some(),
        "{unsupported_effort}"
    );
    assert!(server.agent().unwrap().sessions().is_empty());
    let mut providers = Vec::new();
    for name in ["default", "duplicate", "duplicate"] {
        let id = uuid::Uuid::new_v4().to_string();
        let created = client.rpc("provider.create", json!({"providerId":id,"displayName":id,
            "connection":{"driverKind":"openai-compatible","baseUrl":"http://127.0.0.1:9/v1","auth":{"type":"anonymous"}},
            "models":[{"modelId":name,"displayName":name,"status":"active","capabilities":dim_agent_types::CatalogCapabilities::with_limits(64000,2048)}],"enabled":true})).await;
        assert_ok(&created);
        providers.push((id, created["result"]["version"].clone()));
    }
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"cwd":home,"providerId":providers[0].0,"modelId":"default"}),
            )
            .await,
    );
    let ambiguous = client
        .rpc(
            "session.create",
            json!({"cwd":home,"model":{"modelId":"duplicate"},"startup":{}}),
        )
        .await;
    assert!(
        ambiguous["error"]["message"]
            .as_str()
            .unwrap()
            .contains("ambiguous"),
        "{ambiguous}"
    );
    assert!(server.agent().unwrap().sessions().is_empty());
    assert_ok(&client.rpc("provider.update", json!({"providerId":providers[2].0,"expectedVersion":providers[2].1,"patch":{"enabled":false}})).await);
    let unique = client
        .rpc(
            "session.create",
            json!({"cwd":home,"model":{"modelId":"duplicate"},"startup":{}}),
        )
        .await;
    assert_ok(&unique);
    assert_eq!(unique["result"]["model"]["providerId"], providers[1].0);
    let undeclared = uuid::Uuid::new_v4().to_string();
    assert_ok(&client.rpc("provider.create", json!({"providerId":undeclared,"displayName":"undeclared",
        "connection":{"driverKind":"openai-compatible","baseUrl":"http://127.0.0.1:9/v1","auth":{"type":"anonymous"}},
        "models":[{"modelId":"undeclared","displayName":"undeclared","status":"active",
            "capabilities":dim_agent_types::CatalogCapabilities::default()}],"enabled":true})).await);
    let catalog_before = client
        .rpc("provider.catalog", json!({"providerId":undeclared}))
        .await;
    assert_ok(&catalog_before);
    let declared = client
        .rpc(
            "session.create",
            json!({"cwd":home,"model":{"modelId":"undeclared"},
        "startup":{"contextWindow":12345,"maxOutputTokens":321}}),
        )
        .await;
    assert_ok(&declared);
    let session = server
        .agent()
        .unwrap()
        .get_session(declared["result"]["sessionId"].as_str().unwrap())
        .unwrap();
    let snapshot = session
        .get::<PromptSnapshot>("main")
        .unwrap()
        .unwrap()
        .configuration
        .unwrap()
        .capability_snapshot
        .unwrap();
    assert_eq!(snapshot.capabilities.context_window, 12345);
    assert_eq!(snapshot.capabilities.max_output_tokens, 321);
    let catalog_after = client
        .rpc("provider.catalog", json!({"providerId":undeclared}))
        .await;
    assert_eq!(catalog_before["result"], catalog_after["result"]);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn temporary_startup_rebinds_after_restart_and_preserves_parent_cut_connections() {
    let home = std::env::temp_dir().join(format!("dim-rebind-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let seen = Seen::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/gateway/v1", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let stopped = stop.clone();
    let app = Router::new().fallback(endpoint).with_state(seen.clone());
    let fixture = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(stopped.cancelled_owned())
            .await
            .unwrap()
    });
    let server = start(&home).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let initial = startup(&base, "openai-compatible", "private-original");
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":home,"model":{"modelId":"startup-fixture"},"startup":initial,
                "tools":["agent","branch"],"permissionPolicy":{"allowedTools":["agent","branch"]}}),
        )
        .await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let original = created["result"]["model"]["providerId"].clone();
    submit(&mut client, &sid, "main").await;
    let session = server.agent().unwrap().get_session(&sid).unwrap();
    let cut = session.sequence("main").unwrap();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":sid,"branchId":"main","text":"delegate-startup"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":sid,"branchId":"main"}))
            .await,
    );
    let member = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(member) = session
                .local("main", ..)
                .await
                .unwrap()
                .into_iter()
                .find_map(|event| match &event.kind {
                    dim_agent_types::EventKind::CollaborationDelivered(data) => {
                        match &data.delivery {
                            dim_agent_types::CollaborationDelivery::Ok { session_id, .. } => {
                                Some(session_id.clone())
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                })
            {
                break member;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("startup member delivered");
    submit(&mut client, &member, "main").await;
    assert_eq!(
        seen.lock().unwrap().last().unwrap().1["authorization"],
        "Bearer private-original"
    );
    let child = client
        .rpc(
            "session.branch_create",
            json!({"sessionId":sid,"parentBranchId":"main","at":cut,"prompt":"child"}),
        )
        .await;
    assert_ok(&child);
    let child_id = child["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":sid,"branchId":child_id}))
            .await,
    );
    let second_base = base.replace("/gateway/", "/other/");
    let replacement = startup(&second_base, "openai-compatible", "private-replacement");
    assert_ok(
        &client
            .rpc(
                "session.model.update",
                json!({"sessionId":sid,"branchId":"main","startup":replacement}),
            )
            .await,
    );
    submit(&mut client, &sid, "main").await;
    assert_eq!(
        seen.lock().unwrap().last().unwrap().0.path(),
        "/other/v1/chat/completions"
    );
    submit(&mut client, &sid, &child_id).await;
    assert_eq!(
        seen.lock().unwrap().last().unwrap().1["authorization"],
        "Bearer private-original"
    );
    assert_ok(&client.rpc("session.detach", json!({"sessionId":sid})).await);
    assert!(session.unload_if_idle().await.unwrap());
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
    submit(&mut client, &sid, &child_id).await;
    assert_eq!(
        seen.lock().unwrap().last().unwrap().1["authorization"],
        "Bearer private-original"
    );
    server.shutdown();
    server.join().await.unwrap();
    let server = start(&home).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
    let missing = client
        .rpc(
            "session.model.update",
            json!({"sessionId":sid,"branchId":"main","startup":{}}),
        )
        .await;
    assert_eq!(
        missing["error"]["data"]["appError"], "provider_failed",
        "{missing}"
    );
    assert!(
        missing["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not bound"),
        "{missing}"
    );
    let mut rebind = initial;
    rebind.as_object_mut().unwrap().remove("contextWindow");
    rebind.as_object_mut().unwrap().remove("maxOutputTokens");
    let rebound = client
        .rpc(
            "session.model.update",
            json!({"sessionId":sid,"branchId":"main","startup":rebind}),
        )
        .await;
    assert_ok(&rebound);
    submit(&mut client, &sid, &child_id).await;
    assert_eq!(
        seen.lock().unwrap().last().unwrap().1["authorization"],
        "Bearer private-original"
    );
    let session = server.agent().unwrap().get_session(&sid).unwrap();
    let state = session.get::<PromptSnapshot>(&child_id).unwrap().unwrap();
    assert_eq!(state.configuration.unwrap().provider_id, original);
    submit(&mut client, &member, "main").await;
    session.close().await.unwrap();
    submit(&mut client, &member, "main").await;
    assert_eq!(
        seen.lock().unwrap().last().unwrap().1["authorization"],
        "Bearer private-original",
        "an open member retains its inherited connection after its leader closes"
    );
    server.shutdown();
    server.join().await.unwrap();
    stop.cancel();
    fixture.await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}
