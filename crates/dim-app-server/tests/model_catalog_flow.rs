use dim_app_server::model_catalog::{
    BundledCatalog, ModelEdit, ModelEntry, ModelSource, ProviderProjection, ProviderRole,
};
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
    // A plain id that the vendor declares resolves at the vendor: the id is
    // how the model is spelled, not a different model.
    assert_eq!(
        entry
            .metadata
            .as_ref()
            .unwrap()
            .matched_provider_id
            .as_deref(),
        Some("deepseek")
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

/// Projects one id as a relay outside the catalog, which is the projection a
/// gateway that ships no bundled list has.
fn relay(catalog: &BundledCatalog, id: &str) -> ModelEntry {
    let provider = ProviderProjection {
        provider_id: "my-gateway".into(),
        source: Some(ModelSource::Custom),
        ..Default::default()
    };
    catalog.project(&provider, &json!({ "id": id })).unwrap()
}

/// Projects one id through a builtin provider's own bundled list.
fn builtin(catalog: &BundledCatalog, provider_id: &str, id: &str) -> ModelEntry {
    let provider = ProviderProjection {
        provider_id: provider_id.into(),
        source: Some(ModelSource::Builtin),
        models: catalog
            .bundled_models(provider_id)
            .unwrap_or_default()
            .to_vec(),
        ..Default::default()
    };
    catalog.project(&provider, &json!({ "id": id })).unwrap()
}

fn matched(entry: &ModelEntry) -> (&str, &str) {
    let metadata = entry.metadata.as_ref().unwrap();
    (
        metadata
            .matched_provider_id
            .as_deref()
            .unwrap_or("provider-default"),
        metadata.matched_model_id.as_deref().unwrap_or("-"),
    )
}

fn numbers(entry: &ModelEntry) -> (Option<u64>, Option<u64>) {
    (
        entry.capabilities.context_window,
        entry.capabilities.max_output_tokens,
    )
}

/// The rows outside the vendor and `custom-provider` lists that carry `id`
/// exactly, as `(provider, window, output, placeholder)`. A provider whose pair
/// is its stated default is flagged, because it declares nothing.
fn seller_numbers(catalog: &BundledCatalog, id: &str) -> Vec<(String, u64, u64, bool)> {
    let placeholders = [("tokenflux", 4_096, 4_096), ("aihubmix", 8_192, 8_192)];
    catalog
        .providers()
        .filter(|provider| {
            !matches!(
                catalog.role(&provider.provider_id),
                Some(ProviderRole::Vendor)
            ) && provider.provider_id != "custom-provider"
        })
        .flat_map(|provider| {
            provider
                .models
                .iter()
                .filter(|model| model.model_id == id)
                .filter_map(|model| {
                    Some((
                        provider.provider_id.clone(),
                        model.capabilities.context_window?,
                        model.capabilities.max_output_tokens?,
                        placeholders.iter().any(|(pid, w, o)| {
                            *pid == provider.provider_id
                                && model.capabilities.context_window == Some(*w)
                                && model.capabilities.max_output_tokens == Some(*o)
                        }),
                    ))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The effort lists the rows outside the vendor and `custom-provider` lists
/// declare for `id`.
fn declared_efforts(catalog: &BundledCatalog, id: &str) -> Vec<Vec<String>> {
    catalog
        .providers()
        .filter(|provider| {
            !matches!(
                catalog.role(&provider.provider_id),
                Some(ProviderRole::Vendor)
            ) && provider.provider_id != "custom-provider"
        })
        .flat_map(|provider| {
            provider
                .models
                .iter()
                .filter(|model| model.model_id == id)
                .filter_map(|model| {
                    model
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.reasoning.as_ref())
                        .and_then(|reasoning| reasoning.effort_options.clone())
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The ids #1519's cases name, answered by the first four steps: the vendor's
/// own list, its tokens, its canonical id and display name, and the
/// `custom-provider` declaration. Each expected window and output cap is read
/// from the bundled row of the provider named beside it, so a catalog change
/// moves the expectation with it.
#[test]
fn an_id_is_identified_before_its_numbers_are_read() {
    let catalog = BundledCatalog::load().unwrap();
    for (id, provider, model) in [
        ("deepseek-flash", "deepseek", "deepseek-flash"),
        ("kimi-k3", "moonshot", "kimi-k3"),
        ("kimi-k3:batch", "moonshot", "kimi-k3"),
        ("glm-5.2", "zai", "glm-5.2"),
        ("gemini-3.1-pro-preview", "google", "gemini-3.1-pro-preview"),
        ("gpt-5.5", "openai", "gpt-5.5"),
        ("kimi-k2.7-code", "moonshot", "kimi-k2.7-code"),
        ("MiniMax-M3", "minimax", "MiniMax-M3"),
        ("kimi-k3-20250929", "moonshot", "kimi-k3"),
        ("claude-opus-5-5-20260101", "anthropic", "claude-opus-5-5"),
    ] {
        let entry = relay(&catalog, id);
        assert_eq!(matched(&entry), (provider, model), "{id}");
        let row = catalog
            .bundled_models(provider)
            .unwrap()
            .iter()
            .find(|row| row.model_id == model)
            .unwrap_or_else(|| panic!("{id}: {provider} lists no {model}"));
        let expected = (
            row.capabilities.context_window,
            row.capabilities.max_output_tokens,
        );
        assert!(
            expected.0.is_some() && expected.1.is_some(),
            "{id}: {provider} declares no limit for {model}"
        );
        assert_eq!(numbers(&entry), expected, "{id}");
    }
    let declared = relay(&catalog, "kimi-k2.5");
    assert_eq!(matched(&declared), ("custom-provider", "kimi-k2.5"));
    assert_eq!(numbers(&declared), (Some(262_144), Some(262_144)));
    assert_eq!(
        declared
            .metadata
            .as_ref()
            .unwrap()
            .match_strategy
            .as_deref(),
        Some("custom-provider")
    );
}

/// The DeepSeek case: three vendor rows share one canonical id, and only the
/// display name picks between them, so the narrowed rule reports one row.
#[test]
fn a_canonical_id_is_narrowed_by_the_display_name() {
    let catalog = BundledCatalog::load().unwrap();
    let entry = relay(&catalog, "deepseek-v4.1-flash");
    assert_eq!(matched(&entry), ("deepseek", "deepseek-flash"));
    assert_eq!(numbers(&entry), (Some(1_000_000), Some(384_000)));
    assert_eq!(
        entry.metadata.as_ref().unwrap().match_strategy.as_deref(),
        Some("deepseek-canonical")
    );
    let efforts = entry
        .metadata
        .as_ref()
        .unwrap()
        .reasoning
        .as_ref()
        .and_then(|reasoning| reasoning.effort_options.clone())
        .unwrap();
    assert_eq!(efforts, ["low", "high", "max"]);
}

/// An API entry beats the plan that the same vendor sells beside it, and on
/// the plan's own provider the plan's own numbers still win.
#[test]
fn an_api_entry_beats_a_plan_entry() {
    let catalog = BundledCatalog::load().unwrap();
    let on_a_relay = relay(&catalog, "gpt-5.5");
    assert_eq!(matched(&on_a_relay), ("openai", "gpt-5.5"));
    assert_eq!(numbers(&on_a_relay).0, Some(1_050_000));
    let on_the_plan = builtin(&catalog, "openai-codex", "gpt-5.5");
    assert_eq!(matched(&on_the_plan), ("openai-codex", "gpt-5.5"));
    assert_eq!(numbers(&on_the_plan).0, Some(258_400));
    let kimi = relay(&catalog, "kimi-k2.7-code");
    assert_eq!(matched(&kimi), ("moonshot", "kimi-k2.7-code"));
    assert_eq!(numbers(&kimi).1, Some(262_144));
}

/// A row that names a different model is not an answer for this one, so the
/// id keeps the projection's own default rather than its family base.
#[test]
fn a_variant_is_not_its_family_base() {
    let catalog = BundledCatalog::load().unwrap();
    let entry = relay(&catalog, "gpt-5-codex");
    assert_ne!(matched(&entry), ("openai", "gpt-5"));
    assert_ne!(numbers(&entry), (Some(400_000), Some(128_000)));
}

/// Several rows claim one id, declare different numbers, and no API entry
/// exists between them, so none of them may supply numbers.
#[test]
fn two_vendors_claiming_one_id_report_ambiguity() {
    let catalog = BundledCatalog::load().unwrap();
    let entry = relay(&catalog, "ark-code-latest");
    assert_eq!(matched(&entry), ("provider-default", "-"));
    assert_eq!(
        entry.capabilities.missing_limits(),
        ["contextWindow", "maxOutputTokens"]
    );
    let row = catalog
        .bundled_models("ark-code")
        .unwrap()
        .iter()
        .find(|model| model.model_id == "ark-code-latest")
        .unwrap();
    assert_eq!(row.capabilities.context_window, Some(204_800));
}

/// Two providers selling one model under one name with one set of numbers are
/// one model, not a conflict, and the API entry beside them settles it.
#[test]
fn two_rows_with_one_set_of_numbers_are_one_model() {
    let catalog = BundledCatalog::load().unwrap();
    let entry = relay(&catalog, "kimi-k3");
    assert_eq!(matched(&entry).1, "kimi-k3");
    let moonshot = catalog
        .bundled_models("moonshot")
        .unwrap()
        .iter()
        .find(|model| model.model_id == "kimi-k3")
        .unwrap();
    let mirror = catalog
        .bundled_models("moonshot-ai")
        .unwrap()
        .iter()
        .find(|model| model.model_id == "kimi-k3")
        .unwrap();
    assert_eq!(
        (
            moonshot.capabilities.context_window,
            moonshot.capabilities.max_output_tokens
        ),
        (
            mirror.capabilities.context_window,
            mirror.capabilities.max_output_tokens
        )
    );
}

/// A plan's own provider keeps the plan's numbers even when the vendor's API
/// entry disagrees, since the provider answers for the subscription it sells.
#[test]
fn a_plan_keeps_its_own_numbers_on_its_own_provider() {
    let catalog = BundledCatalog::load().unwrap();
    let plan = builtin(&catalog, "openai-codex", "gpt-6.1-sol");
    assert_eq!(numbers(&plan).0, Some(258_400));
    let api = catalog
        .bundled_models("openai")
        .unwrap()
        .iter()
        .find(|model| model.model_id == "gpt-6.1-sol")
        .unwrap();
    assert_eq!(api.capabilities.context_window, Some(1_050_000));
}

/// A provider's stored list can lag the bundle by a model. When it does, the
/// provider still answers for that model from its own family before any other
/// seller does: `openai-codex` keeps its own window for `gpt-5.5` rather than
/// the vendor API entry the plan rule would otherwise prefer.
#[test]
fn a_providers_own_family_answers_before_another_seller() {
    let catalog = BundledCatalog::load().unwrap();
    let provider = ProviderProjection {
        provider_id: "openai-codex".into(),
        source: Some(ModelSource::Builtin),
        models: catalog
            .bundled_models("openai-codex")
            .unwrap()
            .iter()
            .filter(|model| model.model_id != "gpt-5.5")
            .cloned()
            .collect(),
        ..Default::default()
    };
    let entry = catalog
        .project(&provider, &json!({ "id": "gpt-5.5" }))
        .unwrap();
    assert_eq!(matched(&entry), ("openai-codex", "gpt-5.5"));
    assert_eq!(numbers(&entry).0, Some(258_400));
}

/// A vendor row is an answer where an aggregator row alone is not: the merge
/// that would combine them arrives later, so a vendor's declaration wins now.
#[test]
fn a_plain_id_identifies_at_its_vendor() {
    let catalog = BundledCatalog::load().unwrap();
    let entry = relay(&catalog, "glm-5.2");
    assert_eq!(matched(&entry), ("zai", "glm-5.2"));
    let seller = catalog
        .bundled_models("custom-provider")
        .unwrap()
        .iter()
        .find(|model| model.model_id == "glm-5.2")
        .unwrap();
    assert_ne!(
        seller
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.reasoning.as_ref())
            .and_then(|reasoning| reasoning.effort_options.clone()),
        catalog
            .bundled_models("zai")
            .unwrap()
            .iter()
            .find(|model| model.model_id == "glm-5.2")
            .and_then(|model| model.metadata.as_ref())
            .and_then(|metadata| metadata.reasoning.as_ref())
            .and_then(|reasoning| reasoning.effort_options.clone())
    );
}

/// A vendor the id names in its namespace segment wins over another vendor
/// selling the same model, so `ark-code/ark-code-latest` answers with
/// `ark-code` while the bare id stays ambiguous.
#[test]
fn a_namespaced_id_identifies_the_vendor_it_names() {
    let catalog = BundledCatalog::load().unwrap();
    let entry = relay(&catalog, "ark-code/ark-code-latest");
    assert_eq!(matched(&entry), ("ark-code", "ark-code-latest"));
    assert_eq!(numbers(&entry), (Some(204_800), Some(60_000)));
    let bare = relay(&catalog, "ark-code-latest");
    assert_eq!(matched(&bare), ("provider-default", "-"));
}

/// Step 4 reads the `custom-provider` rows by the same token and name rules,
/// so a model that only that list carries resolves to a declared row and is
/// not left for the merge.
#[test]
fn a_custom_provider_row_is_a_declaration() {
    let catalog = BundledCatalog::load().unwrap();
    for (input, model) in [
        ("qwen3.8-omni-flash", "qwen3.8-omni-flash"),
        ("Qwen3.8 Omni Flash", "qwen3.8-omni-flash"),
    ] {
        let entry = relay(&catalog, input);
        assert_eq!(matched(&entry), ("custom-provider", model), "{input}");
        assert_eq!(
            entry.metadata.as_ref().unwrap().match_strategy.as_deref(),
            Some("custom-provider"),
            "{input}"
        );
        assert!(entry.capabilities.missing_limits().is_empty(), "{input}");
    }
}

/// Step 1 answers an aggregator's own house model on that aggregator's own
/// provider: `openrouter` lists `openrouter/fusion` itself, so the row's own
/// limits stand and no vendor row is consulted. The same id through a relay is
/// identified by the vendor that names `fusion` and refused; that is the
/// relay-only loss, not a broken step 1.
#[test]
fn an_aggregators_house_model_answers_on_its_own_provider() {
    let catalog = BundledCatalog::load().unwrap();
    let entry = builtin(&catalog, "openrouter", "openrouter/fusion");
    assert_eq!(matched(&entry), ("openrouter", "openrouter/fusion"));
    assert_eq!(numbers(&entry), (Some(8_192), Some(8_192)));
    assert_eq!(
        entry.metadata.as_ref().unwrap().match_strategy.as_deref(),
        Some("provider-exact")
    );
    assert_eq!(
        entry
            .metadata
            .as_ref()
            .unwrap()
            .identification
            .as_ref()
            .unwrap()
            .rule,
        "provider-exact"
    );
    assert!(entry.capabilities.missing_limits().is_empty());
}

/// An aggregator's own qualified id is its identity before any step drops the
/// namespace: `openrouter/auto` is OpenRouter's router and not the bare `auto`
/// a vendor sells, and `openrouter/fusion` is its text router and not Rev AI's
/// audio transcription model.
#[test]
fn a_qualified_house_model_keeps_its_own_identity() {
    let catalog = BundledCatalog::load().unwrap();
    for id in ["openrouter/auto", "openrouter/fusion"] {
        let entry = relay(&catalog, id);
        let row = catalog
            .bundled_models("openrouter")
            .unwrap()
            .iter()
            .find(|row| row.model_id == id)
            .unwrap();
        let metadata = entry.metadata.as_ref().unwrap();
        let answered = (
            matched(&entry),
            numbers(&entry),
            entry.capabilities.modalities.clone(),
            entry.capabilities.audio,
            entry.capabilities.tool_calling,
            metadata.identification.as_ref().map(|found| found.rule.as_str()),
            metadata.match_strategy.as_deref(),
        );
        let expected = (
            ("openrouter", id),
            (
                row.capabilities.context_window,
                row.capabilities.max_output_tokens,
            ),
            row.capabilities.modalities.clone(),
            row.capabilities.audio,
            row.capabilities.tool_calling,
            Some("qualified"),
            Some("openrouter-exact"),
        );
        assert_eq!(answered, expected, "{id}: the relay answers with the row itself");
    }
}

/// A namespace that names a known provider binds the id to that provider: when
/// nothing under it names the model, another vendor may not claim the id by its
/// final token, so the refusal names the rows that would have.
#[test]
fn a_namespaced_id_the_namespace_provider_does_not_name_is_refused() {
    let catalog = BundledCatalog::load().unwrap();
    let entry = relay(&catalog, "openai/gpt-image-2.5-flare");
    let identification = entry
        .metadata
        .as_ref()
        .unwrap()
        .identification
        .as_ref()
        .map(|found| found.rule.as_str());
    assert_eq!(
        (matched(&entry), identification),
        (("provider-default", "-"), Some("ambiguous")),
        "the token `flare` is not an answer under the namespace `openai`"
    );
    assert_eq!(
        entry.capabilities.missing_limits(),
        ["contextWindow", "maxOutputTokens"]
    );
    let identification = entry
        .metadata
        .as_ref()
        .unwrap()
        .identification
        .as_ref()
        .unwrap();
    assert_eq!(identification.rule, "ambiguous");
    let named: Vec<_> = identification
        .candidates
        .iter()
        .map(|candidate| (candidate.provider_id.as_str(), candidate.model_id.as_str()))
        .collect();
    assert_eq!(named, [("openai-codex", "gpt-image-2.5-flare")]);
}

/// A merge answers for several sellers at once, so each adopted limit keeps the
/// row it came from: `openai/gpt-oss-20b` takes its window from aihubmix and its
/// output cap from qiniu, while `metadataSource` still names the row the entry
/// is copied from.
#[test]
fn a_merged_limit_names_the_seller_it_came_from() {
    let catalog = BundledCatalog::load().unwrap();
    let entry = relay(&catalog, "openai/gpt-oss-20b");
    let metadata = entry.metadata.as_ref().unwrap();
    let identification = metadata.identification.as_ref().unwrap();
    let filled: Vec<_> = identification
        .filled
        .iter()
        .map(|limit| {
            (
                limit.field.as_str(),
                limit.value,
                limit.provider_id.as_str(),
                limit.model_id.as_str(),
                limit.rule.as_str(),
            )
        })
        .collect();
    assert_eq!(
        (
            numbers(&entry),
            filled,
            metadata.metadata_source.as_deref(),
            metadata.matched_model_id.as_deref()
        ),
        (
            (Some(128_000), Some(4_096)),
            vec![
                (
                    "contextWindow",
                    128_000,
                    "aihubmix",
                    "gpt-oss-20b",
                    "merge"
                ),
                ("maxOutputTokens", 4_096, "qiniu", "gpt-oss-20b", "merge"),
            ],
            Some("openrouter"),
            Some("openai/gpt-oss-20b"),
        ),
        "each adopted minimum names the row that declared it"
    );
    assert_eq!(identification.rule, "merge");
}

/// The same provenance on the list a relay serves: `effective_models` projects
/// every row of a custom provider, and a row that declares neither limit takes
/// both from the merge with the seller of each.
#[test]
fn a_merge_carries_the_sellers_through_the_effective_models() {
    let catalog = BundledCatalog::load().unwrap();
    let row: ModelEntry = serde_json::from_value(json!({
        "modelId": "openai/gpt-oss-20b",
        "displayName": "openai/gpt-oss-20b",
        "status": "active",
        "capabilities": {
            "modalities": ["text"], "toolCalling": true, "streaming": true,
            "structuredOutput": true, "vision": false, "audio": false,
            "reasoning": false
        }
    }))
    .unwrap();
    let provider = ProviderProjection {
        provider_id: "my-gateway".into(),
        source: Some(ModelSource::Custom),
        models: vec![row.clone()],
        ..Default::default()
    };
    let effective = catalog.effective_models(&provider, std::slice::from_ref(&row), &[]);
    let identification = effective[0]
        .metadata
        .as_ref()
        .unwrap()
        .identification
        .as_ref()
        .unwrap();
    let filled: Vec<_> = identification
        .filled
        .iter()
        .map(|limit| (limit.field.as_str(), limit.value, limit.provider_id.as_str()))
        .collect();
    assert_eq!(
        (
            (
                effective[0].capabilities.context_window,
                effective[0].capabilities.max_output_tokens
            ),
            identification.rule.as_str(),
            filled,
        ),
        (
            (Some(128_000), Some(4_096)),
            "merge",
            vec![
                ("contextWindow", 128_000, "aihubmix"),
                ("maxOutputTokens", 4_096, "qiniu"),
            ],
        )
    );
}

/// Every aggregator row that names the same model with the same tag joins, and
/// the merge takes the safe direction: the smallest window and the smallest
/// output cap any joining row declares.
#[test]
fn an_aggregator_only_model_merges_the_minimum() {
    let catalog = BundledCatalog::load().unwrap();
    for (id, window, output) in [
        ("openai/gpt-oss-20b", 128_000, 4_096),
        ("meta/muse-glimmer-30b", 131_072, 16_384),
        ("nvidia/nemotron-3-super-120b-a12b", 256_000, 32_000),
    ] {
        let sellers = seller_numbers(&catalog, id);
        assert!(sellers.len() > 1, "{id}: sellers {sellers:?}");
        let declaring: Vec<_> = sellers.iter().filter(|row| !row.3).collect();
        assert!(
            declaring.iter().any(|row| row.1 != window)
                || declaring.iter().any(|row| row.2 != output),
            "{id}: the minimum must differ from some row, else the test proves nothing"
        );
        for row in &declaring {
            assert!(
                window <= row.1,
                "{id}: window {window} exceeds a row's {}",
                row.1
            );
            assert!(
                output <= row.2,
                "{id}: output {output} exceeds a row's {}",
                row.2
            );
        }
        let entry = relay(&catalog, id);
        assert_eq!(numbers(&entry), (Some(window), Some(output)), "{id}");
        assert_eq!(
            entry
                .metadata
                .as_ref()
                .unwrap()
                .identification
                .as_ref()
                .unwrap()
                .rule,
            "merge",
            "{id}"
        );
        let (seller, answered) = matched(&entry);
        assert_eq!(answered, id, "{id}: the merge answered a row that names it");
        assert!(
            sellers.iter().any(|row| row.0 == seller),
            "{id}: the merge names the seller {seller}, which is not a joining row"
        );
    }
}

/// The efforts are the intersection over the rows that declare any, never the
/// union: an option only some sellers accept is not offered.
#[test]
fn merged_efforts_are_the_intersection() {
    let catalog = BundledCatalog::load().unwrap();
    for (id, expected) in [
        ("inception/mercury-2", ["low", "medium", "high"].as_slice()),
        (
            "meta/muse-glimmer-30b",
            ["low", "medium", "high"].as_slice(),
        ),
        ("fireworks/ember-1", ["low", "high"].as_slice()),
    ] {
        let mut declared = declared_efforts(&catalog, id);
        assert!(declared.len() > 1, "{id}: {declared:?}");
        let union: Vec<String> = {
            let mut all: Vec<String> = declared.iter().flatten().cloned().collect();
            all.sort();
            all.dedup();
            all
        };
        let entry = relay(&catalog, id);
        let mut merged = entry
            .metadata
            .as_ref()
            .unwrap()
            .reasoning
            .as_ref()
            .and_then(|reasoning| reasoning.effort_options.clone())
            .unwrap();
        merged.sort();
        let mut expected = expected.to_vec();
        expected.sort();
        assert_eq!(merged, expected, "{id}");
        assert!(merged.len() < union.len(), "{id}: the union is {union:?}");
        for list in &mut declared {
            list.sort();
            assert!(
                merged.iter().all(|effort| list.contains(effort)),
                "{id}: {merged:?} is not inside {list:?}"
            );
        }
    }
}

/// A row whose own id carries a tag joins only an input carrying that tag: the
/// tagged input merges the tagged row and an untagged input never does.
#[test]
fn a_tagged_row_does_not_join_an_untagged_input() {
    let catalog = BundledCatalog::load().unwrap();
    let tagged = relay(&catalog, "mistralai/codestral-2508:batch");
    let bare = relay(&catalog, "mistralai/codestral-2508");
    assert_ne!(
        numbers(&tagged),
        numbers(&bare),
        "the tagged row and the untagged rows declare different caps"
    );
    assert_eq!(numbers(&tagged), (Some(256_000), Some(204_800)));
    let answered = tagged
        .metadata
        .as_ref()
        .unwrap()
        .matched_model_id
        .clone()
        .unwrap();
    assert_eq!(answered, "mistralai/codestral-2508:batch");

    let mut checked = 0;
    for id in catalog
        .providers()
        .flat_map(|provider| provider.models.iter())
        .map(|model| model.model_id.clone())
        .filter(|id| !id.contains(':'))
        .collect::<Vec<_>>()
    {
        let entry = relay(&catalog, &id);
        let rule = entry
            .metadata
            .as_ref()
            .unwrap()
            .identification
            .as_ref()
            .map(|identification| identification.rule.clone());
        if rule.as_deref() != Some("merge") {
            continue;
        }
        checked += 1;
        let answered = entry.metadata.as_ref().unwrap().matched_model_id.clone();
        assert!(
            answered.as_deref().is_none_or(|row| !row.contains(':')),
            "{id}: an untagged input joined the tagged row {answered:?}"
        );
    }
    assert!(
        checked > 0,
        "no untagged id merged, so the check proved nothing"
    );
}

/// Two rows that declare one set of numbers are one model, so they settle a
/// step instead of reporting an ambiguity even when the seller names differ.
#[test]
fn an_ambiguous_step_collapses_rows_that_agree() {
    let catalog = BundledCatalog::load().unwrap();
    let entry = relay(&catalog, "kimi-k3");
    assert_ne!(
        entry
            .metadata
            .as_ref()
            .unwrap()
            .identification
            .as_ref()
            .unwrap()
            .rule,
        "ambiguous"
    );
}

/// A provider's placeholder pair is its default, not a declaration: the static
/// path that serves it fills the two limits from the entry the id identifies,
/// records where they came from, and leaves the row's own capabilities alone.
#[test]
fn a_placeholder_limit_is_not_a_declaration() {
    let catalog = BundledCatalog::load().unwrap();
    let row = catalog
        .bundled_models("tokenflux")
        .unwrap()
        .iter()
        .find(|model| model.model_id == "openai/gpt-5")
        .expect("tokenflux lists openai/gpt-5")
        .clone();
    assert_eq!(numbers(&row), (Some(4_096), Some(4_096)));
    let provider = ProviderProjection {
        provider_id: "tokenflux".into(),
        source: Some(ModelSource::Builtin),
        models: catalog.bundled_models("tokenflux").unwrap().to_vec(),
        ..Default::default()
    };
    let effective = catalog.effective_models(&provider, std::slice::from_ref(&row), &[]);
    let filled = &effective[0];
    let vendor = catalog
        .bundled_models("openai")
        .unwrap()
        .iter()
        .find(|model| model.model_id == "gpt-5")
        .expect("openai lists gpt-5");
    assert_eq!(numbers(filled), numbers(vendor));
    assert_ne!(numbers(filled), numbers(&row));
    assert!(
        !filled.capabilities.tool_calling,
        "the row keeps its own API"
    );
    let identification = filled
        .metadata
        .as_ref()
        .unwrap()
        .identification
        .as_ref()
        .unwrap();
    assert_eq!(identification.rule, "provider-exact");
    assert_eq!(identification.filled.len(), 2, "{identification:?}");
    assert!(
        identification
            .filled
            .iter()
            .all(|limit| limit.provider_id == "openai"),
        "{identification:?}"
    );
    let fields: Vec<_> = identification
        .filled
        .iter()
        .map(|limit| limit.field.as_str())
        .collect();
    assert_eq!(fields, ["contextWindow", "maxOutputTokens"]);
}

/// The outcome reaches `metadata.identification`, and an ambiguity carries the
/// rows that disagree without becoming a projection error.
#[test]
fn the_identification_reaches_the_entry_and_names_an_ambiguity() {
    use dim_app_server::model_catalog::ModelIdentification;

    let catalog = BundledCatalog::load().unwrap();
    let identified = relay(&catalog, "deepseek-v4.1-flash");
    let identification: &ModelIdentification = identified
        .metadata
        .as_ref()
        .unwrap()
        .identification
        .as_ref()
        .unwrap();
    assert_eq!(identification.rule, "canonical");
    assert!(identification.narrowed_by_display_name);
    assert!(identification.candidates.is_empty());
    assert!(identification.filled.is_empty());

    let exact = relay(&catalog, "glm-5.2");
    assert_eq!(
        exact
            .metadata
            .as_ref()
            .unwrap()
            .identification
            .as_ref()
            .unwrap()
            .rule,
        "token"
    );

    let ambiguous = relay(&catalog, "ark-code-latest");
    let identification = ambiguous
        .metadata
        .as_ref()
        .unwrap()
        .identification
        .as_ref()
        .unwrap();
    assert_eq!(identification.rule, "ambiguous");
    let mut named: Vec<_> = identification
        .candidates
        .iter()
        .map(|candidate| candidate.provider_id.as_str())
        .collect();
    named.sort();
    assert_eq!(
        named,
        ["ark-agent-plan", "ark-code", "byteplus-coding-plan"]
    );

    let unmatched = relay(&catalog, "zzzz-nothing-names-this");
    assert!(
        unmatched
            .metadata
            .as_ref()
            .unwrap()
            .identification
            .is_none(),
        "nothing named the model, so there is no identification"
    );
}
