//! The bound side of one media call: a job's validated parameters become an
//! aimux call, and what aimux returns becomes files in the job's work
//! directory.
//!
//! Every option here is the one aimux declares for that model type; a
//! provider's own keys travel in `params.providerOptions`, which is the
//! namespaced map aimux already reads. Nothing reads or writes a credential.

use aimux_core::image_model::{
    ImageCallOptions, ImageFile, ImageFileData, ImageOutputs, ImageResult,
};
use aimux_core::shared::{AspectRatio, SharedProviderOptions, Size};
use aimux_core::speech_model::{AudioData, SpeechCallOptions, SpeechResult};
use aimux_core::transcription_model::{AudioInput, TranscriptionCallOptions, TranscriptionResult};
use aimux_core::video_model::{VideoCallOptions, VideoData, VideoFile, VideoFileData, VideoResult};
use dim_modality::{AdapterError, Job, JobFile};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

use crate::vendor::Vendor;

/// The `data` a transcription puts on its task result, and the file that
/// carries the same text (A.1).
pub fn transcription_data(result: &TranscriptionResult) -> Value {
    let segments: Vec<Value> = result
        .segments
        .iter()
        .map(|segment| {
            serde_json::json!({
                "start_ms": milliseconds(segment.start_second),
                "end_ms": milliseconds(segment.end_second),
                "text": segment.text,
            })
        })
        .collect();
    let mut data = Map::new();
    data.insert("text".into(), Value::String(result.text.clone()));
    data.insert("segments".into(), Value::Array(segments));
    if let Some(language) = &result.language {
        data.insert("language".into(), Value::String(language.clone()));
    }
    Value::Object(data)
}

/// Seconds as whole milliseconds, rounded: 1.2345 s is 1235 ms.
fn milliseconds(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}

/// One image call's options.
pub fn image_options(job: &Job, vendor: &Vendor) -> Result<ImageCallOptions, AdapterError> {
    let params = &job.params;
    let mut options = ImageCallOptions::new(text(params, "prompt").unwrap_or_default());
    if text(params, "prompt").is_none() {
        options.prompt = None;
    }
    options.n = integer(params, "n")?.unwrap_or(1) as u32;
    options.size = text(params, "size")
        .map(Size::parse)
        .transpose()
        .map_err(invalid_input)?;
    options.aspect_ratio = text(params, "aspectRatio")
        .map(AspectRatio::parse)
        .transpose()
        .map_err(invalid_input)?;
    options.seed = integer(params, "seed")?;
    options.provider_options = provider_options(params, vendor);
    if let (Some(key), Some(format)) = (vendor.format, text(params, "outputFormat")) {
        options
            .provider_options
            .entry(vendor.options.to_owned())
            .or_default()
            .insert(key.into(), Value::String(format.into()));
    }
    let mut files = Vec::new();
    for file in &job.files {
        files.push(image_file(file)?);
    }
    if !files.is_empty() {
        options.files = Some(files);
    }
    if let Some(mask) = text(params, "mask") {
        let file = job
            .files
            .iter()
            .find(|file| file.name == mask)
            .ok_or_else(|| {
                AdapterError::invalid_input(format!("`{mask}` is not one of the job's files"))
            })?;
        options.mask = Some(image_file(file)?);
    }
    Ok(options)
}

/// The requested `outputFormat`, for naming an output whose bytes name none.
pub fn requested_format(job: &Job) -> Option<&str> {
    text(&job.params, "outputFormat")
}

/// One video call's options.
pub fn video_options(job: &Job, vendor: &Vendor) -> Result<VideoCallOptions, AdapterError> {
    let params = &job.params;
    let mut options = VideoCallOptions::new(text(params, "prompt").unwrap_or_default());
    if text(params, "prompt").is_none() {
        options.prompt = None;
    }
    options.n = 1;
    options.aspect_ratio = text(params, "aspectRatio")
        .map(AspectRatio::parse)
        .transpose()
        .map_err(invalid_input)?;
    options.resolution = text(params, "resolution")
        .map(Size::parse)
        .transpose()
        .map_err(invalid_input)?;
    options.duration = integer(params, "durationSeconds")?.map(|value| value as u32);
    options.fps = integer(params, "fps")?.map(|value| value as u32);
    options.seed = integer(params, "seed")?;
    options.generate_audio = boolean(params, "generateAudio")?;
    options.provider_options = provider_options(params, vendor);
    if let Some(file) = job.files.first() {
        options.image = Some(VideoFile::File {
            media_type: file.media_type.clone(),
            data: VideoFileData::Binary(read(file.path.as_path())?),
        });
    }
    Ok(options)
}

/// One speech call's options.
pub fn speech_options(job: &Job, vendor: &Vendor) -> Result<SpeechCallOptions, AdapterError> {
    let params = &job.params;
    let text_body =
        text(params, "text").ok_or_else(|| AdapterError::invalid_input("`text` is required"))?;
    let mut options = SpeechCallOptions::new(text_body);
    options.voice = text(params, "voice").map(str::to_owned);
    options.output_format = text(params, "outputFormat").map(str::to_owned);
    options.instructions = text(params, "instructions").map(str::to_owned);
    options.speed = number(params, "speed")?;
    options.language = text(params, "language").map(str::to_owned);
    let mut provider = provider_options(params, vendor);
    if let Some(language) = &options.language {
        provider
            .entry(vendor.options.to_owned())
            .or_default()
            .insert("language".into(), Value::String(language.clone()));
    }
    if !provider.is_empty() {
        options.provider_options = Some(provider);
    }
    Ok(options)
}

/// One transcription call's options.
pub fn transcription_options(
    job: &Job,
    vendor: &Vendor,
) -> Result<TranscriptionCallOptions, AdapterError> {
    let params = &job.params;
    let file = job
        .files
        .first()
        .ok_or_else(|| AdapterError::invalid_input("`speech.transcribe` needs one audio file"))?;
    if job.files.len() > 1 {
        return Err(AdapterError::invalid_input(
            "`speech.transcribe` takes one audio file",
        ));
    }
    let mut options = TranscriptionCallOptions::new(
        AudioInput::Binary(read(file.path.as_path())?),
        file.media_type.clone(),
    );
    let mut provider = provider_options(params, vendor);
    for key in ["language", "prompt"] {
        if let Some(value) = text(params, key) {
            provider
                .entry(vendor.options.to_owned())
                .or_default()
                .insert(key.into(), Value::String(value.into()));
        }
    }
    if !provider.is_empty() {
        options.provider_options = Some(provider);
    }
    Ok(options)
}

/// The caller's provider options, under the package's own namespace.
///
/// `params.providerOptions` is aimux's namespaced map when it carries the
/// package's key, and otherwise the package's own options: a caller never has
/// to know that Black Forest Labs calls its namespace `blackForestLabs`.
fn provider_options(params: &Value, vendor: &Vendor) -> SharedProviderOptions {
    let mut options = SharedProviderOptions::new();
    let Some(object) = params.get("providerOptions").and_then(Value::as_object) else {
        return options;
    };
    match object.get(vendor.options).and_then(Value::as_object) {
        Some(namespaced) => {
            options.insert(vendor.options.to_owned(), namespaced.clone());
        }
        None if !object.is_empty() => {
            options.insert(vendor.options.to_owned(), object.clone());
        }
        None => {}
    }
    options
}

fn image_file(file: &dim_modality::JobFile) -> Result<ImageFile, AdapterError> {
    Ok(ImageFile::File {
        media_type: file.media_type.clone(),
        data: ImageFileData::Binary(read(file.path.as_path())?),
    })
}

fn read(path: &Path) -> Result<Vec<u8>, AdapterError> {
    std::fs::read(path).map_err(|error| {
        AdapterError::invalid_input(format!(
            "{} cannot be read: {error}",
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        ))
    })
}

fn text<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn integer(params: &Value, key: &str) -> Result<Option<u64>, AdapterError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            AdapterError::invalid_input(format!("`{key}` must be a non-negative integer"))
        }),
    }
}

fn number(params: &Value, key: &str) -> Result<Option<f64>, AdapterError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .map(Some)
            .ok_or_else(|| AdapterError::invalid_input(format!("`{key}` must be a number"))),
    }
}

fn boolean(params: &Value, key: &str) -> Result<Option<bool>, AdapterError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| AdapterError::invalid_input(format!("`{key}` must be a boolean"))),
    }
}

fn invalid_input(message: String) -> AdapterError {
    AdapterError::invalid_input(message)
}

/// The files one image call produced.
pub fn image_output(
    result: ImageResult,
    work_dir: &Path,
    requested: Option<&str>,
) -> Result<Vec<JobFile>, AdapterError> {
    let images: Vec<Vec<u8>> = match result.images {
        ImageOutputs::Base64(images) => images
            .into_iter()
            .map(|image| {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD
                    .decode(image)
                    .map_err(|error| {
                        AdapterError::provider_error(format!(
                            "The provider returned an image that is not base64: {error}"
                        ))
                    })
            })
            .collect::<Result<_, _>>()?,
        ImageOutputs::Binary(images) => images,
    };
    if images.is_empty() {
        return Err(AdapterError::provider_error(
            "The provider returned no images",
        ));
    }
    let mut files = Vec::new();
    for (index, bytes) in images.iter().enumerate() {
        let (extension, media_type) = image_kind(bytes, requested);
        let name = format!("image-{}.{extension}", index + 1);
        let path = write(work_dir, &name, bytes)?;
        files.push(JobFile {
            name,
            media_type: media_type.to_owned(),
            path,
        });
    }
    Ok(files)
}

/// The file one speech call produced.
pub fn speech_output(
    result: SpeechResult,
    work_dir: &Path,
    requested: Option<&str>,
) -> Result<Vec<JobFile>, AdapterError> {
    let bytes = match result.audio {
        AudioData::Base64(audio) => {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(audio)
                .map_err(|error| {
                    AdapterError::provider_error(format!(
                        "The provider returned audio that is not base64: {error}"
                    ))
                })?
        }
        AudioData::Binary(audio) => audio,
    };
    let (extension, media_type) = audio_kind(&bytes, requested);
    let name = format!("speech.{extension}");
    let path = write(work_dir, &name, &bytes)?;
    Ok(vec![JobFile {
        name,
        media_type: media_type.to_owned(),
        path,
    }])
}

/// The file one transcription produced: the text, as A.1's `transcript.txt`.
pub fn transcription_output(
    result: &TranscriptionResult,
    work_dir: &Path,
) -> Result<Vec<JobFile>, AdapterError> {
    let name = "transcript.txt".to_owned();
    let path = write(work_dir, &name, result.text.as_bytes())?;
    Ok(vec![JobFile {
        name,
        media_type: "text/plain".to_owned(),
        path,
    }])
}

/// One video a provider returned as a URL: the adapter downloads it itself so
/// a large video streams to disk.
pub struct VideoLink {
    pub url: String,
    pub media_type: String,
}

/// The files one video call produced, and the URLs the caller must download
/// itself.
pub fn video_output(
    result: &VideoResult,
    work_dir: &Path,
) -> Result<(Vec<JobFile>, Vec<VideoLink>), AdapterError> {
    let mut written = Vec::new();
    let mut links = Vec::new();
    for video in &result.videos {
        match video {
            VideoData::Url { url, media_type } => links.push(VideoLink {
                url: url.clone(),
                media_type: video_media_type(media_type),
            }),
            VideoData::Base64 { data, media_type } => {
                use base64::Engine as _;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|error| {
                        AdapterError::provider_error(format!(
                            "The provider returned a video that is not base64: {error}"
                        ))
                    })?;
                let index = written.len() + 1;
                written.push(video_file(work_dir, index, media_type, &bytes)?);
            }
            VideoData::Binary { data, media_type } => {
                let index = written.len() + 1;
                written.push(video_file(work_dir, index, media_type, data)?);
            }
        }
    }
    Ok((written, links))
}

fn video_file(
    work_dir: &Path,
    index: usize,
    media_type: &str,
    bytes: &[u8],
) -> Result<JobFile, AdapterError> {
    let media_type = video_media_type(media_type);
    let name = video_name(index, &media_type);
    let path = write(work_dir, &name, bytes)?;
    Ok(JobFile {
        name,
        media_type,
        path,
    })
}

/// The name one video is written under.
pub fn video_name(index: usize, media_type: &str) -> String {
    format!("video-{index}.{}", video_extension(media_type))
}

/// A video's media type: aimux names `video/mp4` when a provider reports none
/// or a generic type, and a record must not say `application/octet-stream`.
fn video_media_type(media_type: &str) -> String {
    match media_type {
        "" | "application/octet-stream" => "video/mp4".to_owned(),
        media_type => media_type.to_owned(),
    }
}

/// Writes `bytes` as `name` inside `work_dir`.
pub fn write(work_dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf, AdapterError> {
    let path = work_dir.join(name);
    std::fs::write(&path, bytes).map_err(|error| {
        AdapterError::provider_error(format!("The result file cannot be written: {error}"))
    })?;
    Ok(path)
}

/// The extension and media type of an image, from its bytes (A.2): a provider
/// that ignores the requested format is named by what it returned.
pub fn image_kind(bytes: &[u8], requested: Option<&str>) -> (&'static str, &'static str) {
    if let Ok(format) = image::guess_format(bytes) {
        let extension = format.extensions_str().first().copied().unwrap_or("bin");
        return (extension, format.to_mime_type());
    }
    requested_image_kind(requested)
}

fn requested_image_kind(requested: Option<&str>) -> (&'static str, &'static str) {
    match requested {
        Some("jpeg") => ("jpg", "image/jpeg"),
        Some("webp") => ("webp", "image/webp"),
        _ => ("png", "image/png"),
    }
}

/// The extension and media type of audio, from its signature.
///
/// Only the formats the parameters allow are covered; pcm has no signature, so
/// it is taken as requested.
pub fn audio_kind(bytes: &[u8], requested: Option<&str>) -> (&'static str, &'static str) {
    if bytes.starts_with(b"OggS") {
        return ("ogg", "audio/ogg");
    }
    if bytes.starts_with(b"fLaC") {
        return ("flac", "audio/flac");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        return ("wav", "audio/wav");
    }
    // ADTS AAC: a sync word of `0xFFF` with the two bits that name the layer
    // clear, before MPEG audio's own sync check.
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] & 0xF6 == 0xF0 {
        return ("aac", "audio/aac");
    }
    if bytes.starts_with(b"ID3")
        || (bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0)
    {
        return ("mp3", "audio/mpeg");
    }
    match requested {
        Some("opus") => ("opus", "audio/opus"),
        Some("aac") => ("aac", "audio/aac"),
        Some("flac") => ("flac", "audio/flac"),
        Some("wav") => ("wav", "audio/wav"),
        Some("pcm") => ("pcm", "audio/L16"),
        _ => ("mp3", "audio/mpeg"),
    }
}

/// The extension and media type of a video, from the media type its provider
/// reported.
fn video_extension(media_type: &str) -> &'static str {
    match media_type {
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        _ => "mp4",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimux_core::transcription_model::TranscriptionSegment;

    /// Seconds become whole milliseconds, rounded: A10's transcription shape.
    #[test]
    fn transcription_segments_round_to_milliseconds() {
        assert_eq!(milliseconds(1.2345), 1235);
        assert_eq!(milliseconds(2.0), 2000);
        assert_eq!(milliseconds(0.0), 0);
    }

    #[test]
    fn transcription_data_names_text_segments_and_language() {
        let result = TranscriptionResult {
            text: "hello there".into(),
            segments: vec![TranscriptionSegment {
                text: "hello".into(),
                start_second: 1.2345,
                end_second: 2.5,
            }],
            language: Some("en".into()),
            duration_in_seconds: Some(2.5),
            warnings: Vec::new(),
            request: None,
            response: aimux_core::transcription_model::TranscriptionResponse::default(),
            provider_metadata: None,
        };
        let data = transcription_data(&result);
        assert_eq!(data["text"], "hello there");
        assert_eq!(data["language"], "en");
        assert_eq!(
            data["segments"],
            serde_json::json!([{"start_ms": 1235, "end_ms": 2500, "text": "hello"}])
        );
    }
}
