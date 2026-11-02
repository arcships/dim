//! The external editor behind the edit action (cli-commands.md "Management
//! surfaces").
//!
//! The document's text goes to a file only the user can read, under the
//! product home; the environment's editor opens it; the caller reads it back
//! and writes it with the revision it read with the text. Nothing here talks
//! to the daemon, and nothing here runs an editor that needs no terminal.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::model::session::Area;

/// The drafts' directory under the product home: not the native store, not
/// the Web workspace, and nothing exported.
const DIRECTORY: &str = "edit";

/// Keeps two drafts taken in the same nanosecond apart.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// A document's text in a file only the user can read. Every edit gets its
/// own file, so a kept draft is never overwritten by the next edit, and two
/// clients editing one area never share one. Removing the draft is the
/// owner's decision: a write that keeps it names it in its message.
pub struct Draft {
    path: PathBuf,
}

impl Draft {
    /// Writes `text` to a new draft file of `area` under `home`.
    pub fn open(home: &Path, area: Area, text: &str) -> Result<Self, String> {
        let directory = home.join(DIRECTORY);
        create_private(&directory)?;
        let path = create_private_file(&directory, area.name(), text)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn text(&self) -> Result<String, String> {
        std::fs::read_to_string(&self.path)
            .map_err(|error| format!("{}: {error}", self.path.display()))
    }

    pub fn remove(self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The editor the environment names: `$VISUAL`, then `$EDITOR`, then `vi`
/// (`notepad` on Windows). On Unix it runs through `sh -c '<editor> "$@"'
/// <editor> <file>`, as git does, so a value with quotes or a path with a
/// space works; Windows splits the value on whitespace.
pub fn command(path: &Path) -> Command {
    let name = ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .next()
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "notepad".into()
            } else {
                "vi".into()
            }
        });
    if cfg!(windows) {
        let mut parts = name.split_whitespace();
        let mut command = Command::new(parts.next().unwrap_or("vi"));
        command.args(parts);
        command.arg(path);
        return command;
    }
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(format!("{name} \"$@\""))
        .arg(&name)
        .arg(path);
    command
}

/// Why an editor run wrote nothing.
pub fn exited(status: std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("The editor exited with status {code}."),
        None => format!("The editor stopped without a status code ({status})."),
    }
}

/// A file that did not exist before, readable only by the user. It never
/// follows a file or link that is already there: `create_new` refuses one,
/// and a name that collides is simply not used.
fn create_private_file(directory: &Path, area: &str, text: &str) -> Result<PathBuf, String> {
    for _ in 0..32 {
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let path = directory.join(format!(
            "{area}-{}-{nanos}-{unique}.json",
            std::process::id()
        ));
        let mut file = std::fs::OpenOptions::new();
        file.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            file.mode(0o600);
        }
        match file.open(&path) {
            Ok(mut file) => {
                file.write_all(text.as_bytes())
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
    }
    Err(format!("{}: no free draft name", directory.display()))
}

fn create_private(directory: &Path) -> Result<(), String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(directory)
        .map_err(|error| format!("{}: {error}", directory.display()))
}
