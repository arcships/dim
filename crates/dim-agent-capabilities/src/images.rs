//! One scaling step for every place that turns a local image into an image
//! block. `read` is the caller here; the CLI's attachments will add the second
//! one (#657), so both send an image the same way.
use image::{
    AnimationDecoder, DynamicImage, GenericImageView, ImageDecoder, ImageFormat, ImageReader, Rgb,
    RgbImage, codecs::jpeg::JpegEncoder, imageops::FilterType, metadata::Orientation,
};
use std::io::{BufRead, Cursor, Read, Seek, SeekFrom};

/// The longest edge an image may have and still be sent as it is, in pixels:
/// within the 3,000 px the vision guide gives as safe for a many-image request
/// on every platform.
pub const MAX_EDGE: u32 = 2048;
/// The largest base64 encoding of an image sent as it is, in bytes: 5,000,000,
/// the lowest documented per-image limit; the guide does not define MB as 10^6
/// or 2^20, and this value holds under either reading.
/// Raw bytes `n` fit when `4 * ceil(n / 3) <= MAX_BASE64_BYTES`.
pub const MAX_BASE64_BYTES: u64 = 5_000_000;
/// The pixel guard: a header claiming more pixels is refused before decoding.
pub const MAX_PIXELS: u64 = 100_000_000;
/// The largest allocation one decoder may make, in bytes: [`MAX_PIXELS`] in the
/// largest pixel format this module decodes, 16-bit RGBA at eight bytes each.
pub const MAX_DECODE_BYTES: u64 = MAX_PIXELS * 8;
/// The quality of a JPEG this module encodes.
const JPEG_QUALITY: u8 = 85;
/// The leading bytes read to recognize the format.
const SNIFF_BYTES: usize = 16;

/// The image formats this module decodes and sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    WebP,
    Gif,
}

impl ImageKind {
    /// The name the model-facing text writes.
    pub fn name(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::WebP => "WebP",
            Self::Gif => "GIF",
        }
    }

    /// The media type of the file's own bytes.
    pub fn media_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::WebP => "image/webp",
            Self::Gif => "image/gif",
        }
    }

    fn format(self) -> ImageFormat {
        match self {
            Self::Png => ImageFormat::Png,
            Self::Jpeg => ImageFormat::Jpeg,
            Self::WebP => ImageFormat::WebP,
            Self::Gif => ImageFormat::Gif,
        }
    }
}

/// A width and height in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageSize {
    pub width: u32,
    pub height: u32,
}

impl ImageSize {
    pub fn pixels(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// What a file's first bytes say it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sniffed {
    /// An image this module decodes.
    Image(ImageKind),
    /// A known image format this build cannot decode.
    Unsupported(&'static str),
}

/// The image a file's first bytes say it is, or `None` when they are not an
/// image this module recognizes.
pub fn sniff(bytes: &[u8]) -> Option<Sniffed> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(Sniffed::Image(ImageKind::Png));
    }
    if bytes.starts_with(b"\xFF\xD8\xFF") {
        return Some(Sniffed::Image(ImageKind::Jpeg));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(Sniffed::Image(ImageKind::Gif));
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(Sniffed::Image(ImageKind::WebP));
    }
    // ISO base media: the major brand names the format. `image` knows AVIF but
    // this build carries no decoder for it, and it has no HEIC support at all.
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        return Some(match &bytes[8..12] {
            b"avif" | b"avis" => Sniffed::Unsupported("AVIF"),
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"hevm" | b"hevs" => {
                Sniffed::Unsupported("HEIC/HEIF")
            }
            // The generic HEIF brands: a file carrying one may hold HEVC or
            // AVIF, and the major brand alone does not say which.
            b"mif1" | b"msf1" => Sniffed::Unsupported("HEIF/AVIF"),
            _ => return None,
        });
    }
    None
}

/// An image made acceptable to a model, within the edge and byte limits and
/// upright.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaledImage {
    /// The bytes to send: the file's own bytes when nothing had to change.
    pub bytes: Vec<u8>,
    /// The media type of `bytes`.
    pub media_type: &'static str,
    /// The file's own format.
    pub kind: ImageKind,
    /// The pixel size of the source after its EXIF orientation, before any
    /// scaling: the header's size with a 90° or 270° turn's axes swapped.
    pub original: ImageSize,
    /// The pixel size of `bytes`: [`Self::original`] scaled down when it
    /// exceeded a limit.
    pub sent: ImageSize,
    /// Whether `bytes` were re-encoded — scaled to fit, or turned upright.
    /// When false they are the file's own bytes, unchanged.
    pub scaled: bool,
    /// Whether the source is an animation whose first frame `bytes` hold.
    pub animated: bool,
}

/// Why an image was not turned into a [`ScaledImage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageRefusal {
    /// The bytes are not an image in any format this module recognizes.
    NotAnImage,
    /// A known image format this build cannot decode.
    UnsupportedFormat(&'static str),
    /// The header claims more pixels than the guard allows, or decoding hit
    /// the guard's allocation bound; nothing was sent.
    TooManyPixels(ImageSize),
    /// The bytes are a recognized format this module cannot use; `reason` says
    /// why.
    Invalid { kind: ImageKind, reason: String },
}

/// Makes an image acceptable to a model.
///
/// In: a seekable byte source — a file, or a `Cursor` over bytes that are
/// already in memory. Out: the bytes to send, their media type, the size after
/// the source's EXIF orientation, the size actually sent, and whether the bytes
/// were re-encoded.
///
/// The whole source is never buffered: the first bytes are read to recognize
/// the format, the header is read from the same source and the pixel guard
/// runs, and only then are the pixels decoded. An image sent as it is is read
/// back in full only when its base64 encoding is within [`MAX_BASE64_BYTES`],
/// so memory stays within that bound and the pixel guard.
///
/// An image that fits both limits is returned unchanged. An image whose longest
/// edge exceeds [`MAX_EDGE`], whose base64 encoding exceeds
/// [`MAX_BASE64_BYTES`], whose EXIF orientation is not the identity, or which
/// is animated is decoded and re-encoded: a PNG source stays PNG while its
/// scaled PNG fits, and everything else becomes JPEG at quality 85, its long
/// edge stepping down until the encoding fits. The EXIF orientation is applied
/// before anything else, so the sent pixels are upright, and an animation is
/// sent as its first frame. A header above [`MAX_PIXELS`] is refused before the
/// pixels are decoded. Every decoder that supports one is given
/// [`MAX_DECODE_BYTES`] as its allocation limit, and an allocation refused there
/// is reported as the pixel-guard refusal; the WebP decoder takes no allocation
/// limit, so a metadata chunk larger than that bound, or than the file that
/// declares it, is refused before the decoder reads it.
pub fn scale_image<R: BufRead + Seek>(mut source: R) -> Result<ScaledImage, ImageRefusal> {
    let kind = match sniffed(&mut source)? {
        Some(Sniffed::Image(kind)) => kind,
        Some(Sniffed::Unsupported(name)) => return Err(ImageRefusal::UnsupportedFormat(name)),
        None => return Err(ImageRefusal::NotAnImage),
    };
    match kind {
        ImageKind::Gif => {
            rewind(&mut source, kind)?;
            let (stored, _) = read_header(&mut source, kind)?;
            rewind(&mut source, kind)?;
            let (image, animated) = {
                let mut decoder = image::codecs::gif::GifDecoder::new(&mut source)
                    .map_err(|error| invalid(kind, error))?;
                decoder
                    .set_limits(decode_limits())
                    .map_err(|error| decode_error(kind, stored, error))?;
                first_frame(kind, stored, decoder.into_frames())?
            };
            if let Some(image) = unchanged(
                &mut source,
                kind,
                stored,
                Orientation::NoTransforms,
                animated,
            )? {
                return Ok(image);
            }
            reencoded(kind, stored, Orientation::NoTransforms, image, animated)
        }
        ImageKind::WebP => {
            rewind(&mut source, kind)?;
            check_webp_metadata(&mut source, kind)?;
            rewind(&mut source, kind)?;
            let decoder = image::codecs::webp::WebPDecoder::new(&mut source)
                .map_err(|error| invalid(kind, error))?;
            let stored = size(decoder.dimensions());
            guard(stored)?;
            let mut decoder = decoder;
            // The EXIF chunk sits beside the frames: an animation is oriented
            // like a still, so read it before `into_frames` consumes the
            // decoder.
            let orientation = decoder
                .orientation()
                .map_err(|error| decode_error(kind, stored, error))?;
            if decoder.has_animation() {
                let (image, animated) = first_frame(kind, stored, decoder.into_frames())?;
                return reencoded(kind, stored, orientation, image, animated);
            }
            drop(decoder);
            if let Some(image) = unchanged(&mut source, kind, stored, orientation, false)? {
                return Ok(image);
            }
            rewind(&mut source, kind)?;
            let decoder = image::codecs::webp::WebPDecoder::new(&mut source)
                .map_err(|error| invalid(kind, error))?;
            let image = DynamicImage::from_decoder(decoder)
                .map_err(|error| decode_error(kind, stored, error))?;
            reencoded(kind, stored, orientation, image, false)
        }
        _ => {
            rewind(&mut source, kind)?;
            let (stored, orientation) = read_header(&mut source, kind)?;
            if let Some(image) = unchanged(&mut source, kind, stored, orientation, false)? {
                return Ok(image);
            }
            rewind(&mut source, kind)?;
            let mut reader = ImageReader::with_format(&mut source, kind.format());
            reader.limits(decode_limits());
            let decoder = reader
                .into_decoder()
                .map_err(|error| decode_error(kind, stored, error))?;
            let image = DynamicImage::from_decoder(decoder)
                .map_err(|error| decode_error(kind, stored, error))?;
            reencoded(kind, stored, orientation, image, false)
        }
    }
}

/// The allocation limit every decoder that takes one is given.
fn decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    limits
}

/// A decoder error, with an allocation refused for size reported as the
/// pixel-guard refusal: at [`MAX_PIXELS`] in the largest decoded format the
/// image is exactly what the guard is for, not a broken file.
fn decode_error(kind: ImageKind, stored: ImageSize, error: image::ImageError) -> ImageRefusal {
    if matches!(
        &error,
        image::ImageError::Limits(limit)
            if limit.kind() == image::error::LimitErrorKind::InsufficientMemory
    ) {
        return ImageRefusal::TooManyPixels(stored);
    }
    invalid(kind, error)
}

/// The leading bytes of the source, recognizing its format.
fn sniffed<R: BufRead + Seek>(source: &mut R) -> Result<Option<Sniffed>, ImageRefusal> {
    let mut prefix = [0u8; SNIFF_BYTES];
    let mut filled = 0;
    while filled < prefix.len() {
        match source.read(&mut prefix[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Ok(None),
        }
    }
    Ok(sniff(&prefix[..filled]))
}

/// The header's pixel size and orientation; the guard runs before any pixel
/// is decoded.
fn read_header<R: BufRead + Seek>(
    source: &mut R,
    kind: ImageKind,
) -> Result<(ImageSize, Orientation), ImageRefusal> {
    let (stored, orientation) = {
        let mut reader = ImageReader::with_format(&mut *source, kind.format());
        reader.limits(decode_limits());
        let mut decoder = reader
            .into_decoder()
            .map_err(|error| invalid(kind, error))?;
        let stored = size(decoder.dimensions());
        let orientation = decoder
            .orientation()
            .map_err(|error| decode_error(kind, stored, error))?;
        (stored, orientation)
    };
    guard(stored)?;
    Ok((stored, orientation))
}

/// Refuses a WebP whose EXIF chunk is larger than [`MAX_DECODE_BYTES`] or than
/// the file that declares it. `image`'s WebP decoder reads that chunk into a
/// vector of the size its header states and takes no allocation limit of its
/// own, so the declaration is checked before the decoder is built.
fn check_webp_metadata<R: BufRead + Seek>(
    source: &mut R,
    kind: ImageKind,
) -> Result<(), ImageRefusal> {
    let length = source
        .seek(SeekFrom::End(0))
        .map_err(|error| invalid(kind, error))?;
    let mut position = 12u64;
    loop {
        if position + 8 > length {
            return Ok(());
        }
        source
            .seek(SeekFrom::Start(position))
            .map_err(|error| invalid(kind, error))?;
        let mut chunk = [0u8; 8];
        source
            .read_exact(&mut chunk)
            .map_err(|error| invalid(kind, error))?;
        let size = u64::from(u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]));
        if &chunk[..4] == b"EXIF" && (size > MAX_DECODE_BYTES || position + 8 + size > length) {
            return Err(ImageRefusal::Invalid {
                kind,
                reason: "the EXIF metadata is larger than the memory bound or the file".into(),
            });
        }
        if position == 12 && &chunk[..4] != b"VP8X" {
            // A simple file: its one chunk is the image data, with no EXIF.
            return Ok(());
        }
        position += 8 + size + (size & 1);
    }
}

/// The first frame and whether a second one follows.
fn first_frame(
    kind: ImageKind,
    stored: ImageSize,
    mut frames: image::Frames<'_>,
) -> Result<(DynamicImage, bool), ImageRefusal> {
    let first = frames
        .next()
        .ok_or_else(|| ImageRefusal::Invalid {
            kind,
            reason: "the animation has no frames".into(),
        })?
        .map_err(|error| decode_error(kind, stored, error))?;
    let animated = frames.next().is_some();
    Ok((DynamicImage::ImageRgba8(first.into_buffer()), animated))
}

/// The source's own bytes when nothing has to change and its base64 encoding
/// is within [`MAX_BASE64_BYTES`]; `None` when the image has to be re-encoded
/// instead.
fn unchanged<R: BufRead + Seek>(
    source: &mut R,
    kind: ImageKind,
    stored: ImageSize,
    orientation: Orientation,
    animated: bool,
) -> Result<Option<ScaledImage>, ImageRefusal> {
    let length = source
        .seek(SeekFrom::End(0))
        .map_err(|error| invalid(kind, error))?;
    let fits = !animated
        && orientation == Orientation::NoTransforms
        && stored.width <= MAX_EDGE
        && stored.height <= MAX_EDGE
        && fits_base64(length);
    if !fits {
        return Ok(None);
    }
    rewind(source, kind)?;
    let mut bytes = Vec::new();
    (&mut *source)
        .take(length + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| invalid(kind, error))?;
    // The source grew while it was read: the header no longer describes the
    // bytes, so re-encode from what the decoder sees now.
    if bytes.len() as u64 > length {
        return Ok(None);
    }
    Ok(Some(ScaledImage {
        bytes,
        media_type: kind.media_type(),
        kind,
        original: stored,
        sent: stored,
        scaled: false,
        animated,
    }))
}

/// The decoded image re-encoded to fit both limits.
fn reencoded(
    kind: ImageKind,
    stored: ImageSize,
    orientation: Orientation,
    mut image: DynamicImage,
    animated: bool,
) -> Result<ScaledImage, ImageRefusal> {
    if orientation != Orientation::NoTransforms {
        image.apply_orientation(orientation);
    }
    let (bytes, media_type, sent) = encode(&image, kind)?;
    Ok(ScaledImage {
        bytes,
        media_type,
        kind,
        original: oriented(stored, orientation),
        sent,
        scaled: true,
        animated,
    })
}

/// The stored size after the EXIF orientation: a 90° or 270° turn swaps the
/// axes, so the size sent compares against the size the line names.
fn oriented(stored: ImageSize, orientation: Orientation) -> ImageSize {
    match orientation {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => ImageSize {
            width: stored.height,
            height: stored.width,
        },
        _ => stored,
    }
}

/// The length of `raw` bytes once base64-encoded, padding included.
pub fn base64_len(raw: u64) -> u64 {
    raw.div_ceil(3) * 4
}

/// Whether `raw` bytes of an image encode within [`MAX_BASE64_BYTES`], padding
/// included.
fn fits_base64(raw: u64) -> bool {
    base64_len(raw) <= MAX_BASE64_BYTES
}

/// Rewinds the source to its first byte.
fn rewind<R: Seek>(source: &mut R, kind: ImageKind) -> Result<(), ImageRefusal> {
    source
        .seek(SeekFrom::Start(0))
        .map(|_| ())
        .map_err(|error| invalid(kind, error))
}

/// The encoded image that fits both limits: PNG for a PNG source while its
/// scaled PNG fits, JPEG at quality 85 otherwise, the long edge stepping down
/// until the JPEG fits.
fn encode(
    image: &DynamicImage,
    kind: ImageKind,
) -> Result<(Vec<u8>, &'static str, ImageSize), ImageRefusal> {
    let mut fitted = fit(image);
    if kind == ImageKind::Png {
        let encoded = png(&fitted).map_err(|error| invalid(kind, error))?;
        if fits_base64(encoded.len() as u64) {
            return Ok((encoded, "image/png", size(fitted.dimensions())));
        }
    }
    loop {
        let encoded = jpeg(&fitted)?;
        if fits_base64(encoded.len() as u64) {
            return Ok((encoded, "image/jpeg", size(fitted.dimensions())));
        }
        let (width, height) = fitted.dimensions();
        let next = ((width * 7 / 8).max(1), (height * 7 / 8).max(1));
        if next == (width, height) {
            return Err(ImageRefusal::Invalid {
                kind,
                reason: "the image cannot be encoded within the byte limit".into(),
            });
        }
        fitted = fitted.resize_exact(next.0, next.1, FilterType::Lanczos3);
    }
}

/// The image fitted within [`MAX_EDGE`] on its longest edge, aspect kept.
fn fit(image: &DynamicImage) -> DynamicImage {
    if image.width() <= MAX_EDGE && image.height() <= MAX_EDGE {
        return image.clone();
    }
    image.resize(MAX_EDGE, MAX_EDGE, FilterType::Lanczos3)
}

fn png(image: &DynamicImage) -> image::ImageResult<Vec<u8>> {
    let mut encoded = Cursor::new(Vec::new());
    image.write_to(&mut encoded, ImageFormat::Png)?;
    Ok(encoded.into_inner())
}

fn jpeg(image: &DynamicImage) -> Result<Vec<u8>, ImageRefusal> {
    let mut encoded = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut encoded, JPEG_QUALITY);
    encoder
        .encode_image(&flattened(image))
        .map_err(|error| invalid(ImageKind::Jpeg, error))?;
    Ok(encoded)
}

/// The image with its transparency flattened onto white, for JPEG, which has
/// no alpha channel and would otherwise show a transparent pixel's bare colour
/// — black for a fully transparent pixel.
fn flattened(image: &DynamicImage) -> RgbImage {
    let rgba = image.to_rgba8();
    let mut rgb = RgbImage::new(rgba.width(), rgba.height());
    for (out, pixel) in rgb.pixels_mut().zip(rgba.pixels()) {
        let alpha = u32::from(pixel[3]);
        let mix = |channel: u8| ((u32::from(channel) * alpha + 255 * (255 - alpha)) / 255) as u8;
        *out = Rgb([mix(pixel[0]), mix(pixel[1]), mix(pixel[2])]);
    }
    rgb
}

fn guard(stored: ImageSize) -> Result<(), ImageRefusal> {
    if stored.pixels() > MAX_PIXELS {
        return Err(ImageRefusal::TooManyPixels(stored));
    }
    Ok(())
}

fn size((width, height): (u32, u32)) -> ImageSize {
    ImageSize { width, height }
}

fn invalid(kind: ImageKind, error: impl std::fmt::Display) -> ImageRefusal {
    ImageRefusal::Invalid {
        kind,
        reason: error.to_string(),
    }
}
