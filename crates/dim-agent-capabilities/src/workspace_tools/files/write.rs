//! `write` (TS `write.ts`) and staged replacement shared with `edit`.

use super::super::file_reads::{
    Bom, EncodingSource, FileReads, TextEncoding, TextFileBinding, WriteBinding,
};
use super::codec;
use super::edit::unified_diff;
use super::{
    Access, Cancelled, FileTools, NotRegular, PathError, Target, TargetError, blocking, cancelled,
    failed, field, hex, invalid_input, not_regular, refused, resolve_path, revision, wrote,
};
use crate::host::HostError;
use dim_agent_types::{JsonObject, ToolEffect, ToolResult};
use serde_json::{Value, json};
use std::{
    io::{ErrorKind, Read, Write},
    path::Path,
};
use tokio_util::sync::CancellationToken;

const WRITE_DIFF_MAX_BYTES: usize = 64 * 1024;

pub(super) fn definition() -> Value {
    json!({
        "name": "write",
        "description": "Write content to a file. Creates the file if it does not exist, or overwrites the entire content if it does.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path to write." },
                "content": { "type": "string", "description": "Text content to write." },
            },
            "required": ["path", "content"],
        },
        "source": "builtin",
        "risk": "write_filesystem",
        "fileChanges": {
            "preExecutePaths": { "selector": "path" },
            "postExecutePaths": "path",
        },
    })
}

pub(super) async fn run(
    tools: &FileTools,
    input: &JsonObject,
    reads: &FileReads,
    cancel: CancellationToken,
) -> Result<ToolResult, HostError> {
    let Some(Value::String(display)) =
        field(input, "path").filter(|path| path.as_str() != Some(""))
    else {
        return Ok(invalid_input("write requires a non-empty string path."));
    };
    let Some(Value::String(content)) = field(input, "content") else {
        return Ok(invalid_input("write requires a string content."));
    };
    let established = reads.established();
    let (cwd, home, requested, boundary) = (
        tools.cwd.clone(),
        tools.home.clone(),
        display.clone(),
        tools.boundary.clone(),
    );
    let path = match blocking(move || {
        resolve_path(&cwd, &home, boundary.as_ref(), Access::Write, &requested)
    })
    .await?
    {
        Ok(path) => path,
        Err(PathError::Refused(path)) => return Ok(refused(&path)),
        Err(PathError::Message(message)) => return Ok(invalid_input(message)),
    };
    #[cfg(test)]
    if let Some(hook) = &tools.before_lock {
        hook(&path);
    }
    let _lock = tools.locks.lock(&path, &cancel).await?;
    let binding = reads.write_binding(&path).await?;
    let (display, content, target, bounded) = (
        display.clone(),
        content.clone(),
        path.clone(),
        tools.boundary.is_some(),
    );
    #[cfg(test)]
    let hook = tools.before_commit.clone();
    let (_lock, written) = blocking(move || {
        let written = write_locked(
            &target,
            bounded,
            &display,
            &content,
            binding,
            &cancel,
            #[cfg(test)]
            hook,
        );
        (_lock, written)
    })
    .await?;
    let (result, binding) = written.map_err(|Cancelled| cancelled())?;
    if let Some(binding) = binding {
        reads.set(path, binding, established);
    }
    Ok(result)
}

fn write_locked(
    path: &Path,
    bounded: bool,
    display: &str,
    content: &str,
    binding: WriteBinding,
    cancel: &CancellationToken,
    #[cfg(test)] hook: Option<super::CommitHook>,
) -> Result<(ToolResult, Option<TextFileBinding>), Cancelled> {
    let done = |result| Ok((result, None));
    let target = match Target::for_mutation(path, bounded, true) {
        Ok(target) => target,
        Err(TargetError::Refused) => return done(refused(path)),
        Err(TargetError::Io(error)) => {
            return done(failed(
                format!("Failed to open the target's parent directory: {error}"),
                json!({ "code": "mkdir_error" }),
            ));
        }
    };
    Cancelled::check(cancel)?;
    let exists = match target.stat() {
        Ok(Some(stat)) if stat.directory => {
            return done(failed(
                format!("Cannot write to directory: {display}"),
                json!({ "code": "is_directory" }),
            ));
        }
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(TargetError::Refused) => return done(refused(path)),
        Err(TargetError::Io(error)) => {
            return done(failed(
                format!("Failed to stat file: {error}"),
                json!({ "code": "stat_error" }),
            ));
        }
    };
    Cancelled::check(cancel)?;
    let original = if exists {
        match target.open_regular() {
            Ok(Ok(file)) => Some(file),
            Ok(Err(NotRegular)) => return done(not_regular(display)),
            Err(TargetError::Refused) => return done(refused(path)),
            Err(TargetError::Io(error)) => return done(write_failure(&error, display, false)),
        }
    } else {
        None
    };
    let (binding, before, text) = if exists {
        let binding = match binding {
            WriteBinding::None => {
                return done(failed(
                    "Read the existing file before overwriting it so its encoding and line endings can be preserved.",
                    json!({ "code": "text_binding_required" }),
                ));
            }
            WriteBinding::Compacted => {
                return done(failed(
                    "What you last read or wrote of this file was compacted out of the context. Read the file again (a part is enough) or use edit.",
                    json!({ "code": "text_binding_required" }),
                ));
            }
            WriteBinding::Text(binding) => binding,
        };
        let bytes = match validated_bytes(&target, &binding.raw_revision, display) {
            Ok(bytes) => bytes,
            Err(conflict) => return done(conflict),
        };
        Cancelled::check(cancel)?;
        let before = if bytes.len() <= WRITE_DIFF_MAX_BYTES {
            match codec::decode_text(&bytes, &binding) {
                Ok(text) => Some(text),
                Err(error) => return done(error.into_result()),
            }
        } else {
            None
        };
        let text = codec::normalize_eol(content, binding.eol);
        (binding, before, text)
    } else {
        let binding = TextFileBinding {
            raw_revision: [0; 32],
            encoding: TextEncoding::Utf8,
            encoding_source: EncodingSource::Utf8Policy,
            bom: Bom::None,
            eol: codec::detect_eol(content),
        };
        (binding, Some(String::new()), content.to_owned())
    };
    let bytes = match codec::encode_text(&text, &binding) {
        Ok(bytes) => bytes,
        Err(error) => return done(error.into_result()),
    };
    Cancelled::check(cancel)?;
    let diff = before
        .filter(|before| before.len().saturating_add(text.len()) <= WRITE_DIFF_MAX_BYTES)
        .map(|before| {
            format!(
                "--- {}\n+++ {display}\n{}",
                if exists { display } else { "/dev/null" },
                unified_diff(&before.replace("\r\n", "\n"), &text.replace("\r\n", "\n")),
            )
        })
        .filter(|diff| diff.len() <= WRITE_DIFF_MAX_BYTES);
    Cancelled::check(cancel)?;
    // Path-based writes create missing parents here, as they always did; the
    // bounded target created them during its walk, relative to the handles.
    if let Target::Path(target_path) = &target
        && let Some(parent) = target_path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        return done(failed(
            format!("Failed to create parent directory: {error}"),
            json!({ "code": "mkdir_error" }),
        ));
    }
    Cancelled::check(cancel)?;
    let written = write_target(
        &target,
        &bytes,
        display,
        original.as_ref(),
        &binding.raw_revision,
        cancel,
        #[cfg(test)]
        hook,
    )?;
    if let Err(result) = written {
        return done(result);
    }
    let raw_revision = revision(&bytes);
    let result = wrote(
        if diff.is_some() {
            format!("Wrote {display}")
        } else {
            format!("Wrote {display}\nBefore/after comparison omitted: size limit exceeded.")
        },
        json!({ "path": display, "bytesWritten": bytes.len(), "revision": hex(&raw_revision), "diff": diff }),
        display,
    );
    let binding = TextFileBinding {
        raw_revision,
        eol: codec::detect_eol(&text),
        ..binding
    };
    Ok((result, Some(binding)))
}

/// Fails closed: anything but the recorded bytes is a conflict.
fn validated_bytes(
    target: &Target,
    expected: &[u8; 32],
    display: &str,
) -> Result<Vec<u8>, ToolResult> {
    let modified = || {
        failed(
            "File was modified after it was last read. Use the read tool again before writing.",
            json!({ "code": "file_modified_since_read" }),
        )
    };
    let mut bytes = Vec::new();
    let read = match target.open_regular() {
        Ok(Ok(mut file)) => file.read_to_end(&mut bytes),
        Ok(Err(NotRegular)) => return Err(not_regular(display)),
        Err(TargetError::Refused) => return Err(refused(target.path())),
        Err(TargetError::Io(error)) => Err(error),
    };
    match read {
        Ok(_) if revision(&bytes) == *expected => Ok(bytes),
        Err(error) if error.kind() == ErrorKind::PermissionDenied => Err(failed(
            format!("Permission denied: {display}"),
            json!({ "code": "permission_denied" }),
        )),
        _ => Err(modified()),
    }
}

/// Stages complete bytes before touching the destination. Verification is not
/// portable CAS: uncoordinated path/parent renames or metadata changes can still
/// race the final syscall. Windows installs the owned handle, not a temp name.
///
/// With a boundary the target is a parent-directory handle, and the staged
/// sibling, the verification and the install all run relative to it.
///
/// Cancellation is checked before the publish. After the publish the function
/// must not return `Cancelled`: the destination may already have changed.
pub(super) fn write_target(
    target: &Target,
    bytes: &[u8],
    display: &str,
    original: Option<&std::fs::File>,
    expected: &[u8; 32],
    cancel: &CancellationToken,
    #[cfg(test)] hook: Option<super::CommitHook>,
) -> Result<Result<(), ToolResult>, Cancelled> {
    let path = target.path();
    if let Some(original) = original {
        let writable = match target.open_writable() {
            Ok(file) => file,
            Err(TargetError::Refused) => return Ok(Err(refused(path))),
            Err(TargetError::Io(error)) => return Ok(Err(write_failure(&error, display, false))),
        };
        if !same_identity(original, &writable).unwrap_or(false) {
            return Ok(Err(failed(
                "File was replaced during preparation. Read it again.",
                json!({"code": "file_modified_since_read"}),
            )));
        }
    }
    let metadata_before = match original.map(metadata_revision).transpose() {
        Ok(value) => value,
        Err(error) => return Ok(Err(write_failure(&error, display, false))),
    };
    #[cfg(windows)]
    if original.is_some_and(|file| file.metadata().is_ok_and(|m| m.permissions().readonly())) {
        return Ok(Err(write_failure(
            &ErrorKind::PermissionDenied.into(),
            display,
            false,
        )));
    }
    struct Temporary<'a> {
        target: &'a Target,
        name: std::ffi::OsString,
        file: std::fs::File,
    }
    impl Drop for Temporary<'_> {
        fn drop(&mut self) {
            if self.target.staged_owned(&self.name, &self.file) {
                #[cfg(windows)]
                if let Ok(metadata) = self.file.metadata() {
                    let mut permissions = metadata.permissions();
                    permissions.set_readonly(false);
                    let _ = self.file.set_permissions(permissions);
                }
                self.target.remove_staged(&self.name);
            }
        }
    }
    let staged = std::ffi::OsString::from(format!(".dim-write-{}", uuid::Uuid::new_v4()));
    let file = match target.create_staged(&staged) {
        Ok(file) => file,
        Err(TargetError::Refused) => return Ok(Err(refused(path))),
        Err(TargetError::Io(error)) => return Ok(Err(write_failure(&error, display, false))),
    };
    let mut temporary = Temporary {
        target,
        name: staged.clone(),
        file,
    };
    let prepare = (|| -> std::io::Result<()> {
        temporary.file.write_all(bytes)?;
        if let Some(original) = original {
            #[cfg(unix)]
            {
                use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
                let metadata = original.metadata()?;
                let staged = temporary.file.metadata()?;
                if (metadata.uid(), metadata.gid()) != (staged.uid(), staged.gid())
                    && unsafe {
                        libc::fchown(temporary.file.as_raw_fd(), metadata.uid(), metadata.gid())
                    } != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            temporary
                .file
                .set_permissions(original.metadata()?.permissions())?;
            preserve_metadata(original, &temporary.file)?;
        }
        temporary.file.sync_all()
    })();
    if let Err(error) = prepare {
        return Ok(Err(write_failure(&error, display, false)));
    }
    #[cfg(test)]
    if let Some(hook) = hook {
        hook(path);
    }
    Cancelled::check(cancel)?;
    if let Some(original) = original {
        if let Err(conflict) = validated_bytes(target, expected, display) {
            return Ok(Err(conflict));
        }
        let same = target
            .open_regular()
            .ok()
            .and_then(Result::ok)
            .is_some_and(|current| same_identity(original, &current).unwrap_or(false))
            && metadata_revision(original).ok().as_ref() == metadata_before.as_ref()
            && target.final_regular()
            && original
                .metadata()
                .and_then(|metadata| {
                    temporary
                        .file
                        .metadata()
                        .map(|staged| metadata.permissions() == staged.permissions())
                })
                .unwrap_or(false);
        if !same {
            return Ok(Err(failed(
                "File was replaced during preparation. Read it again.",
                json!({"code": "file_modified_since_read"}),
            )));
        }
        #[cfg(windows)]
        if metadata_revision(&temporary.file).ok().as_ref() != metadata_before.as_ref() {
            return Ok(Err(failed(
                "Cannot preserve the file's owner, group or access control during replacement.",
                json!({"code": "write_error"}),
            )));
        }
    }
    Cancelled::check(cancel)?;
    if !target.staged_owned(&staged, &temporary.file) {
        return Ok(Err(failed(
            "Staged file was replaced during preparation.",
            json!({"code": "write_error"}),
        )));
    }
    let committed = target.install(&staged, &temporary.file, original.is_some());
    if let Err(error) = committed {
        return Ok(Err(match error {
            TargetError::Refused => refused(path),
            TargetError::Io(error)
                if original.is_none() && error.kind() == ErrorKind::AlreadyExists =>
            {
                failed(
                    "Read the existing file before overwriting it.",
                    json!({"code": "text_binding_required"}),
                )
            }
            TargetError::Io(error) => write_failure(&error, display, false),
        }));
    }
    drop(temporary);
    #[cfg(unix)]
    if let Err(error) = target.sync_parent() {
        return Ok(Err(match error {
            TargetError::Refused => refused(path),
            TargetError::Io(error) => write_failure(&error, display, true),
        }));
    }
    Ok(Ok(()))
}

#[cfg(unix)]
fn metadata_revision(file: &std::fs::File) -> std::io::Result<Vec<u8>> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata()?;
    Ok(format!(
        "{}:{}:{}:{}:{}",
        m.ctime(),
        m.ctime_nsec(),
        m.uid(),
        m.gid(),
        m.mode()
    )
    .into_bytes())
}

#[cfg(windows)]
fn metadata_revision(file: &std::fs::File) -> std::io::Result<Vec<u8>> {
    crate::file_storage::windows::workspace_security_revision(file)
}

fn preserve_metadata(source: &std::fs::File, target: &std::fs::File) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        if unsafe {
            libc::fcopyfile(
                source.as_raw_fd(),
                target.as_raw_fd(),
                std::ptr::null_mut(),
                libc::COPYFILE_METADATA,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let read = |file: &std::fs::File| -> std::io::Result<std::collections::BTreeMap<Vec<u8>, Vec<u8>>> {
            let fd = file.as_raw_fd();
            let size = unsafe { libc::flistxattr(fd, std::ptr::null_mut(), 0) };
            if size < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut names = vec![0u8; (size as usize).max(1)];
            let size = unsafe { libc::flistxattr(fd, names.as_mut_ptr().cast(), names.len()) };
            if size < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut attributes = std::collections::BTreeMap::new();
            for name in names[..size as usize].split_inclusive(|b| *b == 0) {
                let size = unsafe { libc::fgetxattr(fd, name.as_ptr().cast(), std::ptr::null_mut(), 0) };
                if size < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let mut value = vec![0u8; (size as usize).max(1)];
                let size = unsafe { libc::fgetxattr(fd, name.as_ptr().cast(), value.as_mut_ptr().cast(), value.len()) };
                if size < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                value.truncate(size as usize);
                attributes.insert(name.to_vec(), value);
            }
            Ok(attributes)
        };
        let (wanted, inherited) = (read(source)?, read(target)?);
        for name in inherited.keys().filter(|name| !wanted.contains_key(*name)) {
            if unsafe { libc::fremovexattr(target.as_raw_fd(), name.as_ptr().cast()) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        for (name, value) in wanted {
            if inherited.get(&name) == Some(&value) {
                continue;
            }
            if unsafe {
                libc::fsetxattr(
                    target.as_raw_fd(),
                    name.as_ptr().cast(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    #[cfg(windows)]
    let _ = (source, target);
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    return Err(std::io::ErrorKind::Unsupported.into());
    Ok(())
}

pub(super) fn same_identity(a: &std::fs::File, b: &std::fs::File) -> std::io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (a, b) = (a.metadata()?, b.metadata()?);
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let info = |file: &std::fs::File| -> std::io::Result<BY_HANDLE_FILE_INFORMATION> {
            let mut info = unsafe { std::mem::zeroed() };
            if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(info)
        };
        let (a, b) = (info(a)?, info(b)?);
        Ok(a.dwVolumeSerialNumber == b.dwVolumeSerialNumber
            && a.nFileIndexHigh == b.nFileIndexHigh
            && a.nFileIndexLow == b.nFileIndexLow)
    }
}

fn write_failure(error: &std::io::Error, display: &str, may_have_modified: bool) -> ToolResult {
    let warning = if may_have_modified {
        "\nThe file may have been partially written. Read it before retrying."
    } else {
        ""
    };
    let (code, message) = if is_locked(error) {
        (
            "file_locked",
            format!(
                "File is locked by another process: {display}. Close the process that is using it and retry.{warning}"
            ),
        )
    } else if error.kind() == ErrorKind::PermissionDenied {
        (
            "permission_denied",
            format!("Permission denied: {display}{warning}"),
        )
    } else if error.kind() == ErrorKind::StorageFull {
        (
            "no_space_left",
            format!("No space left on device: {display}.{warning}"),
        )
    } else {
        (
            "write_error",
            format!("Failed to write file: {error}.{warning}"),
        )
    };
    let mut structured = json!({ "code": code });
    if may_have_modified {
        structured["mayHaveModified"] = json!(true);
    }
    let mut result = failed(message, structured);
    if may_have_modified {
        result.effects = Some(vec![ToolEffect::FileWritten {
            path: display.to_owned(),
        }]);
    }
    result
}

/// Windows sharing violation, which Node reports as `EBUSY`.
const ERROR_SHARING_VIOLATION: i32 = 32;
/// Windows lock violation, which Node reports as `EBUSY`.
const ERROR_LOCK_VIOLATION: i32 = 33;

fn is_locked(error: &std::io::Error) -> bool {
    error.kind() == ErrorKind::ResourceBusy
        || (cfg!(windows)
            && matches!(
                error.raw_os_error(),
                Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
            ))
}
