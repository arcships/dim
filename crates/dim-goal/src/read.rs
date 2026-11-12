use crate::{
    branch::recovery::{self, BusinessState},
    fold::Fold,
    text,
};
use dim_agent_capabilities::timeline_tree::TreeCommands;
use dim_agent_runtime::{
    AgentError, BoxFuture, QueryOutput, QueryPick, QueryValue, TimelineQuery, TimelineQueryReply,
};
use dim_agent_types::{GoalRead, GoalView, GoalViewStatus};
use std::num::NonZeroU64;
use tokio_util::sync::CancellationToken;

type ReadQuery<'a> = dyn Fn(TimelineQuery) -> BoxFuture<'a, Result<TimelineQueryReply, AgentError>>
    + Send
    + Sync
    + 'a;

pub(crate) fn pause_reloaded(fold: &mut Fold) {
    let previous = std::mem::take(fold);
    fold.objective = previous.objective;
    fold.state = previous.state;
    fold.judge = previous.judge;
    fold.live = previous.live;
    fold.local_blocks = previous.local_blocks;
    fold.last_block = previous.last_block;
    fold.last_change = previous.last_change;
    fold.state_after = previous.state_after;
    fold.record_after = previous.record_after;
    if fold.objective.is_some() && fold.state == text::State::Active {
        fold.state = text::State::Paused("interrupted_reload".into());
    }
}

pub(crate) async fn current(
    tree: &TreeCommands,
    branch_id: &str,
    cancel: &CancellationToken,
) -> Result<GoalRead, AgentError> {
    let mut opening = recovery::request(
        branch_id,
        None,
        u64::MAX,
        &["branch/start", "session/forked"],
        &["position.sequence"],
    );
    opening.through = None;
    opening.query.pick = Some(QueryPick::Last);
    opening.page.as_mut().expect("opening page").limit = NonZeroU64::MIN;
    let reply = tree.query(branch_id.into(), opening).await?;
    let through = reply.through;
    let QueryOutput::Rows { rows } = reply.result else {
        unreachable!("opening position")
    };
    let start = rows.first().map(|row| match &row.values[0] {
        QueryValue::Value { value } => value.as_u64().expect("opening sequence"),
        _ => unreachable!("opening sequence"),
    });
    let query_fn = |query| -> BoxFuture<'_, _> { Box::pin(tree.query(branch_id.into(), query)) };
    let mut fold = load(branch_id, start, through, &query_fn, cancel).await?;
    pause_reloaded(&mut fold);
    Ok(view(branch_id, through, &fold))
}

pub(crate) async fn load(
    branch: &str,
    start: Option<u64>,
    through: u64,
    query_fn: &ReadQuery<'_>,
    cancel: &CancellationToken,
) -> Result<Fold, AgentError> {
    let mut query = recovery::request(
        branch,
        start,
        through,
        &[recovery::KEY],
        &["position.sequence", "data"],
    );
    query.query.pick = Some(QueryPick::Last);
    query.page.as_mut().expect("state page").limit = NonZeroU64::MIN;
    let reply = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(AgentError::Cancelled("Goal read".into())),
        reply = query_fn(query) => reply?,
    };
    let QueryOutput::Rows { mut rows } = reply.result else {
        unreachable!("Goal state fields")
    };
    let Some(row) = rows.pop() else {
        return Ok(Fold::default());
    };
    let [
        QueryValue::Value { value: position },
        QueryValue::Value { value: data },
    ] = <[_; 2]>::try_from(row.values).expect("Goal state fields")
    else {
        return Err(AgentError::fault("Goal", "missing business state"));
    };
    let position = position
        .as_u64()
        .ok_or_else(|| AgentError::fault("Goal", "invalid business position"))?;
    let mut fold = Fold::default();
    BusinessState::decode(data)?.apply(&mut fold);
    fold.last_change = position;
    fold.state_after = position;
    fold.last_block = position;
    fold.record_after = position;
    Ok(fold)
}

pub(crate) fn view(branch_id: &str, through: u64, fold: &Fold) -> GoalRead {
    GoalRead {
        branch_id: branch_id.into(),
        through,
        goal: goal(fold.objective.clone(), &fold.state, fold.judge),
    }
}

pub(crate) fn goal(
    objective: Option<String>,
    state: &text::State,
    judge: text::Judge,
) -> Option<GoalView> {
    objective.map(|objective| GoalView {
        objective,
        status: match state {
            text::State::Active => GoalViewStatus::Active,
            text::State::Paused(reason) => GoalViewStatus::Paused(reason.clone()),
            text::State::Completed(_) => GoalViewStatus::Completed,
            text::State::Blocked(_) => GoalViewStatus::Blocked,
            text::State::None => GoalViewStatus::None,
        },
        evaluator: judge == text::Judge::Evaluator,
    })
}
