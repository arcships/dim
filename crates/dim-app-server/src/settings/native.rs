//! User-source SQLite authority. Import commits the document and provenance atomically.
use super::*;
use crate::storage::{ImportUser, ReplaceUser, UserDecision, UserDocument, UserWriteError};

fn token(source: &str, version: i64) -> String {
    format!("native:user/{source}:{version}")
}

fn write_error(error: dim_agent_runtime::AgentError) -> SettingsError {
    match error {
        dim_agent_runtime::AgentError::Cancelled(_) => SettingsError::Cancelled,
        dim_agent_runtime::AgentError::Closed(_) => SettingsError::Closed,
        _ => SettingsError::Persistence,
    }
}

fn decode<F: Format>(
    format: &F,
    row: UserDocument,
) -> Result<(i64, SettingsSnapshot<F::Document>)> {
    if row.version <= 0 {
        return Err(SettingsError::Invalid(
            "invalid persisted settings revision",
        ));
    }
    Ok((
        row.version,
        SettingsSnapshot {
            document: format.decode(Some(row.document.as_bytes()))?,
            revision: token(format.source(), row.version),
        },
    ))
}
impl SettingsService {
    pub(super) async fn native_read<F: Format>(
        &self,
        format: Arc<F>,
        path: FilePath,
        cancel: &CancellationToken,
    ) -> Result<(i64, SettingsSnapshot<F::Document>)> {
        let task = {
            let _guard = self.gate.lock().unwrap();
            self.check_open(cancel)?;
            let db = self
                .database
                .as_ref()
                .expect("native settings binding")
                .clone();
            let files = self.files.clone();
            let cancel = cancel.clone();
            let stopping = self.stopping.clone();
            self.updates.spawn(async move {
                if let Some(row) = db
                    .read_user(format.source().to_owned())
                    .await
                    .map_err(|_| SettingsError::Persistence)?
                {
                    return decode(&*format, row);
                }
                let imported = snapshot(&*format, files.read(&path, &cancel).await)?;
                let body = serde_json::to_string(&imported.document)
                    .map_err(|_| SettingsError::Invalid("cannot encode settings"))?;
                let row = db
                    .import_user_if_absent(
                        ImportUser {
                            source: format.source().to_owned(),
                            document: body,
                            imported_revision: imported.revision,
                        },
                        cancel,
                        stopping,
                    )
                    .await
                    .map_err(write_error)?;
                decode(&*format, row)
            })
        };
        task.await?
    }
    pub(super) async fn native_replace<F: Format>(
        self: &Arc<Self>,
        format: Arc<F>,
        path: FilePath,
        expected: String,
        document: F::Document,
        check: impl FnOnce(&F::Document, &F::Document) -> Result<()> + Send + 'static,
        cancel: &CancellationToken,
    ) -> Result<SettingsSnapshot<F::Document>> {
        let (version, current) = self.native_read(format.clone(), path, cancel).await?;
        let _publication = self.native_publication.lock().await;
        self.check_open(cancel)?;
        if current.revision != expected {
            return Err(SettingsError::Conflict);
        }
        let decision = match check(&current.document, &document) {
            Ok(()) => UserDecision::Write {
                document: serde_json::to_string(&document)
                    .map_err(|_| SettingsError::Invalid("cannot encode settings"))?,
            },
            Err(SettingsError::UserOnlyField(field)) => UserDecision::Reject { field },
            Err(error) => return Err(error),
        };
        let version = self
            .database
            .as_ref()
            .expect("native settings binding")
            .replace_user(
                ReplaceUser {
                    source: format.source().to_owned(),
                    expected_version: version,
                    decision,
                },
                cancel.clone(),
                self.stopping.clone(),
            )
            .await
            .map_err(write_error)?
            .map_err(|error| match error {
                UserWriteError::Conflict => SettingsError::Conflict,
                UserWriteError::Missing => {
                    SettingsError::Invalid("persisted settings source is missing")
                }
                UserWriteError::Overflow => SettingsError::Invalid("settings revision overflow"),
                UserWriteError::Rejected(field) => SettingsError::UserOnlyField(field),
            })?;
        let revision = token(format.source(), version);
        self.publish(format.source(), SettingsScope::User, &revision);
        Ok(SettingsSnapshot { revision, document })
    }
}
