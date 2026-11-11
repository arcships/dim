//! The built-in Modality adapter: the media operations aimux serves, executed
//! for one task at a time.
//!
//! The vendor is the catalog provider's identity ([`vendor`]); the connection
//! is Provider's per-call resolution; nothing is kept between calls except the
//! job a blocking operation is still running and, for video, the handle the
//! start call produced.
//!
//! A blocking operation does its whole call in `wait` and returns no handle,
//! so a restart leaves it `interrupted` — A.2's statement for an adapter with
//! no handle. Video starts a provider job in `submit`, stores the operation
//! reference as the handle, and polls it in `wait`, so a restart resumes the
//! same job instead of starting a second one.

mod job;
mod media;
mod realtime;
pub mod vendor;

use aimux_core::AiMuxError;
use aimux_core::video_model::VideoOperationStatus;
use aimux_provider_utils::FetchFunction;
use dim_agent_aimux::MediaTransport;
use dim_agent_capabilities::host::{ModalityConnection, ModelConnection, ModelNetwork};
use dim_agent_runtime::AgentBuilder;
use dim_agent_types::ModelId;
use dim_modality::{
    AdapterError, AdapterFuture, AdapterOperation, AdapterOutput, AdapterProgress, Job, JobFile,
    ModalityAdapter, ModalityAdapterService, ProgressSink, RealtimeEvent, RealtimeFrame,
    RealtimeOpened, RemoteCancel, Submitted,
};
use rutis::Ctx;
use rutis::host::{BoxFuture, DynPlugin, Effect};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

use vendor::Operation;

/// How long a video job may run before its task fails.
///
/// It exists for one failure mode: a provider job that never settles would
/// otherwise hold a scheduler slot of its category for the life of the
/// process (A.6 runs at most four per category). One hour is a bound, not a
/// provider's own limit; a job past it fails `provider_error`, and the record
/// keeps the remote handle, so an operator can still see what was running.
pub const MAX_VIDEO_WAIT: Duration = Duration::from_secs(60 * 60);
/// How long `wait` sleeps between two `do_status` calls.
pub const VIDEO_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// The most a downloaded video may write; aimux's own download guard uses it.
pub const MAX_VIDEO_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// How long a blocking job waits in the map for the `wait` that removes it.
///
/// The engine can skip `wait` after a successful `submit`: a cancel during the
/// submit phase, an adapter that turned unavailable or a connection failure,
/// or a cancel that wins the select before `wait` is polled. Such a job is
/// never looked at again, so it must expire instead of living with the
/// process.
const MAX_PENDING_AGE: Duration = Duration::from_secs(10 * 60);
/// The most blocking jobs the map keeps: an age bound alone still lets a
/// burst of submits hold every job for the full age.
const MAX_PENDING_JOBS: usize = 64;

/// One submitted blocking job, waiting for its `wait`.
struct Pending {
    job: Job,
    since: Instant,
}

/// The process-scoped `modalityAdapter` the product serves.
pub struct MediaAdapter {
    transport: media::Transport,
    /// The openai-realtime relay's sessions (A.2 `realtimeOpen`).
    relay: realtime::Relay,
    /// The jobs of blocking operations this generation is running: their
    /// `wait` carries no handle, so the job is kept from `submit` to `wait`
    /// and never across a generation. The map is bounded because the engine
    /// can skip `wait`; see [`MAX_PENDING_AGE`].
    jobs: Mutex<HashMap<String, Pending>>,
}

impl MediaAdapter {
    /// The adapter the product installs: the connection's own transport.
    pub fn new() -> Self {
        Self {
            transport: media::Transport::Shared(MediaTransport::new()),
            relay: realtime::Relay::default(),
            jobs: Mutex::new(HashMap::new()),
        }
    }
    /// An adapter whose requests are answered by `fetch`, for tests.
    pub fn scripted(fetch: FetchFunction) -> Self {
        Self {
            transport: media::Transport::Fixed(fetch),
            relay: realtime::Relay::default(),
            jobs: Mutex::new(HashMap::new()),
        }
    }
    /// The vendor of a model's provider, or `unavailable` for a created
    /// provider this build has no row for.
    fn vendor(&self, model: &ModelId) -> Result<&'static vendor::Vendor, AdapterError> {
        vendor::vendor(&model.provider_id).ok_or_else(|| {
            AdapterError::unavailable(format!(
                "No adapter row serves the provider `{}`",
                model.provider_id
            ))
        })
    }
    /// Holds a blocking job until its `wait`, within the bounds above.
    fn keep(&self, job: Job, now: Instant) {
        let mut jobs = self.jobs.lock().unwrap();
        jobs.retain(|_, pending| now.saturating_duration_since(pending.since) < MAX_PENDING_AGE);
        jobs.insert(job.task_id.clone(), Pending { job, since: now });
        while jobs.len() > MAX_PENDING_JOBS {
            let Some(oldest) = jobs
                .iter()
                .min_by_key(|(_, pending)| pending.since)
                .map(|(task_id, _)| task_id.clone())
            else {
                break;
            };
            jobs.remove(&oldest);
        }
    }
}

impl Default for MediaAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ModalityAdapter for MediaAdapter {
    fn available(&self) -> bool {
        true
    }

    fn operations(&self, model: &ModelId) -> Vec<AdapterOperation> {
        // The relay speaks the openai-realtime protocol a catalog entry
        // declares; the connection carries the base URL, headers and proxy,
        // so any provider with such an entry opens through it (A.2).
        let mut operations = vec![AdapterOperation {
            category: "realtime".to_owned(),
            operation: "voice".to_owned(),
            params_schema: realtime_schema(),
        }];
        let Some(vendor) = vendor::vendor(&model.provider_id) else {
            return operations;
        };
        let edit = media::serves_edit(vendor, &model.model_id);
        for (kind, category, operation) in vendor.statements(edit) {
            let schema = match (kind, operation) {
                (Operation::Image, "edit") => image_edit_schema(),
                (Operation::Image, _) => image_generate_schema(),
                (Operation::Video, _) => video_schema(),
                (Operation::Speech, _) => speech_schema(),
                (Operation::Transcription, _) => transcription_schema(),
            };
            operations.push(AdapterOperation {
                category: category.to_owned(),
                operation: operation.to_owned(),
                params_schema: schema,
            });
        }
        operations
    }

    fn submit(
        &self,
        job: Job,
        connection: ModalityConnection,
        cancel: CancellationToken,
    ) -> AdapterFuture<'_, Submitted> {
        Box::pin(async move {
            let vendor = self.vendor(&job.model)?;
            let kind = kind_of(&job)?;
            if !vendor.kinds.serves(kind) {
                return Err(AdapterError::unavailable(format!(
                    "The {} provider serves no {}",
                    vendor.catalog, job.operation
                )));
            }
            if job.operation == "edit" && !media::serves_edit(vendor, &job.model.model_id) {
                return Err(AdapterError::unavailable(format!(
                    "The {} provider serves no image.edit for `{}`",
                    vendor.catalog, job.model.model_id
                )));
            }
            if kind != Operation::Video {
                self.keep(job, Instant::now());
                return Ok(Submitted::default());
            }
            let provider = media::provider(&connection, vendor, kind, &self.transport)?;
            let model = media::video_model(&provider, &connection.model)?;
            let options = job::video_options(&job, vendor)?;
            let started = model
                .do_start(&options)
                .await
                .map_err(|error| failure(error, &cancel, &Secrets::of(&connection)))?;
            let handle = VideoHandle {
                operation: started.operation,
                job,
                started_at: now(),
            };
            let handle = serde_json::to_string(&handle).map_err(|error| {
                AdapterError::provider_error(format!("The job handle cannot be stored: {error}"))
            })?;
            Ok(Submitted {
                remote_handle: Some(handle),
            })
        })
    }

    fn wait(
        &self,
        task_id: &str,
        handle: Option<&str>,
        connection: ModalityConnection,
        progress: ProgressSink,
        cancel: CancellationToken,
    ) -> AdapterFuture<'_, AdapterOutput> {
        // The record's strings are borrowed from the caller; the future owns
        // them so it never outlives the call's arguments.
        let task_id = task_id.to_owned();
        let handle = handle.map(str::to_owned);
        Box::pin(async move {
            if let Some(handle) = handle {
                return self
                    .wait_video(&handle, &connection, progress, cancel)
                    .await;
            }
            // A blocking operation's job belongs to this generation only: a
            // recorded task never reaches here with an empty handle (A.6).
            let pending = self.jobs.lock().unwrap().remove(&task_id).ok_or_else(|| {
                AdapterError::provider_error(format!(
                    "This generation never started the task {task_id}"
                ))
            })?;
            let job = pending.job;
            let vendor = self.vendor(&job.model)?;
            let secrets = Secrets::of(&connection);
            match kind_of(&job)? {
                Operation::Image => {
                    progress.send(AdapterProgress {
                        fraction: None,
                        message: Some("Generating the image".into()),
                    });
                    let provider =
                        media::provider(&connection, vendor, Operation::Image, &self.transport)?;
                    let model = media::image_model(&provider, &connection.model)?;
                    let options = job::image_options(&job, vendor)?;
                    let result = aimux_core::image_model::generate_image(&*model, options)
                        .await
                        .map_err(|error| failure(error, &cancel, &secrets))?;
                    let files =
                        job::image_output(result, &job.work_dir, job::requested_format(&job))?;
                    Ok(AdapterOutput { files, data: None })
                }
                Operation::Speech => {
                    progress.send(AdapterProgress {
                        fraction: None,
                        message: Some("Synthesizing the audio".into()),
                    });
                    let provider =
                        media::provider(&connection, vendor, Operation::Speech, &self.transport)?;
                    let model = media::speech_model(&provider, &connection.model)?;
                    let options = job::speech_options(&job, vendor)?;
                    let result = aimux_core::speech_model::generate_speech(&*model, options)
                        .await
                        .map_err(|error| failure(error, &cancel, &secrets))?;
                    let files =
                        job::speech_output(result, &job.work_dir, job::requested_format(&job))?;
                    Ok(AdapterOutput { files, data: None })
                }
                Operation::Transcription => {
                    progress.send(AdapterProgress {
                        fraction: None,
                        message: Some("Transcribing the audio".into()),
                    });
                    let provider = media::provider(
                        &connection,
                        vendor,
                        Operation::Transcription,
                        &self.transport,
                    )?;
                    let model = media::transcription_model(&provider, &connection.model)?;
                    let options = job::transcription_options(&job, vendor)?;
                    let result = aimux_core::transcription_model::transcribe(&*model, options)
                        .await
                        .map_err(|error| failure(error, &cancel, &secrets))?;
                    let data = job::transcription_data(&result);
                    let files = job::transcription_output(&result, &job.work_dir)?;
                    Ok(AdapterOutput {
                        files,
                        data: Some(data),
                    })
                }
                Operation::Video => Err(AdapterError::provider_error(
                    "A video task is waited on by its handle, not by its task id",
                )),
            }
        })
    }

    fn cancel_remote(
        &self,
        _handle: &str,
        _connection: ModalityConnection,
    ) -> AdapterFuture<'_, RemoteCancel> {
        // aimux exposes no cancel for a video job: the provider finishes and
        // bills it whatever this build does (A.2 `unsupported`).
        Box::pin(async { Ok(RemoteCancel::Unsupported) })
    }

    fn realtime_open(
        &self,
        connection: ModalityConnection,
        _params: Value,
        events: tokio::sync::mpsc::Sender<RealtimeEvent>,
        cancel: CancellationToken,
    ) -> AdapterFuture<'_, RealtimeOpened> {
        Box::pin(async move { self.relay.open(&connection, events, &cancel).await })
    }

    fn realtime_send(
        &self,
        channel_id: &str,
        frame: RealtimeFrame,
        cancel: CancellationToken,
    ) -> AdapterFuture<'_, ()> {
        let channel = channel_id.to_owned();
        Box::pin(async move { self.relay.send(&channel, frame, &cancel).await })
    }

    fn realtime_close(&self, channel_id: &str) -> AdapterFuture<'_, ()> {
        let channel = channel_id.to_owned();
        Box::pin(async move { self.relay.close(&channel).await })
    }
}

/// A realtime session's parameters: the relay takes none; a client configures
/// the provider session with its first text frame (A.3, D7).
fn realtime_schema() -> Value {
    json!({"type": "object", "maxProperties": 0})
}

impl MediaAdapter {
    /// Polls one started video job, downloads what it produced, and returns
    /// its files.
    async fn wait_video(
        &self,
        handle: &str,
        connection: &ModalityConnection,
        progress: ProgressSink,
        cancel: CancellationToken,
    ) -> Result<AdapterOutput, AdapterError> {
        let handle: VideoHandle = serde_json::from_str(handle).map_err(|error| {
            AdapterError::provider_error(format!("The stored job handle is unreadable: {error}"))
        })?;
        let vendor = self.vendor(&handle.job.model)?;
        let provider = media::provider(connection, vendor, Operation::Video, &self.transport)?;
        let model = media::video_model(&provider, &connection.model)?;
        let options = job::video_options(&handle.job, vendor)?;
        let secrets = Secrets::of(connection);
        progress.send(AdapterProgress {
            fraction: None,
            message: Some("Waiting for the video".into()),
        });
        let deadline = handle.started_at.saturating_add(MAX_VIDEO_WAIT.as_secs());
        let result = loop {
            // The hour bounds the provider settling the job, including a status
            // request in flight and the wait between polls: the connection's
            // own timeout may be longer than what is left.
            let remaining = deadline.saturating_sub(now());
            if remaining == 0 {
                return Err(AdapterError::provider_error(
                    "the provider did not finish the job within 1 hour",
                ));
            }
            let status = tokio::time::timeout(
                Duration::from_secs(remaining),
                model.do_status(&handle.operation, &options),
            )
            .await
            .map_err(|_| {
                AdapterError::provider_error("the provider did not finish the job within 1 hour")
            })?
            .map_err(|error| failure(error, &cancel, &secrets))?;
            match status {
                VideoOperationStatus::Pending => {
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            return Err(AdapterError::provider_error("The job was cancelled"));
                        }
                        _ = tokio::time::sleep(
                            VIDEO_POLL_INTERVAL.min(Duration::from_secs(remaining)),
                        ) => {}
                    }
                }
                VideoOperationStatus::Completed(result) => break result,
            }
        };
        let (mut files, links) = job::video_output(&result, &handle.job.work_dir)?;
        if links.is_empty() {
            return Ok(AdapterOutput { files, data: None });
        }
        for link in links {
            let index = files.len() + 1;
            let name = job::video_name(index, &link.media_type);
            let path = handle.job.work_dir.join(&name);
            // The credential travels only to the connection's own origin, and
            // the client follows a redirect only as far as that credential
            // may go.
            let credentialed = match &connection.connection {
                ModelConnection::Http(http) => same_origin(&link.url, &http.base_url),
                _ => false,
            };
            let client = self.transport.client(network(connection)?, credentialed)?;
            download(&link.url, connection, &client, &path, &cancel).await?;
            files.push(JobFile {
                name,
                media_type: link.media_type,
                path,
            });
        }
        Ok(AdapterOutput { files, data: None })
    }
}

/// The network facts of one connection.
fn network(connection: &ModalityConnection) -> Result<&ModelNetwork, AdapterError> {
    match &connection.connection {
        ModelConnection::Http(http) => Ok(&http.network),
        // Media::provider refuses both before a call reaches here; the fixed
        // sentences keep one reason for a connection no media row serves.
        ModelConnection::Bedrock(_) => Err(AdapterError::unavailable(
            "No bundled catalog model declares media on an Amazon Bedrock connection",
        )),
        ModelConnection::Local => Err(AdapterError::unavailable(
            "The model's connection is local and serves no media call",
        )),
    }
}

/// One started video job, as the task record keeps it.
#[derive(serde::Serialize, serde::Deserialize)]
struct VideoHandle {
    /// `VideoOperationStart.operation`: the provider's own reference.
    operation: Value,
    /// Everything `wait` needs after a restart, which the record does not hand
    /// back to the adapter.
    job: Job,
    /// When `submit` started the job, in Unix seconds: the one-hour bound
    /// counts from here, a restart included.
    started_at: u64,
}

/// Streams one video into `path`, counting the bytes.
///
/// Every byte is flushed to `path` before this returns, so a reader that
/// opens the file after the call sees the whole video: a `tokio::fs::File`
/// does not finish its writes when it is dropped.
///
/// The connection's headers reach the request only when the URL shares the
/// connection's origin: a provider hands back a pre-signed URL on another
/// host, and the credential must not travel there.
async fn download(
    url: &str,
    connection: &ModalityConnection,
    client: &reqwest::Client,
    path: &Path,
    cancel: &CancellationToken,
) -> Result<u64, AdapterError> {
    let mut request = client.get(url);
    if let ModelConnection::Http(http) = &connection.connection
        && same_origin(url, &http.base_url)
    {
        for (name, value) in &http.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        if let Some(bearer) = &http.bearer {
            request = request.bearer_auth(bearer);
        }
    }
    let response = tokio::select! {
        _ = cancel.cancelled() => {
            return Err(AdapterError::provider_error("The download was cancelled"));
        }
        response = request.send() => response.map_err(|_| {
            AdapterError::provider_error("The video cannot be downloaded")
        })?,
    };
    if !response.status().is_success() {
        return Err(AdapterError::provider_error(format!(
            "The video download answered HTTP {}",
            response.status()
        )));
    }
    let mut file = tokio::fs::File::create(path).await.map_err(|error| {
        AdapterError::provider_error(format!("The video cannot be written: {error}"))
    })?;
    use futures_util::StreamExt as _;
    use tokio::io::AsyncWriteExt as _;
    let mut written = 0u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|_| AdapterError::provider_error("The video download was interrupted"))?;
        written += chunk.len() as u64;
        if written > MAX_VIDEO_BYTES {
            return Err(AdapterError::provider_error(
                "The video exceeds the 2 GiB download limit",
            ));
        }
        file.write_all(&chunk).await.map_err(|error| {
            AdapterError::provider_error(format!("The video cannot be written: {error}"))
        })?;
    }
    file.flush().await.map_err(|error| {
        AdapterError::provider_error(format!("The video cannot be written: {error}"))
    })?;
    Ok(written)
}

/// Whether two URLs share scheme, host and port.
fn same_origin(left: &str, right: &str) -> bool {
    url::Url::parse(left)
        .ok()
        .zip(url::Url::parse(right).ok())
        .is_some_and(|(left, right)| left.origin() == right.origin())
}

/// The values one call must never write into a record: the credential, and
/// the header values that may carry it.
struct Secrets(Vec<String>);

impl Secrets {
    fn of(connection: &ModalityConnection) -> Self {
        let mut values = Vec::new();
        if let ModelConnection::Http(http) = &connection.connection {
            for value in [&http.bearer, &http.api_key].into_iter().flatten() {
                if !value.is_empty() {
                    values.push(value.clone());
                }
            }
            for value in http.headers.values() {
                if !value.is_empty() && !values.contains(value) {
                    values.push(value.clone());
                }
            }
        }
        Self(values)
    }
    /// The message with every URL and every secret removed.
    fn redact(&self, message: &str) -> String {
        let mut text = strip_urls(message);
        for secret in &self.0 {
            text = text.replace(secret.as_str(), "[redacted]");
        }
        text
    }
}

/// `message` with every `http(s)://…` token replaced by `[url]`.
fn strip_urls(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while !rest.is_empty() {
        let start = rest
            .char_indices()
            .find(|(index, _)| {
                let tail = &rest[*index..];
                tail.starts_with("https://") || tail.starts_with("http://")
            })
            .map(|(index, _)| index);
        match start {
            Some(index) => {
                out.push_str(&rest[..index]);
                let tail = &rest[index..];
                let end = tail.find(char::is_whitespace).unwrap_or(tail.len());
                out.push_str("[url]");
                rest = &tail[end..];
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

/// One aimux failure as the adapter reports it, with nothing a record may not
/// hold: [`dim_agent_aimux::failure`] is the one-line classification a chat
/// call already reports, and the adapter scrubs what a record must not store.
fn failure(error: AiMuxError, cancel: &CancellationToken, secrets: &Secrets) -> AdapterError {
    let unavailable = matches!(
        error,
        AiMuxError::NoSuchModel { .. }
            | AiMuxError::NoSuchProvider { .. }
            | AiMuxError::UnsupportedFunctionality(_)
    );
    let message = secrets.redact(&dim_agent_aimux::failure(error, cancel).to_string());
    if unavailable {
        AdapterError::unavailable(message)
    } else {
        AdapterError::provider_error(message)
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// The aimux operation a job's `<category>.<operation>` names.
fn kind_of(job: &Job) -> Result<Operation, AdapterError> {
    match (job.category.as_str(), job.operation.as_str()) {
        ("image", "generate") | ("image", "edit") => Ok(Operation::Image),
        ("video", "generate") => Ok(Operation::Video),
        ("speech", "synthesize") => Ok(Operation::Speech),
        ("speech", "transcribe") => Ok(Operation::Transcription),
        (category, operation) => Err(AdapterError::invalid_input(format!(
            "The aimux adapter serves no {category}.{operation}"
        ))),
    }
}

fn image_generate_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "prompt": {"type": "string", "minLength": 1},
            "n": {"type": "integer", "minimum": 1, "maximum": 4},
            "size": {"type": "string", "pattern": "^[0-9]+x[0-9]+$"},
            "aspectRatio": {"type": "string", "pattern": "^[0-9]+:[0-9]+$"},
            "seed": {"type": "integer", "minimum": 0},
            "outputFormat": {"enum": ["png", "jpeg", "webp"]},
            "providerOptions": {"type": "object"}
        },
        "required": ["prompt"],
        "additionalProperties": false
    })
}

fn image_edit_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "prompt": {"type": "string"},
            "n": {"type": "integer", "minimum": 1, "maximum": 4},
            "size": {"type": "string", "pattern": "^[0-9]+x[0-9]+$"},
            "seed": {"type": "integer", "minimum": 0},
            "outputFormat": {"enum": ["png", "jpeg", "webp"]},
            "mask": {"type": "string"},
            "providerOptions": {"type": "object"}
        },
        "additionalProperties": false
    })
}

fn video_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "prompt": {"type": "string"},
            "aspectRatio": {"type": "string", "pattern": "^[0-9]+:[0-9]+$"},
            "resolution": {"type": "string", "pattern": "^[0-9]+x[0-9]+$"},
            "durationSeconds": {"type": "integer", "minimum": 1, "maximum": 60},
            "fps": {"type": "integer", "minimum": 1, "maximum": 60},
            "seed": {"type": "integer", "minimum": 0},
            "generateAudio": {"type": "boolean"},
            "providerOptions": {"type": "object"}
        },
        "additionalProperties": false
    })
}

fn speech_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "text": {"type": "string", "minLength": 1},
            "voice": {"type": "string"},
            "outputFormat": {"enum": ["mp3", "opus", "aac", "flac", "wav", "pcm"]},
            "speed": {"type": "number", "minimum": 0},
            "instructions": {"type": "string"},
            "language": {"type": "string"},
            "providerOptions": {"type": "object"}
        },
        "required": ["text"],
        "additionalProperties": false
    })
}

fn transcription_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "language": {"type": "string"},
            "prompt": {"type": "string"},
            "providerOptions": {"type": "object"}
        },
        "additionalProperties": false
    })
}

/// The plugin that provides the adapter at the process scope.
struct AdapterPlugin(Arc<MediaAdapter>);

impl DynPlugin for AdapterPlugin {
    fn name(&self) -> &str {
        "ModalityAimux"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, rutis::Error>> {
        Box::pin(async move {
            ctx.provide(ModalityAdapterService(self.0.clone()))?;
            Ok(Effect::Done)
        })
    }
}

/// Installs the built-in adapter as a process plugin.
///
/// The host registers the service name first, and adds this plugin before
/// Modality's own: Modality reads the service once, when it starts.
pub fn install(builder: &mut AgentBuilder) {
    dim_agent_runtime::names::register::<ModalityAdapterService>(builder.catalog_mut(), None);
    builder.plugin(|_| {
        Ok(Box::new(AdapterPlugin(Arc::new(MediaAdapter::new()))) as Box<dyn DynPlugin>)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn pending_job(task_id: &str) -> Job {
        Job {
            task_id: task_id.to_owned(),
            category: "image".to_owned(),
            operation: "generate".to_owned(),
            model: ModelId {
                provider_id: "openai".to_owned(),
                model_id: "gpt-image-2".to_owned(),
            },
            params: json!({"prompt": "a cat"}),
            files: Vec::new(),
            work_dir: PathBuf::from("/tmp"),
        }
    }

    #[test]
    fn an_unwaited_job_is_swept_after_the_age_bound() {
        let adapter = MediaAdapter::new();
        let now = Instant::now();
        adapter.keep(
            pending_job("mt_old"),
            now - MAX_PENDING_AGE - Duration::from_secs(1),
        );
        adapter.keep(pending_job("mt_new"), now);
        let jobs = adapter.jobs.lock().unwrap();
        assert!(!jobs.contains_key("mt_old"), "the aged job was swept");
        assert!(jobs.contains_key("mt_new"), "the fresh job is kept");
    }

    #[test]
    fn the_pending_map_keeps_at_most_the_job_bound() {
        let adapter = MediaAdapter::new();
        let now = Instant::now();
        for index in 0..=MAX_PENDING_JOBS {
            adapter.keep(
                pending_job(&format!("mt_{index}")),
                now + Duration::from_millis(index as u64),
            );
        }
        let jobs = adapter.jobs.lock().unwrap();
        assert_eq!(jobs.len(), MAX_PENDING_JOBS);
        assert!(!jobs.contains_key("mt_0"), "the oldest job made room");
        assert!(jobs.contains_key(&format!("mt_{MAX_PENDING_JOBS}")));
    }
}
