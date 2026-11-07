//! The helper contract without an engine: argument parsing, artifact names,
//! the result document and the typed failure line.
use dim_ocr::{
    Document, DocumentText, HelperArguments, HelperErrorCode, MAX_ARTIFACT_BYTES, MAX_INPUT_BYTES,
    MAX_PAGE_PIXELS, MAX_PDF_PAGES, OutputMode, Page, PageText, Region, helper_main,
    page_image_name, read_pages,
};
use std::ffi::OsString;

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn the_limits_are_named_constants_with_the_agreed_values() {
    assert_eq!(MAX_PDF_PAGES, 500);
    assert_eq!(MAX_PAGE_PIXELS, 25_000_000);
    assert_eq!(MAX_ARTIFACT_BYTES, 1024 * 1024 * 1024);
    assert_eq!(MAX_INPUT_BYTES, 100 * 1024 * 1024);
}

#[test]
fn arguments_default_to_text_and_accept_one_mode() {
    let parsed =
        HelperArguments::parse(args(&["--input", "/in.png", "--artifacts", "/out"])).unwrap();
    assert_eq!(parsed.input, std::path::PathBuf::from("/in.png"));
    assert_eq!(parsed.artifacts, std::path::PathBuf::from("/out"));
    assert_eq!(parsed.output_mode, OutputMode::Text);
    let layout = HelperArguments::parse(args(&[
        "--input",
        "/in.pdf",
        "--artifacts",
        "/out",
        "--output-mode",
        "layout",
    ]))
    .unwrap();
    assert_eq!(layout.output_mode, OutputMode::Layout);
}

#[test]
fn arguments_refuse_anything_outside_the_contract() {
    for (values, expected) in [
        (args(&["--bogus"]), "Unknown argument"),
        (
            args(&["--input", "/a", "--input", "/b", "--artifacts", "/o"]),
            "Duplicate argument",
        ),
        (
            args(&["--input", "rel.png", "--artifacts", "/o"]),
            "absolute",
        ),
        (args(&["--input", "/a", "--artifacts"]), "Missing value"),
        (
            args(&["--input", "/a", "--artifacts", "/o", "--output-mode", "pdf"]),
            "Output mode",
        ),
        (args(&[]), "Usage"),
    ] {
        let error = HelperArguments::parse(values).unwrap_err();
        assert_eq!(error.code, HelperErrorCode::InvalidInput);
        assert!(
            error.message.contains(expected),
            "{:?} does not mention {expected:?}",
            error.message
        );
    }
}

#[test]
fn page_images_are_flat_and_numbered_from_one() {
    assert_eq!(page_image_name(1), "page-0001.png");
    assert_eq!(page_image_name(42), "page-0042.png");
    assert_eq!(page_image_name(500), "page-0500.png");
}

#[test]
fn the_result_document_reads_back_as_pages() {
    let directory = std::env::temp_dir().join(format!(
        "dim-ocr-contract-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let document = Document {
        text: "one\n\ntwo".into(),
        page_count: 2,
        pages: vec![
            Page {
                page: 1,
                text: "one".into(),
                width: 10,
                height: 20,
                image_path: None,
                regions: vec![Region {
                    text: "one".into(),
                    confidence: 0.5,
                    polygon: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                }],
            },
            Page {
                page: 2,
                text: "two".into(),
                width: 10,
                height: 20,
                image_path: Some("page-0002.png".into()),
                regions: Vec::new(),
            },
        ],
    };
    let path = directory.join("result.json");
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    let read = read_pages(std::fs::File::open(&path).unwrap(), u64::MAX).unwrap();
    assert_eq!(
        read,
        DocumentText {
            page_count: 2,
            text_truncated: false,
            pages: vec![
                PageText {
                    page: 1,
                    text: Some("one".into())
                },
                PageText {
                    page: 2,
                    text: Some("two".into())
                },
            ],
        }
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn pages_past_the_text_budget_keep_only_their_number() {
    let directory = std::env::temp_dir().join(format!(
        "dim-ocr-contract-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("result.json");
    // The document `text` member duplicates the pages and is never decoded.
    let document = serde_json::json!({
        "text": "one\n\ntwo\n\nthree",
        "pageCount": 3,
        "pages": [
            {"page": 1, "text": "one", "width": 10, "height": 20, "regions": []},
            {"page": 2, "text": "two", "width": 10, "height": 20, "regions": []},
            {"page": 3, "text": "three", "width": 10, "height": 20, "regions": []},
        ],
    });
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    let read = read_pages(std::fs::File::open(&path).unwrap(), 6).unwrap();
    assert_eq!(read.page_count, 3);
    assert!(read.text_truncated);
    assert_eq!(
        read.pages,
        vec![
            PageText {
                page: 1,
                text: Some("one".into())
            },
            PageText {
                page: 2,
                text: Some("two".into())
            },
            PageText {
                page: 3,
                text: None
            },
        ]
    );
    let kept: usize = read
        .pages
        .iter()
        .filter_map(|page| page.text.as_ref())
        .map(String::len)
        .sum();
    assert!(kept as u64 <= 6, "{kept} bytes of text kept");
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_refused_call_prints_one_typed_line_and_exits_two() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = helper_main(
        vec![OsString::from("--input"), OsString::from("/missing.png")],
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(code, 2);
    let stdout = String::from_utf8(stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1);
    let payload: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "invalid_input");
}
