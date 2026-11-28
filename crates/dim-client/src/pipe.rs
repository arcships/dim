use std::io;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

pub fn bind(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    use std::{
        ffi::c_void,
        mem::size_of,
        os::windows::io::{FromRawHandle, OwnedHandle},
        ptr::null_mut,
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_INSUFFICIENT_BUFFER, LocalFree},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1,
            },
            GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    if !name.starts_with(r"\\.\pipe\") || name.len() == r"\\.\pipe\".len() || name.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "named pipe requires a local \\\\.\\pipe\\ name",
        ));
    }
    struct LocalMemory(*mut c_void);
    impl Drop for LocalMemory {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut length = 0;
    if unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut length) } == 0
        && io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
    {
        return Err(io::Error::last_os_error());
    }
    let mut user = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            user.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let sid = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut sid_text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut sid_text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _sid_text = LocalMemory(sid_text.cast());
    let mut sid_length = 0;
    while unsafe { *sid_text.add(sid_length) } != 0 {
        sid_length += 1;
    }
    let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(sid_text, sid_length) })
        .map_err(io::Error::other)?;
    let descriptor: Vec<u16> = format!("O:{sid}D:P(A;;GA;;;{sid})")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut security = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor.as_ptr(),
            SDDL_REVISION_1,
            &mut security,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let _security = LocalMemory(security);
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security,
        bInheritHandle: 0,
    };
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )
    }
}

pub async fn connect(
    name: &str,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, (&'static str, io::Error)> {
    use tokio::net::windows::named_pipe::ClientOptions;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY};
    if !name.starts_with(r"\\.\pipe\") || name.len() == r"\\.\pipe\".len() || name.contains('\0') {
        return Err((
            "EINVAL",
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "a local named pipe is required",
            ),
        ));
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let pipe = loop {
        match ClientOptions::new().open(name) {
            Ok(pipe) => break pipe,
            Err(error) => {
                let code = match error.raw_os_error().map(|code| code as u32) {
                    Some(ERROR_PIPE_BUSY) if tokio::time::Instant::now() < deadline => {
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        continue;
                    }
                    Some(ERROR_FILE_NOT_FOUND) => "ENOENT",
                    Some(ERROR_PIPE_BUSY) => "EBUSY",
                    _ => "EACCES",
                };
                return Err((code, error));
            }
        }
    };
    authenticate(&pipe).map_err(|error| ("EACCES", error))?;
    Ok(pipe)
}

fn authenticate(pipe: &tokio::net::windows::named_pipe::NamedPipeClient) -> io::Result<()> {
    use std::{
        mem::size_of,
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        ptr::null_mut,
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_INSUFFICIENT_BUFFER, LocalFree},
        Security::{
            Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT},
            EqualSid, GetTokenInformation, OWNER_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER,
            TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut length = 0;
    unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut length) };
    if io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
        return Err(io::Error::last_os_error());
    }
    let mut user = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            user.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut owner = null_mut();
    let mut descriptor = null_mut();
    let status = unsafe {
        GetSecurityInfo(
            pipe.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let same_user = !owner.is_null()
        && unsafe { EqualSid(owner, (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid) != 0 };
    unsafe { LocalFree(descriptor) };
    if !same_user {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "named pipe is not owned by the current user",
        ));
    }
    Ok(())
}
