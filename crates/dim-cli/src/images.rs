//! Local images at the CLI's boundary: every entry that makes an image item
//! scales it with the shared rule ([`dim_agent_capabilities::images`]) instead
//! of refusing it, so the draft holds exactly the bytes that will be sent.

use crate::Result;
use base64::{Engine, engine::general_purpose::STANDARD};
use dim_agent_capabilities::images::{ImageRefusal, ImageSize, MAX_PIXELS};
use dim_agent_types::{ContentBlock, ImageSource, ImageSourceType};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

/// An image made acceptable to a model: the block to send and the sizes its
/// marker names.
#[derive(Debug, Clone, PartialEq)]
pub struct Attached {
    /// The image block: the bytes that will be sent.
    pub source: ImageSource,
    /// The size the source reported before scaling.
    pub original: ImageSize,
    /// The size of the pixels `source` carries.
    pub sent: ImageSize,
    /// The media type of the source's own encoded form; `None` for a platform
    /// bitmap, which has no encoded source.
    pub source_media_type: Option<&'static str>,
    /// Whether the bytes were re-encoded — scaled to fit, or turned upright.
    pub scaled: bool,
    /// Whether the source is an animation whose first frame was sent.
    pub animated: bool,
}

impl Attached {
    fn new(
        image: dim_agent_capabilities::images::ScaledImage,
        source_media_type: Option<&'static str>,
    ) -> Self {
        Self {
            source: ImageSource {
                file_id: None,
                source_type: ImageSourceType::Base64,
                media_type: image.media_type.into(),
                data: Some(STANDARD.encode(&image.bytes)),
                url: None,
            },
            original: image.original,
            sent: image.sent,
            source_media_type,
            scaled: image.scaled,
            animated: image.animated,
        }
    }

    /// The content block this image is sent as.
    pub fn block(&self) -> ContentBlock {
        ContentBlock::Image {
            source: self.source.clone(),
        }
    }

    /// Both sizes, as a marker gives them: `8064×6048 → 2048×1536`.
    pub fn sizes(&self) -> String {
        format!(
            "{}×{} → {}×{}",
            self.original.width, self.original.height, self.sent.width, self.sent.height
        )
    }

    /// Both media types, as a marker gives them: `image/png → image/jpeg`.
    /// `None` when the sent bytes keep the source's own type.
    pub fn media_types(&self) -> Option<String> {
        let source = self.source_media_type?;
        (source != self.source.media_type).then(|| format!("{source} → {}", self.source.media_type))
    }

    /// Whether the sent pixels are a different size from the source's.
    /// `scaled` says the bytes were re-encoded; this says the size changed.
    pub fn resized(&self) -> bool {
        self.original != self.sent
    }

    /// Whether the sent form differs from the source's own form: the bytes
    /// were re-encoded and there was an encoded source to compare with. A
    /// platform bitmap has none — it has no file form to differ from.
    pub fn differs(&self) -> bool {
        self.scaled && self.source_media_type.is_some()
    }

    /// Whether the re-encode is the only change: the source's size and media
    /// type both stand, so no size, type or first-frame note names it.
    pub fn reencoded(&self) -> bool {
        self.differs() && !self.resized() && !self.animated && self.media_types().is_none()
    }
}

/// What a local file is to a draft: an image item, or the path sent as a
/// link.
pub enum Item {
    /// The path names an image: the scaled block and its sizes.
    Image(Attached),
    /// The path is submitted as its own text.
    Text(String),
}

/// The images one submission carries, in argument order, and the lines their
/// scaling adds to the run's diagnostics.
pub struct Loaded {
    pub images: Vec<Attached>,
    pub notes: Vec<String>,
}

/// The images given to a run or to the terminal interface's first message,
/// in argument order.
pub fn load(paths: &[PathBuf]) -> Result<Loaded> {
    let mut loaded = Loaded {
        images: Vec::new(),
        notes: Vec::new(),
    };
    if paths.len() > 20 {
        return Err("At most 20 images are allowed per submission".into());
    }
    for path in paths {
        let item =
            file(path).map_err(|error| format!("Cannot use image {}: {error}", path.display()))?;
        let Item::Image(image) = item else {
            return Err(format!(
                "Cannot use image {}: Expected PNG, JPEG, WebP or GIF image data",
                path.display()
            )
            .into());
        };
        if let Some(note) = scaled_note(path, &image) {
            loaded.notes.push(note);
        }
        loaded.images.push(image);
    }
    Ok(loaded)
}

/// The image a local path names, or its text when it is not meant as one.
pub fn file(path: &Path) -> Result<Item> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);
    let mut file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err("Expected a local ordinary file".into());
    }
    let mut bytes = Vec::new();
    (&mut file).take(32).read_to_end(&mut bytes)?;
    if !image_header(&bytes) && !image_extension(path) {
        return Ok(Item::Text(
            path.to_str().ok_or("File paths must be UTF-8")?.into(),
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    // The file itself is the source: the shared function reads the header,
    // runs the pixel guard and only then decodes, so it is never all held.
    scale(BufReader::new(file)).map(Item::Image)
}

/// An image from bytes already in memory: the clipboard or a terminal paste.
pub fn from_bytes(bytes: Vec<u8>) -> Result<Attached> {
    scale(std::io::Cursor::new(bytes))
}

/// An image from pixels a platform decoder produced: a clipboard bitmap.
#[cfg(any(test, target_os = "macos", target_os = "windows"))]
pub fn from_pixels(image: image::DynamicImage) -> Result<Attached> {
    let image = dim_agent_capabilities::images::scale_pixels(image).map_err(refusal)?;
    attach(image, None)
}

/// The shared pixel guard on a decoder's dimensions, run before the decoder
/// is asked for any pixel.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn guard((width, height): (u32, u32)) -> Result<()> {
    let size = ImageSize { width, height };
    if size.pixels() > MAX_PIXELS {
        return Err(refusal(ImageRefusal::TooManyPixels(size)));
    }
    Ok(())
}

/// Why an image was refused, as the CLI shows it.
pub fn refusal(error: ImageRefusal) -> crate::Error {
    match error {
        ImageRefusal::NotAnImage => "Expected PNG, JPEG, WebP or GIF image data".into(),
        ImageRefusal::UnsupportedFormat(name) => format!("{name} images are not supported").into(),
        ImageRefusal::TooManyPixels(size) => format!(
            "Image is {}×{} px; images above {} megapixels are refused",
            size.width,
            size.height,
            MAX_PIXELS / 1_000_000
        )
        .into(),
        ImageRefusal::Invalid { kind, reason } => {
            format!("Invalid or truncated {} image: {reason}", kind.name()).into()
        }
    }
}

/// The marker an image shows in the draft and in the transcript: its
/// position among the message's images, then what its sent form changed.
/// The same fixed text in every interface language, so the two ends of one
/// message read alike.
pub fn marker(index: usize, details: &str) -> String {
    format!("[Image #{index}{details}]")
}

/// Whether a local file is meant as an image: its first bytes or its
/// extension say so. Unreadable files are not.
pub fn is_image(path: &Path) -> bool {
    if image_extension(path) {
        return true;
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);
    let Ok(file) = options.open(path) else {
        return false;
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return false;
    }
    let mut bytes = Vec::new();
    file.take(32).read_to_end(&mut bytes).is_ok() && image_header(&bytes)
}

/// The line exec writes for an image whose sent form differs from its file:
/// the source and sent sizes when the size changed, the first-frame note of an
/// animation, and the sent encoding of a re-encode.
fn scaled_note(path: &Path, image: &Attached) -> Option<String> {
    let name = path.file_name().unwrap_or(path.as_os_str());
    if image.resized() {
        let mut note = format!(
            "Scaled {} from {}×{} to {}×{} px ({})",
            name.to_string_lossy(),
            image.original.width,
            image.original.height,
            image.sent.width,
            image.sent.height,
            image.source.media_type
        );
        if image.animated {
            note.push_str("; only the first frame is sent");
        }
        return Some(note);
    }
    if image.animated {
        return Some(format!(
            "Only the first frame of {} is sent ({})",
            name.to_string_lossy(),
            image.source.media_type
        ));
    }
    if image.differs() {
        return Some(format!(
            "Re-encoded {} as {} ({}×{} px)",
            name.to_string_lossy(),
            image.source.media_type,
            image.sent.width,
            image.sent.height
        ));
    }
    None
}

fn scale<R: BufRead + Seek>(source: R) -> Result<Attached> {
    let image = dim_agent_capabilities::images::scale_image(source).map_err(refusal)?;
    let source_media_type = image.kind.media_type();
    attach(image, Some(source_media_type))
}

/// The CLI's item for a scaled image, refusing an image whose header claims
/// a zero dimension: invalid data, not a limit. `source_media_type` is the
/// source's own encoded form, absent for a platform bitmap.
fn attach(
    image: dim_agent_capabilities::images::ScaledImage,
    source_media_type: Option<&'static str>,
) -> Result<Attached> {
    if image.original.width == 0 || image.original.height == 0 {
        return Err(refusal(ImageRefusal::Invalid {
            kind: image.kind,
            reason: "the image dimensions are zero".into(),
        }));
    }
    Ok(Attached::new(image, source_media_type))
}

fn image_header(bytes: &[u8]) -> bool {
    matches!(
        image::guess_format(bytes),
        Ok(image::ImageFormat::Png
            | image::ImageFormat::Jpeg
            | image::ImageFormat::WebP
            | image::ImageFormat::Gif)
    )
}

fn image_extension(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        image::ImageFormat::from_extension(extension).is_some()
            || extension
                .to_str()
                .is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "svg" | "heic" | "heif"))
    })
}

/// Imports the accepted draft image bytes before submitting their references.
pub async fn import(
    client: &mut crate::client::Client,
    session: &str,
    blocks: &mut [ContentBlock],
) -> crate::Result<()> {
    for block in blocks {
        let ContentBlock::Image { source } = block else {
            continue;
        };
        if source.source_type != ImageSourceType::Base64 {
            continue;
        }
        let bytes = STANDARD.decode(source.data.as_deref().ok_or("Image data is missing")?)?;
        let mut file = tempfile::NamedTempFile::new()?;
        std::io::Write::write_all(&mut file, &bytes)?;
        #[derive(serde::Deserialize)]
        struct Imported {
            image: ImageSource,
        }
        let imported: Imported = client
            .typed(
                "attachment.import",
                serde_json::json!({"sessionId":session,"path":file.path(),"image":true}),
            )
            .await?;
        *source = imported.image;
    }
    Ok(())
}
