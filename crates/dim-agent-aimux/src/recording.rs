//! RFC-0023 recording of model wire exchanges, switched by the App Server's
//! `model-log` Settings source. Dim calls providers directly, so it also binds
//! the recorder to each request and closes the recording when the stream ends.
use aimux_core::recording::{
    HttpExchange, JsonlRecorder, OutcomeRecord, OutcomeStatus, ProviderRecord, Recorder,
    RecordingContext, RecordingError, ResponseRecord, init_recording, recorder,
};
use std::{path::Path, sync::Arc};

struct StartupRecorder(Arc<dyn Recorder>);

fn redact_headers(headers: &mut [(String, String)]) {
    for (_, value) in headers {
        *value = "[REDACTED]".into();
    }
}

fn redact_option_headers(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            for (name, value) in fields {
                if name.eq_ignore_ascii_case("headers") {
                    if let Some(headers) = value.as_object_mut() {
                        for value in headers.values_mut() {
                            *value = "[REDACTED]".into();
                        }
                    }
                } else {
                    redact_option_headers(value);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_option_headers(value);
            }
        }
        _ => {}
    }
}

impl Recorder for StartupRecorder {
    fn record_input(
        &self,
        call_id: &str,
        options: &aimux_core::options::CallOptions,
        provider: &str,
        model_id: &str,
    ) {
        let mut options = options.clone();
        if let Some(headers) = &mut options.headers {
            for value in headers.values_mut() {
                *value = "[REDACTED]".into();
            }
        }
        if let Some(providers) = &mut options.provider_options {
            for value in providers.values_mut() {
                redact_option_headers(value);
            }
        }
        self.0.record_input(call_id, &options, provider, model_id);
    }
    fn record_session(&self, call_id: &str, session_id: &str, step: u32) {
        self.0.record_session(call_id, session_id, step);
    }
    fn record_provider(&self, call_id: &str, snapshot: &ProviderRecord) {
        let mut snapshot = snapshot.clone();
        if let Some(options) = &mut snapshot.provider_options {
            redact_option_headers(options);
        }
        self.0.record_provider(call_id, &snapshot);
    }
    fn record_exchange(&self, call_id: &str, exchange: &HttpExchange) {
        let mut exchange = exchange.clone();
        redact_headers(&mut exchange.request.headers);
        if let Some(response) = &mut exchange.response {
            redact_headers(&mut response.headers);
        }
        self.0.record_exchange(call_id, &exchange);
    }
    fn record_exchange_update(
        &self,
        call_id: &str,
        attempt: u32,
        exchange_index: u32,
        response: &ResponseRecord,
        error: Option<String>,
    ) {
        let mut response = response.clone();
        redact_headers(&mut response.headers);
        self.0
            .record_exchange_update(call_id, attempt, exchange_index, &response, error);
    }
    fn record_outcome(&self, call_id: &str, outcome: &OutcomeRecord) {
        self.0.record_outcome(call_id, outcome);
    }
    fn record_transport_closed(&self, call_id: &str) {
        self.0.record_transport_closed(call_id);
    }
    fn flush(&self) {
        self.0.flush();
    }
    fn try_flush(&self) -> Result<(), RecordingError> {
        self.0.try_flush()
    }
}

/// One request's recorder and correlation id. The request carries the context;
/// this keeps what the stream's terminal needs.
pub(crate) struct Capture {
    pub(crate) recorder: Arc<dyn Recorder>,
    pub(crate) call_id: String,
}

impl Capture {
    pub(crate) fn input(
        &self,
        options: &aimux_core::options::CallOptions,
        provider: &str,
        model_id: &str,
    ) {
        self.recorder
            .record_input(&self.call_id, options, provider, model_id);
    }

    /// The terminal a request writes when it ends before its stream installs
    /// the outcome wrapper: cancellation and provider errors before response
    /// headers. Disarm it once the wrapper owns the stream.
    pub(crate) fn pending(&self) -> Pending {
        Pending {
            recorder: self.recorder.clone(),
            call_id: self.call_id.clone(),
            terminal: None,
            armed: true,
        }
    }
}

pub(crate) struct Pending {
    recorder: Arc<dyn Recorder>,
    call_id: String,
    terminal: Option<OutcomeRecord>,
    armed: bool,
}

impl Pending {
    pub(crate) fn failed(&mut self, message: &str) {
        self.terminal = Some(OutcomeRecord {
            status: OutcomeStatus::Error,
            error: Some(message.to_owned()),
            ..OutcomeRecord::default()
        });
    }

    pub(crate) fn cancelled(&mut self) {
        self.terminal = Some(OutcomeRecord {
            status: OutcomeStatus::Cancelled,
            ..OutcomeRecord::default()
        });
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let outcome = self.terminal.take().unwrap_or(OutcomeRecord {
            status: OutcomeStatus::Cancelled,
            ..OutcomeRecord::default()
        });
        self.recorder.record_outcome(&self.call_id, &outcome);
        self.recorder.record_transport_closed(&self.call_id);
    }
}

/// Binds the request to the recorder that is on now, if any.
pub(crate) fn capture(
    call_id: String,
    options: &mut aimux_core::options::CallOptions,
    startup: bool,
) -> Option<Capture> {
    let mut recorder = recorder()?;
    if startup {
        recorder = Arc::new(StartupRecorder(recorder));
    }
    options.call_id = Some(call_id.clone());
    options.recording_context = Some(RecordingContext::new(call_id.clone(), recorder.clone()));
    Some(Capture { recorder, call_id })
}

/// Writes every request's wire exchanges under `directory`, or stops when
/// `None`. Requests started afterwards carry the new state; one already in
/// flight finishes with the recorder it started with.
pub fn set_request_recording(directory: Option<&Path>) -> Result<(), RecordingError> {
    let recorder = match directory {
        Some(directory) => {
            secure(directory)?;
            let recorder = JsonlRecorder::try_new(directory)?;
            Some(Arc::new(recorder) as Arc<dyn Recorder>)
        }
        None => None,
    };
    init_recording(recorder);
    Ok(())
}

/// Creates the directory and file private before the recorder appends. The
/// recorder keeps an existing file's mode, so this must run first.
#[cfg(unix)]
fn secure(directory: &Path) -> Result<(), RecordingError> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(directory)
        .map_err(|source| RecordingError::Init {
            path: directory.to_owned(),
            source,
        })?;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).map_err(
        |source| RecordingError::Init {
            path: directory.to_owned(),
            source,
        },
    )?;
    let file = directory.join("recordings.jsonl");
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&file)
        .map_err(|source| RecordingError::OpenFile {
            path: file.clone(),
            source,
        })?;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))
        .map_err(|source| RecordingError::OpenFile { path: file, source })
}

#[cfg(not(unix))]
fn secure(_: &Path) -> Result<(), RecordingError> {
    Ok(())
}
