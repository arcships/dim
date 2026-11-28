use dim_agent_capabilities::{
    images,
    methods::RpcError,
    workspace_tools::mentions::{self, MentionIndex},
};
use dim_agent_types::native_files::{Complete, Completions, FileCandidate};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

pub(super) fn complete(index: &MentionIndex, request: Complete) -> Result<Completions, RpcError> {
    let root = super::canonical_cwd(&request.cwd)?;
    let home = std::env::home_dir().and_then(|path| path.canonicalize().ok());
    let spelling = request.query;
    #[cfg(windows)]
    let spelling = spelling.replace('\\', "/");
    let query = if spelling == "~" {
        "~/"
    } else {
        spelling.as_str()
    };
    let (prefix, needle) = query
        .rfind('/')
        .map_or(("", query), |at| query.split_at(at + 1));
    let limit = mentions::parse_limit(request.limit);
    let explicit = prefix.starts_with("~/")
        || Path::new(prefix).is_absolute()
        || prefix.split('/').any(|c| c == "..");
    let candidates = if explicit || root.parent().is_none() || home.as_deref() == Some(&root) {
        let dir = if let Some(rest) = prefix.strip_prefix("~/") {
            home.ok_or_else(|| RpcError::invalid("Home directory unavailable"))?
                .join(rest)
        } else {
            root.join(prefix)
        };
        let mut entries = Vec::new();
        let needle = needle.to_lowercase();
        let children = match std::fs::read_dir(dir) {
            Ok(children) => children,
            Err(error)
                if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
            {
                return Ok(Completions {
                    candidates: Vec::new(),
                });
            }
            Err(error) => return Err(RpcError::invalid(error.to_string())),
        };
        for entry in children.take(2000) {
            let entry = entry.map_err(|error| RpcError::invalid(error.to_string()))?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if name.starts_with('.') && !needle.starts_with('.') {
                continue;
            }
            if let Some(score) = mentions::score(&needle, &name.to_lowercase()) {
                entries.push((
                    std::cmp::Reverse(score),
                    !entry.path().is_dir(),
                    name,
                    entry.path(),
                ));
            }
        }
        entries.sort();
        entries
            .into_iter()
            .take(limit)
            .filter_map(|(_, file, name, path)| candidate(format!("{prefix}{name}"), path, !file))
            .collect()
    } else {
        index
            .search(&root, query, limit)
            .into_iter()
            .filter_map(|c| candidate(c.relative_path, c.absolute_path, c.dir))
            .collect()
    };
    Ok(Completions { candidates })
}

fn candidate(mut text: String, path: PathBuf, dir: bool) -> Option<FileCandidate> {
    if dir {
        text.push('/');
    }
    let image = !dir && images::is_image(&path);
    Some(FileCandidate {
        insert_text: text,
        absolute_path: std::path::absolute(path)
            .ok()?
            .into_os_string()
            .into_string()
            .ok()?,
        dir,
        image,
    })
}
