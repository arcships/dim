use dim_app_server::model_catalog::{BundledCatalog, ModelEdit, ModelSource, ProviderProjection};
use serde_json::json;

#[test]
fn bundled_lookup_preserves_explicit_asymmetric_limits_and_hidden_metadata() {
    let catalog = BundledCatalog::load().unwrap();
    let provider = ProviderProjection {
        provider_id: "my-gateway".into(),
        source: Some(ModelSource::Custom),
        ..Default::default()
    };
    let entry = catalog
        .project(
            &provider,
            &json!({
                "id": "gpt-5-nano", "limit": { "context": 777 },
                "raw": { "vendorFlag": { "nested": true } }
            }),
        )
        .unwrap();

    assert_eq!(entry.capabilities.context_window, Some(777));
    assert_eq!(entry.capabilities.max_output_tokens, Some(128_000));
    assert!(entry.metadata.as_ref().unwrap().pricing.is_some());
    assert_eq!(
        entry.metadata.as_ref().unwrap().raw["vendorFlag"]["nested"],
        true
    );
    assert_eq!(
        entry.metadata.as_ref().unwrap().match_strategy.as_deref(),
        Some("openai-exact")
    );
}

#[test]
fn a_bundled_match_declares_both_limits_and_an_unmatched_descriptor_has_none() {
    let catalog = BundledCatalog::load().unwrap();
    let provider = ProviderProjection {
        provider_id: "custom".into(),
        source: Some(ModelSource::Custom),
        ..Default::default()
    };
    let matched = catalog
        .project(&provider, &json!({ "id": "gpt-5-nano" }))
        .unwrap();
    assert_eq!(matched.capabilities.context_window, Some(400_000));
    assert_eq!(matched.capabilities.max_output_tokens, Some(128_000));
    assert!(matched.capabilities.missing_limits().is_empty());
    let id = "zzzz-private-descriptor";
    assert!(catalog.fallback(id).is_none());
    let unmatched = catalog.project(&provider, &json!({ "id": id })).unwrap();
    assert_eq!(
        unmatched.capabilities.missing_limits(),
        ["contextWindow", "maxOutputTokens"]
    );
    let effective = catalog.effective_models(&provider, &[unmatched], &[]);
    assert_eq!(
        effective[0].capabilities.missing_limits(),
        ["contextWindow", "maxOutputTokens"]
    );
}

#[test]
fn effective_override_and_reset_use_the_same_bundled_projection() {
    let catalog = BundledCatalog::load().unwrap();
    let provider = ProviderProjection {
        provider_id: "custom".into(),
        source: Some(ModelSource::Custom),
        ..Default::default()
    };
    let defaults = vec![
        catalog
            .project(&provider, &json!({ "id": "gpt-5-nano" }))
            .unwrap(),
    ];
    let base = catalog.effective_models(&provider, &defaults, &[]);
    let edited = catalog.merge_visible_edit(
        &base[0],
        ModelEdit {
            display_name: Some("Mine".into()),
            ..Default::default()
        },
    );
    let overridden = catalog.effective_models(&provider, &defaults, &[edited]);
    let reset = catalog.effective_models(&provider, &defaults, &[]);

    assert_eq!(overridden[0].display_name, "Mine");
    assert_eq!(reset[0].display_name, "GPT-5 Nano");
    assert_eq!(reset[0].capabilities.max_output_tokens, Some(128_000));
    let mut stored = defaults[0].clone();
    stored.capabilities = Default::default();
    let effective = catalog.effective_models(&provider, &[stored], &[]);
    assert_eq!(effective[0].capabilities.context_window, Some(400_000));
    assert_eq!(effective[0].capabilities.max_output_tokens, Some(128_000));
    assert!(!effective[0].capabilities.vision);
    assert_eq!(effective[0].capabilities.modalities, ["text"]);
}

#[test]
fn normalized_family_lookup_matches_prefixed_ids() {
    let catalog = BundledCatalog::load().unwrap();
    let provider = ProviderProjection {
        provider_id: "custom-provider".into(),
        source: Some(ModelSource::Custom),
        ..Default::default()
    };
    let entry = catalog
        .project(
            &provider,
            &json!({
                "id": "models/vendor/DeepSeek_V4_Flash"
            }),
        )
        .unwrap();
    assert_eq!(
        entry
            .metadata
            .as_ref()
            .unwrap()
            .matched_provider_id
            .as_deref(),
        Some("custom-provider")
    );
    let official = catalog
        .project(
            &provider,
            &json!({
                "id": "models/vendor/GPT_5_Nano"
            }),
        )
        .unwrap();
    assert_eq!(
        official
            .metadata
            .as_ref()
            .unwrap()
            .matched_provider_id
            .as_deref(),
        Some("openai")
    );
    assert_eq!(official.capabilities.max_output_tokens, Some(128000));
    assert!(catalog.project(&provider, &json!(null)).is_err());
}

#[test]
fn media_lookup_and_explicit_capabilities_survive_editing() {
    let catalog = BundledCatalog::load().unwrap();
    let provider = ProviderProjection {
        provider_id: "custom".into(),
        source: Some(ModelSource::Custom),
        ..Default::default()
    };
    let media = catalog
        .project(
            &provider,
            &json!({
                "id": "eleven_multilingual_v2"
            }),
        )
        .unwrap();
    assert_eq!(
        media
            .metadata
            .as_ref()
            .unwrap()
            .matched_provider_id
            .as_deref(),
        Some("elevenlabs")
    );
    assert_eq!(
        media.metadata.as_ref().unwrap().raw["modalityCapabilities"],
        json!(["speech.synthesize"])
    );
    let model = catalog
        .project(
            &provider,
            &json!({
                "id": "gpt-5-nano", "limit": {"context": 128000, "output": 16384},
                "vision": true, "modalities": ["text"],
                "capabilities": {"streaming": false, "providerState": {"custom": true}},
                "reasoning": {"supported": false, "effortOptions": ["off"]}
            }),
        )
        .unwrap();
    assert_eq!(model.capabilities.context_window, Some(128000));
    assert_eq!(model.capabilities.max_output_tokens, Some(16384));
    assert_eq!(model.capabilities.modalities, ["text"]);
    assert!(!model.capabilities.streaming);
    assert!(!model.capabilities.reasoning);
    assert_eq!(
        model.capabilities.provider_state.as_ref().unwrap()["custom"],
        true
    );
    let nested_limits = catalog
        .project(
            &provider,
            &json!({
                "id": "gpt-5-nano",
                "capabilities": {"contextWindow": 128000, "maxOutputTokens": 16384}
            }),
        )
        .unwrap();
    assert_eq!(nested_limits.capabilities.context_window, Some(128000));
    assert_eq!(nested_limits.capabilities.max_output_tokens, Some(16384));
    let effective = catalog.effective_models(&provider, &[nested_limits], &[]);
    assert_eq!(effective[0].capabilities.context_window, Some(128000));
    assert_eq!(effective[0].capabilities.max_output_tokens, Some(16384));
    assert_eq!(
        model
            .metadata
            .as_ref()
            .unwrap()
            .reasoning
            .as_ref()
            .unwrap()
            .effort_options
            .as_ref()
            .unwrap(),
        &["off"]
    );
    let edited = catalog.merge_visible_edit(
        &model,
        ModelEdit {
            display_name: Some("Custom".into()),
            ..Default::default()
        },
    );
    assert_eq!(edited.metadata, model.metadata);
}

#[test]
fn visible_edits_preserve_hidden_fields_and_update_legacy_media_readers() {
    let catalog = BundledCatalog::load().unwrap();
    let provider = ProviderProjection {
        provider_id: "custom".into(),
        ..Default::default()
    };
    let model = catalog.project(&provider, &json!({
        "id": "gpt-5-nano", "reasoning": {"supported": true, "mode": "effort", "defaultEnabled": true, "effort": "high"},
        "serviceTier": {"supported": true, "default": "priority", "options": ["priority"]},
        "raw": {"imageGeneration": false, "private": {"value": 73}}
    })).unwrap();
    let edit = serde_json::from_value(json!({
        "reasoning": {"supported": false, "effortOptions": ["low", "high"]},
        "modalityCapabilities": ["image.generate"]
    }))
    .unwrap();
    let edited = catalog.merge_visible_edit(&model, edit);
    let metadata = edited.metadata.as_ref().unwrap();
    assert_eq!(metadata.raw["imageGeneration"], true);
    assert_eq!(metadata.raw["private"]["value"], 73);
    assert_eq!(
        metadata.service_tier,
        model.metadata.as_ref().unwrap().service_tier
    );
    let reasoning = metadata.reasoning.as_ref().unwrap();
    assert!(!reasoning.supported);
    assert!(reasoning.effort_options.is_none());
    assert_eq!(reasoning.mode.as_deref(), Some("effort"));
    assert_eq!(reasoning.effort.as_deref(), Some("high"));
    let edited = catalog.merge_visible_edit(
        &edited,
        serde_json::from_value(json!({"modalityCapabilities": []})).unwrap(),
    );
    assert_eq!(
        edited.metadata.as_ref().unwrap().raw["imageGeneration"],
        false
    );
    for field in [
        "status",
        "streaming",
        "structuredOutput",
        "audio",
        "serviceTier",
    ] {
        assert!(
            serde_json::from_value::<ModelEdit>(json!({field: null})).is_err(),
            "{field} must not be editable"
        );
    }
}

/// The listings of some OpenAI-compatible servers name the limits under
/// other keys; each model keeps what its server states.
#[test]
fn listing_keys_of_other_servers_declare_the_limits_they_state() {
    let catalog = BundledCatalog::load().unwrap();
    let provider = ProviderProjection {
        provider_id: "my-gateway".into(),
        source: Some(ModelSource::Custom),
        ..Default::default()
    };
    let openrouter = catalog
        .project(
            &provider,
            &json!({
                "id": "zzzz-openrouter-model", "context_length": 200_000,
                "top_provider": {"max_completion_tokens": 32_000}
            }),
        )
        .unwrap();
    assert_eq!(openrouter.capabilities.context_window, Some(200_000));
    assert_eq!(openrouter.capabilities.max_output_tokens, Some(32_000));
    let vllm = catalog
        .project(
            &provider,
            &json!({"id": "zzzz-vllm-model", "max_model_len": 32_768}),
        )
        .unwrap();
    assert_eq!(vllm.capabilities.context_window, Some(32_768));
    assert_eq!(vllm.capabilities.missing_limits(), ["maxOutputTokens"]);
    let lmstudio = catalog
        .project(
            &provider,
            &json!({"id": "zzzz-lmstudio-model", "max_context_length": 131_072}),
        )
        .unwrap();
    assert_eq!(lmstudio.capabilities.context_window, Some(131_072));
    assert_eq!(lmstudio.capabilities.missing_limits(), ["maxOutputTokens"]);
}

/// A shipped model that declares a `realtime.*` capability also declares the
/// transport its adapter speaks: an operation without one could never open a
/// session, and `modality.capabilities` would offer it anyway.
#[test]
fn every_shipped_realtime_capability_declares_its_transport() {
    use dim_agent_capabilities::host::RealtimeDeclaration;
    let catalog = BundledCatalog::load().unwrap();
    let mut declared = Vec::new();
    for provider in catalog.providers() {
        for model in &provider.models {
            let Some(raw) = model.metadata.as_ref().map(|metadata| &metadata.raw) else {
                continue;
            };
            let operations = raw
                .get("modalityCapabilities")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default();
            if !operations
                .iter()
                .any(|id| id.as_str().is_some_and(|id| id.starts_with("realtime.")))
            {
                continue;
            }
            let transport: RealtimeDeclaration =
                serde_json::from_value(raw.get("realtime").cloned().unwrap_or_else(|| {
                    panic!("{} declares no realtime transport", model.model_id)
                }))
                .unwrap_or_else(|error| {
                    panic!(
                        "{} declares an invalid realtime transport: {error}",
                        model.model_id
                    )
                });
            declared.push((model.model_id.clone(), transport));
        }
    }
    assert_eq!(
        declared,
        [(
            "stepaudio-2.5-realtime".to_owned(),
            RealtimeDeclaration {
                protocol: "openai-realtime".into(),
                path: "/realtime".into(),
            }
        )]
    );
}

#[test]
fn every_bundled_provider_states_whether_it_serves_or_resells() {
    use dim_app_server::model_catalog::ProviderRole;
    let catalog = BundledCatalog::load().unwrap();
    let mut sellers = Vec::new();
    let mut resellers = Vec::new();
    for provider in catalog.providers() {
        let role = catalog.role(&provider.provider_id);
        if provider.models.is_empty() {
            assert_eq!(role, None, "{} lists no models", provider.provider_id);
            continue;
        }
        match role {
            Some(ProviderRole::Vendor) => sellers.push(provider.provider_id.as_str()),
            Some(ProviderRole::Aggregator) => resellers.push(provider.provider_id.as_str()),
            None => panic!("{} lists models but no role", provider.provider_id),
        }
    }
    assert!(sellers.contains(&"openai"));
    assert!(resellers.contains(&"openrouter"));
    assert_eq!(catalog.role("not-a-provider"), None);
}

#[test]
fn a_plan_provider_is_marked_and_its_api_entry_is_not() {
    let catalog = BundledCatalog::load().unwrap();
    assert_eq!(catalog.is_plan("openai-codex"), Some(true));
    assert_eq!(catalog.is_plan("openai"), Some(false));
    assert_eq!(catalog.is_plan("not-a-provider"), None);
    let plans: Vec<_> = catalog
        .providers()
        .filter(|provider| catalog.is_plan(&provider.provider_id) == Some(true))
        .map(|provider| provider.provider_id.as_str())
        .collect();
    assert_eq!(plans.len(), 27, "{plans:?}");
    for api in ["openai", "moonshot", "zai", "deepseek", "minimax"] {
        assert_eq!(catalog.is_plan(api), Some(false), "{api}");
    }
}
