#![cfg(unix)]
mod support;
use dim_agent_capabilities::{adapters::MockTools, defaults};
use dim_agent_runtime::testing::branch_events;
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_agent_types::{EventKind, ToolContent};
use dim_app_server::{ServerConfig, serve};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scheduled_delivery_does_not_replace_the_first_operator_input_for_titles() {
    let store = Store::new();
    std::fs::create_dir(store.0.join("scheduler")).unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_session_title::install(&mut builder);
    dim_agent_scheduler::install(&mut builder);
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    dim_app_server::collaboration::install(&mut builder);
    let mut config = ServerConfig {
        product_home: Some(store.0.clone()),
        unix_path: Some(store.0.join("server.sock")),
        ..store.config()
    };
    config.session_defaults.tools = Some(vec!["echo".into(), "schedule".into()]);
    config.session_defaults.permission_policy.preset =
        dim_agent_types::PermissionPreset::FullAccess;
    let (sent, mut requests) = tokio::sync::mpsc::unbounded_channel();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let http = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new()
                .route("/{*path}", axum::routing::post(complete))
                .with_state(sent),
        )
        .await
        .unwrap();
    });
    let server = serve(builder, config).await.unwrap();
    let agent = server.agent().unwrap();
    let mut client = client(&server).await;
    when_served(&mut client, "schedule.list", json!({})).await;
    let provider = "018ec73f-0000-7000-8000-000000000009";
    client.rpc("provider.create",json!({"providerId":provider,"displayName":"Title fixture",
        "connection":{"driverKind":"openai-compatible","baseUrl":format!("http://{address}/v1"),"auth":{"type":"anonymous"}},
        "models":[{"modelId":"main","displayName":"Main","status":"active","capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000,16_384)}],"enabled":true})).await.unwrap();
    client
        .rpc(
            "provider.select",
            json!({"providerId":provider,"modelId":"main"}),
        )
        .await
        .unwrap();
    let mut settings = json!({"enabled":false,"model":{"providerId":provider,"modelId":"main"}});
    configure(&mut client, settings.clone()).await;
    let created = client
        .rpc("session.create", json!({"cwd":store.0,"interactive":true}))
        .await
        .unwrap();
    let id = created["sessionId"].as_str().unwrap();
    let session = agent.get_session(id).unwrap();
    client
        .rpc("session.attach", json!({"sessionId":id}))
        .await
        .unwrap();
    when_served(
        &mut client,
        "session.rename",
        json!({"sessionId":id,"title":null}),
    )
    .await;
    let arguments = json!({"action":"create","job":{"name":"title provenance","schedule":{"at":"2030-01-01T00:00:00Z"},"prompt":"Review the project"}});
    let first = format!("tool:schedule {arguments}");
    client
        .rpc(
            "session.submit",
            json!({"sessionId":id,"branchId":"main","text":first}),
        )
        .await
        .unwrap();
    client
        .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
        .await
        .unwrap();
    let history = branch_events(&session, "main", ..).await.unwrap();
    let result = history
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            EventKind::ToolResult(batch) => batch.results.last().map(|r| &r.result),
            _ => None,
        })
        .expect("the operator saves a scheduled job");
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let ToolContent::Text(text) = &result.content else {
        panic!("schedule returns text")
    };
    let saved: Value = serde_json::from_str(text).unwrap();
    settings["enabled"] = json!(true);
    configure(&mut client, settings).await;
    let started = client
        .rpc("schedule.run_now", json!({"job_id":saved["job"]["id"]}))
        .await
        .unwrap();
    let run = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let history = client
                .rpc(
                    "schedule.history",
                    json!({"job_id":saved["job"]["id"],"limit":50}),
                )
                .await
                .unwrap();
            if let Some(run) = history["runs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["run_id"] == started["run_id"])
                && !matches!(
                    run["state"].as_str(),
                    Some("delivering" | "launching" | "submitted" | "running")
                )
            {
                break run.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(run["state"], "delivered", "{run}");

    // Answer every model request from here on: title questions with the
    // title, anything else with a short answer. Every title question must be
    // about the first operator input, never the scheduled run.
    let title_questions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let answered = {
        let title_questions = title_questions.clone();
        tokio::spawn(async move {
            while let Some((request, release)) = requests.recv().await {
                if request["messages"][0]["content"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("Write a short title"))
                {
                    title_questions
                        .lock()
                        .unwrap()
                        .push(request["messages"][1]["content"].clone());
                    let _ = release.send("Scheduled review".into());
                } else {
                    let _ = release.send("done".into());
                }
            }
        })
    };
    let mut titled = false;
    for attempt in 0..2 {
        if attempt == 1 {
            client
                .rpc(
                    "session.submit",
                    json!({"sessionId":id,"branchId":"main","text":"Check the scheduled result"}),
                )
                .await
                .unwrap();
        }
        titled = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let listed = client.rpc("session.summaries", json!({})).await.unwrap();
                if listed
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|e| e["sessionId"] == id && e["title"] == "Scheduled review")
                {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or(false);
        if titled {
            break;
        }
    }
    assert!(titled, "the first operator input titles the Session");
    let questions = title_questions.lock().unwrap().clone();
    assert!(!questions.is_empty(), "a title question was asked");
    for question in &questions {
        assert_eq!(
            question, &first,
            "a title question asks about the first operator input only"
        );
    }
    // The scheduled run reached the Session's own branch: its input carries
    // the `dim-scheduler` marker and the saved prompt, and the branch
    // consumed it.
    let run_id = run["run_id"].as_str().unwrap().to_owned();
    let reached = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let history = branch_events(&session, "main", ..).await.unwrap();
            let input = history.iter().find(|event| {
                matches!(&event.kind, EventKind::InputMessage(data)
                    if data.submission_id.as_deref() == Some(format!("scheduler:{run_id}").as_str()))
            });
            if let Some(input) = input {
                let EventKind::InputMessage(data) = &input.kind else {
                    unreachable!()
                };
                let marker = data
                    .blocks
                    .iter()
                    .map(|block| match block {
                        dim_agent_types::InputBlock::Raw { text, .. } => text.as_str(),
                    })
                    .collect::<String>();
                let prompt = data
                    .user
                    .as_ref()
                    .map(|user| {
                        user.content
                            .iter()
                            .map(|block| match block {
                                dim_agent_types::ContentBlock::Text { text } => text.as_str(),
                                dim_agent_types::ContentBlock::Image { .. } => "",
                            })
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                assert!(
                    marker.contains("dim-scheduler") && prompt.contains("Review the project"),
                    "the scheduled input carries the marker and the saved prompt: {marker:?} {prompt:?}"
                );
                if history.iter().any(|event| {
                    matches!(&event.kind, EventKind::InputConsumed(data)
                        if data.input_id == input.id)
                }) {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        reached.is_ok(),
        "the scheduled-run input is consumed by the Session's branch"
    );
    answered.abort();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    http.abort();
}
