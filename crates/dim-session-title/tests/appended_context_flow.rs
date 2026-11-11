//! The context a capability appends to an operator input does not title the
//! Session: Hooks A.6.2 and Skills §9 add their blocks after the operator's
//! own, and the title is generated from the operator's words alone (#1378).
#![cfg(unix)]
mod support;

use axum::{Router, routing::post};
use dim_agent_capabilities::{adapters::MockTools, defaults};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_app_server::{RunningServer, ServerConfig, serve};
use dim_client::Client;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use support::*;
use tokio::sync::mpsc;

const PROVIDER: &str = "018ec73f-0000-7000-8000-00000000000b";
const HOOK: &str = "<dim-hooks kind=\"prompt-submit\">The parser's entry points are in src/parser.rs; read it before answering, and keep the answer short.</dim-hooks>";
const SKILL: &str = "<dim-skill name=\"check\">\nSkill: check\nDescription: D\nFile: /tmp/check/SKILL.md\n\nInstructions:\nBODY\n</dim-skill>";
/// What the real `UserPromptSubmit` hook used by the empty-content flow
/// answers as its additional context.
const HOOK_CONTEXT: &str = "Read src/parser.rs before answering.";

fn builder(home: &std::path::Path) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_session_title::install(&mut builder);
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    dim_app_server::collaboration::install(&mut builder);
    dim_app_server::hooks_settings::install(&mut builder, Some(home.to_owned()));
    dim_agent_capabilities::hooks::install(&mut builder);
    builder
}

/// A served product profile whose title requests answer through the returned
/// channel, as the local fixture stands in for the provider.
async fn start() -> (
    Box<Store>,
    RunningServer,
    Client,
    mpsc::UnboundedReceiver<Request>,
) {
    start_with(false).await
}

/// The `UserPromptSubmit` command the user's hooks run, if the Session gets
/// one: a script that records each run and answers with `HOOK_CONTEXT` as its
/// additional context. A script keeps the command's own shell quoting out of
/// the fixture.
fn hook(home: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let script = home.join("prompt-context.sh");
    let runs = home.join("hook-runs");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf 'run\\n' >> '{}'\nprintf '%s' '{{\"hookSpecificOutput\":{{\"additionalContext\":\"{HOOK_CONTEXT}\"}}}}'\n",
            runs.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script.to_string_lossy().into_owned()
}

/// How many times the `UserPromptSubmit` hook command ran.
fn hook_runs(store: &Store) -> usize {
    std::fs::read_to_string(store.0.join("hook-runs"))
        .unwrap_or_default()
        .lines()
        .count()
}

/// [`start`] with the user's hooks document running the `UserPromptSubmit`
/// script `use_hook` writes, so the served Session runs a real hook.
async fn start_with(
    use_hook: bool,
) -> (
    Box<Store>,
    RunningServer,
    Client,
    mpsc::UnboundedReceiver<Request>,
) {
    let store = Box::new(Store::new());
    if use_hook {
        let command = hook(&store.0);
        std::fs::write(
            store.0.join("hooks.json"),
            json!({"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":command}]}]}})
                .to_string(),
        )
        .unwrap();
    }
    let (sent, requests) = mpsc::unbounded_channel::<Request>();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/{*path}", post(complete))
                .with_state(sent),
        )
        .await
        .unwrap();
    });
    let server = serve(
        builder(&store.0),
        ServerConfig {
            unix_path: Some(PathBuf::from("/tmp").join(format!(
                "dim-title-appended-{}/server.sock",
                uuid::Uuid::new_v4()
            ))),
            product_home: Some(store.0.clone()),
            ..store.config()
        },
    )
    .await
    .unwrap();
    let mut client = client(&server).await;
    client.rpc("provider.create", json!({"providerId":PROVIDER,"displayName":"Title fixture",
        "connection":{"driverKind":"openai-compatible","baseUrl":format!("http://{address}/v1"),"auth":{"type":"anonymous"}},
        "models":[{"modelId":"main","displayName":"Main","status":"active",
            "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000,16_384)}],"enabled":true})).await.unwrap();
    client
        .rpc(
            "provider.select",
            json!({"providerId":PROVIDER,"modelId":"main"}),
        )
        .await
        .unwrap();
    configure(
        &mut client,
        json!({"enabled":true,"model":{"providerId":PROVIDER,"modelId":"main"}}),
    )
    .await;
    (store, server, client, requests)
}

async fn summary(client: &mut Client, id: &str) -> Value {
    client
        .rpc("session.summaries", json!({}))
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["sessionId"] == id)
        .expect("the Session is listed")
        .clone()
}

/// An interactive Session whose SessionTitle part has activated: `apply`
/// registers `session.rename` after it registers the observation.
async fn session(client: &mut Client, home: &std::path::Path) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":home,"tools":["wait"],"interactive":true,
            "permissionPolicy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}),
        )
        .await
        .unwrap();
    let id = created["sessionId"].as_str().unwrap().to_owned();
    client
        .rpc("session.attach", json!({"sessionId":id}))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply = client
                .rpc("session.rename", json!({"sessionId":id,"title":0}))
                .await
                .unwrap_err();
            if dim_client::app_error(&reply) == Some("invalid_params") {
                break;
            }
            assert!(waiting(&reply), "{reply}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the SessionTitle part activates");
    id
}

async fn submit(client: &mut Client, id: &str, content: Vec<&str>) {
    let content: Vec<Value> = content
        .into_iter()
        .map(|text| json!({"type":"text","text":text}))
        .collect();
    client
        .rpc(
            "session.submit",
            json!({"sessionId":id,"branchId":"main","user":{"content":content}}),
        )
        .await
        .unwrap();
}

/// Submits user content as given, which may be empty: an empty content list is
/// the input shape whose first block has no operator text before it (#1386).
async fn submit_content(
    client: &mut Client,
    id: &str,
    content: Value,
) -> Result<Value, dim_client::Error> {
    client
        .rpc(
            "session.submit",
            json!({"sessionId":id,"branchId":"main","user":{"content":content}}),
        )
        .await
}

/// The text the title request carried and its release, once the request that
/// follows `submitted` reaches the provider.
async fn title_request(
    requests: &mut mpsc::UnboundedReceiver<Request>,
) -> (String, tokio::sync::oneshot::Sender<String>) {
    let (request, release) = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .expect("the adopted model receives the title request")
        .unwrap();
    let messages = request["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "{request}");
    (messages[1]["content"].as_str().unwrap().to_owned(), release)
}

async fn titled(client: &mut Client, id: &str, title: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while summary(client, id).await["title"] != title {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the committed title arrives");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hook_context_after_the_operators_text_does_not_title() {
    let (store, server, mut client, mut requests) = start().await;
    let id = session(&mut client, &store.0).await;
    submit(&mut client, &id, vec!["Why does the parser stall?", HOOK]).await;
    let (text, release) = title_request(&mut requests).await;
    assert_eq!(text, "Why does the parser stall?");
    release.send("Parser stall".into()).unwrap();
    titled(&mut client, &id, "Parser stall").await;
    assert!(requests.try_recv().is_err(), "one title attempt per input");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_operators_own_marker_in_the_first_block_still_titles() {
    let (store, server, mut client, mut requests) = start().await;
    let id = session(&mut client, &store.0).await;
    submit(&mut client, &id, vec![HOOK]).await;
    let (text, release) = title_request(&mut requests).await;
    assert_eq!(text, HOOK);
    release.send("Hook text".into()).unwrap();
    titled(&mut client, &id, "Hook text").await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_loaded_skill_after_the_operators_text_does_not_title() {
    let (store, server, mut client, mut requests) = start().await;
    let id = session(&mut client, &store.0).await;
    submit(&mut client, &id, vec!["$skill: check the parser", SKILL]).await;
    let (text, release) = title_request(&mut requests).await;
    assert_eq!(text, "$skill: check the parser");
    release.send("Parser check".into()).unwrap();
    titled(&mut client, &id, "Parser check").await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_input_without_appended_context_titles_unchanged() {
    let (store, server, mut client, mut requests) = start().await;
    let id = session(&mut client, &store.0).await;
    submit(&mut client, &id, vec!["Just the operator's own words"]).await;
    let (text, release) = title_request(&mut requests).await;
    assert_eq!(text, "Just the operator's own words");
    release.send("Own words".into()).unwrap();
    titled(&mut client, &id, "Own words").await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// The operator-input entries refuse a content with no operator content before
/// any submit check runs (#1386). Each refused shape is the same input the
/// entry would refuse with no hook installed; the difference is that no check
/// may run first, because a check's context would be the only content and would
/// make the invalid input valid.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_input_shape_is_refused_before_any_check_runs() {
    for content in [
        json!([{"type":"text","text":""}]),
        json!([]),
        json!([
            {"type":"text","text":""},
            {"type":"text","text":""}
        ]),
    ] {
        let (store, server, mut client, mut requests) = start_with(true).await;
        let id = session(&mut client, &store.0).await;
        let refused = submit_content(&mut client, &id, content.clone())
            .await
            .expect_err("an input with no operator content is refused");
        assert_eq!(
            dim_client::app_error(&refused),
            Some("session_invalid_input"),
            "{content}: {refused}"
        );
        assert_eq!(
            hook_runs(&store),
            0,
            "{content}: a refused input ran the hook"
        );
        refused_wrote_nothing(&mut client, &id, content, &mut requests).await;
        drop(client);
        server.shutdown();
        server.join().await.unwrap();
    }
}

/// The same refusal through `session.input.withdraw` with `steer`: neither the
/// withdrawal nor the replacement is written, and no check runs. An input the
/// queue does not hold would otherwise answer `notHeld` and still write both,
/// so the refusal has to come first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_steer_is_refused_before_any_check_runs_and_withdraws_nothing() {
    for content in [
        json!([{"type":"text","text":""}]),
        json!([]),
        json!([
            {"type":"text","text":""},
            {"type":"text","text":""}
        ]),
    ] {
        let (store, server, mut client, mut requests) = start_with(true).await;
        let id = session(&mut client, &store.0).await;
        let refused = client
            .rpc(
                "session.input.withdraw",
                json!({"sessionId":id,"branchId":"main","inputId":"not-held",
                    "steer":{"content":content}}),
            )
            .await
            .expect_err("an empty steer is refused");
        assert_eq!(
            dim_client::app_error(&refused),
            Some("session_invalid_input"),
            "{content}: {refused}"
        );
        assert_eq!(
            hook_runs(&store),
            0,
            "{content}: a refused steer ran the hook"
        );
        let history = history(&mut client, &id).await;
        assert!(
            !history.iter().any(|event| event["name"] == "withdraw"),
            "{content}: a refused steer wrote its withdrawal: {history:?}"
        );
        assert!(
            !history
                .iter()
                .any(|event| event["name"] == "message" && event["data"].get("user").is_some()),
            "{content}: a refused steer wrote a replacement: {history:?}"
        );
        refused_wrote_nothing(&mut client, &id, content, &mut requests).await;
        drop(client);
        server.shutdown();
        server.join().await.unwrap();
    }
}

/// The refused input changed nothing observable: no title request, no preview,
/// no operator input in history.
async fn refused_wrote_nothing(
    client: &mut Client,
    id: &str,
    content: Value,
    requests: &mut mpsc::UnboundedReceiver<Request>,
) {
    assert!(
        tokio::time::timeout(Duration::from_secs(5), requests.recv())
            .await
            .is_err(),
        "{content}: a context the operator did not write titles nothing"
    );
    let preview = summary(client, id).await["preview"].clone();
    assert!(preview.is_null(), "{content}: no preview: {preview}");
    assert!(
        !history(client, id)
            .await
            .iter()
            .any(|event| event["name"] == "message" && event["data"].get("user").is_some()),
        "{content}: the refused input wrote nothing"
    );
}

async fn history(client: &mut Client, id: &str) -> Vec<Value> {
    client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":"main","after":0,"limit":64}),
        )
        .await
        .unwrap()["events"]
        .as_array()
        .unwrap()
        .clone()
}

/// An image-only input has operator content, so the entry accepts it and the
/// hook's context goes after the image, where both readers skip it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_image_only_input_takes_the_context_and_neither_reader_uses_it() {
    let (store, server, mut client, mut requests) = start_with(true).await;
    let id = session(&mut client, &store.0).await;
    submit_content(
        &mut client,
        &id,
        json!([{"type":"image","source":{"type":"url","mediaType":"image/png",
            "url":"https://example.com/image.png"}}]),
    )
    .await
    .expect("an image-only input is valid");
    let events = history(&mut client, &id).await;
    let user = events
        .iter()
        .filter(|event| event["name"] == "message")
        .filter_map(|event| event["data"]["user"]["content"].as_array().cloned())
        .find(|content| content[0]["type"] == "image")
        .unwrap_or_else(|| panic!("the image input is in history: {events:#?}"));
    assert!(
        matches!(&user[1], Value::Object(block) if block["text"]
        .as_str()
        .is_some_and(|text| text.starts_with("<dim-hooks kind=\"prompt-submit\">"))),
        "{user:?}"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(5), requests.recv())
            .await
            .is_err(),
        "the context does not title an image-only input"
    );
    let preview = summary(&mut client, &id).await["preview"].clone();
    assert!(preview.is_null(), "no preview: {preview}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}
