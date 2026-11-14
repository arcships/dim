//! Attachments ([design](../../../docs/design/plugins/Attachments.md)): the
//! Session attachment methods import files through SessionStore and return
//! durable references. Reads verify the calling Session owns the file.
use dim_agent_capabilities::{
    host::{SessionFileError, SessionFileErrorCode, SessionFilesHost},
    methods::{self, Interruption, MethodCall, MethodDecl, RpcError, SessionMethods},
};
use dim_agent_runtime::{AgentBuilder, Names, SessionIdentity};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::{path::PathBuf, sync::Arc};

/// The `sessionMethods` namespace Attachments provides.
const NAMESPACE: &str = "attachment";
/// The content limit of one put (A2); SessionStore's own cap is 16 MiB.
const MAX_TEXT_BYTES: usize = 10 * 1024 * 1024;

/// Installs Attachments in the profile. A host calls this only where
/// SessionStore provides `sessionFiles` and the model has `read`.
pub fn install(builder: &mut AgentBuilder) {
    builder.require_feature(
        dim_agent_runtime::Feature::new("Attachments").session("Attachments", |names| {
            Ok(Box::new(AttachmentsPlugin::new(names)?))
        }),
    );
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PutParams {
    /// Read by Rpc to route the call; the provider stores for its own Session.
    #[serde(rename = "sessionId")]
    _session_id: String,
    name: Option<String>,
    text: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportParams {
    session_id: String,
    path: String,
    name: Option<String>,
    #[serde(default)]
    image: bool,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadParams {
    session_id: String,
    file_id: String,
    offset: u64,
}

/// SessionStore's reply passed through, with `attachmentId = fileId`.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PutReply {
    attachment_id: String,
    path: PathBuf,
    bytes: u64,
}

/// `attachment.put` errors for SessionStore's codes (design A.2).
fn put_error(error: SessionFileError) -> RpcError {
    let code = match error.code {
        SessionFileErrorCode::NotFound => return RpcError::closed(),
        SessionFileErrorCode::Cancelled => return RpcError::reloading(),
        SessionFileErrorCode::Invalid => "attachment_invalid",
        SessionFileErrorCode::TooLarge => "attachment_too_large",
        SessionFileErrorCode::NodeUnavailable => "node_unavailable",
        SessionFileErrorCode::Storage => "storage_failed",
    };
    RpcError::app(-32000, code, error.message)
}

fn prepare_image(path: String) -> Result<(tempfile::NamedTempFile, &'static str), RpcError> {
    use std::io::{BufReader, Write};
    if !std::path::Path::new(&path).is_absolute() {
        return Err(RpcError::invalid("Image source path must be absolute"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|e| RpcError::invalid(e.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|e| RpcError::invalid(e.to_string()))?;
    if !metadata.is_file() {
        return Err(RpcError::invalid("Image source must be a regular file"));
    }
    if metadata.len() > 16 * 1024 * 1024 {
        return Err(RpcError::app(
            -32000,
            "attachment_too_large",
            "Image source exceeds 16 MiB",
        ));
    }
    let image = dim_agent_capabilities::images::scale_image(BufReader::new(file))
        .map_err(|e| RpcError::invalid(format!("Invalid image: {e:?}")))?;
    let mut temporary =
        tempfile::NamedTempFile::new().map_err(|e| RpcError::invalid(e.to_string()))?;
    temporary
        .write_all(&image.bytes)
        .map_err(|e| RpcError::invalid(e.to_string()))?;
    Ok((temporary, image.media_type))
}

pub struct AttachmentsPlugin {
    identity: Key<SessionIdentity>,
    files: Key<SessionFilesHost>,
    methods: Key<SessionMethods>,
}
impl AttachmentsPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            identity: names.key()?,
            files: names.key()?,
            methods: names.key()?,
        })
    }
}
impl DynPlugin for AttachmentsPlugin {
    fn name(&self) -> &str {
        "Attachments"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.identity.erase(),
            self.files.erase(),
            self.methods.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.identity)?;
            let files = ctx.require_at(&self.files)?;
            let methods = ctx.require_at(&self.methods)?;
            let session_id = identity.session_id.clone();
            methods.register(
                ctx,
                NAMESPACE,
                vec![MethodDecl::new("attachment.put", true, Interruption::Drain), MethodDecl::new("attachment.import", true, Interruption::Drain), MethodDecl::new("attachment.read", false, Interruption::Report)],
                Arc::new(move |call, cancel, _| {
                    let MethodCall { params: value, method, .. } = call;
                    let (files, session_id) = (files.clone(), session_id.clone());
                    Box::pin(async move {
                        if method == "attachment.read" {
                            use base64::Engine;
                            let input: ReadParams = methods::params(value)?;
                            if input.session_id != session_id { return Err(RpcError::invalid("Session identity mismatch")); }
                            let bytes = files.0.read(session_id, input.file_id, input.offset, cancel).await.map_err(put_error)?;
                            return methods::reply(serde_json::json!({"data": base64::engine::general_purpose::STANDARD.encode(&bytes), "eof": bytes.is_empty()}));
                        }
                        if method == "attachment.import" {
                            let input: ImportParams = methods::params(value)?;
                            if input.session_id != session_id { return Err(RpcError::invalid("Session identity mismatch")); }
                            let name = input.name.unwrap_or_else(|| if input.image { "image".into() } else { "attachment".into() });
                            let prepared = if input.image {
                                let path = input.path.clone();
                                Some(tokio::task::spawn_blocking(move || prepare_image(path)).await.map_err(|e| RpcError::invalid(e.to_string()))??)
                            } else { None };
                            let path = prepared.as_ref().map(|(file, _)| file.path().to_string_lossy().into_owned()).unwrap_or(input.path);
                            let stored = files.0.import(session_id.clone(), name, path, cancel.clone()).await.map_err(put_error)?;
                            let image = prepared.as_ref().map(|(_, media)| dim_agent_types::ImageSource {
                                source_type: dim_agent_types::ImageSourceType::File,
                                file_id: Some(stored.file_id.clone()), media_type: (*media).into(), data: None, url: None,
                            });
                            return methods::reply(serde_json::json!({ "attachmentId": stored.file_id, "path": stored.path, "bytes": stored.bytes, "image": image }));
                        }
                        let PutParams { name, text, .. } = methods::params(value)?;
                        if text.is_empty() {
                            return Err(RpcError::app(
                                -32000,
                                "attachment_invalid",
                                "Attachment text is empty",
                            ));
                        }
                        if text.len() > MAX_TEXT_BYTES {
                            return Err(RpcError::app(
                                -32000,
                                "attachment_too_large",
                                "Attachment text exceeds 10 MiB",
                            ));
                        }
                        let stored = files
                            .0
                            .put(
                                session_id,
                                name.unwrap_or_else(|| "paste.txt".into()),
                                text.into_bytes(),
                                cancel,
                            )
                            .await
                            .map_err(put_error)?;
                        methods::reply(PutReply {
                            attachment_id: stored.file_id,
                            path: stored.path,
                            bytes: stored.bytes,
                        })
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}
