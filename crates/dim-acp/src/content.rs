use crate::rpc::Rpc;
use agent_client_protocol::{Error, schema::v1 as acp};
use base64::{Engine, engine::general_purpose::STANDARD};
use dim_agent_types::{ContentBlock, ImageSource, ImageSourceType, UserInput};
use serde::Deserialize;
use serde_json::json;

pub(crate) enum Part {
    Inline(ContentBlock),
    Attachment { uri: String, text: String },
}

fn unsupported() -> Error {
    Error::new(-32001, "Unsupported prompt content").data(json!({"code":"unsupported_content"}))
}

pub(crate) fn parse(blocks: Vec<acp::ContentBlock>, attachments: bool) -> Result<Vec<Part>, Error> {
    if blocks.is_empty() {
        return Err(Error::invalid_params());
    }
    blocks
        .into_iter()
        .map(|block| {
            let inline = match block {
                acp::ContentBlock::Text(text) => ContentBlock::Text { text: text.text },
                acp::ContentBlock::ResourceLink(link) => ContentBlock::Text {
                    text: format!("@{}", link.uri),
                },
                acp::ContentBlock::Image(image) => {
                    if !matches!(
                        image.mime_type.as_str(),
                        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                    ) || STANDARD.decode(&image.data).is_err()
                    {
                        return Err(Error::invalid_params());
                    }
                    ContentBlock::Image {
                        source: ImageSource {
                            file_id: None,
                            source_type: ImageSourceType::Base64,
                            media_type: image.mime_type,
                            data: Some(image.data),
                            url: None,
                        },
                    }
                }
                acp::ContentBlock::Resource(resource) => match resource.resource {
                    acp::EmbeddedResourceResource::TextResourceContents(resource) => {
                        if resource.text.len() > 10 * 1024 * 1024 {
                            return Err(Error::invalid_params());
                        }
                        if resource.text.len() > 32 * 1024 {
                            if !attachments {
                                return Err(unsupported());
                            }
                            return Ok(Part::Attachment {
                                uri: resource.uri,
                                text: resource.text,
                            });
                        }
                        ContentBlock::Text {
                            text: format!("{}\n{}", resource.uri, resource.text),
                        }
                    }
                    _ => return Err(unsupported()),
                },
                _ => return Err(unsupported()),
            };
            Ok(Part::Inline(inline))
        })
        .collect()
}

pub(crate) async fn upload(rpc: &Rpc, session: &str, parts: Vec<Part>) -> Result<UserInput, Error> {
    #[derive(Deserialize)]
    struct Uploaded {
        path: String,
    }
    let mut content = Vec::new();
    for part in parts {
        content.push(match part {
            Part::Inline(ContentBlock::Image { source })
                if source.source_type == ImageSourceType::Base64 =>
            {
                let bytes = STANDARD
                    .decode(source.data.as_deref().ok_or_else(Error::invalid_params)?)
                    .map_err(|_| Error::invalid_params())?;
                let mut file =
                    tempfile::NamedTempFile::new().map_err(|_| Error::internal_error())?;
                std::io::Write::write_all(&mut file, &bytes)
                    .map_err(|_| Error::internal_error())?;
                #[derive(Deserialize)]
                struct Imported {
                    image: ImageSource,
                }
                let reply: Imported = rpc
                    .typed(
                        "attachment.import",
                        json!({"sessionId":session,"path":file.path(),"image":true}),
                    )
                    .await?;
                ContentBlock::Image {
                    source: reply.image,
                }
            }
            Part::Inline(block) => block,
            Part::Attachment { uri, text } => {
                let uploaded: Uploaded = rpc
                    .typed("attachment.put", json!({"sessionId":session,"text":text}))
                    .await?;
                ContentBlock::Text {
                    text: format!("{uri}\n@{}", uploaded.path),
                }
            }
        });
    }
    Ok(UserInput { content })
}
