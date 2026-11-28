use dim_agent_capabilities::methods::RpcError;
use std::path::PathBuf;

pub(crate) struct UploadStaging {
    pub(crate) path: PathBuf,
    _lock: std::fs::File,
    _temporary: Option<tempfile::TempDir>,
}
impl UploadStaging {
    pub(super) fn directory(database: &std::path::Path) -> PathBuf {
        let mut path = database.as_os_str().to_owned();
        path.push(".web-uploads");
        path.into()
    }

    pub(crate) fn open(database: Option<PathBuf>) -> Result<Self, RpcError> {
        let temporary = if database.is_none() {
            Some(tempfile::tempdir().map_err(|e| RpcError::invalid(e.to_string()))?)
        } else {
            None
        };
        let path = match database {
            Some(database) => Self::directory(&database),
            None => temporary
                .as_ref()
                .expect("temporary staging")
                .path()
                .to_owned(),
        };
        let open = || -> std::io::Result<std::fs::File> {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            if !std::fs::symlink_metadata(&path)?.is_dir() {
                return Err(std::io::Error::other("Upload staging is not a directory"));
            }
            let mut options = std::fs::OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            let lock = options.open(path.join("lock"))?;
            lock.try_lock().map_err(std::io::Error::other)?;
            for entry in std::fs::read_dir(&path)? {
                let entry = entry?;
                if entry.file_name().to_string_lossy().starts_with("upload-") {
                    if entry.file_type()?.is_dir() {
                        std::fs::remove_dir_all(entry.path())?;
                    } else {
                        std::fs::remove_file(entry.path())?;
                    }
                }
            }
            Ok(lock)
        };
        let lock = open().map_err(|e| RpcError::invalid(e.to_string()))?;
        Ok(Self {
            path,
            _lock: lock,
            _temporary: temporary,
        })
    }
}
