//! The Apple Vision engine: one recognition request per page, its text blocks
//! turned into the contract's regions. The frameworks are system libraries;
//! nothing is downloaded.
use crate::contract::{HelperError, Region};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, ClassType, msg_send};
use objc2_core_foundation::{CFString, CFURL, CGPoint};
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSArray, NSDictionary, NSString, NSURL};
use objc2_image_io::{CGImageDestination, CGImageSource};
use objc2_vision::{
    VNImageOption, VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizedText,
    VNRecognizedTextObservation, VNRequest, VNRequestTextRecognitionLevel,
};
use std::path::Path;

/// The Vision revision: 3 from macOS 13, 2 on 12.x.
fn revision() -> usize {
    match crate::platform::macos_version() {
        Some((major, _)) if major >= 13 => 3,
        _ => 2,
    }
}

/// The recognition languages, in the order the framework tries them.
fn languages() -> Retained<NSArray<NSString>> {
    NSArray::from_retained_slice(&[
        NSString::from_str("zh-Hans"),
        NSString::from_str("zh-Hant"),
        NSString::from_str("en-US"),
    ])
}

/// An empty options dictionary: no camera intrinsics, no CI context.
fn no_options() -> Retained<NSDictionary<VNImageOption, AnyObject>> {
    let keys: &[&NSString] = &[];
    let values: &[&AnyObject] = &[];
    NSDictionary::from_slices(keys, values)
}

/// The image source of `path`: the URL the handler reads and the pixel size
/// the contract reports.
pub fn image_source(path: &Path) -> Result<(Retained<NSURL>, u32, u32), HelperError> {
    let text = path
        .to_str()
        .ok_or_else(|| HelperError::invalid_input("The input path is not valid UTF-8"))?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(text));
    let cf_url: &CFURL = url.as_ref();
    // SAFETY: the URL names the input file; the source owns what ImageIO
    // returns for it.
    unsafe {
        let source = CGImageSource::with_url(cf_url, None).ok_or_else(|| {
            HelperError::invalid_input("The input is not a supported image or PDF")
        })?;
        let kind = source.r#type().map(|kind| kind.to_string());
        if !kind.as_deref().is_some_and(supported_image) {
            return Err(HelperError::invalid_input(
                "Unsupported or invalid input format",
            ));
        }
        let image = source
            .image_at_index(0, None)
            .ok_or_else(|| HelperError::invalid_input("The image could not be decoded"))?;
        let width = CGImage::width(Some(&image));
        let height = CGImage::height(Some(&image));
        if width == 0 || height == 0 {
            return Err(HelperError::invalid_input("The image has no pixels"));
        }
        if width as u64 * height as u64 > crate::contract::MAX_PAGE_PIXELS {
            return Err(HelperError::limit(
                "The image pixel count exceeds the limit",
            ));
        }
        Ok((url, width as u32, height as u32))
    }
}

/// The ImageIO types the helper accepts, matched on the sniffed content.
fn supported_image(kind: &str) -> bool {
    matches!(
        kind,
        "public.png"
            | "public.jpeg"
            | "public.heic"
            | "public.heif"
            | "public.tiff"
            | "public.webp"
            | "org.webmproject.webp"
    )
}

/// Recognizes an image the handler loads from `url`.
pub fn recognize_url(url: &NSURL, width: u32, height: u32) -> Result<Vec<Region>, HelperError> {
    let options = no_options();
    // SAFETY: the URL names a regular image file and the handler owns the
    // request it performs.
    unsafe {
        let handler = VNImageRequestHandler::initWithURL_options(
            VNImageRequestHandler::alloc(),
            url,
            &options,
        );
        recognize(&handler, width, height)
    }
}

/// Recognizes a rasterized page image.
pub fn recognize_image(image: &CGImage) -> Result<Vec<Region>, HelperError> {
    let options = no_options();
    let width = CGImage::width(Some(image)) as u32;
    let height = CGImage::height(Some(image)) as u32;
    // SAFETY: the image outlives the handler and its request.
    unsafe {
        let handler = VNImageRequestHandler::initWithCGImage_options(
            VNImageRequestHandler::alloc(),
            image,
            &options,
        );
        recognize(&handler, width, height)
    }
}

/// Performs one accurate text-recognition request and converts its results.
///
/// SAFETY: `handler` was created by one of the initializers above.
unsafe fn recognize(
    handler: &VNImageRequestHandler,
    width: u32,
    height: u32,
) -> Result<Vec<Region>, HelperError> {
    // SAFETY: `new` on a VNRecognizeTextRequest returns a fresh request; every
    // setter and getter below is the framework's own contract.
    unsafe {
        let request: Retained<VNRecognizeTextRequest> =
            msg_send![VNRecognizeTextRequest::class(), new];
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        request.setRecognitionLanguages(&languages());
        request.setUsesLanguageCorrection(true);
        request.setRevision(revision());
        let request_super: Retained<VNRequest> = Retained::cast_unchecked(request.clone());
        let requests = NSArray::from_retained_slice(&[request_super]);
        handler.performRequests_error(&requests).map_err(|error| {
            HelperError::failed(format!(
                "Apple Vision text recognition failed: {}",
                error.localizedDescription()
            ))
        })?;
        let Some(results) = request.results() else {
            return Ok(Vec::new());
        };
        let mut regions = Vec::new();
        for index in 0..results.count() {
            let observation: Retained<VNRecognizedTextObservation> = results.objectAtIndex(index);
            let candidates = observation.topCandidates(1);
            if candidates.count() == 0 {
                continue;
            }
            let candidate: Retained<VNRecognizedText> = candidates.objectAtIndex(0);
            let text = candidate.string().to_string().trim().to_string();
            if text.is_empty() {
                continue;
            }
            let confidence = (candidate.confidence() as f64).clamp(0.0, 1.0);
            let bounds = observation.boundingBox();
            let left = bounds.origin.x;
            let right = bounds.origin.x + bounds.size.width;
            let top = bounds.origin.y + bounds.size.height;
            let bottom = bounds.origin.y;
            let polygon = [
                pixel(CGPoint::new(left, top), width, height),
                pixel(CGPoint::new(right, top), width, height),
                pixel(CGPoint::new(right, bottom), width, height),
                pixel(CGPoint::new(left, bottom), width, height),
            ];
            regions.push(Region {
                text,
                confidence,
                polygon,
            });
        }
        regions.sort_by(reading_order);
        Ok(regions)
    }
}

/// A Vision corner in normalized, bottom-left coordinates as page pixels with
/// a top-left origin.
fn pixel(point: objc2_core_foundation::CGPoint, width: u32, height: u32) -> [f64; 2] {
    let x = (point.x * width as f64).clamp(0.0, width as f64);
    let y = ((1.0 - point.y) * height as f64).clamp(0.0, height as f64);
    [x, y]
}

/// Top to bottom, then left to right, with a 10-pixel row tolerance.
fn reading_order(left: &Region, right: &Region) -> std::cmp::Ordering {
    let top = |region: &Region| {
        region
            .polygon
            .iter()
            .map(|point| point[1])
            .fold(f64::INFINITY, f64::min)
    };
    let (left_y, right_y) = (top(left), top(right));
    if (left_y - right_y).abs() >= 10.0 {
        return left_y
            .partial_cmp(&right_y)
            .unwrap_or(std::cmp::Ordering::Equal);
    }
    left.polygon[0][0]
        .partial_cmp(&right.polygon[0][0])
        .unwrap_or(std::cmp::Ordering::Equal)
}

/// Writes `image` as a PNG at `path`.
pub fn write_png(image: &CGImage, path: &Path) -> Result<(), HelperError> {
    let text = path
        .to_str()
        .ok_or_else(|| HelperError::failed("The page image path is not valid UTF-8"))?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(text));
    let cf_url: &CFURL = url.as_ref();
    let kind = CFString::from_str("public.png");
    // SAFETY: the destination owns the file it writes; the image is a live
    // CGImage for the whole call.
    unsafe {
        let destination = CGImageDestination::with_url(cf_url, &kind, 1, None)
            .ok_or_else(|| HelperError::failed("The page image could not be created"))?;
        destination.add_image(image, None);
        if !destination.finalize() {
            return Err(HelperError::failed("The page image could not be written"));
        }
    }
    Ok(())
}
