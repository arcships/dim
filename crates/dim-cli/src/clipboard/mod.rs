//! The system clipboard: reading files, an image or text for Ctrl+V
//! (below, or through the terminal: `terminal`), and copying text out of
//! the fullscreen interface (`copy`).

mod copy;
pub mod terminal;
pub use copy::{copy_local, osc52};

use crate::{
    Result,
    attachments::file_url,
    images::{self, Attached},
};
use std::path::PathBuf;

/// The most bytes one clipboard image read may make the CLI hold in memory:
/// a read guard, not an image limit.
pub const IMAGE_READ_LIMIT: usize = 64 * 1024 * 1024;

pub enum Source {
    Files(Vec<PathBuf>),
    Image(Attached),
    Text(String),
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn text(value: String) -> Result<Source> {
    if value.len() > 10 * 1024 * 1024 {
        return Err("Clipboard text exceeds 10 MiB".into());
    }
    Ok(Source::Text(value))
}

#[cfg(target_os = "macos")]
pub async fn read() -> Result<Source> {
    objc2::rc::autoreleasepool(|_| read_board(&objc2_app_kit::NSPasteboard::generalPasteboard()))
}

#[cfg(target_os = "macos")]
fn read_board(board: &objc2_app_kit::NSPasteboard) -> Result<Source> {
    use objc2_foundation::NSString;
    let types = board.types().ok_or("Clipboard contents are unavailable")?;
    let types = types
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    if types.iter().any(|value| value == "public.file-url") {
        let file_type = NSString::from_str("public.file-url");
        let mut files = Vec::new();
        for item in board
            .pasteboardItems()
            .ok_or("Clipboard files are unavailable")?
        {
            if item.types().iter().any(|value| *value == *file_type) {
                files.push(
                    file_url(
                        &item
                            .stringForType(&file_type)
                            .ok_or("Clipboard file URL is unavailable")?
                            .to_string(),
                    )
                    .ok_or("Clipboard file URL is not local")?,
                );
            }
        }
        if files.is_empty() {
            return Err("Clipboard file list is empty".into());
        }
        return Ok(Source::Files(files));
    }
    for format in [
        "public.png",
        "public.jpeg",
        "org.webmproject.webp",
        "com.compuserve.gif",
        "public.tiff",
    ] {
        if types.iter().any(|value| value == format) {
            let bytes = board
                .dataForType(&NSString::from_str(format))
                .ok_or("Clipboard image is unavailable")?
                .to_vec();
            let image = if format == "public.tiff"
                && image::guess_format(&bytes).ok() == Some(image::ImageFormat::Tiff)
            {
                use dim_agent_capabilities::images::{
                    ImageKind, ImageRefusal, ImageSize, decode_limits, decode_refusal,
                };
                use image::ImageDecoder;
                let mut reader = image::ImageReader::with_format(
                    std::io::Cursor::new(bytes),
                    image::ImageFormat::Tiff,
                );
                reader.limits(decode_limits());
                let decoder = reader.into_decoder().map_err(|error| {
                    images::refusal(ImageRefusal::Invalid {
                        kind: ImageKind::Tiff,
                        reason: error.to_string(),
                    })
                })?;
                let dimensions = decoder.dimensions();
                images::guard(dimensions)?;
                let stored = ImageSize {
                    width: dimensions.0,
                    height: dimensions.1,
                };
                let image = image::DynamicImage::from_decoder(decoder).map_err(|error| {
                    images::refusal(decode_refusal(ImageKind::Tiff, stored, error))
                })?;
                images::from_pixels(image)?
            } else {
                images::from_bytes(bytes)?
            };
            return Ok(Source::Image(image));
        }
    }
    if types.iter().any(|value| value == "public.utf8-plain-text") {
        return text(
            board
                .stringForType(&NSString::from_str("public.utf8-plain-text"))
                .ok_or("Clipboard text is unavailable")?
                .to_string(),
        );
    }
    Err("Clipboard has no supported files, image or text".into())
}

#[cfg(all(test, target_os = "macos"))]
mod native_flow {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use dim_agent_capabilities::images::MAX_EDGE;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSPasteboard, NSPasteboardItem};
    use objc2_foundation::{NSArray, NSData, NSString};

    #[test]
    fn copied_file_references_submit_images_and_real_file_paths() {
        use crate::model::session::DraftPart;
        use crate::ui::composer::{Composer, Outcome, read_planned};
        use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
        use objc2_foundation::NSURL;
        objc2::rc::autoreleasepool(|_| {
            let dir = tempfile::tempdir().unwrap();
            let image = dir.path().join("截图 with space.png");
            let note = dir.path().join("note with space.md");
            let mut png = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(2, 1)
                .write_to(&mut png, image::ImageFormat::Png)
                .unwrap();
            std::fs::write(&image, png.get_ref()).unwrap();
            std::fs::write(&note, "local file").unwrap();
            let paths = [image.canonicalize().unwrap(), note.canonicalize().unwrap()];
            let board = NSPasteboard::pasteboardWithUniqueName();
            board.clearContents();
            let items: Vec<_> = paths
                .iter()
                .map(|path| {
                    let file = NSURL::fileURLWithPath(&NSString::from_str(path.to_str().unwrap()));
                    let reference = file.fileReferenceURL().unwrap();
                    assert!(reference.isFileReferenceURL());
                    let reference = reference.absoluteString().unwrap();
                    let item = NSPasteboardItem::new();
                    assert!(
                        item.setString_forType(&reference, &NSString::from_str("public.file-url"))
                    );
                    assert_eq!(
                        crate::attachments::pasted_entries(&reference.to_string(), dir.path()),
                        Some(vec![path.clone()])
                    );
                    let raw = url::Url::parse(&reference.to_string())
                        .unwrap()
                        .to_file_path()
                        .unwrap();
                    assert_eq!(
                        crate::attachments::pasted_entries(raw.to_str().unwrap(), dir.path()),
                        Some(vec![path.clone()])
                    );
                    item
                })
                .collect();
            let objects: Vec<_> = items
                .iter()
                .map(|item| ProtocolObject::from_ref(&**item))
                .collect();
            assert!(board.writeObjects(&NSArray::from_slice(&objects)));
            let Source::Files(files) = read_board(&board).unwrap() else {
                panic!("copied files must remain files");
            };
            assert_eq!(files, paths);
            let mut composer = Composer::new(Vec::new());
            let plan = composer.plan_files(&files).unwrap();
            let attached = read_planned(&plan).unwrap();
            composer.insert_files(plan, attached);
            let Outcome::SubmitDraft { parts, .. } =
                composer.handle_key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
            else {
                panic!("copied files must submit content");
            };
            assert!(
                matches!(&parts[0], DraftPart::Block(dim_agent_types::ContentBlock::Image { source }) if source.media_type == "image/png" && source.data.as_deref() == Some(STANDARD.encode(png.get_ref()).as_str()))
            );
            assert!(
                matches!(&parts[1], DraftPart::Text(text) if text.contains(paths[1].to_str().unwrap()) && !text.contains("/.file/"))
            );
            board.clearContents();
        });
    }

    #[test]
    fn private_pasteboard_preserves_formats_precedence_pixels_and_rejections() {
        objc2::rc::autoreleasepool(|_| {
            let board = NSPasteboard::pasteboardWithUniqueName();
            board.clearContents();
            let plain = NSString::from_str("public.utf8-plain-text");
            let png = NSString::from_str("public.png");
            let file = NSString::from_str("public.file-url");
            let mut encoded = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(2, 1)
                .write_to(&mut encoded, image::ImageFormat::Png)
                .unwrap();
            let bytes = encoded.into_inner();
            let item = NSPasteboardItem::new();
            assert!(item.setString_forType(&NSString::from_str("text alternative"), &plain));
            assert!(item.setData_forType(&NSData::with_bytes(&bytes), &png));
            assert!(board.writeObjects(&NSArray::from_slice(&[ProtocolObject::from_ref(&*item)])));
            let Source::Image(source) = read_board(&board).unwrap() else {
                panic!("Expected image before text")
            };
            assert_eq!(source.source.media_type, "image/png");
            assert!(!source.scaled);
            assert_eq!(source.source.data, Some(STANDARD.encode(bytes)));

            board.clearContents();
            let first = NSPasteboardItem::new();
            let second = NSPasteboardItem::new();
            let first_url = "file:///tmp/first%20image.png";
            let second_url = "file:///tmp/second.txt";
            assert!(first.setString_forType(&NSString::from_str(first_url), &file));
            assert!(first.setData_forType(&NSData::with_bytes(&[0]), &png));
            assert!(second.setString_forType(&NSString::from_str(second_url), &file));
            assert!(board.writeObjects(&NSArray::from_slice(&[
                ProtocolObject::from_ref(&*first),
                ProtocolObject::from_ref(&*second)
            ])));
            let Source::Files(files) = read_board(&board).unwrap() else {
                panic!("Expected files before image")
            };
            assert_eq!(
                files,
                [
                    PathBuf::from("/tmp/first image.png"),
                    PathBuf::from("/tmp/second.txt")
                ]
            );

            board.clearContents();
            let text_value = "中\r\ntrailing newline\n";
            assert!(board.setString_forType(&NSString::from_str(text_value), &plain));
            let Source::Text(value) = read_board(&board).unwrap() else {
                panic!("Expected text")
            };
            assert_eq!(value, text_value);

            for width in [2, 8001] {
                board.clearContents();
                let pixels = image::RgbImage::from_pixel(width, 1, image::Rgb([4, 5, 6]));
                let mut data = std::io::Cursor::new(Vec::new());
                image::DynamicImage::ImageRgb8(pixels)
                    .write_to(&mut data, image::ImageFormat::Tiff)
                    .unwrap();
                assert!(board.setData_forType(
                    Some(&NSData::with_bytes(&data.into_inner())),
                    &NSString::from_str("public.tiff")
                ));
                let result = read_board(&board);
                let Source::Image(source) = result.unwrap() else {
                    panic!("Expected pixel image")
                };
                assert!(source.scaled);
                assert_eq!(source.source.media_type, "image/png");
                assert_eq!(source.sent.width, width.min(MAX_EDGE));
                assert_eq!(source.sent.height, 1);
                let decoded =
                    image::load_from_memory(&STANDARD.decode(source.source.data.unwrap()).unwrap())
                        .unwrap()
                        .to_rgb8();
                assert_eq!(decoded.dimensions(), (width.min(MAX_EDGE), 1));
                if width == 2 {
                    assert_eq!(decoded.get_pixel(0, 0), &image::Rgb([4, 5, 6]));
                }
            }

            // A TIFF the decoder cannot read is the image refusal, not a raw
            // decode error: the header is recognized, the offset points past
            // the bytes.
            board.clearContents();
            assert!(board.setData_forType(
                Some(&NSData::with_bytes(b"II*\0\x08\0\0\0")),
                &NSString::from_str("public.tiff")
            ));
            let Err(error) = read_board(&board) else {
                panic!("Expected a refused TIFF")
            };
            assert!(
                error
                    .to_string()
                    .starts_with("Invalid or truncated TIFF image: "),
                "{error}"
            );

            board.clearContents();
            let broken = NSPasteboardItem::new();
            assert!(broken.setData_forType(&NSData::with_bytes(&[0, 1]), &png));
            assert!(
                broken
                    .setString_forType(&NSString::from_str("Do not substitute this text"), &plain)
            );
            assert!(
                board.writeObjects(&NSArray::from_slice(&[ProtocolObject::from_ref(&*broken)]))
            );
            assert!(read_board(&board).is_err());
            board.clearContents();
            assert!(board.setString_forType(
                &NSString::from_str(&"x".repeat(10 * 1024 * 1024 + 1)),
                &plain
            ));
            assert!(read_board(&board).is_err());
            board.clearContents();
            assert!(read_board(&board).is_err());
        });
    }
}

#[cfg(target_os = "windows")]
pub async fn read() -> Result<Source> {
    use windows_sys::Win32::{
        System::{DataExchange::*, Memory::*, Ole::*},
        UI::Shell::DragQueryFileW,
    };
    struct Open;
    impl Drop for Open {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }
    struct Locked(windows_sys::Win32::Foundation::HGLOBAL);
    impl Drop for Locked {
        fn drop(&mut self) {
            unsafe {
                GlobalUnlock(self.0);
            }
        }
    }
    fn data<T>(format: u32, consume: impl FnOnce(&[u8]) -> Result<T>) -> Result<T> {
        let handle = unsafe { GetClipboardData(format) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let pointer = unsafe { GlobalLock(handle) };
        if pointer.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let _locked = Locked(handle);
        let size = unsafe { GlobalSize(handle) };
        consume(unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), size) })
    }
    if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let _open = Open;
    if unsafe { IsClipboardFormatAvailable(CF_HDROP as u32) } != 0 {
        let handle = unsafe { GetClipboardData(CF_HDROP as u32) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let count = unsafe { DragQueryFileW(handle, u32::MAX, std::ptr::null_mut(), 0) };
        let mut files = Vec::new();
        for index in 0..count {
            let size = unsafe { DragQueryFileW(handle, index, std::ptr::null_mut(), 0) };
            let mut path = vec![0; size as usize + 1];
            if unsafe { DragQueryFileW(handle, index, path.as_mut_ptr(), size + 1) } != size {
                return Err("Clipboard file path is unavailable".into());
            }
            files.push(PathBuf::from(
                String::from_utf16(&path[..size as usize])
                    .map_err(|_| "Clipboard file paths must be valid Unicode")?,
            ));
        }
        if files.is_empty() {
            return Err("Clipboard file list is empty".into());
        }
        return Ok(Source::Files(files));
    }
    for name in [
        "PNG",
        "JFIF",
        "image/png",
        "image/jpeg",
        "image/webp",
        "image/gif",
    ] {
        let name = name.encode_utf16().chain([0]).collect::<Vec<_>>();
        let format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
        if format == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if unsafe { IsClipboardFormatAvailable(format) } != 0 {
            return data(format, |bytes| {
                Ok(Source::Image(images::from_bytes(bytes.to_vec())?))
            });
        }
    }
    for format in [CF_DIBV5, CF_DIB] {
        if unsafe { IsClipboardFormatAvailable(format as u32) } != 0 {
            return data(format as u32, |bytes| {
                use image::ImageDecoder;
                let decoder = image::codecs::bmp::BmpDecoder::new_without_file_header(
                    std::io::Cursor::new(bytes),
                )?;
                images::guard(decoder.dimensions())?;
                Ok(Source::Image(images::from_pixels(
                    image::DynamicImage::from_decoder(decoder)?,
                )?))
            });
        }
    }
    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT as u32) } != 0 {
        return data(CF_UNICODETEXT as u32, |bytes| {
            let mut units = Vec::new();
            for unit in bytes.as_chunks::<2>().0 {
                let unit = u16::from_le_bytes([unit[0], unit[1]]);
                if unit == 0 {
                    return text(
                        String::from_utf16(&units)
                            .map_err(|_| "Clipboard text must be valid Unicode")?,
                    );
                }
                if units.len() >= 10 * 1024 * 1024 {
                    return Err("Clipboard text exceeds 10 MiB".into());
                }
                units.push(unit);
            }
            Err("Clipboard text has no terminator".into())
        });
    }
    Err("Clipboard has no supported files, image or text".into())
}

#[cfg(target_os = "linux")]
pub async fn read() -> Result<Source> {
    use tokio::io::AsyncReadExt;
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let program = if wayland { "wl-paste" } else { "xclip" };
    async fn capture(program: &str, args: &[&str], limit: usize) -> Result<Vec<u8>> {
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("Cannot start {program}: {error}"))?;
        let mut bytes = Vec::new();
        let result = child
            .stdout
            .take()
            .expect("piped clipboard output")
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .await;
        if let Err(error) = result {
            let _ = child.kill().await;
            return Err(error.into());
        }
        if bytes.len() > limit {
            child.kill().await?;
            return Err("Clipboard content exceeds its size limit".into());
        }
        let status = child.wait().await?;
        if !status.success() {
            return Err(format!("{program} could not read the clipboard ({status})").into());
        }
        Ok(bytes)
    }
    let formats = capture(
        program,
        if wayland {
            &["--list-types"]
        } else {
            &["-selection", "clipboard", "-out", "-target", "TARGETS"]
        },
        10 * 1024 * 1024,
    )
    .await?;
    let formats = String::from_utf8(formats).map_err(|_| "Clipboard formats must be UTF-8")?;
    let formats = formats.lines().collect::<Vec<_>>();
    let read = |format, limit| async move {
        let args = if wayland {
            vec!["--no-newline", "--type", format]
        } else {
            vec!["-selection", "clipboard", "-out", "-target", format]
        };
        capture(program, &args, limit).await
    };
    for format in ["text/uri-list", "x-special/gnome-copied-files"] {
        if formats.contains(&format) {
            let raw = String::from_utf8(read(format, 10 * 1024 * 1024).await?)
                .map_err(|_| "Clipboard file URLs must be UTF-8")?;
            let mut lines = raw.lines();
            if format == "x-special/gnome-copied-files"
                && !matches!(lines.next(), Some("copy" | "cut"))
            {
                return Err("Clipboard file list has an invalid action".into());
            }
            let entries = lines.filter(|line| !line.is_empty() && !line.starts_with('#'));
            if format == "text/uri-list"
                && !entries
                    .clone()
                    .any(|value| url::Url::parse(value).is_ok_and(|url| url.scheme() == "file"))
            {
                continue;
            }
            let files = entries
                .map(|value| {
                    file_url(value).ok_or_else(|| "Clipboard file URL is not local".into())
                })
                .collect::<Result<Vec<_>>>()?;
            if files.is_empty() {
                return Err("Clipboard file list is empty".into());
            }
            return Ok(Source::Files(files));
        }
    }
    let image = ["image/png", "image/jpeg", "image/webp", "image/gif"]
        .into_iter()
        .find(|format| formats.contains(format))
        .or_else(|| {
            formats
                .iter()
                .copied()
                .find(|format| format.starts_with("image/"))
        });
    if let Some(format) = image {
        return Ok(Source::Image(images::from_bytes(
            read(format, IMAGE_READ_LIMIT).await?,
        )?));
    }
    let format = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"]
        .into_iter()
        .find(|format| formats.contains(format))
        .ok_or("Clipboard has no supported files, image or text")?;
    Ok(Source::Text(
        String::from_utf8(read(format, 10 * 1024 * 1024).await?)
            .map_err(|_| "Clipboard text must be UTF-8")?,
    ))
}

#[cfg(all(test, windows))]
mod windows_flow {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use dim_agent_capabilities::images::MAX_EDGE;
    use std::{
        mem::{size_of, size_of_val},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::{DataExchange::*, Memory::*, Ole::*},
        UI::{Shell::DROPFILES, WindowsAndMessaging::*},
    };

    struct Window(HWND);
    impl Drop for Window {
        fn drop(&mut self) {
            unsafe { DestroyWindow(self.0) };
        }
    }
    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain([0]).collect()
    }
    fn install(window: HWND, values: &[(u32, Vec<u8>)]) -> Vec<Vec<u8>> {
        assert_ne!(unsafe { OpenClipboard(window) }, 0);
        assert_ne!(unsafe { EmptyClipboard() }, 0);
        let mut installed = Vec::new();
        for (format, bytes) in values {
            let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes.len()) };
            assert!(!memory.is_null());
            let address = unsafe { GlobalLock(memory) };
            assert!(!address.is_null());
            unsafe {
                ptr::copy_nonoverlapping(bytes.as_ptr(), address.cast(), bytes.len());
                GlobalUnlock(memory);
            }
            let mut delivered = bytes.clone();
            delivered.resize(unsafe { GlobalSize(memory) }, 0);
            if unsafe { SetClipboardData(*format, memory) }.is_null() {
                unsafe {
                    GlobalFree(memory);
                    CloseClipboard();
                }
                panic!("SetClipboardData: {}", std::io::Error::last_os_error());
            }
            installed.push(delivered);
        }
        assert_ne!(unsafe { CloseClipboard() }, 0);
        installed
    }

    #[test]
    fn clipboard_preserves_files_images_text_and_rejects_invalid_data() {
        let window = Window(unsafe {
            CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                wide("fixture").as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        });
        assert!(!window.0.is_null(), "{}", std::io::Error::last_os_error());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let plain = "中\r\ntrailing newline\n";
        let text_bytes = wide(plain)
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        install(window.0, &[(CF_UNICODETEXT as u32, text_bytes.clone())]);
        let Source::Text(value) = runtime.block_on(read()).unwrap() else {
            panic!("Expected text")
        };
        assert_eq!(value, plain);
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 1)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let png = png.into_inner();
        let format = unsafe { RegisterClipboardFormatW(wide("PNG").as_ptr()) };
        assert_ne!(format, 0);
        let delivered = install(
            window.0,
            &[(format, png), (CF_UNICODETEXT as u32, text_bytes.clone())],
        );
        let Source::Image(source) = runtime.block_on(read()).unwrap() else {
            panic!("Expected image before text")
        };
        assert_eq!(source.source.media_type, "image/png");
        assert_eq!(source.source.data, Some(STANDARD.encode(&delivered[0])));

        let paths = [r"C:\fixture\first image.png", r"C:\fixture\second.txt"];
        let header = DROPFILES {
            pFiles: size_of::<DROPFILES>() as u32,
            pt: POINT { x: 0, y: 0 },
            fNC: 0,
            fWide: 1,
        };
        let mut list = unsafe {
            std::slice::from_raw_parts(ptr::from_ref(&header).cast::<u8>(), size_of_val(&header))
        }
        .to_vec();
        for path in paths {
            list.extend(wide(path).into_iter().flat_map(u16::to_le_bytes));
        }
        list.extend(0u16.to_le_bytes());
        install(window.0, &[(CF_HDROP as u32, list), (format, vec![0])]);
        let Source::Files(files) = runtime.block_on(read()).unwrap() else {
            panic!("Expected files before image")
        };
        assert_eq!(files, paths.map(PathBuf::from));

        for width in [2u32, 8001] {
            let row = (width as usize * 3).div_ceil(4) * 4;
            let header = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: 1,
                biPlanes: 1,
                biBitCount: 24,
                biCompression: BI_RGB,
                biSizeImage: row as u32,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            };
            let mut bitmap = unsafe {
                std::slice::from_raw_parts(
                    ptr::from_ref(&header).cast::<u8>(),
                    size_of_val(&header),
                )
            }
            .to_vec();
            bitmap.extend([6, 5, 4].repeat(width as usize));
            bitmap.resize(size_of::<BITMAPINFOHEADER>() + row, 0);
            install(window.0, &[(CF_DIB as u32, bitmap)]);
            let result = runtime.block_on(read());
            let Source::Image(source) = result.unwrap() else {
                panic!("Expected bitmap pixels")
            };
            assert!(source.scaled);
            assert_eq!(source.sent.width, width.min(MAX_EDGE));
            assert_eq!(source.sent.height, 1);
            let bytes = STANDARD.decode(source.source.data.unwrap()).unwrap();
            let pixels = image::load_from_memory(&bytes).unwrap().to_rgb8();
            assert_eq!(pixels.dimensions(), (width.min(MAX_EDGE), 1));
            if width == 2 {
                assert_eq!(pixels.get_pixel(0, 0), &image::Rgb([4, 5, 6]));
            }
        }
        install(
            window.0,
            &[(format, vec![0]), (CF_UNICODETEXT as u32, text_bytes)],
        );
        assert!(runtime.block_on(read()).is_err());
        install(
            window.0,
            &[(
                CF_UNICODETEXT as u32,
                [0xd800u16, 0]
                    .into_iter()
                    .flat_map(u16::to_le_bytes)
                    .collect(),
            )],
        );
        assert!(runtime.block_on(read()).is_err());
    }
}
