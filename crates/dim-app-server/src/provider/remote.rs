//! Remote catalog I/O belongs to Provider; parsing happens once at this boundary.
use super::*;
use reqwest::{Url, header::HeaderMap};

pub(super) fn client(network: &ModelNetwork) -> Result<Client, HostError> {
    let mut builder = Client::builder()
        .timeout(Duration::from_millis(network.timeout_ms.unwrap_or(15_000)))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("DimCode/Rust")
        .default_headers(
            [(
                reqwest::header::HeaderName::from_static("x-title"),
                reqwest::header::HeaderValue::from_static("DimCode"),
            )]
            .into_iter()
            .collect(),
        );
    if let Some(proxy) = &network.proxy {
        builder = builder
            .proxy(reqwest::Proxy::all(proxy).map_err(|_| failure("Invalid catalog proxy"))?);
    }
    builder
        .build()
        .map_err(|_| failure("Catalog HTTP client initialization failed"))
}

pub(super) async fn read_body(
    mut response: reqwest::Response,
    cancel: &CancellationToken,
    total_bytes: &mut usize,
) -> Result<Value, HostError> {
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Err(HostError::Cancelled("Catalog request cancelled".into())),
            chunk = response.chunk() => chunk.map_err(|_| HostError::classified(HostErrorKind::Transport, "Model catalog response failed"))?,
        };
        let Some(chunk) = chunk else { break };
        *total_bytes += chunk.len();
        if *total_bytes > 8 * 1024 * 1024 {
            return Err(failure("Model catalog exceeds size limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| failure("Invalid model catalog JSON"))
}

impl ProviderService {
    pub(super) async fn discover(
        self: &Arc<Self>,
        provider_id: &str,
        record: &StaticProviderRecord,
        cancel: &CancellationToken,
    ) -> Result<Vec<ModelEntry>, ProviderError> {
        if !self.remote_model_list(provider_id, record) {
            return Err(ProviderError::Unsupported(
                "Provider does not support remote model listing".into(),
            ));
        }
        if record.auth != StaticAuth::Anonymous && record.credential.is_none() {
            return Err(ProviderError::AuthRequired);
        }
        let credential = self
            .credential(provider_id, &record.credential, &record.auth, cancel)
            .await?;
        let (mut url, headers) = request(record, credential.as_ref())?;
        let client = if record.network.proxy.is_none() && record.network.timeout_ms.is_none() {
            self.client.clone()
        } else {
            client(&record.network)?
        };
        let projection = ProviderProjection {
            provider_id: provider_id.to_owned(),
            source: Some(if record.kind == StaticRecordKind::Builtin {
                ModelSource::Builtin
            } else {
                ModelSource::Custom
            }),
            models: record.models.clone(),
            ..Default::default()
        };
        let mut models = Vec::new();
        let mut ids = HashSet::new();
        let mut cursors = HashSet::new();
        let mut total_bytes = 0;
        loop {
            self.check(cancel)?;
            let response = tokio::select! {
                _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
                response = client.get(url.clone()).headers(headers.clone()).send() =>
                    response.map_err(|_| HostError::classified(HostErrorKind::Transport, "Model catalog request failed"))?,
            };
            if !response.status().is_success() {
                return Err(HostError::classified(
                    HostErrorKind::Http {
                        status: Some(response.status().as_u16()),
                        code: None,
                    },
                    "Model catalog request rejected",
                )
                .into());
            }
            let body = read_body(response, cancel, &mut total_bytes).await?;
            let (first, second) = if record.driver_kind == "gemini" {
                ("models", "data")
            } else {
                ("data", "models")
            };
            let items = body
                .get(first)
                .and_then(Value::as_array)
                .or_else(|| body.get(second).and_then(Value::as_array))
                .ok_or_else(|| failure("Model catalog must contain a model array"))?;
            for item in items {
                let model = project(&self.bundled, &projection, &record.driver_kind, item)?;
                if ids.insert(model.model_id.clone()) {
                    models.push(model);
                }
            }
            let cursor = if record.driver_kind == "gemini" {
                match body.get("nextPageToken") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(value)) if value.is_empty() => None,
                    Some(Value::String(value)) => Some(("pageToken", value.clone())),
                    _ => return Err(ProviderError::Failed),
                }
            } else if record.driver_kind == "anthropic" {
                match body.get("has_more") {
                    None | Some(Value::Bool(false)) => None,
                    Some(Value::Bool(true)) => Some((
                        "after_id",
                        body.get("last_id")
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty())
                            .ok_or_else(|| failure("Model catalog is missing its page cursor"))?
                            .to_owned(),
                    )),
                    _ => return Err(ProviderError::Failed),
                }
            } else {
                None
            };
            let Some((key, cursor)) = cursor else { break };
            if !cursors.insert(cursor.clone()) || cursors.len() >= 100 {
                return Err(ProviderError::Failed);
            }
            url.set_query(None);
            url.query_pairs_mut().append_pair(key, &cursor);
        }

        Ok(models)
    }

    pub async fn refresh_for(
        self: &Arc<Self>,
        provider_id: String,
        cancel: &CancellationToken,
    ) -> Result<ProviderCatalog, ProviderError> {
        if self
            .oauth
            .contains(&provider_id, cancel)
            .await
            .map_err(ProviderError::from)?
        {
            return self.refresh(provider_id, cancel).await;
        }
        let owner = self.clone();
        self.owned(cancel, move |cancel| {
            Box::pin(async move {
                let id = provider_id.clone();
                let mut record = owner
                    .db
                    .run(move |db| read_static(db, &id))
                    .await
                    .map_err(ProviderError::from)?
                    .ok_or(ProviderError::NotFound)?;
                if !record.enabled {
                    return Err(ProviderError::InvalidConfig("Provider is disabled".into()));
                }
                let expected = serde_json::to_value(&record).map_err(ProviderError::from)?;
                let models = owner.discover(&provider_id, &record, &cancel).await?;
                reject_user_model_conflicts(&record, &models)?;
                let version = record.version;
                record.models = models;
                record.remote_models = true;
                record.version = version
                    .checked_add(1)
                    .ok_or_else(|| failure("Provider version overflow"))?;
                validate_static_record(&provider_id, &mut record)?;
                owner.check(&cancel)?;
                let bundled = owner.bundled.clone();
                owner
                    .db
                    .run(move |db| {
                        Ok((|| -> Result<_, ProviderError> {
                            let tx = db.transaction()?;
                            let current = read_static(&tx, &provider_id)?
                                .map(serde_json::to_value)
                                .transpose()?;
                            if current.as_ref() != Some(&expected) {
                                return Err(ProviderError::Conflict(
                                    "Provider version or admitted configuration changed".into(),
                                ));
                            }
                            tx.execute(
                                "UPDATE providers SET version=?2,record=?3 WHERE provider_id=?1",
                                params![
                                    provider_id,
                                    record.version,
                                    serde_json::to_string(&record)?
                                ],
                            )?;
                            tx.commit()?;
                            Ok(static_catalog(&bundled, &provider_id, &record, true))
                        })())
                    })
                    .await?
            })
        })
        .await
    }
}

fn request(
    record: &StaticProviderRecord,
    credential: Option<&ProviderSecret>,
) -> Result<(Url, HeaderMap), HostError> {
    let base = record.base_url.trim_end_matches('/');
    let base = if record.driver_kind == "new-api-router" {
        super::new_api::base_url(base, ModelProtocol::OpenaiCompatible)
    } else {
        base.to_owned()
    };
    let mut url = Url::parse(&base).map_err(|_| failure("Invalid model catalog URL"))?;
    let suffix = match record.driver_kind.as_str() {
        "anthropic" if !url.path().trim_end_matches('/').ends_with("/v1") => "/v1/models",
        _ => "/models",
    };
    url.set_path(&format!("{}{suffix}", url.path().trim_end_matches('/')));
    let mut headers: HeaderMap = (&record.headers)
        .try_into()
        .map_err(|_| failure("Invalid provider headers"))?;
    if record.auth != StaticAuth::Anonymous {
        let Some(ProviderSecret::Token(secret)) = credential else {
            return Err(failure("Model catalog requires a token credential"));
        };
        let (name, prefix) = match &record.auth {
            StaticAuth::ApiKey {
                header_name,
                prefix,
            } => (header_name.as_str(), prefix.as_str()),
            StaticAuth::CustomHeader { header_name } => (header_name.as_str(), ""),
            StaticAuth::Bearer => ("Authorization", "Bearer "),
            _ => return Err(failure("Unsupported model catalog authentication")),
        };
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| failure("Invalid credential header"))?;
        let mut value = reqwest::header::HeaderValue::from_str(&format!("{prefix}{secret}"))
            .map_err(|_| failure("Invalid credential header value"))?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    if record.driver_kind == "anthropic" {
        headers
            .entry("anthropic-version")
            .or_insert(reqwest::header::HeaderValue::from_static("2023-06-01"));
    }
    Ok((url, headers))
}

fn project(
    bundled: &BundledCatalog,
    provider: &ProviderProjection,
    driver: &str,
    item: &Value,
) -> Result<ModelEntry, HostError> {
    let mut descriptor = item
        .as_object()
        .cloned()
        .ok_or_else(|| failure("Invalid remote model descriptor"))?;
    let id = ["id", "modelId", "name"]
        .into_iter()
        .find_map(|key| item.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| failure("Invalid remote model ID"))?;
    let id = if driver == "gemini" {
        id.strip_prefix("models/").unwrap_or(id)
    } else {
        id
    };
    descriptor.insert("id".into(), Value::String(id.into()));
    let mut raw = descriptor.clone();
    if driver == "gemini"
        && let Some(methods) = item.get("supportedGenerationMethods")
    {
        let methods: Vec<String> = serde_json::from_value(methods.clone())
            .map_err(|_| failure("Invalid Gemini model generation methods"))?;
        if !methods.iter().any(|method| method == "generateContent") {
            raw.insert(
                "type".into(),
                Value::String("unsupported-generation".into()),
            );
        }
    }
    descriptor.insert("raw".into(), Value::Object(raw));
    for (target, sources) in [
        ("displayName", &["display_name", "displayName", "name"][..]),
        (
            "contextWindow",
            &["context_window", "contextWindow", "inputTokenLimit"][..],
        ),
        (
            "maxOutputTokens",
            &["max_output_tokens", "maxOutputTokens", "outputTokenLimit"][..],
        ),
        ("toolCalling", &["toolCalling", "toolCall", "tool_call"][..]),
    ] {
        if let Some(value) = sources.iter().find_map(|key| item.get(key)) {
            descriptor.insert(target.into(), value.clone());
        }
    }
    for value in [
        descriptor.get("contextWindow"),
        descriptor.get("maxOutputTokens"),
        item.pointer("/limit/context"),
        item.pointer("/limit/output"),
        item.pointer("/capabilities/contextWindow"),
        item.pointer("/capabilities/maxOutputTokens"),
    ]
    .into_iter()
    .flatten()
    {
        if !value
            .as_u64()
            .is_some_and(|n| (1..=MAX_MODEL_TOKENS).contains(&n))
        {
            return Err(failure(
                "Remote model limits must be positive safe JSON integers",
            ));
        }
    }
    if let Some(modalities) = item.get("modalities").and_then(|value| value.get("input")) {
        descriptor.insert("modalities".into(), modalities.clone());
        descriptor.insert(
            "vision".into(),
            Value::Bool(
                modalities
                    .as_array()
                    .is_some_and(|values| values.iter().any(|v| v == "image")),
            ),
        );
    }
    bundled
        .project(provider, &Value::Object(descriptor))
        .map_err(failure)
}
