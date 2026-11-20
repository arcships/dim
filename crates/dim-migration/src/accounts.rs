use crate::{
    Error, Migration, ObjectResult, Outcome, Result, TaskState,
    native::Native,
    source::{Source, fingerprint},
};
use dim_agent_capabilities::file_storage::{FileStorageError, FsPermissions};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

async fn insert(
    target: &Native,
    file: &str,
    id: &str,
    fields: Map<String, Value>,
    cancel: &CancellationToken,
) -> Result<Outcome> {
    let path = target.home.join(file);
    crate::ledger::while_locked(&target.files, &path, cancel, async |lease| {
        let mut doc: Value = match target
            .files
            .read_bytes(&path, usize::MAX, Some(lease), cancel)
            .await
        {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| Error::Object("Invalid target account file is preserved".into()))?,
            Err(FileStorageError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
            Err(e) => return Err(e.into()),
        };
        let doc = doc
            .as_object_mut()
            .ok_or_else(|| Error::Object("Invalid target account file is preserved".into()))?;
        let receipt = format!("migration:{id}");
        let digest = fingerprint(&fields)?;
        if let Some(previous) = doc.get(&receipt) {
            return Ok(if previous == &digest {
                Outcome::AlreadyImported
            } else {
                Outcome::Conflict
            });
        }
        if fields.keys().any(|key| doc.contains_key(key)) {
            return Ok(Outcome::Conflict);
        }
        doc.extend(fields);
        doc.insert(receipt, Value::String(digest));
        target
            .files
            .replace(
                &path,
                serde_json::to_vec_pretty(doc).map_err(crate::native::failure)?,
                FsPermissions::Private,
                Some(lease),
                cancel,
            )
            .await?;
        Ok(Outcome::Imported)
    })
    .await
}

pub(crate) async fn import(
    owner: &Migration,
    task: &TaskState,
    source: &Source,
    target: &Native,
) -> Result<()> {
    if let Some(auth) = crate::engine::document(owner, task, source, "auth.json").await? {
        for (id, key, state_key) in [
            ("dimcode-api-oauth", "nextApiOauth", "dimAccountState"),
            ("openai-codex", "openaiCodex", "openaiCodexAccountState"),
            ("xai-grok", "xaiGrokBuild", "xaiGrokBuildAccountState"),
        ] {
            let tokens = auth.get(key);
            let state = auth.get(state_key);
            if tokens.is_none() && (key != "nextApiOauth" || state.is_none()) {
                continue;
            }
            let result = async {
                let positive_integer = |value: &Value| {
                    value.as_f64().is_some_and(|number| number >= 1.0
                        && number <= dim_agent_types::JS_MAX_SAFE_INTEGER as f64
                        && number.fract() == 0.0)
                };
                let http_url = |value: &Value| {
                    value.as_str().is_some_and(|raw| {
                        reqwest::Url::parse(raw).is_ok_and(|url| {
                            matches!(url.scheme(), "http" | "https")
                                && url.username().is_empty()
                                && url.password().is_none()
                                && url.fragment().is_none_or(str::is_empty)
                        })
                    })
                };
                let mut fields = Map::new();
                if let Some(tokens) = tokens {
                    let valid_expiry = if key == "nextApiOauth" {
                        positive_integer(&tokens["expires"])
                    } else {
                        tokens["expires"].as_u64().is_some_and(|expires| {
                            expires > 0 && expires <= dim_agent_types::JS_MAX_SAFE_INTEGER
                        })
                    };
                    if tokens["type"] != "oauth"
                        || !["access", "refresh"].iter().all(|key| {
                            tokens[*key].as_str().is_some_and(|s| {
                                !s.trim().is_empty() && !s.chars().any(char::is_control)
                            })
                        })
                        || !valid_expiry
                    {
                        return Err(Error::Object(
                            "Legacy OAuth tokens are invalid; source preserved".into(),
                        ));
                    }
                    if key == "nextApiOauth" {
                        let required_endpoints = ["issuer", "tokenEndpoint", "relayBaseUrl"];
                        let optional_endpoints = [
                            "authorizationEndpoint",
                            "deviceAuthorizationEndpoint",
                            "deviceTokenEndpoint",
                        ];
                        let has_client = tokens["clientId"]
                            .as_str()
                            .is_some_and(|id| !id.trim().is_empty());
                        let endpoints_valid = required_endpoints
                            .iter()
                            .all(|key| http_url(&tokens[*key]));
                        let bad_optional_endpoint = optional_endpoints.iter().any(|key| {
                            tokens
                                .get(*key)
                                .is_some_and(|value| !value.is_null() && !http_url(value))
                        });
                        let bad_scope = tokens
                            .get("scope")
                            .is_some_and(|value| !matches!(value, Value::Null | Value::String(_)));
                        if !has_client || !endpoints_valid || bad_optional_endpoint || bad_scope
                        {
                            return Err(Error::Object(
                                "Legacy Dim OAuth tokens are incompatible; source preserved".into(),
                            ));
                        }
                    }
                    fields.insert(key.into(), tokens.clone());
                }
                if let Some(state) = state {
                    if state["schemaVersion"] != 1 {
                        return Err(Error::Unsupported("Legacy account state schema is unsupported".into()));
                    }
                    if key == "nextApiOauth" {
                        if !state["credentialEpoch"].as_str().is_some_and(|epoch| {
                            epoch.len() == 36 && uuid::Uuid::parse_str(epoch).is_ok()
                        }) || !positive_integer(&state["generation"])
                            || !positive_integer(&state["credentialRevision"])
                        {
                            return Err(Error::Unsupported("Legacy Dim account state is incompatible; source preserved".into()));
                        }
                        match state["state"].as_str() {
                            Some("signed_out") if tokens.is_none()
                                && state.get("identity").is_none()
                                && state.get("tokenDigest").is_none() => {},
                            Some("authenticated") => {
                                let tokens = tokens.ok_or_else(|| Error::Unsupported("Authenticated legacy Dim account has no tokens; source preserved".into()))?;
                                let identity = &state["identity"];
                                if !identity["subject"].as_str().is_some_and(|subject| !subject.trim().is_empty())
                                    || !http_url(&identity["issuer"])
                                    || identity["issuer"].as_str() != tokens["issuer"].as_str()
                                    || ["email", "name"].iter().any(|key| identity.get(*key).is_some_and(|value| !value.is_string()))
                                {
                                    return Err(Error::Unsupported("Legacy Dim account identity is incompatible; source preserved".into()));
                                }
                            },
                            _ => return Err(Error::Unsupported("Legacy Dim account state is incompatible; source preserved".into())),
                        }
                    } else if !state["generation"].as_str().is_some_and(|id| uuid::Uuid::parse_str(id).is_ok_and(|id| !id.is_nil()))
                        || !state["credentialRevision"].as_u64().is_some_and(|revision| revision > 0 && revision <= dim_agent_types::JS_MAX_SAFE_INTEGER)
                    {
                        return Err(Error::Unsupported("Legacy account state is incompatible; source preserved".into()));
                    }
                    if let Some(tokens) = tokens {
                        let token_digest = format!("{:x}", Sha256::digest(serde_jcs::to_vec(tokens)
                            .map_err(|_| Error::Object("Invalid legacy account tokens".into()))?));
                        if state["tokenDigest"].as_str() != Some(&token_digest) {
                            return Err(Error::Unsupported("Legacy account digest requires recovery; source preserved".into()));
                        }
                    }
                    fields.insert(state_key.into(), state.clone());
                }
                insert(target, "auth.json", id, fields, &owner.stopping).await
            }.await;
            record(owner, task, id, result).await?;
        }
    }
    if let Some(mcp) = crate::engine::document(owner, task, source, "mcp-oauth.json").await? {
        let Some(servers) = mcp["servers"].as_object() else {
            return Err(Error::Object("Invalid legacy MCP OAuth file".into()));
        };
        for (id, entry) in servers {
            let result = import_mcp(target, id, entry, &owner.stopping).await;
            record(owner, task, &format!("mcp:{id}"), result).await?;
        }
    }
    Ok(())
}

async fn record(
    owner: &Migration,
    task: &TaskState,
    id: &str,
    result: Result<Outcome>,
) -> Result<()> {
    let (outcome, reason) = match result {
        Ok(outcome) => (outcome, None),
        Err(Error::Object(reason) | Error::Unsupported(reason)) => {
            (Outcome::Unsupported, Some(reason))
        }
        Err(e) => return Err(e),
    };
    owner
        .ledger
        .object(
            &task.id,
            &ObjectResult {
                kind: "account".into(),
                id: id.into(),
                outcome,
                target_id: None,
                reason,
                omitted: Vec::new(),
            },
        )
        .await
}

async fn import_mcp(
    target: &Native,
    id: &str,
    entry: &Value,
    cancel: &CancellationToken,
) -> Result<Outcome> {
    let unsupported =
        || Error::Unsupported("Legacy MCP OAuth binding is incompatible; source preserved".into());
    let legacy: LegacyMcp = serde_json::from_value(entry.clone()).map_err(|_| unsupported())?;
    let url = reqwest::Url::parse(&legacy.server_url).map_err(|_| unsupported())?;
    let loopback = |url: &reqwest::Url| {
        url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        })
    };
    let trusted = |raw: &str| {
        reqwest::Url::parse(raw).is_ok_and(|url| {
            url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none()
                && (url.scheme() == "https" || url.scheme() == "http" && loopback(&url))
        })
    };
    if !trusted(&legacy.server_url) {
        return Err(unsupported());
    }
    let endpoint = |key: &str| {
        legacy.authorization_metadata[key]
            .as_str()
            .filter(|raw| trusted(raw))
            .ok_or_else(unsupported)
    };
    let issuer = endpoint("issuer")?;
    let token = endpoint("token_endpoint")?;
    let settings = target.settings("mcp", cancel).await?;
    let configured = &settings["mcpServers"][id];
    if configured["url"]
        .as_str()
        .and_then(|raw| reqwest::Url::parse(raw).ok())
        .as_ref()
        != Some(&url)
        || configured.get("auth").is_some_and(|auth| !auth.is_null())
        || configured["headers"].as_object().is_some_and(|headers| {
            headers
                .keys()
                .any(|key| key.eq_ignore_ascii_case("authorization"))
        })
    {
        return Err(unsupported());
    }
    let client = legacy.client_info;
    let tokens = legacy.tokens;
    let redirect = client
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("redirectUri"))
        .and_then(Value::as_str)
        .ok_or_else(unsupported)?;
    let redirect_url = reqwest::Url::parse(redirect).map_err(|_| unsupported())?;
    if redirect_url.scheme() != "http" || !loopback(&redirect_url) || !trusted(redirect) {
        return Err(unsupported());
    }
    let text = |text: &str| !text.trim().is_empty() && !text.chars().any(char::is_control);
    let now = dim_agent_runtime::core::unix_millis();
    let max = dim_agent_types::JS_MAX_SAFE_INTEGER;
    if !text(&client.client_id)
        || !text(&tokens.access_token)
        || tokens
            .refresh_token
            .as_deref()
            .is_some_and(|token| !text(token))
        || tokens.expires_at.is_some_and(|expires| {
            expires > max || expires <= now && tokens.refresh_token.is_none()
        })
        || client.client_id_issued_at.is_some_and(|at| at > max)
        || client
            .client_secret_expires_at
            .is_some_and(|at| at > max || at > 0 && at.saturating_mul(1000) <= now)
    {
        return Err(unsupported());
    }
    let method = client
        .token_endpoint_auth_method
        .as_deref()
        .unwrap_or("none");
    if !matches!(
        (method, client.client_secret.as_deref()),
        ("none", None) | ("client_secret_basic" | "client_secret_post", Some(_))
    ) || client
        .client_secret
        .as_deref()
        .is_some_and(|secret| !text(secret))
    {
        return Err(unsupported());
    }
    let resource = legacy
        .protected_resource_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("resource"))
        .and_then(Value::as_str)
        .unwrap_or(&legacy.server_url);
    if !trusted(resource)
        || reqwest::Url::parse(resource)
            .map_err(|_| unsupported())?
            .origin()
            != url.origin()
    {
        return Err(unsupported());
    }
    let scopes: Vec<_> = tokens
        .scope
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .collect();
    let mut canonical = url.to_string();
    if url.path() == "/" && url.query().is_none() {
        canonical.pop();
    }
    let registration = if let Some(client_id) = configured["oauth"]["clientId"].as_str() {
        if client.client_id != client_id {
            return Err(unsupported());
        }
        "preregistered"
    } else {
        "dynamic"
    };
    let requested_scopes: Vec<_> = configured["oauth"]["scopes"]
        .as_str()
        .map(|scopes| scopes.split_whitespace().collect())
        .unwrap_or_else(|| scopes.clone());
    let record = json!({"serverId":id,"url":canonical,"resource":resource,"issuer":issuer,"tokenEndpoint":token,"client":{"clientId":client.client_id,"clientSecret":client.client_secret,"registration":registration,"redirectUri":redirect,"authMethod":method},"tokens":{"accessToken":tokens.access_token,"refreshToken":tokens.refresh_token,"expiresAt":tokens.expires_at,"scopes":scopes},"requestedScopes":requested_scopes});
    let path = target.home.join("mcp-oauth.json");
    crate::ledger::while_locked(&target.files, &path, cancel, async |lease| {
        let mut doc: Value = match target
            .files
            .read_bytes(&path, usize::MAX, Some(lease), cancel)
            .await
        {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| unsupported())?,
            Err(FileStorageError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                json!({"version":1,"credentials":[]})
            }
            Err(e) => return Err(e.into()),
        };
        if doc["version"] != 1 {
            return Err(unsupported());
        }
        let receipt = format!("migration:{id}");
        let digest = fingerprint(entry)?;
        if let Some(previous) = doc.get(&receipt) {
            return Ok(if previous == &digest {
                Outcome::AlreadyImported
            } else {
                Outcome::Conflict
            });
        }
        let records = doc["credentials"].as_array_mut().ok_or_else(unsupported)?;
        if records
            .iter()
            .any(|r| r["serverId"] == id && r["url"] == canonical)
        {
            return Ok(Outcome::Conflict);
        }
        records.push(record);
        doc[receipt] = Value::String(digest);
        target
            .files
            .replace(
                &path,
                serde_json::to_vec_pretty(&doc).map_err(crate::native::failure)?,
                FsPermissions::Private,
                Some(lease),
                cancel,
            )
            .await?;
        Ok(Outcome::Imported)
    })
    .await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyMcp {
    server_url: String,
    tokens: LegacyMcpTokens,
    client_info: LegacyMcpClient,
    authorization_metadata: Value,
    protected_resource_metadata: Option<Value>,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyMcpTokens {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<u64>,
    scope: Option<String>,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyMcpClient {
    client_id: String,
    client_secret: Option<String>,
    client_id_issued_at: Option<u64>,
    client_secret_expires_at: Option<u64>,
    token_endpoint_auth_method: Option<String>,
    metadata: Option<Value>,
}
