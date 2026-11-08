//! The Modality commands' portable tests: command-line parsing, the result
//! names the CLI accepts and the inline-text rule. They need no socket and
//! run on every platform; the tests that drive one are in `tests_scripted.rs`
//! and are Unix-only.

use super::{Command, ocr, task};
use crate::args::{Action, Options};
use std::path::{Path, PathBuf};

/// The arguments `dimcode ARGS...` in `home` are parsed from.
fn raw(home: &Path, args: &[&str]) -> Vec<String> {
    let mut arguments = vec![
        "--home".to_owned(),
        home.to_str().unwrap().to_owned(),
        "--cwd".to_owned(),
        home.to_str().unwrap().to_owned(),
    ];
    arguments.extend(args.iter().map(|argument| (*argument).to_owned()));
    arguments
}

/// The options `dimcode ARGS...` parses in `home`.
pub(super) fn parse(home: &Path, args: &[&str]) -> Options {
    Options::parse(raw(home, args).into_iter()).unwrap()
}

/// The parsed command lines: each shape reaches its command, and a word the
/// namespace does not have is a usage error.
#[test]
fn the_command_lines_parse_to_their_commands() {
    let home = tempfile::tempdir().unwrap();
    let command = |args: &[&str]| match parse(home.path(), args).action {
        Action::Modality(command) => Some(command),
        _ => None,
    };
    assert!(matches!(command(&["modality"]), Some(Command::List)));
    match command(&[
        "modality",
        "set-default",
        "image.edit",
        "openai/gpt-image-2",
    ]) {
        Some(Command::SetDefault {
            operation,
            model,
            clear,
        }) => {
            assert_eq!(operation, "image.edit");
            assert_eq!(model.as_deref(), Some("openai/gpt-image-2"));
            assert!(!clear);
        }
        _ => panic!("set-default did not parse"),
    }
    match command(&["modality", "set-default", "video", "--clear"]) {
        Some(Command::SetDefault {
            operation,
            model,
            clear,
        }) => {
            assert_eq!(operation, "video");
            assert!(model.is_none());
            assert!(clear);
        }
        _ => panic!("--clear did not parse"),
    }
    assert!(matches!(
        command(&["modality", "task", "list"]),
        Some(Command::TaskList)
    ));
    match command(&["modality", "task", "status", "task-1"]) {
        Some(Command::TaskStatus { task_id }) => assert_eq!(task_id, "task-1"),
        _ => panic!("task status did not parse"),
    }
    match command(&["modality", "task", "wait", "task-2", "--out", "/tmp"]) {
        Some(Command::TaskWait { task_id }) => assert_eq!(task_id, "task-2"),
        _ => panic!("task wait did not parse"),
    }
    match command(&["modality", "task", "cancel", "task-3"]) {
        Some(Command::TaskCancel { task_id }) => assert_eq!(task_id, "task-3"),
        _ => panic!("task cancel did not parse"),
    }
    match command(&["ocr", "scan.png", "--output-mode", "layout"]) {
        Some(Command::Ocr { file }) => assert_eq!(file, PathBuf::from("scan.png")),
        _ => panic!("ocr did not parse"),
    }
    for args in [
        vec!["ocr", "scan.png", "--no-wait"],
        vec!["ocr", "scan.png", "--out", "/tmp"],
        vec!["modality", "task", "list", "--limit", "5"],
    ] {
        assert!(
            Options::parse(raw(home.path(), &args).into_iter()).is_ok(),
            "{args:?}"
        );
    }
    for args in [
        vec!["ocr", "scan.png", "--output-mode", "html"],
        vec!["modality", "task", "list", "--output-mode", "text"],
    ] {
        assert!(
            Options::parse(raw(home.path(), &args).into_iter()).is_err(),
            "{args:?}"
        );
    }
    assert!(
        Options::parse(raw(home.path(), &["modality", "task", "list", "--limit", "x"]).into_iter())
            .is_err()
    );
    assert!(
        Options::parse(raw(home.path(), &["models", "openai", "--state", "running"]).into_iter())
            .is_err()
    );
    assert!(Options::parse(raw(home.path(), &["modality", "frobnicate"]).into_iter()).is_err());
    assert!(Options::parse(raw(home.path(), &["modality", "--json"]).into_iter()).is_ok());
    assert!(Options::parse(raw(home.path(), &["models", "openai", "--json"]).into_iter()).is_err());
    assert!(
        Options::parse(
            raw(
                home.path(),
                &["modality", "set-default", "image", "--clear", "x/y"]
            )
            .into_iter()
        )
        .is_err()
    );
}

/// A result name is exactly one normal path component: a directory name, a
/// path, and the dot entries never decide where a file lands (A.4).
#[test]
fn the_result_name_is_one_normal_component() {
    for name in ["image-1.png", "text.txt", "a b.txt"] {
        assert!(task::one_component(name), "{name:?}");
    }
    for name in [
        "",
        ".",
        "..",
        "/",
        "/etc/escape.png",
        "a/b.png",
        "./a.png",
        "../a.png",
    ] {
        assert!(!task::one_component(name), "{name:?}");
    }
}

/// The fallback where hard links are unsupported never replaces a file: a
/// target that already exists is refused with the same message the pre-write
/// check gives and keeps its bytes; a normal call writes the bytes, and a
/// copy that fails removes the target it created.
#[test]
fn the_copy_fallback_never_replaces_a_target() {
    let root = tempfile::tempdir().unwrap();
    let partial = root.path().join(".image-1.png.partial");
    std::fs::write(&partial, b"hello").unwrap();
    let target = root.path().join("image-1.png");

    std::fs::write(&target, b"mine").unwrap();
    let refusal = task::copy_in_place(&partial, &target, "image-1.png", "task-1").unwrap_err();
    let checked =
        task::check_targets(root.path(), &["image-1.png".to_string()], "task-1").unwrap_err();
    assert_eq!(refusal.to_string(), checked.to_string());
    assert_eq!(std::fs::read(&target).unwrap(), b"mine");

    std::fs::remove_file(&target).unwrap();
    task::copy_in_place(&partial, &target, "image-1.png", "task-1").unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"hello");

    let missing = root.path().join("missing.partial");
    let second = root.path().join("image-2.png");
    assert!(task::copy_in_place(&missing, &second, "image-2.png", "task-1").is_err());
    assert!(
        !second.exists(),
        "a failed copy removes the target it created"
    );
}

/// The inline rule holds both limits: at most 64 KiB of text and at most
/// 30,000 UTF-16 units (A.5).
#[test]
fn the_inline_text_rule_holds_both_limits() {
    assert_eq!(ocr::fitting(b"hello"), Some("hello".to_string()));
    let at_units = "a".repeat(30_000);
    assert_eq!(ocr::fitting(at_units.as_bytes()), Some(at_units));
    assert_eq!(ocr::fitting("a".repeat(30_001).as_bytes()), None);
    assert_eq!(ocr::fitting(&[b'a'; 64 * 1024 + 1]), None);
    // 15,000 astral characters are 30,000 UTF-16 units; one more is over the
    // unit limit while still within the byte limit.
    let astral = "\u{1F600}".repeat(15_000);
    assert_eq!(ocr::fitting(astral.as_bytes()), Some(astral));
    assert_eq!(ocr::fitting("\u{1F600}".repeat(15_001).as_bytes()), None);
}
