use super::*;
use dim_app_server::provider::{ManagementPageRequest, ModelQuery, UpdateReplyKind};

#[tokio::test]
async fn indexed_names_match_full_catalog_across_pages_and_enrichment() {
    let home = temp_home("indexed-provider-names");
    let (root, provider) = provider_root(&home).await;
    let cancel = CancellationToken::new();
    let id = "00000000-0000-4000-8000-000000001668";
    let rows = [
        ("gpt-4o", "gpt-4o", None),
        ("probe-other", "GPT-4o", None),
        ("alias", "alias", None),
        ("a", "\u{2003}Shared\u{a0}", Some("Ignored · Channel")),
        ("a 2", "Shared", Some("Ignored · Channel")),
        ("reserved", "Shared · a", None),
        ("counter", "Shared · a 2", None),
        ("c", "Shared", Some("Ignored · Reserved")),
        ("label", "Shared · Reserved", None),
    ];
    let mut config = input(
        id,
        "http://127.0.0.1:1/v1",
        StaticAuth::Anonymous,
        None,
        "names",
        43,
    );
    config.models = rows
        .iter()
        .map(|(id, name, label)| {
            let mut entry = model(id, 43);
            entry.display_name = (*name).into();
            if let Some(label) = label {
                entry
                    .metadata
                    .get_or_insert_default()
                    .raw
                    .insert("magpie_label".into(), json!(label));
            }
            entry
        })
        .collect();
    config.models[4].status = ModelStatus::Unavailable;
    provider.create(config, &cancel).await.unwrap();
    for phase in 0..3 {
        let catalog = provider
            .catalog_for(id.into(), &cancel)
            .await
            .unwrap()
            .unwrap();
        let expected: std::collections::BTreeMap<_, _> = catalog
            .models
            .iter()
            .map(|m| (m.id.clone(), m.name.clone()))
            .collect();
        assert_eq!(expected["gpt-4o"], "GPT-4o · gpt-4o");
        let mut after = None;
        let mut actual = std::collections::BTreeMap::new();
        loop {
            let page = provider
                .query_models(
                    ModelQuery {
                        provider_id: Some(id.into()),
                        after,
                        limit: Some(1),
                        ..Default::default()
                    },
                    &cancel,
                )
                .await
                .unwrap();
            for row in page.models {
                actual.insert(row.model.id, row.model.name);
            }
            after = page.next;
            if after.is_none() {
                break;
            }
        }
        assert_eq!(actual, expected);
        for (model_id, name) in expected {
            let exact = provider
                .management(id.into(), Some(model_id), &cancel)
                .await
                .unwrap();
            let row = &exact.providers[0].models.as_ref().unwrap()[0];
            assert_eq!(
                row.catalog_name
                    .as_deref()
                    .unwrap_or(&row.model.display_name),
                name
            );
        }
        if phase == 0 {
            provider
                .update_model_config(
                    id.into(),
                    catalog.version,
                    "alias".into(),
                    serde_json::from_value(
                        json!({"alias":{"action":"set","value":"openai/gpt-4o"}}),
                    )
                    .unwrap(),
                    &cancel,
                )
                .await
                .unwrap();
        } else if phase == 1 {
            let changed = provider
                .set_enabled_models(
                    id.into(),
                    catalog.version,
                    catalog
                        .models
                        .iter()
                        .filter(|m| m.id != "probe-other")
                        .map(|m| m.id.clone())
                        .collect(),
                    &cancel,
                )
                .await
                .unwrap();
            provider
                .update_model_config(
                    id.into(),
                    changed.version,
                    "a".into(),
                    serde_json::from_value(json!({"displayName":"GPT-4o"})).unwrap(),
                    &cancel,
                )
                .await
                .unwrap();
            let db = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
            db.execute("UPDATE provider_bundle SET naming_revision='previous-naming-generation' WHERE provider_id=?1",[id]).unwrap();
        }
    }
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn sparse_startup_summary_cas_and_bounded_payload_reads() {
    let home = temp_home("indexed-provider-work");
    let (root, provider) = provider_root(&home).await;
    let cancel = CancellationToken::new();
    let db = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM providers", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM provider_models", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        0
    );
    let summary = provider
        .management_page(
            ManagementPageRequest {
                provider_id: Some("deepseek".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(summary.providers[0].version, Some(1));
    let updated=provider.update_reply("deepseek".into(),1,serde_json::from_value(json!({"enabled":true,"credential":{"action":"set_ref","reference":"fixture-reference"}})).unwrap(),UpdateReplyKind::Summary,&cancel).await.unwrap();
    let updated = serde_json::to_value(updated).unwrap();
    assert_eq!(updated["version"], 2);
    assert!(updated.get("models").is_none());
    assert!(!updated.to_string().contains("fixture-reference"));
    assert!(!dim_app_server::provider::testing::catalog_client_initialized(&provider));
    assert_eq!(
        db.query_row("SELECT count(*) FROM provider_models", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        0
    );
    assert!(matches!(
        provider
            .update_reply(
                "deepseek".into(),
                1,
                serde_json::from_value(json!({"enabled":false})).unwrap(),
                UpdateReplyKind::Summary,
                &cancel
            )
            .await,
        Err(ProviderError::Conflict(_))
    ));
    let id = "00000000-0000-4000-8000-000000001669";
    let mut config = input(
        id,
        "http://127.0.0.1:1/v1",
        StaticAuth::Anonymous,
        None,
        "work",
        43,
    );
    config.connection.driver_kind = "azure-openai".into();
    config.connection.base_url = "https://fixture.openai.azure.com/openai".into();
    config.models = (0..4096)
        .map(|i| {
            let mut entry = model(&format!("unrelated-{i:04}"), 43);
            entry.display_name = format!("Distinct {i:04}");
            entry
        })
        .collect();
    provider.create(config, &cancel).await.unwrap();
    let page = provider
        .query_models(
            ModelQuery {
                provider_id: Some(id.into()),
                limit: Some(32),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(page.models.len(), 32);
    eprintln!("4096-row page work: {:?}", page.work);
    assert_eq!(page.work.candidate_keys, 33);
    assert!(page.work.candidate_vm_steps < 1000, "{:?}", page.work);
    assert!(page.work.naming_rows <= 96, "{:?}", page.work);
    assert_eq!(page.work.stored_payloads, 32);
    assert_eq!(page.work.source_payloads, 0);
    let exact = provider
        .query_models(
            ModelQuery {
                provider_id: Some(id.into()),
                model_id: Some("unrelated-2048".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
    eprintln!("4096-row exact work: {:?}", exact.work);
    assert_eq!(exact.work.candidate_keys, 1);
    assert!(exact.work.candidate_vm_steps < 100, "{:?}", exact.work);
    assert!(exact.work.naming_rows <= 3, "{:?}", exact.work);
    assert_eq!(exact.work.stored_payloads, 1);
    db.execute_batch("CREATE TABLE model_writes (operation TEXT); CREATE TRIGGER model_update_probe AFTER UPDATE ON provider_models BEGIN INSERT INTO model_writes VALUES ('update'); END; CREATE TRIGGER model_insert_probe AFTER INSERT ON provider_models BEGIN INSERT INTO model_writes VALUES ('insert'); END; CREATE TRIGGER model_delete_probe AFTER DELETE ON provider_models BEGIN INSERT INTO model_writes VALUES ('delete'); END;").unwrap();
    db.execute("UPDATE provider_models SET entry='{unreadable' WHERE provider_id=?1 AND model_id='unrelated-4095'",[id]).unwrap();
    db.execute("DELETE FROM model_writes", []).unwrap();
    provider
        .update_reply(
            id.into(),
            1,
            serde_json::from_value(json!({"network":{"timeoutMs":{"action":"set","value":4000}}}))
                .unwrap(),
            UpdateReplyKind::Summary,
            &cancel,
        )
        .await
        .unwrap();
    let updated = provider
        .update_reply(
            id.into(),
            2,
            serde_json::from_value(json!({"baseUrl":"https://changed.openai.azure.com/openai"}))
                .unwrap(),
            UpdateReplyKind::Summary,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(updated).unwrap()["modelsListed"],
        "unsupported"
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM model_writes", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        0
    );
    let page = provider
        .query_models(
            ModelQuery {
                provider_id: Some(id.into()),
                limit: Some(32),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        page.models.len(),
        32,
        "an unrelated corrupt payload is not parsed"
    );
    assert_eq!(page.models[0].version, 3);
    provider.delete_static(id.into(), 3, &cancel).await.unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM provider_bundle WHERE provider_id=?1",
            [id],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn archived_source_bases_keep_exact_names_through_native_backup_restore() {
    let home = temp_home("provider-source-archive");
    let (root, provider) = provider_root(&home).await;
    let cancel = CancellationToken::new();
    provider.update_reply("deepseek".into(),1,serde_json::from_value(json!({"enabled":true,"credential":{"action":"set_ref","reference":"fixture-reference"}})).unwrap(),UpdateReplyKind::Summary,&cancel).await.unwrap();
    let first = provider
        .query_models(
            ModelQuery {
                provider_id: Some("deepseek".into()),
                limit: Some(1),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
    let model_id = first.models[0].model.id.clone();
    provider
        .update_model_config(
            "deepseek".into(),
            first.models[0].version,
            model_id.clone(),
            serde_json::from_value(json!({"displayName":"Pinned override"})).unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    let first = provider
        .query_models(
            ModelQuery {
                provider_id: Some("deepseek".into()),
                model_id: Some(model_id.clone()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
    let expected = serde_json::to_value(&first.models[0]).unwrap();
    assert_eq!(first.models[0].model.name, "Pinned override");
    root.shutdown().await.unwrap();
    {
        let db = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
        db.execute("INSERT INTO provider_sources SELECT 'archived-fixture',image FROM provider_sources LIMIT 1",[]).unwrap();
        db.execute(
            "UPDATE provider_bases SET revision='archived-fixture' WHERE provider_id='deepseek'",
            [],
        )
        .unwrap();
        db.execute("UPDATE provider_bundle SET naming_revision='previous-naming-generation' WHERE provider_id='deepseek'", []).unwrap();
    }
    let archive = home.join("archive");
    let restored = home.join("restored");
    dim_app_server::storage::backup_store(home.join("native.sqlite"), archive.clone()).unwrap();
    dim_app_server::storage::restore_store(archive, restored.clone()).unwrap();
    std::fs::rename(
        restored.join("dimcode.sqlite"),
        restored.join("native.sqlite"),
    )
    .unwrap();
    let (root, provider) = provider_root(&restored).await;
    let catalog = provider
        .catalog_for("deepseek".into(), &cancel)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        catalog
            .models
            .iter()
            .find(|m| m.id == model_id)
            .unwrap()
            .name,
        "Pinned override"
    );
    let exact = provider
        .query_models(
            ModelQuery {
                model_id: Some(model_id),
                provider_id: Some("deepseek".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(serde_json::to_value(&exact.models[0]).unwrap(), expected);
    assert_eq!(exact.work.source_payloads, 1);
    assert_eq!(exact.work.stored_payloads, 1);
    root.shutdown().await.unwrap();
    {
        let db = rusqlite::Connection::open(restored.join("native.sqlite")).unwrap();
        let mut body: Value =
            serde_json::from_str(&provider_body(&db, "deepseek").unwrap()).unwrap();
        let mut historical = body["models"][0].clone();
        historical["modelId"] = json!("admitted-history");
        historical["displayName"] = json!("Admitted history");
        body["models"].as_array_mut().unwrap().push(historical);
        dim_app_server::provider::testing::replace_provider(
            &db,
            "deepseek",
            exact.models[0].version,
            &body.to_string(),
        )
        .unwrap();
        body["definitionFingerprint"] = Value::Null;
        body["version"] = json!(1);
        dim_app_server::provider::testing::replace_provider(
            &db,
            "retired-history",
            1,
            &body.to_string(),
        )
        .unwrap();
    }
    let (root, provider) = provider_root(&restored).await;
    let retired = provider
        .catalog_for("retired-history".into(), &cancel)
        .await
        .unwrap()
        .unwrap();
    assert!(
        retired
            .models
            .iter()
            .any(|model| model.id == "admitted-history")
    );
    let mut expected = expected;
    for reply in [UpdateReplyKind::Summary, UpdateReplyKind::Full] {
        let version = expected["version"].as_u64().unwrap() as u32;
        provider
            .update_reply(
                "deepseek".into(),
                version,
                serde_json::from_value(json!({"enabled":true})).unwrap(),
                reply,
                &cancel,
            )
            .await
            .unwrap();
        expected["version"] = json!(version + 1);
        let exact = provider
            .query_models(
                ModelQuery {
                    provider_id: Some("deepseek".into()),
                    model_id: Some(expected["model"]["id"].as_str().unwrap().into()),
                    ..Default::default()
                },
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(serde_json::to_value(&exact.models[0]).unwrap(), expected);
        assert_eq!(exact.work.source_payloads, 0);
        assert_eq!(exact.work.stored_payloads, 2);
    }
    let version = expected["version"].as_u64().unwrap() as u32;
    provider
        .delete_static("deepseek".into(), version, &cancel)
        .await
        .unwrap();
    provider
        .update_model_config(
            "deepseek".into(),
            version + 1,
            "admitted-history".into(),
            serde_json::from_value(json!({"displayName":"Edited admitted history"})).unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    let catalog = provider
        .catalog_for("deepseek".into(), &cancel)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        catalog
            .models
            .iter()
            .find(|model| model.id == "admitted-history")
            .unwrap()
            .name,
        "Edited admitted history"
    );
    let db = rusqlite::Connection::open(restored.join("native.sqlite")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM provider_bases WHERE provider_id='deepseek'",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        0,
        "ordinary edits preserve admitted history without guessing its source"
    );
    drop(db);
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}
