//! One connection as an aimux provider, and one model out of it.
//!
//! The catalog names the vendor (through [`crate::vendor`]); the connection
//! supplies the endpoint, the credential and the transport. Nothing here keeps
//! either: the provider is built for one call and dropped with it.

use aimux_core::AiMuxError;
use aimux_core::image_model::ImageModel;
use aimux_core::provider::Provider;
use aimux_core::speech_model::SpeechModel;
use aimux_core::transcription_model::TranscriptionModel;
use aimux_core::video_model::VideoModel;
use aimux_provider_utils::{FetchFunction, Resolvable};
use aimux_providers::PresetSettings;
use dim_agent_aimux::MediaTransport;
use dim_agent_capabilities::host::{ModalityConnection, ModelConnection, ModelNetwork};
use dim_modality::AdapterError;
use std::sync::Arc;

use crate::vendor::{Base, Binding, Operation, Package};

/// Where a media call's transport comes from.
pub enum Transport {
    /// The product's transport: the connection's proxy, timeout and query,
    /// with the redirect policy a chat call gets.
    Shared(MediaTransport),
    /// A test's scripted transport. The connection's base URL then carries no
    /// query, because only [`MediaTransport`] splits one off.
    Fixed(FetchFunction),
}

impl Transport {
    fn resolve(&self, network: &ModelNetwork, base_url: &str) -> (String, FetchFunction) {
        match self {
            Self::Shared(transport) => transport.resolve(network, base_url),
            Self::Fixed(fetch) => (base_url.to_owned(), fetch.clone()),
        }
    }
    /// The client a video download streams through: the connection's proxy,
    /// and the redirect rule the download's credential allows.
    ///
    /// `credentialed` is true when the URL shares the connection's origin and
    /// the request therefore carries its headers; the client then follows a
    /// same-origin 307 or 308 only.
    pub fn client(
        &self,
        network: &ModelNetwork,
        credentialed: bool,
    ) -> Result<reqwest::Client, AdapterError> {
        match self {
            Self::Shared(_) => MediaTransport::client(Some(network), credentialed)
                .map_err(AdapterError::provider_error),
            // A scripted transport answers the provider calls; a download goes
            // to whatever address the script hands back. It applies the same
            // redirect rule, so a test sees the production policy.
            Self::Fixed(_) => {
                MediaTransport::client(None, credentialed).map_err(AdapterError::provider_error)
            }
        }
    }
}

/// The base URL one binding composes from the connection's: vendor rows
/// adjust the catalog's base to their package's path shape, and a family
/// binding takes it as it is.
fn binding_base(binding: &Binding, operation: Operation, url: &str) -> String {
    let url = url.trim_end_matches('/');
    match (binding.base, operation) {
        (Base::FalImage, Operation::Image) => {
            if let Some(rest) = url.strip_prefix("https://queue.fal.run") {
                format!("https://fal.run{rest}")
            } else {
                url.to_owned()
            }
        }
        (Base::Strip(suffix), _) => url
            .strip_suffix(suffix)
            .map(str::to_owned)
            .unwrap_or_else(|| url.to_owned()),
        (Base::Append(suffix), _) => {
            if url.ends_with(suffix) {
                url.to_owned()
            } else {
                format!("{url}{suffix}")
            }
        }
        (_, _) => url.to_owned(),
    }
}

/// The aimux provider that executes one connection's `operation`.
///
/// `provider_id` names the record in a refusal; the connection carries the
/// endpoint but not the record's id.
pub fn provider(
    connection: &ModalityConnection,
    binding: &Binding,
    operation: Operation,
    transport: &Transport,
    provider_id: &str,
) -> Result<Arc<dyn Provider>, AdapterError> {
    match &connection.resolved.connection {
        ModelConnection::Http(http) => {
            let (base_url, fetch) = transport.resolve(
                &http.network,
                &binding_base(binding, operation, &http.base_url),
            );
            // A record may carry no base URL of its own (the bundled catalog
            // leaves google without one): the package's default then applies,
            // as it does for any other unset setting.
            let base_url = (!base_url.is_empty()).then_some(base_url);
            // The credential reaches the package the way the connection
            // resolved it: as the bearer when Provider stripped one off, or
            // already inside `headers` for a vendor's own header name. An
            // empty key leaves the header names to that vendor; a per-call
            // header of the same name then wins (the layers merge with the
            // call last).
            let credential = http
                .bearer
                .clone()
                .or_else(|| http.api_key.clone())
                .unwrap_or_default();
            let unavailable = |error: AiMuxError| {
                AdapterError::unavailable(format!(
                    "The {provider_id} provider is unavailable: {error}"
                ))
            };
            match binding.package {
                Package::Preset(name) => {
                    let settings = PresetSettings {
                        api_key: Some(Resolvable::Value(credential)),
                        base_url,
                        headers: Some(
                            http.headers
                                .iter()
                                .map(|(name, value)| (name.clone(), Some(value.clone())))
                                .collect(),
                        ),
                        fetch: Some(fetch),
                        ..Default::default()
                    };
                    aimux_providers::create_provider(name, settings).map_err(unavailable)
                }
                Package::OpenaiCompatible => {
                    let settings =
                        aimux_providers::openai_compatible::OpenAICompatibleProviderSettings {
                            base_url: base_url.unwrap_or_default(),
                            api_key: (!credential.is_empty()).then_some(credential),
                            headers: Some(
                                http.headers
                                    .iter()
                                    .map(|(name, value)| (name.clone(), Some(value.clone())))
                                    .collect(),
                            ),
                            fetch: Some(fetch),
                            ..Default::default()
                        };
                    aimux_providers::openai_compatible::create_openai_compatible(settings)
                        .map(|provider| Arc::new(provider) as Arc<dyn Provider>)
                        .map_err(unavailable)
                }
            }
        }
        ModelConnection::Bedrock(_) => Err(AdapterError::unavailable(
            "No bundled catalog model declares media on an Amazon Bedrock connection",
        )),
        ModelConnection::Local => Err(AdapterError::unavailable(
            "The model's connection is local and serves no media call",
        )),
    }
}

/// Whether the package's image model for `model` reads the input files an
/// edit sends.
///
/// The row records whether the package reads `options.files` at all; where the
/// package also reports file input per model (`supports_file_inputs`), a model
/// that says `false` drops edit too. `None` — an unknown model — and a package
/// that cannot be built keep the row's answer.
pub fn serves_edit(binding: &Binding, model: &str) -> bool {
    if !binding.image_edit {
        return false;
    }
    match binding.package {
        Package::Preset(name) => {
            let Ok(provider) = aimux_providers::create_provider(name, PresetSettings::default())
            else {
                return true;
            };
            match provider.image_model(model) {
                Ok(image) => image.supports_file_inputs() != Some(false),
                Err(_) => true,
            }
        }
        // The package's models answer file input per model; the probe needs
        // no base URL because the model struct carries it without reading it.
        Package::OpenaiCompatible => {
            let settings = aimux_providers::openai_compatible::OpenAICompatibleProviderSettings {
                base_url: "http://127.0.0.1".to_owned(),
                ..Default::default()
            };
            let Ok(provider) =
                aimux_providers::openai_compatible::create_openai_compatible(settings)
            else {
                return true;
            };
            match Provider::image_model(&provider, model) {
                Ok(image) => image.supports_file_inputs() != Some(false),
                Err(_) => true,
            }
        }
    }
}

/// The image model of `provider`; a package without one answers `unavailable`.
pub fn image_model(
    provider: &Arc<dyn Provider>,
    model: &str,
) -> Result<Arc<dyn ImageModel>, AdapterError> {
    match provider.image_model(model) {
        Ok(model) => Ok(model),
        Err(AiMuxError::NoSuchModel { .. }) => Err(AdapterError::unavailable(format!(
            "The provider serves no image model `{model}`"
        ))),
        Err(error) => Err(AdapterError::unavailable(format!(
            "The provider refused `{model}`: {error}"
        ))),
    }
}

/// The video model of `provider`; a package without one answers `unavailable`.
pub fn video_model(
    provider: &Arc<dyn Provider>,
    model: &str,
) -> Result<Arc<dyn VideoModel>, AdapterError> {
    optional(provider.video_model(model), "video", model)
}

/// The speech model of `provider`; a package without one answers `unavailable`.
pub fn speech_model(
    provider: &Arc<dyn Provider>,
    model: &str,
) -> Result<Arc<dyn SpeechModel>, AdapterError> {
    optional(provider.speech_model(model), "speech", model)
}

/// The transcription model of `provider`; a package without one answers
/// `unavailable`.
pub fn transcription_model(
    provider: &Arc<dyn Provider>,
    model: &str,
) -> Result<Arc<dyn TranscriptionModel>, AdapterError> {
    optional(provider.transcription_model(model), "transcription", model)
}

/// One of aimux's optional media constructors, refused as `unavailable`.
fn optional<T: ?Sized>(
    model: Option<Result<Arc<T>, AiMuxError>>,
    kind: &str,
    id: &str,
) -> Result<Arc<T>, AdapterError> {
    let refusal =
        || AdapterError::unavailable(format!("The provider serves no {kind} model `{id}`"));
    match model {
        Some(Ok(model)) => Ok(model),
        Some(Err(error)) => Err(AdapterError::unavailable(format!(
            "The provider refused the {kind} model `{id}`: {error}"
        ))),
        None => Err(refusal()),
    }
}
