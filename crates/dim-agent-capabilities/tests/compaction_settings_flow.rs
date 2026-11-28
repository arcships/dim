//! The model stacks' consumption of the persisted `compaction` settings
//! source (#2235 PR 1b): the selection order drives the overlay, an adopted
//! startup window exempts its request, small values raise to the floor,
//! read failures pass through, and the decorator shapes the resolution the
//! way an adopted startup window of the same size does.

use dim_agent_capabilities::compaction_settings::{
    CompactionContextModel, CompactionSettingsCell, CompactionSettingsError,
    CompactionSettingsHost, inject_window, overlay_window,
};
use dim_agent_capabilities::host::ResolveContextModel as _;
use dim_agent_types::ModelRequest;
use dim_agent_types::compaction_settings::CompactionSettings;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn snapshot_json() -> serde_json::Value {
    serde_json::json!({
        "target": {"providerId": "p", "modelId": "m"},
        "capabilities": {"modalities": ["text"], "toolCalling": true, "streaming": true,
            "structuredOutput": false, "vision": false, "audio": false, "reasoning": false,
            "contextWindow": 128000, "maxOutputTokens": 8192}
    })
}

fn host_with(document: CompactionSettings) -> CompactionSettingsHost {
    CompactionSettingsHost(Arc::new(move |_| {
        let document = document.clone();
        Box::pin(async move { Ok(document) })
    }))
}

fn failing_host() -> CompactionSettingsHost {
    CompactionSettingsHost(Arc::new(|_| {
        Box::pin(async { Err(CompactionSettingsError::Storage) })
    }))
}

fn bare_request(provider: &str, model: &str) -> ModelRequest {
    let json = serde_json::json!({
        "providerId": provider,
        "modelId": model,
        "messages": []
    });
    serde_json::from_value(json).unwrap()
}

fn inject_window_into(request: &mut ModelRequest, window: u64) {
    *request = inject_window(request, window);
}

#[tokio::test]
async fn selection_drives_the_overlay_in_order() {
    let document: CompactionSettings =
        serde_json::from_str(r#"{"default":"40%","models":{"p/m":"400k","m":50000}}"#).unwrap();
    let host = host_with(document);
    // exact key wins, 400k soft-caps above the 128k window
    assert_eq!(
        overlay_window(
            &host,
            &bare_request("p", "m"),
            128_000,
            CancellationToken::new()
        )
        .await,
        None,
        "400k is wider than the window and falls back to the model default"
    );
    // bare model key
    assert_eq!(
        overlay_window(
            &host,
            &bare_request("other", "m"),
            128_000,
            CancellationToken::new()
        )
        .await,
        Some((50_000, None))
    );
    // default percent: 40% of 128k
    assert_eq!(
        overlay_window(
            &host,
            &bare_request("other", "missing"),
            128_000,
            CancellationToken::new()
        )
        .await,
        Some((51_200, None))
    );
}

#[tokio::test]
async fn an_explicit_startup_window_exempts_the_request() {
    let document: CompactionSettings = serde_json::from_str(r#"{"default":"40%"}"#).unwrap();
    let host = host_with(document);
    let mut request = bare_request("p", "m");
    inject_window_into(&mut request, 64_000);
    assert_eq!(
        overlay_window(&host, &request, 128_000, CancellationToken::new()).await,
        None,
        "an adopted startup token window is not overridden"
    );
    // explicit auto: no value, but the explicit fact exempts
    let mut auto_request = bare_request("p", "m");
    inject_window_into(&mut auto_request, 64_000);
    auto_request
        .metadata
        .as_mut()
        .unwrap()
        .startup
        .as_mut()
        .unwrap()
        .auto_compact_window = None;
    auto_request
        .metadata
        .as_mut()
        .unwrap()
        .startup
        .as_mut()
        .unwrap()
        .auto_compact_window_explicit = true;
    assert_eq!(
        overlay_window(&host, &auto_request, 128_000, CancellationToken::new()).await,
        None,
        "an explicit startup auto is not overridden"
    );
    // a bare startup (only model/effort selected) is still overlaid
    let mut request = bare_request("p", "m");
    request
        .metadata
        .get_or_insert_with(Default::default)
        .startup
        .get_or_insert_with(Default::default);
    assert!(
        overlay_window(&host, &request, 128_000, CancellationToken::new())
            .await
            .is_some(),
        "a startup that carries no window does not exempt the request"
    );
}

#[tokio::test]
async fn small_values_raise_to_the_floor_and_read_failures_pass_through() {
    let document: CompactionSettings = serde_json::from_str(r#"{"default":4000}"#).unwrap();
    assert_eq!(
        overlay_window(
            &host_with(document),
            &bare_request("p", "m"),
            1_000_000,
            CancellationToken::new()
        )
        .await,
        Some((32_000, Some(4_000))),
        "the floor lifts the small window and reports the raise"
    );
    assert_eq!(
        overlay_window(
            &failing_host(),
            &bare_request("p", "m"),
            128_000,
            CancellationToken::new()
        )
        .await,
        None,
        "a storage failure prepares without the overlay"
    );
}

#[tokio::test]
async fn the_decorator_injects_the_shaped_window() {
    let request = serde_json::json!({
        "providerId": "p",
        "modelId": "m",
        "messages": [],
        "capabilitySnapshot": snapshot_json(),
        "config": { "maxTokens": 8192.0 }
    });
    let request: ModelRequest = serde_json::from_value(request).unwrap();
    let cell = CompactionSettingsCell::default();
    let document: CompactionSettings = serde_json::from_str(r#"{"default":64000}"#).unwrap();
    cell.set(host_with(document));
    let decorated = CompactionContextModel::new(
        Arc::new(dim_agent_capabilities::context_model::ProductContextModel),
        cell,
    );
    let model = decorated
        .resolve(Arc::new(request), CancellationToken::new())
        .await
        .unwrap();
    let facts_request = serde_json::json!({
        "providerId": "p",
        "modelId": "m",
        "messages": [],
        "capabilitySnapshot": snapshot_json(),
        "config": { "maxTokens": 8192.0 }
    });
    let facts_request: ModelRequest = serde_json::from_value(facts_request).unwrap();
    let overlaid = dim_agent_capabilities::context_model::window_facts(
        128_000,
        8_192.,
        &inject_window(&facts_request, 64_000),
    );
    assert_eq!(
        model.compaction_budget(),
        overlaid.compaction_budget(),
        "the decorated resolution shapes the budget like a startup window of the same size"
    );
    let plain =
        dim_agent_capabilities::context_model::window_facts(128_000, 8_192., &facts_request);
    assert!(
        model.compaction_budget() < plain.compaction_budget(),
        "the persisted 64k window shapes below the physical budget"
    );
    // without the host the request resolves untouched
    let undecorated = CompactionContextModel::new(
        Arc::new(dim_agent_capabilities::context_model::ProductContextModel),
        CompactionSettingsCell::default(),
    );
    let request: ModelRequest = serde_json::from_value(serde_json::json!({
        "providerId": "p",
        "modelId": "m",
        "messages": [],
        "capabilitySnapshot": snapshot_json(),
        "config": { "maxTokens": 8192.0 }
    }))
    .unwrap();
    let model = undecorated
        .resolve(Arc::new(request.clone()), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(model.compaction_budget(), plain.compaction_budget());
}
