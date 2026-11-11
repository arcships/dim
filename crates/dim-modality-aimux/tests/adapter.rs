//! The adapter against a scripted transport: the exact request each operation
//! sends, the files it writes, and what a failure may say.

use aimux_provider_utils::{Fetch, FetchError, FetchFunction, FetchRequest, FetchResponse};
use async_trait::async_trait;
use bytes::Bytes;
use dim_agent_capabilities::host::{
    HttpModelConnection, ModalityConnection, ModelConnection, ModelNetwork, ModelProtocol,
};
use dim_agent_types::ModelId;
use dim_modality::{AdapterErrorCode, Job, JobFile, ModalityAdapter, ProgressSink};
use dim_modality_aimux::{MAX_VIDEO_WAIT, MediaAdapter};
use http::{HeaderMap, Method, StatusCode};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

const CREDENTIAL: &str = "test-credential";

/// One request the adapter sent.
#[derive(Clone, Debug)]
struct Recorded {
    method: Method,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
    fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// A reply a responder produces.
struct Reply {
    status: StatusCode,
    body: Vec<u8>,
}

fn ok_json(body: Value) -> Reply {
    Reply {
        status: StatusCode::OK,
        body: serde_json::to_vec(&body).unwrap(),
    }
}

fn ok_bytes(body: impl Into<Vec<u8>>) -> Reply {
    Reply {
        status: StatusCode::OK,
        body: body.into(),
    }
}

/// A 401 whose body names the credential and a URL, as a provider's does.
fn unauthorized() -> Reply {
    Reply {
        status: StatusCode::UNAUTHORIZED,
        body: format!(
            r#"{{"error":{{"message":"unauthorized: the key {CREDENTIAL} is not valid for https://api.example.test/v1?key={CREDENTIAL}","detail":"BODY-MARKER-7731"}}}}"#
        )
        .into_bytes(),
    }
}

#[derive(Clone)]
struct Scripted {
    requests: Arc<Mutex<Vec<Recorded>>>,
    respond: Arc<dyn Fn(&Recorded) -> Reply + Send + Sync>,
}

#[async_trait]
impl Fetch for Scripted {
    async fn fetch(&self, request: FetchRequest) -> Result<FetchResponse, FetchError> {
        let recorded = Recorded {
            method: request.method.clone(),
            url: request.url.to_string(),
            headers: request
                .headers
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_owned(),
                        value.to_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect(),
            body: request.body.to_vec(),
        };
        self.requests.lock().unwrap().push(recorded.clone());
        let reply = (self.respond)(&recorded);
        Ok(FetchResponse::from_bytes(
            reply.status,
            HeaderMap::new(),
            request.url,
            Bytes::from(reply.body),
        ))
    }
}

/// An adapter whose transport answers `respond`, with every request recorded.
fn scripted(
    respond: impl Fn(&Recorded) -> Reply + Send + Sync + 'static,
) -> (Arc<MediaAdapter>, Arc<Mutex<Vec<Recorded>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let fetch: FetchFunction = Arc::new(Scripted {
        requests: requests.clone(),
        respond: Arc::new(respond),
    });
    (Arc::new(MediaAdapter::scripted(fetch)), requests)
}

fn connection(base_url: &str, model: &str) -> ModalityConnection {
    ModalityConnection {
        connection: ModelConnection::Http(HttpModelConnection {
            protocol: ModelProtocol::OpenaiCompatible,
            base_url: base_url.to_owned(),
            network: ModelNetwork::default(),
            headers: Default::default(),
            bearer: Some(CREDENTIAL.to_owned()),
            api_key: None,
            preset: None,
        }),
        protocol: ModelProtocol::OpenaiCompatible,
        model: model.to_owned(),
        realtime: None,
    }
}

/// The same connection with a request timeout, as a provider record sets one.
fn connection_with_timeout(base_url: &str, model: &str, timeout_ms: u64) -> ModalityConnection {
    let mut connection = connection(base_url, model);
    if let ModelConnection::Http(http) = &mut connection.connection {
        http.network.timeout_ms = Some(timeout_ms);
    }
    connection
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dim-modality-aimux-{name}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn input(dir: &Path, name: &str, media_type: &str, bytes: &[u8]) -> JobFile {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    JobFile {
        name: name.to_owned(),
        media_type: media_type.to_owned(),
        path,
    }
}

fn job(
    provider: &str,
    model: &str,
    category: &str,
    operation: &str,
    params: Value,
    files: Vec<JobFile>,
    work_dir: &Path,
) -> Job {
    Job {
        task_id: uuid::Uuid::new_v4().to_string(),
        category: category.to_owned(),
        operation: operation.to_owned(),
        model: ModelId {
            provider_id: provider.to_owned(),
            model_id: model.to_owned(),
        },
        params,
        files,
        work_dir: work_dir.to_path_buf(),
    }
}

/// The 8-byte signature and IHDR length of a PNG, which is all the media-type
/// sniffing reads.
const PNG_HEADER: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";

/// An MP3 with an ID3 tag, the bytes OpenAI returns when it silently falls
/// back to mp3.
const MP3_HEADER: &[u8] = b"ID3\x04\x00\x00\x00\x00\x00\x00";

fn base64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Runs a blocking operation end to end through the adapter.
async fn run(
    adapter: &Arc<MediaAdapter>,
    job: Job,
    base_url: &str,
    model: &str,
) -> Result<dim_modality::AdapterOutput, dim_modality::AdapterError> {
    let task_id = job.task_id.clone();
    adapter
        .submit(job, connection(base_url, model), CancellationToken::new())
        .await?;
    adapter
        .wait(
            &task_id,
            None,
            connection(base_url, model),
            ProgressSink::none(),
            CancellationToken::new(),
        )
        .await
}

#[tokio::test]
async fn image_generate_pins_the_request_and_names_the_output() {
    let dir = work_dir("image-generate");
    let png = [PNG_HEADER, b"image-bytes"].concat();
    let reply_png = png.clone();
    let (adapter, requests) = scripted(move |_| {
        ok_json(json!({"created": 1, "data": [{"b64_json": base64(&reply_png)}]}))
    });
    let output = run(
        &adapter,
        job(
            "openai",
            "gpt-image-1",
            "image",
            "generate",
            json!({"prompt": "a cat", "n": 1, "size": "1024x1024", "outputFormat": "png"}),
            Vec::new(),
            &dir,
        ),
        "https://api.openai.com/v1",
        "gpt-image-1",
    )
    .await
    .unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.url, "https://api.openai.com/v1/images/generations");
    assert_eq!(
        request.header("authorization"),
        Some("Bearer test-credential")
    );
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["model"], "gpt-image-1");
    assert_eq!(body["prompt"], "a cat");
    assert_eq!(body["n"], 1);
    assert_eq!(body["size"], "1024x1024");
    assert_eq!(body["output_format"], "png");

    assert_eq!(output.files.len(), 1);
    assert_eq!(output.files[0].name, "image-1.png");
    assert_eq!(output.files[0].media_type, "image/png");
    assert_eq!(std::fs::read(&output.files[0].path).unwrap(), png);
    assert!(path_is_inside(&output.files[0].path, &dir));
    assert!(output.data.is_none());
}

#[tokio::test]
async fn image_edit_pins_the_multipart_fields() {
    let dir = work_dir("image-edit");
    let input = input(
        &dir,
        "cat.png",
        "image/png",
        &[PNG_HEADER, b"input"].concat(),
    );
    let (adapter, requests) = scripted(move |_| {
        ok_json(json!({"created": 1, "data": [{"b64_json": base64(PNG_HEADER)}]}))
    });
    run(
        &adapter,
        job(
            "openai",
            "gpt-image-1",
            "image",
            "edit",
            json!({"prompt": "add a hat"}),
            vec![input],
            &dir,
        ),
        "https://api.openai.com/v1",
        "gpt-image-1",
    )
    .await
    .unwrap();

    let requests = requests.lock().unwrap();
    let request = &requests[0];
    assert_eq!(request.url, "https://api.openai.com/v1/images/edits");
    assert!(
        request
            .header("content-type")
            .is_some_and(|value| value.starts_with("multipart/form-data; boundary="))
    );
    let body = request.body_text();
    for field in ["name=\"model\"", "name=\"prompt\"", "name=\"image\""] {
        assert!(body.contains(field), "multipart body has no {field}");
    }
    assert!(body.contains("add a hat"));
    assert!(body.contains("filename=\"image\""));
    assert!(body.contains("Content-Type: image/png"));
}

#[tokio::test]
async fn speech_pins_the_response_format_and_the_voice() {
    let dir = work_dir("speech");
    let mp3 = [MP3_HEADER, b"audio-bytes"].concat();
    let (adapter, requests) = scripted(move |_| ok_bytes(mp3.clone()));
    let output = run(
        &adapter,
        job(
            "openai",
            "gpt-4o-mini-tts",
            "speech",
            "synthesize",
            json!({"text": "hello", "voice": "alloy", "outputFormat": "mp3"}),
            Vec::new(),
            &dir,
        ),
        "https://api.openai.com/v1",
        "gpt-4o-mini-tts",
    )
    .await
    .unwrap();

    let requests = requests.lock().unwrap();
    let request = &requests[0];
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.url, "https://api.openai.com/v1/audio/speech");
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["model"], "gpt-4o-mini-tts");
    assert_eq!(body["input"], "hello");
    assert_eq!(body["voice"], "alloy");
    assert_eq!(body["response_format"], "mp3");

    assert_eq!(output.files[0].name, "speech.mp3");
    assert_eq!(output.files[0].media_type, "audio/mpeg");
}

/// The provider's bytes name the file, not the request: OpenAI answers mp3 when
/// it cannot produce the requested format, and that is not an error.
#[tokio::test]
async fn speech_is_named_by_its_bytes_not_the_requested_format() {
    let dir = work_dir("speech-fallback");
    let (adapter, _requests) = scripted(move |_| ok_bytes(MP3_HEADER.to_vec()));
    let output = run(
        &adapter,
        job(
            "openai",
            "gpt-4o-mini-tts",
            "speech",
            "synthesize",
            json!({"text": "hello", "outputFormat": "wav"}),
            Vec::new(),
            &dir,
        ),
        "https://api.openai.com/v1",
        "gpt-4o-mini-tts",
    )
    .await
    .unwrap();
    assert_eq!(output.files[0].name, "speech.mp3");
    assert_eq!(output.files[0].media_type, "audio/mpeg");
}

#[tokio::test]
async fn transcription_pins_the_file_field_and_the_data() {
    let dir = work_dir("transcription");
    let audio = input(&dir, "clip.mp3", "audio/mpeg", MP3_HEADER);
    let (adapter, requests) = scripted(move |_| ok_json(json!({"text": "hello there"})));
    let output = run(
        &adapter,
        job(
            "openai",
            "whisper-1",
            "speech",
            "transcribe",
            json!({"language": "en"}),
            vec![audio],
            &dir,
        ),
        "https://api.openai.com/v1",
        "whisper-1",
    )
    .await
    .unwrap();

    let requests = requests.lock().unwrap();
    let request = &requests[0];
    assert_eq!(request.method, Method::POST);
    assert_eq!(
        request.url,
        "https://api.openai.com/v1/audio/transcriptions"
    );
    let body = request.body_text();
    assert!(body.contains("name=\"file\""));
    assert!(body.contains("filename=\"audio.mp3\""));
    assert!(body.contains("Content-Type: audio/mpeg"));
    assert!(body.contains("name=\"model\""));
    assert!(body.contains("whisper-1"));
    assert!(body.contains("name=\"language\""));
    assert!(body.contains("\r\n\r\nen\r\n"));

    assert_eq!(output.files[0].name, "transcript.txt");
    assert_eq!(output.files[0].media_type, "text/plain");
    assert_eq!(
        std::fs::read_to_string(&output.files[0].path).unwrap(),
        "hello there"
    );
    let data = output.data.unwrap();
    assert_eq!(data["text"], "hello there");
    assert_eq!(data["segments"], json!([]));
    assert_eq!(data["language"], Value::Null);
}

/// A 401 is stored in a record: the message may carry neither the response
/// body nor the credential, and no URL.
#[tokio::test]
async fn a_provider_failure_carries_no_body_no_credential_and_no_url() {
    let dir = work_dir("redaction");
    let (adapter, _requests) = scripted(|_| unauthorized());
    let error = run(
        &adapter,
        job(
            "openai",
            "gpt-image-1",
            "image",
            "generate",
            json!({"prompt": "a cat"}),
            Vec::new(),
            &dir,
        ),
        "https://api.openai.com/v1",
        "gpt-image-1",
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, AdapterErrorCode::ProviderError);
    let message = &error.message;
    assert!(
        !message.contains(CREDENTIAL),
        "message carries the credential: {message}"
    );
    assert!(
        !message.contains("http"),
        "message carries a URL: {message}"
    );
    assert!(
        !message.contains("BODY-MARKER-7731"),
        "message carries the response body: {message}"
    );
    assert!(
        !message.contains('\n'),
        "message is not one line: {message}"
    );
}

#[tokio::test]
async fn operations_follow_the_binding_table() {
    let adapter = MediaAdapter::new();
    let openai = ModelId {
        provider_id: "openai".into(),
        model_id: "gpt-image-1".into(),
    };
    let names: Vec<(String, String)> = adapter
        .operations(&openai)
        .into_iter()
        .map(|operation| (operation.category, operation.operation))
        .collect();
    assert_eq!(
        names,
        vec![
            ("realtime".to_owned(), "voice".to_owned()),
            ("image".to_owned(), "generate".to_owned()),
            ("image".to_owned(), "edit".to_owned()),
            ("speech".to_owned(), "synthesize".to_owned()),
            ("speech".to_owned(), "transcribe".to_owned()),
        ]
    );
    // The relay speaks the transport a catalog entry declares, so it serves
    // a session for any provider; its schema refuses every parameter (D7).
    let voice = adapter
        .operations(&openai)
        .into_iter()
        .find(|operation| operation.category == "realtime")
        .unwrap();
    assert_eq!(voice.operation, "voice");
    assert_eq!(
        voice.params_schema,
        json!({"type": "object", "maxProperties": 0})
    );
    // xAI's package refuses image models, so Grok Imagine reports no image
    // operation.
    let xai = ModelId {
        provider_id: "xai".into(),
        model_id: "grok-imagine-video".into(),
    };
    let categories: Vec<String> = adapter
        .operations(&xai)
        .into_iter()
        .map(|operation| format!("{}.{}", operation.category, operation.operation))
        .collect();
    assert_eq!(
        categories,
        vec![
            "realtime.voice",
            "video.generate",
            "speech.synthesize",
            "speech.transcribe"
        ]
    );
    // A package that ignores an edit's input files serves generation alone.
    for (provider_id, model_id) in [
        ("stability-ai", "inpaint"),
        ("recraft", "recraftv3"),
        ("luma-ai", "photon"),
    ] {
        let model = ModelId {
            provider_id: provider_id.to_owned(),
            model_id: model_id.to_owned(),
        };
        let names: Vec<String> = adapter
            .operations(&model)
            .into_iter()
            .map(|operation| format!("{}.{}", operation.category, operation.operation))
            .collect();
        assert_eq!(
            names,
            vec!["realtime.voice", "image.generate"],
            "{provider_id}"
        );
    }
    // A package that reports no file input for a model drops edit for it.
    let dall_e = ModelId {
        provider_id: "openai".into(),
        model_id: "dall-e-3".into(),
    };
    let names: Vec<String> = adapter
        .operations(&dall_e)
        .into_iter()
        .map(|operation| format!("{}.{}", operation.category, operation.operation))
        .collect();
    assert!(names.contains(&"image.generate".to_owned()));
    assert!(names.contains(&"realtime.voice".to_owned()));
    assert!(
        !names.contains(&"image.edit".to_owned()),
        "dall-e-3 takes no file input: {names:?}"
    );
    // One that reads the files keeps both statements.
    let flux = ModelId {
        provider_id: "black-forest-labs".into(),
        model_id: "flux-2-pro".into(),
    };
    let names: Vec<String> = adapter
        .operations(&flux)
        .into_iter()
        .map(|operation| format!("{}.{}", operation.category, operation.operation))
        .collect();
    assert_eq!(
        names,
        vec!["realtime.voice", "image.generate", "image.edit"]
    );
    // A created provider's UUID matches no row.
    let created = ModelId {
        provider_id: "6f1c2f9e-0000-4000-8000-000000000000".into(),
        model_id: "anything".into(),
    };
    assert_eq!(
        adapter
            .operations(&created)
            .into_iter()
            .map(|operation| format!("{}.{}", operation.category, operation.operation))
            .collect::<Vec<String>>(),
        vec!["realtime.voice"]
    );
}

#[tokio::test]
async fn an_unmapped_provider_is_unavailable() {
    let dir = work_dir("unmapped");
    let (adapter, _requests) = scripted(|_| ok_json(json!({})));
    let error = run(
        &adapter,
        job(
            "6f1c2f9e-0000-4000-8000-000000000000",
            "anything",
            "image",
            "generate",
            json!({"prompt": "a cat"}),
            Vec::new(),
            &dir,
        ),
        "https://api.example.test/v1",
        "anything",
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, AdapterErrorCode::Unavailable);
}

fn path_is_inside(path: &Path, dir: &Path) -> bool {
    path.strip_prefix(dir).is_ok()
}

/// One (row, operation) pair with the exact request it must send.
struct Case {
    provider: &'static str,
    base: &'static str,
    model: &'static str,
    category: &'static str,
    operation: &'static str,
    params: &'static str,
    file: Option<&'static str>,
    url: &'static str,
}

/// Every row of the binding table and every operation it serves, with the
/// catalog's `defaultBaseUrl` and the exact URL the package appends.
///
/// `google` is listed with no base URL: the bundled catalog carries none, so
/// the package's own default applies.
const CASES: &[Case] = &[
    Case {
        provider: "openai",
        base: "https://api.openai.com/v1",
        model: "gpt-image-1",
        category: "image",
        operation: "generate",
        params: r#"{"prompt":"a cat"}"#,
        file: None,
        url: "https://api.openai.com/v1/images/generations",
    },
    Case {
        provider: "openai",
        base: "https://api.openai.com/v1",
        model: "gpt-image-1",
        category: "image",
        operation: "edit",
        params: r#"{"prompt":"a cat"}"#,
        file: Some("image/png"),
        url: "https://api.openai.com/v1/images/edits",
    },
    Case {
        provider: "openai",
        base: "https://api.openai.com/v1",
        model: "gpt-4o-mini-tts",
        category: "speech",
        operation: "synthesize",
        params: r#"{"text":"hello"}"#,
        file: None,
        url: "https://api.openai.com/v1/audio/speech",
    },
    Case {
        provider: "openai",
        base: "https://api.openai.com/v1",
        model: "whisper-1",
        category: "speech",
        operation: "transcribe",
        params: r#"{}"#,
        file: Some("audio/mpeg"),
        url: "https://api.openai.com/v1/audio/transcriptions",
    },
    Case {
        provider: "google",
        base: "",
        model: "gemini-2.5-flash-image",
        category: "image",
        operation: "generate",
        params: r#"{"prompt":"a cat"}"#,
        file: None,
        url: "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash-image:generateContent",
    },
    Case {
        provider: "google",
        base: "",
        model: "gemini-2.5-flash-image",
        category: "image",
        operation: "edit",
        params: r#"{"prompt":"a cat"}"#,
        file: Some("image/png"),
        url: "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash-image:generateContent",
    },
    Case {
        provider: "google",
        base: "",
        model: "veo-3.1",
        category: "video",
        operation: "generate",
        params: r#"{"prompt":"a cat"}"#,
        file: None,
        url: "https://generativelanguage.googleapis.com/v1beta/models/veo-3.1:predictLongRunning",
    },
    Case {
        provider: "google",
        base: "",
        model: "gemini-2.5-flash-preview-tts",
        category: "speech",
        operation: "synthesize",
        params: r#"{"text":"hello"}"#,
        file: None,
        url: "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash-preview-tts:generateContent",
    },
    Case {
        provider: "google",
        base: "",
        model: "gemini-2.5-flash",
        category: "speech",
        operation: "transcribe",
        params: r#"{}"#,
        file: Some("audio/mpeg"),
        url: "https://generativelanguage.googleapis.com/v1beta/interactions",
    },
    Case {
        provider: "xai",
        base: "https://api.x.ai/v1",
        model: "grok-imagine-video",
        category: "video",
        operation: "generate",
        params: r#"{"prompt":"a cat"}"#,
        file: None,
        url: "https://api.x.ai/v1/videos/generations",
    },
    Case {
        provider: "xai",
        base: "https://api.x.ai/v1",
        model: "speech",
        category: "speech",
        operation: "synthesize",
        params: r#"{"text":"hello"}"#,
        file: None,
        url: "https://api.x.ai/v1/tts",
    },
    Case {
        provider: "xai",
        base: "https://api.x.ai/v1",
        model: "stt",
        category: "speech",
        operation: "transcribe",
        params: r#"{}"#,
        file: Some("audio/mpeg"),
        url: "https://api.x.ai/v1/stt",
    },
    Case {
        provider: "grok-build",
        base: "https://api.x.ai/v1",
        model: "grok-imagine-video",
        category: "video",
        operation: "generate",
        params: r#"{"prompt":"a cat"}"#,
        file: None,
        url: "https://api.x.ai/v1/videos/generations",
    },
    Case {
        provider: "grok-build",
        base: "https://api.x.ai/v1",
        model: "speech",
        category: "speech",
        operation: "synthesize",
        params: r#"{"text":"hello"}"#,
        file: None,
        url: "https://api.x.ai/v1/tts",
    },
    Case {
        provider: "grok-build",
        base: "https://api.x.ai/v1",
        model: "stt",
        category: "speech",
        operation: "transcribe",
        params: r#"{}"#,
        file: Some("audio/mpeg"),
        url: "https://api.x.ai/v1/stt",
    },
    Case {
        provider: "elevenlabs",
        base: "https://api.elevenlabs.io",
        model: "eleven_flash_v2_5",
        category: "speech",
        operation: "synthesize",
        params: r#"{"text":"hello","voice":"test-voice"}"#,
        file: None,
        url: "https://api.elevenlabs.io/v1/text-to-speech/test-voice",
    },
    Case {
        provider: "elevenlabs",
        base: "https://api.elevenlabs.io",
        model: "scribe_v1",
        category: "speech",
        operation: "transcribe",
        params: r#"{}"#,
        file: Some("audio/mpeg"),
        url: "https://api.elevenlabs.io/v1/speech-to-text",
    },
    Case {
        provider: "fal",
        base: "https://queue.fal.run",
        model: "fal-ai/flux/dev",
        category: "image",
        operation: "generate",
        params: r#"{"prompt":"a cat"}"#,
        file: None,
        url: "https://fal.run/fal-ai/flux/dev",
    },
    Case {
        provider: "fal",
        base: "https://queue.fal.run",
        model: "fal-ai/flux/dev",
        category: "image",
        operation: "edit",
        params: r#"{"prompt":"a cat"}"#,
        file: Some("image/png"),
        url: "https://fal.run/fal-ai/flux/dev",
    },
    Case {
        provider: "fal",
        base: "https://queue.fal.run",
        model: "fal-ai/kling-video",
        category: "video",
        operation: "generate",
        params: r#"{"prompt":"a cat"}"#,
        file: None,
        url: "https://queue.fal.run/fal-ai/kling-video",
    },
    Case {
        provider: "fal",
        base: "https://queue.fal.run",
        model: "whisper",
        category: "speech",
        operation: "transcribe",
        params: r#"{}"#,
        file: Some("audio/mpeg"),
        url: "https://queue.fal.run/fal-ai/whisper",
    },
    Case {
        provider: "deepgram",
        base: "https://api.deepgram.com",
        model: "nova-3",
        category: "speech",
        operation: "transcribe",
        params: r#"{}"#,
        file: Some("audio/mpeg"),
        url: "https://api.deepgram.com/v1/listen",
    },
    Case {
        provider: "assemblyai",
        base: "https://api.assemblyai.com",
        model: "universal",
        category: "speech",
        operation: "transcribe",
        params: r#"{}"#,
        file: Some("audio/mpeg"),
        url: "https://api.assemblyai.com/v2/upload",
    },
    Case {
        provider: "cartesia",
        base: "https://api.cartesia.ai",
        model: "sonic-3",
        category: "speech",
        operation: "synthesize",
        params: r#"{"text":"hello","voice":"test-voice"}"#,
        file: None,
        url: "https://api.cartesia.ai/tts/bytes",
    },
    Case {
        provider: "cartesia",
        base: "https://api.cartesia.ai",
        model: "ink-whisper",
        category: "speech",
        operation: "transcribe",
        params: r#"{}"#,
        file: Some("audio/mpeg"),
        url: "https://api.cartesia.ai/stt",
    },
    Case {
        provider: "gladia",
        base: "https://api.gladia.io",
        model: "solaria-1",
        category: "speech",
        operation: "transcribe",
        params: r#"{}"#,
        file: Some("audio/mpeg"),
        url: "https://api.gladia.io/v2/upload",
    },
    Case {
        provider: "groq",
        base: "https://api.groq.com/openai/v1",
        model: "whisper-large-v3",
        category: "speech",
        operation: "transcribe",
        params: r#"{}"#,
        file: Some("audio/mpeg"),
        url: "https://api.groq.com/openai/v1/audio/transcriptions",
    },
    Case {
        provider: "replicate",
        base: "https://api.replicate.com/v1",
        model: "black-forest-labs/flux-schnell",
        category: "image",
        operation: "generate",
        params: r#"{"prompt":"a cat"}"#,
        file: None,
        url: "https://api.replicate.com/v1/models/black-forest-labs/flux-schnell/predictions",
    },
    Case {
        provider: "replicate",
        base: "https://api.replicate.com/v1",
        model: "black-forest-labs/flux-schnell",
        category: "image",
        operation: "edit",
        params: r#"{"prompt":"a cat"}"#,
        file: Some("image/png"),
        url: "https://api.replicate.com/v1/models/black-forest-labs/flux-schnell/predictions",
    },
    Case {
        provider: "replicate",
        base: "https://api.replicate.com/v1",
        model: "minimax/video-01",
        category: "video",
        operation: "generate",
        params: r#"{"prompt":"a cat"}"#,
        file: None,
        url: "https://api.replicate.com/v1/predictions",
    },
    Case {
        provider: "stability-ai",
        base: "https://api.stability.ai/v2beta",
        model: "core",
        category: "image",
        operation: "generate",
        params: r#"{"prompt":"a cat"}"#,
        file: None,
        url: "https://api.stability.ai/v2beta/stable-image/generate/core",
    },
    Case {
        provider: "recraft",
        base: "https://external.api.recraft.ai",
        model: "recraftv3",
        category: "image",
        operation: "generate",
        params: r#"{"prompt":"a cat"}"#,
        file: None,
        url: "https://external.api.recraft.ai/v1/images/generations",
    },
    Case {
        provider: "black-forest-labs",
        base: "https://api.bfl.ai/v1",
        model: "flux-pro-1.1",
        category: "image",
        operation: "generate",
        params: r#"{"prompt":"a cat"}"#,
        file: None,
        url: "https://api.bfl.ai/v1/flux-pro-1.1",
    },
    Case {
        provider: "black-forest-labs",
        base: "https://api.bfl.ai/v1",
        model: "flux-pro-1.1",
        category: "image",
        operation: "edit",
        params: r#"{"prompt":"a cat"}"#,
        file: Some("image/png"),
        url: "https://api.bfl.ai/v1/flux-pro-1.1",
    },
    Case {
        provider: "luma-ai",
        base: "https://api.lumalabs.ai",
        model: "photon",
        category: "image",
        operation: "generate",
        params: r#"{"prompt":"a cat"}"#,
        file: None,
        url: "https://api.lumalabs.ai/dream-machine/v1/generations/image",
    },
    Case {
        provider: "runway",
        base: "https://api.dev.runwayml.com",
        model: "gen4_turbo",
        category: "video",
        operation: "generate",
        params: r#"{"prompt":"a cat"}"#,
        file: None,
        url: "https://api.dev.runwayml.com/v1/text_to_video",
    },
    Case {
        provider: "kling",
        base: "https://api-singapore.klingai.com",
        model: "kling-v2",
        category: "video",
        operation: "generate",
        params: r#"{"prompt":"a cat"}"#,
        file: None,
        url: "https://api-singapore.klingai.com/v1/videos/text2video",
    },
    Case {
        provider: "hume-ai",
        base: "https://api.hume.ai",
        model: "aura",
        category: "speech",
        operation: "synthesize",
        params: r#"{"text":"hello"}"#,
        file: None,
        url: "https://api.hume.ai/v0/tts/file",
    },
    Case {
        provider: "mistral-ai",
        base: "https://api.mistral.ai/v1",
        model: "voxtral-mini-tts",
        category: "speech",
        operation: "synthesize",
        params: r#"{"text":"hello","voice":"test-voice"}"#,
        file: None,
        url: "https://api.mistral.ai/v1/audio/speech",
    },
    Case {
        provider: "mistral-ai",
        base: "https://api.mistral.ai/v1",
        model: "voxtral-mini",
        category: "speech",
        operation: "transcribe",
        params: r#"{}"#,
        file: Some("audio/mpeg"),
        url: "https://api.mistral.ai/v1/audio/transcriptions",
    },
    Case {
        provider: "rev-ai",
        base: "https://api.rev.ai/speechtotext/v1",
        model: "whisper",
        category: "speech",
        operation: "transcribe",
        params: r#"{}"#,
        file: Some("audio/mpeg"),
        url: "https://api.rev.ai/speechtotext/v1/jobs",
    },
];

/// The auth header and value one row sends. The package names it: Google and
/// Hume take a key header, fal and Deepgram take a scheme word, Replicate's
/// prediction endpoint takes `Token` where its model endpoint takes `Bearer`.
fn expected_credential(case: &Case) -> (&'static str, &'static str) {
    match case.provider {
        "google" => ("x-goog-api-key", CREDENTIAL),
        "elevenlabs" => ("xi-api-key", CREDENTIAL),
        "hume-ai" => ("x-hume-api-key", CREDENTIAL),
        "assemblyai" => ("authorization", CREDENTIAL),
        "fal" => ("authorization", "Key test-credential"),
        "deepgram" => ("authorization", "Token test-credential"),
        "replicate" if case.url == "https://api.replicate.com/v1/predictions" => {
            ("authorization", "Token test-credential")
        }
        _ => ("authorization", "Bearer test-credential"),
    }
}

/// Every (row, operation) pair sends its exact URL and credential header.
///
/// Every reply is a 401, so each pair stops after its first request and the
/// recorded request is the one `submit` (or `wait`) opened with.
#[tokio::test]
async fn every_row_operation_sends_its_url_and_credential() {
    let dir = work_dir("table");
    let mut failures = Vec::new();
    for case in CASES {
        let (adapter, requests) = scripted(|_| unauthorized());
        let files = case
            .file
            .map(|media_type| vec![input(&dir, "input.bin", media_type, MP3_HEADER)])
            .unwrap_or_default();
        let task = job(
            case.provider,
            case.model,
            case.category,
            case.operation,
            serde_json::from_str(case.params).unwrap(),
            files,
            &dir,
        );
        let outcome = run(&adapter, task, case.base, case.model).await;
        if outcome.is_ok() {
            failures.push(format!(
                "{}.{} answered a 401 with a result",
                case.provider, case.operation
            ));
            continue;
        }
        let recorded = requests.lock().unwrap();
        let Some(request) = recorded.first() else {
            failures.push(format!(
                "{}.{} sent no request: {}",
                case.provider,
                case.operation,
                outcome.unwrap_err().message
            ));
            continue;
        };
        let url = request.url.split('?').next().unwrap();
        if url != case.url {
            failures.push(format!(
                "{}.{}: expected {}, recorded {}",
                case.provider, case.operation, case.url, request.url
            ));
        }
        let (header, value) = expected_credential(case);
        let sent: Vec<&str> = request
            .headers
            .iter()
            .filter(|(_, sent)| sent.contains(CREDENTIAL))
            .map(|(name, _)| name.as_str())
            .collect();
        if sent != [header] {
            failures.push(format!(
                "{}.{}: expected one {header} header, recorded {sent:?}",
                case.provider, case.operation
            ));
        }
        if request.header(header) != Some(value) {
            failures.push(format!(
                "{}.{}: expected {header}: {value}, recorded {:?}",
                case.provider,
                case.operation,
                request.header(header)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// One video: `submit` stores the operation, `wait` polls until it settles and
/// streams the result to a file.
#[tokio::test]
async fn video_stores_the_operation_and_downloads_the_result() {
    let dir = work_dir("video");
    let video = b"fake-mp4-bytes-for-the-download".to_vec();
    let (url, downloaded) = video_server(video.clone()).await;
    let polls = Arc::new(Mutex::new(0usize));
    let scripted_polls = polls.clone();
    let reply_url = url.clone();
    let (adapter, requests) = scripted(move |request| {
        if request.method == Method::POST {
            return ok_json(json!({"request_id": "req_1"}));
        }
        let mut count = scripted_polls.lock().unwrap();
        *count += 1;
        if *count == 1 {
            Reply {
                status: StatusCode::ACCEPTED,
                body: br#"{"status":"pending","progress":0.5}"#.to_vec(),
            }
        } else {
            ok_json(json!({
                "status": "done",
                "video": {"url": reply_url, "duration": 5},
            }))
        }
    });

    let task = job(
        "xai",
        "grok-imagine-video",
        "video",
        "generate",
        json!({"prompt": "a cat"}),
        Vec::new(),
        &dir,
    );
    let task_id = task.task_id.clone();
    let submitted = adapter
        .submit(
            task,
            connection("https://api.x.ai/v1", "grok-imagine-video"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let handle = submitted.remote_handle.expect("video returns a handle");
    assert!(
        handle.contains("req_1"),
        "handle names the operation: {handle}"
    );
    assert!(handle.contains(&task_id), "handle keeps the job: {handle}");

    let output = adapter
        .wait(
            &task_id,
            Some(&handle),
            connection("https://api.x.ai/v1", "grok-imagine-video"),
            ProgressSink::none(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(output.files.len(), 1);
    assert_eq!(output.files[0].name, "video-1.mp4");
    assert_eq!(output.files[0].media_type, "video/mp4");
    assert_eq!(std::fs::read(&output.files[0].path).unwrap(), video);

    let requests = requests.lock().unwrap();
    let start: Vec<_> = requests
        .iter()
        .filter(|request| request.method == Method::POST)
        .collect();
    assert_eq!(start.len(), 1, "one start call");
    assert_eq!(start[0].url, "https://api.x.ai/v1/videos/generations");
    assert_eq!(*polls.lock().unwrap(), 2, "two status polls");

    // The download is a different origin: the credential must not travel.
    let downloaded = downloaded.lock().unwrap();
    assert_eq!(downloaded.len(), 1);
    for (name, value) in &downloaded[0] {
        assert!(
            !value.contains(CREDENTIAL),
            "download carried {name}: {value}"
        );
        assert!(
            !matches!(
                name.as_str(),
                "authorization" | "x-goog-api-key" | "xi-api-key" | "x-hume-api-key"
            ),
            "download carried {name}: {value}"
        );
    }
}

/// A status request that never answers fails at the job's hour bound, not at
/// the connection's much longer request timeout.
#[tokio::test]
async fn a_stalled_status_request_fails_at_the_job_hour() {
    let server = stalled_video_server().await;
    let adapter = MediaAdapter::new();
    let connect = || {
        connection_with_timeout(
            &format!("{}/v1", server.base),
            "grok-imagine-video",
            600_000,
        )
    };

    let dir = work_dir("video-hour");
    let task = job(
        "xai",
        "grok-imagine-video",
        "video",
        "generate",
        json!({"prompt": "a cat"}),
        Vec::new(),
        &dir,
    );
    let task_id = task.task_id.clone();
    let handle = adapter
        .submit(task, connect(), CancellationToken::new())
        .await
        .unwrap()
        .remote_handle
        .expect("video returns a handle");

    // The stored handle is two seconds short of its hour.
    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - MAX_VIDEO_WAIT.as_secs()
        + 2;
    let mut handle: Value = serde_json::from_str(&handle).unwrap();
    handle["started_at"] = json!(started_at);
    let handle = handle.to_string();

    let outcome = tokio::time::timeout(
        Duration::from_secs(15),
        adapter.wait(
            &task_id,
            Some(&handle),
            connect(),
            ProgressSink::none(),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the status request is bounded by the job's hour");
    let error = outcome.unwrap_err();
    assert_eq!(error.code, AdapterErrorCode::ProviderError);
    assert_eq!(
        error.message,
        "the provider did not finish the job within 1 hour"
    );
}

/// A provider whose start answers at once and whose status never does.
struct StalledVideo {
    base: String,
}

async fn stalled_video_server() -> StalledVideo {
    use axum::Router;
    use axum::routing::{get, post};
    let app = Router::new()
        .route(
            "/v1/videos/generations",
            post(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    r#"{"request_id":"req_1"}"#,
                )
            }),
        )
        .route(
            "/v1/videos/req_1",
            get(|| async { std::future::pending::<axum::response::Response>().await }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    StalledVideo {
        base: format!("http://{address}"),
    }
}

/// A credentialed download that meets a cross-origin redirect stops at it:
/// the provider's own header must not cross, and reqwest strips only
/// `Authorization`.
#[tokio::test]
async fn a_credentialed_download_stops_at_a_cross_origin_redirect() {
    use axum::Router;
    use axum::routing::get;

    // Origin B is the redirect's target: it must never see the connection.
    let seen_b = Arc::new(Mutex::new(Vec::new()));
    let recorder_b = seen_b.clone();
    let b_app = Router::new().route(
        "/collect",
        get(move |headers: axum::http::HeaderMap| {
            let recorder = recorder_b.clone();
            async move {
                recorder.lock().unwrap().push(header_pairs(&headers));
                "collected"
            }
        }),
    );
    let b_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let b_base = format!("http://{}", b_listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(b_listener, b_app).await.unwrap();
    });

    // Origin A serves the video URL; it redirects to B.
    let seen_a = Arc::new(Mutex::new(Vec::new()));
    let recorder_a = seen_a.clone();
    let target = format!("{b_base}/collect");
    let a_app = Router::new().route(
        "/download",
        get(move |headers: axum::http::HeaderMap| {
            let recorder = recorder_a.clone();
            let target = target.clone();
            async move {
                recorder.lock().unwrap().push(header_pairs(&headers));
                let mut response = axum::response::Response::new(axum::body::Body::empty());
                *response.status_mut() = StatusCode::FOUND;
                response
                    .headers_mut()
                    .insert(axum::http::header::LOCATION, target.parse().unwrap());
                response
            }
        }),
    );
    let a_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a_base = format!("http://{}", a_listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(a_listener, a_app).await.unwrap();
    });

    // The connection shares A's origin, so the download carries its headers;
    // a custom provider header is the one reqwest would let cross a redirect.
    let connect = || {
        let mut connection = connection(&format!("{a_base}/v1"), "grok-imagine-video");
        if let ModelConnection::Http(http) = &mut connection.connection {
            http.headers
                .insert("x-goog-api-key".to_owned(), CREDENTIAL.to_owned());
        }
        connection
    };
    let reply_url = format!("{a_base}/download");
    let (adapter, _requests) = scripted(move |request| {
        if request.method == Method::POST {
            return ok_json(json!({"request_id": "req_1"}));
        }
        ok_json(json!({
            "status": "done",
            "video": {"url": reply_url, "duration": 5},
        }))
    });

    let dir = work_dir("redirect");
    let task = job(
        "xai",
        "grok-imagine-video",
        "video",
        "generate",
        json!({"prompt": "a cat"}),
        Vec::new(),
        &dir,
    );
    let task_id = task.task_id.clone();
    let handle = adapter
        .submit(task, connect(), CancellationToken::new())
        .await
        .unwrap()
        .remote_handle
        .expect("video returns a handle");
    let outcome = adapter
        .wait(
            &task_id,
            Some(&handle),
            connect(),
            ProgressSink::none(),
            CancellationToken::new(),
        )
        .await;
    // The redirect target is the property under test: it must never be asked.
    let seen_b = seen_b.lock().unwrap();
    assert!(
        seen_b.is_empty(),
        "the redirect target was asked: {seen_b:?}"
    );
    drop(seen_b);
    let error = outcome.unwrap_err();
    assert_eq!(error.message, "The video download answered HTTP 302 Found");

    // A was asked with the connection's headers; the redirect never reached B.
    let seen_a = seen_a.lock().unwrap();
    assert_eq!(seen_a.len(), 1);
    assert!(
        seen_a[0]
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("x-goog-api-key")
                && value == CREDENTIAL),
        "the download carried the connection's header: {:?}",
        seen_a[0]
    );
}

/// The headers of one request as (name, value) pairs.
fn header_pairs(headers: &axum::http::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// A tiny HTTP server for one video, recording the headers it was asked with.
async fn video_server(body: Vec<u8>) -> (String, Arc<Mutex<Vec<Vec<(String, String)>>>>) {
    use axum::Router;
    use axum::routing::get;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();
    let app = Router::new().route(
        "/video.mp4",
        get(move |headers: axum::http::HeaderMap| {
            let recorder = recorder.clone();
            let body = body.clone();
            async move {
                recorder.lock().unwrap().push(
                    headers
                        .iter()
                        .map(|(name, value)| {
                            (
                                name.as_str().to_owned(),
                                value.to_str().unwrap_or_default().to_owned(),
                            )
                        })
                        .collect(),
                );
                ([(axum::http::header::CONTENT_TYPE, "video/mp4")], body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}/video.mp4"), seen)
}
