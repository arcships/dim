//! Where the loader's `user` layer is stored: `plugins.json` in the App
//! Server home, a JSON array of the layer's patches, replaced atomically.
//! Its version is the SHA-256 of the file's bytes; a missing file is the
//! empty layer at the empty version.
use rutis::host::BoxFuture;
use rutis_loader::edit::{Edit, Persist, PersistError, Version};
use rutis_loader::{LoaderError, Patch};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub struct UserLayer {
    path: PathBuf,
}
impl UserLayer {
    pub fn new(home: &std::path::Path) -> Self {
        Self {
            path: home.join("plugins.json"),
        }
    }

    /// The stored patches and their version.
    pub fn read(&self) -> Result<(Vec<Patch>, Version), String> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((vec![], Version::default()));
            }
            Err(error) => return Err(format!("cannot read {}: {error}", self.path.display())),
        };
        let patches = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid {}: {error}", self.path.display()))?;
        Ok((patches, version(&bytes)))
    }

    fn current(&self) -> std::io::Result<Version> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(version(&bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Version::default()),
            Err(error) => Err(error),
        }
    }

    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, bytes)?;
        std::fs::File::open(&temporary)?.sync_all()?;
        std::fs::rename(&temporary, &self.path)
    }
}

fn version(bytes: &[u8]) -> Version {
    Version(format!("{:x}", Sha256::digest(bytes)))
}

impl Persist for UserLayer {
    fn load<'a>(
        &'a self,
        _layer: &'a str,
    ) -> BoxFuture<'a, Result<(Vec<Patch>, Version), LoaderError>> {
        Box::pin(async move { self.read().map_err(LoaderError::Unsupported) })
    }

    fn save<'a>(
        &'a self,
        _layer: &'a str,
        expected: &'a Version,
        _edits: &'a [Edit],
        patches: &'a [Patch],
    ) -> BoxFuture<'a, Result<Version, PersistError>> {
        Box::pin(async move {
            // A file edited by hand since the loader read it: rutis-loader
            // replays its edits on the new content.
            if &self.current().map_err(io)? != expected {
                return Err(PersistError::Conflict);
            }
            let mut bytes = serde_json::to_vec_pretty(patches).expect("patches serialize");
            bytes.push(b'\n');
            self.write(&bytes).map_err(io)?;
            Ok(version(&bytes))
        })
    }
}

fn io(error: std::io::Error) -> PersistError {
    PersistError::Failed(error.to_string())
}
