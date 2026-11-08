//! `dimcode image|video|speech|music|audio|asset|motion`: one Modality
//! submission per command line (Modality A.1, A.5).
//!
//! The CLI types only the inputs A.1 declares — the prompt or text, the input
//! files, `image edit --mask` and `image enhance MODE`. Everything else an
//! operation takes is an adapter parameter, sent through a repeatable
//! `--param KEY=VALUE`, and the adapter's schema decides whether it is valid:
//! a typed flag per parameter would restate every adapter's schema.

use super::{Modality, task};
use crate::Result;
use crate::args::{ModalityFlags, Options};
use crate::client::Client;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// The largest inline input total; a larger submission names its files by
/// `node_path` instead (A.3).
pub(crate) const MAX_INLINE_BYTES: u64 = 8 * 1024 * 1024;

/// The input A.1 declares before the files.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Positional {
    /// `params.prompt`.
    Prompt,
    /// `params.text`, for `speech.synthesize`.
    Text,
    /// `params.mode`, for `image.enhance`.
    Mode,
}

impl Positional {
    fn key(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Text => "text",
            Self::Mode => "mode",
        }
    }
}

/// One operation as the CLI submits it (A.1).
#[derive(Debug)]
struct Shape {
    category: &'static str,
    operation: &'static str,
    positional: Option<Positional>,
    /// How many input files the operation needs at least.
    files: usize,
}

/// A.1's categories and operations, in the order the help lists them.
const SHAPES: &[Shape] = &[
    Shape {
        category: "image",
        operation: "generate",
        positional: Some(Positional::Prompt),
        files: 0,
    },
    Shape {
        category: "image",
        operation: "edit",
        positional: Some(Positional::Prompt),
        files: 0,
    },
    Shape {
        category: "image",
        operation: "enhance",
        positional: Some(Positional::Mode),
        files: 0,
    },
    Shape {
        category: "video",
        operation: "generate",
        positional: Some(Positional::Prompt),
        files: 0,
    },
    Shape {
        category: "video",
        operation: "edit",
        positional: Some(Positional::Prompt),
        files: 0,
    },
    Shape {
        category: "video",
        operation: "enhance",
        positional: Some(Positional::Prompt),
        files: 0,
    },
    Shape {
        category: "speech",
        operation: "synthesize",
        positional: Some(Positional::Text),
        files: 0,
    },
    Shape {
        category: "speech",
        operation: "transcribe",
        positional: None,
        files: 1,
    },
    Shape {
        category: "music",
        operation: "generate",
        positional: Some(Positional::Prompt),
        files: 0,
    },
    Shape {
        category: "audio",
        operation: "generate",
        positional: Some(Positional::Prompt),
        files: 0,
    },
    Shape {
        category: "asset",
        operation: "generate",
        positional: Some(Positional::Prompt),
        files: 0,
    },
    Shape {
        category: "motion",
        operation: "generate",
        positional: Some(Positional::Prompt),
        files: 0,
    },
];

/// One parsed submission.
#[derive(Clone, Debug)]
pub(crate) struct Submit {
    shape: &'static Shape,
    /// The prompt, text or enhance mode the command line named.
    positional: Option<String>,
    files: Vec<PathBuf>,
    mask: Option<PathBuf>,
    model: Option<String>,
    /// The `--param` values, already typed from their text (D4).
    params: Vec<(String, Value)>,
}

/// Parses `CATEGORY ARGS...` into a submission. `--json`, `--out` and
/// `--no-wait` are the CLI's own flags and are recorded where they belong.
pub(crate) fn parse(
    category: &str,
    args: &mut dyn Iterator<Item = String>,
    json: &mut bool,
    flags: &mut ModalityFlags,
) -> Result<Submit> {
    let operation = args.next().ok_or_else(|| usage(category))?;
    let Some(shape) = SHAPES
        .iter()
        .find(|shape| shape.category == category && shape.operation == operation)
    else {
        return Err(usage(category).into());
    };
    let mut submit = Submit {
        shape,
        positional: None,
        files: Vec::new(),
        mask: None,
        model: None,
        params: Vec::new(),
    };
    while let Some(word) = args.next() {
        let mut value = |name: &str| -> Result<String> {
            args.next()
                .ok_or_else(|| format!("{name} requires a value").into())
        };
        match word.as_str() {
            "--json" => *json = true,
            "--out" => flags.out = Some(PathBuf::from(value("--out")?)),
            "--no-wait" => flags.no_wait = true,
            "--model" => submit.model = Some(value("--model")?),
            "--mask" => submit.mask = Some(PathBuf::from(value("--mask")?)),
            "--param" => submit.params.push(param(&value("--param")?)?),
            word if word.starts_with("--") => {
                return Err(format!("Unknown argument: {word}").into());
            }
            word if submit.positional.is_none() && shape.positional.is_some() => {
                submit.positional = Some(word.to_owned());
            }
            word => submit.files.push(PathBuf::from(word)),
        }
    }
    if shape.positional.is_some() && submit.positional.is_none() {
        return Err(usage(category).into());
    }
    if submit.files.len() < shape.files {
        return Err(usage(category).into());
    }
    if submit.mask.is_some() && !(category == "image" && operation == "edit") {
        return Err("--mask applies to image edit".into());
    }
    // A typed input is not repeatable through `--param`: the key would be
    // written twice, and the daemon would have to guess which wins (D4).
    let typed = shape.positional.map(Positional::key).into_iter().chain(
        (submit.mask.is_some() || (category == "image" && operation == "edit")).then_some("mask"),
    );
    for key in typed {
        if submit.params.iter().any(|(name, _)| name == key) {
            return Err(format!("--param {key}= restates a value the command line types").into());
        }
    }
    Ok(submit)
}

/// The usage line of a category's commands.
fn usage(category: &str) -> String {
    let operations: Vec<String> = SHAPES
        .iter()
        .filter(|shape| shape.category == category)
        .map(|shape| match shape.positional {
            Some(Positional::Prompt) => format!("{} PROMPT [FILE...]", shape.operation),
            Some(Positional::Text) => format!("{} TEXT", shape.operation),
            Some(Positional::Mode) => format!("{} MODE [FILE...]", shape.operation),
            None => format!("{} FILE...", shape.operation),
        })
        .collect();
    format!("Use {category} {}", operations.join(" | "))
}

/// One `--param KEY=VALUE`: the value is JSON when it parses as JSON, a
/// string otherwise; a string that would parse as JSON is written quoted.
fn param(text: &str) -> Result<(String, Value)> {
    let Some((key, value)) = text.split_once('=') else {
        return Err(format!("--param takes KEY=VALUE, not {text}").into());
    };
    if key.is_empty() {
        return Err(format!("--param takes KEY=VALUE, not {text}").into());
    }
    let value = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_owned()));
    Ok((key.to_owned(), value))
}

impl Submit {
    /// The `modality.task.submit` params (A.3). Every input goes inline when
    /// the whole set fits 8 MiB, and by its canonical `node_path` otherwise.
    pub(crate) fn request(&self) -> Result<Value> {
        let mut params = Map::new();
        if let Some(positional) = &self.positional {
            let key = self
                .shape
                .positional
                .expect("a positional was parsed")
                .key();
            params.insert(key.to_owned(), Value::String(positional.clone()));
        }
        for (key, value) in &self.params {
            params.insert(key.clone(), value.clone());
        }
        let mut files: Vec<(String, &Path)> = Vec::new();
        for path in &self.files {
            files.push((file_name(path)?, path));
        }
        if let Some(mask) = &self.mask {
            // The mask is an input file, and its parameter names it (A.1).
            let name = file_name(mask)?;
            params.insert("mask".to_owned(), Value::String(name.clone()));
            files.push((name, mask));
        }
        let inline = total_bytes(&files)? <= MAX_INLINE_BYTES;
        let mut inputs = Vec::with_capacity(files.len());
        for (name, path) in files {
            inputs.push(input(name, path, inline)?);
        }
        let mut request = json!({
            "category": self.shape.category,
            "operation": self.shape.operation,
            "params": Value::Object(params),
            "files": inputs,
        });
        if let Some(model) = &self.model {
            let Some((provider_id, model_id)) = model.split_once('/') else {
                return Err(format!("{model} is not PROVIDER/MODEL").into());
            };
            if provider_id.is_empty() || model_id.is_empty() {
                return Err(format!("{model} is not PROVIDER/MODEL").into());
            }
            request["model"] = json!({"providerId": provider_id, "modelId": model_id});
        }
        Ok(request)
    }
}

/// Submits the operation, then waits and writes its results unless
/// `--no-wait` (A.5).
pub(crate) async fn run(
    client: &mut Client,
    submit: &Submit,
    options: &Options,
    output: &mut task::Output<'_>,
) -> Result<i32> {
    let request = submit.request()?;
    let mut modality = Modality::open(client)?;
    task::submit(&mut modality, request, &options.modality, output).await
}

/// The size of every input, as bytes on disk.
fn total_bytes(files: &[(String, &Path)]) -> Result<u64> {
    let mut total = 0;
    for (_, path) in files {
        let metadata =
            std::fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("{} is not a file", path.display()).into());
        }
        total += metadata.len();
    }
    Ok(total)
}

/// The name the submission gives `path`: its last component, which is what
/// an adapter sees among the job's files.
pub(crate) fn file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("{} has no file name", path.display()).into())
}

/// One input file, inline or by `node_path` (A.3).
pub(crate) fn input(name: String, path: &Path, inline: bool) -> Result<Value> {
    let media_type = media_type(path)?;
    if inline {
        let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(json!({
            "name": name,
            "media_type": media_type,
            "data_base64": STANDARD.encode(bytes),
        }))
    } else {
        let canonical =
            std::fs::canonicalize(path).map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(json!({
            "name": name,
            "media_type": media_type,
            "node_path": canonical,
        }))
    }
}

/// The media type of `path`, from its extension. The operation's accepted
/// prefixes stay the daemon's rule: an input it does not take is its
/// `invalid_input`, not a second table here.
fn media_type(path: &Path) -> Result<String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let media_type = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "ogg" => "audio/ogg",
        "opus" => "audio/opus",
        "flac" => "audio/flac",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "m4v" => "video/x-m4v",
        "pdf" => "application/pdf",
        _ => return Err(format!("Cannot tell the media type of {}", path.display()).into()),
    };
    Ok(media_type.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Submit> {
        let mut json = false;
        let mut flags = ModalityFlags::default();
        parse(
            "image",
            &mut args.iter().map(|arg| (*arg).to_owned()),
            &mut json,
            &mut flags,
        )
    }

    /// A `--param` value is JSON when it parses as JSON, and a string
    /// otherwise, including a string that would parse as JSON when quoted.
    #[test]
    fn parameters_take_the_type_their_text_has() {
        let submit = parsed(&[
            "generate",
            "a cat",
            "--param",
            "n=2",
            "--param",
            "size=1024x1024",
        ])
        .unwrap();
        assert_eq!(
            submit.params,
            [
                ("n".to_owned(), json!(2)),
                ("size".to_owned(), json!("1024x1024")),
            ]
        );
        let submit = parsed(&["generate", "a cat", "--param", "label=\"true\""]).unwrap();
        assert_eq!(submit.params, [("label".to_owned(), json!("true"))]);
        assert!(
            serde_json::from_str::<Value>("true").is_ok(),
            "the unquoted form types as a boolean"
        );
    }

    /// A typed input cannot be restated as a parameter.
    #[test]
    fn a_typed_input_cannot_be_restated_as_a_parameter() {
        assert!(parsed(&["generate", "a cat", "--param", "prompt=x"]).is_err());
        let mut json = false;
        let mut flags = ModalityFlags::default();
        assert!(
            parse(
                "image",
                &mut ["enhance", "upscale", "--param", "mode=restore"]
                    .iter()
                    .map(|arg| (*arg).to_owned()),
                &mut json,
                &mut flags,
            )
            .is_err()
        );
    }
}
