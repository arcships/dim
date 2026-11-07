//! The PDF engine: PDFKit reads the document, its own text layer is preferred,
//! and a page without usable text is drawn into a bitmap for Vision. Every call
//! runs on the main thread of the helper process, which handles one call and
//! exits.
use crate::contract::{HelperError, MAX_PAGE_PIXELS, MAX_PDF_PAGES, RENDER_DPI};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImage};
use objc2_foundation::{NSString, NSURL};
use objc2_pdf_kit::{PDFDisplayBox, PDFDocument, PDFPage};
use std::path::Path;
use std::ptr::NonNull;

/// The points in an inch; `RENDER_DPI` is measured against it.
const POINTS_PER_INCH: f64 = 72.0;

/// The alpha component of the bitmap: premultiplied RGBA, 8 bits per channel.
const PREMULTIPLIED_RGBA: u32 = 1;

/// A PDF the helper can read pages from.
pub struct PdfDocument {
    document: Retained<PDFDocument>,
}

impl PdfDocument {
    /// Opens the PDF at `path`; a document that needs a password is refused.
    pub fn open(path: &Path) -> Result<Self, HelperError> {
        let text = path
            .to_str()
            .ok_or_else(|| HelperError::invalid_input("The PDF path is not valid UTF-8"))?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(text));
        // SAFETY: the URL names the input file; PDFKit owns the document it
        // returns.
        let document = unsafe { PDFDocument::initWithURL(PDFDocument::alloc(), &url) }
            .ok_or_else(|| HelperError::invalid_input("The input is not a readable PDF"))?;
        if unsafe { document.isLocked() } {
            return Err(HelperError::encrypted_pdf());
        }
        let count = unsafe { document.pageCount() };
        if count == 0 {
            return Err(HelperError::invalid_input("The PDF has no pages"));
        }
        if count > MAX_PDF_PAGES as usize {
            return Err(HelperError::limit(format!(
                "The PDF exceeds {MAX_PDF_PAGES} pages"
            )));
        }
        Ok(Self { document })
    }

    /// The number of pages the document has.
    pub fn page_count(&self) -> u32 {
        (unsafe { self.document.pageCount() }) as u32
    }

    /// The 0-based page at `index`.
    pub fn page(&self, index: u32) -> Result<Retained<PDFPage>, HelperError> {
        // SAFETY: the index is below `page_count`.
        unsafe { self.document.pageAtIndex(index as usize) }
            .ok_or_else(|| HelperError::failed("A PDF page could not be read"))
    }

    /// The page's own text layer, normalized; empty when it has none.
    pub fn native_text(&self, page: &PDFPage) -> String {
        // SAFETY: `page` belongs to a live document.
        unsafe { page.string() }
            .map(|string| normalize_text(&string.to_string()))
            .unwrap_or_default()
    }

    /// The pixel size the page is represented at: `RENDER_DPI`, shrunk
    /// uniformly when the page would exceed the pixel limit.
    pub fn pixel_size(&self, page: &PDFPage) -> Result<(u32, u32), HelperError> {
        let bounds = page_bounds(page);
        let (width, height) = (bounds.size.width, bounds.size.height);
        if width <= 0.0 || height <= 0.0 {
            return Err(HelperError::invalid_input("A PDF page has no area"));
        }
        let mut scale = RENDER_DPI / POINTS_PER_INCH;
        let pixels = (width * scale) * (height * scale);
        if pixels > MAX_PAGE_PIXELS as f64 {
            scale *= (MAX_PAGE_PIXELS as f64 / pixels).sqrt();
        }
        Ok((
            ((width * scale).round() as u32).max(1),
            ((height * scale).round() as u32).max(1),
        ))
    }

    /// Renders the page into a white bitmap of `width` × `height` pixels.
    pub fn render(
        &self,
        page: &PDFPage,
        width: u32,
        height: u32,
    ) -> Result<CFRetained<CGImage>, HelperError> {
        let bounds = page_bounds(page);
        if bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
            return Err(HelperError::invalid_input("A PDF page has no area"));
        }
        let context = bitmap_context(width, height)?;
        let scale_x = width as f64 / bounds.size.width;
        let scale_y = height as f64 / bounds.size.height;
        CGContext::save_g_state(Some(&context));
        CGContext::set_rgb_fill_color(Some(&context), 1.0, 1.0, 1.0, 1.0);
        CGContext::fill_rect(
            Some(&context),
            CGRect::new(
                CGPoint::new(0.0, 0.0),
                CGSize::new(width as f64, height as f64),
            ),
        );
        CGContext::scale_ctm(Some(&context), scale_x, scale_y);
        CGContext::translate_ctm(Some(&context), -bounds.origin.x, -bounds.origin.y);
        // SAFETY: the context is a live RGBA bitmap and PDFKit draws the page
        // into it synchronously.
        unsafe { page.drawWithBox_toContext(PDFDisplayBox::MediaBox, &context) };
        CGContext::restore_g_state(Some(&context));
        CGBitmapContextCreateImage(Some(&context))
            .ok_or_else(|| HelperError::failed("The PDF page could not be rendered"))
    }
}

/// The page's media box, the box the helper recognizes.
fn page_bounds(page: &PDFPage) -> CGRect {
    // SAFETY: `page` belongs to a live document.
    unsafe { page.boundsForBox(PDFDisplayBox::MediaBox) }
}

/// A premultiplied RGBA bitmap context of `width` × `height` pixels.
fn bitmap_context(width: u32, height: u32) -> Result<CFRetained<CGContext>, HelperError> {
    let space = CGColorSpace::new_device_rgb()
        .ok_or_else(|| HelperError::failed("The bitmap color space could not be created"))?;
    let bytes_per_row = width as usize * 4;
    // SAFETY: the buffer is allocated by Core Graphics, the geometry and
    // component sizes describe RGBA8, and the context owns what it allocates.
    unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            width as usize,
            height as usize,
            8,
            bytes_per_row,
            Some(&space),
            PREMULTIPLIED_RGBA,
        )
    }
    .map(|context| unsafe { CFRetained::from_raw(context) })
    .ok_or_else(|| HelperError::failed("The PDF page bitmap could not be created"))
}

unsafe extern "C-unwind" {
    /// `CGContextRef CGBitmapContextCreate(void *, size_t, size_t, size_t,
    /// size_t, CGColorSpaceRef, uint32_t)`. The binding crate does not expose
    /// it, so the helper declares the documented symbol itself.
    #[link_name = "CGBitmapContextCreate"]
    fn CGBitmapContextCreate(
        data: *mut std::ffi::c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: Option<&CGColorSpace>,
        bitmap_info: u32,
    ) -> Option<NonNull<CGContext>>;
}

/// Normalizes a text layer: `\r\n` and `\r` become `\n`, trailing blanks on
/// each line and the whole text are trimmed.
fn normalize_text(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let text = text
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    text.trim().to_string()
}
