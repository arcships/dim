use dim_app_server::model_catalog::{
    BundledCatalog, ModelEdit, ModelEntry, ModelIdentification, ModelSource, ProviderProjection,
};
use serde_json::json;
use std::collections::BTreeMap;

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
    let row = bundled(&catalog, "openai", "gpt-5-nano");

    assert_eq!(entry.capabilities.context_window, Some(777));
    assert_eq!(numbers(&entry).1, numbers(&row).1);
    assert!(row.metadata.as_ref().unwrap().pricing.is_some());
    assert_eq!(
        entry.metadata.as_ref().unwrap().pricing,
        row.metadata.as_ref().unwrap().pricing
    );
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
    let row = bundled(&catalog, "openai", "gpt-5-nano");
    assert_eq!(numbers(&matched), numbers(&row));
    assert!(matched.capabilities.missing_limits().is_empty());
    let id = "zzzz-private-descriptor";
    assert!(catalog.fallback(&Default::default(), id, None).is_none());
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
    let row = bundled(&catalog, "openai", "gpt-5-nano");
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
    assert_eq!(reset[0].display_name, row.display_name);
    assert_eq!(numbers(&reset[0]), numbers(&row));
    let mut emptied = defaults[0].clone();
    emptied.capabilities = Default::default();
    let effective = catalog.effective_models(&provider, &[emptied], &[]);
    assert_eq!(numbers(&effective[0]), numbers(&row));
    assert!(!effective[0].capabilities.vision);
    assert_eq!(effective[0].capabilities.modalities, ["text"]);
}

/// The id a listing prints carries a vendor-relative path, underscores and
/// mixed case; each spelling still names the vendor's own row, so the answer is
/// that row and its numbers, not a new model.
#[test]
fn normalized_family_lookup_matches_prefixed_ids() {
    let catalog = catalog(json!([
        vendor(
            "acme",
            json!([row("acme-flash", "Acme Flash", 1_000_000, 384_000)]),
        ),
        vendor(
            "globex",
            json!([row("globex-flash", "Globex Flash", 1_048_576, 393_216)]),
        ),
    ]));
    let provider = ProviderProjection {
        provider_id: "custom-provider".into(),
        source: Some(ModelSource::Custom),
        ..Default::default()
    };
    for (id, vendor_id, model_id) in [
        ("models/vendor/ACME_Flash", "acme", "acme-flash"),
        ("models/vendor/GLOBEX_Flash", "globex", "globex-flash"),
    ] {
        let entry = catalog.project(&provider, &json!({ "id": id })).unwrap();
        assert_eq!(matched(&entry), (vendor_id, model_id), "{id}");
        assert_eq!(
            numbers(&entry),
            numbers(&bundled(&catalog, vendor_id, model_id)),
            "{id}"
        );
    }
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
                "id": "gpt-5-nano", "limit": {"context": 200_000, "output": 16_384},
                "vision": true, "modalities": ["text"],
                "capabilities": {"streaming": false, "providerState": {"custom": true}},
                "reasoning": {"supported": false, "effortOptions": ["off"]}
            }),
        )
        .unwrap();
    assert_eq!(model.capabilities.context_window, Some(200_000));
    assert_eq!(model.capabilities.max_output_tokens, Some(16_384));
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
                "capabilities": {"contextWindow": 200_000, "maxOutputTokens": 16_384},
                "vision": true
            }),
        )
        .unwrap();
    assert_eq!(nested_limits.capabilities.context_window, Some(200_000));
    assert_eq!(nested_limits.capabilities.max_output_tokens, Some(16_384));
    let effective = catalog.effective_models(&provider, &[nested_limits], &[]);
    assert_eq!(effective[0].capabilities.context_window, Some(200_000));
    assert_eq!(effective[0].capabilities.max_output_tokens, Some(16_384));
    assert_eq!(efforts(&model), Some(vec!["off".to_owned()]));
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
/// other keys; each model keeps what its server states. The ids name no row of
/// this catalog, so the listing is the only declaration read.
#[test]
fn listing_keys_of_other_servers_declare_the_limits_they_state() {
    let catalog = catalog(json!([]));
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
    let mut declared = 0;
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
            assert!(
                !transport.protocol.is_empty() && transport.path.starts_with('/'),
                "{}: the transport states the protocol and the path under the base URL",
                model.model_id
            );
            declared += 1;
        }
    }
    assert!(
        declared > 0,
        "a realtime operation ships with the transport it speaks"
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
            Some(ProviderRole::Vendor) => sellers.push(provider.provider_id.clone()),
            Some(ProviderRole::Aggregator) => resellers.push(provider.provider_id.clone()),
            None => panic!("{} lists models but no role", provider.provider_id),
        }
    }
    assert!(sellers.iter().any(|id| id == "openai"));
    assert!(resellers.iter().any(|id| id == "openrouter"));
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
        .map(|provider| provider.provider_id)
        .collect();
    assert!(
        !plans.is_empty(),
        "the catalog marks the subscriptions it sells"
    );
    for plan in plans {
        assert!(
            !catalog.bundled_models(&plan).unwrap_or_default().is_empty(),
            "{plan} is a plan with no declarations"
        );
    }
    for api in ["openai", "moonshot", "zai", "deepseek", "minimax"] {
        assert_eq!(catalog.is_plan(api), Some(false), "{api}");
    }
}

/// Alibaba sells Token Plan once per region, each with its own endpoint and API
/// keys, so the mainland plan is a provider of its own beside the international
/// one: both are API-key plans of the same vendor, and neither answers for the
/// other's endpoint.
#[test]
fn the_mainland_token_plan_is_its_own_plan_provider_beside_the_international_one() {
    let catalog = BundledCatalog::load().unwrap();
    for (provider_id, display_name, base_url) in [
        (
            "bailian-token-plan",
            "Alibaba Cloud Model Studio Token Plan",
            "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
        ),
        (
            "qwencloud-token-plan",
            "QwenCloud Token Plan",
            "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
        ),
    ] {
        let provider = catalog.provider(provider_id).expect(provider_id);
        assert_eq!(provider.display_name, display_name);
        assert_eq!(provider.default_base_url.as_deref(), Some(base_url));
        assert_eq!(provider.auth.kind, "api_key", "{provider_id}");
        assert_eq!(catalog.is_plan(provider_id), Some(true), "{provider_id}");
        assert_eq!(
            catalog.vendor_namespace(provider_id).as_deref(),
            Some("alibaba"),
            "{provider_id}"
        );
        assert!(
            !provider.models.is_empty(),
            "{provider_id} is a plan with no declarations"
        );
    }
}

/// The ids the shipped catalog answers where the catalog's own metadata says
/// they belong: a model a plan resells at the vendor row that declares it, a
/// model only the user's list declares at that declaration, a vendor's own id
/// at its vendor, and Qwen's own rows on the provider that serves them.
#[test]
fn the_resold_models_identify_where_the_catalog_says_they_belong() {
    let catalog = BundledCatalog::load().unwrap();
    for (id, provider, model) in [
        ("deepseek-v4.1-flash", "deepseek", "deepseek-flash"),
        ("kimi-k2.5", "custom-provider", "kimi-k2.5"),
        ("glm-5.2", "zai", "glm-5.2"),
        ("qwen3.8-max", "qwencloud", "qwen3.8-max"),
        ("qwen3.7-plus", "qwencloud", "qwen3.7-plus"),
    ] {
        let entry = relay(&catalog, id);
        assert_eq!(matched(&entry), (provider, model), "{id}");
        let row = bundled(&catalog, provider, model);
        assert!(
            row.capabilities.missing_limits().is_empty(),
            "{id}: {provider} declares no limit for {model}"
        );
        assert_eq!(numbers(&entry), numbers(&row), "{id}");
    }
}

/// Every vendor provider whose rows name a canonical id states the namespace
/// its own models name, and no row that names another vendor answers as its
/// provider's declaration: such a row is the merge's, not the vendor's.
#[test]
fn a_vendor_provider_answers_only_for_the_namespace_it_states() {
    use dim_app_server::model_catalog::ProviderRole;
    let catalog = BundledCatalog::load().unwrap();
    let mut declaring = 0;
    let mut resold = 0;
    for provider in catalog.providers() {
        let Some(own) = catalog.vendor_namespace(&provider.provider_id) else {
            continue;
        };
        let names = provider
            .models
            .iter()
            .any(|model| canonical_namespace(model).is_some());
        if names {
            declaring += 1;
        }
        for model in &provider.models {
            let Some(named) = canonical_namespace(model) else {
                continue;
            };
            if named == own {
                continue;
            }
            resold += 1;
            let entry = relay(&catalog, &model.model_id);
            let answered_by_its_provider =
                matched(&entry) == (provider.provider_id.as_str(), model.model_id.as_str());
            assert!(
                !answered_by_its_provider
                    || !["token", "qualified", "canonical"].contains(&rule(&entry)),
                "{}/{} names {named} and answered by the {} step",
                provider.provider_id,
                model.model_id,
                rule(&entry)
            );
        }
    }
    assert!(
        declaring > 0,
        "no vendor provider states a namespace, so nothing is covered"
    );
    assert!(resold > 0, "no row resells another vendor's model");
    let vendors: Vec<_> = catalog
        .providers()
        .filter(|provider| {
            catalog.role(&provider.provider_id) == Some(ProviderRole::Vendor)
                && provider
                    .models
                    .iter()
                    .any(|model| canonical_namespace(model).is_some())
        })
        .map(|provider| provider.provider_id)
        .collect();
    for vendor in vendors {
        assert!(
            catalog.vendor_namespace(&vendor).is_some(),
            "{vendor} declares canonical ids but states no namespace"
        );
    }
}

/// Every plan row the catalog leaves without a canonical id, on a plan provider
/// that states a namespace, names no other vendor's model: no vendor declares
/// the same id, and no row of the catalog states another vendor's canonical id
/// for it. Such a row would be a resale the merge reads, so the committed
/// catalog owes it the canonical id.
///
/// Ids are compared exactly, by their last path segment, case-insensitively. A
/// vendor provider's own namespace is the one it states, and an aggregator
/// states none, so only the canonical id a row states can tie it to a vendor. A
/// plan provider that states no namespace is skipped: `openai-codex`,
/// `grok-build` and `step-plan` have no canonical rows and resell nothing, so
/// the catalog has nothing to compare them against.
#[test]
fn a_plan_row_without_a_canonical_id_names_no_other_vendor() {
    use dim_app_server::model_catalog::ProviderRole;
    let catalog = BundledCatalog::load().unwrap();
    let mut checked = 0;
    for provider in catalog.providers() {
        if catalog.role(&provider.provider_id) != Some(ProviderRole::Vendor)
            || catalog.is_plan(&provider.provider_id) != Some(true)
        {
            continue;
        }
        let Some(own) = catalog.vendor_namespace(&provider.provider_id) else {
            continue;
        };
        for model in &provider.models {
            if canonical_namespace(model).is_some() {
                continue;
            }
            checked += 1;
            let segment = last_segment(&model.model_id);
            let mut ties = Vec::new();
            for other in catalog.providers() {
                let states = catalog.vendor_namespace(&other.provider_id);
                for row in &other.models {
                    if let Some(namespace) = states.as_deref() {
                        let declares = namespace != own
                            && canonical_namespace(row).is_none_or(|named| named == namespace)
                            && last_segment(&row.model_id).eq_ignore_ascii_case(segment);
                        if declares {
                            ties.push(format!(
                                "`{provider_id}/{row_model}` declares it as {namespace}'s",
                                provider_id = other.provider_id,
                                row_model = row.model_id,
                            ));
                        }
                    }
                    let canonical = row
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.raw.get("canonical_model_id"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|canonical| {
                            canonical.split_once('/').is_some_and(|(it, _)| it != own)
                                && last_segment(canonical).eq_ignore_ascii_case(segment)
                        });
                    if let Some(canonical) = canonical {
                        ties.push(format!(
                            "`{provider_id}/{row_model}` states `{canonical}`",
                            provider_id = other.provider_id,
                            row_model = row.model_id,
                        ));
                    }
                }
            }
            assert!(
                ties.is_empty(),
                "{}/{} states no canonical id but is another vendor's model, and its provider's own namespace is {own}: {}",
                provider.provider_id,
                model.model_id,
                ties.join("; ")
            );
        }
    }
    assert!(
        checked > 0,
        "no plan provider that states a namespace lists a row without a canonical id"
    );
}

/// A catalog of exactly the providers a case states, built through the step
/// the shipped file goes through: `from_value` drops every limit a row's
/// `metadata.raw` does not declare, so a built row declares its limits there.
///
/// It answers "what does the matcher run over": a case's input, never its
/// expected value, which `bundled` reads from the row the case names.
fn catalog(providers: serde_json::Value) -> BundledCatalog {
    BundledCatalog::from_value(json!({ "providers": providers }))
        .expect("the case states the catalog its rule needs")
}

/// A provider serving its own vendor's models.
fn vendor(provider_id: &str, models: serde_json::Value) -> serde_json::Value {
    provider(provider_id, "vendor", false, models)
}

/// The same provider, stating the canonical namespace its own models name.
fn namespaced_vendor(
    provider_id: &str,
    vendor_namespace: &str,
    models: serde_json::Value,
) -> serde_json::Value {
    let mut provider = provider(provider_id, "vendor", false, models);
    provider["metadata"]["vendorNamespace"] = json!(vendor_namespace);
    provider
}

/// A vendor selling a subscription beside its API entry, stating its namespace.
fn namespaced_plan_provider(
    provider_id: &str,
    vendor_namespace: &str,
    models: serde_json::Value,
) -> serde_json::Value {
    let mut provider = provider(provider_id, "vendor", true, models);
    provider["metadata"]["vendorNamespace"] = json!(vendor_namespace);
    provider
}

/// A provider reselling other vendors' models.
fn aggregator(provider_id: &str, models: serde_json::Value) -> serde_json::Value {
    provider(provider_id, "aggregator", false, models)
}

/// A vendor selling a subscription beside its API entry.
fn plan_provider(provider_id: &str, models: serde_json::Value) -> serde_json::Value {
    provider(provider_id, "vendor", true, models)
}

/// The fields no step reads are the same in every case; the role is what the
/// steps partition the rows on, and the plan mark is what the plan rule reads.
fn provider(
    provider_id: &str,
    role: &str,
    plan: bool,
    models: serde_json::Value,
) -> serde_json::Value {
    json!({
        "providerId": provider_id,
        "displayName": provider_id,
        "driverKind": "openai",
        "defaultBaseUrl": "https://example.test/v1",
        "auth": { "type": "api_key", "headerName": "Authorization" },
        "metadata": { "role": role, "plan": plan },
        "models": models,
    })
}

/// One row declaring both limits.
fn row(model_id: &str, display_name: &str, window: u64, output: u64) -> serde_json::Value {
    json!({
        "modelId": model_id,
        "displayName": display_name,
        "status": "active",
        "capabilities": {
            "modalities": ["text"], "toolCalling": true, "streaming": true,
            "structuredOutput": true, "vision": false, "audio": false,
            "reasoning": false,
            "contextWindow": window, "maxOutputTokens": output,
        },
        "metadata": { "raw": { "limit": { "context": window, "output": output } } },
    })
}

/// The same row, with the fields its server lists beside the entry.
fn with_raw(mut row: serde_json::Value, raw: serde_json::Value) -> serde_json::Value {
    let fields = row["metadata"]["raw"].as_object_mut().unwrap();
    for (key, value) in raw.as_object().unwrap() {
        fields.insert(key.clone(), value.clone());
    }
    row
}

/// The same row, declaring the modalities it reads and emits.
fn with_vision(mut row: serde_json::Value, modalities: &[&str]) -> serde_json::Value {
    row["capabilities"]["vision"] = json!(true);
    row["capabilities"]["modalities"] = json!(modalities);
    row
}

/// The same row, with the upstream id its vendor publishes for it.
fn with_canonical(row: serde_json::Value, canonical: &str) -> serde_json::Value {
    with_raw(row, json!({ "canonical_model_id": canonical }))
}

/// The same row, with the efforts it offers.
fn with_efforts(mut row: serde_json::Value, efforts: &[&str]) -> serde_json::Value {
    row["metadata"]["reasoning"] = json!({ "supported": true, "effortOptions": efforts });
    row
}

/// The same row, with the caller's own statement of its tool calling.
fn with_tool_calling(mut row: serde_json::Value, tool_calling: bool) -> serde_json::Value {
    row["capabilities"]["toolCalling"] = json!(tool_calling);
    row
}

/// The same row, stating the kind of model it describes.
fn with_kind(mut row: serde_json::Value, kind: &str) -> serde_json::Value {
    row["metadata"]["raw"]["type"] = json!(kind);
    row
}

/// One row of a kind other than chat: the kind stated in its raw `type`, and a
/// placeholder pair its provider writes across its rows.
fn media_row(
    model_id: &str,
    display_name: &str,
    kind: &str,
    window: u64,
    output: u64,
) -> serde_json::Value {
    let mut row = with_raw(
        row(model_id, display_name, window, output),
        json!({ "type": kind }),
    );
    row["metadata"]["reasoning"] = json!({ "supported": false });
    row
}

/// One entry of a provider's stored list, as a provider that states its own
/// kind, its pair and a reasoning toggle saved it.
fn chat_row(model_id: &str, display_name: &str, window: u64, output: u64) -> ModelEntry {
    serde_json::from_value(json!({
        "modelId": model_id,
        "displayName": display_name,
        "status": "active",
        "capabilities": {
            "modalities": ["text"], "toolCalling": true, "streaming": true,
            "structuredOutput": true, "vision": false, "audio": false,
            "reasoning": true, "contextWindow": window, "maxOutputTokens": output,
        },
        "metadata": {
            "reasoning": { "supported": true, "effortOptions": ["low", "high"] },
            "raw": { "type": "chat" },
        },
    }))
    .expect("a listed chat row states its id, pair, kind and efforts")
}

/// The namespace a row's canonical id names, as the matcher reads it.
fn canonical_namespace(model: &ModelEntry) -> Option<&str> {
    model
        .metadata
        .as_ref()?
        .raw
        .get("canonical_model_id")?
        .as_str()?
        .split_once('/')
        .map(|(namespace, _)| namespace)
}

/// An id's last path segment, which is the part two ids name a model by.
fn last_segment(id: &str) -> &str {
    id.rsplit_once('/').map_or(id, |(_, segment)| segment)
}

/// One entry of a provider's stored list, as that provider saved it.
fn stored(model_id: &str) -> ModelEntry {
    serde_json::from_value(json!({
        "modelId": model_id,
        "displayName": model_id,
        "status": "active",
        "capabilities": {
            "modalities": ["text"], "toolCalling": true, "streaming": true,
            "structuredOutput": true, "vision": false, "audio": false,
            "reasoning": false,
        },
    }))
    .expect("a stored entry states its id and API")
}

/// One row of a catalog, as the catalog holds it, read directly rather than
/// through the matcher.
///
/// It answers "what does this row declare": a case's expected value, which the
/// matcher's answer (`relay`, `builtin`) is compared against. It never builds
/// a case's input, since an expectation read back from the rows a case already
/// projected would only assert that the catalog agrees with itself.
fn bundled(catalog: &BundledCatalog, provider_id: &str, model_id: &str) -> ModelEntry {
    catalog
        .bundled_models(provider_id)
        .unwrap_or_else(|| panic!("{provider_id} is not a provider of this catalog"))
        .iter()
        .find(|row| row.model_id == model_id)
        .unwrap_or_else(|| panic!("{provider_id} lists no {model_id}"))
        .clone()
}

/// The identification an entry earned.
fn identification(entry: &ModelEntry) -> &ModelIdentification {
    entry
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.identification.as_ref())
        .expect("the entry carries the identification it earned")
}

/// The step that identified an entry.
fn rule(entry: &ModelEntry) -> &str {
    identification(entry).rule.as_str()
}

/// The efforts an entry states.
fn efforts(entry: &ModelEntry) -> Option<Vec<String>> {
    entry
        .metadata
        .as_ref()?
        .reasoning
        .as_ref()?
        .effort_options
        .clone()
}

/// The rows an ambiguity names, as `provider/model`.
fn candidates(entry: &ModelEntry) -> Vec<String> {
    identification(entry)
        .candidates
        .iter()
        .map(|candidate| format!("{}/{}", candidate.provider_id, candidate.model_id))
        .collect()
}

/// The limits an identification records as adopted, as `(field, value, seller)`.
fn adopted(entry: &ModelEntry) -> Vec<(&str, u64, &str)> {
    identification(entry)
        .filled
        .iter()
        .map(|limit| {
            (
                limit.field.as_str(),
                limit.value,
                limit.provider_id.as_str(),
            )
        })
        .collect()
}

/// Projects one id as a relay outside the catalog, which is the projection a
/// gateway that ships no bundled list has.
fn relay(catalog: &BundledCatalog, id: &str) -> ModelEntry {
    prefixed_relay(catalog, id, None)
}

/// The same projection, with the prefix the provider states its ids carry.
fn prefixed_relay(catalog: &BundledCatalog, id: &str, prefix: Option<&str>) -> ModelEntry {
    let provider = ProviderProjection {
        provider_id: "my-gateway".into(),
        source: Some(ModelSource::Custom),
        glued_prefix: prefix.map(str::to_owned),
        ..Default::default()
    };
    catalog.project(&provider, &json!({ "id": id })).unwrap()
}

/// The same projection, for a listed id that states its own kind: the kind it
/// carries is the one the merge joins rows of.
fn kinded_relay(catalog: &BundledCatalog, id: &str, kind: &str) -> ModelEntry {
    let provider = ProviderProjection {
        provider_id: "my-gateway".into(),
        source: Some(ModelSource::Custom),
        ..Default::default()
    };
    catalog
        .project(&provider, &json!({ "id": id, "raw": { "type": kind } }))
        .unwrap()
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

/// The ids the shipped catalog answers at a vendor row, by the first four
/// steps: the vendor's own list, its tokens, its canonical id and display name.
/// Each expected window and output cap is read from the bundled row of the
/// provider named beside it, so a catalog change moves the expectation with it.
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
        let row = bundled(&catalog, provider, model);
        let expected = numbers(&row);
        assert!(
            expected.0.is_some() && expected.1.is_some(),
            "{id}: {provider} declares no limit for {model}"
        );
        assert_eq!(numbers(&entry), expected, "{id}");
    }
}

/// One canonical id names three vendor rows and their numbers and efforts
/// differ, so the display name of one row is what settles which of them
/// answers, and that row's efforts come with it.
#[test]
fn a_canonical_id_is_narrowed_by_the_display_name() {
    let catalog = catalog(json!([vendor(
        "acme",
        json!([
            with_efforts(
                with_canonical(
                    row("acme-v4-flash", "Acme V4 Flash", 200_000, 64_000),
                    "acme/acme-v4.1-flash-1017",
                ),
                &["low", "high"],
            ),
            with_canonical(
                row(
                    "acme-v4-flash-vision",
                    "Acme V4 Flash Vision",
                    100_000,
                    32_000
                ),
                "acme/acme-v4.1-flash-1017",
            ),
            with_efforts(
                with_canonical(
                    row("acme-flash", "Acme V4.1 Flash", 1_000_000, 128_000),
                    "acme/acme-v4.1-flash-1017",
                ),
                &["low", "high", "max"],
            ),
        ])
    ),]));
    let entry = relay(&catalog, "acme-v4.1-flash");
    let narrowed = bundled(&catalog, "acme", "acme-flash");
    assert_eq!(matched(&entry), ("acme", "acme-flash"));
    assert_eq!(numbers(&entry), numbers(&narrowed));
    assert_eq!(rule(&entry), "canonical");
    assert!(identification(&entry).narrowed_by_display_name);
    assert_eq!(
        entry.metadata.as_ref().unwrap().match_strategy.as_deref(),
        Some("acme-canonical")
    );
    assert!(
        efforts(&narrowed).is_some_and(|efforts| !efforts.is_empty()),
        "the narrowed row offers efforts, so carrying them is visible"
    );
    assert_ne!(
        efforts(&narrowed),
        efforts(&bundled(&catalog, "acme", "acme-v4-flash")),
        "the rows offer different efforts, so which one answers is visible"
    );
    assert_eq!(efforts(&entry), efforts(&narrowed));
}

/// An API entry beats the plan that the same vendor sells beside it, and on
/// the plan's own provider the plan's own numbers still win.
#[test]
fn an_api_entry_beats_a_plan_entry() {
    let catalog = catalog(json!([
        vendor(
            "acme",
            json!([row("acme-5.5", "Acme 5.5", 1_050_000, 128_000)]),
        ),
        plan_provider(
            "acme-plan",
            json!([row("acme-5.5", "Acme 5.5", 258_400, 128_000)]),
        ),
    ]));
    let api = bundled(&catalog, "acme", "acme-5.5");
    let plan = bundled(&catalog, "acme-plan", "acme-5.5");
    assert_ne!(
        numbers(&api),
        numbers(&plan),
        "the two entries disagree, so which one answers is visible"
    );
    let on_a_relay = relay(&catalog, "acme-5.5");
    assert_eq!(matched(&on_a_relay), ("acme", "acme-5.5"));
    assert_eq!(numbers(&on_a_relay), numbers(&api));
    let on_the_plan = builtin(&catalog, "acme-plan", "acme-5.5");
    assert_eq!(matched(&on_the_plan), ("acme-plan", "acme-5.5"));
    assert_eq!(numbers(&on_the_plan), numbers(&plan));
}

/// A row whose tokens are a head of the input is not an answer when the tail
/// names another model, so the id keeps the projection's own default rather
/// than the family base's pair.
#[test]
fn a_variant_is_not_its_family_base() {
    let catalog = catalog(json!([vendor(
        "acme",
        json!([row("acme-5", "Acme 5", 400_000, 128_000)]),
    ),]));
    let entry = relay(&catalog, "acme-5-codex");
    assert_ne!(matched(&entry), ("acme", "acme-5"));
    assert_ne!(
        numbers(&entry),
        numbers(&bundled(&catalog, "acme", "acme-5"))
    );
    assert_eq!(
        entry.capabilities.missing_limits(),
        ["contextWindow", "maxOutputTokens"]
    );
}

/// Several rows claim one id, declare different numbers, and no API entry
/// exists between them, so none of them may supply numbers.
#[test]
fn two_vendors_claiming_one_id_report_ambiguity() {
    let catalog = catalog(json!([
        vendor(
            "acme",
            json!([row("acme-code-latest", "Acme Code Latest", 204_800, 60_000)]),
        ),
        vendor(
            "globex",
            json!([row("acme-code-latest", "Acme Code Latest", 256_000, 60_000)]),
        ),
    ]));
    let entry = relay(&catalog, "acme-code-latest");
    assert_eq!(matched(&entry), ("provider-default", "-"));
    assert_eq!(rule(&entry), "ambiguous");
    assert_eq!(
        candidates(&entry),
        ["acme/acme-code-latest", "globex/acme-code-latest"]
    );
    assert_eq!(
        entry.capabilities.missing_limits(),
        ["contextWindow", "maxOutputTokens"]
    );
    assert_ne!(
        numbers(&bundled(&catalog, "acme", "acme-code-latest")),
        numbers(&bundled(&catalog, "globex", "acme-code-latest")),
        "the two claiming rows disagree, which is why neither may answer"
    );
}

/// Two providers selling one model under one name with one set of numbers are
/// one model, not a conflict, and the API entry beside them settles it.
/// A plan's own provider keeps the plan's numbers even when the vendor's API
/// entry disagrees, since the provider answers for the subscription it sells.
#[test]
fn a_plan_keeps_its_own_numbers_on_its_own_provider() {
    let catalog = catalog(json!([
        vendor(
            "acme",
            json!([row("acme-6.1-sol", "Acme 6.1 Sol", 1_050_000, 128_000)]),
        ),
        plan_provider(
            "acme-plan",
            json!([row("acme-6.1-sol", "Acme 6.1 Sol", 258_400, 128_000)]),
        ),
    ]));
    let plan = builtin(&catalog, "acme-plan", "acme-6.1-sol");
    assert_eq!(
        numbers(&plan),
        numbers(&bundled(&catalog, "acme-plan", "acme-6.1-sol"))
    );
    let api = bundled(&catalog, "acme", "acme-6.1-sol");
    assert_ne!(numbers(&api), numbers(&plan));
}

/// A provider answers for its own family before another seller does, and the
/// token step is what makes it once step 1 names nothing: the id is a dated
/// form no row spells, so the plan keeps its own window for `acme-5.5` rather
/// than the vendor API entry the plan rule would otherwise prefer.
#[test]
fn a_providers_own_family_answers_before_another_seller() {
    let catalog = catalog(json!([
        vendor(
            "acme",
            json!([row("acme-5.5", "Acme 5.5", 1_050_000, 128_000)]),
        ),
        plan_provider(
            "acme-plan",
            json!([row("acme-5.5", "Acme 5.5", 258_400, 128_000)]),
        ),
    ]));
    let provider = ProviderProjection {
        provider_id: "acme-plan".into(),
        source: Some(ModelSource::Builtin),
        models: catalog.bundled_models("acme-plan").unwrap().to_vec(),
        ..Default::default()
    };
    let entry = catalog
        .project(&provider, &json!({ "id": "acme-5.5-20250801" }))
        .unwrap();
    assert_eq!(matched(&entry), ("acme-plan", "acme-5.5"));
    assert_eq!(
        numbers(&entry),
        numbers(&bundled(&catalog, "acme-plan", "acme-5.5"))
    );
    assert_ne!(
        numbers(&entry),
        numbers(&bundled(&catalog, "acme", "acme-5.5"))
    );
    assert_eq!(rule(&entry), "token");
}

/// A vendor row is an answer where an aggregator row alone is not: the merge
/// that would combine them arrives later, so a vendor's declaration wins now.
#[test]
fn a_plain_id_identifies_at_its_vendor() {
    let catalog = catalog(json!([
        vendor(
            "acme",
            json!([with_efforts(
                row("acme-2", "Acme 2", 1_000_000, 131_072),
                &["high", "max"],
            )]),
        ),
        aggregator(
            "custom-provider",
            json!([row("acme-2", "Acme 2", 1_000_000, 131_072)]),
        ),
    ]));
    let entry = relay(&catalog, "acme-2");
    assert_eq!(matched(&entry), ("acme", "acme-2"));
    assert_eq!(
        efforts(&entry),
        efforts(&bundled(&catalog, "acme", "acme-2"))
    );
    assert_ne!(
        efforts(&entry),
        efforts(&bundled(&catalog, "custom-provider", "acme-2"))
    );
}

/// A vendor the id names in its namespace segment wins over another vendor
/// selling the same model, so `acme/acme-code-latest` answers with `acme`
/// while the bare id stays ambiguous.
#[test]
fn a_namespaced_id_identifies_the_vendor_it_names() {
    let catalog = catalog(json!([
        vendor(
            "acme",
            json!([row("acme-code-latest", "Acme Code Latest", 204_800, 60_000)]),
        ),
        vendor(
            "globex",
            json!([row("acme-code-latest", "Acme Code Latest", 256_000, 60_000)]),
        ),
    ]));
    let entry = relay(&catalog, "acme/acme-code-latest");
    assert_eq!(matched(&entry), ("acme", "acme-code-latest"));
    assert_eq!(
        numbers(&entry),
        numbers(&bundled(&catalog, "acme", "acme-code-latest"))
    );
    assert_ne!(
        numbers(&entry),
        numbers(&bundled(&catalog, "globex", "acme-code-latest"))
    );
    let bare = relay(&catalog, "acme-code-latest");
    assert_eq!(matched(&bare), ("provider-default", "-"));
}

/// Step 4 reads the `custom-provider` rows by the same token and name rules,
/// so a model that only that list carries resolves to a declared row and is
/// not left for the merge.
#[test]
fn a_custom_provider_row_is_a_declaration() {
    let declared = row("acme-omni-flash", "Acme Omni Flash", 100_000, 20_000);
    let catalog = catalog(json!([
        vendor(
            "acme",
            json!([row("acme-omni", "Acme Omni", 200_000, 40_000)]),
        ),
        aggregator("custom-provider", json!([declared])),
    ]));
    for input in ["acme-omni-flash", "Acme Omni Flash"] {
        let entry = relay(&catalog, input);
        assert_eq!(
            matched(&entry),
            ("custom-provider", "acme-omni-flash"),
            "{input}"
        );
        assert_eq!(rule(&entry), "custom-provider", "{input}");
        assert_eq!(
            entry.metadata.as_ref().unwrap().match_strategy.as_deref(),
            Some("custom-provider"),
            "{input}"
        );
        assert_eq!(
            numbers(&entry),
            numbers(&bundled(&catalog, "custom-provider", "acme-omni-flash")),
            "{input}"
        );
        assert!(entry.capabilities.missing_limits().is_empty(), "{input}");
    }
}

/// A vendor provider's foreign-canonical row is not its declaration, while its
/// own row still is: the id the foreign row names is answered by the vendor
/// that model belongs to, and the id the own row names is answered here.
#[test]
fn a_foreign_canonical_row_is_not_a_declaration() {
    let own = row("acme-5", "Acme 5", 400_000, 128_000);
    let catalog = catalog(json!([
        namespaced_vendor(
            "acme-plan",
            "acme",
            json!([
                with_canonical(
                    row("acme-own", "Acme Own", 200_000, 64_000),
                    "acme/acme-own"
                ),
                with_canonical(
                    row("globus-5", "Globus 5", 100_000, 32_000),
                    "globus/globus-5",
                ),
            ]),
        ),
        namespaced_vendor("acme", "acme", json!([own.clone()])),
        namespaced_vendor(
            "globus",
            "globus",
            json!([row("globus-5", "Globus 5", 300_000, 96_000)]),
        ),
    ]));
    let foreign = relay(&catalog, "globus-5");
    assert_eq!(
        matched(&foreign),
        ("globus", "globus-5"),
        "the plan's resold row does not answer for the vendor that owns it"
    );
    assert_eq!(
        numbers(&foreign),
        numbers(&bundled(&catalog, "globus", "globus-5"))
    );
    let own_id = relay(&catalog, "acme-own");
    assert_eq!(
        matched(&own_id),
        ("acme-plan", "acme-own"),
        "the plan's own row is still its declaration"
    );
    assert_eq!(rule(&own_id), "token");
}

/// A foreign-canonical row of a vendor provider joins the merge exactly as an
/// aggregator row does, so an id only that row names still gets its numbers and
/// the smallest pair wins.
#[test]
fn a_foreign_canonical_row_still_joins_the_merge() {
    let catalog = catalog(json!([
        namespaced_vendor(
            "acme-plan",
            "acme",
            json!([with_canonical(
                row("globus-5", "Globus 5", 100_000, 32_000),
                "globus/globus-5",
            )]),
        ),
        aggregator(
            "reseller",
            json!([row("globus-5", "Globus 5", 200_000, 16_000)]),
        ),
    ]));
    let entry = relay(&catalog, "globus-5");
    assert_eq!(identification(&entry).rule, "merge");
    let merged = (
        numbers(&bundled(&catalog, "acme-plan", "globus-5")).0,
        numbers(&bundled(&catalog, "reseller", "globus-5")).1,
    );
    assert_eq!(numbers(&entry), merged);
    assert_eq!(
        adopted(&entry),
        [
            ("contextWindow", merged.0.unwrap(), "acme-plan"),
            ("maxOutputTokens", merged.1.unwrap(), "reseller"),
        ]
    );
}

/// A vendor-provider row that states no canonical id stays a declaration: the
/// catalog says nothing against the role for it, so the role's answer stands.
#[test]
fn a_row_without_a_canonical_id_stays_a_declaration() {
    let catalog = catalog(json!([
        namespaced_plan_provider(
            "acme-plan",
            "acme",
            json!([
                row("acme-5", "Acme 5", 258_400, 128_000),
                with_canonical(
                    row("acme-5-pro", "Acme 5 Pro", 1_000_000, 128_000),
                    "acme/acme-5-pro"
                ),
            ]),
        ),
        namespaced_vendor(
            "acme",
            "acme",
            json!([row("acme-5", "Acme 5", 1_050_000, 128_000)]),
        ),
    ]));
    let on_the_plan = builtin(&catalog, "acme-plan", "acme-5");
    assert_eq!(matched(&on_the_plan), ("acme-plan", "acme-5"));
    assert_eq!(
        numbers(&on_the_plan),
        numbers(&bundled(&catalog, "acme-plan", "acme-5"))
    );
    assert_eq!(rule(&on_the_plan), "provider-exact");
    let on_a_relay = relay(&catalog, "acme-5");
    assert_eq!(
        matched(&on_a_relay),
        ("acme", "acme-5"),
        "the API entry still answers a bare id over the plan"
    );
    assert_eq!(
        numbers(&on_a_relay),
        numbers(&bundled(&catalog, "acme", "acme-5"))
    );
}

/// A plan row whose canonical id names another vendor stops answering a bare id
/// ahead of that vendor's row: with the id stated, the plan's row leaves the
/// vendor index and the vendor's own canonical id answers instead.
#[test]
fn a_stated_resale_stops_answering_ahead_of_its_vendor() {
    let build = |canonical: Option<&str>| {
        let plan = match canonical {
            Some(canonical) => {
                with_canonical(row("globus-5", "Globus 5", 262_144, 32_768), canonical)
            }
            None => row("globus-5", "Globus 5", 262_144, 32_768),
        };
        catalog(json!([
            namespaced_plan_provider("acme-coding-plan", "acme", json!([plan])),
            namespaced_vendor(
                "globus",
                "globus",
                json!([with_canonical(
                    row("globus-5-2026-01", "Globus 5", 1_048_576, 384_000),
                    "globus/globus-5"
                )]),
            ),
        ]))
    };
    let unstated = build(None);
    let answered = relay(&unstated, "globus-5");
    assert_eq!(
        matched(&answered),
        ("acme-coding-plan", "globus-5"),
        "without the canonical id the plan's row answers the bare id by token"
    );
    assert_eq!(rule(&answered), "token");

    let stated = build(Some("globus/globus-5"));
    let restored = relay(&stated, "globus-5");
    assert_eq!(matched(&restored), ("globus", "globus-5-2026-01"));
    assert_eq!(rule(&restored), "canonical");
    assert_eq!(
        numbers(&restored),
        numbers(&bundled(&stated, "globus", "globus-5-2026-01")),
        "the vendor's own numbers answer, not the plan's"
    );
}

/// A plan row that resells another vendor's model stops answering ahead of the
/// `custom-provider` declaration too: the declaration is what the user wrote,
/// and the resold row is only the merge's.
#[test]
fn a_stated_resale_stops_answering_ahead_of_the_declaration() {
    let build = |canonical: Option<&str>| {
        let plan = match canonical {
            Some(canonical) => {
                with_canonical(row("globus-5", "Globus 5", 262_144, 32_768), canonical)
            }
            None => row("globus-5", "Globus 5", 262_144, 32_768),
        };
        catalog(json!([
            namespaced_plan_provider("acme-coding-plan", "acme", json!([plan])),
            aggregator(
                "custom-provider",
                json!([row("globus-5", "Globus 5", 262_144, 262_144)]),
            ),
        ]))
    };
    let unstated = build(None);
    assert_eq!(
        matched(&relay(&unstated, "globus-5")),
        ("acme-coding-plan", "globus-5"),
        "without the canonical id the plan's row answers over the declaration"
    );

    let stated = build(Some("globus/globus-5"));
    let declared = relay(&stated, "globus-5");
    assert_eq!(matched(&declared), ("custom-provider", "globus-5"));
    assert_eq!(rule(&declared), "custom-provider");
    assert_eq!(
        numbers(&declared),
        numbers(&bundled(&stated, "custom-provider", "globus-5"))
    );
}

/// A row states its own namespace because its canonical id names the vendor
/// that model belongs to, so a provider's own plan row that repeats the
/// provider's namespace stays its declaration: the row is what decides, not the
/// provider's list of canonical ids.
#[test]
fn a_plan_row_naming_its_own_namespace_stays_a_declaration() {
    let catalog = catalog(json!([
        namespaced_plan_provider(
            "acme-plan",
            "acme",
            json!([with_canonical(
                row("acme-5", "Acme 5", 258_400, 128_000),
                "acme/acme-5"
            )]),
        ),
        aggregator(
            "custom-provider",
            json!([row("acme-5", "Acme 5", 262_144, 262_144)]),
        ),
    ]));
    let answered = relay(&catalog, "acme-5");
    assert_eq!(matched(&answered), ("acme-plan", "acme-5"));
    assert_eq!(rule(&answered), "token");
    assert_eq!(
        numbers(&answered),
        numbers(&bundled(&catalog, "acme-plan", "acme-5"))
    );
}

/// Step 1 answers an aggregator's own house model on that aggregator's own
/// provider: a reseller lists its own router itself, so the row's own limits
/// stand and no vendor row is consulted.
#[test]
fn an_aggregators_house_model_answers_on_its_own_provider() {
    let catalog = catalog(json!([aggregator(
        "reseller",
        json!([row("reseller/fusion", "Reseller: Fusion", 8_192, 8_192)]),
    )]));
    let entry = builtin(&catalog, "reseller", "reseller/fusion");
    assert_eq!(matched(&entry), ("reseller", "reseller/fusion"));
    assert_eq!(
        numbers(&entry),
        numbers(&bundled(&catalog, "reseller", "reseller/fusion"))
    );
    assert_eq!(
        entry.metadata.as_ref().unwrap().match_strategy.as_deref(),
        Some("provider-exact")
    );
    assert_eq!(rule(&entry), "provider-exact");
    assert!(entry.capabilities.missing_limits().is_empty());
}

/// An aggregator's own qualified id is its identity before any step drops the
/// namespace: `reseller/auto` is the reseller's router and not the bare `auto`
/// a vendor sells, and `reseller/fusion` is its text router and not another
/// seller's audio transcription model.
#[test]
fn a_qualified_house_model_keeps_its_own_identity() {
    let catalog = catalog(json!([
        aggregator(
            "reseller",
            json!([
                with_vision(
                    row("reseller/auto", "Auto Router", 8_192, 8_192),
                    &["text", "image", "audio", "video"],
                ),
                row("reseller/fusion", "Reseller: Fusion", 8_192, 8_192),
            ]),
        ),
        vendor(
            "acme",
            json!([
                with_vision(
                    row("auto", "Acme Auto", 200_000, 64_000),
                    &["text", "image"]
                ),
                with_raw(
                    row("fusion", "Acme Fusion", 32_000, 4_096),
                    json!({ "modalityCapabilities": ["audio.transcribe"] }),
                ),
            ]),
        ),
    ]));
    for id in ["reseller/auto", "reseller/fusion"] {
        let entry = relay(&catalog, id);
        let row = bundled(&catalog, "reseller", id);
        let metadata = entry.metadata.as_ref().unwrap();
        let answered = (
            matched(&entry),
            numbers(&entry),
            entry.capabilities.modalities.clone(),
            entry.capabilities.audio,
            entry.capabilities.tool_calling,
            metadata
                .identification
                .as_ref()
                .map(|found| found.rule.as_str()),
            metadata.match_strategy.as_deref(),
        );
        let expected = (
            ("reseller", id),
            numbers(&row),
            row.capabilities.modalities.clone(),
            row.capabilities.audio,
            row.capabilities.tool_calling,
            Some("qualified"),
            Some("reseller-exact"),
        );
        assert_eq!(
            answered, expected,
            "{id}: the relay answers with the row itself"
        );
    }
}

/// The prefix states what a relay glued onto every id it lists, so it may not
/// take a qualified id away from the step that reads it whole: the reseller's
/// router is its own model even on a relay whose prefix is `auto`, and the same
/// statement that names `auto-acme-k3` leaves this id its own numbers.
#[test]
fn a_qualified_house_model_survives_the_relays_prefix() {
    let catalog = prefix_catalog();
    let entry = prefixed_relay(&catalog, "reseller/auto", Some("auto"));
    assert_eq!(
        (matched(&entry), numbers(&entry), rule(&entry),),
        (
            ("reseller", "reseller/auto"),
            numbers(&bundled(&catalog, "reseller", "reseller/auto")),
            "qualified"
        ),
        "the qualified step reads the id the prefix would consume"
    );
}

/// A namespace that names a known provider binds the id to that provider: when
/// nothing under it names the model, another vendor may not claim the id by its
/// final token, so the refusal names the rows that would have.
#[test]
fn a_namespaced_id_the_namespace_provider_does_not_name_is_refused() {
    let catalog = catalog(json!([
        vendor(
            "acme",
            json!([
                row("acme-image-flare", "Acme Image Flare", 100_000, 20_000),
                row("acme-flare-mini", "Acme Flare Mini", 50_000, 10_000),
            ]),
        ),
        vendor(
            "globex",
            json!([row("globex-flare", "Globex Flare", 200_000, 40_000)]),
        ),
    ]));
    let entry = relay(&catalog, "acme/globex-flare");
    assert_eq!(
        (matched(&entry), rule(&entry)),
        (("provider-default", "-"), "ambiguous"),
        "the token `flare` is not an answer under the namespace `acme`"
    );
    assert_eq!(
        entry.capabilities.missing_limits(),
        ["contextWindow", "maxOutputTokens"]
    );
    assert_eq!(candidates(&entry), ["globex/globex-flare"]);
}

/// The namespace binds every identification step, not only the token step: a
/// canonical-id or display-name answer that names another provider is refused
/// too, and the refusal names those rows. Both spellings of one id reach the
/// canonical step, so both carry the same answer.
#[test]
fn the_namespace_refuses_canonical_and_display_name_answers_too() {
    let catalog = catalog(json!([
        vendor(
            "acme",
            json!([row("acme-router", "Acme Router", 8_000, 1_000)])
        ),
        vendor(
            "globex",
            json!([with_canonical(
                row("globex-owl", "Globex Owl", 200_000, 40_000),
                "globex/globex-owl-1017",
            )]),
        ),
    ]));
    for id in ["acme/globex-owl", "acme/Globex Owl"] {
        let entry = relay(&catalog, id);
        assert_eq!(
            (
                matched(&entry),
                rule(&entry),
                candidates(&entry),
                entry.capabilities.missing_limits(),
            ),
            (
                ("provider-default", "-"),
                "ambiguous",
                vec!["globex/globex-owl".to_owned()],
                vec!["contextWindow".to_owned(), "maxOutputTokens".to_owned()],
            ),
            "{id}: the answer is not this namespace's row"
        );
    }
}

/// A merge answers for several sellers at once, so each adopted limit keeps the
/// row it came from: the id takes its window from one reseller and its output
/// cap from another, while `metadataSource` still names the row the entry is
/// copied from.
#[test]
fn a_merged_limit_names_the_seller_it_came_from() {
    let catalog = catalog(json!([
        aggregator(
            "reseller-a",
            json!([row(
                "acme/precision-8b",
                "Acme Precision 8B",
                128_000,
                8_000
            )]),
        ),
        aggregator(
            "reseller-b",
            json!([row(
                "acme/precision-8b",
                "Acme Precision 8B",
                64_000,
                16_000
            )]),
        ),
    ]));
    let entry = relay(&catalog, "acme/precision-8b");
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
            (
                numbers(&bundled(&catalog, "reseller-b", "acme/precision-8b")).0,
                numbers(&bundled(&catalog, "reseller-a", "acme/precision-8b")).1,
            ),
            vec![
                (
                    "contextWindow",
                    64_000,
                    "reseller-b",
                    "acme/precision-8b",
                    "merge"
                ),
                (
                    "maxOutputTokens",
                    8_000,
                    "reseller-a",
                    "acme/precision-8b",
                    "merge"
                ),
            ],
            Some("reseller-a"),
            Some("acme/precision-8b"),
        ),
        "each adopted minimum names the row that declared it"
    );
    assert_eq!(identification.rule, "merge");
}

/// The catalog the merge cases state: two resellers of one model whose declared
/// pairs differ, so neither can answer alone and the minimum is visible.
fn merge_catalog() -> BundledCatalog {
    catalog(json!([
        aggregator(
            "reseller-a",
            json!([row(
                "acme/precision-8b",
                "Acme Precision 8B",
                128_000,
                8_000
            )]),
        ),
        aggregator(
            "reseller-b",
            json!([row(
                "acme/precision-8b",
                "Acme Precision 8B",
                64_000,
                16_000
            )]),
        ),
    ]))
}

/// A limit the descriptor declares is the entry's own number, so the merge's
/// record of an adopted minimum for that field is not a limit this entry
/// inherited: `filled` lists only the ones it did. A value equal to the
/// adopted minimum is still a declaration of its own.
#[test]
fn filled_lists_only_the_limits_the_descriptor_did_not_declare() {
    let catalog = merge_catalog();
    let window = numbers(&bundled(&catalog, "reseller-b", "acme/precision-8b")).0;
    let output = numbers(&bundled(&catalog, "reseller-a", "acme/precision-8b")).1;
    let provider = ProviderProjection {
        provider_id: "custom_relay".into(),
        source: Some(ModelSource::Custom),
        ..Default::default()
    };
    for (descriptor, declared, filled) in [
        (
            json!({"id":"acme/precision-8b","limit":{"context":777,"output":333}}),
            (Some(777), Some(333)),
            vec![],
        ),
        (
            json!({"id":"acme/precision-8b","limit":{"context":777}}),
            (Some(777), output),
            vec![("maxOutputTokens", 8_000, "reseller-a")],
        ),
        (
            json!({"id":"acme/precision-8b","limit":{"context":64_000}}),
            (window, output),
            vec![("maxOutputTokens", 8_000, "reseller-a")],
        ),
        (
            json!({"id":"acme/precision-8b","limit":{"context":64_000,"output":8_000}}),
            (window, output),
            vec![],
        ),
    ] {
        let entry = catalog.project(&provider, &descriptor).unwrap();
        let listed = adopted(&entry);
        assert_eq!(
            (numbers(&entry), listed),
            (declared, filled),
            "{descriptor}: a declared limit has no filled record"
        );
    }
}

/// The same provenance on the list a relay serves: `effective_models` projects
/// every row of a custom provider, and a row that declares neither limit takes
/// both from the merge with the seller of each.
#[test]
fn a_merge_carries_the_sellers_through_the_effective_models() {
    let catalog = merge_catalog();
    let row = stored("acme/precision-8b");
    let provider = ProviderProjection {
        provider_id: "my-gateway".into(),
        source: Some(ModelSource::Custom),
        models: vec![row.clone()],
        ..Default::default()
    };
    let effective = catalog.effective_models(&provider, std::slice::from_ref(&row), &[]);
    let identification = identification(&effective[0]);
    assert_eq!(
        (
            numbers(&effective[0]),
            identification.rule.as_str(),
            adopted(&effective[0]),
        ),
        (
            (
                numbers(&bundled(&catalog, "reseller-b", "acme/precision-8b")).0,
                numbers(&bundled(&catalog, "reseller-a", "acme/precision-8b")).1,
            ),
            "merge",
            vec![
                ("contextWindow", 64_000, "reseller-b"),
                ("maxOutputTokens", 8_000, "reseller-a"),
            ],
        )
    );
}

/// Every aggregator row that names the same model with the same tag joins, and
/// the merge takes the safe direction: the smallest window and the smallest
/// output cap any joining row declares, each naming the row it came from.
#[test]
fn an_aggregator_only_model_merges_the_minimum() {
    let catalog = catalog(json!([
        aggregator(
            "reseller-a",
            json!([row(
                "acme/precision-8b",
                "Acme Precision 8B",
                128_000,
                8_000
            )]),
        ),
        aggregator(
            "reseller-b",
            json!([row(
                "acme/precision-8b",
                "Acme Precision 8B",
                64_000,
                16_000
            )]),
        ),
        aggregator(
            "reseller-c",
            json!([row(
                "acme/precision-8b",
                "Acme Precision 8B",
                256_000,
                32_000
            )]),
        ),
    ]));
    let entry = relay(&catalog, "acme/precision-8b");
    assert_eq!(
        numbers(&entry),
        (
            numbers(&bundled(&catalog, "reseller-b", "acme/precision-8b")).0,
            numbers(&bundled(&catalog, "reseller-a", "acme/precision-8b")).1,
        ),
        "the smallest window and the smallest output cap any joining row states"
    );
    assert_eq!(rule(&entry), "merge");
    assert_eq!(
        adopted(&entry),
        [
            ("contextWindow", 64_000, "reseller-b"),
            ("maxOutputTokens", 8_000, "reseller-a"),
        ],
        "each adopted minimum names the row that declared it"
    );
    assert_eq!(matched(&entry).1, "acme/precision-8b");
}

/// The efforts are the intersection over the rows that declare any, never the
/// union: an option only some sellers accept is not offered.
#[test]
fn merged_efforts_are_the_intersection() {
    let catalog = catalog(json!([
        aggregator(
            "reseller-a",
            json!([with_efforts(
                row("acme/precision-8b", "Acme Precision 8B", 128_000, 8_000),
                &["low", "medium", "high"],
            )]),
        ),
        aggregator(
            "reseller-b",
            json!([with_efforts(
                row("acme/precision-8b", "Acme Precision 8B", 128_000, 8_000),
                &["low", "high", "max"],
            )]),
        ),
    ]));
    let entry = relay(&catalog, "acme/precision-8b");
    assert_eq!(rule(&entry), "merge");
    let mut merged = efforts(&entry).expect("the merge states the efforts it agreed on");
    merged.sort();
    assert_eq!(merged, ["high", "low"]);
    for seller in ["reseller-a", "reseller-b"] {
        let declared = efforts(&bundled(&catalog, seller, "acme/precision-8b")).unwrap();
        assert!(
            merged.iter().all(|effort| declared.contains(effort)),
            "{seller}: {merged:?} is not inside {declared:?}"
        );
    }
}

/// A row whose own id carries a tag joins only an input carrying that tag: the
/// tagged input merges the tagged row, and an untagged input never does.
#[test]
fn a_tagged_row_does_not_join_an_untagged_input() {
    let catalog = catalog(json!([
        aggregator(
            "reseller-a",
            json!([
                row("acme/precision-8b", "Acme Precision 8B", 128_000, 8_000),
                row(
                    "acme/precision-8b:batch",
                    "Acme Precision 8B (batch)",
                    256_000,
                    32_000,
                ),
            ]),
        ),
        aggregator(
            "reseller-b",
            json!([row(
                "acme/precision-8b",
                "Acme Precision 8B",
                64_000,
                16_000
            )]),
        ),
    ]));
    let tagged = relay(&catalog, "acme/precision-8b:batch");
    let bare = relay(&catalog, "acme/precision-8b");
    assert_eq!(rule(&tagged), "merge");
    assert_eq!(rule(&bare), "merge");
    assert_eq!(
        matched(&tagged).1,
        "acme/precision-8b:batch",
        "the tagged input merges the tagged row"
    );
    assert_eq!(
        numbers(&tagged),
        numbers(&bundled(&catalog, "reseller-a", "acme/precision-8b:batch")),
        "no untagged row joins the tagged input"
    );
    assert_eq!(
        matched(&bare).1,
        "acme/precision-8b",
        "the untagged input merges an untagged row"
    );
    assert_eq!(
        numbers(&bare),
        (
            numbers(&bundled(&catalog, "reseller-b", "acme/precision-8b")).0,
            numbers(&bundled(&catalog, "reseller-a", "acme/precision-8b")).1,
        ),
        "the untagged input takes the untagged rows' minimum"
    );
}

/// A listed chat id whose only joining rows are media rows is answered by
/// none of them. The entry keeps the name, the window, the output cap and the
/// reasoning toggle the listing declared, and reports no identification at
/// all, so the picker says nothing named the model instead of naming a video
/// row as the model it was taken to be.
#[test]
fn a_chat_id_joins_no_row_of_another_kind() {
    let catalog = catalog(json!([
        aggregator(
            "video-reseller",
            json!([media_row(
                "acme-omni-3",
                "Acme Omni 3",
                "video-generation",
                8_192,
                8_192,
            )]),
        ),
        aggregator(
            "image-reseller",
            json!([media_row(
                "acme-omni-3",
                "Acme Omni 3",
                "image-generation",
                8_192,
                8_192,
            )]),
        ),
    ]));
    let declared = chat_row("acme-omni-3", "Acme Omni 3", 1_000_000, 128_000);
    let declared_efforts = efforts(&declared).expect("the listing states its efforts");
    let declared_numbers = numbers(&declared);
    for reseller in ["video-reseller", "image-reseller"] {
        let joining = bundled(&catalog, reseller, "acme-omni-3");
        assert_eq!(
            numbers(&joining),
            (Some(8_192), Some(8_192)),
            "{reseller}'s placeholder pair is what a kind-blind merge would adopt"
        );
    }
    let provider = ProviderProjection {
        provider_id: "my-gateway".into(),
        source: Some(ModelSource::Custom),
        models: vec![declared.clone()],
        ..Default::default()
    };
    let effective = catalog.effective_models(&provider, std::slice::from_ref(&declared), &[]);
    let entry = &effective[0];
    assert!(
        entry.metadata.as_ref().unwrap().identification.is_none(),
        "no step named the model, so the entry reports no identification"
    );
    assert_eq!(
        (entry.display_name.as_str(), numbers(entry)),
        (declared.display_name.as_str(), declared_numbers),
        "the entry keeps the name and the pair its own listing declared"
    );
    assert_eq!(
        efforts(entry),
        Some(declared_efforts),
        "the entry keeps the efforts its own listing declared"
    );
    assert!(
        entry.capabilities.reasoning,
        "a media row that declares reasoning unsupported never turns it off"
    );
    assert_eq!(
        matched(entry),
        ("provider-default", "-"),
        "neither media row is named as the model this id was taken to be"
    );
}

/// A chat id beside a chat row and a media row of the same name merges with
/// the chat row alone: the numbers are the chat row's, and the media row's
/// placeholder pair takes no part in the minimum.
#[test]
fn a_chat_id_joins_the_chat_row_beside_a_media_row_of_the_same_name() {
    let catalog = catalog(json!([
        aggregator(
            "chat-reseller",
            json!([with_efforts(
                with_kind(row("acme-omni-3", "Acme Omni 3", 200_000, 64_000), "chat"),
                &["low", "high"],
            )]),
        ),
        aggregator(
            "video-reseller",
            json!([media_row(
                "acme-omni-3",
                "Acme Omni 3",
                "video-generation",
                8_192,
                8_192,
            )]),
        ),
    ]));
    let entry = kinded_relay(&catalog, "acme-omni-3", "chat");
    let chat = bundled(&catalog, "chat-reseller", "acme-omni-3");
    let media = bundled(&catalog, "video-reseller", "acme-omni-3");
    assert!(
        numbers(&media).0 < numbers(&chat).0,
        "the media row's pair is the smaller one, so a kind-blind join would take it"
    );
    assert_eq!(rule(&entry), "merge");
    assert_eq!(
        (numbers(&entry), efforts(&entry)),
        (numbers(&chat), efforts(&chat)),
        "the merge is the chat row's own numbers and efforts"
    );
    assert_eq!(matched(&entry), ("chat-reseller", "acme-omni-3"));
    assert_eq!(
        adopted(&entry),
        [
            ("contextWindow", 200_000, "chat-reseller"),
            ("maxOutputTokens", 64_000, "chat-reseller"),
        ],
        "the media row supplies no limit"
    );
}

/// A row whose `raw.type` is `"model"` names the JSON object rather than a
/// kind, so it joins a chat id as a row stating the chat kind does; a row that
/// states no type at all joins one too.
#[test]
fn a_row_stating_no_kind_of_its_own_joins_a_chat_id() {
    let catalog = catalog(json!([
        aggregator(
            "kinds-reseller",
            json!([with_raw(
                row("acme-omni-3", "Acme Omni 3", 128_000, 8_000),
                json!({ "type": "model" }),
            )]),
        ),
        aggregator(
            "bare-reseller",
            json!([row("acme-omni-3", "Acme Omni 3", 64_000, 16_000)]),
        ),
    ]));
    let entry = kinded_relay(&catalog, "acme-omni-3", "chat");
    assert_eq!(rule(&entry), "merge");
    assert_eq!(
        numbers(&entry),
        (
            numbers(&bundled(&catalog, "bare-reseller", "acme-omni-3")).0,
            numbers(&bundled(&catalog, "kinds-reseller", "acme-omni-3")).1,
        ),
        "both rows join, so both minima are taken"
    );
    assert_eq!(
        adopted(&entry),
        [
            ("contextWindow", 64_000, "bare-reseller"),
            ("maxOutputTokens", 8_000, "kinds-reseller"),
        ],
        "a row stating `model` and a row stating no kind both supply a limit"
    );
}

/// The kind rule holds on both paths the id's kind travels through here: a
/// media id joins a row of its own kind and refuses a chat row, and a chat id
/// takes the chat row. `effective_models` reads a stored row's kind and
/// `project` reads a listed descriptor's, which are the two paths the kind
/// reaches identification through.
///
/// `identify_entry` is the third caller of that chain and is `pub(crate)`, so
/// an integration test cannot reach it; its stored-row case is the one
/// `effective_models` states here, because both read the kind from the entry
/// they are handed.
#[test]
fn a_media_id_joins_only_a_row_of_its_own_kind_on_every_path() {
    let catalog = catalog(json!([
        aggregator(
            "media-reseller",
            json!([media_row(
                "acme-omni-3",
                "Acme Omni 3",
                "image-generation",
                8_192,
                4_096,
            )]),
        ),
        aggregator(
            "chat-reseller",
            json!([row("acme-omni-3", "Acme Omni 3", 200_000, 64_000)]),
        ),
    ]));
    let media = bundled(&catalog, "media-reseller", "acme-omni-3");
    let chat = bundled(&catalog, "chat-reseller", "acme-omni-3");
    assert!(
        numbers(&media).0 < numbers(&chat).0,
        "a kind-blind merge would take the media row's smaller window"
    );
    let projection = ProviderProjection {
        provider_id: "my-gateway".into(),
        source: Some(ModelSource::Custom),
        models: vec![media.clone()],
        ..Default::default()
    };

    let effective = catalog.effective_models(&projection, std::slice::from_ref(&media), &[]);
    assert_eq!(
        (rule(&effective[0]), numbers(&effective[0])),
        ("merge", numbers(&media)),
        "an image-generation id merges the row of its own kind"
    );
    assert_eq!(
        adopted(&effective[0]),
        [
            ("contextWindow", 8_192, "media-reseller"),
            ("maxOutputTokens", 4_096, "media-reseller"),
        ],
        "the chat row supplies nothing to a media id"
    );

    let listed = |kind: &str| {
        catalog
            .project(
                &ProviderProjection {
                    provider_id: "my-gateway".into(),
                    source: Some(ModelSource::Custom),
                    ..Default::default()
                },
                &json!({ "id": "acme-omni-3", "raw": { "type": kind } }),
            )
            .unwrap()
    };
    let image = listed("image-generation");
    assert_eq!(
        (rule(&image), numbers(&image)),
        ("merge", numbers(&media)),
        "a listed image-generation id takes the media row"
    );
    let chatted = listed("chat");
    assert_eq!(
        (rule(&chatted), numbers(&chatted)),
        ("merge", numbers(&chat)),
        "a listed chat id takes the chat row the media row does not outrank"
    );
}

/// An id that states no kind keeps every row of its name, so a media model a
/// relay lists without a `type` still merges and still carries the media kind
/// it inherited. That inherited `type` is what keeps it out of conversations.
#[test]
fn a_no_kind_id_keeps_a_media_row_of_one_kind() {
    let catalog = catalog(json!([aggregator(
        "image-reseller",
        json!([media_row(
            "acme-omni-3",
            "Acme Omni 3",
            "image-generation",
            32_768,
            4_096,
        )]),
    )]));
    let entry = relay(&catalog, "acme-omni-3");
    let row = bundled(&catalog, "image-reseller", "acme-omni-3");
    assert_eq!(rule(&entry), "merge");
    assert_eq!(
        entry.metadata.as_ref().unwrap().raw.get("type"),
        row.metadata.as_ref().unwrap().raw.get("type"),
        "the entry carries the media kind it inherited from the row"
    );
    assert_eq!(numbers(&entry), numbers(&row));
}

/// A no-kind id beside a chat row and a media row of the same name merges with
/// the chat row only: the numbers are the chat row's and the media row's pair
/// is not in the minimum.
#[test]
fn a_no_kind_id_beside_a_chat_row_takes_only_the_chat_row() {
    let catalog = catalog(json!([
        aggregator(
            "chat-reseller",
            json!([with_kind(
                row("acme-omni-3", "Acme Omni 3", 200_000, 64_000),
                "chat",
            )]),
        ),
        aggregator(
            "video-reseller",
            json!([media_row(
                "acme-omni-3",
                "Acme Omni 3",
                "video-generation",
                8_192,
                8_192,
            )]),
        ),
    ]));
    let entry = relay(&catalog, "acme-omni-3");
    let chat = bundled(&catalog, "chat-reseller", "acme-omni-3");
    assert_eq!(rule(&entry), "merge");
    assert_eq!(
        numbers(&entry),
        numbers(&chat),
        "the chat row is the only row the merge reads"
    );
    assert_eq!(matched(&entry), ("chat-reseller", "acme-omni-3"));
    assert_eq!(
        adopted(&entry),
        [
            ("contextWindow", 200_000, "chat-reseller"),
            ("maxOutputTokens", 64_000, "chat-reseller"),
        ],
        "the media row's smaller pair is not in the minimum"
    );
    assert_eq!(
        entry.metadata.as_ref().unwrap().raw.get("type"),
        chat.metadata.as_ref().unwrap().raw.get("type"),
        "the entry carries the chat row's `type`"
    );
}

/// A no-kind id whose only joining rows are media rows of two different kinds
/// still merges both, which is what a listing that states no kind does today.
#[test]
fn a_no_kind_id_keeps_media_rows_of_two_kinds() {
    let catalog = catalog(json!([
        aggregator(
            "video-reseller",
            json!([media_row(
                "acme-omni-3",
                "Acme Omni 3",
                "video-generation",
                128_000,
                16_384,
            )]),
        ),
        aggregator(
            "audio-reseller",
            json!([media_row(
                "acme-omni-3",
                "Acme Omni 3",
                "audio",
                64_000,
                32_768,
            )]),
        ),
    ]));
    let entry = relay(&catalog, "acme-omni-3");
    let video = bundled(&catalog, "video-reseller", "acme-omni-3");
    let audio = bundled(&catalog, "audio-reseller", "acme-omni-3");
    assert_ne!(
        numbers(&video),
        numbers(&audio),
        "the two rows declare different pairs, so each minimum names one row"
    );
    assert_eq!(rule(&entry), "merge");
    assert_eq!(
        numbers(&entry),
        (
            numbers(&audio).0.min(numbers(&video).0),
            numbers(&audio).1.min(numbers(&video).1),
        ),
        "both rows join, so the smaller window and the smaller cap are taken"
    );
}

/// A listed chat id whose only joining rows are media rows stays unidentified
/// on the projection path too, and the projection says so rather than naming
/// the row it refused.
#[test]
fn a_chat_id_naming_only_media_rows_stays_unidentified_on_a_projection() {
    let catalog = catalog(json!([aggregator(
        "video-reseller",
        json!([media_row(
            "acme-omni-3",
            "Acme Omni 3",
            "video-generation",
            8_192,
            8_192,
        )]),
    ),]));
    let provider = ProviderProjection {
        provider_id: "my-gateway".into(),
        source: Some(ModelSource::Custom),
        ..Default::default()
    };
    let entry = catalog
        .project(
            &provider,
            &json!({
                "id": "acme-omni-3",
                "raw": { "type": "chat" },
                "limit": { "context": 1_000_000, "output": 128_000 },
                "reasoning": { "supported": true },
            }),
        )
        .unwrap();
    let metadata = entry.metadata.as_ref().unwrap();
    assert!(metadata.identification.is_none(), "no step named the model");
    assert_eq!(
        (
            metadata.matched_provider_id.as_deref(),
            metadata.matched_model_id.as_deref(),
            metadata.match_strategy.as_deref(),
        ),
        (None, None, Some("provider-default")),
        "a refused merge leaves no matched row and no merge strategy"
    );
    assert_eq!(
        numbers(&entry),
        (Some(1_000_000), Some(128_000)),
        "the projection keeps the pair the descriptor declared"
    );
    assert_eq!(
        metadata.metadata_source.as_deref(),
        Some("provider-default"),
        "no media row supplies the entry's metadata"
    );
    assert_eq!(
        entry.display_name, "acme-omni-3",
        "the media row's display name is not adopted"
    );
}

/// Two rows that declare one set of numbers are one model, so they settle a
/// step instead of reporting an ambiguity even when the seller names differ.
#[test]
fn an_ambiguous_step_collapses_rows_that_agree() {
    let catalog = catalog(json!([
        vendor(
            "acme",
            json!([row("acme-k3", "Acme K3", 1_048_576, 1_048_576)]),
        ),
        vendor(
            "acme-mirror",
            json!([row("acme-k3", "Acme K3", 1_048_576, 1_048_576)]),
        ),
    ]));
    let entry = relay(&catalog, "acme-k3");
    assert_eq!(rule(&entry), "token");
    assert!(identification(&entry).candidates.is_empty());
}

/// A provider's placeholder pair is its default, not a declaration: the static
/// path that serves it fills the two limits from the entry the id identifies,
/// records where they came from, and leaves the row's own capabilities alone.
#[test]
fn a_placeholder_limit_is_not_a_declaration() {
    let catalog = catalog(json!([
        vendor("acme", json!([row("acme-5", "Acme 5", 400_000, 128_000)]),),
        aggregator(
            "tokenflux",
            json!([with_tool_calling(
                row("acme/acme-5", "acme/acme-5", 4_096, 4_096),
                false
            )]),
        ),
    ]));
    let row = bundled(&catalog, "tokenflux", "acme/acme-5");
    let vendor = bundled(&catalog, "acme", "acme-5");
    assert_eq!(numbers(&row), (Some(4_096), Some(4_096)));
    let provider = ProviderProjection {
        provider_id: "tokenflux".into(),
        source: Some(ModelSource::Builtin),
        models: catalog.bundled_models("tokenflux").unwrap().to_vec(),
        ..Default::default()
    };
    let effective = catalog.effective_models(&provider, std::slice::from_ref(&row), &[]);
    let filled = &effective[0];
    assert_eq!(numbers(filled), numbers(&vendor));
    assert_ne!(numbers(filled), numbers(&row));
    assert!(
        !filled.capabilities.tool_calling,
        "the row keeps its own API"
    );
    let identification = identification(filled);
    assert_eq!(identification.rule, "provider-exact");
    assert_eq!(identification.filled.len(), 2, "{identification:?}");
    assert!(
        identification
            .filled
            .iter()
            .all(|limit| limit.provider_id == "acme"),
        "{identification:?}"
    );
    let fields: Vec<_> = identification
        .filled
        .iter()
        .map(|limit| limit.field.as_str())
        .collect();
    assert_eq!(fields, ["contextWindow", "maxOutputTokens"]);
}

/// The outcome reaches `metadata.identification` for every step: the narrowed
/// canonical answer states that name and no candidates, the token answer names
/// its step, and an ambiguity carries the rows that disagree without becoming
/// a projection error. An id nothing names carries no identification.
#[test]
fn the_identification_reaches_the_entry_and_names_an_ambiguity() {
    let catalog = catalog(json!([
        vendor(
            "acme",
            json!([
                with_canonical(
                    row("acme-v4-flash", "Acme V4 Flash", 200_000, 64_000),
                    "acme/acme-v4.1-flash-1017",
                ),
                with_canonical(
                    row("acme-flash", "Acme V4.1 Flash", 1_000_000, 128_000),
                    "acme/acme-v4.1-flash-1017",
                ),
                row("acme-router", "Acme Router", 8_000, 1_000),
            ]),
        ),
        vendor(
            "globex",
            json!([
                row("globex-router", "Globex Router", 16_000, 2_000),
                row("shared-model", "Shared Model", 16_000, 2_000),
            ]),
        ),
        vendor(
            "initech",
            json!([row("shared-model", "Shared Model", 32_000, 4_000)]),
        ),
    ]));
    let identified = relay(&catalog, "acme-v4.1-flash");
    let found: &ModelIdentification = identification(&identified);
    assert_eq!(found.rule, "canonical");
    assert!(found.narrowed_by_display_name);
    assert_eq!(matched(&identified), ("acme", "acme-flash"));
    assert!(found.candidates.is_empty());
    assert!(found.filled.is_empty());

    let exact = relay(&catalog, "acme-router");
    assert_eq!(rule(&exact), "token");

    let ambiguous = relay(&catalog, "shared-model");
    assert_eq!(rule(&ambiguous), "ambiguous");
    assert_eq!(
        candidates(&ambiguous),
        ["globex/shared-model", "initech/shared-model"]
    );
    assert_eq!(
        ambiguous.capabilities.missing_limits(),
        ["contextWindow", "maxOutputTokens"],
        "an ambiguity supplies no numbers"
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

/// A relay whose ids carry its own prefix resolves the model under the
/// stripped id: the strip happens once, at the head, so the numbers are the
/// bare id's own and the step that named them is the token step.
#[test]
fn a_providers_glued_prefix_is_stripped_before_matching() {
    let catalog = prefix_catalog();
    let entry = prefixed_relay(&catalog, "auto-acme-k3", Some("auto"));
    assert_eq!(matched(&entry), ("acme", "acme-k3"));
    assert_eq!(
        numbers(&entry),
        numbers(&bundled(&catalog, "acme", "acme-k3"))
    );
    assert_eq!(rule(&entry), "token");
    // The strategy reports against the id the provider listed, not the
    // stripped one, so the entry says how its own id was read.
    assert_eq!(
        entry.metadata.as_ref().unwrap().match_strategy.as_deref(),
        Some("acme-token")
    );
    let bracketed = BundledCatalog::from_value(json!({"providers":[vendor(
        "acme",
        json!([row("acme-k3[1m]", "Acme K3", 1_048_576, 1_048_576)]),
    )]}))
    .unwrap();
    for id in ["auto-acme-k3", "auto-acme-k3[1m]:latest"] {
        let entry = prefixed_relay(&bracketed, id, Some("auto"));
        assert_eq!(matched(&entry), ("acme", "acme-k3[1m]"));
        assert_eq!(rule(&entry), "token");
    }
}

/// The same provider without the statement is unidentified, so the field is
/// what identifies rather than a guess about the id shape.
#[test]
fn a_prefixed_id_without_the_statement_is_unidentified() {
    let catalog = prefix_catalog();
    let entry = relay(&catalog, "auto-acme-k3");
    assert_eq!(matched(&entry), ("provider-default", "-"));
    assert_eq!(
        entry.capabilities.missing_limits(),
        ["contextWindow", "maxOutputTokens"]
    );
    assert!(
        entry.metadata.as_ref().unwrap().identification.is_none(),
        "nothing named the model, so there is no identification"
    );
}

/// Step 1 reads the whole id, so an exact row still wins with the prefix set,
/// and an id the prefix does not head is left as it is.
#[test]
fn an_exact_row_still_wins_with_the_prefix_set() {
    let catalog = prefix_catalog();
    // The bundle carries only `acme-k3`, so step 1 has no `auto-acme-k3` row
    // to read and answers from the model this provider added itself.
    let provider = ProviderProjection {
        provider_id: "acme".into(),
        source: Some(ModelSource::Builtin),
        models: vec![
            catalog
                .project(
                    &ProviderProjection {
                        provider_id: "acme".into(),
                        source: Some(ModelSource::Custom),
                        ..Default::default()
                    },
                    &json!({ "id": "auto-acme-k3" }),
                )
                .unwrap(),
        ],
        glued_prefix: Some("auto".into()),
        ..Default::default()
    };
    let entry = catalog
        .project(&provider, &json!({ "id": "auto-acme-k3" }))
        .unwrap();
    assert_eq!(
        matched(&entry),
        ("acme", "auto-acme-k3"),
        "the provider's own row answers before any step reads the prefix"
    );
    assert_eq!(rule(&entry), "provider-exact");
}

/// The prefix is a token head, so `auto` and `auto-` are one statement, and a
/// prefix that consumes the whole id leaves nothing to identify.
#[test]
fn the_prefix_is_normalised_and_must_leave_a_name() {
    let catalog = prefix_catalog();
    for prefix in ["auto", "auto-", " AUTO "] {
        let entry = prefixed_relay(&catalog, "auto-acme-k3", Some(prefix));
        assert_eq!(
            matched(&entry),
            ("acme", "acme-k3"),
            "the prefix {prefix:?} is the same statement"
        );
    }
    let empty = prefixed_relay(&catalog, "auto-", Some("auto"));
    assert_eq!(matched(&empty), ("provider-default", "-"));
    assert!(
        empty.metadata.as_ref().unwrap().identification.is_none(),
        "a prefix that leaves no name identifies nothing"
    );
}

/// The catalog the prefix cases state: one vendor row, one reseller router, and
/// their own ids.
fn prefix_catalog() -> BundledCatalog {
    catalog(json!([
        vendor(
            "acme",
            json!([row("acme-k3", "Acme K3", 1_048_576, 1_048_576)]),
        ),
        aggregator(
            "reseller",
            json!([row("reseller/auto", "Auto Router", 8_192, 8_192)]),
        ),
    ]))
}

/// The relay projection with the provider's own statement that a listed id
/// stands for another model.
fn aliased_relay(catalog: &BundledCatalog, id: &str, target: &str) -> ModelEntry {
    let provider = ProviderProjection {
        provider_id: "my-gateway".into(),
        source: Some(ModelSource::Custom),
        model_aliases: BTreeMap::from([(id.to_owned(), target.to_owned())]),
        ..Default::default()
    };
    catalog.project(&provider, &json!({ "id": id })).unwrap()
}

/// A provider's own statement that a listed id stands for another model is
/// what that id resolves through: the target goes through the same steps, so
/// the answer carries the target row's numbers and rule, and no second
/// vocabulary is introduced for the fact that an alias was used.
///
/// A `provider/model` target answers through the token step on a relay, not
/// through the qualified one: the qualified index keys a row by the id the row
/// carries itself, so a target that no row spells whole is read by its tokens,
/// with its namespace binding those tokens to that provider. The rule word is
/// therefore `token`, and the strategy keeps the target's own provider.
#[test]
fn a_provider_alias_resolves_an_id_through_the_pipeline() {
    let catalog = alias_catalog();
    let entry = aliased_relay(&catalog, "acme-code-latest", "acme/acme-code-latest");
    assert_eq!(matched(&entry), ("acme", "acme-code-latest"));
    assert_eq!(
        numbers(&entry),
        numbers(&bundled(&catalog, "acme", "acme-code-latest"))
    );
    assert_eq!(rule(&entry), "token");
    assert_eq!(
        entry.metadata.as_ref().unwrap().match_strategy.as_deref(),
        Some("acme-token")
    );
}

/// An alias on an id no rule can read answers with the target's own rule, not
/// a rule of its own.
#[test]
fn an_alias_on_an_unreadable_id_takes_the_targets_rule() {
    let catalog = alias_catalog();
    let entry = aliased_relay(&catalog, "my-deployment-acme-5", "acme-5.5");
    assert_eq!(matched(&entry), ("acme", "acme-5.5"));
    assert_eq!(
        numbers(&entry),
        numbers(&bundled(&catalog, "acme", "acme-5.5"))
    );
    assert_eq!(rule(&entry), "token");
}

/// An alias whose target is itself ambiguous returns the target's candidates:
/// an alias names one model, and it may not choose a side of a disagreement.
#[test]
fn an_alias_onto_an_ambiguous_target_stays_ambiguous() {
    let catalog = alias_catalog();
    let entry = aliased_relay(&catalog, "my-deployment-acme", "acme-code-latest");
    assert_eq!(matched(&entry), ("provider-default", "-"));
    assert_eq!(
        entry.capabilities.missing_limits(),
        ["contextWindow", "maxOutputTokens"]
    );
    assert_eq!(rule(&entry), "ambiguous");
    assert_eq!(candidates(&entry), contested());
}

/// Without the statement `acme-code-latest` keeps the ambiguity and its
/// candidates, so the alias is the only change to the id, and an alias that
/// names nothing leaves the id exactly as it was without one.
#[test]
fn an_unaliased_ambiguous_id_keeps_its_candidates_and_a_stale_alias_adds_nothing() {
    let catalog = alias_catalog();
    let entry = relay(&catalog, "acme-code-latest");
    assert_eq!(rule(&entry), "ambiguous");
    assert_eq!(candidates(&entry), contested());
    let stale = aliased_relay(&catalog, "acme-code-latest", "zzzz-nothing-names-this");
    assert_eq!(
        (rule(&stale), candidates(&stale)),
        ("ambiguous", contested()),
        "a target that names nothing falls through to the merge as if there was no alias"
    );
}

/// The catalog the alias cases state: a vendor row its own namespace spells
/// whole, one id two vendors sell at different numbers, and a row no other id
/// names.
fn alias_catalog() -> BundledCatalog {
    catalog(json!([
        vendor(
            "acme",
            json!([
                row("acme-code-latest", "Acme Code Latest", 204_800, 60_000),
                row("acme-5.5", "Acme 5.5", 1_050_000, 128_000),
            ]),
        ),
        vendor(
            "globex",
            json!([row("acme-code-latest", "Acme Code Latest", 256_000, 60_000)]),
        ),
    ]))
}

/// The two `acme-code-latest` rows the case states, which disagree on the
/// window and so may not supply numbers.
fn contested() -> Vec<String> {
    ["acme/acme-code-latest", "globex/acme-code-latest"]
        .map(str::to_owned)
        .to_vec()
}
