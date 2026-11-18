use crate::projection::tool_id;
use agent_client_protocol::{Client, ConnectionTo, schema::v1 as acp};
use dim_agent_types::{
    ApprovalCallData, MEMBER_APPROVAL_ALLOW, MEMBER_APPROVAL_GET, MemberApprovalDetail,
};
use dim_client::question::{self, Answer, Call};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Approval {
    pub session_id: String,
    pub branch_id: String,
    pub request_id: String,
    pub call: ApprovalCallData,
    pub reason: String,
}

pub(crate) fn key(kind: &str, session: &str, branch: &str, request: &str, call: &str) -> String {
    json!([kind, session, branch, request, call]).to_string()
}

pub(crate) async fn approval(
    cx: ConnectionTo<Client>,
    outbox: crate::outbox::Outbox,
    stop: CancellationToken,
    approval: Approval,
    rpc: crate::rpc::Rpc,
) -> Option<Value> {
    let args = |decision: &str| json!({"sessionId":approval.session_id,"branchId":approval.branch_id,"requestId":approval.request_id,"decision":decision});
    let member = approval.call.name == MEMBER_APPROVAL_ALLOW;
    let detail_params = json!({"sessionId":approval.session_id,"branchId":approval.branch_id,"requestId":approval.request_id});
    let mut detail = if member {
        Some(
            rpc.typed::<MemberApprovalDetail>(MEMBER_APPROVAL_GET, detail_params.clone())
                .await,
        )
    } else {
        None
    };
    loop {
        let (title, input, can_allow) = match &detail {
            Some(Ok(MemberApprovalDetail::Pending { descriptor, .. })) => (
                format!(
                    "{} / {} / {}: {} — {}",
                    approval.branch_id,
                    descriptor.member.agent_id,
                    descriptor.member.branch_id,
                    descriptor.call.name,
                    descriptor.reason
                ),
                serde_json::to_value(descriptor).ok()?,
                true,
            ),
            Some(Ok(MemberApprovalDetail::Ended)) => return Some(args("deny")),
            Some(Err(error)) => (
                format!("Cannot review the original member action: {error}. Reject this request."),
                json!({"error":error.to_string()}),
                false,
            ),
            None => (
                approval.reason.clone(),
                Value::Object(approval.call.input.clone()),
                true,
            ),
        };
        // `kind` and `status`: the rule `projection::notification` states.
        let mut fields = acp::ToolCallUpdateFields::new()
            .kind(acp::ToolKind::Other)
            .status(acp::ToolCallStatus::Pending)
            .title(title);
        if member {
            let detail = serde_json::to_string_pretty(&input).ok()?;
            fields = fields.content(vec![acp::ToolCallContent::from(acp::ContentBlock::Text(
                acp::TextContent::new(detail),
            ))]);
        }
        let tool_call = acp::ToolCallUpdate::new(
            key(
                "approval",
                &approval.session_id,
                &approval.branch_id,
                &approval.request_id,
                "",
            ),
            fields.raw_input(input),
        );
        let mut options = vec![acp::PermissionOption::new(
            "reject_once",
            "Reject once",
            acp::PermissionOptionKind::RejectOnce,
        )];
        if can_allow {
            options.insert(
                0,
                acp::PermissionOption::new(
                    "allow_once",
                    "Allow once",
                    acp::PermissionOptionKind::AllowOnce,
                ),
            );
        }
        let params = serde_json::to_value(acp::RequestPermissionRequest::new(
            acp::SessionId::new(approval.session_id.as_str()),
            tool_call,
            options,
        ))
        .ok()?;
        let result = tokio::select! {
            () = stop.cancelled() => return None,
            result = outbox.request(&cx,"session/request_permission",params) => match result {
                Ok(result) => result,
                Err(_) => return member.then(|| args("deny")),
            },
        };
        let decision = match result
            .get("outcome")
            .filter(|outcome| outcome["outcome"] == "selected")
            .and_then(|outcome| outcome["optionId"].as_str())
        {
            Some("allow_once") if can_allow => "allow",
            Some("reject_once") => "deny",
            _ => return member.then(|| args("deny")),
        };
        if !member || decision == "deny" {
            return Some(args(decision));
        }
        detail = Some(
            rpc.typed::<MemberApprovalDetail>(MEMBER_APPROVAL_GET, detail_params.clone())
                .await,
        );
        if matches!(detail, Some(Ok(MemberApprovalDetail::Pending { .. }))) {
            return Some(args("allow"));
        }
    }
}

pub(crate) async fn question(
    cx: ConnectionTo<Client>,
    outbox: crate::outbox::Outbox,
    stop: CancellationToken,
    session: &str,
    call: &Call,
) -> Option<Value> {
    let mut schema = acp::ElicitationSchema::new();
    for (index, question) in call.questions.iter().enumerate() {
        let choices: Vec<_> = question
            .options
            .iter()
            .map(|choice| {
                acp::EnumOption::new(choice.label.as_str(), choice.label.as_str())
                    .description(choice.description.clone())
            })
            .collect();
        let title = question.header.as_deref().unwrap_or(&question.question);
        let property = if question.multi_select {
            acp::ElicitationPropertySchema::Array(
                acp::MultiSelectPropertySchema::titled(choices)
                    .title(title)
                    .description(question.question.as_str())
                    .min_items(1)
                    .max_items(question.options.len() as u64),
            )
        } else {
            acp::ElicitationPropertySchema::String(
                acp::StringPropertySchema::new()
                    .title(title)
                    .description(question.question.as_str())
                    .one_of(choices),
            )
        };
        schema = schema.property(format!("q{index}"), property, !question.allow_custom);
        if question.allow_custom {
            schema = schema.property(
                format!("custom{index}"),
                acp::StringPropertySchema::new()
                    .title(format!("Custom answer: {}", question.question)),
                false,
            );
        }
    }
    // The request has always carried `required`, empty included; the crate
    // omits it when no property is required.
    schema.required.get_or_insert_default();
    let mut params = serde_json::to_value(acp::CreateElicitationRequest::new(
        acp::ElicitationFormMode::new(
            acp::ElicitationSessionScope::new(acp::SessionId::new(session)).tool_call_id(
                acp::ToolCallId::new(tool_id(&call.branch_id, &call.request_id, &call.call_id)),
            ),
            schema,
        ),
        "Answer the agent's questions",
    ))
    .ok()?;
    // `ElicitationSchema` stores `properties` in a sorted map, while the form
    // has always listed them in question order, each custom answer right after
    // its question: a client renders the fields in the order the schema lists.
    // `serde_json/preserve_order`, which the build enables, keeps that order.
    let serialized = params["requestedSchema"]["properties"]
        .as_object_mut()
        .expect("a serialized schema carries its properties as an object");
    let mut ordered = Map::new();
    for (index, question) in call.questions.iter().enumerate() {
        let name = format!("q{index}");
        let mut property = serialized
            .remove(&name)
            .expect("the schema carries every question it was built from");
        // `EnumOption` omits a description holding null while the form has
        // always stated it: the rule `projection::notification` states for a
        // tool call's `kind` and `status`, at the boundary that serializes.
        let choices = if question.multi_select {
            &mut property["items"]["anyOf"]
        } else {
            &mut property["oneOf"]
        };
        for choice in choices.as_array_mut().into_iter().flatten() {
            if let Some(choice) = choice.as_object_mut() {
                choice.entry("description").or_insert(Value::Null);
            }
        }
        ordered.insert(name, property);
        if question.allow_custom {
            let name = format!("custom{index}");
            let custom = serialized
                .remove(&name)
                .expect("the schema carries every custom answer it was built from");
            ordered.insert(name, custom);
        }
    }
    params["requestedSchema"]["properties"] = Value::Object(ordered);
    let result: Option<Value> = tokio::select! {
        () = stop.cancelled() => return None,
        result = outbox.request(&cx,"elicitation/create",params) => result.ok(),
    };
    let answers: Option<Vec<_>> = result
        .as_ref()
        .filter(|result| result["action"] == "accept")
        .and_then(|result| result["content"].as_object())
        .and_then(|content| {
            call.questions
                .iter()
                .enumerate()
                .map(|(index, question)| {
                    let selection = content.get(&format!("q{index}"));
                    let selected: Vec<String> = match selection {
                        None => Vec::new(),
                        Some(Value::String(value)) if !question.multi_select => vec![value.clone()],
                        Some(Value::Array(values)) if question.multi_select => values
                            .iter()
                            .map(|value| value.as_str().map(str::to_owned))
                            .collect::<Option<_>>()?,
                        _ => return None,
                    };
                    let custom = content
                        .get(&format!("custom{index}"))
                        .and_then(Value::as_str)
                        .filter(|value| !value.trim().is_empty())
                        .map(str::to_owned);
                    if selected.iter().enumerate().any(|(index, value)| {
                        !question.options.iter().any(|choice| &choice.label == value)
                            || selected[..index].contains(value)
                    }) || (custom.is_some() && !question.allow_custom)
                        || (selected.is_empty() && custom.is_none())
                        || (!question.multi_select
                            && selected.len() + usize::from(custom.is_some()) > 1)
                    {
                        return None;
                    }
                    Some(Answer {
                        question_id: question.id.clone(),
                        selected,
                        custom,
                    })
                })
                .collect()
        });
    Some(question::respond(session, call, answers.as_deref()))
}
