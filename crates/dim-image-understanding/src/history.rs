use super::*;
use dim_agent_capabilities::{images::image_reference, session_files};
use dim_agent_runtime::{
    QueryComparison, QueryFilter, QueryOperand, QueryOutput, QueryPage, QuerySelect, QuerySpec,
    TimelineQuery, TimelineQueryScope,
};
use dim_agent_types::{
    AgentEvent, ContentBlock, EventKind, ImageSource, ImageSourceType, TimelinePosition,
    ToolContent,
};
use std::num::NonZeroU64;

pub(super) struct ImageLocation {
    event: String,
    result: Option<usize>,
    block: usize,
}

#[derive(Default)]
pub(super) struct Index {
    through: Option<u64>,
    pub images: HashMap<String, ImageLocation>,
    pub answers: HashMap<ReuseKey, (String, usize)>,
    calls: HashMap<(String, String), Input>,
}

fn query(
    scope: TimelineQueryScope,
    through: Option<u64>,
    filter: Option<QueryFilter>,
    select: QuerySelect,
) -> TimelineQuery {
    let page = matches!(select, QuerySelect::Events).then_some(QueryPage {
        limit: NonZeroU64::new(64).unwrap(),
        after: None,
    });
    TimelineQuery {
        scope,
        through,
        from: None,
        include_publishing: false,
        query: QuerySpec {
            filter,
            select,
            group_by: vec![],
            pick: None,
            order_by: vec![],
        },
        page,
    }
}

async fn source(
    owner: &ImageUnderstanding,
    source: &ImageSource,
    cancel: &CancellationToken,
) -> Result<ImageSource, HostError> {
    if source.source_type == ImageSourceType::File {
        session_files::image(&owner.files, &owner.identity.session_id, source, cancel).await
    } else {
        Ok(source.clone())
    }
}

impl Index {
    pub async fn update(
        &mut self,
        owner: &ImageUnderstanding,
        branch: &str,
        cancel: &CancellationToken,
    ) -> Result<u64, HostError> {
        let head = owner
            .tree
            .query(
                branch.into(),
                query(TimelineQueryScope::Branch, None, None, QuerySelect::Count),
            )
            .await
            .map_err(|e| HostError::failed(e.to_string()))?
            .through;
        if self.through == Some(head) {
            return Ok(head);
        }
        let mut request = query(
            TimelineQueryScope::Chain,
            Some(head),
            Some(QueryFilter::In {
                value: QueryOperand::Field {
                    path: "kind".into(),
                },
                values: vec![
                    json!("input/message"),
                    json!("step/tool-call"),
                    json!("step/tool-result"),
                ],
            }),
            QuerySelect::Events,
        );
        if let Some(through) = self.through {
            request.from = Some(TimelinePosition {
                branch_id: branch.into(),
                sequence: through + 1,
            });
        }
        loop {
            let reply = owner
                .tree
                .query(branch.into(), request.clone())
                .await
                .map_err(|e| HostError::failed(e.to_string()))?;
            let QueryOutput::Events { events } = reply.result else {
                unreachable!("image history selects events")
            };
            for item in events {
                match &item.event.kind {
                    EventKind::InputMessage(data) => {
                        if let Some(user) = &data.user {
                            for (block, content) in user.content.iter().enumerate() {
                                if let ContentBlock::Image { source: image } = content
                                    && let Some(reference) =
                                        image_reference(&source(owner, image, cancel).await?)
                                {
                                    self.images.insert(
                                        reference,
                                        ImageLocation {
                                            event: item.event.id.clone(),
                                            result: None,
                                            block,
                                        },
                                    );
                                }
                            }
                        }
                    }
                    EventKind::ToolCall(data) if item.position.branch_id == branch => {
                        for call in &data.calls {
                            if call.name == TOOL
                                && let Ok(input) = serde_json::from_value::<Input>(
                                    serde_json::Value::Object(call.input.clone()),
                                )
                            {
                                self.calls
                                    .insert((item.event.id.clone(), call.id.clone()), input);
                            }
                        }
                    }
                    EventKind::ToolResult(data) => {
                        for (result, returned) in data.results.iter().enumerate() {
                            if let ToolContent::Blocks(blocks) = &returned.result.content {
                                for (block, content) in blocks.iter().enumerate() {
                                    if let ContentBlock::Image { source: image } = content
                                        && let Some(reference) =
                                            image_reference(&source(owner, image, cancel).await?)
                                    {
                                        self.images.insert(
                                            reference,
                                            ImageLocation {
                                                event: item.event.id.clone(),
                                                result: Some(result),
                                                block,
                                            },
                                        );
                                    }
                                }
                            }
                            if item.position.branch_id != branch {
                                continue;
                            }
                            let Some(call_event) = &data.call_event_id else {
                                continue;
                            };
                            let Some(input) = self
                                .calls
                                .remove(&(call_event.clone(), returned.call_id.clone()))
                            else {
                                continue;
                            };
                            if returned.result.is_error == Some(true) {
                                continue;
                            }
                            let Some(stamp) = returned
                                .result
                                .structured_content
                                .as_ref()
                                .and_then(|v| v.get("imageUnderstanding"))
                            else {
                                continue;
                            };
                            let Ok(stamp) = serde_json::from_value::<AnswerStamp>(stamp.clone())
                            else {
                                continue;
                            };
                            if stamp.version != VERSION {
                                continue;
                            }
                            self.answers.insert(
                                ReuseKey {
                                    image: input.image,
                                    question: input.question,
                                    provider: stamp.target.provider_id,
                                    model: stamp.target.model_id,
                                },
                                (item.event.id.clone(), result),
                            );
                        }
                    }
                    _ => {}
                }
            }
            let Some(next) = reply.next else {
                break;
            };
            request.page.as_mut().unwrap().after = Some(next);
        }
        self.through = Some(head);
        Ok(head)
    }
}

async fn event(
    owner: &ImageUnderstanding,
    branch: &str,
    through: u64,
    id: &str,
) -> Result<Arc<AgentEvent>, HostError> {
    let reply = owner
        .tree
        .query(
            branch.into(),
            query(
                TimelineQueryScope::Chain,
                Some(through),
                Some(QueryFilter::Compare {
                    left: QueryOperand::Field { path: "id".into() },
                    comparison: QueryComparison::Eq,
                    right: QueryOperand::Value { value: json!(id) },
                }),
                QuerySelect::Events,
            ),
        )
        .await
        .map_err(|e| HostError::failed(e.to_string()))?;
    let QueryOutput::Events { events } = reply.result else {
        unreachable!("image location selects events")
    };
    Ok(events
        .into_iter()
        .next()
        .expect("indexed event at its published cut")
        .event)
}

pub(super) async fn image(
    owner: &ImageUnderstanding,
    branch: &str,
    through: u64,
    location: &ImageLocation,
    cancel: &CancellationToken,
) -> Result<ImageSource, HostError> {
    let event = event(owner, branch, through, &location.event).await?;
    let blocks = match &event.kind {
        EventKind::InputMessage(data) => &data.user.as_ref().expect("indexed user image").content,
        EventKind::ToolResult(data) => {
            let ToolContent::Blocks(blocks) = &data.results
                [location.result.expect("indexed tool image")]
            .result
            .content
            else {
                unreachable!("indexed image blocks")
            };
            blocks
        }
        _ => unreachable!("indexed image event"),
    };
    let ContentBlock::Image { source: image } = &blocks[location.block] else {
        unreachable!("indexed image block")
    };
    source(owner, image, cancel).await
}

pub(super) async fn answer(
    owner: &ImageUnderstanding,
    branch: &str,
    through: u64,
    id: &str,
    result: usize,
) -> Result<ToolResult, HostError> {
    let event = event(owner, branch, through, id).await?;
    let EventKind::ToolResult(data) = &event.kind else {
        unreachable!("indexed tool answer")
    };
    Ok(data.results[result].result.clone())
}
