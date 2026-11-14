use crate::{request, stream};
use aimux_core::{AbortSignal, AiMuxError, language_model::LanguageModel};
use aimux_provider_utils::{
    Fetch, FetchError, FetchFunction, FetchRequest, FetchResponse, HeaderMapOpt, ProxyConfig,
    ReqwestFetch, Resolvable,
};
use aimux_providers::{
    anthropic::{AnthropicProviderSettings, create_anthropic},
    bedrock::{AmazonBedrockProviderSettings, create_amazon_bedrock},
    google::{GoogleProviderSettings, create_google},
    openai::{OpenAIProviderSettings, create_openai},
    openai_compatible::{OpenAICompatibleProviderSettings, create_openai_compatible},
    xai::{XAIProviderSettings, create_xai},
};
use dim_agent_capabilities::host::*;
use dim_agent_types::ModelRequest;
use futures_util::Stream;
use std::{
    collections::HashMap,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    task::{Context, Poll},
    time::Duration,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const OPENAI_CODEX_PRESET: &str = "openai-codex";
/// The vendor packages the compatible protocol builds by name: their
/// provider-option namespace is mapped in [`request::reasoning`], and neither
/// has a registry row. Every other accepted name is a registry row.
const COMPATIBLE_PACKAGES: [&str; 2] = ["deepseek", "xai"];
const MAX_REDIRECTS: usize = 10;

/// Whether `name` is a preset the compatible protocol builds: a registry row,
/// or a package whose options dim maps by name. Any other package name, such
/// as `anthropic`, is refused rather than built as a generic Chat endpoint.
pub(crate) fn compatible_preset(name: &str) -> bool {
    COMPATIBLE_PACKAGES.contains(&name) || aimux_providers::preset::lookup(name).is_some()
}
/// Names a redirect the transport refused, which the error text then reports:
/// the helper layer above the transport follows any `3xx` that carries a
/// `Location`, so a refused one must not carry it.
const REDIRECT_TARGET_HEADER: &str = "x-dim-redirect-target";

/// Whether a status is one the fetch layers treat as a redirect.
fn is_redirect(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

/// The transport dim hands every provider: reqwest with dim's redirect policy.
///
/// The tag's helper layer follows a redirect the transport returns, so the
/// policy is applied here, hop by hop, and a hop that ends the chain is
/// returned without its `Location` (its target goes in
/// [`REDIRECT_TARGET_HEADER`]) so nothing above follows it.
struct DimFetch {
    transport: ReqwestFetch,
    timeout: Option<Duration>,
    /// The connection base URL's query, applied to every request URL. The
    /// models build an endpoint URL by appending the path to the base URL, so
    /// the query travels here instead of on the base URL itself.
    query: Vec<(String, String)>,
}

impl std::fmt::Debug for DimFetch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DimFetch")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl Fetch for DimFetch {
    async fn fetch(&self, request: FetchRequest) -> Result<FetchResponse, FetchError> {
        let mut request = with_connection_query(request, &self.query);
        // The transport's request timeout covers the response body, as the
        // connection's timeout did before the upgrade; a wrapper future here
        // would end at the headers.
        request.timeout = request.timeout.or(self.timeout);
        follow_redirects(&self.transport, request).await
    }
}

/// Moves a connection base URL's query onto a request URL: the model's own
/// query pairs stay first, and a connection pair the URL already names is not
/// repeated.
fn with_connection_query(mut request: FetchRequest, query: &[(String, String)]) -> FetchRequest {
    if query.is_empty() {
        return request;
    }
    let mut pairs: Vec<(String, String)> = request
        .url
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    for (name, value) in query {
        if !pairs.iter().any(|(existing, _)| existing == name) {
            pairs.push((name.clone(), value.clone()));
        }
    }
    request.url.query_pairs_mut().clear().extend_pairs(pairs);
    request
}

/// Splits a connection base URL into the part the models append a path to and
/// the query pairs every request carries, in the order the base URL lists them.
fn split_query(base_url: &str) -> (String, Vec<(String, String)>) {
    let Some((base, query)) = base_url.split_once('?') else {
        return (base_url.to_owned(), Vec::new());
    };
    let query = query.split('#').next().unwrap_or_default();
    let pairs = url::form_urlencoded::parse(query.as_bytes())
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    (base.to_owned(), pairs)
}

async fn follow_redirects(
    transport: &ReqwestFetch,
    mut request: FetchRequest,
) -> Result<FetchResponse, FetchError> {
    let mut followed = 0;
    loop {
        let response = transport.fetch(request.clone()).await?;
        if !is_redirect(response.status) {
            return Ok(response);
        }
        let Some(location) = response.headers().get(http::header::LOCATION) else {
            return Ok(response);
        };
        let Ok(location) = location.to_str() else {
            return Ok(response);
        };
        let Ok(target) = request.url.join(location) else {
            return Ok(response);
        };
        let keeps_method = matches!(response.status.as_u16(), 307 | 308);
        let same_origin = target.origin() == request.url.origin();
        if keeps_method && same_origin && followed < MAX_REDIRECTS {
            followed += 1;
            request.url = target;
            continue;
        }
        return Ok(refuse(response, &target));
    }
}

/// The redirect the policy refuses, with its target named out of band so the
/// error can report it and no layer above follows it.
fn refuse(mut response: FetchResponse, target: &url::Url) -> FetchResponse {
    let mut target = target.clone();
    target.set_query(None);
    target.set_fragment(None);
    let _ = target.set_username("");
    let _ = target.set_password(None);
    response.headers.remove(http::header::LOCATION);
    if let Ok(value) = http::HeaderValue::from_str(target.as_str()) {
        response.headers.insert(REDIRECT_TARGET_HEADER, value);
    }
    response
}

/// Restates a redirect that the client did not follow with its target, which
/// the provider's own error text does not name.
fn name_redirect_target(mut error: AiMuxError) -> AiMuxError {
    let AiMuxError::ApiCall(detail) = &mut error else {
        return error;
    };
    let Some(status) = detail.status_code else {
        return error;
    };
    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return error;
    }
    let Some(target) = detail
        .response_headers
        .as_ref()
        .and_then(|headers| headers.get(REDIRECT_TARGET_HEADER))
        .cloned()
    else {
        return error;
    };
    // The origin is the reason a cross-origin target is refused whatever the
    // status; only a same-origin 301 or 302 is refused for the method change.
    let reason = if url::Url::parse(&detail.url)
        .ok()
        .zip(url::Url::parse(&target).ok())
        .is_some_and(|(base, target)| base.origin() != target.origin())
    {
        "the target is a different origin"
    } else if !matches!(status, 307 | 308) {
        "the request would be re-sent as a GET without its body; update the provider URL to the target"
    } else {
        "the redirect limit was reached"
    };
    detail.message = format!("redirect to {target} was not followed: {reason}");
    error
}

/// A transport carrying one connection's proxy, timeout and query.
///
/// Without a proxy it is a clone of the driver's own transport, which shares
/// its connection pool; only a proxy needs a client of its own. Taking the
/// shared transport here keeps a query or a timeout from costing a new pool
/// per request.
fn fetch(
    shared: &ReqwestFetch,
    network: &ModelNetwork,
    query: Vec<(String, String)>,
) -> FetchFunction {
    let transport = match &network.proxy {
        Some(proxy) => ReqwestFetch::new(ProxyConfig {
            all_url: Some(proxy.clone()),
            ..ProxyConfig::default()
        }),
        None => shared.clone(),
    };
    Arc::new(DimFetch {
        transport,
        timeout: network.timeout_ms.map(Duration::from_millis),
        query,
    })
}

/// The endpoint base the Anthropic model appends `/messages` to: the
/// connection's base URL with the version segment, unless it already names it.
fn anthropic_base_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/v1") {
        base_url.to_owned()
    } else {
        format!("{base_url}/v1")
    }
}

/// The transport a non-conversational call uses: the same client, redirect
/// policy and connection facts as a chat call to the same model.
///
/// One instance per adapter, so every call to one connection shares its
/// connection pool. [`Self::resolve`] splits a base URL the way a chat call
/// does: the models append a path to the base, so the base URL's query travels
/// on the transport instead.
pub struct MediaTransport {
    transport: ReqwestFetch,
}
impl MediaTransport {
    pub fn new() -> Self {
        Self {
            transport: ReqwestFetch::default(),
        }
    }
    /// The base URL without its query, and the transport carrying that query,
    /// the connection's proxy and its timeout.
    pub fn resolve(&self, network: &ModelNetwork, base_url: &str) -> (String, FetchFunction) {
        let (base, query) = split_query(base_url);
        (base, fetch(&self.transport, network, query))
    }
    /// The client a media download streams through: the connection's proxy
    /// and the redirect rule its credential allows.
    ///
    /// `credentialed` is true when the request carries the connection's
    /// headers because its URL shares the connection's origin. Only a
    /// same-origin 307 or 308 is then followed, the rule
    /// [`follow_redirects`] applies: reqwest strips `Authorization` on a
    /// cross-origin redirect but not a provider's own header, so another hop
    /// could carry `x-goog-api-key` or `xi-api-key` to a different host. An
    /// uncredentialed URL is a pre-signed one the provider minted; it carries
    /// no secret, so the default policy follows its redirects.
    ///
    /// The connection's timeout is not applied: it bounds one provider
    /// exchange, and a download is a body of its own size. The error is a
    /// fixed sentence, because a proxy URL may carry a credential.
    pub fn client(
        network: Option<&ModelNetwork>,
        credentialed: bool,
    ) -> Result<reqwest::Client, String> {
        let mut builder = reqwest::Client::builder();
        if let Some(proxy) = network.and_then(|network| network.proxy.as_ref()) {
            builder = builder.proxy(
                reqwest::Proxy::all(proxy)
                    .map_err(|_| "the connection's proxy URL cannot be used".to_owned())?,
            );
        }
        if credentialed {
            builder = builder.redirect(reqwest::redirect::Policy::custom(|attempt| {
                let original = attempt.previous().first().map(url::Url::origin);
                let same_origin = original == Some(attempt.url().origin());
                if matches!(attempt.status().as_u16(), 307 | 308)
                    && same_origin
                    && attempt.previous().len() < MAX_REDIRECTS
                {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }));
        }
        builder
            .build()
            .map_err(|_| "the media download client cannot be built".to_owned())
    }
}
impl Default for MediaTransport {
    fn default() -> Self {
        Self::new()
    }
}

/// The provider headers that keep provider-settings auth out of the way: dim
/// sends every credential through `options.headers`, so the provider itself
/// must produce none, not even an empty one.
fn no_credential(header: &str) -> HeaderMapOpt {
    HashMap::from([(header.to_string(), None)])
}

fn build_model(
    connection: &ModelConnection,
    base_url: &str,
    protocol: &ModelProtocol,
    preset: Option<&str>,
    model_id: &str,
    fetch: FetchFunction,
) -> Result<Arc<dyn LanguageModel>, AiMuxError> {
    if matches!(connection, ModelConnection::Local) {
        return Err(AiMuxError::invalid_argument(
            "Aimux requires a resolved model connection",
        ));
    }
    let base_url = base_url.to_owned();
    match protocol {
        ModelProtocol::OpenaiChat => {
            // The tag's own OpenAI Chat model carries the request rules a
            // reasoning model takes: the token key, the system role and the
            // sampling parameters it rejects.
            let provider = create_openai(OpenAIProviderSettings {
                base_url: Some(base_url),
                api_key: Some(String::new()),
                headers: Some(no_credential("authorization")),
                fetch: Some(fetch),
                ..OpenAIProviderSettings::default()
            })?;
            Ok(Arc::new(provider.chat(model_id)))
        }
        ModelProtocol::OpenaiCompatible => {
            // A registry preset supplies the row's wire profile (for example
            // Zenmux's `max_completion_tokens`). A name that is a vendor
            // package instead (DeepSeek) has no registry row; the generic
            // Chat model is what dim's protocols need there, so that the
            // provider-option fields dim writes reach the body unchanged.
            if let Some(preset) = preset
                && aimux_providers::preset::lookup(preset).is_some()
            {
                let provider = aimux_providers::preset::create(
                    preset,
                    aimux_providers::PresetSettings {
                        api_key: Some(Resolvable::Value(String::new())),
                        base_url: Some(base_url),
                        headers: Some(no_credential("authorization")),
                        fetch: Some(fetch),
                        ..aimux_providers::PresetSettings::default()
                    },
                )?;
                return Ok(Arc::new(provider.chat(model_id)));
            }
            let provider = create_openai_compatible(OpenAICompatibleProviderSettings {
                name: preset.unwrap_or("openai").to_owned(),
                base_url,
                api_key: None,
                headers: None,
                include_usage: Some(true),
                fetch: Some(fetch),
                ..OpenAICompatibleProviderSettings::default()
            })?;
            Ok(Arc::new(provider.chat(model_id)))
        }
        ModelProtocol::OpenaiResponses => {
            let provider = create_openai(OpenAIProviderSettings {
                base_url: Some(base_url),
                api_key: Some(String::new()),
                headers: Some(no_credential("authorization")),
                fetch: Some(fetch),
                ..OpenAIProviderSettings::default()
            })?;
            Ok(Arc::new(provider.responses(model_id)))
        }
        ModelProtocol::Xai => {
            let provider = create_openai_compatible(OpenAICompatibleProviderSettings {
                name: "xai".into(),
                base_url,
                api_key: None,
                headers: None,
                include_usage: Some(true),
                fetch: Some(fetch),
                ..OpenAICompatibleProviderSettings::default()
            })?;
            Ok(Arc::new(provider.chat(model_id)))
        }
        ModelProtocol::XaiResponses => {
            let provider = create_xai(XAIProviderSettings {
                base_url: Some(base_url),
                api_key: Some(String::new()),
                headers: Some(no_credential("authorization")),
                fetch: Some(fetch),
            })?;
            Ok(Arc::new(provider.responses(model_id)))
        }
        ModelProtocol::Anthropic => {
            let provider = create_anthropic(AnthropicProviderSettings {
                // The v0.6 model appends only `/messages`; the endpoint base
                // carries the version segment, as the v0.5 model's URL builder
                // supplied it.
                base_url: Some(anthropic_base_url(&base_url)),
                api_key: Some(String::new()),
                auth_token: None,
                headers: Some(no_credential("x-api-key")),
                fetch: Some(fetch),
                ..AnthropicProviderSettings::default()
            })?;
            Ok(Arc::new(provider.chat(model_id)))
        }
        ModelProtocol::Gemini => {
            let provider = create_google(GoogleProviderSettings {
                base_url: Some(base_url),
                api_key: Some(String::new()),
                headers: Some(no_credential("x-goog-api-key")),
                fetch: Some(fetch),
                ..GoogleProviderSettings::default()
            })?;
            Ok(Arc::new(provider.chat(model_id)))
        }
        ModelProtocol::Bedrock => {
            let ModelConnection::Bedrock(connection) = connection else {
                return Err(AiMuxError::invalid_argument(
                    "Bedrock requires a native Bedrock connection",
                ));
            };
            let (key, credentials) = match &connection.auth {
                BedrockAuth::Bearer(token) => (
                    Some(Resolvable::Value(token.clone())),
                    (None, None, None, None),
                ),
                BedrockAuth::SigV4 {
                    credentials,
                    region,
                } => (
                    Some(Resolvable::Value(String::new())),
                    (
                        Some(credentials.access_key_id.clone()),
                        Some(credentials.secret_access_key.clone()),
                        credentials.session_token.clone(),
                        Some(region.clone()),
                    ),
                ),
            };
            let provider = create_amazon_bedrock(AmazonBedrockProviderSettings {
                region: credentials.3,
                api_key: key,
                access_key_id: credentials.0,
                secret_access_key: credentials.1,
                session_token: credentials.2,
                base_url: Some(base_url.clone()),
                fetch: Some(fetch),
                ..AmazonBedrockProviderSettings::default()
            })?;
            Ok(Arc::new(provider.chat(model_id)))
        }
    }
}

pub(crate) struct AimuxDriver {
    files: Option<Arc<dim_agent_capabilities::host::SessionFilesHost>>,
    /// The transport every connection without a proxy reuses, so their
    /// clients share one connection pool.
    transport: ReqwestFetch,
    fetch: FetchFunction,
    generation: CancellationToken,
    tasks: TaskTracker,
    streams: Mutex<Vec<Weak<Mutex<Option<ModelStream>>>>>,
}
impl AimuxDriver {
    pub fn with_files(
        generation: CancellationToken,
        files: Option<Arc<dim_agent_capabilities::host::SessionFilesHost>>,
    ) -> Result<Arc<Self>, HostError> {
        let transport = ReqwestFetch::default();
        Ok(Arc::new(Self {
            files,
            fetch: Arc::new(DimFetch {
                transport: transport.clone(),
                timeout: None,
                query: Vec::new(),
            }),
            transport,
            generation,
            tasks: TaskTracker::new(),
            streams: Mutex::new(Vec::new()),
        }))
    }
    pub async fn close(&self) {
        {
            self.generation.cancel();
        }
        for stream in self
            .streams
            .lock()
            .unwrap()
            .drain(..)
            .filter_map(|stream| stream.upgrade())
        {
            stream.lock().unwrap().take();
        }
        self.tasks.close();
        self.tasks.wait().await;
    }
}
impl ModelDriver for AimuxDriver {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async move {
            let lease = {
                if self.generation.is_cancelled() {
                    return Err(HostError::classified(
                        HostErrorKind::OwnerClosed,
                        "Aimux owner is closed",
                    ));
                }
                self.tasks.token()
            };
            if cancel.is_cancelled() {
                return Err(HostError::Cancelled("Model invocation cancelled".into()));
            }
            let (protocol, preset, connection_headers, network) = match &connection {
                ModelConnection::Http(connection)
                    if connection.protocol != ModelProtocol::Bedrock =>
                {
                    (
                        connection.protocol,
                        connection.preset.as_deref(),
                        &connection.headers,
                        &connection.network,
                    )
                }
                ModelConnection::Bedrock(connection) => (
                    ModelProtocol::Bedrock,
                    None,
                    &connection.headers,
                    &connection.network,
                ),
                ModelConnection::Http(_) => {
                    return Err(HostError::classified(
                        HostErrorKind::InvalidRequest,
                        "Bedrock requires a native Bedrock connection",
                    ));
                }
                ModelConnection::Local => {
                    return Err(HostError::classified(
                        HostErrorKind::InvalidRequest,
                        "Aimux requires a resolved model connection",
                    ));
                }
            };
            let (base_url, query) = match &connection {
                ModelConnection::Http(connection) => split_query(&connection.base_url),
                ModelConnection::Bedrock(connection) => split_query(&connection.base_url),
                ModelConnection::Local => unreachable!("the connection was checked above"),
            };
            let fetch =
                if network.proxy.is_none() && network.timeout_ms.is_none() && query.is_empty() {
                    self.fetch.clone()
                } else {
                    fetch(&self.transport, network, query)
                };
            if preset == Some(OPENAI_CODEX_PRESET) && protocol != ModelProtocol::OpenaiResponses {
                return Err(HostError::classified(
                    HostErrorKind::InvalidRequest,
                    "openai-codex requires the OpenAI Responses protocol",
                ));
            }
            let request = tokio::select! {
                biased;
                _ = self.generation.cancelled() => return Err(HostError::classified(
                    HostErrorKind::OwnerClosed, "Aimux owner is closed",
                )),
                _ = cancel.cancelled() => return Err(HostError::Cancelled("Model invocation cancelled".into())),
                result = dim_agent_capabilities::session_files::materialize(
                    &request, self.files.as_deref(), &cancel,
                ) => result?,
            };
            let request::PreparedRequest {
                mut options,
                tool_names,
                recording,
            } = request::convert(&request, &protocol, preset)?;
            if preset == Some(OPENAI_CODEX_PRESET) {
                options.max_output_tokens = None;
                options
                    .provider_options
                    .get_or_insert_default()
                    .entry("openai".into())
                    .or_default()
                    .insert("store".into(), serde_json::Value::Bool(false));
            }
            let mut headers = HashMap::new();
            let authorization = if protocol == ModelProtocol::Anthropic {
                "authorization"
            } else {
                "Authorization"
            };
            if let ModelConnection::Http(connection) = &connection {
                if let Some(bearer) = &connection.bearer {
                    headers.insert(authorization.into(), format!("Bearer {bearer}"));
                }
                if let Some(key) = &connection.api_key {
                    let (name, value) = match protocol {
                        ModelProtocol::Anthropic => ("x-api-key", key.clone()),
                        ModelProtocol::Gemini => ("x-goog-api-key", key.clone()),
                        _ => (authorization, format!("Bearer {key}")),
                    };
                    headers.insert(name.into(), value);
                }
            }
            for (name, value) in connection_headers {
                let name = if name.eq_ignore_ascii_case("authorization") {
                    authorization.into()
                } else if name.eq_ignore_ascii_case("content-type") {
                    "Content-Type".into()
                } else {
                    name.to_ascii_lowercase()
                };
                headers.insert(name, value.clone());
            }
            options.headers = Some(headers);
            let abort = AbortSignal::new();
            options.abort_signal = Some(abort.clone());
            let model = match build_model(
                &connection,
                &base_url,
                &protocol,
                preset,
                &request.model_id,
                fetch,
            ) {
                Ok(model) => model,
                Err(error) => {
                    return Err(stream::error(name_redirect_target(error), &cancel));
                }
            };
            let mut pending = recording.as_ref().map(|capture| {
                capture.input(&options, model.provider(), &request.model_id);
                capture.pending()
            });
            let result = tokio::select! {
                biased;
                _ = self.generation.cancelled() => {
                    abort.abort();
                    if let Some(pending) = pending.as_mut() { pending.cancelled(); }
                    return Err(HostError::classified(HostErrorKind::OwnerClosed, "Aimux owner is closed"));
                }
                _ = cancel.cancelled() => {
                    abort.abort();
                    if let Some(pending) = pending.as_mut() { pending.cancelled(); }
                    return Err(HostError::Cancelled("Model invocation cancelled".into()));
                }
                result = async {
                    match &recording {
                        Some(capture) => capture.scope(model.do_stream(&options)).await,
                        None => model.do_stream(&options).await,
                    }
                } => match result.map_err(|error| stream::error(name_redirect_target(error), &cancel)) {
                    Ok(result) => result,
                    Err(error) => {
                        if let Some(pending) = pending.as_mut() {
                            pending.failed(&error.to_string());
                        }
                        return Err(error);
                    }
                },
            };
            let result = match recording {
                Some(capture) => {
                    let mut result = result;
                    let stream = capture.scope_stream(result.stream);
                    result.stream = Box::pin(aimux_core::recording::RecordingOutcomeStream::new(
                        stream,
                        Some(capture.recorder.clone()),
                        capture.call_id.clone(),
                    ));
                    result
                }
                None => result,
            };
            if let Some(pending) = pending.as_mut() {
                pending.disarm();
            }
            let stream = stream::convert(
                result,
                tool_names,
                protocol,
                cancel,
                abort,
                self.generation.clone(),
                lease,
            );
            let mut streams = self.streams.lock().unwrap();
            if self.generation.is_cancelled() {
                return Err(HostError::classified(
                    HostErrorKind::OwnerClosed,
                    "Aimux owner is closed",
                ));
            }
            streams.retain(|stream| stream.strong_count() > 0);
            let stream = Arc::new(Mutex::new(Some(stream)));
            streams.push(Arc::downgrade(&stream));
            Ok(Box::pin(OwnedStream {
                stream,
                ended: false,
            }) as ModelStream)
        })
    }
}

struct OwnedStream {
    stream: Arc<Mutex<Option<ModelStream>>>,
    ended: bool,
}
impl Stream for OwnedStream {
    type Item = Result<ModelChunk, HostError>;
    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.ended {
            return Poll::Ready(None);
        }
        let mut stream = this.stream.lock().unwrap();
        let result = match stream.as_mut() {
            Some(stream) => stream.as_mut().poll_next(context),
            None => {
                this.ended = true;
                return Poll::Ready(Some(Err(HostError::classified(
                    HostErrorKind::OwnerClosed,
                    "Aimux owner is closed",
                ))));
            }
        };
        if matches!(result, Poll::Ready(None)) {
            stream.take();
            this.ended = true;
        }
        result
    }
}
