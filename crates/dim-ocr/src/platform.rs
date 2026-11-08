//! Platform facts: whether the on-device engine can run here, and the host
//! target identity the daemon offers for it. A target the platform cannot run
//! is absent from the daemon's option list, never "unavailable".

/// The `providerId` of a host-owned OCR target.
pub const APPLE_VISION_PROVIDER: &str = "local";
/// The `modelId` of the Apple Vision target; the client shows
/// `local/apple-vision`.
pub const APPLE_VISION_MODEL: &str = "apple-vision";
/// The oldest macOS whose Vision framework the helper uses.
pub const MINIMUM_MACOS: (u32, u32) = (12, 0);

/// The macOS version as `(major, minor)`, or `None` elsewhere.
#[cfg(target_os = "macos")]
pub fn macos_version() -> Option<(u32, u32)> {
    use std::ffi::CStr;
    let name = c"kern.osproductversion";
    let mut size = 0usize;
    // SAFETY: the name is a NUL-terminated C string, a null old value with a
    // zero size is the size query the call documents, and the size it writes
    // back is used to allocate exactly that much.
    unsafe {
        if libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        ) != 0
            || size == 0
        {
            return None;
        }
        let mut buffer = vec![0u8; size];
        if libc::sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return None;
        }
        let text = CStr::from_bytes_until_nul(&buffer).ok()?.to_str().ok()?;
        let mut parts = text.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
        Some((major, minor))
    }
}

/// The macOS version as `(major, minor)`, or `None` elsewhere.
#[cfg(not(target_os = "macos"))]
pub fn macos_version() -> Option<(u32, u32)> {
    None
}

/// Whether this platform runs Apple Vision: Apple Silicon on macOS 12 or
/// later. The recognition path opens Apple Silicon I/O user-client classes
/// that Intel Macs do not have; there the framework returns no text instead
/// of falling back, so those hosts do not offer the target.
pub fn apple_vision_available() -> bool {
    cfg!(target_arch = "aarch64") && macos_version().is_some_and(|version| version >= MINIMUM_MACOS)
}
