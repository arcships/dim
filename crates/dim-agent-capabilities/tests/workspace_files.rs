//! File tool flows: text preservation, branch-scoped read bindings, write
//! serialization, and glob/grep parity with the TS grep-glob acceptance test.

use dim_agent_capabilities::{
    host::HostError,
    workspace_tools::{file_reads::FileReads, files::FileTools},
};
use dim_agent_runtime::{MemoryTimelineStore, Timeline, TimelineKey};
use dim_agent_types::{ToolCall, ToolContent, ToolResult};
use rutis::host::CtxHost;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

/// One branch with its own empty Timeline.
fn branch() -> FileReads {
    FileReads::new(Arc::new(Timeline::new(
        rutis::Ctx::root().unwrap(),
        TimelineKey {
            session_id: "workspace".into(),
            branch_id: "main".into(),
        },
        Vec::new(),
        Arc::new(MemoryTimelineStore::default()),
    )))
}

struct Workspace {
    root: PathBuf,
    tools: FileTools,
}
impl Workspace {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .join(format!("dim-workspace-files-{}", uuid::Uuid::new_v4()))
            .join("work");
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let home = root.parent().unwrap().to_owned();
        Self {
            tools: FileTools::new(root.clone(), home.clone(), home.join("outputs")),
            root,
        }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    fn put(&self, name: &str, bytes: &[u8]) {
        let path = self.path(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn bytes(&self, name: &str) -> Vec<u8> {
        fs::read(self.path(name)).unwrap()
    }
    async fn call(&self, reads: &FileReads, name: &str, input: Value) -> ToolResult {
        self.try_call(reads, name, input, CancellationToken::new())
            .await
            .unwrap()
    }
    async fn try_call(
        &self,
        reads: &FileReads,
        name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<ToolResult, HostError> {
        let call = ToolCall {
            id: "call".into(),
            name: name.into(),
            input: input.as_object().unwrap().clone(),
        };
        self.tools.execute(&call, reads, cancel).await
    }
    async fn read(&self, reads: &FileReads, name: &str) -> ToolResult {
        self.call(reads, "read", json!({ "path": name })).await
    }
    async fn edit(&self, reads: &FileReads, name: &str, old: &str, new: &str) -> ToolResult {
        self.call(
            reads,
            "edit",
            json!({ "path": name, "oldText": old, "newText": new }),
        )
        .await
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.root.parent().unwrap());
    }
}

fn text(result: &ToolResult) -> &str {
    match &result.content {
        ToolContent::Text(text) => text,
        other => panic!("unexpected content {other:?}"),
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn atomic_mutations_preserve_native_metadata() {
    use std::{os::unix::fs::MetadataExt, process::Command};
    for tool in ["write", "edit"] {
        let workspace = Workspace::new();
        workspace.put("file", b"before\n");
        let path = workspace.path("file");
        assert!(
            Command::new("xattr")
                .args(["-w", "com.dim.regression", "private"])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        fs::write(path.join("..namedfork/rsrc"), b"resource fork").unwrap();
        let user =
            String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout).unwrap();
        assert!(
            Command::new("chmod")
                .arg("+a")
                .arg(format!(
                    "{} allow read,write,readattr,writeattr,readextattr,writeextattr,readsecurity",
                    user.trim()
                ))
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let acl = Command::new("ls")
            .arg("-le")
            .arg(&path)
            .output()
            .unwrap()
            .stdout;
        let acl = String::from_utf8(acl)
            .unwrap()
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n");
        let before = fs::metadata(&path).unwrap();
        let reads = branch();
        workspace.read(&reads, "file").await;
        let result = workspace.call(&reads, tool, json!({"path": "file", "content": "after\n", "oldText": "before", "newText": "after"})).await;
        assert_eq!(result.is_error, None, "{}", text(&result));
        assert_eq!(workspace.bytes("file"), b"after\n");
        assert_eq!(
            fs::read(path.join("..namedfork/rsrc")).unwrap(),
            b"resource fork"
        );
        assert_eq!(
            Command::new("xattr")
                .args(["-p", "com.dim.regression"])
                .arg(&path)
                .output()
                .unwrap()
                .stdout,
            b"private\n"
        );
        let after_acl = Command::new("ls")
            .arg("-le")
            .arg(&path)
            .output()
            .unwrap()
            .stdout;
        assert_eq!(
            String::from_utf8(after_acl)
                .unwrap()
                .lines()
                .skip(1)
                .collect::<Vec<_>>()
                .join("\n"),
            acl
        );
        let after = fs::metadata(&path).unwrap();
        assert_eq!(
            (after.uid(), after.gid(), after.mode()),
            (before.uid(), before.gid(), before.mode())
        );
    }
}
fn structured(result: &ToolResult) -> &serde_json::Map<String, Value> {
    result.structured_content.as_ref().unwrap()
}
fn code(result: &ToolResult) -> &str {
    assert_eq!(result.is_error, Some(true), "{}", text(result));
    structured(result)["code"].as_str().unwrap()
}
fn ok(result: &ToolResult) -> &str {
    assert_eq!(result.is_error, None, "{}", text(result));
    text(result)
}
fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

#[cfg(unix)]
#[tokio::test]
async fn coordinated_sessions_reject_stale_mutations() {
    use dim_agent_capabilities::workspace_tools::files::FileWriteCoordinator;
    use std::sync::Arc;
    let mut workspace = Workspace::new();
    let coordinator = Arc::new(FileWriteCoordinator::default());
    workspace.tools = FileTools::with_coordinator(
        workspace.root.clone(),
        workspace.root.clone(),
        workspace.root.join("outputs"),
        coordinator.clone(),
    );
    let other = FileTools::with_coordinator(
        workspace.root.clone(),
        workspace.root.clone(),
        workspace.root.join("outputs"),
        coordinator,
    );
    workspace.put("shared", b"before\r\n");
    let (a, b) = (branch(), branch());
    workspace.read(&a, "shared").await;
    let call = |name: &str, input: Value| ToolCall {
        id: "other".into(),
        name: name.into(),
        input: input.as_object().unwrap().clone(),
    };
    other
        .execute(
            &call("read", json!({"path": "shared"})),
            &b,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let second_call = call("write", json!({"path": "shared", "content": "two\n"}));
    let (first, second) = tokio::join!(
        workspace.call(&a, "write", json!({"path": "shared", "content": "one\n"})),
        other.execute(&second_call, &b, CancellationToken::new())
    );
    let second = second.unwrap();
    assert_ne!(first.is_error, second.is_error);
    let rejected = if first.is_error == Some(true) {
        &first
    } else {
        &second
    };
    assert_eq!(code(rejected), "file_modified_since_read");
    assert!(matches!(
        workspace.bytes("shared").as_slice(),
        b"one\r\n" | b"two\r\n"
    ));
    assert_eq!(fs::read_dir(&workspace.root).unwrap().count(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn replacement_preserves_permissions_and_open_original() {
    use std::{io::Read, os::unix::fs::PermissionsExt};
    let workspace = Workspace::new();
    workspace.put("private", b"before\r\n");
    fs::set_permissions(workspace.path("private"), fs::Permissions::from_mode(0o600)).unwrap();
    let mut original = fs::File::open(workspace.path("private")).unwrap();
    let reads = branch();
    workspace.read(&reads, "private").await;
    ok(&workspace
        .call(
            &reads,
            "write",
            json!({"path": "private", "content": "after\n"}),
        )
        .await);
    let mut old = Vec::new();
    original.read_to_end(&mut old).unwrap();
    assert_eq!(old, b"before\r\n");
    assert_eq!(workspace.bytes("private"), b"after\r\n");
    assert_eq!(
        fs::metadata(workspace.path("private"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(fs::read_dir(&workspace.root).unwrap().count(), 1);
}

#[test]
fn definitions_match_the_ts_tool_contracts() {
    let definitions = FileTools::definitions();
    let summary: Vec<(&str, Option<&str>)> = definitions
        .iter()
        .map(|definition| (definition.name.as_str(), definition.risk.as_deref()))
        .collect();
    assert_eq!(
        summary,
        [
            ("read", Some("read_filesystem")),
            ("write", Some("write_filesystem")),
            ("edit", Some("write_filesystem")),
            ("glob", Some("read_filesystem")),
            ("grep", Some("read_filesystem")),
        ]
    );
    assert!(
        definitions
            .iter()
            .all(|definition| FileTools::handles(&definition.name))
    );
    assert!(!FileTools::handles("exec"));
    assert_eq!(
        definitions[0].description,
        "Read a file from the filesystem. Text comes back with line numbers; use offset and limit for large files. A PNG, JPEG, WebP or GIF image comes back as an image block with one line giving its path and pixel size."
    );
}

#[tokio::test]
async fn edits_and_writes_preserve_bom_and_line_endings() {
    let workspace = Workspace::new();
    let reads = branch();

    workspace.put("crlf.txt", b"one\r\ntwo\r\nthree\r\n");
    ok(&workspace.read(&reads, "crlf.txt").await);
    ok(&workspace.edit(&reads, "crlf.txt", "two", "2a\n2b").await);
    assert_eq!(workspace.bytes("crlf.txt"), b"one\r\n2a\r\n2b\r\nthree\r\n");
    ok(&workspace
        .call(
            &reads,
            "write",
            json!({ "path": "crlf.txt", "content": "x\ny\n" }),
        )
        .await);
    assert_eq!(workspace.bytes("crlf.txt"), b"x\r\ny\r\n");

    workspace.put("bom.txt", b"\xef\xbb\xbfalpha\nbeta\n");
    let read = workspace.read(&reads, "bom.txt").await;
    assert_eq!(ok(&read), "1\talpha\n2\tbeta");
    assert_eq!(structured(&read)["bom"], "utf8");
    ok(&workspace.edit(&reads, "bom.txt", "beta", "gamma").await);
    assert_eq!(workspace.bytes("bom.txt"), b"\xef\xbb\xbfalpha\ngamma\n");
    ok(&workspace
        .call(
            &reads,
            "write",
            json!({ "path": "bom.txt", "content": "delta\n" }),
        )
        .await);
    assert_eq!(workspace.bytes("bom.txt"), b"\xef\xbb\xbfdelta\n");

    let mut utf16 = vec![0xff, 0xfe];
    utf16.extend(utf16le("héllo\r\nwörld\r\n"));
    workspace.put("utf16.txt", &utf16);
    let read = workspace.read(&reads, "utf16.txt").await;
    assert_eq!(ok(&read), "1\théllo\n2\twörld");
    assert_eq!(structured(&read)["encoding"], json!({ "kind": "utf16le" }));
    ok(&workspace.edit(&reads, "utf16.txt", "wörld", "wörld!").await);
    let mut expected = vec![0xff, 0xfe];
    expected.extend(utf16le("héllo\r\nwörld!\r\n"));
    assert_eq!(workspace.bytes("utf16.txt"), expected);

    workspace.put("mixed.txt", b"a\r\nb\nc\r\nd\n");
    let read = workspace.read(&reads, "mixed.txt").await;
    assert_eq!(structured(&read)["eol"], "mixed");
    ok(&workspace.edit(&reads, "mixed.txt", "d", "d1\nd2").await);
    assert_eq!(workspace.bytes("mixed.txt"), b"a\r\nb\nc\r\nd1\nd2\n");
    ok(&workspace.edit(&reads, "mixed.txt", "c\nd1", "C\nD1").await);
    assert_eq!(workspace.bytes("mixed.txt"), b"a\r\nb\nC\r\nD1\nd2\n");
    ok(&workspace.edit(&reads, "mixed.txt", "a", "a1\na2").await);
    assert_eq!(
        workspace.bytes("mixed.txt"),
        b"a1\r\na2\r\nb\nC\r\nD1\nd2\n"
    );

    workspace.put("quotes.txt", "say “hi”  \nnext\n".as_bytes());
    ok(&workspace.read(&reads, "quotes.txt").await);
    let edited = workspace
        .edit(&reads, "quotes.txt", "say \"hi\"\nnext", "said hi\nnext")
        .await;
    ok(&edited);
    assert_eq!(workspace.bytes("quotes.txt"), b"said hi\nnext\n");
    assert!(
        structured(&edited)["diff"]
            .as_str()
            .unwrap()
            .contains("-say “hi”")
    );
}

#[tokio::test]
async fn writes_require_a_fresh_read_on_the_same_branch() {
    let workspace = Workspace::new();
    let main = branch();
    workspace.put("a.txt", b"original\n");

    let unread = workspace
        .call(&main, "write", json!({ "path": "a.txt", "content": "new" }))
        .await;
    assert_eq!(code(&unread), "text_binding_required");
    assert_eq!(
        code(&workspace.edit(&main, "a.txt", "original", "x").await),
        "text_binding_required"
    );

    ok(&workspace.read(&main, "a.txt").await);
    let child = branch();
    let foreign = workspace
        .call(
            &child,
            "write",
            json!({ "path": "a.txt", "content": "child" }),
        )
        .await;
    assert_eq!(code(&foreign), "text_binding_required");
    assert_eq!(
        code(&workspace.edit(&child, "a.txt", "original", "x").await),
        "text_binding_required"
    );

    workspace.put("a.txt", b"changed outside\n");
    let stale = workspace
        .call(
            &main,
            "write",
            json!({ "path": "./a.txt", "content": "new" }),
        )
        .await;
    assert_eq!(code(&stale), "file_modified_since_read");
    assert_eq!(
        code(&workspace.edit(&main, "a.txt", "changed", "x").await),
        "file_modified_since_read"
    );
    assert_eq!(workspace.bytes("a.txt"), b"changed outside\n");

    let created = workspace
        .call(
            &child,
            "write",
            json!({ "path": "sub/new.txt", "content": "one\n" }),
        )
        .await;
    assert_eq!(ok(&created), "Wrote sub/new.txt");
    ok(&workspace.edit(&child, "sub/new.txt", "one", "two").await);
    assert_eq!(workspace.bytes("sub/new.txt"), b"two\n");
}

/// One blocking thread makes the order of file work observable: a gate task
/// queued behind the path resolutions runs only after both finished, and the
/// holder's file work then waits behind the gate.
#[test]
fn edits_of_one_file_run_in_fifo_order_and_waiters_can_cancel() {
    use futures_util::FutureExt;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let workspace = Workspace::new();
        let reads = branch();
        workspace.put("pair.txt", b"left right\n");
        ok(&workspace.read(&reads, "pair.txt").await);
        let (first, second) = tokio::join!(
            workspace.edit(&reads, "pair.txt", "left", "L"),
            workspace.edit(&reads, "pair.txt", "right", "R"),
        );
        ok(&first);
        ok(&second);
        assert_eq!(workspace.bytes("pair.txt"), b"L R\n");

        let gate = |release: std::sync::mpsc::Receiver<()>| {
            let (started, running) = tokio::sync::oneshot::channel();
            tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                release.recv().unwrap();
            });
            running
        };
        let (open_first, first_gate) = std::sync::mpsc::channel();
        gate(first_gate).await.unwrap();
        let waiter = CancellationToken::new();
        let mut holder = std::pin::pin!(workspace.edit(&reads, "pair.txt", "L", "HEAD"));
        let mut queued = std::pin::pin!(workspace.try_call(
            &reads,
            "edit",
            json!({ "path": "pair.txt", "oldText": "HEAD", "newText": "late" }),
            waiter.clone(),
        ));
        assert!(holder.as_mut().now_or_never().is_none());
        assert!(queued.as_mut().now_or_never().is_none());
        let (open_second, second_gate) = std::sync::mpsc::channel();
        let second = gate(second_gate);
        open_first.send(()).unwrap();
        second.await.unwrap();
        assert!(holder.as_mut().now_or_never().is_none());
        assert!(queued.as_mut().now_or_never().is_none());
        waiter.cancel();
        let queued = queued
            .now_or_never()
            .expect("a cancelled waiter returns at once");
        assert!(matches!(queued, Err(HostError::Cancelled(_))), "{queued:?}");
        open_second.send(()).unwrap();
        ok(&holder.await);
        assert_eq!(workspace.bytes("pair.txt"), b"HEAD R\n");

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let result = workspace
            .try_call(&reads, "read", json!({ "path": "pair.txt" }), cancelled)
            .await;
        assert!(matches!(result, Err(HostError::Cancelled(_))));
    });
}

#[tokio::test]
async fn read_rejects_non_text_targets() {
    let workspace = Workspace::new();
    let reads = branch();
    workspace.put("latin.txt", b"caf\xe9\n");
    workspace.put("blob.dat", b"ab\0cd");
    workspace.put("archive.zip", b"PK");
    fs::create_dir(workspace.path("dir")).unwrap();

    assert_eq!(
        code(&workspace.read(&reads, "latin.txt").await),
        "text_encoding_required"
    );
    assert_eq!(
        code(&workspace.read(&reads, "blob.dat").await),
        "binary_unsupported"
    );
    assert_eq!(
        code(&workspace.read(&reads, "archive.zip").await),
        "binary_unsupported"
    );
    let directory = workspace.read(&reads, "dir").await;
    assert_eq!(code(&directory), "is_directory");
    assert_eq!(text(&directory), "Cannot read directory: dir");
    let missing = workspace.read(&reads, "Latin.TXT.missing").await;
    assert_eq!(code(&missing), "file_not_found");
    assert_eq!(
        code(&workspace.call(&reads, "read", json!({ "path": "" })).await),
        "invalid_input"
    );
}

#[tokio::test]
async fn read_pages_long_files() {
    let workspace = Workspace::new();
    let reads = branch();
    workspace.put("lines.txt", b"a\nb\nc\nd\n");
    let page = workspace
        .call(
            &reads,
            "read",
            json!({ "path": "lines.txt", "offset": 2, "limit": 2 }),
        )
        .await;
    assert_eq!(
        ok(&page),
        "2\tb\n3\tc\n\nFile is larger than the safe read budget for one response. Showing lines 2-3 of 4. Continue with read({ path: \"lines.txt\", offset: 4 })."
    );
    let beyond = workspace
        .call(&reads, "read", json!({ "path": "lines.txt", "offset": 9 }))
        .await;
    assert_eq!(code(&beyond), "offset_out_of_range");
    assert_eq!(text(&beyond), "Read offset 9 exceeds file length.");
}

#[tokio::test]
async fn read_offloads_large_results_to_the_output_directory() {
    let workspace = Workspace::new();
    let reads = branch();
    let line = "x".repeat(99);
    let body: String = (0..400).map(|_| format!("{line}\n")).collect();
    workspace.put("big.txt", body.as_bytes());
    let result = workspace.read(&reads, "big.txt").await;
    let content = ok(&result);
    assert_eq!(structured(&result)["offloaded"], true);
    let output = structured(&result)["outputPath"].as_str().unwrap();
    assert!(
        output.starts_with(
            workspace
                .root
                .parent()
                .unwrap()
                .join("outputs")
                .to_str()
                .unwrap()
        )
    );
    let full = fs::read_to_string(output).unwrap();
    assert_eq!(full.lines().count(), 400);
    assert!(full.starts_with(&format!("1\t{line}\n2\t")));
    let (head, preview) = content
        .split_once("\n\nPreview (first 2000 chars):\n")
        .unwrap();
    assert_eq!(
        head,
        format!(
            "Read output too large ({} bytes). Full output: {output}",
            full.len()
        )
    );
    assert_eq!(preview, &full[..2000]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(Path::new(output)), 0o600);
        assert_eq!(mode(Path::new(output).parent().unwrap()), 0o700);
    }
    let last = format!("{line}\n");
    let edit = workspace
        .call(
            &reads,
            "edit",
            json!({ "path": "big.txt", "oldText": last, "newText": "y\n", "replaceAll": true }),
        )
        .await;
    ok(&edit);
    assert_eq!(workspace.bytes("big.txt"), "y\n".repeat(400).into_bytes());

    workspace.put("small.txt", b"hi\n");
    let small = workspace.read(&reads, "small.txt").await;
    assert_eq!(ok(&small), "1\thi");
    assert!(!structured(&small).contains_key("offloaded"));
}

#[tokio::test]
async fn oversized_lines_report_partial_content_without_skipping_the_tail() {
    let workspace = Workspace::new();
    let reads = branch();
    for (body, partial) in [
        (format!("{}TAIL", "a".repeat(262_138)), false),
        (format!("{}TAIL", "a".repeat(300_000)), true),
        (format!("{}TAIL\r\nnext", "😀".repeat(150_000)), true),
    ] {
        workspace.put("long.txt", body.as_bytes());
        let result = workspace.read(&reads, "long.txt").await;
        ok(&result);
        let full = fs::read_to_string(structured(&result)["outputPath"].as_str().unwrap()).unwrap();
        if partial {
            assert!(text(&result).contains("truncated"));
            assert!(full.contains("truncated"));
            assert!(!full.contains("TAIL"));
            assert!(!full.contains("offset: 2"));
        } else {
            assert!(full.ends_with("TAIL"));
            assert!(!full.contains("truncated"));
        }
        workspace.put("long.txt", body.replace("TAIL", "CHANGED").as_bytes());
        let edit = workspace.edit(&reads, "long.txt", "a", "b").await;
        assert_eq!(code(&edit), "file_modified_since_read");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn special_files_are_refused_without_blocking() {
    let workspace = Workspace::new();
    let reads = branch();
    let fifo = workspace.path("pipe");
    let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: plain syscall creating a FIFO in the test directory.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        workspace.read(&reads, "pipe"),
    )
    .await
    .expect("read of a FIFO returns");
    assert_eq!(code(&result), "not_regular_file");
    let socket_path = std::env::temp_dir().join(uuid::Uuid::new_v4().simple().to_string());
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    let socket = workspace.read(&reads, socket_path.to_str().unwrap()).await;
    drop(listener);
    fs::remove_file(socket_path).unwrap();
    assert_eq!(code(&socket), "read_error");
}

#[tokio::test]
async fn read_windows_far_into_a_file_and_rejects_overflowing_offsets() {
    let workspace = Workspace::new();
    let reads = branch();
    let body: String = (1..=5000).map(|n| format!("line {n}\n")).collect();
    workspace.put("log.txt", body.as_bytes());
    let page = workspace
        .call(
            &reads,
            "read",
            json!({ "path": "log.txt", "offset": 4999, "limit": 5 }),
        )
        .await;
    assert_eq!(ok(&page), "4999\tline 4999\n5000\tline 5000");
    let beyond = workspace
        .call(
            &reads,
            "read",
            json!({ "path": "log.txt", "offset": u64::MAX as f64, "limit": 2 }),
        )
        .await;
    assert_eq!(code(&beyond), "offset_out_of_range");
    let mut broken = body.into_bytes();
    broken.push(0xFF);
    workspace.put("broken.txt", &broken);
    let beyond = workspace
        .call(
            &reads,
            "read",
            json!({ "path": "broken.txt", "offset": u64::MAX as f64, "limit": 2 }),
        )
        .await;
    assert_eq!(code(&beyond), "offset_out_of_range");
}

#[tokio::test]
async fn a_failed_offload_does_not_bind_the_file() {
    let workspace = Workspace::new();
    let reads = branch();
    workspace.put("blocker", b"");
    let tools = FileTools::new(
        workspace.root.clone(),
        workspace.root.clone(),
        workspace.path("blocker").join("outputs"),
    );
    let body: String = (0..400).map(|_| format!("{}\n", "x".repeat(99))).collect();
    workspace.put("big.txt", body.as_bytes());
    let call = |name: &str, input: Value| ToolCall {
        id: "call".into(),
        name: name.into(),
        input: input.as_object().unwrap().clone(),
    };
    let read = tools
        .execute(
            &call("read", json!({ "path": "big.txt" })),
            &reads,
            CancellationToken::new(),
        )
        .await;
    assert!(read.is_err(), "the offload failed");
    let write = tools
        .execute(
            &call("write", json!({ "path": "big.txt", "content": "replaced" })),
            &reads,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(code(&write), "text_binding_required");
    assert_eq!(workspace.bytes("big.txt"), body.into_bytes());
}

#[tokio::test]
async fn edit_inserts_new_text_literally() {
    let workspace = Workspace::new();
    let reads = branch();
    workspace.put("run.sh", b"echo PID\n");
    ok(&workspace.read(&reads, "run.sh").await);
    ok(&workspace.edit(&reads, "run.sh", "PID", "$$ $& $' $`").await);
    assert_eq!(workspace.bytes("run.sh"), b"echo $$ $& $' $`\n");
}

#[tokio::test]
async fn legacy_encodings_round_trip_and_reject_unrepresentable_text() {
    let workspace = Workspace::new();
    let reads = branch();
    let (gbk, _, _) = encoding_rs::GBK.encode("中文内容\r\n第二行\r\n");
    workspace.put("gbk.txt", &gbk);

    assert_eq!(
        code(&workspace.read(&reads, "gbk.txt").await),
        "text_encoding_required"
    );
    let read = workspace
        .call(
            &reads,
            "read",
            json!({ "path": "gbk.txt", "encoding": "gbk" }),
        )
        .await;
    assert_eq!(ok(&read), "1\t中文内容\n2\t第二行");
    assert_eq!(
        structured(&read)["encoding"],
        json!({ "kind": "legacy", "name": "gbk" })
    );
    assert_eq!(structured(&read)["encodingSource"], "user");

    ok(&workspace.edit(&reads, "gbk.txt", "第二行", "第三行").await);
    let (expected, _, _) = encoding_rs::GBK.encode("中文内容\r\n第三行\r\n");
    assert_eq!(workspace.bytes("gbk.txt"), expected.as_ref());

    let before = workspace.bytes("gbk.txt");
    let emoji = workspace.edit(&reads, "gbk.txt", "第三行", "😀").await;
    assert_eq!(code(&emoji), "text_not_representable");
    let overwrite = workspace
        .call(
            &reads,
            "write",
            json!({ "path": "gbk.txt", "content": "😀" }),
        )
        .await;
    assert_eq!(code(&overwrite), "text_not_representable");
    assert_eq!(workspace.bytes("gbk.txt"), before);

    let cp936 = workspace
        .call(
            &reads,
            "read",
            json!({ "path": "gbk.txt", "encoding": "cp936" }),
        )
        .await;
    assert_eq!(ok(&cp936), "1\t中文内容\n2\t第三行");
    let conflict = {
        workspace.put("bom.txt", b"\xef\xbb\xbfx");
        workspace
            .call(
                &reads,
                "read",
                json!({ "path": "bom.txt", "encoding": "gbk" }),
            )
            .await
    };
    assert_eq!(code(&conflict), "text_encoding_conflict");
}

/// Mirrors `executor.acceptance.test.ts` (ripgrep runner expectations).
fn search_fixture(workspace: &Workspace) {
    fs::create_dir_all(workspace.path(".git")).unwrap();
    workspace.put(".ignore", b"ignored.ts\n");
    workspace.put(".hidden.ts", b"needle hidden\n");
    workspace.put("ignored.ts", b"needle ignored\n");
    let visible: Vec<String> = (0..7)
        .map(|index| format!("needle visible {index}"))
        .collect();
    workspace.put("visible.ts", visible.join("\n").as_bytes());
    workspace.put("bundle.min.js", b"needle minified\n");
    workspace.put("node_modules/excluded.ts", b"needle excluded\n");
    fs::File::create(workspace.path("large.bin"))
        .unwrap()
        .set_len(10 * 1024 * 1024 + 1)
        .unwrap();
}

fn glob_paths(result: &ToolResult) -> Vec<String> {
    let mut paths: Vec<String> = structured(result)["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap().to_owned())
        .collect();
    paths.sort();
    paths
}

#[tokio::test]
async fn glob_matches_the_ripgrep_runner() {
    let workspace = Workspace::new();
    let reads = branch();
    search_fixture(&workspace);

    let all = workspace
        .call(&reads, "glob", json!({ "pattern": "*.ts" }))
        .await;
    assert_eq!(glob_paths(&all), [".hidden.ts", "ignored.ts", "visible.ts"]);
    assert_eq!(structured(&all)["truncated"], false);
    assert!(ok(&all).starts_with("Matched 3 files. Showing 3 files. Not truncated.\n\n"));
    let first = &structured(&all)["files"][0];
    assert_eq!(first["label"], first["path"]);
    assert_eq!(
        Path::new(first["fsPath"].as_str().unwrap()),
        workspace.path(first["path"].as_str().unwrap())
    );

    for name in [
        "src/a.ts",
        "src/]file.ts",
        "src/nested/b.ts",
        "src/nested/afile.ts",
        "src/nested/c.ts",
    ] {
        workspace.put(name, b"");
    }
    let scoped = workspace
        .call(&reads, "glob", json!({ "pattern": "src/**/[ab].ts" }))
        .await;
    assert_eq!(glob_paths(&scoped), ["src/a.ts", "src/nested/b.ts"]);
    let bracket = workspace
        .call(&reads, "glob", json!({ "pattern": "src/**/[]a]file.ts" }))
        .await;
    assert_eq!(
        glob_paths(&bracket),
        ["src/]file.ts", "src/nested/afile.ts"]
    );

    let limited = workspace
        .call(&reads, "glob", json!({ "pattern": "**/*.TS", "limit": 2 }))
        .await;
    assert_eq!(structured(&limited)["truncated"], true);
    assert!(ok(&limited).starts_with(
        "Matched at least 2 files. Showing 2 files. More files may remain; narrow path or pattern and retry.\n\n"
    ));

    let file = workspace
        .call(
            &reads,
            "glob",
            json!({ "pattern": "*.ts", "path": "src/a.ts" }),
        )
        .await;
    assert_eq!(glob_paths(&file), ["a.ts"]);

    for pattern in [
        "*",
        "a*",
        "**",
        "/abs/*.ts",
        "a/../b.ts",
        "*.{ts,js}",
        "!x.ts",
    ] {
        let rejected = workspace
            .call(&reads, "glob", json!({ "pattern": pattern }))
            .await;
        assert_eq!(code(&rejected), "invalid_input", "{pattern}");
        assert_eq!(
            text(&rejected),
            "pattern must be a relative git-style glob with at least two consecutive literal characters in its final component, for example *.mjs or **/*.rs; bare *, **, **/*, leading ./, absolute paths, .., braces, and backslashes are not supported. Use path to choose the search directory"
        );
    }
    let home = FileTools::new(
        workspace.root.clone(),
        workspace.root.clone(),
        workspace.root.join("outputs"),
    );
    let call = ToolCall {
        id: "call".into(),
        name: "glob".into(),
        input: json!({ "pattern": "*.ts" }).as_object().unwrap().clone(),
    };
    let open_ended = home
        .execute(&call, &reads, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        text(&open_ended),
        "Open-ended searches cannot use the filesystem root or user home"
    );
}

#[tokio::test]
async fn grep_survives_byte_matches_and_rejects_bad_include_globs() {
    let workspace = Workspace::new();
    let reads = branch();
    workspace.put("accent.txt", "café\n".as_bytes());
    let result = workspace
        .call(
            &reads,
            "grep",
            json!({ "pattern": "(?-u:\\xA9)", "path": "accent.txt", "outputMode": "content" }),
        )
        .await;
    assert_eq!(result.is_error, None, "{}", text(&result));
    assert_eq!(structured(&result)["matches"][0]["column"], 4);
    for pattern in ["(?-u:\\xA9)", "(?-u:\\xC3)"] {
        let result = workspace
            .call(
                &reads,
                "grep",
                json!({ "pattern": pattern, "path": "accent.txt", "outputMode": "content", "multiline": true }),
            )
            .await;
        assert_eq!(result.is_error, None, "{}", text(&result));
        assert_eq!(structured(&result)["matches"][0]["column"], 4);
        assert_eq!(structured(&result)["matches"][0]["content"], "é");
    }
    for path in ["accent.txt", "."] {
        let rejected = workspace
            .call(
                &reads,
                "grep",
                json!({ "pattern": "caf", "path": path, "include": "[" }),
            )
            .await;
        assert_eq!(code(&rejected), "invalid_input", "path: {path}");
        assert!(
            text(&rejected).contains("Invalid glob"),
            "{}",
            text(&rejected)
        );
    }
}

#[tokio::test]
async fn grep_matches_the_ripgrep_runner() {
    let workspace = Workspace::new();
    let reads = branch();
    search_fixture(&workspace);

    let found = workspace
        .call(
            &reads,
            "grep",
            json!({ "pattern": "needle", "literal": true, "lineNumbers": true }),
        )
        .await;
    let matches = structured(&found)["matches"].as_array().unwrap();
    let mut files: Vec<&str> = matches
        .iter()
        .map(|row| row["file"].as_str().unwrap())
        .collect();
    files.dedup();
    files.sort();
    assert_eq!(files, [".hidden.ts", "visible.ts"]);
    assert_eq!(structured(&found)["truncated"], true);
    assert_eq!(
        matches
            .iter()
            .filter(|row| row["file"] == "visible.ts")
            .count(),
        5
    );
    assert_eq!(
        ok(&found),
        "Matched at least 6 records in 2 shown files. More matches may remain; narrow path or include and retry.\n\n\
         .hidden.ts\n  1| needle hidden\n\
         visible.ts\n  1| needle visible 0\n  2| needle visible 1\n  3| needle visible 2\n  4| needle visible 3\n  5| needle visible 4"
    );

    let large = workspace
        .call(
            &reads,
            "grep",
            json!({ "pattern": "needle", "path": "large.bin" }),
        )
        .await;
    assert_eq!(
        structured(&large)["skippedFiles"],
        json!([{ "file": "large.bin", "reason": "too_large" }])
    );

    workspace.put(
        "ctx/a.txt",
        b"zero\nOne target\ntwo\nthree\nfour target\nfive\n",
    );
    let context = workspace
        .call(
            &reads,
            "grep",
            json!({ "pattern": "target", "path": "ctx", "-C": 1, "-n": true }),
        )
        .await;
    assert_eq!(
        ok(&context),
        "Matched 2 records in 1 shown files. Not truncated.\n\n\
         a.txt\n  1| zero\n  2| One target\n  3| two\n  4| three\n  5| four target\n  6| five"
    );
    assert_eq!(structured(&context)["matches"][0]["column"], 5);

    let files_mode = workspace
        .call(
            &reads,
            "grep",
            json!({ "pattern": "Target", "include": "*.{txt,md}", "outputMode": "files" }),
        )
        .await;
    assert_eq!(
        ok(&files_mode),
        "Matched 0 files in 0 shown files. Not truncated."
    );
    let files_mode = workspace
        .call(
            &reads,
            "grep",
            json!({ "pattern": "target", "include": "*.{txt,md}", "output_mode": "files" }),
        )
        .await;
    assert_eq!(
        ok(&files_mode),
        "Matched 1 files in 1 shown files. Not truncated.\n\nctx/a.txt: 2 matches"
    );

    let multiline = workspace
        .call(
            &reads,
            "grep",
            json!({ "pattern": "two\\nthree", "path": "ctx", "multiline": true }),
        )
        .await;
    assert_eq!(structured(&multiline)["matches"][0]["line"], 3);
    assert_eq!(
        structured(&multiline)["matches"][0]["content"],
        "two\nthree"
    );

    for (input, message) in [
        (
            json!({ "pattern": "(a+)+" }),
            "Potentially catastrophic quantifier pattern detected (group with alternation/wildcard + broad quantifier)",
        ),
        (
            json!({ "pattern": "(x)\\1" }),
            "Backreferences (\\1, \\k<name>) are not allowed",
        ),
        (
            json!({ "pattern": "x", "ignoreCase": true }),
            "ignoreCase is no longer supported",
        ),
        (
            json!({ "pattern": "x", "limit": 101 }),
            "limit must be an integer from 1 to 100",
        ),
        (
            json!({ "pattern": "x", "-A": 11 }),
            "afterContext must be an integer from 0 to 10",
        ),
    ] {
        let rejected = workspace.call(&reads, "grep", input).await;
        assert_eq!(code(&rejected), "invalid_input");
        assert_eq!(text(&rejected), message);
    }
}

#[tokio::test]
async fn file_tools_declare_the_file_they_published() {
    use dim_agent_types::ToolEffect;
    let workspace = Workspace::new();
    let reads = branch();
    workspace.put("file.txt", b"before\n");
    let path = workspace.path("file.txt");
    let path = path.to_str().unwrap().to_owned();

    // A call that fails before the publish declares nothing.
    let refused = workspace
        .call(
            &reads,
            "write",
            json!({ "path": path, "content": "after\n" }),
        )
        .await;
    assert_eq!(code(&refused), "text_binding_required");
    assert_eq!(refused.effects, None);

    ok(&workspace.read(&reads, &path).await);
    let written = workspace
        .call(
            &reads,
            "write",
            json!({ "path": path, "content": "after\n" }),
        )
        .await;
    assert_eq!(
        written.effects,
        Some(vec![ToolEffect::FileWritten { path: path.clone() }])
    );
    let edited = workspace.edit(&reads, &path, "after", "changed").await;
    assert_eq!(
        edited.effects,
        Some(vec![ToolEffect::FileWritten { path: path.clone() }])
    );
    let unmatched = workspace.edit(&reads, &path, "missing", "x").await;
    assert_eq!(code(&unmatched), "oldtext_not_found");
    assert_eq!(unmatched.effects, None);
    assert_eq!(workspace.bytes("file.txt"), b"changed\n");
}

// ── #656: `read` returns image files as image blocks ────────────────────────

use base64::Engine as _;
use dim_agent_capabilities::images;
use dim_agent_types::{ContentBlock, ImageSource, ImageSourceType};
use image::{DynamicImage, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};

fn png_bytes(image: RgbaImage) -> Vec<u8> {
    let mut encoded = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut encoded, ImageFormat::Png)
        .unwrap();
    encoded.into_inner()
}

fn png(width: u32, height: u32, pixel: impl Fn(u32, u32) -> Rgba<u8>) -> Vec<u8> {
    png_bytes(RgbaImage::from_fn(width, height, pixel))
}

/// Deterministic noise: an image that compresses to neither PNG small nor JPEG
/// small, so its byte size follows its pixel count.
fn noise_png(width: u32, height: u32) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    png_bytes(RgbaImage::from_fn(width, height, |_, _| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        Rgba([state as u8, (state >> 8) as u8, (state >> 16) as u8, 255])
    }))
}

fn jpeg(image: &DynamicImage, quality: u8) -> Vec<u8> {
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, quality)
        .encode_image(image)
        .unwrap();
    encoded
}

/// The image block and its line from a `read` result.
fn image_blocks(result: &ToolResult) -> (&str, &ImageSource) {
    let ToolContent::Blocks(blocks) = &result.content else {
        panic!("not an image result: {:?}", result.content);
    };
    let ContentBlock::Text { text } = &blocks[0] else {
        panic!("the first block is not the line: {blocks:?}");
    };
    let ContentBlock::Image { source } = &blocks[1] else {
        panic!("the second block is not the image: {blocks:?}");
    };
    (text, source)
}

fn sent(source: &ImageSource) -> Vec<u8> {
    assert_eq!(source.source_type, ImageSourceType::Base64);
    base64::engine::general_purpose::STANDARD
        .decode(source.data.as_deref().expect("image data"))
        .unwrap()
}

fn decoded(source: &ImageSource) -> RgbaImage {
    image::load_from_memory(&sent(source)).unwrap().to_rgba8()
}

#[tokio::test]
async fn read_returns_a_png_as_an_image_block_and_a_line() {
    let workspace = Workspace::new();
    let reads = branch();
    let bytes = png(320, 240, |x, _| {
        if x < 160 {
            Rgba([255, 0, 0, 255])
        } else {
            Rgba([0, 0, 255, 255])
        }
    });
    workspace.put("screen.png", &bytes);
    let result = workspace.read(&reads, "screen.png").await;
    assert_eq!(result.is_error, None);
    let (line, source) = image_blocks(&result);
    assert_eq!(line, "Read image: screen.png (320x240 px).");
    assert_eq!(source.media_type, "image/png");
    assert_eq!(
        sent(source),
        bytes,
        "an image within the limits is sent as read"
    );
    assert_eq!(decoded(source).dimensions(), (320, 240));
    let structured = structured(&result);
    assert_eq!(structured["kind"], "image");
    assert_eq!(structured["width"], 320);
    assert_eq!(structured["height"], 240);
    assert!(structured.get("sentWidth").is_none());

    // offset and limit do not apply to images, and the line says so.
    let windowed = workspace
        .call(&reads, "read", json!({ "path": "screen.png", "offset": 2 }))
        .await;
    let (line, _) = image_blocks(&windowed);
    assert!(
        line.ends_with(" Offset and limit do not apply to images."),
        "{line}"
    );

    // The image is recognized by its content, not its extension: text named
    // like an image stays text.
    workspace.put("fake.png", b"plain text\n");
    assert_eq!(
        ok(&workspace.read(&reads, "fake.png").await),
        "1\tplain text"
    );
}

#[tokio::test]
async fn read_recognizes_an_image_ahead_of_the_binary_extension_refusal() {
    let workspace = Workspace::new();
    let reads = branch();
    let bytes = png(320, 240, |x, _| {
        if x < 160 {
            Rgba([255, 0, 0, 255])
        } else {
            Rgba([0, 0, 255, 255])
        }
    });
    workspace.put("screenshot.bin", &bytes);
    let result = workspace.read(&reads, "screenshot.bin").await;
    assert_eq!(result.is_error, None);
    let (line, source) = image_blocks(&result);
    assert_eq!(line, "Read image: screenshot.bin (320x240 px).");
    assert_eq!(source.media_type, "image/png");
    assert_eq!(sent(source), bytes);
    assert_eq!(structured(&result)["kind"], "image");

    // The extension still refuses content that is not an image, text included.
    workspace.put("blob.bin", b"ab\0cd");
    assert_eq!(
        code(&workspace.read(&reads, "blob.bin").await),
        "binary_unsupported"
    );
    workspace.put("notes.bin", b"plain text\n");
    assert_eq!(
        code(&workspace.read(&reads, "notes.bin").await),
        "binary_unsupported"
    );
}

/// The extension refusal comes after the file is opened, so a missing binary
/// file reports its missing path rather than its extension.
#[tokio::test]
async fn read_reports_a_missing_file_before_its_binary_extension() {
    let workspace = Workspace::new();
    let reads = branch();
    assert_eq!(
        code(&workspace.read(&reads, "missing.zip").await),
        "file_not_found"
    );
}

#[tokio::test]
async fn read_scales_a_large_jpeg_within_the_limits() {
    let workspace = Workspace::new();
    let reads = branch();
    let mut image = RgbImage::new(3000, 2000);
    let mut state = 0x2545_f491_4f6c_dd1du64;
    for pixel in image.pixels_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *pixel = Rgb([state as u8, (state >> 8) as u8, (state >> 16) as u8]);
    }
    let bytes = jpeg(&DynamicImage::ImageRgb8(image), 95);
    assert!(
        images::base64_len(bytes.len() as u64) > images::MAX_BASE64_BYTES,
        "the fixture is over the base64 limit: {}",
        images::base64_len(bytes.len() as u64)
    );
    workspace.put("photo.jpg", &bytes);
    let result = workspace.read(&reads, "photo.jpg").await;
    assert_eq!(result.is_error, None);
    let (line, source) = image_blocks(&result);
    assert!(line.contains("(3000x2000 px)"), "{line}");
    assert!(line.contains("as image/jpeg"), "{line}");
    assert!(line.contains("Sent 2048x1365 px"), "{line}");
    let sent = sent(source);
    assert!(
        images::base64_len(sent.len() as u64) <= images::MAX_BASE64_BYTES,
        "{}",
        images::base64_len(sent.len() as u64)
    );
    assert!(line.contains(&format!("({} bytes)", sent.len())), "{line}");
    assert_eq!(decoded(source).dimensions(), (2048, 1365));
    assert_eq!(structured(&result)["sentWidth"], 2048);
}

/// The old limit measured raw bytes, so this image was sent unchanged and the
/// request then exceeded the provider's per-image limit once base64-encoded.
#[tokio::test]
async fn read_reencodes_an_image_whose_base64_exceeds_the_limit() {
    let workspace = Workspace::new();
    let reads = branch();
    let bytes = noise_png(1000, 1000);
    assert!(
        bytes.len() as u64 <= 4 * 1024 * 1024,
        "within the 4 MiB of raw bytes the old limit measured: {}",
        bytes.len()
    );
    assert!(
        images::base64_len(bytes.len() as u64) > images::MAX_BASE64_BYTES,
        "over the base64 limit: {}",
        images::base64_len(bytes.len() as u64)
    );
    workspace.put("noise.png", &bytes);
    let result = workspace.read(&reads, "noise.png").await;
    assert_eq!(result.is_error, None);
    let (line, source) = image_blocks(&result);
    assert!(line.contains("(1000x1000 px)"), "{line}");
    assert!(line.contains("Sent 1000x1000 px as image/jpeg"), "{line}");
    assert_ne!(sent(source), bytes, "the image is re-encoded");
    assert!(
        images::base64_len(sent(source).len() as u64) <= images::MAX_BASE64_BYTES,
        "the re-encoded base64 fits: {}",
        images::base64_len(sent(source).len() as u64)
    );
}

/// Pad a PNG to exactly `target` bytes with a tEXt chunk before IEND.
fn pad_png_to(mut bytes: Vec<u8>, target: u64) -> Vec<u8> {
    assert!(target > bytes.len() as u64 + 12 + 8, "the padding fits");
    let data_len = (target - bytes.len() as u64 - 12) as usize;
    let mut data = b"Comment\0".to_vec();
    data.resize(data_len, b'x');
    let mut typed = b"tEXt".to_vec();
    typed.extend_from_slice(&data);
    let iend = bytes.split_off(bytes.len() - 12);
    bytes.extend_from_slice(&(data_len as u32).to_be_bytes());
    bytes.extend_from_slice(&typed);
    bytes.extend_from_slice(&crc32(&typed).to_be_bytes());
    bytes.extend_from_slice(&iend);
    bytes
}

#[tokio::test]
async fn read_sends_an_image_whose_base64_just_fits_byte_for_byte() {
    let workspace = Workspace::new();
    let reads = branch();
    let fits_raw = images::MAX_BASE64_BYTES / 4 * 3;
    assert_eq!(
        fits_raw, 3_750_000,
        "the raw length whose base64 is 5,000,000"
    );
    let bytes = pad_png_to(noise_png(900, 900), fits_raw);
    assert_eq!(
        images::base64_len(bytes.len() as u64),
        images::MAX_BASE64_BYTES
    );
    workspace.put("exact.png", &bytes);
    let result = workspace.read(&reads, "exact.png").await;
    assert_eq!(result.is_error, None);
    let (line, source) = image_blocks(&result);
    assert_eq!(line, "Read image: exact.png (900x900 px).");
    assert_eq!(
        sent(source),
        bytes,
        "an image whose base64 exactly fits is sent as read"
    );
    assert_eq!(
        source.data.as_deref().unwrap().len() as u64,
        images::MAX_BASE64_BYTES
    );
}

#[tokio::test]
async fn read_reencodes_an_image_one_byte_over_the_raw_fit() {
    let workspace = Workspace::new();
    let reads = branch();
    let over_raw = images::MAX_BASE64_BYTES / 4 * 3 + 1;
    let bytes = pad_png_to(noise_png(900, 900), over_raw);
    assert_eq!(bytes.len() as u64, 3_750_001);
    assert_eq!(
        images::base64_len(bytes.len() as u64),
        images::MAX_BASE64_BYTES + 4
    );
    workspace.put("over.png", &bytes);
    let result = workspace.read(&reads, "over.png").await;
    assert_eq!(result.is_error, None);
    let (line, source) = image_blocks(&result);
    assert!(line.contains("(900x900 px)"), "{line}");
    assert_ne!(sent(source), bytes, "the image is re-encoded");
    assert!(
        images::base64_len(sent(source).len() as u64) <= images::MAX_BASE64_BYTES,
        "the re-encoded base64 fits: {}",
        images::base64_len(sent(source).len() as u64)
    );
}

/// A TIFF block whose EXIF orientation tag holds `orientation`.
fn tiff_orientation(orientation: u16) -> Vec<u8> {
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II\x2a\x00");
    tiff.extend_from_slice(&8u32.to_le_bytes());
    tiff.extend_from_slice(&1u16.to_le_bytes());
    tiff.extend_from_slice(&0x0112u16.to_le_bytes());
    tiff.extend_from_slice(&3u16.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&orientation.to_le_bytes());
    tiff.extend_from_slice(&0u16.to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes());
    tiff
}

/// A JPEG with an EXIF APP1 orientation segment in front of its frame.
fn with_orientation(jpeg: Vec<u8>, orientation: u16) -> Vec<u8> {
    let mut app1 = Vec::new();
    app1.extend_from_slice(b"Exif\x00\x00");
    app1.extend_from_slice(&tiff_orientation(orientation));
    let mut out = Vec::new();
    out.extend_from_slice(&jpeg[..2]);
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);
    out
}

#[tokio::test]
async fn read_turns_an_exif_rotated_photo_upright() {
    let workspace = Workspace::new();
    let reads = branch();
    let image = RgbaImage::from_fn(40, 20, |x, _| {
        if x < 20 {
            Rgba([255, 0, 0, 255])
        } else {
            Rgba([0, 0, 255, 255])
        }
    });
    workspace.put(
        "portrait.jpg",
        &with_orientation(jpeg(&DynamicImage::ImageRgba8(image), 95), 6),
    );
    let result = workspace.read(&reads, "portrait.jpg").await;
    assert_eq!(result.is_error, None);
    let (line, source) = image_blocks(&result);
    assert!(line.contains("(20x40 px)"), "{line}");
    assert!(line.contains("Sent 20x40 px as image/jpeg"), "{line}");
    let image = decoded(source);
    assert_eq!(image.dimensions(), (20, 40), "the sent pixels are upright");
    // Stored left half red, right half blue, rotated clockwise: the first row
    // is the stored bottom edge, so its ends keep their colours.
    let top = image.get_pixel(0, 0);
    let bottom = image.get_pixel(0, 39);
    assert!(top[0] > 200 && top[2] < 60, "top {top:?}");
    assert!(bottom[2] > 200 && bottom[0] < 60, "bottom {bottom:?}");
    let structured = structured(&result);
    assert_eq!(structured["width"], 20);
    assert_eq!(structured["height"], 40);
}

#[tokio::test]
async fn read_keeps_a_scaled_screenshot_png() {
    let workspace = Workspace::new();
    let reads = branch();
    workspace.put(
        "wide.png",
        &png(3000, 1000, |_, _| Rgba([250, 250, 250, 255])),
    );
    let result = workspace.read(&reads, "wide.png").await;
    let (line, source) = image_blocks(&result);
    assert!(line.contains("(3000x1000 px)"), "{line}");
    assert!(line.contains("Sent 2048x683 px as image/png"), "{line}");
    assert_eq!(source.media_type, "image/png");
    assert_eq!(decoded(source).dimensions(), (2048, 683));
}

#[tokio::test]
async fn read_returns_the_first_frame_of_an_animated_gif() {
    let workspace = Workspace::new();
    let reads = branch();
    let mut gif = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
        encoder
            .encode_frames([
                image::Frame::new(RgbaImage::from_pixel(20, 20, Rgba([255, 0, 0, 255]))),
                image::Frame::new(RgbaImage::from_pixel(20, 20, Rgba([0, 0, 255, 255]))),
            ])
            .unwrap();
    }
    workspace.put("anim.gif", &gif);
    let result = workspace.read(&reads, "anim.gif").await;
    let (line, source) = image_blocks(&result);
    assert!(
        line.contains("Animated GIF: only the first frame was sent."),
        "{line}"
    );
    let image = decoded(source);
    assert_eq!(image.dimensions(), (20, 20));
    let pixel = image.get_pixel(10, 10);
    assert!(pixel[0] > 200 && pixel[2] < 60, "{pixel:?}");
    assert_eq!(structured(&result)["animated"], true);
}

/// One RIFF chunk: fourcc, little-endian payload size, payload, padded to
/// even.
fn riff_chunk(fourcc: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = fourcc.to_vec();
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// The `VP8L` bitstream the lossless WebP encoder writes for a frame.
fn vp8l(frame: &RgbaImage) -> Vec<u8> {
    let mut encoded = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut encoded)
        .encode(
            frame.as_raw(),
            frame.width(),
            frame.height(),
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    assert_eq!(&encoded[..4], b"RIFF");
    assert_eq!(&encoded[12..16], b"VP8L");
    let size = u32::from_le_bytes(encoded[16..20].try_into().unwrap()) as usize;
    encoded[20..20 + size].to_vec()
}

/// An animated WebP carrying an EXIF orientation: a `VP8X` header marking the
/// animation and the EXIF chunk, `ANIM`, one `ANMF` per frame and `EXIF`.
fn animated_webp(frames: [&RgbaImage; 2], orientation: u16) -> Vec<u8> {
    let (width, height) = frames[0].dimensions();
    let alpha = frames
        .iter()
        .any(|frame| frame.pixels().any(|pixel| pixel[3] < 255));
    let mut body = Vec::new();
    let mut vp8x = vec![0x0A | if alpha { 0x10 } else { 0 }, 0, 0, 0];
    vp8x.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
    vp8x.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
    body.extend_from_slice(&riff_chunk(b"VP8X", &vp8x));
    body.extend_from_slice(&riff_chunk(b"ANIM", &[0, 0, 0, 0, 0, 0]));
    for frame in frames {
        let mut anmf = vec![0; 16];
        anmf[6..9].copy_from_slice(&(width - 1).to_le_bytes()[..3]);
        anmf[9..12].copy_from_slice(&(height - 1).to_le_bytes()[..3]);
        anmf[12..15].copy_from_slice(&100u32.to_le_bytes()[..3]);
        anmf.extend_from_slice(&riff_chunk(b"VP8L", &vp8l(frame)));
        body.extend_from_slice(&riff_chunk(b"ANMF", &anmf));
    }
    body.extend_from_slice(&riff_chunk(b"EXIF", &tiff_orientation(orientation)));
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&body);
    out
}

#[tokio::test]
async fn read_turns_an_animated_webp_first_frame_upright() {
    let workspace = Workspace::new();
    let reads = branch();
    let first = RgbaImage::from_fn(40, 20, |x, _| {
        if x < 20 {
            Rgba([255, 0, 0, 255])
        } else {
            Rgba([0, 0, 255, 255])
        }
    });
    let second = RgbaImage::from_pixel(40, 20, Rgba([0, 255, 0, 255]));
    workspace.put("anim.webp", &animated_webp([&first, &second], 6));
    let result = workspace.read(&reads, "anim.webp").await;
    assert_eq!(result.is_error, None);
    let (line, source) = image_blocks(&result);
    assert!(line.contains("(20x40 px)"), "{line}");
    assert!(
        line.contains("Animated WebP: only the first frame was sent."),
        "{line}"
    );
    assert!(line.contains("Sent 20x40 px as image/jpeg"), "{line}");
    let image = decoded(source);
    assert_eq!(image.dimensions(), (20, 40), "the sent pixels are upright");
    // The first frame is stored left half red, right half blue, rotated
    // clockwise like the JPEG orientation test.
    let top = image.get_pixel(0, 0);
    let bottom = image.get_pixel(0, 39);
    assert!(top[0] > 200 && top[2] < 60, "top {top:?}");
    assert!(bottom[2] > 200 && bottom[0] < 60, "bottom {bottom:?}");
    let structured = structured(&result);
    assert_eq!(structured["animated"], true);
    assert_eq!(structured["sentWidth"], 20);
    assert_eq!(structured["sentHeight"], 40);
}

#[tokio::test]
async fn read_sends_transparent_pixels_on_white_when_it_encodes_jpeg() {
    let workspace = Workspace::new();
    let reads = branch();
    let first = RgbaImage::from_fn(20, 20, |x, _| {
        if x < 10 {
            Rgba([255, 0, 0, 0])
        } else {
            Rgba([0, 0, 255, 255])
        }
    });
    let second = RgbaImage::from_pixel(20, 20, Rgba([0, 255, 0, 255]));
    workspace.put("clear.webp", &animated_webp([&first, &second], 1));
    let result = workspace.read(&reads, "clear.webp").await;
    assert_eq!(result.is_error, None);
    let (line, source) = image_blocks(&result);
    assert!(line.contains("as image/jpeg"), "{line}");
    let image = decoded(source);
    // JPEG has no alpha: a transparent pixel must not come out as its bare
    // colour or as black.
    let clear = image.get_pixel(0, 0);
    assert!(
        clear[0] > 240 && clear[1] > 240 && clear[2] > 240,
        "transparent pixel on white {clear:?}"
    );
    let opaque = image.get_pixel(19, 0);
    assert!(opaque[2] > 200 && opaque[0] < 60, "opaque pixel {opaque:?}");
}

/// An animated WebP whose EXIF chunk header claims 1 GiB but holds no bytes:
/// its declared size must not become an allocation.
fn webp_with_oversized_exif() -> Vec<u8> {
    let frame = RgbaImage::from_pixel(20, 20, Rgba([255, 0, 0, 255]));
    let mut body = Vec::new();
    let mut vp8x = vec![0x0A, 0, 0, 0];
    vp8x.extend_from_slice(&19u32.to_le_bytes()[..3]);
    vp8x.extend_from_slice(&19u32.to_le_bytes()[..3]);
    body.extend_from_slice(&riff_chunk(b"VP8X", &vp8x));
    body.extend_from_slice(&riff_chunk(b"ANIM", &[0, 0, 0, 0, 0, 0]));
    let mut anmf = vec![0; 16];
    anmf[6..9].copy_from_slice(&19u32.to_le_bytes()[..3]);
    anmf[9..12].copy_from_slice(&19u32.to_le_bytes()[..3]);
    anmf[12..15].copy_from_slice(&100u32.to_le_bytes()[..3]);
    anmf.extend_from_slice(&riff_chunk(b"VP8L", &vp8l(&frame)));
    body.extend_from_slice(&riff_chunk(b"ANMF", &anmf));
    body.extend_from_slice(b"EXIF");
    body.extend_from_slice(&0x4000_0000u32.to_le_bytes());
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&body);
    out
}

#[tokio::test]
async fn read_refuses_a_webp_exif_chunk_larger_than_the_file() {
    let workspace = Workspace::new();
    let reads = branch();
    workspace.put("exif.webp", &webp_with_oversized_exif());
    let result = workspace.read(&reads, "exif.webp").await;
    assert_eq!(code(&result), "binary_unsupported");
    assert!(text(&result).contains("EXIF metadata"), "{}", text(&result));
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (index, entry) in table.iter_mut().enumerate() {
        let mut value = index as u32;
        for _ in 0..8 {
            value = if value & 1 == 1 {
                0xEDB8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
        }
        *entry = value;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        crc = table[((crc ^ u32::from(*byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// A PNG header that claims `width` x `height` and holds no pixels.
fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut typed = b"IHDR".to_vec();
    typed.extend_from_slice(&ihdr);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    out.extend_from_slice(&13u32.to_be_bytes());
    out.extend_from_slice(&typed);
    out.extend_from_slice(&crc32(&typed).to_be_bytes());
    // A decoder needs an IDAT chunk present, not a decodable one: the guard
    // runs after the header and before the pixels are decoded.
    let mut idat = b"IDAT".to_vec();
    idat.extend_from_slice(&[0x78, 0x9c, 0x03, 0x00]);
    out.extend_from_slice(&4u32.to_be_bytes());
    out.extend_from_slice(&idat);
    out.extend_from_slice(&crc32(&idat).to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(b"IEND");
    out.extend_from_slice(&0xAE42_6082u32.to_be_bytes());
    out
}

#[tokio::test]
async fn read_refuses_a_header_above_the_pixel_guard_before_decoding() {
    let workspace = Workspace::new();
    let reads = branch();
    workspace.put("huge.png", &png_header(20_000, 20_000));
    let result = workspace.read(&reads, "huge.png").await;
    assert_eq!(code(&result), "binary_unsupported");
    let message = text(&result);
    assert!(message.contains("20000x20000"), "{message}");
    assert!(message.contains("Make a smaller copy"), "{message}");
}

/// The guard runs on the header, not on the file's length: a sparse tail
/// makes this file 1 GiB long, and the refusal still comes from the header.
#[tokio::test]
async fn read_refuses_a_sparse_file_before_reading_it() {
    let workspace = Workspace::new();
    let reads = branch();
    workspace.put("huge-sparse.png", &png_header(20_000, 20_000));
    std::fs::OpenOptions::new()
        .write(true)
        .open(workspace.path("huge-sparse.png"))
        .unwrap()
        .set_len(1 << 30)
        .unwrap();
    let result = workspace.read(&reads, "huge-sparse.png").await;
    assert_eq!(code(&result), "binary_unsupported");
    let message = text(&result);
    assert!(message.contains("20000x20000"), "{message}");
    assert!(message.contains("Make a smaller copy"), "{message}");
}

#[tokio::test]
async fn read_refuses_formats_it_cannot_decode_and_other_binaries() {
    let workspace = Workspace::new();
    let reads = branch();
    workspace.put(
        "photo.heic",
        b"\x00\x00\x00\x18ftypheic\x00\x00\x00\x00mif1heic",
    );
    let heic = workspace.read(&reads, "photo.heic").await;
    assert_eq!(code(&heic), "binary_unsupported");
    assert!(text(&heic).contains("HEIC/HEIF"), "{}", text(&heic));
    // The generic HEIF brand may hold HEVC or AVIF, so it is named for both.
    workspace.put(
        "photo.heif",
        b"\x00\x00\x00\x18ftypmif1\x00\x00\x00\x00mif1avif",
    );
    let heif = workspace.read(&reads, "photo.heif").await;
    assert_eq!(code(&heif), "binary_unsupported");
    assert!(text(&heif).contains("HEIF/AVIF"), "{}", text(&heif));
    workspace.put("archive.zip", b"PK\x03\x04");
    assert_eq!(
        code(&workspace.read(&reads, "archive.zip").await),
        "binary_unsupported"
    );
}
