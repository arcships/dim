#![allow(dead_code)] // Not wired into the composer yet; remove when it is.

//! What a paste or a drag-in becomes in the composer, and what an attachment
//! turns into for the model. The rules mirror the TS composer
//! (`tui-core/composer/paste/classify.ts` and `payload.ts`). Model capabilities
//! are an input here: where they come from is the wire layer's business.

use std::path::Path;

/// Pastes at least this long become a text file token.
pub const LONG_TEXT_PASTE_MIN_CHARS: usize = 5000;
/// Pastes this long, or with this many lines, are multiline tokens.
pub const MULTILINE_PASTE_MIN_CHARS: usize = 201;
pub const MULTILINE_PASTE_MIN_LINES: usize = 4;
/// Largest image the image-read path accepts (TS `IMAGE_READ_MAX_INPUT_BYTES`).
pub const IMAGE_READ_MAX_INPUT_BYTES: u64 = 4 * 1024 * 1024;

/// What the terminal delivered in one paste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteKind {
    /// Every line is an existing absolute path (a drag-in): references, not text.
    Paths(Vec<String>),
    /// Long text: stored as a `pasted-text-N.txt` attachment.
    TextFile,
    /// Multiline text: one atomic token.
    Multiline,
    /// Ordinary text inserted as typed.
    Text,
}

/// Classifies a paste. `path_exists` decides whether a line counts as a path.
pub fn classify_paste(text: &str, path_exists: impl Fn(&str) -> bool) -> PasteKind {
    let lines: Vec<String> = text
        .split(['\r', '\n'])
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect();
    if !lines.is_empty() {
        let paths: Vec<String> = lines
            .iter()
            .filter_map(|line| normalize_path(line))
            .collect();
        if paths.len() == lines.len() && paths.iter().all(|p| path_exists(p)) {
            return PasteKind::Paths(paths);
        }
    }
    let chars = text.chars().count();
    if chars > LONG_TEXT_PASTE_MIN_CHARS {
        PasteKind::TextFile
    } else if chars >= MULTILINE_PASTE_MIN_CHARS || lines.len() >= MULTILINE_PASTE_MIN_LINES {
        PasteKind::Multiline
    } else {
        PasteKind::Text
    }
}

/// One pasted line as an absolute path, with terminal drag-in quoting
/// removed (`\ ` for a space, and surrounding quotes). `None` if it is not an
/// absolute path.
pub fn normalize_path(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let unquoted = if trimmed.len() >= 2
        && ((trimmed.starts_with('\'') && trimmed.ends_with('\''))
            || (trimmed.starts_with('"') && trimmed.ends_with('"')))
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    let mut out = String::with_capacity(unquoted.len());
    let mut chars = unquoted.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    Path::new(&out).is_absolute().then_some(out)
}

/// What a file is, as far as attachments go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Image,
    Video,
    Audio,
    Other,
}

/// Classifies a file by its extension, the way the drag-in path does.
pub fn file_kind(path: &str) -> FileKind {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" => FileKind::Image,
        "mp4" | "mov" | "webm" | "mkv" | "avi" => FileKind::Video,
        "mp3" | "wav" | "m4a" | "flac" | "ogg" => FileKind::Audio,
        _ => FileKind::Other,
    }
}

/// What the current model accepts as input.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub image: bool,
    pub video: bool,
    pub audio: bool,
}

/// How one attachment reaches the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentPlan {
    /// Sent as content the model reads directly.
    Attach,
    /// The model cannot read the image: it is kept on disk and read through
    /// the image-read tool, which takes the path.
    ImageRead,
    /// The image is larger than the image-read limit: the model is asked for
    /// a smaller copy first.
    ImageTooLarge,
    /// Sent as a file reference (path), not content.
    Reference,
}

/// The plan for one attachment of `kind` and `size` bytes under `caps`.
pub fn plan_attachment(kind: FileKind, size: u64, caps: Capabilities) -> AttachmentPlan {
    match kind {
        FileKind::Image if caps.image => AttachmentPlan::Attach,
        FileKind::Image if size <= IMAGE_READ_MAX_INPUT_BYTES => AttachmentPlan::ImageRead,
        FileKind::Image => AttachmentPlan::ImageTooLarge,
        FileKind::Video if caps.video => AttachmentPlan::Attach,
        FileKind::Audio if caps.audio => AttachmentPlan::Attach,
        FileKind::Video | FileKind::Audio | FileKind::Other => AttachmentPlan::Reference,
    }
}

/// The text the model sees in place of an image it cannot read directly.
pub fn image_guidance(plan: &AttachmentPlan, path: &str, size: u64) -> Option<String> {
    match plan {
        AttachmentPlan::ImageRead => Some(format!(
            "This image ({path}) is not readable by the current model. Read it with the image tool at this path."
        )),
        AttachmentPlan::ImageTooLarge => Some(format!(
            "This image ({path}) is {size} bytes, over the {} MiB image-read limit. Make a smaller PNG, JPEG or WebP copy and read that.",
            IMAGE_READ_MAX_INPUT_BYTES / 1024 / 1024
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exists(_: &str) -> bool {
        true
    }

    #[test]
    fn existing_absolute_paths_are_references() {
        assert_eq!(
            classify_paste("/tmp/a.png\n/tmp/b.pdf", exists),
            PasteKind::Paths(vec!["/tmp/a.png".into(), "/tmp/b.pdf".into()])
        );
        // A path that does not exist is just text.
        assert_eq!(classify_paste("/tmp/nope", |_| false), PasteKind::Text);
    }

    #[test]
    fn drag_in_quoting_is_undone() {
        assert_eq!(
            normalize_path("/Users/me/My\\ Shots/x.png").as_deref(),
            Some("/Users/me/My Shots/x.png")
        );
        assert_eq!(
            normalize_path("'/Users/me/a b.png'").as_deref(),
            Some("/Users/me/a b.png")
        );
        assert_eq!(normalize_path("relative/x.png"), None);
    }

    #[test]
    fn long_and_multiline_pastes_follow_the_ts_thresholds() {
        assert_eq!(
            classify_paste(&"x".repeat(5001), |_| false),
            PasteKind::TextFile
        );
        assert_eq!(
            classify_paste(&"x".repeat(5000), |_| false),
            PasteKind::Multiline
        );
        assert_eq!(
            classify_paste("a\nb\nc\nd", |_| false),
            PasteKind::Multiline
        );
        assert_eq!(classify_paste("a\nb", |_| false), PasteKind::Text);
    }

    #[test]
    fn images_depend_on_the_model_and_the_size() {
        let none = Capabilities::default();
        let vision = Capabilities {
            image: true,
            ..Capabilities::default()
        };
        assert_eq!(
            plan_attachment(FileKind::Image, 10, vision),
            AttachmentPlan::Attach
        );
        assert_eq!(
            plan_attachment(FileKind::Image, 10, none),
            AttachmentPlan::ImageRead
        );
        assert_eq!(
            plan_attachment(FileKind::Image, IMAGE_READ_MAX_INPUT_BYTES + 1, none),
            AttachmentPlan::ImageTooLarge
        );
    }

    #[test]
    fn video_is_attached_only_to_models_that_read_it() {
        let none = Capabilities::default();
        let video = Capabilities {
            video: true,
            ..Capabilities::default()
        };
        assert_eq!(
            plan_attachment(FileKind::Video, 1, none),
            AttachmentPlan::Reference
        );
        assert_eq!(
            plan_attachment(FileKind::Video, 1, video),
            AttachmentPlan::Attach
        );
        assert_eq!(file_kind("/x/clip.MP4"), FileKind::Video);
    }
}
