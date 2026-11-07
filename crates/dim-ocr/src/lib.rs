//! The OCR helper: the contract the daemon and the engine share, the argument
//! parser, the platform facts and the Apple Vision engine (Modality A.1/A.2).
//!
//! The daemon starts its own executable with the hidden `ocr-helper`
//! subcommand; the helper reads one input file, writes its artifacts into a
//! directory the caller passes, prints one JSON line and exits 0 (success), 2
//! (typed error) or 1 (anything else). Nothing here talks to a daemon, a
//! network or a Provider.
pub mod args;
pub mod contract;
pub mod helper;
pub mod platform;

#[cfg(target_os = "macos")]
mod pdf;
#[cfg(target_os = "macos")]
mod vision;

pub use args::HelperArguments;
pub use contract::{
    Document, DocumentText, HelperError, HelperErrorCode, MAX_ARTIFACT_BYTES, MAX_INPUT_BYTES,
    MAX_PAGE_PIXELS, MAX_PDF_PAGES, OutputMode, PAGE_IMAGE_PREFIX, Page, PageText, RESULT_FILE,
    Region, TEXT_FILE, page_image_name, read_pages,
};
pub use helper::{helper_main, helper_main_with_budget};
pub use platform::{
    APPLE_VISION_MODEL, APPLE_VISION_PROVIDER, apple_vision_available, macos_version,
};
