//! The helper's entry point: one input, one artifact directory, one JSON line.
//! Exit 0 is success, 2 a typed `{ok:false,error:{code,message}}`, 1 anything
//! else.
use crate::args::HelperArguments;
use crate::contract::{Document, HelperError, HelperErrorCode, MAX_ARTIFACT_BYTES};
#[cfg(target_os = "macos")]
use crate::contract::{MAX_INPUT_BYTES, OutputMode, RESULT_FILE, TEXT_FILE, page_image_name};
use serde::Serialize;
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;

/// What the helper prints on success.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SuccessPayload {
    ok: bool,
    page_count: u32,
}

/// What the helper prints for a typed failure.
#[derive(Serialize)]
struct FailurePayload {
    ok: bool,
    error: FailureError,
}

#[derive(Serialize)]
struct FailureError {
    code: HelperErrorCode,
    message: String,
}

/// Runs one helper call and returns its exit code.
pub fn helper_main<I, O, E>(arguments: I, stdout: &mut O, stderr: &mut E) -> i32
where
    I: IntoIterator<Item = OsString>,
    O: Write,
    E: Write,
{
    helper_main_with_budget(arguments, stdout, stderr, MAX_ARTIFACT_BYTES)
}

/// Runs one helper call under a caller-supplied artifact bound and returns its
/// exit code. The product passes [`MAX_ARTIFACT_BYTES`]; a test passes a small
/// value to watch the bound stop the writing.
pub fn helper_main_with_budget<I, O, E>(
    arguments: I,
    stdout: &mut O,
    stderr: &mut E,
    artifact_limit: u64,
) -> i32
where
    I: IntoIterator<Item = OsString>,
    O: Write,
    E: Write,
{
    let outcome =
        HelperArguments::parse(arguments).and_then(|arguments| execute(&arguments, artifact_limit));
    match outcome {
        Ok(document) => {
            let payload = SuccessPayload {
                ok: true,
                page_count: document.page_count,
            };
            match serde_json::to_writer(&mut *stdout, &payload) {
                Ok(()) => {
                    let _ = stdout.write_all(b"\n");
                    0
                }
                Err(error) => {
                    let _ = writeln!(stderr, "ocr-helper could not write its reply: {error}");
                    1
                }
            }
        }
        Err(error) => {
            let payload = FailurePayload {
                ok: false,
                error: FailureError {
                    code: error.code,
                    message: error.message.clone(),
                },
            };
            match serde_json::to_writer(&mut *stdout, &payload) {
                Ok(()) => {
                    let _ = stdout.write_all(b"\n");
                    2
                }
                Err(write_error) => {
                    let _ = writeln!(stderr, "ocr-helper failed: {error}; {write_error}");
                    1
                }
            }
        }
    }
}

/// Writes the helper's artifacts, charging each one against the bound before
/// the directory can hold more than it.
#[cfg(target_os = "macos")]
struct ArtifactWriter<'a> {
    directory: &'a Path,
    limit: u64,
    bytes: u64,
}

#[cfg(target_os = "macos")]
impl<'a> ArtifactWriter<'a> {
    fn new(directory: &'a Path, limit: u64) -> Self {
        Self {
            directory,
            limit,
            bytes: 0,
        }
    }

    /// Writes `bytes` as `name` when they fit; a write that would cross the
    /// bound writes nothing.
    fn write(&mut self, name: &str, bytes: &[u8]) -> Result<(), HelperError> {
        let next = self.bytes.saturating_add(bytes.len() as u64);
        if next > self.limit {
            return Err(HelperError::limit(format!(
                "The OCR artifacts exceed {} bytes",
                self.limit
            )));
        }
        let path = self.directory.join(name);
        std::fs::write(&path, bytes).map_err(|error| {
            HelperError::failed(format!("The OCR artifact cannot be written: {error}"))
        })?;
        self.bytes = next;
        Ok(())
    }

    /// Charges a file another writer produced; the file that crosses the bound
    /// is removed, so the directory never holds more than the bound.
    fn charge_written(&mut self, name: &str) -> Result<(), HelperError> {
        let path = self.directory.join(name);
        let length = std::fs::metadata(&path)
            .map_err(|error| {
                HelperError::failed(format!("The OCR artifact cannot be inspected: {error}"))
            })?
            .len();
        self.bytes = self.bytes.saturating_add(length);
        if self.bytes > self.limit {
            let _ = std::fs::remove_file(&path);
            return Err(HelperError::limit(format!(
                "The OCR artifacts exceed {} bytes",
                self.limit
            )));
        }
        Ok(())
    }
}

/// Validates the artifact directory, runs the engine under the artifact bound.
fn execute(arguments: &HelperArguments, artifact_limit: u64) -> Result<Document, HelperError> {
    prepare_artifacts(&arguments.artifacts)?;
    engine(arguments, artifact_limit)
}

/// The artifact directory must exist, be a directory and hold nothing yet.
fn prepare_artifacts(path: &Path) -> Result<(), HelperError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| HelperError::invalid_input("Artifact directory is missing"))?;
    if !metadata.is_dir() {
        return Err(HelperError::invalid_input(
            "Artifact path is not a directory",
        ));
    }
    let mut entries = std::fs::read_dir(path)
        .map_err(|_| HelperError::invalid_input("Artifact directory cannot be read"))?;
    if entries.next().is_some() {
        return Err(HelperError::invalid_input(
            "Artifact directory contains unexpected files",
        ));
    }
    Ok(())
}

/// The engine's input, sniffed from the bytes rather than the declared type.
#[cfg(target_os = "macos")]
enum Input {
    /// An image the Vision handler loads itself; its pixel size gates the
    /// pixel limit before recognition.
    Image {
        url: objc2::rc::Retained<objc2_foundation::NSURL>,
        width: u32,
        height: u32,
    },
    Pdf(crate::pdf::PdfDocument),
}

/// Runs the on-device engine over one input.
#[cfg(target_os = "macos")]
fn engine(arguments: &HelperArguments, artifact_limit: u64) -> Result<Document, HelperError> {
    let input = load_input(&arguments.input)?;
    let mut writer = ArtifactWriter::new(&arguments.artifacts, artifact_limit);
    let mut pages = Vec::new();
    match input {
        Input::Image { url, width, height } => {
            let regions = crate::vision::recognize_url(&url, width, height)?;
            let text = regions_text(&regions);
            pages.push(crate::contract::Page {
                page: 1,
                text,
                width,
                height,
                image_path: None,
                regions,
            });
        }
        Input::Pdf(document) => {
            let count = document.page_count();
            for index in 0..count {
                let page = document.page(index)?;
                let number = index + 1;
                let native = document.native_text(&page);
                let (width, height) = document.pixel_size(&page)?;
                if arguments.output_mode == OutputMode::Text && !native.is_empty() {
                    pages.push(crate::contract::Page {
                        page: number,
                        text: native,
                        width,
                        height,
                        image_path: None,
                        regions: Vec::new(),
                    });
                    continue;
                }
                let image = document.render(&page, width, height)?;
                let regions = crate::vision::recognize_image(&image)?;
                let text = if native.is_empty() {
                    regions_text(&regions)
                } else {
                    native
                };
                let image_path = if arguments.output_mode == OutputMode::Layout {
                    let name = page_image_name(number);
                    crate::vision::write_png(&image, &arguments.artifacts.join(&name))?;
                    writer.charge_written(&name)?;
                    Some(name)
                } else {
                    None
                };
                pages.push(crate::contract::Page {
                    page: number,
                    text,
                    width,
                    height,
                    image_path,
                    regions,
                });
            }
        }
    }
    let text = pages
        .iter()
        .map(|page| page.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let document = Document {
        text,
        page_count: pages.len() as u32,
        pages,
    };
    write_artifacts(&mut writer, &document)?;
    Ok(document)
}

/// The engine never runs on a host without Apple Vision.
#[cfg(not(target_os = "macos"))]
fn engine(_arguments: &HelperArguments, _artifact_limit: u64) -> Result<Document, HelperError> {
    Err(HelperError::unavailable(
        "Apple Vision text recognition requires macOS 12 or later on Apple Silicon",
    ))
}

/// Writes `result.json` and `text.txt` against the same artifact budget.
#[cfg(target_os = "macos")]
fn write_artifacts(
    writer: &mut ArtifactWriter<'_>,
    document: &Document,
) -> Result<(), HelperError> {
    let result = serde_json::to_vec(document).map_err(|error| {
        HelperError::failed(format!("The OCR result could not be encoded: {error}"))
    })?;
    writer.write(RESULT_FILE, &result)?;
    writer.write(TEXT_FILE, document.text.as_bytes())?;
    Ok(())
}

/// The text of a page: its regions' texts in reading order.
#[cfg(target_os = "macos")]
fn regions_text(regions: &[crate::contract::Region]) -> String {
    regions
        .iter()
        .map(|region| region.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Sniffs the input and opens it.
#[cfg(target_os = "macos")]
fn load_input(path: &Path) -> Result<Input, HelperError> {
    use std::io::Read;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| HelperError::invalid_input("The input file is missing"))?;
    if !metadata.is_file() {
        return Err(HelperError::invalid_input(
            "The input is not a regular file",
        ));
    }
    if metadata.len() == 0 {
        return Err(HelperError::invalid_input("The input file is empty"));
    }
    if metadata.len() > MAX_INPUT_BYTES {
        return Err(HelperError::limit(format!(
            "The input exceeds {MAX_INPUT_BYTES} bytes"
        )));
    }
    let mut prefix = Vec::with_capacity(1024);
    std::fs::File::open(path)
        .and_then(|file| file.take(1024).read_to_end(&mut prefix))
        .map_err(|_| HelperError::invalid_input("The input file cannot be read"))?;
    if prefix.windows(5).any(|window| window == b"%PDF-") {
        return Ok(Input::Pdf(crate::pdf::PdfDocument::open(path)?));
    }
    let (url, width, height) = crate::vision::image_source(path)?;
    Ok(Input::Image { url, width, height })
}
