use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

/// Returns the launch directory and its counterparts in non-bare Git worktrees.
/// Outside a worktree, or if discovery fails or takes over two seconds, the set is empty.
pub async fn repository_workspaces(workspace: &str) -> Vec<String> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let cwd = tokio::fs::canonicalize(workspace).await.ok()?;
        let top = git_output(&cwd, &["rev-parse", "--show-toplevel"]).await?;
        let top = std::str::from_utf8(&top).ok()?.strip_suffix('\n')?;
        #[cfg(windows)]
        let top = top.strip_suffix('\r').unwrap_or(top);
        let top = tokio::fs::canonicalize(top).await.ok()?;
        let relative = cwd.strip_prefix(&top).ok()?;
        let list = git_output(&cwd, &["worktree", "list", "--porcelain", "-z"]).await?;
        let mut workspaces = vec![workspace.to_owned()];
        let mut entry = None;
        for field in list.split(|byte| *byte == 0) {
            if let Some(path) = field.strip_prefix(b"worktree ") {
                entry = std::str::from_utf8(path).ok().map(PathBuf::from);
            } else if field == b"bare" {
                entry = None;
            } else if field.is_empty()
                && let Some(path) = entry.take()
            {
                let root = tokio::fs::canonicalize(&path).await.unwrap_or(path);
                if root == top {
                    continue;
                }
                let path = if relative.as_os_str().is_empty() {
                    root
                } else {
                    root.join(relative)
                };
                if let Ok(path) = path.into_os_string().into_string()
                    && !workspaces.contains(&path)
                {
                    workspaces.push(path);
                }
            }
        }
        Some(workspaces)
    })
    .await
    .ok()
    .flatten()
    .unwrap_or_default()
}

async fn git_output(cwd: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(name);
        }
    }
    let output = command
        .current_dir(cwd)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    output.status.success().then_some(output.stdout)
}
