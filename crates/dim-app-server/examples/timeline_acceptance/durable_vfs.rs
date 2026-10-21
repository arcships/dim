//! Measurement-only VFS: recover only bytes covered by a completed xSync.
//! This models truthful fsync and discarded unsynced writes, not drive firmware.
use rusqlite::ffi;
use std::{
    collections::HashMap,
    ffi::CStr,
    io::Write,
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Instant,
};

struct File {
    original: usize,
    wrapper: usize,
    path: Option<PathBuf>,
}
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static FILES: OnceLock<Mutex<HashMap<usize, File>>> = OnceLock::new();
static IMAGES: OnceLock<PathBuf> = OnceLock::new();
static INTERRUPT: AtomicBool = AtomicBool::new(false);
static WAL_BYTES: AtomicU64 = AtomicU64::new(0);
static WAL_SYNCS: Mutex<Vec<Instant>> = Mutex::new(Vec::new());
static PLAIN_FSYNC: OnceLock<bool> = OnceLock::new();

pub fn install(images: PathBuf) {
    std::fs::create_dir_all(&images).unwrap();
    IMAGES.set(images).unwrap();
    install_tracking();
}

pub fn install_tracking() {
    FILES.set(Mutex::new(HashMap::new())).ok().unwrap();
    // The delegated VFS owns the native file layout. Only three I/O callbacks
    // change; every other operation uses the original callback and file pointer.
    unsafe {
        assert_eq!(ffi::sqlite3_initialize(), ffi::SQLITE_OK);
        let original = ffi::sqlite3_vfs_find(std::ptr::null());
        assert!(!original.is_null());
        ORIGINAL.store(original as usize, Ordering::SeqCst);
        let mut wrapper = Box::new(*original);
        wrapper.zName = c"acceptance-durable".as_ptr();
        wrapper.xOpen = Some(open);
        assert_eq!(
            ffi::sqlite3_vfs_register(Box::into_raw(wrapper), 1),
            ffi::SQLITE_OK
        );
    }
}

pub fn interrupt_next_wal_sync() {
    INTERRUPT.store(true, Ordering::SeqCst);
}

pub fn reset_measurement() {
    WAL_BYTES.store(0, Ordering::SeqCst);
    WAL_SYNCS.lock().unwrap().clear();
}

/// The WAL syncs since the last reset.
pub fn wal_syncs() -> usize {
    WAL_SYNCS.lock().unwrap().len()
}

pub fn measurement() -> (u64, Vec<Instant>) {
    (
        WAL_BYTES.load(Ordering::SeqCst),
        WAL_SYNCS.lock().unwrap().clone(),
    )
}

unsafe extern "C" fn write(
    file: *mut ffi::sqlite3_file,
    buffer: *const std::ffi::c_void,
    amount: i32,
    offset: i64,
) -> i32 {
    let (original, wal) = {
        let files = FILES.get().unwrap().lock().unwrap();
        let tracked = files.get(&(file as usize)).unwrap();
        (
            tracked.original,
            tracked
                .path
                .as_ref()
                .is_some_and(|p| p.to_string_lossy().ends_with("-wal")),
        )
    };
    let result = unsafe {
        (*(original as *const ffi::sqlite3_io_methods))
            .xWrite
            .unwrap()(file, buffer, amount, offset)
    };
    if wal && result == ffi::SQLITE_OK {
        WAL_BYTES.fetch_add(amount as u64, Ordering::Relaxed);
    }
    result
}

unsafe extern "C" fn open(
    _: *mut ffi::sqlite3_vfs,
    name: ffi::sqlite3_filename,
    file: *mut ffi::sqlite3_file,
    flags: i32,
    output: *mut i32,
) -> i32 {
    unsafe {
        let vfs = ORIGINAL.load(Ordering::SeqCst) as *mut ffi::sqlite3_vfs;
        let result = (*vfs).xOpen.unwrap()(vfs, name, file, flags, output);
        if result == ffi::SQLITE_OK {
            let original = (*file).pMethods;
            let mut methods = Box::new(*original);
            methods.xSync = Some(sync);
            methods.xClose = Some(close);
            methods.xWrite = Some(write);
            let wrapper = Box::into_raw(methods);
            let path = (!name.is_null())
                .then(|| PathBuf::from(CStr::from_ptr(name).to_str().expect("measurement path")));
            FILES.get().unwrap().lock().unwrap().insert(
                file as usize,
                File {
                    original: original as usize,
                    wrapper: wrapper as usize,
                    path,
                },
            );
            (*file).pMethods = wrapper;
        }
        result
    }
}

unsafe extern "C" fn sync(file: *mut ffi::sqlite3_file, flags: i32) -> i32 {
    let (original, path) = {
        let files = FILES.get().unwrap().lock().unwrap();
        let tracked = files.get(&(file as usize)).unwrap();
        (tracked.original, tracked.path.clone())
    };
    if path
        .as_ref()
        .is_some_and(|p| p.to_string_lossy().ends_with("-wal"))
        && INTERRUPT.swap(false, Ordering::SeqCst)
    {
        println!("WAL_WRITTEN_BEFORE_SYNC");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }
    // `TIMELINE_ACCEPTANCE_PLAIN_FSYNC` turns macOS F_FULLFSYNC into a plain
    // fsync, a model of a disk whose sync is fast; SQLite uses F_FULLFSYNC
    // nowhere else.
    let flags = if flags & 0x0f == ffi::SQLITE_SYNC_FULL
        && *PLAIN_FSYNC
            .get_or_init(|| std::env::var_os("TIMELINE_ACCEPTANCE_PLAIN_FSYNC").is_some())
    {
        (flags & !0x0f) | ffi::SQLITE_SYNC_NORMAL
    } else {
        flags
    };
    let result = unsafe {
        (*(original as *const ffi::sqlite3_io_methods))
            .xSync
            .unwrap()(file, flags)
    };
    if result == ffi::SQLITE_OK
        && let Some(path) = path
    {
        if path.to_string_lossy().ends_with("-wal") {
            WAL_SYNCS.lock().unwrap().push(Instant::now());
        }
        let Some(images) = IMAGES.get() else {
            return result;
        };
        std::fs::copy(&path, images.join(path.file_name().unwrap())).unwrap();
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(images.join("sync-log.jsonl"))
            .unwrap();
        writeln!(log, "{}", serde_json::json!({"file":path,"flags":flags,"bytes":std::fs::metadata(path).unwrap().len()})).unwrap();
    }
    result
}

unsafe extern "C" fn close(file: *mut ffi::sqlite3_file) -> i32 {
    let tracked = FILES
        .get()
        .unwrap()
        .lock()
        .unwrap()
        .remove(&(file as usize))
        .unwrap();
    unsafe {
        (*file).pMethods = tracked.original as *const ffi::sqlite3_io_methods;
        let result = (*(*file).pMethods).xClose.unwrap()(file);
        drop(Box::from_raw(
            tracked.wrapper as *mut ffi::sqlite3_io_methods,
        ));
        result
    }
}
