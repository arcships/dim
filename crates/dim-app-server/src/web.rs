mod content;
mod network;

use crate::{
    managed_provider::{ManagedProviders, ManagedSearchError, WebSearchRequest},
    settings::{SettingsScope, SettingsService, SourceDeclaration},
};
use content::{convert, plain_text, redact};
use dim_agent_capabilities::{
    host::{HostFuture, ToolExecutionContext},
    tool_registry::{ToolContribution, ToolHandler, ToolRegistry},
};
use dim_agent_runtime::{Names, PartRegistry, PartScope};
use dim_agent_types::{ToolCall, ToolContent, ToolDefinition, ToolResult, ToolSource};
use network::{
    DOWNLOAD_LIMIT, Diagnostic, NetworkConfig, WebError, domain, get, hostname, parse_url,
    proxy_url,
};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use url::Url;

const CACHE_LIMIT: usize = 50 * 1024 * 1024;
const CACHE_TTL_MS: u64 = 15 * 60_000;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebDocument {
    #[serde(default)]
    pub allow_private_hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_service: Option<SearchServiceConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchServiceConfig {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

pub fn source_declaration() -> SourceDeclaration {
    SourceDeclaration {
        source: "web".into(),
        user_file: "web.json".into(),
        project_file: None,
        schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "searchService": {
                    "type": "object",
                    "required": ["kind"],
                    "additionalProperties": false,
                    "properties": {
                        "kind": {
                            "type": "string",
                            "minLength": 1
                        },
                        "endpoint": {
                            "type": "string",
                            "format": "uri"
                        },
                        "apiKey": {
                            "type": "string",
                            "minLength": 1,
                            "writeOnly": true
                        }
                    }
                },
                "allowPrivateHosts": {
                    "type": "array",
                    "items": {
                        "type": "string"
                    },
                    "default": []
                },
                "proxy": {
                    "type": "string",
                    "format": "uri",
                    "writeOnly": true
                }
            }
        }),
        user_only: vec!["/searchService".into(), "/allowPrivateHosts".into()],
    }
}

pub struct WebPlugin {
    settings: Key<SettingsService>,
    parts: Key<PartRegistry>,
}

impl WebPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
            parts: names.key()?,
        })
    }
}

impl DynPlugin for WebPlugin {
    fn name(&self) -> &str {
        "web"
    }

    fn injects(&self) -> Vec<TypeKey> {
        vec![self.settings.erase(), self.parts.erase()]
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            let parts = ctx.require_at(&self.parts)?;
            settings.register_declared(ctx, source_declaration())?;
            PartRegistry::register(&parts, ctx, PartScope::Session, "Web", |mount| {
                Ok(Some(Box::new(WebSessionPlugin::new(mount.names)?)))
            })?;
            Ok(Effect::Done)
        })
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub struct WebSessionPlugin {
    settings: Key<SettingsService>,
    tools: Key<ToolRegistry>,
    managed: Key<ManagedProviders>,
}

impl WebSessionPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
            tools: names.key()?,
            managed: names.key()?,
        })
    }
}

impl DynPlugin for WebSessionPlugin {
    fn name(&self) -> &str {
        "Web"
    }

    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.settings.erase(),
            self.tools.erase(),
            self.managed.erase(),
        ]
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            let tools = ctx.require_at(&self.tools)?;
            let managed = ctx.require_at(&self.managed)?;
            let state = Arc::new(SessionState {
                settings,
                managed,
                cache: Mutex::new(PageCache::default()),
                closing: CancellationToken::new(),
                inflight: TaskTracker::new(),
            });
            let cleanup_state = state.clone();
            ctx.effect(move || async move {
                cleanup_state.closing.cancel();
                cleanup_state.inflight.close();
                cleanup_state.inflight.wait().await;
                cleanup_state.cache.lock().unwrap().clear();
                Ok(())
            })?;
            for (name, description, schema) in [
                (
                    "web_search",
                    "Search the web. Returns numbered titles, URLs and snippets.",
                    search_schema(),
                ),
                (
                    "web_fetch",
                    "Read a public web page as text. Use start_char to continue at the returned final URL.",
                    fetch_schema(),
                ),
            ] {
                let runner = state.clone();
                tools.register(
                    ctx,
                    ToolContribution {
                        definition: ToolDefinition {
                            name: name.into(),
                            description: description.into(),
                            input_schema: serde_json::from_value(schema)
                                .expect("valid tool schema"),
                            source: ToolSource::Extension,
                            risk: Some("network".into()),
                            file_changes: None,
                            metadata: None,
                        },
                        handler: Arc::new(WebToolHandler {
                            name,
                            state: runner,
                        }),
                    },
                )?;
            }
            Ok(Effect::Done)
        })
    }
}

struct WebToolHandler {
    name: &'static str,
    state: Arc<SessionState>,
}

impl ToolHandler for WebToolHandler {
    fn execute(
        &self,
        call: ToolCall,
        _context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        let name = self.name;
        let state = self.state.clone();
        Box::pin(async move { Ok(state.execute(name, call, cancel).await) })
    }
}

#[derive(Clone)]
struct Page {
    url: String,
    status: u16,
    media: String,
    title: Option<String>,
    text: String,
    total: usize,
    limited: bool,
    expires: u64,
    bytes: usize,
}

#[derive(Default)]
struct PageCache {
    entries: HashMap<String, Page>,
    order: VecDeque<String>,
    bytes: usize,
}

impl PageCache {
    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }

    fn get(&mut self, url: &str, now: u64) -> Option<Page> {
        if let Some(page) = self.entries.get(url) {
            if page.expires <= now {
                let removed = self.entries.remove(url).unwrap();
                self.bytes = self.bytes.saturating_sub(removed.bytes);
                self.order.retain(|k| k != url);
                return None;
            }
            return Some(page.clone());
        }
        None
    }

    fn insert_if_absent(&mut self, page: Page, now: u64) -> Page {
        if let Some(existing) = self.get(&page.url, now) {
            return existing;
        }
        if page.bytes <= CACHE_LIMIT {
            while self.bytes + page.bytes > CACHE_LIMIT {
                let Some(oldest) = self.order.pop_front() else {
                    break;
                };
                if let Some(removed) = self.entries.remove(&oldest) {
                    self.bytes = self.bytes.saturating_sub(removed.bytes);
                }
            }
            self.bytes += page.bytes;
            self.order.push_back(page.url.clone());
            self.entries.insert(page.url.clone(), page.clone());
        }
        page
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct Hit {
    title: String,
    url: String,
    snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    published_at: Option<String>,
}

struct SessionState {
    settings: Arc<SettingsService>,
    managed: Arc<ManagedProviders>,
    cache: Mutex<PageCache>,
    closing: CancellationToken,
    inflight: TaskTracker,
}

impl SessionState {
    async fn execute(
        self: &Arc<Self>,
        name: &'static str,
        call: ToolCall,
        cancel: CancellationToken,
    ) -> ToolResult {
        if self.closing.is_cancelled() || cancel.is_cancelled() {
            return error_result(
                name,
                &WebError::new("cancelled", "Call cancelled."),
                None,
                &[],
            );
        }
        let owner = self.clone();
        let handle = self
            .inflight
            .spawn(async move { owner.run_call(name, call, cancel).await });
        match handle.await {
            Ok(result) => result,
            Err(_) => error_result(
                name,
                &WebError::new("cancelled", "Call cancelled."),
                None,
                &[],
            ),
        }
    }

    async fn run_call(
        &self,
        name: &'static str,
        call: ToolCall,
        cancel: CancellationToken,
    ) -> ToolResult {
        let call_cancel = CancellationToken::new();
        let _drop_guard = call_cancel.clone().drop_guard();
        let timeout_duration = if name == "web_search" {
            Duration::from_secs(20)
        } else {
            Duration::from_secs(30)
        };
        let mut diagnostics = Vec::new();
        let mut secret: Option<String> = None;
        let outcome = tokio::select! {
            _ = self.closing.cancelled() => {
                call_cancel.cancel();
                Err(WebError::new("cancelled", "Call cancelled."))
            }
            _ = cancel.cancelled() => {
                call_cancel.cancel();
                Err(WebError::new("cancelled", "Call cancelled."))
            }
            res = tokio::time::timeout(timeout_duration, async {
                let snapshot = self
                    .settings
                    .read_declared("web", SettingsScope::User, &call_cancel)
                    .await
                    .map_err(|_| {
                        WebError::new("settings_unavailable", "Web settings could not be read.")
                    })?;
                let document: WebDocument =
                    serde_json::from_value(snapshot.document).map_err(|_| {
                        WebError::new("settings_unavailable", "Web settings are invalid.")
                    })?;
                secret = document
                    .search_service
                    .as_ref()
                    .and_then(|s| s.api_key.clone());
                if name == "web_fetch" {
                    self.fetch_page(&call.input, &document, &call_cancel, &mut diagnostics).await
                } else {
                    self.search(&call.input, &document, &call_cancel, &mut diagnostics).await
                }
            }) => {
                match res {
                    Ok(inner) => inner,
                    Err(_) => {
                        call_cancel.cancel();
                        Err(WebError::new("timeout", "Call timed out."))
                    }
                }
            }
        };
        match outcome {
            Ok(result) => result,
            Err(err) => error_result(name, &err, secret.as_deref(), &diagnostics),
        }
    }

    async fn fetch_page(
        &self,
        args: &serde_json::Map<String, Value>,
        document: &WebDocument,
        cancel: &CancellationToken,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<ToolResult, WebError> {
        check_fields(args, &["url", "start_char", "max_chars"])?;
        let raw_url = args
            .get("url")
            .and_then(Value::as_str)
            .filter(|u| u.chars().count() <= 2048)
            .ok_or_else(|| {
                WebError::new(
                    "invalid_input",
                    "url is required and must contain at most 2048 characters.",
                )
            })?;
        let mut url = parse_url(raw_url)?;
        url.set_fragment(None);
        let start = parse_integer(args.get("start_char"), 0, 0, MAX_SAFE_INTEGER)? as usize;
        let count = parse_integer(args.get("max_chars"), 20_000, 1_000, 50_000)? as usize;
        let config = NetworkConfig {
            allow_private_hosts: document.allow_private_hosts.clone(),
            proxy: proxy_url(document.proxy.as_deref())?,
        };
        let key = document
            .search_service
            .as_ref()
            .and_then(|s| s.api_key.as_deref());
        let now = now_ms();
        let mut page = self.cache.lock().unwrap().get(url.as_str(), now);
        if page.is_none() {
            let empty_headers = HashMap::new();
            for hop in 0.. {
                let response = get(&url, &config, cancel, diagnostics, &empty_headers).await?;
                if (300..400).contains(&response.status) {
                    let Some(location) = response.location.as_deref() else {
                        return Err(WebError::new(
                            "http_status",
                            format!(
                                "HTTP {} {}",
                                response.status,
                                status_reason(response.status)
                            ),
                        ));
                    };
                    let joined = url.join(location).map_err(|_| {
                        WebError::new("invalid_input", "The redirect URL is invalid.")
                    })?;
                    let mut target = parse_url(joined.as_str()).map_err(|_| {
                        WebError::new("invalid_input", "The redirect URL is invalid.")
                    })?;
                    target.set_fragment(None);
                    if target.as_str().chars().count() > 2048 {
                        return Err(WebError::new(
                            "invalid_input",
                            "The redirect URL is invalid.",
                        ));
                    }
                    if host_group(&target) != host_group(&url) {
                        return Ok(ok_result(
                            format!(
                                "Redirected to {}; call web_fetch with that URL to follow it.",
                                redact(target.as_str(), key)
                            ),
                            None,
                        ));
                    }
                    if hop == 5 {
                        return Err(WebError::new(
                            "too_many_redirects",
                            "More than five redirects.",
                        ));
                    }
                    url = target;
                    continue;
                }
                if !(200..300).contains(&response.status) {
                    return Err(WebError::new(
                        "http_status",
                        format!(
                            "HTTP {} {}",
                            response.status,
                            status_reason(response.status)
                        ),
                    ));
                }
                if cancel.is_cancelled() {
                    return Err(WebError::new("cancelled", "Call cancelled."));
                }
                let converted = convert(&response.body, &response.content_type, &url, diagnostics)?;
                let text = redact(&converted.text, key);
                let title = converted
                    .title
                    .map(|t| truncate_chars(&redact(&t, key), 500));
                let total = text.chars().count();
                let media = response
                    .content_type
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase();
                let bytes = text.len() + title.as_deref().map_or(0, str::len) + url.as_str().len();
                let now = now_ms();
                let candidate = Page {
                    url: url.as_str().to_owned(),
                    status: response.status,
                    media,
                    title,
                    text,
                    total,
                    limited: response.download_limited,
                    expires: now + CACHE_TTL_MS,
                    bytes,
                };
                page = Some(self.cache.lock().unwrap().insert_if_absent(candidate, now));
                break;
            }
        }
        let page = page.expect("page resolved");
        let end = (start + count).min(page.total);
        let slice: String = if start < end {
            page.text.chars().skip(start).take(end - start).collect()
        } else {
            String::new()
        };
        let truncated = end < page.total;
        let title = page
            .title
            .as_deref()
            .map(|t| truncate_chars(&redact(t, key), 500));
        let display_end = start.max(end);
        let mut header = vec![format!("URL: {}", redact(&page.url, key))];
        if let Some(t) = &title {
            header.push(format!("Title: {t}"));
        }
        header.push(format!("Content-Type: {}", redact(&page.media, key)));
        header.push(format!(
            "Characters: {start}-{display_end} of {}",
            page.total
        ));
        if truncated {
            header.push(format!(
                "[Truncated: call web_fetch again with start_char={end} to continue.]"
            ));
        }
        if page.limited {
            header.push(format!(
                "[Download limit reached: the page exceeds {DOWNLOAD_LIMIT} bytes and later content is missing.]"
            ));
        }
        let body_text = if page.total == 0 {
            "(no text content)".to_owned()
        } else {
            redact(&slice, key)
        };
        let mut structured = serde_json::Map::new();
        structured.insert("url".into(), Value::String(redact(raw_url, key)));
        structured.insert("final_url".into(), Value::String(redact(&page.url, key)));
        structured.insert("status".into(), Value::from(page.status));
        structured.insert(
            "content_type".into(),
            Value::String(redact(&page.media, key)),
        );
        if let Some(t) = title {
            structured.insert("title".into(), Value::String(t));
        }
        structured.insert("start".into(), Value::from(start));
        structured.insert("end".into(), Value::from(display_end));
        structured.insert("total".into(), Value::from(page.total));
        structured.insert("truncated".into(), Value::Bool(truncated));
        structured.insert("download_limited".into(), Value::Bool(page.limited));
        if !diagnostics.is_empty() {
            structured.insert(
                "diagnostics".into(),
                serde_json::to_value(&*diagnostics).unwrap(),
            );
        }
        Ok(ok_result(
            format!("{}\n\n{body_text}", header.join("\n")),
            Some(structured),
        ))
    }

    async fn search(
        &self,
        args: &serde_json::Map<String, Value>,
        document: &WebDocument,
        cancel: &CancellationToken,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<ToolResult, WebError> {
        check_fields(
            args,
            &["query", "max_results", "allowed_domains", "blocked_domains"],
        )?;
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if query.is_empty() || query.chars().count() > 500 {
            return Err(WebError::new(
                "invalid_input",
                "query must contain 1 to 500 characters.",
            ));
        }
        let count = parse_integer(args.get("max_results"), 5, 1, 10)? as usize;
        let allowed = parse_filters(args.get("allowed_domains"))?;
        let blocked = parse_filters(args.get("blocked_domains"))?;
        if allowed.is_some() && blocked.is_some() {
            return Err(WebError::new(
                "invalid_input",
                "Domain filters are mutually exclusive.",
            ));
        }
        let effective_query = if let Some(allowed) = &allowed {
            let ops = allowed
                .iter()
                .map(|h| format!("site:{h}"))
                .collect::<Vec<_>>()
                .join(" OR ");
            if ops.is_empty() {
                query.to_owned()
            } else {
                format!("{query} ({ops})")
            }
        } else if let Some(blocked) = &blocked {
            let ops = blocked
                .iter()
                .map(|h| format!("-site:{h}"))
                .collect::<Vec<_>>()
                .join(" ");
            if ops.is_empty() {
                query.to_owned()
            } else {
                format!("{query} {ops}")
            }
        } else {
            query.to_owned()
        };
        let service = document.search_service.as_ref();
        let raw = if service.is_none_or(|s| s.kind == "dim-oauth") {
            if service.is_some_and(|s| s.endpoint.is_some() || s.api_key.is_some()) {
                return Err(WebError::new(
                    "search_unavailable",
                    "The configured search service is unusable.",
                ));
            }
            let relay_query = if effective_query.chars().count() > 500 {
                query.to_owned()
            } else {
                effective_query
            };
            let reply = self
                .managed
                .search(
                    WebSearchRequest {
                        query: relay_query,
                        count: count as u32,
                    },
                    cancel,
                )
                .await
                .map_err(|err| {
                    if cancel.is_cancelled() {
                        WebError::new("cancelled", "Call cancelled.")
                    } else {
                        match err {
                            ManagedSearchError::Unavailable => WebError::new(
                                "search_unavailable",
                                "No usable search service is configured.",
                            ),
                            ManagedSearchError::SearchFailed => {
                                WebError::new("search_failed", "The search service failed.")
                            }
                        }
                    }
                })?;
            let mut hits = Vec::with_capacity(reply.results.len());
            for item in reply.results {
                let url = parse_url(&item.url).map_err(|_| {
                    WebError::new("search_failed", "The search service returned invalid data.")
                })?;
                let published_at = item
                    .published_at
                    .map(|p| plain_text(&p))
                    .filter(|p| !p.is_empty())
                    .map(|p| truncate_chars(&p, 500));
                hits.push(Hit {
                    title: truncate_chars(&plain_text(&item.title), 500),
                    url: url.to_string(),
                    snippet: truncate_chars(&plain_text(&item.snippet), 500),
                    published_at,
                });
            }
            hits
        } else if let Some(s) = service
            && s.kind == "brave"
            && let Some(api_key) = s.api_key.as_deref()
        {
            let endpoint = s
                .endpoint
                .as_deref()
                .unwrap_or("https://api.search.brave.com/res/v1/web/search");
            let mut url = parse_url(endpoint).map_err(|_| {
                WebError::new(
                    "search_unavailable",
                    "The configured search service is unusable.",
                )
            })?;
            let proxy = proxy_url(document.proxy.as_deref()).map_err(|_| {
                WebError::new(
                    "search_unavailable",
                    "The configured search service is unusable.",
                )
            })?;
            if http::HeaderValue::from_str(api_key).is_err()
                || api_key.bytes().any(|b| b < 0x20 || b == 0x7f)
            {
                return Err(WebError::new(
                    "search_unavailable",
                    "The configured search service is unusable.",
                ));
            }
            url.query_pairs_mut()
                .append_pair("q", &effective_query)
                .append_pair("count", &count.to_string());
            let config = NetworkConfig {
                allow_private_hosts: document.allow_private_hosts.clone(),
                proxy,
            };
            let headers = HashMap::from([("X-Subscription-Token".to_owned(), api_key.to_owned())]);
            let response = get(&url, &config, cancel, diagnostics, &headers).await?;
            if !(200..300).contains(&response.status) || response.download_limited {
                return Err(WebError::new("search_failed", "The search service failed."));
            }
            let value: Value = serde_json::from_slice(&response.body).map_err(|_| {
                WebError::new("search_failed", "The search service returned invalid data.")
            })?;
            parse_brave(&value, api_key)?
        } else {
            return Err(WebError::new(
                "search_unavailable",
                "No usable search service is configured.",
            ));
        };
        let results: Vec<Hit> = raw
            .into_iter()
            .filter(|hit| {
                let Ok(parsed) = Url::parse(&hit.url) else {
                    return false;
                };
                let host = hostname(&parsed);
                allowed.as_ref().is_none_or(|a| within_domains(&host, a))
                    && blocked.as_ref().is_none_or(|b| !within_domains(&host, b))
            })
            .take(count)
            .collect();
        let safe_query = redact(query, service.and_then(|s| s.api_key.as_deref()));
        let content = if results.is_empty() {
            format!("No results for \"{safe_query}\".")
        } else {
            let formatted = results
                .iter()
                .enumerate()
                .map(|(i, hit)| format!("[{}] {}\n{}\n{}", i + 1, hit.title, hit.url, hit.snippet))
                .collect::<Vec<_>>()
                .join("\n\n");
            format!(
                "Search results for \"{safe_query}\" ({} results)\n\n{formatted}",
                results.len()
            )
        };
        let mut structured = serde_json::Map::new();
        structured.insert("results".into(), serde_json::to_value(&results).unwrap());
        if !diagnostics.is_empty() {
            structured.insert(
                "diagnostics".into(),
                serde_json::to_value(&*diagnostics).unwrap(),
            );
        }
        Ok(ok_result(content, Some(structured)))
    }
}

fn parse_brave(value: &Value, key: &str) -> Result<Vec<Hit>, WebError> {
    let bad = || WebError::new("search_failed", "The search service returned invalid data.");
    let obj = value.as_object().ok_or_else(bad)?;
    if obj.get("type").and_then(Value::as_str) != Some("search") {
        return Err(bad());
    }
    let Some(web) = obj.get("web") else {
        return Ok(Vec::new());
    };
    let results = web
        .as_object()
        .and_then(|w| w.get("results"))
        .and_then(Value::as_array)
        .ok_or_else(bad)?;
    let mut hits = Vec::with_capacity(results.len());
    for item in results {
        let item_obj = item.as_object().ok_or_else(bad)?;
        let title = item_obj
            .get("title")
            .and_then(Value::as_str)
            .ok_or_else(bad)?;
        let raw_url = item_obj
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(bad)?;
        let description = match item_obj.get("description") {
            Some(v) => v.as_str().ok_or_else(bad)?,
            None => "",
        };
        let url = parse_url(raw_url).map_err(|_| bad())?;
        if redact(url.as_str(), Some(key)) != url.as_str() {
            return Err(bad());
        }
        hits.push(Hit {
            title: truncate_chars(&redact(&plain_text(title), Some(key)), 500),
            url: url.to_string(),
            snippet: truncate_chars(&redact(&plain_text(description), Some(key)), 500),
            published_at: None,
        });
    }
    Ok(hits)
}

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn check_fields(obj: &serde_json::Map<String, Value>, allowed: &[&str]) -> Result<(), WebError> {
    if obj.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(WebError::new("invalid_input", "Unknown parameter."));
    }
    Ok(())
}

fn parse_integer(
    value: Option<&Value>,
    fallback: u64,
    min: u64,
    max: u64,
) -> Result<u64, WebError> {
    let Some(value) = value else {
        return Ok(fallback);
    };
    let n = value
        .as_u64()
        .or_else(|| {
            value.as_f64().and_then(|f| {
                if f.fract() == 0.0 && f >= 0.0 && f <= (MAX_SAFE_INTEGER as f64) {
                    Some(f as u64)
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| WebError::new("invalid_input", "Invalid numeric parameter."))?;
    if n < min || n > max || n > MAX_SAFE_INTEGER {
        return Err(WebError::new("invalid_input", "Invalid numeric parameter."));
    }
    Ok(n)
}

fn parse_filters(value: Option<&Value>) -> Result<Option<Vec<String>>, WebError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let arr = value.as_array().ok_or_else(|| {
        WebError::new(
            "invalid_input",
            "Domain filters need at most ten hostnames.",
        )
    })?;
    if arr.len() > 10 {
        return Err(WebError::new(
            "invalid_input",
            "Domain filters need at most ten hostnames.",
        ));
    }
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let host = item
            .as_str()
            .and_then(domain)
            .ok_or_else(|| WebError::new("invalid_input", "Domain filters must be hostnames."))?;
        out.push(host);
    }
    Ok(Some(out))
}

fn within_domains(host: &str, domains: &[String]) -> bool {
    domains
        .iter()
        .any(|d| host == d || host.ends_with(&format!(".{d}")))
}

fn host_group(url: &Url) -> String {
    let h = hostname(url);
    h.strip_prefix("www.").unwrap_or(&h).to_owned()
}

fn status_reason(status: u16) -> &'static str {
    http::StatusCode::from_u16(status)
        .ok()
        .and_then(|s| s.canonical_reason())
        .unwrap_or("")
}

fn ok_result(
    content: String,
    structured_content: Option<serde_json::Map<String, Value>>,
) -> ToolResult {
    ToolResult {
        content: ToolContent::Text(content),
        structured_content,
        is_error: None,
        blob_refs: None,
        context_messages: None,
        background_task: None,
        effects: None,
    }
}

fn error_result(
    name: &str,
    err: &WebError,
    secret: Option<&str>,
    diagnostics: &[Diagnostic],
) -> ToolResult {
    let structured_content = (!diagnostics.is_empty()).then(|| {
        let mut map = serde_json::Map::new();
        map.insert(
            "diagnostics".into(),
            serde_json::to_value(diagnostics).unwrap(),
        );
        map
    });
    ToolResult {
        content: ToolContent::Text(format!(
            "{name} failed: {}: {}",
            err.code,
            redact(&err.message, secret)
        )),
        structured_content,
        is_error: Some(true),
        blob_refs: None,
        context_messages: None,
        background_task: None,
        effects: None,
    }
}

fn fetch_schema() -> Value {
    json!({
        "type": "object",
        "required": ["url"],
        "additionalProperties": false,
        "properties": {
            "url": { "type": "string", "maxLength": 2048 },
            "start_char": { "type": "integer", "minimum": 0 },
            "max_chars": { "type": "integer", "minimum": 1000, "maximum": 50000 }
        }
    })
}

fn search_schema() -> Value {
    json!({
        "type": "object",
        "required": ["query"],
        "additionalProperties": false,
        "properties": {
            "query": { "type": "string", "minLength": 1, "maxLength": 500 },
            "max_results": { "type": "integer", "minimum": 1, "maximum": 10 },
            "allowed_domains": { "type": "array", "maxItems": 10, "items": { "type": "string" } },
            "blocked_domains": { "type": "array", "maxItems": 10, "items": { "type": "string" } }
        }
    })
}
