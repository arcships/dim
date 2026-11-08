//! The helper contract: the artifact names, the limits, the document the
//! helper writes as `result.json`, and the typed error every exit 2 carries.
use serde::de::DeserializeSeed;
use serde::{Deserialize, Serialize};
use std::io::Read;

/// The helper's result document in the artifacts directory.
pub const RESULT_FILE: &str = "result.json";
/// The helper's whole-text artifact in the artifacts directory.
pub const TEXT_FILE: &str = "text.txt";
/// The prefix of a rasterized page's image, numbered from 1 with four digits.
pub const PAGE_IMAGE_PREFIX: &str = "page-";

/// The most input bytes one call reads.
pub const MAX_INPUT_BYTES: u64 = 100 * 1024 * 1024;
/// The most pages one PDF may have.
pub const MAX_PDF_PAGES: u32 = 500;
/// The most pixels one page may have, whichever way the page was produced.
pub const MAX_PAGE_PIXELS: u64 = 25_000_000;
/// The most bytes the artifacts of one call may occupy.
pub const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
/// The resolution a PDF page without usable native text is rendered at.
pub const RENDER_DPI: f64 = 200.0;

/// What the helper extracts: the text only, or the text with its regions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    /// The text only; a page that had to be rasterized keeps no image.
    #[default]
    Text,
    /// The text and each page's regions; a rasterized page keeps its image.
    Layout,
}
impl OutputMode {
    /// The mode named by `value`, or `None` for anything else.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "text" => Some(Self::Text),
            "layout" => Some(Self::Layout),
            _ => None,
        }
    }
    /// The mode as the command line spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Layout => "layout",
        }
    }
}

/// Why the helper could not produce a document (exit 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperErrorCode {
    /// The request, the input bytes or an artifact path violates the contract.
    InvalidInput,
    /// A PDF needs a password.
    EncryptedPdf,
    /// The input or the artifacts exceed a named limit.
    ResourceLimitExceeded,
    /// The platform cannot run the requested engine now.
    LocalRuntimeUnavailable,
    /// The engine failed or produced invalid output.
    LocalRuntimeFailed,
}
impl HelperErrorCode {
    /// The code as the contract spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::EncryptedPdf => "encrypted_pdf",
            Self::ResourceLimitExceeded => "resource_limit_exceeded",
            Self::LocalRuntimeUnavailable => "local_runtime_unavailable",
            Self::LocalRuntimeFailed => "local_runtime_failed",
        }
    }
}

/// A typed helper failure; its message is display text, never parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperError {
    pub code: HelperErrorCode,
    pub message: String,
}
impl HelperError {
    pub fn new(code: HelperErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(HelperErrorCode::InvalidInput, message)
    }
    pub fn encrypted_pdf() -> Self {
        Self::new(HelperErrorCode::EncryptedPdf, "The PDF requires a password")
    }
    pub fn limit(message: impl Into<String>) -> Self {
        Self::new(HelperErrorCode::ResourceLimitExceeded, message)
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(HelperErrorCode::LocalRuntimeUnavailable, message)
    }
    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(HelperErrorCode::LocalRuntimeFailed, message)
    }
}
impl std::fmt::Display for HelperError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}
impl std::error::Error for HelperError {}

/// One recognized text block: its text, its confidence and its box in page
/// pixels, clockwise from the top-left.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub text: String,
    pub confidence: f64,
    pub polygon: [[f64; 2]; 4],
}

/// One page of the document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    /// 1-based, the same numbering `data.pages[].page` uses.
    pub page: u32,
    pub text: String,
    pub width: u32,
    pub height: u32,
    /// The page image, only for a rasterized page in `layout` mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_path: Option<String>,
    pub regions: Vec<Region>,
}

/// The whole document, written as `result.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    pub text: String,
    pub page_count: u32,
    pub pages: Vec<Page>,
}

/// The pages as the task record's `data` needs them: one streaming parse of
/// `result.json`, the regions, geometry and the whole-document `text` member
/// never materialized.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentText {
    pub page_count: u32,
    /// Whether a page's text was dropped because the budget was spent.
    pub text_truncated: bool,
    pub pages: Vec<PageText>,
}

/// One page's text in `result.json`; a page beyond the text budget keeps only
/// its number.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PageText {
    pub page: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Reads the pages of `result.json` without holding the whole document: the
/// reader parses incrementally, the region members and the document `text`
/// member are skipped, and the page texts kept add up to at most `text_limit`
/// bytes. A page whose text would cross the budget keeps its number without a
/// text, and `text_truncated` says so. The caller opens the source, so a
/// daemon can hand over a reader that never followed a link.
pub fn read_pages<R: Read>(source: R, text_limit: u64) -> Result<DocumentText, HelperError> {
    let mut deserializer = serde_json::Deserializer::from_reader(std::io::BufReader::new(source));
    let document = DocumentSeed { text_limit }
        .deserialize(&mut deserializer)
        .map_err(|error| HelperError::failed(format!("The OCR result is invalid: {error}")))?;
    deserializer
        .end()
        .map_err(|error| HelperError::failed(format!("The OCR result is invalid: {error}")))?;
    Ok(document)
}

/// The seed carrying the text budget into the streaming parse.
struct DocumentSeed {
    text_limit: u64,
}

impl<'de> serde::de::DeserializeSeed<'de> for DocumentSeed {
    type Value = DocumentText;
    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(DocumentVisitor {
            text_limit: self.text_limit,
        })
    }
}

struct DocumentVisitor {
    text_limit: u64,
}

impl<'de> serde::de::Visitor<'de> for DocumentVisitor {
    type Value = DocumentText;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a result document")
    }

    fn visit_map<A>(self, mut map: A) -> Result<DocumentText, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let mut page_count = 0u32;
        let mut text_truncated = false;
        let mut pages = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "pageCount" => page_count = map.next_value::<u32>()?,
                "pages" => {
                    let seed = PagesSeed {
                        text_limit: self.text_limit,
                    };
                    let bounded = map.next_value_seed(seed)?;
                    pages = bounded.pages;
                    text_truncated = bounded.text_truncated;
                }
                // The document `text` member duplicates the page texts and the
                // record never uses it: skip it without decoding it.
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(DocumentText {
            page_count,
            text_truncated,
            pages,
        })
    }
}

struct BoundedPages {
    text_truncated: bool,
    pages: Vec<PageText>,
}

struct PagesSeed {
    text_limit: u64,
}

impl<'de> serde::de::DeserializeSeed<'de> for PagesSeed {
    type Value = BoundedPages;
    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(PagesVisitor {
            text_limit: self.text_limit,
            used: 0,
        })
    }
}

struct PagesVisitor {
    text_limit: u64,
    used: u64,
}

impl<'de> serde::de::Visitor<'de> for PagesVisitor {
    type Value = BoundedPages;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the pages of a result document")
    }

    fn visit_seq<A>(mut self, mut sequence: A) -> Result<BoundedPages, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut pages = Vec::new();
        let mut text_truncated = false;
        while let Some(page) = sequence.next_element_seed(PageSeed {
            text_limit: self.text_limit,
            used: &mut self.used,
            text_truncated: &mut text_truncated,
        })? {
            pages.push(page);
        }
        Ok(BoundedPages {
            text_truncated,
            pages,
        })
    }
}

struct PageSeed<'a> {
    text_limit: u64,
    used: &'a mut u64,
    text_truncated: &'a mut bool,
}

impl<'de> serde::de::DeserializeSeed<'de> for PageSeed<'_> {
    type Value = PageText;
    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(PageVisitor {
            text_limit: self.text_limit,
            used: self.used,
            text_truncated: self.text_truncated,
            page: None,
            text: None,
        })
    }
}

struct PageVisitor<'a> {
    text_limit: u64,
    used: &'a mut u64,
    text_truncated: &'a mut bool,
    page: Option<u32>,
    text: Option<String>,
}

impl<'de> serde::de::Visitor<'de> for PageVisitor<'_> {
    type Value = PageText;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("one page of a result document")
    }

    fn visit_map<A>(mut self, mut map: A) -> Result<PageText, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "page" => self.page = Some(map.next_value::<u32>()?),
                "text" if !*self.text_truncated => {
                    let text = map.next_value::<String>()?;
                    let next = self.used.saturating_add(text.len() as u64);
                    if next <= self.text_limit {
                        *self.used = next;
                        self.text = Some(text);
                    } else {
                        *self.text_truncated = true;
                    }
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        let page = self
            .page
            .ok_or_else(|| serde::de::Error::missing_field("page"))?;
        Ok(PageText {
            page,
            text: self.text,
        })
    }
}

/// The image artifact name of the 1-based page `page`.
pub fn page_image_name(page: u32) -> String {
    format!("{PAGE_IMAGE_PREFIX}{page:04}.png")
}
