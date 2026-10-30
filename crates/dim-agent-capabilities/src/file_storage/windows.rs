//! Handle-relative Windows filesystem operations. No pathname fallback.
use super::*;
use std::{
    mem::{offset_of, size_of},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::MetadataExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Prefix,
    ptr::{null, null_mut},
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_IF,
            FILE_OPEN_REPARSE_POINT, FILE_RENAME_IGNORE_READONLY_ATTRIBUTE,
            FILE_RENAME_INFORMATION, FILE_RENAME_POSIX_SEMANTICS, FILE_RENAME_REPLACE_IF_EXISTS,
            FILE_SYNCHRONOUS_IO_NONALERT, FILE_WRITE_THROUGH, FileRenameInformationEx,
            NtCreateFile, NtSetInformationFile, NtSetSecurityObject,
        },
    },
    Win32::{
        Foundation::{
            ERROR_ACCESS_DENIED, ERROR_INVALID_FUNCTION, ERROR_INVALID_LEVEL,
            ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED, FILETIME, LocalFree,
            RtlNtStatusToDosError, UNICODE_STRING, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Security::{
            Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
            CreatePrivateObjectSecurityEx, DACL_SECURITY_INFORMATION, DestroyPrivateObjectSecurity,
            GENERIC_MAPPING, GetSecurityDescriptorControl, PROTECTED_DACL_SECURITY_INFORMATION,
            PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED, SEF_DACL_AUTO_INHERIT,
            SetSecurityDescriptorControl, TOKEN_QUERY, UNPROTECTED_DACL_SECURITY_INFORMATION,
        },
        Storage::FileSystem::*,
        System::{
            IO::{DeviceIoControl, IO_STATUS_BLOCK},
            SystemServices::IO_REPARSE_TAG_MOUNT_POINT,
            Threading::{
                GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken,
                PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
            },
        },
    },
};

const DIRECTORY_ACCESS: u32 =
    FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL;

pub(super) fn validate_component(name: &OsStr) -> Result<()> {
    let wide: Vec<u16> = name.encode_wide().collect();
    if wide.is_empty()
        || wide
            .iter()
            .any(|c| *c < 32 || b"\":*?<>|/\\".iter().any(|b| *c == u16::from(*b)))
        || matches!(wide.last(), Some(32 | 46))
    {
        return Err(FileStorageError::InvalidPath(
            "invalid Windows file component",
        ));
    }
    let stem = wide.split(|c| *c == u16::from(b'.')).next().unwrap();
    let stem: Vec<u16> = stem
        .iter()
        .map(|c| match *c {
            97..=122 => *c - 32,
            _ => *c,
        })
        .collect();
    let ascii_device = ["CON", "PRN", "AUX", "NUL", "CLOCK$", "CONIN$", "CONOUT$"]
        .iter()
        .any(|device| stem.iter().copied().eq(device.encode_utf16()));
    let numbered_device = stem.len() == 4
        && (stem[..3] == [67, 79, 77] || stem[..3] == [76, 80, 84])
        && matches!(stem[3], 49..=57 | 0x00b9 | 0x00b2 | 0x00b3);
    if ascii_device || numbered_device {
        return Err(FileStorageError::InvalidPath(
            "reserved Windows device name",
        ));
    }
    Ok(())
}
fn nt_result(status: i32) -> io::Result<()> {
    if status >= 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ))
    }
}
/// Opens or creates the entry itself: a reparse point is opened, not followed.
fn create(
    parent: Option<&File>,
    name: &OsStr,
    access: u32,
    disposition: u32,
    options: u32,
    security: PSECURITY_DESCRIPTOR,
) -> io::Result<File> {
    let mut wide: Vec<u16> = name.encode_wide().collect();
    let length = u16::try_from(wide.len() * 2)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file name is too long"))?;
    let mut object_name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: wide.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.map_or(null_mut(), |p| p.as_raw_handle()),
        ObjectName: &mut object_name,
        Attributes: 0,
        SecurityDescriptor: security.cast(),
        SecurityQualityOfService: null(),
    };
    let mut handle = null_mut();
    let mut status = IO_STATUS_BLOCK::default();
    let created = nt_result(unsafe {
        NtCreateFile(
            &mut handle,
            access | SYNCHRONIZE,
            &attributes,
            &mut status,
            null(),
            FILE_ATTRIBUTE_NORMAL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            disposition,
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            null(),
            0,
        )
    });
    if let Err(error) = created {
        return Err(match parent {
            Some(parent) => absent_if_deleted(error, parent),
            None => error,
        });
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}
/// Whether the file or directory behind `file` has been deleted. POSIX
/// semantics deletion unlinks the name at once while open handles stay
/// valid; those handles report the deletion.
fn deleted(file: &File) -> io::Result<bool> {
    let mut standard = FILE_STANDARD_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStandardInfo,
            (&mut standard as *mut FILE_STANDARD_INFO).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(standard.DeletePending || standard.NumberOfLinks == 0)
}
/// Unix fails an operation on a deleted entry, or inside a deleted
/// directory, with ENOENT. Windows fails it with access denied
/// (`STATUS_DELETE_PENDING` or `STATUS_FILE_DELETED`), as it does a real
/// denial, so this checks whether `file` is deleted and then reports
/// NotFound, which removal treats as absent and locking as a stale key.
fn absent_if_deleted(error: io::Error, file: &File) -> io::Error {
    if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) && deleted(file).unwrap_or(false) {
        io::Error::new(io::ErrorKind::NotFound, "deleted by a concurrent removal")
    } else {
        error
    }
}
/// Opens or creates a disk file or directory of the requested type, never a
/// reparse point.
fn open(
    parent: Option<&File>,
    name: &OsStr,
    access: u32,
    disposition: u32,
    options: u32,
    security: PSECURITY_DESCRIPTOR,
) -> io::Result<File> {
    let file = create(parent, name, access, disposition, options, security)?;
    let validated = (|| {
        let metadata = file.metadata()?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || (options & FILE_DIRECTORY_FILE != 0 && !metadata.is_dir())
            || (options & FILE_NON_DIRECTORY_FILE != 0 && !metadata.is_file())
            || unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reparse point or unexpected file type",
            ));
        }
        Ok(())
    })();
    if let Err(error) = validated {
        return Err(if disposition == FILE_CREATE {
            cleanup_created(&file, error)
        } else {
            error
        });
    }
    Ok(file)
}
fn directory(parent: &File, name: &OsStr) -> io::Result<File> {
    open(
        Some(parent),
        name,
        DIRECTORY_ACCESS,
        FILE_OPEN_IF,
        FILE_DIRECTORY_FILE,
        null_mut(),
    )
}
/// Creates the directory `name`, inheriting the parent's DACL; an existing
/// entry is left as it is for the caller to check.
pub(super) fn create_directory(parent: &File, name: &OsStr) -> io::Result<()> {
    match open(
        Some(parent),
        name,
        DIRECTORY_ACCESS,
        FILE_CREATE,
        FILE_DIRECTORY_FILE,
        null_mut(),
    ) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        result => result.map(drop),
    }
}
/// Windows cannot provide the Unix directory-fsync durability contract; as
/// for `replace`, durability rests on NTFS metadata journaling.
pub(super) fn sync_directory(_: &File) -> io::Result<()> {
    Ok(())
}
/// Opens or creates the reserved directory and the temporary directory in it.
pub(super) fn directories(parent: &File) -> io::Result<(File, File)> {
    let locks = directory(parent, OsStr::new(LOCK_DIRECTORY))?;
    let temporary = directory(&locks, OsStr::new(TEMPORARY_DIRECTORY))?;
    Ok((locks, temporary))
}
pub(crate) fn open_root(path: &Path) -> Result<File> {
    if !path.is_absolute() {
        return Err(FileStorageError::InvalidPath(
            "authorized roots must be absolute",
        ));
    }
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Err(FileStorageError::InvalidPath(
            "invalid Windows authorized root",
        ));
    };
    let mut nt_path = OsString::from("\\??\\");
    match prefix.kind() {
        Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
            nt_path.push(format!("{}:\\", char::from(drive)));
        }
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
            nt_path.push("UNC\\");
            nt_path.push(server);
            nt_path.push("\\");
            nt_path.push(share);
            nt_path.push("\\");
        }
        _ => {
            return Err(FileStorageError::InvalidPath(
                "unsupported Windows namespace",
            ));
        }
    }
    let mut root = open(
        None,
        &nt_path,
        DIRECTORY_ACCESS,
        FILE_OPEN,
        FILE_DIRECTORY_FILE,
        null_mut(),
    )?;
    for component in components {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                validate_component(name)?;
                root = open(
                    Some(&root),
                    name,
                    DIRECTORY_ACCESS,
                    FILE_OPEN,
                    FILE_DIRECTORY_FILE,
                    null_mut(),
                )?;
            }
            _ => return Err(FileStorageError::InvalidPath("invalid authorized root")),
        }
    }
    Ok(root)
}
fn identity(file: &File) -> io::Result<FILE_ID_INFO> {
    let mut id = FILE_ID_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(id)
}
pub(super) fn same_identity(a: &File, b: &File) -> io::Result<bool> {
    let (a, b) = (identity(a)?, identity(b)?);
    Ok(a.VolumeSerialNumber == b.VolumeSerialNumber && a.FileId.Identifier == b.FileId.Identifier)
}
/// Whether `name` in `directory` still refers to `file`. A deleted file's
/// handle reports the deletion, also once the directory itself is gone.
pub(super) fn linked(directory: &File, name: &OsStr, file: &File) -> io::Result<bool> {
    if deleted(file)? {
        return Ok(false);
    }
    match create(
        Some(directory),
        name,
        FILE_READ_ATTRIBUTES,
        FILE_OPEN,
        0,
        null_mut(),
    ) {
        Ok(entry) => same_identity(&entry, file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
pub(super) fn parent(mut root: File, path: &Path) -> io::Result<(File, OsString)> {
    let mut components = path.iter().peekable();
    while let Some(name) = components.next() {
        if components.peek().is_none() {
            return Ok((root, name.to_os_string()));
        }
        root = open_directory(&root, name)?;
    }
    unreachable!("validated file path")
}
/// Opens a business directory below `parent`; the reserved directory is
/// refused under every spelling the filesystem equates with it.
pub(super) fn open_directory(parent: &File, name: &OsStr) -> io::Result<File> {
    let next = open(
        Some(parent),
        name,
        DIRECTORY_ACCESS,
        FILE_OPEN,
        FILE_DIRECTORY_FILE,
        null_mut(),
    )?;
    match reserved_directory(parent, OsStr::new(LOCK_DIRECTORY))? {
        Some(reserved) if same_identity(&next, &reserved)? => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reserved storage directory",
        )),
        _ => Ok(next),
    }
}
/// Opens a directory to remove. Mount points are reparse points, which
/// `open` refuses, so another volume is never entered. A leftover tree may
/// itself hold the reserved directory's name.
pub(super) fn subdirectory(parent: &File, name: &OsStr, tree: Tree<'_>) -> io::Result<File> {
    match tree {
        Tree::Business => open_directory(parent, name),
        Tree::Leftover { .. } => open(
            Some(parent),
            name,
            DIRECTORY_ACCESS,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
            null_mut(),
        ),
    }
}
/// Opens one of this module's directories in `directory`, if present.
pub(super) fn reserved_directory(directory: &File, name: &OsStr) -> io::Result<Option<File>> {
    match open(
        Some(directory),
        name,
        DIRECTORY_ACCESS,
        FILE_OPEN,
        FILE_DIRECTORY_FILE,
        null_mut(),
    ) {
        Ok(locks) => Ok(Some(locks)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
pub(super) fn open_key(locks: &File, name: &OsStr) -> io::Result<File> {
    open(
        Some(locks),
        name,
        FILE_GENERIC_READ | FILE_GENERIC_WRITE,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
        null_mut(),
    )
}
pub(super) fn create_key(locks: &File, name: &OsStr) -> io::Result<File> {
    open(
        Some(locks),
        name,
        FILE_GENERIC_READ | FILE_GENERIC_WRITE,
        FILE_OPEN_IF,
        FILE_NON_DIRECTORY_FILE,
        null_mut(),
    )
}
/// The 128-bit file id within the listed directory's volume.
pub(super) type Identity = [u8; 16];
fn kind(attributes: u32) -> EntryKind {
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        EntryKind::Other
    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        EntryKind::Directory
    } else {
        EntryKind::File
    }
}
pub(super) fn entry_kind(parent: &File, name: &OsStr) -> io::Result<Option<EntryKind>> {
    match create(
        Some(parent),
        name,
        FILE_READ_ATTRIBUTES,
        FILE_OPEN,
        0,
        null_mut(),
    ) {
        Ok(entry) => Ok(Some(kind(entry.metadata()?.file_attributes()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
/// Identities of the reserved directory and account marker in `directory`.
pub(super) fn reserved(directory: &File) -> io::Result<[Option<Identity>; 2]> {
    let entry = |name: &str| match create(
        Some(directory),
        OsStr::new(name),
        FILE_READ_ATTRIBUTES,
        FILE_OPEN,
        0,
        null_mut(),
    ) {
        Ok(entry) => Ok(Some(identity(&entry)?.FileId.Identifier)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    };
    Ok([entry(LOCK_DIRECTORY)?, entry("auth.json.lock")?])
}
pub(super) fn entries(directory: &File) -> io::Result<Vec<(DirectoryEntry, Identity)>> {
    let mut buffer = vec![0u64; 8 * 1024];
    let mut entries = Vec::new();
    let mut class = FileIdExtdDirectoryRestartInfo;
    loop {
        if unsafe {
            GetFileInformationByHandleEx(
                directory.as_raw_handle(),
                class,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * size_of::<u64>()) as u32,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            return match error.raw_os_error().map(|code| code as u32) {
                Some(ERROR_NO_MORE_FILES) => Ok(entries),
                Some(
                    ERROR_INVALID_PARAMETER
                    | ERROR_INVALID_LEVEL
                    | ERROR_NOT_SUPPORTED
                    | ERROR_INVALID_FUNCTION,
                ) if class == FileIdExtdDirectoryRestartInfo => Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "directory listing needs file IDs, as on NTFS or ReFS",
                )),
                _ => Err(absent_if_deleted(error, directory)),
            };
        }
        class = FileIdExtdDirectoryInfo;
        let base = buffer.as_ptr().cast::<u8>();
        let mut offset = 0;
        loop {
            let record = unsafe { &*base.add(offset).cast::<FILE_ID_EXTD_DIR_INFO>() };
            let name = unsafe {
                std::slice::from_raw_parts(
                    base.add(offset + offset_of!(FILE_ID_EXTD_DIR_INFO, FileName))
                        .cast::<u16>(),
                    record.FileNameLength as usize / 2,
                )
            };
            if name != [46] && name != [46, 46] {
                entries.push((
                    DirectoryEntry {
                        name: OsString::from_wide(name),
                        kind: kind(record.FileAttributes),
                    },
                    record.FileId.Identifier,
                ));
            }
            if record.NextEntryOffset == 0 {
                break;
            }
            offset += record.NextEntryOffset as usize;
        }
    }
}
pub(super) fn target_name(parent: &File, name: &OsString) -> io::Result<OsString> {
    let file = match open(
        Some(parent),
        name,
        FILE_READ_ATTRIBUTES,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
        null_mut(),
    ) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(name.clone()),
        Err(error) => return Err(error),
    };
    let mut wide = vec![0; 256];
    loop {
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                wide.as_mut_ptr(),
                wide.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_NT,
            )
        };
        if length == 0 {
            let error = io::Error::last_os_error();
            return if deleted(&file).unwrap_or(false) {
                Ok(name.clone())
            } else {
                Err(error)
            };
        }
        if length as usize >= wide.len() {
            wide.resize(length as usize + 1, 0);
            continue;
        }
        wide.truncate(length as usize);
        let basename = wide.rsplit(|c| *c == u16::from(b'\\')).next().unwrap();
        let basename = OsString::from_wide(basename);
        return Ok(if linked(parent, &basename, &file)? {
            basename
        } else {
            name.clone()
        });
    }
}
pub(super) fn create_account_file(parent: &File, name: &OsStr) -> io::Result<File> {
    open(
        Some(parent),
        name,
        FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE | FILE_WRITE_THROUGH,
        null_mut(),
    )
}
pub(super) fn read_account_file(parent: &File, name: &OsString) -> io::Result<File> {
    open(
        Some(parent),
        name,
        FILE_GENERIC_READ | DELETE,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
        null_mut(),
    )
}
pub(super) fn read_user_file(parent: &File, name: &OsString) -> io::Result<File> {
    let file = open(
        Some(parent),
        name,
        FILE_GENERIC_READ,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
        null_mut(),
    )?;
    match open(
        Some(parent),
        OsStr::new("auth.json.lock"),
        FILE_READ_ATTRIBUTES,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
        null_mut(),
    ) {
        Ok(marker) if same_identity(&file, &marker)? => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reserved storage lock",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(file)
}
pub(super) fn permissions(parent: &File, name: &OsString) -> io::Result<Option<Permissions>> {
    match read_user_file(parent, name) {
        Ok(file) => Ok(Some(file.metadata()?.permissions())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
struct SecurityDescriptor {
    pointer: PSECURITY_DESCRIPTOR,
    derived: bool,
}
/// Snapshot native ownership and DACL (including protection flags), without a storage
/// authorization/service dependency. Used to detect concurrent security changes.
pub(crate) fn workspace_security_revision(file: &File) -> io::Result<Vec<u8>> {
    let mut streams = vec![0usize; 8192];
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStreamInfo,
            streams.as_mut_ptr().cast(),
            (streams.len() * size_of::<usize>()) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let stream = unsafe { &*streams.as_ptr().cast::<FILE_STREAM_INFO>() };
    let default_name: Vec<u16> = "::$DATA".encode_utf16().collect();
    let default_stream = stream.StreamNameLength as usize == default_name.len() * 2
        && unsafe { std::slice::from_raw_parts(stream.StreamName.as_ptr(), default_name.len()) }
            == default_name;
    if stream.NextEntryOffset != 0 || (stream.StreamNameLength != 0 && !default_stream) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic workspace replacement cannot preserve alternate data streams",
        ));
    }
    let security = SecurityDescriptor::read(
        file,
        DACL_SECURITY_INFORMATION
            | windows_sys::Win32::Security::OWNER_SECURITY_INFORMATION
            | windows_sys::Win32::Security::GROUP_SECURITY_INFORMATION,
    )?;
    let length =
        unsafe { windows_sys::Win32::Security::GetSecurityDescriptorLength(security.pointer) };
    Ok(
        unsafe { std::slice::from_raw_parts(security.pointer.cast::<u8>(), length as usize) }
            .to_vec(),
    )
}
impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            if self.derived {
                DestroyPrivateObjectSecurity(&raw mut self.pointer);
            } else {
                LocalFree(self.pointer);
            }
        }
    }
}
impl SecurityDescriptor {
    fn read(file: &File, sections: u32) -> io::Result<Self> {
        let mut pointer = null_mut();
        let error = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                sections,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut pointer,
            )
        };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        Ok(Self {
            pointer,
            derived: false,
        })
    }
    fn inherit(parent: &File) -> io::Result<Self> {
        let parent = Self::read(parent, DACL_SECURITY_INFORMATION)?;
        let mut token = null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mapping = GENERIC_MAPPING {
            GenericRead: FILE_GENERIC_READ,
            GenericWrite: FILE_GENERIC_WRITE,
            GenericExecute: FILE_GENERIC_EXECUTE,
            GenericAll: FILE_ALL_ACCESS,
        };
        let mut pointer = null_mut();
        if unsafe {
            CreatePrivateObjectSecurityEx(
                parent.pointer,
                null_mut(),
                &mut pointer,
                null(),
                0,
                SEF_DACL_AUTO_INHERIT,
                token.as_raw_handle(),
                &mapping,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            pointer,
            derived: true,
        })
    }
    fn protected(&self) -> io::Result<bool> {
        let mut control = 0;
        let mut revision = 0;
        if unsafe { GetSecurityDescriptorControl(self.pointer, &mut control, &mut revision) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(control & SE_DACL_PROTECTED != 0)
    }
    fn set_protected(&self, protected: bool) -> io::Result<()> {
        if unsafe {
            SetSecurityDescriptorControl(
                self.pointer,
                SE_DACL_PROTECTED,
                if protected { SE_DACL_PROTECTED } else { 0 },
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
pub(crate) fn create_temp(
    parent: &File,
    name: &OsString,
    business_parent: &File,
    target: &OsString,
    _: Option<&Permissions>,
) -> io::Result<File> {
    let security = match open(
        Some(business_parent),
        target,
        READ_CONTROL | FILE_READ_ATTRIBUTES,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
        null_mut(),
    ) {
        Ok(file) => SecurityDescriptor::read(&file, DACL_SECURITY_INFORMATION)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            SecurityDescriptor::inherit(business_parent)?
        }
        Err(error) => return Err(error),
    };
    let protected = security.protected()?;
    security.set_protected(true)?;
    let file = open(
        Some(parent),
        name,
        FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES | READ_CONTROL | WRITE_DAC | DELETE,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE | FILE_WRITE_THROUGH,
        security.pointer,
    )?;
    let result = (|| {
        security.set_protected(protected)?;
        nt_result(unsafe {
            NtSetSecurityObject(
                file.as_raw_handle(),
                DACL_SECURITY_INFORMATION
                    | if protected {
                        PROTECTED_DACL_SECURITY_INFORMATION
                    } else {
                        UNPROTECTED_DACL_SECURITY_INFORMATION
                    },
                security.pointer,
            )
        })
    })();
    if let Err(error) = result {
        return Err(cleanup_created(&file, error));
    }
    Ok(file)
}
pub(super) fn sync_replacement(file: &File, _: &File, _: &File) -> io::Result<()> {
    file.sync_all()
}
pub(crate) fn replace(
    file: &File,
    _: &File,
    _: &OsString,
    target_parent: &File,
    target: &OsString,
) -> io::Result<()> {
    let name: Vec<u16> = target.encode_wide().collect();
    let bytes = size_of::<FILE_RENAME_INFORMATION>() + name.len() * 2;
    let mut buffer = vec![0usize; bytes.div_ceil(size_of::<usize>())];
    let rename = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    let mut status = IO_STATUS_BLOCK::default();
    unsafe {
        (*rename).Anonymous.Flags = FILE_RENAME_REPLACE_IF_EXISTS
            | FILE_RENAME_POSIX_SEMANTICS
            | FILE_RENAME_IGNORE_READONLY_ATTRIBUTE;
        (*rename).RootDirectory = target_parent.as_raw_handle();
        (*rename).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            buffer
                .as_mut_ptr()
                .cast::<u8>()
                .add(offset_of!(FILE_RENAME_INFORMATION, FileName))
                .cast(),
            name.len(),
        );
        nt_result(NtSetInformationFile(
            file.as_raw_handle(),
            &mut status,
            rename.cast(),
            bytes as u32,
            FileRenameInformationEx,
        ))
    }
}
fn delete(file: &File) -> io::Result<()> {
    let disposition = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfoEx,
            (&disposition as *const FILE_DISPOSITION_INFO_EX).cast(),
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    } == 0
    {
        return Err(absent_if_deleted(io::Error::last_os_error(), file));
    }
    Ok(())
}
fn cleanup_created(file: &File, error: io::Error) -> io::Error {
    match delete(file) {
        Ok(()) => error,
        Err(cleanup) => io::Error::new(
            error.kind(),
            FileStorageError::Cleanup {
                operation: Box::new(FileStorageError::Io(error)),
                cleanup,
            },
        ),
    }
}
pub(super) fn remove_account(_: &File, _: &OsString, file: &File) -> io::Result<()> {
    delete(file)
}
/// Removes a non-directory entry. A reparse point is removed itself, never
/// followed, except that a volume mounted there is refused, as Unix refuses
/// to remove a directory on another filesystem.
pub(super) fn remove(parent: &File, name: &OsStr) -> io::Result<()> {
    let (entry, kind) = deletable(parent, name)?;
    if kind == EntryKind::Directory {
        return Err(io::Error::new(
            io::ErrorKind::IsADirectory,
            "expected a non-directory entry",
        ));
    }
    delete_entry(&entry, kind)
}
/// Removes a link or other reparse point through the handle that checked
/// it, so an entry replaced since it was listed, as by a key or business
/// file, fails instead.
pub(super) fn remove_other(parent: &File, name: &OsStr) -> io::Result<()> {
    let (entry, kind) = match deletable(parent, name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        result => result?,
    };
    if kind != EntryKind::Other {
        return Err(io::Error::other("entry replaced during removal"));
    }
    delete_entry(&entry, kind)
}
/// Opens an entry itself for deletion, with its type.
fn deletable(parent: &File, name: &OsStr) -> io::Result<(File, EntryKind)> {
    let entry = create(
        Some(parent),
        name,
        DELETE | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES,
        FILE_OPEN,
        0,
        null_mut(),
    )?;
    let kind = kind(entry.metadata()?.file_attributes());
    Ok((entry, kind))
}
/// Deletes an opened non-directory entry, refusing a volume mount point.
fn delete_entry(entry: &File, kind: EntryKind) -> io::Result<()> {
    if kind == EntryKind::Other && volume_mount_point(entry)? {
        return Err(io::Error::new(
            io::ErrorKind::CrossesDevices,
            "volume mounted here",
        ));
    }
    delete(entry)
}
/// Whether a reparse point mounts a volume: a mount point whose target is a
/// volume GUID path, not a directory as a junction's is. Reads the reparse
/// data without following it.
fn volume_mount_point(entry: &File) -> io::Result<bool> {
    // Defined in Win32_System_Ioctl, which this crate does not enable.
    const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00a8;
    let mut buffer = vec![0u32; MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize / size_of::<u32>()];
    let mut returned = 0;
    if unsafe {
        DeviceIoControl(
            entry.as_raw_handle(),
            FSCTL_GET_REPARSE_POINT,
            null(),
            0,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * size_of::<u32>()) as u32,
            &mut returned,
            null_mut(),
        )
    } == 0
    {
        return Err(absent_if_deleted(io::Error::last_os_error(), entry));
    }
    let data =
        unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), returned as usize) };
    let field = |offset: usize| {
        data.get(offset..offset + 2)
            .map(|bytes| usize::from(u16::from_le_bytes([bytes[0], bytes[1]])))
    };
    let tag = data
        .get(..4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()));
    let (Some(IO_REPARSE_TAG_MOUNT_POINT), Some(offset), Some(length)) = (tag, field(8), field(10))
    else {
        return Ok(false);
    };
    let Some(target) = data.get(16 + offset..16 + offset + length) else {
        return Ok(false);
    };
    let target: Vec<u16> = target
        .as_chunks::<2>()
        .0
        .iter()
        .map(|unit| u16::from_le_bytes(*unit))
        .collect();
    Ok(String::from_utf16_lossy(&target)
        .to_ascii_lowercase()
        .starts_with(r"\??\volume{"))
}
pub(super) fn remove_directory(parent: &File, name: &OsStr) -> io::Result<()> {
    delete(&open(
        Some(parent),
        name,
        DELETE | FILE_READ_ATTRIBUTES,
        FILE_OPEN,
        FILE_DIRECTORY_FILE,
        null_mut(),
    )?)
}
/// The directories a removal descended from, held open: Windows reopens no
/// parent from a handle without a path, and has no small handle limit.
#[derive(Default)]
pub(super) struct Ancestors(Vec<File>);
impl Ancestors {
    pub(super) fn push(&mut self, directory: File) -> io::Result<()> {
        self.0.push(directory);
        Ok(())
    }
    /// The directory `child` was entered from. NotFound once a concurrent
    /// removal deleted it, so the removal starts again from its target
    /// rather than empty a deleted directory.
    pub(super) fn pop(&mut self, _: &File) -> io::Result<File> {
        let parent = self.0.pop().expect("a directory removal descended from");
        if deleted(&parent)? {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "directory deleted during removal",
            ));
        }
        Ok(parent)
    }
}
pub(super) fn process_owns_marker(pid: u32, created_at: u64) -> io::Result<bool> {
    let handle = unsafe { OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        return match error.raw_os_error().map(|error| error as u32) {
            Some(ERROR_INVALID_PARAMETER) => Ok(false),
            Some(ERROR_ACCESS_DENIED) => Ok(false),
            _ => Err(error),
        };
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    match unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } {
        WAIT_OBJECT_0 => Ok(false),
        WAIT_TIMEOUT => {
            let mut times = [FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            }; 4];
            let inspected = unsafe {
                GetProcessTimes(
                    handle.as_raw_handle(),
                    times.as_mut_ptr(),
                    times.as_mut_ptr().add(1),
                    times.as_mut_ptr().add(2),
                    times.as_mut_ptr().add(3),
                )
            };
            if inspected == 0 {
                return Err(io::Error::last_os_error());
            }
            let ticks =
                (u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime);
            let started = ticks.checked_sub(116_444_736_000_000_000).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid process start time")
            })? / 10_000;
            Ok(started <= created_at)
        }
        _ => Err(io::Error::last_os_error()),
    }
}
