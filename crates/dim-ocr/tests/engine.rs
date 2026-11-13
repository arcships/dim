//! The Apple Vision engine end to end: a checked-in image, a mixed PDF whose
//! first page has its own text layer and whose second page does not, and a
//! scanned PDF whose two pages are images.
#![cfg(target_os = "macos")]
use dim_ocr::{Document, helper_main, helper_main_with_budget, read_pages};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The checked-in fixtures beside this test.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A fresh artifact directory.
fn artifacts() -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("dim-ocr-engine-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// Runs the helper in-process and returns its exit code and stdout.
fn run(input: &Path, artifacts: &Path, mode: &str) -> (i32, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = helper_main(
        [
            OsString::from("--input"),
            input.as_os_str().to_owned(),
            OsString::from("--artifacts"),
            artifacts.as_os_str().to_owned(),
            OsString::from("--output-mode"),
            OsString::from(mode),
        ],
        &mut stdout,
        &mut stderr,
    );
    (code, String::from_utf8(stdout).unwrap())
}

/// Runs the helper under a caller-supplied artifact bound.
fn run_with_limit(input: &Path, artifacts: &Path, mode: &str, limit: u64) -> (i32, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = helper_main_with_budget(
        [
            OsString::from("--input"),
            input.as_os_str().to_owned(),
            OsString::from("--artifacts"),
            artifacts.as_os_str().to_owned(),
            OsString::from("--output-mode"),
            OsString::from(mode),
        ],
        &mut stdout,
        &mut stderr,
        limit,
    );
    (code, String::from_utf8(stdout).unwrap())
}

/// The parsed result document of one call.
fn document(artifacts: &Path) -> Document {
    serde_json::from_slice(&std::fs::read(artifacts.join("result.json")).unwrap()).unwrap()
}

#[test]
#[ignore = "asks the Apple Vision and PDFium frameworks; runs with the ocr CI group"]
fn an_image_is_recognized_with_its_regions() {
    let artifacts = artifacts();
    let (code, stdout) = run(&fixture("latin.png"), &artifacts, "text");
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("\"pageCount\":1"));
    let document = document(&artifacts);
    assert_eq!(document.page_count, 1);
    let page = &document.pages[0];
    assert_eq!(page.page, 1);
    assert!(
        page.text.to_lowercase().contains("dimcode"),
        "{:?}",
        page.text
    );
    assert!(page.text.contains("12345678"), "{:?}", page.text);
    assert!(!page.regions.is_empty());
    for region in &page.regions {
        assert!(
            (0.0..=1.0).contains(&region.confidence),
            "confidence {} outside [0, 1]",
            region.confidence
        );
        for [x, y] in region.polygon {
            assert!(
                (0.0..=f64::from(page.width)).contains(&x)
                    && (0.0..=f64::from(page.height)).contains(&y),
                "point ({x}, {y}) outside {}x{}",
                page.width,
                page.height
            );
        }
    }
    assert!(page.image_path.is_none());
    let text = std::fs::read_to_string(artifacts.join("text.txt")).unwrap();
    assert_eq!(text, document.text);
    std::fs::remove_dir_all(artifacts).unwrap();
}

#[test]
#[ignore = "asks the Apple Vision and PDFium frameworks; runs with the ocr CI group"]
fn a_pdf_keeps_its_native_text_and_reads_the_page_without_one() {
    let artifacts = artifacts();
    let (code, stdout) = run(&fixture("mixed.pdf"), &artifacts, "text");
    assert_eq!(code, 0, "{stdout}");
    let document = document(&artifacts);
    assert_eq!(document.page_count, 2);
    assert!(document.pages[0].text.contains("Native page one"));
    assert!(
        document.pages[1].text.to_lowercase().contains("dimcode"),
        "{:?}",
        document.pages[1].text
    );
    // Text mode writes no page images.
    assert!(!artifacts.join("page-0001.png").exists());
    assert!(!artifacts.join("page-0002.png").exists());
    std::fs::remove_dir_all(artifacts).unwrap();
}

#[test]
#[ignore = "asks the Apple Vision and PDFium frameworks; runs with the ocr CI group"]
fn layout_writes_the_rasterized_pages() {
    let artifacts = artifacts();
    let (code, stdout) = run(&fixture("mixed.pdf"), &artifacts, "layout");
    assert_eq!(code, 0, "{stdout}");
    let document = document(&artifacts);
    assert_eq!(
        document.pages[0].image_path.as_deref(),
        Some("page-0001.png")
    );
    assert_eq!(
        document.pages[1].image_path.as_deref(),
        Some("page-0002.png")
    );
    assert!(artifacts.join("page-0001.png").is_file());
    assert!(artifacts.join("page-0002.png").is_file());
    let mut images: Vec<String> = std::fs::read_dir(&artifacts)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(dim_ocr::PAGE_IMAGE_PREFIX))
        .collect();
    images.sort();
    assert_eq!(images, ["page-0001.png", "page-0002.png"]);
    // The native text layer wins over the recognized one for page one.
    assert!(document.pages[0].text.contains("Native page one"));
    assert!(!document.pages[0].regions.is_empty());
    let pages = read_pages(
        std::fs::File::open(artifacts.join("result.json")).unwrap(),
        u64::MAX,
    )
    .unwrap();
    assert_eq!(pages.pages.len(), 2);
    std::fs::remove_dir_all(artifacts).unwrap();
}

#[test]
fn an_unsupported_input_is_a_typed_failure() {
    let directory = artifacts();
    let input = directory.join("input.txt");
    std::fs::write(&input, b"not an image").unwrap();
    let artifacts = artifacts();
    let (code, stdout) = run(&input, &artifacts, "text");
    assert_eq!(code, 2);
    assert!(stdout.contains("\"invalid_input\""), "{stdout}");
    assert!(!stdout.contains("\"local_runtime_failed\""), "{stdout}");
    std::fs::remove_dir_all(&artifacts).unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_layout_run_stops_at_the_page_image_that_would_cross_the_bound() {
    // The size of the first page image decides the bound the second run uses:
    // page one fits exactly, page two would cross it.
    let measured = artifacts();
    let (code, stdout) = run(&fixture("scanned.pdf"), &measured, "layout");
    assert_eq!(code, 0, "{stdout}");
    let first = std::fs::metadata(measured.join("page-0001.png"))
        .unwrap()
        .len();
    assert!(first > 0);
    assert!(measured.join("page-0002.png").is_file());

    let artifacts = artifacts();
    let (code, stdout) = run_with_limit(&fixture("scanned.pdf"), &artifacts, "layout", first);
    assert_eq!(code, 2, "{stdout}");
    assert!(stdout.contains("\"resource_limit_exceeded\""), "{stdout}");
    assert!(artifacts.join("page-0001.png").is_file());
    assert!(!artifacts.join("page-0002.png").exists());
    let total: u64 = std::fs::read_dir(&artifacts)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    assert!(
        total <= first,
        "{total} bytes written under a {first} bound"
    );

    std::fs::remove_dir_all(&artifacts).unwrap();
    std::fs::remove_dir_all(measured).unwrap();
}
