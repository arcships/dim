#![cfg(any(unix, windows))]
use dim_agent_capabilities::file_storage::{EntryKind, FileStorage, FileStoragePlugin};
use dim_agent_runtime::Names;
use dim_app_server::plugin_files::{
    MAX_WHOLE_FILE_BYTES, PluginFileEntry, PluginFileError, PluginFileTransactions,
    PluginFilesPlugin,
};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis_bridge::session::{HostDispatch, Value};
use serde_json::json;
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/plugin-files-flow")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn resolved(value: Value) -> Value {
    match value {
        Value::Reference(reference) if reference.is_future() => {
            reference.wait_async().await.unwrap()
        }
        value => value,
    }
}
fn member(value: &Value, name: &str) -> Value {
    let Value::Record(fields) = value else {
        panic!("expected record")
    };
    fields[name].clone()
}
async fn call(value: &Value, name: &str, args: Vec<Value>) -> Value {
    resolved(
        member(value, name)
            .reference()
            .unwrap()
            .call_async(Value::List(args))
            .await
            .unwrap(),
    )
    .await
}
type Services = (Arc<PluginFileTransactions>, Arc<FileStorage>);
struct Capture {
    captured: Arc<OnceLock<Arc<dyn HostDispatch>>>,
    services: Arc<OnceLock<Services>>,
    files: Key<dyn HostDispatch>,
    transactions: Key<PluginFileTransactions>,
    storage: Key<FileStorage>,
}
impl DynPlugin for Capture {
    fn name(&self) -> &str {
        "capture-plugin-files"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.files.erase(),
            self.transactions.erase(),
            self.storage.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let files = ctx.require_at(&self.files)?;
            let _ = self.captured.set(files);
            let transactions = ctx.require_at(&self.transactions)?;
            let storage = ctx.require_at(&self.storage)?;
            let _ = self.services.set((transactions, storage));
            Ok(Effect::Done)
        })
    }
}
async fn mount(path: &Path) -> (Ctx, Value, Services) {
    let root = Ctx::root().unwrap();
    (&root.plugin(FileStoragePlugin::new(vec![path.to_path_buf()])))
        .await
        .unwrap();
    let catalog = dim_app_server::catalog();
    let build = rutis_loader::resolve::Build::default();
    let names = Names::copy(&catalog, &build);
    (&root.plugin(PluginFilesPlugin::new(&names, 0).unwrap()))
        .await
        .unwrap();
    let captured = Arc::new(OnceLock::new());
    let services = Arc::new(OnceLock::new());
    let key = names.named::<dyn HostDispatch>("pluginFiles").unwrap();
    (&root.plugin(Capture {
        captured: captured.clone(),
        services: services.clone(),
        files: key,
        transactions: names.key().unwrap(),
        storage: names.key().unwrap(),
    }))
        .await
        .unwrap();
    let services = services.get().unwrap().clone();
    let files = captured.get().unwrap();
    let scope = resolved(files.invoke("open", json!([]).into()).unwrap()).await;
    (root, scope, services)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transactions_keep_backups_private_and_revoke_escaped_handles() {
    let directory = Directory::new();
    let (root, scope, _) = mount(&directory.0).await;
    let escaped = Arc::new(Mutex::new(None));
    let captured = escaped.clone();
    let result = call(
        &scope,
        "withLock",
        vec![
            json!("state.json").into(),
            Value::callback(move |args| {
                let lease = args.list()?.remove(0);
                *captured.lock().unwrap() = Some(lease.clone());
                Ok(Value::future(async move {
                    assert_eq!(
                        call(&lease, "read", vec![]).await.json().unwrap(),
                        json!({"ok":true,"value":null})
                    );
                    assert_eq!(
                        call(&lease, "replace", vec![json!("original\n").into()])
                            .await
                            .json()
                            .unwrap()["ok"],
                        true
                    );
                    assert_eq!(
                        call(&lease, "read", vec![]).await.json().unwrap(),
                        json!({"ok":true,"value":"original\n"})
                    );
                    assert_eq!(
                        call(&lease, "backup", vec![json!("before-reset").into()])
                            .await
                            .json()
                            .unwrap()["ok"],
                        true
                    );
                    assert_eq!(
                        call(&lease, "replace", vec![json!("replacement\n").into()])
                            .await
                            .json()
                            .unwrap()["ok"],
                        true
                    );
                    assert_eq!(
                        call(&lease, "backup", vec![json!("before-reset").into()])
                            .await
                            .json()
                            .unwrap()["error"],
                        "exists"
                    );
                    assert_eq!(
                        call(&lease, "backup", vec![json!("../escape").into()])
                            .await
                            .json()
                            .unwrap()["error"],
                        "invalid_path"
                    );
                    Ok(json!("completed").into())
                }))
            }),
        ],
    )
    .await
    .json()
    .unwrap();
    assert_eq!(result, json!({"ok":true,"value":"completed"}));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(directory.0.join("state.json")).unwrap()
        )
        .unwrap(),
        json!({"version":1,"text":"replacement\n"})
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(directory.0.join("state.json.before-reset")).unwrap()
        )
        .unwrap(),
        json!({"version":1,"text":"original\n"})
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["state.json", "state.json.before-reset"] {
            assert_eq!(
                std::fs::metadata(directory.0.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    let escaped = escaped.lock().unwrap().take().unwrap();
    assert_eq!(
        call(&escaped, "replace", vec![json!("late").into()])
            .await
            .json()
            .unwrap()["error"],
        "closed"
    );
    for path in [
        "../outside",
        "/absolute",
        "auth.json.lock",
        ".dim-file-storage-locks/anything",
    ] {
        let result = call(
            &scope,
            "withLock",
            vec![
                json!(path).into(),
                Value::callback(|_| panic!("invalid path admitted")),
            ],
        )
        .await;
        assert_eq!(result.json().unwrap()["error"], "invalid_path");
    }
    call(&scope, "close", vec![]).await;
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn versioned_records_reopen_and_preserve_rejected_files() {
    let directory = Directory::new();
    let (root, scope, _) = mount(&directory.0).await;
    let text = "{\"version\":99,\"private\":\"fixture-secret\"}\n中文\t\0";
    let result = call(
        &scope,
        "withLock",
        vec![
            json!("state").into(),
            Value::callback(move |args| {
                let file = args.list()?.remove(0);
                Ok(Value::future(async move {
                    for text in ["", text] {
                        assert_eq!(
                            call(&file, "replace", vec![json!(text).into()])
                                .await
                                .json()
                                .unwrap(),
                            json!({"ok":true,"value":null})
                        );
                        assert_eq!(
                            call(&file, "read", vec![]).await.json().unwrap(),
                            json!({"ok":true,"value":text})
                        );
                    }
                    Ok(json!(null).into())
                }))
            }),
        ],
    )
    .await
    .json()
    .unwrap();
    assert_eq!(result, json!({"ok":true,"value":null}));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(directory.0.join("state")).unwrap()
        )
        .unwrap(),
        json!({"version":1,"text":text})
    );
    call(&scope, "close", vec![]).await;
    root.shutdown().await.unwrap();

    let (root, scope, _) = mount(&directory.0).await;
    let read = Value::callback(|args| {
        let file = args.list()?.remove(0);
        Ok(Value::future(async move {
            Ok(call(&file, "read", vec![]).await)
        }))
    });
    let reopened = call(
        &scope,
        "withLock",
        vec![json!("state").into(), read.clone()],
    )
    .await
    .json()
    .unwrap();
    assert_eq!(
        reopened,
        json!({"ok":true,"value":{"ok":true,"value":text}})
    );

    for (index, bytes) in [
        &b"unversioned fixture-secret"[..],
        br#"{"text":"missing version"}"#,
        br#"{"version":0,"text":"old"}"#,
        br#"{"version":2,"text":"future"}"#,
        br#"{"version":1.0,"text":"not an integer"}"#,
        br#"{"version":1}"#,
        br#"{"version":1,"text":null}"#,
        br#"{"version":1,"text":"extra","unknown":true}"#,
        br#"{"version":1,"version":1,"text":"duplicate"}"#,
        b"{",
        b"\xff",
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("rejected-{index}");
        let path = directory.0.join(&name);
        std::fs::write(&path, bytes).unwrap();
        let rejected = call(&scope, "withLock", vec![json!(name).into(), read.clone()])
            .await
            .json()
            .unwrap();
        assert_eq!(
            rejected,
            json!({"ok":true,"value":{"ok":false,"error":"invalid_data"}})
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let backup = call(
            &scope,
            "withLock",
            vec![
                json!(name).into(),
                Value::callback(|args| {
                    let file = args.list()?.remove(0);
                    Ok(Value::future(async move {
                        Ok(call(&file, "backup", vec![json!("preserved").into()]).await)
                    }))
                }),
            ],
        )
        .await
        .json()
        .unwrap();
        assert_eq!(backup, json!({"ok":true,"value":{"ok":true,"value":null}}));
        assert_eq!(
            std::fs::read(directory.0.join(format!("{name}.preserved"))).unwrap(),
            bytes
        );
    }
    call(&scope, "close", vec![]).await;
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quiesce_cancels_waiters_but_settles_issued_transactions_and_isolates_scopes() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let directory = Directory::new();
        let (root, scope, _) = mount(&directory.0).await;
        let (other_root, other, _) = mount(&directory.0).await;
        let entered = Arc::new(Notify::new());
        let resume = Arc::new(Notify::new());
        let (ready, proceed) = (entered.clone(), resume.clone());
        let active = scope.clone();
        let task = tokio::spawn(async move {
            call(
                &active,
                "withLock",
                vec![
                    json!("auth.json").into(),
                    Value::callback(move |args| {
                        let lease = args.list()?.remove(0);
                        let (ready, proceed) = (ready.clone(), proceed.clone());
                        Ok(Value::future(async move {
                            ready.notify_one();
                            proceed.notified().await;
                            Ok(call(&lease, "replace", vec![json!("rotated").into()]).await)
                        }))
                    }),
                ],
            )
            .await
        });
        entered.notified().await;
        let cancelled = call(
            &other,
            "withLock",
            vec![
                json!("auth.json").into(),
                Value::callback(|_| panic!("cancelled acquisition entered locked file")),
                Value::future(async { Ok(json!(null).into()) }),
            ],
        )
        .await;
        assert_eq!(cancelled.json().unwrap()["error"], "closed");
        let waiting = other.clone();
        let waiter = tokio::spawn(async move {
            call(
                &waiting,
                "withLock",
                vec![
                    json!("auth.json").into(),
                    Value::callback(|_| panic!("waiter entered locked file")),
                ],
            )
            .await
        });
        call(&other, "quiesce", vec![]).await;
        assert_eq!(waiter.await.unwrap().json().unwrap()["error"], "closed");
        call(&other, "close", vec![]).await;
        call(&scope, "quiesce", vec![]).await;
        assert!(!task.is_finished());
        resume.notify_one();
        assert_eq!(task.await.unwrap().json().unwrap()["value"]["ok"], true);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &std::fs::read(directory.0.join("auth.json")).unwrap()
            )
            .unwrap(),
            json!({"version":1,"text":"rotated"})
        );
        call(&scope, "close", vec![]).await;
        root.shutdown().await.unwrap();
        other_root.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_stale_lock_reclaimers_serialize_through_native_ownership() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let directory = Directory::new();
        let lock = directory.0.join("auth.json.lock");
        std::fs::write(&lock, b"2147483647\n0\nstale\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(lock)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(60)),
            )
            .unwrap();
        std::fs::write(
            directory.0.join("auth.json"),
            br#"{"version":1,"text":"0"}"#,
        )
        .unwrap();
        let (root, a, _) = mount(&directory.0).await;
        let (other_root, b, _) = mount(&directory.0).await;
        let increment = |scope: Value, amount: u64| async move {
            call(
                &scope,
                "withLock",
                vec![
                    json!("auth.json").into(),
                    Value::callback(move |args| {
                        let file = args.list()?.remove(0);
                        Ok(Value::future(async move {
                            let value = call(&file, "read", vec![]).await.json().unwrap()["value"]
                                .as_str()
                                .unwrap()
                                .parse::<u64>()
                                .unwrap();
                            tokio::time::sleep(Duration::from_millis(30)).await;
                            Ok(call(
                                &file,
                                "replace",
                                vec![json!((value + amount).to_string()).into()],
                            )
                            .await)
                        }))
                    }),
                ],
            )
            .await
        };
        let (first, second) = tokio::join!(increment(a.clone(), 17), increment(b.clone(), 31));
        assert_eq!(first.json().unwrap()["value"]["ok"], true);
        assert_eq!(second.json().unwrap()["value"]["ok"], true);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &std::fs::read(directory.0.join("auth.json")).unwrap()
            )
            .unwrap(),
            json!({"version":1,"text":"48"})
        );
        call(&a, "close", vec![]).await;
        call(&b, "close", vec![]).await;
        root.shutdown().await.unwrap();
        other_root.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn binary_files_round_trip_and_stream_without_an_envelope() {
    let directory = Directory::new();
    let (root, _, (files, _)) = mount(&directory.0).await;
    let cancel = CancellationToken::new();
    files.directory("media").await.unwrap();

    let bytes: Vec<u8> = (0..200_003u32).map(|i| (i % 251) as u8).collect();
    files
        .write_stream("media/blob.bin", std::io::Cursor::new(bytes.clone()))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(directory.0.join("media/blob.bin")).unwrap(),
        bytes
    );
    assert_eq!(
        files.read_bytes("media/blob.bin").await.unwrap(),
        Some(bytes.clone())
    );
    assert_eq!(files.read_bytes("media/missing.bin").await.unwrap(), None);

    let reader = files.open_read("media/blob.bin").await.unwrap().unwrap();
    assert_eq!(reader.len(), bytes.len() as u64);
    let mut streamed = Vec::new();
    loop {
        let chunk = reader
            .read_at(streamed.len() as u64, usize::MAX, &cancel)
            .await
            .unwrap();
        if chunk.is_empty() {
            break;
        }
        streamed.extend_from_slice(&chunk);
    }
    assert_eq!(streamed, bytes);
    assert!(
        files
            .open_read("media/missing.bin")
            .await
            .unwrap()
            .is_none()
    );

    files
        .transact("media/text.json", |_| (Some("inner".to_owned()), ()))
        .await
        .unwrap();
    assert_eq!(
        files.read_bytes("media/text.json").await.unwrap().unwrap(),
        br#"{"version":1,"text":"inner"}"#
    );
    assert_eq!(
        files
            .transact("media/blob.bin", |current| (
                None,
                current.map(str::to_owned)
            ))
            .await,
        Err(PluginFileError::InvalidData)
    );
    assert_eq!(
        std::fs::read(directory.0.join("media/blob.bin")).unwrap(),
        bytes
    );

    let oversized = directory.0.join("media/oversized.bin");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len(MAX_WHOLE_FILE_BYTES as u64 + 1)
        .unwrap();
    assert_eq!(
        files.read_bytes("media/oversized.bin").await,
        Err(PluginFileError::InvalidData)
    );
    assert_eq!(
        std::fs::metadata(&oversized).unwrap().len(),
        MAX_WHOLE_FILE_BYTES as u64 + 1
    );
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_streaming_write_keeps_the_old_file_and_leaves_no_temporary() {
    let directory = Directory::new();
    let (root, _, (files, _)) = mount(&directory.0).await;
    files.directory("media").await.unwrap();
    files
        .write_stream("media/blob.bin", std::io::Cursor::new(b"original".to_vec()))
        .await
        .unwrap();

    struct GatedSource {
        first: Option<Vec<u8>>,
        blocked: Option<tokio::sync::oneshot::Sender<()>>,
        released: std::sync::mpsc::Receiver<Vec<u8>>,
    }
    impl GatedSource {
        fn fill(bytes: Vec<u8>, buffer: &mut [u8]) -> usize {
            let count = bytes.len().min(buffer.len());
            buffer[..count].copy_from_slice(&bytes[..count]);
            count
        }
    }
    impl Read for GatedSource {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if let Some(bytes) = self.first.take() {
                return Ok(Self::fill(bytes, buffer));
            }
            if let Some(blocked) = self.blocked.take() {
                let _ = blocked.send(());
            }
            match self.released.recv() {
                Ok(bytes) => Ok(Self::fill(bytes, buffer)),
                Err(_) => Ok(0),
            }
        }
    }

    let (blocked, writing) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let source = GatedSource {
        first: Some(vec![1; 4096]),
        blocked: Some(blocked),
        released,
    };
    let writer = files.clone();
    let write = tokio::spawn(async move { writer.write_stream("media/blob.bin", source).await });
    writing.await.unwrap();
    let stopped = root.shutdown();
    tokio::pin!(stopped);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut stopped)
            .await
            .is_err()
    );
    release.send(vec![2; 4096]).unwrap();
    assert_eq!(write.await.unwrap(), Err(PluginFileError::Closed));
    stopped.await.unwrap();
    assert_eq!(
        std::fs::read(directory.0.join("media/blob.bin")).unwrap(),
        b"original"
    );
    assert_eq!(
        std::fs::read_dir(
            directory
                .0
                .join("media/.dim-file-storage-locks/.dim-file-storage-temps")
        )
        .unwrap()
        .count(),
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_remove_and_work_directories_follow_the_root_and_its_leases() {
    let directory = Directory::new();
    let (root, _, (files, storage)) = mount(&directory.0).await;
    let cancel = CancellationToken::new();

    let work = files.directory("media/tasks/a1/result").await.unwrap();
    assert_eq!(work, directory.0.join("media/tasks/a1/result"));
    assert!(work.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for created in [
            "media",
            "media/tasks",
            "media/tasks/a1",
            "media/tasks/a1/result",
        ] {
            assert_eq!(
                std::fs::metadata(directory.0.join(created))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700,
                "{created}"
            );
        }
        std::fs::create_dir(directory.0.join("media/real")).unwrap();
        std::os::unix::fs::symlink(
            directory.0.join("media/real"),
            directory.0.join("media/link"),
        )
        .unwrap();
        assert_eq!(
            files.directory("media/link/work").await,
            Err(PluginFileError::InvalidPath)
        );
        assert!(!directory.0.join("media/real/work").exists());
    }

    files
        .write_stream(
            "media/tasks/a1/result/out.bin",
            std::io::Cursor::new(b"result".to_vec()),
        )
        .await
        .unwrap();
    files
        .transact("media/tasks/a1/task.json", |_| {
            (Some("record".to_owned()), ())
        })
        .await
        .unwrap();
    let entries = files.list("media/tasks/a1").await.unwrap();
    assert_eq!(
        entries,
        [
            PluginFileEntry {
                name: "media/tasks/a1/result".to_owned(),
                kind: EntryKind::Directory,
            },
            PluginFileEntry {
                name: "media/tasks/a1/task.json".to_owned(),
                kind: EntryKind::File,
            },
        ]
    );
    assert_eq!(files.list("media/tasks/a1/").await.unwrap(), entries);
    assert_eq!(
        files.list("media/tasks/a1/result").await.unwrap(),
        [PluginFileEntry {
            name: "media/tasks/a1/result/out.bin".to_owned(),
            kind: EntryKind::File,
        }]
    );

    let path = storage.path(0, "media/tasks/a1/result/out.bin").unwrap();
    let lease = storage.lock(&path, &cancel).await.unwrap();
    assert_eq!(
        files.remove("media/tasks/a1").await,
        Err(PluginFileError::Locked)
    );
    drop(lease);
    files.remove("media/tasks/a1").await.unwrap();
    assert!(!directory.0.join("media/tasks/a1").exists());
    assert_eq!(files.list("media/tasks").await.unwrap(), []);
    files.remove("media/tasks/a1").await.unwrap();

    for name in ["../escape", "/absolute", ".dim-file-storage-locks/work"] {
        assert_eq!(
            files.list(name).await,
            Err(PluginFileError::InvalidPath),
            "{name}"
        );
        assert_eq!(
            files.remove(name).await,
            Err(PluginFileError::InvalidPath),
            "{name}"
        );
    }
    assert_eq!(files.list("").await, Err(PluginFileError::InvalidPath));
    root.shutdown().await.unwrap();
}

/// On Unix a backslash is an ordinary byte in a name, so `a\b` and `a/b` are
/// different directories. A listed name is built from the components the
/// storage resolved, so removing the returned name cannot reach the other file.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_backslash_in_a_listed_name_stays_inside_its_component() {
    let directory = Directory::new();
    let (root, _, (files, _)) = mount(&directory.0).await;

    std::fs::create_dir_all(directory.0.join("a\\b")).unwrap();
    std::fs::create_dir_all(directory.0.join("a/b")).unwrap();
    std::fs::write(directory.0.join("a\\b/item"), b"backslash").unwrap();
    std::fs::write(directory.0.join("a/b/item"), b"slash").unwrap();

    let backslash = files.list("a\\b").await.unwrap();
    assert_eq!(
        backslash,
        [PluginFileEntry {
            name: "a\\b/item".to_owned(),
            kind: EntryKind::File,
        }]
    );
    assert_eq!(
        files.list("a/b").await.unwrap(),
        [PluginFileEntry {
            name: "a/b/item".to_owned(),
            kind: EntryKind::File,
        }]
    );

    files.remove(&backslash[0].name).await.unwrap();
    assert!(!directory.0.join("a\\b/item").exists());
    assert_eq!(
        std::fs::read(directory.0.join("a/b/item")).unwrap(),
        b"slash"
    );

    root.shutdown().await.unwrap();
}
