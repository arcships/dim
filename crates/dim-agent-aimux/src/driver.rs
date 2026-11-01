use crate::{request, stream};
use aimux_core::{AbortSignal, AiMuxError, language_model::LanguageModel};
use dim_agent_capabilities::host::*;
use dim_agent_types::ModelRequest;
use futures_util::Stream;
use std::{
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    task::{Context, Poll},
    time::Duration,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const OPENAI_CODEX_PRESET: &str = "openai-codex";
const MAX_REDIRECTS: usize = 10;

fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let keeps_method = matches!(attempt.status().as_u16(), 307 | 308);
        let same_origin = attempt
            .previous()
            .last()
            .is_some_and(|previous| previous.origin() == attempt.url().origin());
        if keeps_method && same_origin && attempt.previous().len() <= MAX_REDIRECTS {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
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
    let Some(location) = detail
        .response_headers
        .as_ref()
        .and_then(|headers| headers.get("location"))
        .cloned()
    else {
        return error;
    };
    let Ok(base) = url::Url::parse(&detail.url) else {
        return error;
    };
    let Ok(target) = base.join(&location) else {
        return error;
    };
    let reason = if target.origin() != base.origin() {
        "the target is a different origin"
    } else if !matches!(status, 307 | 308) {
        "the request would be re-sent as a GET without its body; update the provider URL to the target"
    } else {
        "the redirect limit was reached"
    };
    let mut target = target;
    target.set_query(None);
    target.set_fragment(None);
    let _ = target.set_username("");
    let _ = target.set_password(None);
    detail.message = format!("redirect to {target} was not followed: {reason}");
    error
}

fn client(network: &ModelNetwork) -> Result<reqwest::Client, HostError> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .pool_max_idle_per_host(10)
        .pool_idle_timeout(Duration::from_secs(30))
        .tcp_keepalive(Duration::from_secs(20))
        .redirect(redirect_policy());
    if let Some(proxy) = &network.proxy {
        builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(|_| {
            HostError::classified(HostErrorKind::InvalidRequest, "Invalid model proxy")
        })?);
    }
    if let Some(timeout) = network.timeout_ms {
        builder = builder.timeout(Duration::from_millis(timeout));
    }
    builder.build().map_err(|_| {
        HostError::classified(
            HostErrorKind::Transport,
            "Model HTTP client initialization failed",
        )
    })
}

pub(crate) struct AimuxDriver {
    client: Mutex<Option<reqwest::Client>>,
    generation: CancellationToken,
    tasks: TaskTracker,
    streams: Mutex<Vec<Weak<Mutex<Option<ModelStream>>>>>,
}
impl AimuxDriver {
    pub fn new(generation: CancellationToken) -> Result<Arc<Self>, HostError> {
        let client = client(&ModelNetwork::default())?;
        Ok(Arc::new(Self {
            client: Mutex::new(Some(client)),
            generation,
            tasks: TaskTracker::new(),
            streams: Mutex::new(Vec::new()),
        }))
    }
    pub async fn close(&self) {
        {
            let _admission = self.client.lock().unwrap();
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
        self.client.lock().unwrap().take();
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
            let (default_client, lease) = {
                let client = self.client.lock().unwrap();
                if self.generation.is_cancelled() {
                    return Err(HostError::classified(
                        HostErrorKind::OwnerClosed,
                        "Aimux owner is closed",
                    ));
                }
                let client = client.as_ref().ok_or_else(|| {
                    HostError::classified(HostErrorKind::OwnerClosed, "Aimux owner is closed")
                })?;
                (client.clone(), self.tasks.token())
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
            let client = if network.proxy.is_none() && network.timeout_ms.is_none() {
                default_client
            } else {
                client(network)?
            };
            if preset == Some(OPENAI_CODEX_PRESET) && protocol != ModelProtocol::OpenaiResponses {
                return Err(HostError::classified(
                    HostErrorKind::InvalidRequest,
                    "openai-codex requires the OpenAI Responses protocol",
                ));
            }
            let request::PreparedRequest {
                mut options,
                tool_names,
                recording,
            } = request::convert(&request, &protocol, preset)?;
            let (provider, profile) = match preset {
                Some(OPENAI_CODEX_PRESET) => (
                    "openai",
                    aimux_providers::openai::OpenAICompatProfile::full(),
                ),
                Some(preset) => (
                    preset,
                    aimux_providers::provider_registry_entry(preset).ok_or_else(|| {
                        HostError::classified(
                            HostErrorKind::Unsupported,
                            "Unknown compatible preset",
                        )
                    })?,
                ),
                None => (
                    "openai",
                    aimux_providers::openai::OpenAICompatProfile::full(),
                ),
            };
            if preset == Some(OPENAI_CODEX_PRESET) {
                options.max_output_tokens = None;
                options
                    .provider_options
                    .get_or_insert_default()
                    .entry("openai".into())
                    .or_insert_with(|| serde_json::json!({}))["store"] =
                    serde_json::Value::Bool(false);
            }
            let mut headers = std::collections::HashMap::new();
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
            options.headers = Some(headers.clone());
            let abort = AbortSignal::new();
            options.abort_signal = Some(abort.clone());
            let mut pending = recording.as_ref().map(|capture| {
                capture.input(&options, provider, &request.model_id);
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
                    match &connection {
                        ModelConnection::Http(connection) => match protocol {
                        ModelProtocol::OpenaiChat | ModelProtocol::OpenaiCompatible => {
                            let mut endpoint = url::Url::parse(&connection.base_url)
                                .map_err(|_| aimux_core::AiMuxError::InvalidArgument("Invalid model endpoint".into()))?;
                            endpoint.set_path(&format!("{}/chat/completions", endpoint.path().trim_end_matches('/')));
                            aimux_providers::openai::model::execute_stream_with_client(
                                endpoint.as_str(), &headers, &request.model_id, &options, provider, &profile, Some(client)
                            ).await
                        }
                        ModelProtocol::OpenaiResponses => {
                            let config = aimux_providers::openai::OpenAIConfig::new(connection.bearer.clone().unwrap_or_default()).with_base_url(&connection.base_url);
                            aimux_providers::openai::responses::OpenAIResponsesModel::new(request.model_id.clone(), config).with_client(client).do_stream(&options).await
                        }
                        ModelProtocol::Xai => {
                            let config = aimux_providers::xai::XAIConfig::new("").with_base_url(&connection.base_url);
                            aimux_providers::xai::XaiModel::new(request.model_id.clone(), config).with_client(client).do_stream(&options).await
                        }
                        ModelProtocol::XaiResponses => {
                            let config = aimux_providers::xai::XAIConfig::new("").with_base_url(&connection.base_url);
                            aimux_providers::xai::XaiResponsesModel::new(request.model_id.clone(), config).with_client(client).do_stream(&options).await
                        }
                        ModelProtocol::Anthropic => {
                            let mut config = aimux_providers::anthropic::AnthropicConfig::new(connection.api_key.clone().unwrap_or_default()).with_base_url(&connection.base_url);
                            config.auth_token = connection.bearer.clone();
                            aimux_providers::anthropic::model::AnthropicModel::new(request.model_id.clone(), config).with_client(client).do_stream(&options).await
                        }
                        ModelProtocol::Gemini => {
                            let config = aimux_providers::google::GoogleConfig::new(connection.api_key.clone().unwrap_or_default()).with_base_url(&connection.base_url);
                            aimux_providers::google::GoogleModel::new(request.model_id.clone(), config).with_client(client).do_stream(&options).await
                        }
                        ModelProtocol::Bedrock => unreachable!(),
                        },
                        ModelConnection::Bedrock(connection) => {
                            use aimux_providers::bedrock::{AwsCredentials as NativeCredentials, BedrockAuth as NativeAuth, BedrockConfig, BedrockModel};
                            let auth = match &connection.auth {
                                BedrockAuth::Bearer(token) => NativeAuth::BearerToken(token.clone()),
                                BedrockAuth::SigV4 { credentials, region } => NativeAuth::SigV4(NativeCredentials {
                                    access_key_id: credentials.access_key_id.clone(),
                                    secret_access_key: credentials.secret_access_key.clone(),
                                    session_token: credentials.session_token.clone(),
                                    region: region.clone(),
                                }),
                            };
                            BedrockModel::new(request.model_id.clone(), BedrockConfig {
                                base_url: connection.base_url.clone(),
                                auth,
                                retry_config: aimux_core::retry::RetryConfig { max_retries: 0, ..Default::default() },
                                api_key_source: None,
                            }).with_client(client).do_stream(&options).await
                        }
                        ModelConnection::Local => unreachable!(),
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
                    result.stream = Box::pin(aimux_core::recording::RecordingOutcomeStream::new(
                        result.stream,
                        Some(capture.recorder),
                        capture.call_id,
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
