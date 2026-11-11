//! The process host ports whose owner is a Settings source or a model
//! catalogue: the switches, the installed runtimes, PromptSuggestion's
//! document and the two model ports. Each face is served under its own name
//! for any language, the documented request object is `{}`, and the Rust
//! binding calls the same operations. These tests pin that the binding and the
//! wire answer the same, that a cancelled or abandoned call ends the owner's
//! read, and that a failure keeps the code its caller acts on
//! ([service faces §4.8](../../docs/design/service-faces.md#48-宿主端口executionsettingshost-等五个)).
use dim_agent_capabilities::execution_settings::{
    ExecutionSettings, ExecutionSettingsError, ExecutionSettingsHost,
};
use dim_agent_capabilities::host::{
    HostError, ModalityModelsReply, ModelPortError, ModelSelectionHost, ModelSelectionRequest,
    SelectableModelsHost, SelectableModelsReply,
};
use dim_agent_capabilities::prompt_suggestion::{
    PromptSuggestionSettings, PromptSuggestionSettingsError, PromptSuggestionSettingsHost,
};
use dim_agent_capabilities::toolchain::{
    InstalledToolchain, ToolchainSourceHost, ToolchainSourceReply, ToolchainView,
};
use dim_agent_types::{ModalityModelSnapshot, ModelId, ModelSelection};
use rutis::Ctx;
use rutis::host::CtxHost;
use rutis_bridge::session::{Error, HostDispatch, Value as RpcValue, host_key};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// The face `name` is provided under, as a caller of another language reads
/// it.
fn face(root: &Ctx, name: &str) -> Arc<dyn HostDispatch> {
    root.get_at::<dyn HostDispatch>(&host_key(name))
        .unwrap_or_else(|| panic!("{name} is a data face"))
}

/// One call of `port`'s face with its request object, waiting for its result.
async fn call(port: &Arc<dyn HostDispatch>, method: &str, request: Value) -> Result<Value, Error> {
    match port.invoke(method, RpcValue::List(vec![request.into()]))? {
        RpcValue::Reference(reply) if reply.is_future() => Ok(reply.wait_async().await?.json()?),
        value => Ok(value.json()?),
    }
}

/// What the face `name` answers `method` to its documented `{}` request.
async fn wire<T: DeserializeOwned>(root: &Ctx, name: &str, method: &str) -> T {
    let reply = call(&face(root, name), method, json!({}))
        .await
        .unwrap_or_else(|error| panic!("{name}.{method}: {error}"));
    serde_json::from_value(reply).unwrap_or_else(|error| panic!("{name}.{method}: {error}"))
}

#[tokio::test]
async fn the_execution_switches_answer_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    let switches = ExecutionSettings {
        sandbox: true,
        network: false,
    };
    dim_agent_capabilities::execution_settings::provide(
        &root,
        &ExecutionSettingsHost::fixed(switches),
    )
    .unwrap();
    let binding = root.get::<ExecutionSettingsHost>().unwrap();
    let cancel = CancellationToken::new();
    assert_eq!(
        binding.read(cancel.clone()).await.unwrap(),
        wire::<ExecutionSettings>(&root, "executionSettingsHost", "read").await
    );
    assert_eq!(
        wire::<ExecutionSettings>(&root, "executionSettingsHost", "read").await,
        switches
    );
}

#[tokio::test]
async fn the_installed_runtimes_answer_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    let installed = vec![
        InstalledToolchain {
            bin: "/home/runtimes/node-v22/bin".into(),
            root: "/home/runtimes/node-v22".into(),
        },
        InstalledToolchain {
            bin: "/home/runtimes/python/bin".into(),
            root: "/home/runtimes/python".into(),
        },
    ];
    dim_agent_capabilities::toolchain::provide(&root, &ToolchainSourceHost::fixed(installed))
        .unwrap();
    let binding = root.get::<ToolchainSourceHost>().unwrap();
    let listed = wire::<ToolchainSourceReply>(&root, "toolchainSourceHost", "list").await;
    assert_eq!(binding.list().unwrap(), listed);
    assert_eq!(
        listed,
        ToolchainSourceReply {
            toolchains: vec![
                ToolchainView {
                    bin: "/home/runtimes/node-v22/bin".into(),
                    root: "/home/runtimes/node-v22".into(),
                },
                ToolchainView {
                    bin: "/home/runtimes/python/bin".into(),
                    root: "/home/runtimes/python".into(),
                },
            ],
        }
    );
}

#[tokio::test]
async fn a_runtime_whose_path_cannot_cross_is_left_out_and_the_others_still_list() {
    use std::os::unix::ffi::OsStringExt;
    let root = Ctx::root().unwrap();
    let odd = std::ffi::OsString::from_vec(vec![b'/', b'r', 0xff, b'n']);
    let installed = vec![
        InstalledToolchain {
            bin: "/home/runtimes/node/bin".into(),
            root: "/home/runtimes/node".into(),
        },
        InstalledToolchain {
            bin: odd.into(),
            root: "/home/runtimes/odd".into(),
        },
    ];
    dim_agent_capabilities::toolchain::provide(&root, &ToolchainSourceHost::fixed(installed))
        .unwrap();
    let listed = root.get::<ToolchainSourceHost>().unwrap().list().unwrap();
    assert_eq!(
        listed.toolchains,
        [ToolchainView {
            bin: "/home/runtimes/node/bin".into(),
            root: "/home/runtimes/node".into(),
        }],
        "one unrepresentable entry does not remove the runtimes a child can use"
    );
}

#[tokio::test]
async fn the_prompt_suggestion_document_answers_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    let document = PromptSuggestionSettings {
        enabled: true,
        max_chars: 120,
    };
    let read =
        PromptSuggestionSettingsHost::new(Arc::new(move |_| Box::pin(async move { Ok(document) })));
    dim_agent_capabilities::prompt_suggestion::provide_settings(&root, &read).unwrap();
    let binding = root.get::<PromptSuggestionSettingsHost>().unwrap();
    let over_the_wire =
        wire::<PromptSuggestionSettings>(&root, "promptSuggestionSettingsHost", "read").await;
    assert_eq!(
        binding.read(CancellationToken::new()).await.unwrap(),
        over_the_wire
    );
    assert_eq!(
        over_the_wire, document,
        "the document keeps its Settings spelling"
    );
}

#[tokio::test]
async fn the_selectable_models_answer_their_documented_empty_request() {
    let root = Ctx::root().unwrap();
    let declared = || ModalityModelSnapshot {
        target: ModelId {
            provider_id: "provider".into(),
            model_id: "media".into(),
        },
        operations: vec!["image.generate".into()],
    };
    let selectable = SelectableModelsHost::new(
        Arc::new(|_| Box::pin(async { Ok(Vec::new()) })),
        Arc::new(move |_| Box::pin(async move { Ok(vec![declared()]) })),
    );
    dim_agent_capabilities::host::provide_selectable_models(&root, &selectable).unwrap();
    let binding = root.get::<SelectableModelsHost>().unwrap();
    let empty =
        wire::<SelectableModelsReply>(&root, "selectableModelsHost", "selectableModels").await;
    assert!(empty.models.is_empty());
    assert_eq!(
        binding
            .selectable_models(CancellationToken::new())
            .await
            .unwrap(),
        empty
    );
    let modality =
        wire::<ModalityModelsReply>(&root, "selectableModelsHost", "selectableForModality").await;
    assert_eq!(
        binding
            .selectable_for_modality(CancellationToken::new())
            .await
            .unwrap(),
        modality
    );
    assert_eq!(modality.models, vec![declared()]);
    // The request object is the contract: a wrong one is the caller's error.
    let refused = call(
        &face(&root, "selectableModelsHost"),
        "selectableModels",
        json!({"unexpected": true}),
    )
    .await
    .expect_err("an unknown request field is refused");
    assert_eq!(refused.code(), Some("invalid_request"));
}

#[tokio::test]
async fn the_model_selection_answers_the_same_through_the_binding_and_the_face() {
    let root = Ctx::root().unwrap();
    let selection = ModelSelectionHost::new(Arc::new(|explicit, _, _| {
        Box::pin(async move {
            let id = explicit.unwrap_or(ModelId {
                provider_id: "provider".into(),
                model_id: "default".into(),
            });
            Ok(ModelSelection {
                provider_id: id.provider_id,
                model_id: id.model_id,
                capability_snapshot: None,
                config: None,
                metadata: None,
            })
        })
    }));
    dim_agent_capabilities::host::provide_model_selection(&root, &selection).unwrap();
    let binding = root.get::<ModelSelectionHost>().unwrap();
    let request = ModelSelectionRequest::at(
        Some(ModelId {
            provider_id: "provider".into(),
            model_id: "chosen".into(),
        }),
        Some(std::path::Path::new("/work")),
    )
    .unwrap();
    let reply = call(
        &face(&root, "modelSelectionHost"),
        "resolve",
        serde_json::to_value(&request).unwrap(),
    )
    .await
    .unwrap();
    let selected: ModelSelection = serde_json::from_value(reply).unwrap();
    assert_eq!(
        binding
            .resolve(request, CancellationToken::new())
            .await
            .unwrap(),
        selected
    );
    assert_eq!(selected.model_id, "chosen");
}

#[tokio::test]
async fn a_host_failure_keeps_the_code_its_caller_acts_on() {
    let root = Ctx::root().unwrap();
    let selectable = SelectableModelsHost::new(
        Arc::new(|_| Box::pin(async { Err(HostError::Cancelled("the owner closed".into())) })),
        Arc::new(|_| Box::pin(async { Err(HostError::failed("storage")) })),
    );
    dim_agent_capabilities::host::provide_selectable_models(&root, &selectable).unwrap();
    let cancelled = call(
        &face(&root, "selectableModelsHost"),
        "selectableModels",
        json!({}),
    )
    .await
    .expect_err("the owner fails");
    assert_eq!(cancelled.code(), Some("cancelled"));
    let failed = call(
        &face(&root, "selectableModelsHost"),
        "selectableForModality",
        json!({}),
    )
    .await
    .expect_err("the owner fails");
    assert_eq!(failed.code(), Some("failed"));
    let binding = root.get::<SelectableModelsHost>().unwrap();
    let cancelled = binding.selectable_models(CancellationToken::new()).await;
    assert!(
        matches!(cancelled, Err(ModelPortError::Cancelled { .. })),
        "{cancelled:?}"
    );
    let failed = binding
        .selectable_for_modality(CancellationToken::new())
        .await;
    assert!(
        matches!(failed, Err(ModelPortError::Failed { .. })),
        "{failed:?}"
    );
}

/// A switch read that runs on its own worker, as an owner's catalog read does:
/// the worker holds the call's token, so it observes a cancellation that drops
/// the call's future.
fn watched(
    started: Arc<tokio::sync::Notify>,
    cancelled: Arc<Mutex<Option<bool>>>,
    settled: Arc<tokio::sync::Notify>,
) -> ExecutionSettingsHost {
    ExecutionSettingsHost::new(Arc::new(move |cancel: CancellationToken| {
        let started = started.clone();
        let cancelled = cancelled.clone();
        let settled = settled.clone();
        Box::pin(async move {
            let worker = cancel.clone();
            tokio::spawn(async move {
                worker.cancelled().await;
                *cancelled.lock().unwrap() = Some(true);
                settled.notify_waiters();
            });
            started.notify_waiters();
            std::future::pending::<()>().await;
            unreachable!("the call's own future never answers")
        })
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abandoned_wait_cancels_the_owner_read() {
    let root = Ctx::root().unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let cancelled = Arc::new(Mutex::new(None));
    let settled = Arc::new(tokio::sync::Notify::new());
    dim_agent_capabilities::execution_settings::provide(
        &root,
        &watched(started.clone(), cancelled.clone(), settled.clone()),
    )
    .unwrap();
    let reply = face(&root, "executionSettingsHost")
        .invoke("read", RpcValue::List(vec![json!({}).into()]))
        .unwrap();
    let RpcValue::Reference(wait) = reply else {
        panic!("the face answers a future");
    };
    let entered = started.notified();
    let held = tokio::spawn(async move {
        let _ = wait.wait_async().await;
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered)
        .await
        .expect("the owner's read starts once its wait does");
    held.abort();
    let _ = held.await;
    tokio::time::timeout(std::time::Duration::from_secs(5), settled.notified())
        .await
        .expect("dropping the wait cancels the owner's read");
    assert_eq!(*cancelled.lock().unwrap(), Some(true));
}

#[tokio::test]
async fn a_cancelled_binding_call_does_not_reach_the_owner() {
    let root = Ctx::root().unwrap();
    let entered = Arc::new(Mutex::new(false));
    let seen = entered.clone();
    let settings = ExecutionSettingsHost::new(Arc::new(move |_| {
        *seen.lock().unwrap() = true;
        Box::pin(async move { Ok(ExecutionSettings::default()) })
    }));
    dim_agent_capabilities::execution_settings::provide(&root, &settings).unwrap();
    let binding = root.get::<ExecutionSettingsHost>().unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(binding.read(cancel).await.is_err());
    assert!(!*entered.lock().unwrap(), "a cancelled call reads nothing");
}

#[test]
fn the_port_errors_cross_as_code_and_message() {
    let cancelled = serde_json::to_value(ModelPortError::Cancelled {
        message: "stop".into(),
    })
    .unwrap();
    assert_eq!(cancelled, json!({"code": "cancelled", "message": "stop"}));
    let closed = serde_json::to_value(ModelPortError::OwnerClosed {
        message: "reload".into(),
    })
    .unwrap();
    assert_eq!(closed, json!({"code": "owner_closed", "message": "reload"}));
    let failed = serde_json::to_value(ExecutionSettingsError::Failed {
        message: "no document".into(),
    })
    .unwrap();
    assert_eq!(failed, json!({"code": "failed", "message": "no document"}));
    let failed = serde_json::to_value(PromptSuggestionSettingsError::Failed {
        message: "no document".into(),
    })
    .unwrap();
    assert_eq!(failed, json!({"code": "failed", "message": "no document"}));
}
