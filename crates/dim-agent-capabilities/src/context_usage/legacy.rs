use super::{Key, NumericResult, Recovered, State, Summary, fault};
use dim_agent_runtime::{BoxFuture, QueryOutput, QuerySelect, QueryValue, Result, Timeline};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    schema: String,
    source_through: u64,
    state: SavedState,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedState {
    branches: Vec<String>,
    initialized: Key,
    root: Option<Reference>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    id: String,
    first: Key,
    last: Key,
    height: u8,
    summary: Summary,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Node {
    Leaf { entries: Vec<Entry> },
    Branch { children: Vec<Reference> },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    at: Key,
    summary: Summary,
}

fn saved(data: Value) -> Result<Saved> {
    let saved: Saved = serde_json::from_value(data).map_err(|error| fault(error.to_string()))?;
    if !matches!(
        saved.schema.as_str(),
        "context-usage/v1" | "context-usage/v2" | "context-usage/v3"
    ) {
        return Err(fault("legacy numeric reference names another schema"));
    }
    if saved
        .state
        .root
        .as_ref()
        .is_some_and(|root| root.id.is_empty() || root.first > root.last || root.height > 32)
    {
        return Err(fault("invalid legacy numeric range"));
    }
    Ok(saved)
}

pub(super) fn restore(data: Value, id: &str) -> Result<Recovered> {
    let saved = saved(data)?;
    Ok(Recovered {
        schema: super::SCHEMA.into(),
        source_through: saved.source_through,
        state: State {
            epoch: id.into(),
            branches: saved.state.branches,
            initialized: saved.state.initialized,
            legacy_result: Some(id.into()),
            current: saved.state.root.map(|root| NumericResult {
                at: root.last,
                totals: root.summary,
            }),
        },
    })
}

async fn data(timeline: &Timeline, through: u64, id: &str, kind: &str) -> Result<Value> {
    let QueryOutput::Rows { mut rows } = timeline
        .query(crate::history::chain_by_id(
            id,
            through,
            QuerySelect::Fields {
                fields: vec!["kind".into(), "data".into()],
            },
        ))
        .await?
        .result
    else {
        unreachable!("legacy numeric fields")
    };
    let row = rows
        .pop()
        .ok_or_else(|| fault("missing legacy numeric record"))?;
    let [
        QueryValue::Value { value: actual },
        QueryValue::Value { value },
    ] = <[_; 2]>::try_from(row.values).expect("legacy numeric field count")
    else {
        return Err(fault("invalid legacy numeric record"));
    };
    if actual.as_str() != Some(kind) {
        return Err(fault("legacy numeric reference names another event type"));
    }
    Ok(value)
}

fn join(summary: &mut Summary, next: &Summary) -> Result<()> {
    let calibration = summary.calibration.clone();
    summary.join(next)?;
    if next.calibration.is_none() {
        summary.calibration = calibration;
    }
    Ok(())
}

async fn load(timeline: &Timeline, through: u64, reference: &Reference) -> Result<Node> {
    let node: Node =
        serde_json::from_value(data(timeline, through, &reference.id, "context-usage/node").await?)
            .map_err(|error| fault(error.to_string()))?;
    let mut summary = Summary::default();
    let (first, last, height) = match &node {
        Node::Leaf { entries } => {
            if entries.is_empty()
                || entries.len() > 64
                || entries.windows(2).any(|pair| pair[0].at >= pair[1].at)
            {
                return Err(fault("invalid legacy numeric entries"));
            }
            for entry in entries {
                join(&mut summary, &entry.summary)?;
            }
            (entries[0].at, entries.last().unwrap().at, 0)
        }
        Node::Branch { children } => {
            if children.is_empty()
                || children.len() > 32
                || children
                    .windows(2)
                    .any(|pair| pair[0].last >= pair[1].first)
            {
                return Err(fault("invalid legacy numeric children"));
            }
            let height = children[0]
                .height
                .checked_add(1)
                .filter(|height| *height <= 32)
                .ok_or_else(|| fault("invalid legacy numeric height"))?;
            for child in children {
                if child.id.is_empty()
                    || child.first > child.last
                    || child.height.checked_add(1) != Some(height)
                {
                    return Err(fault("invalid legacy numeric child"));
                }
                join(&mut summary, &child.summary)?;
            }
            (children[0].first, children.last().unwrap().last, height)
        }
    };
    if first != reference.first
        || last != reference.last
        || height != reference.height
        || summary != reference.summary
    {
        return Err(fault("legacy numeric node disagrees with its reference"));
    }
    Ok(node)
}

fn at<'a>(
    timeline: &'a Timeline,
    through: u64,
    reference: &'a Reference,
    end: Key,
    inclusive: bool,
) -> BoxFuture<'a, Result<Summary>> {
    Box::pin(async move {
        if reference.first > end || (!inclusive && reference.first == end) {
            return Ok(Summary::default());
        }
        if reference.last < end || (inclusive && reference.last == end) {
            return Ok(reference.summary.clone());
        }
        let mut summary = Summary::default();
        match load(timeline, through, reference).await? {
            Node::Leaf { entries } => {
                for entry in entries {
                    if entry.at > end || (!inclusive && entry.at == end) {
                        break;
                    }
                    join(&mut summary, &entry.summary)?;
                }
            }
            Node::Branch { children } => {
                for child in children {
                    if child.first > end || (!inclusive && child.first == end) {
                        break;
                    }
                    join(
                        &mut summary,
                        &at(timeline, through, &child, end, inclusive).await?,
                    )?;
                }
            }
        }
        Ok(summary)
    })
}

pub(super) async fn prefix(
    timeline: &Timeline,
    through: u64,
    id: &str,
    end: Key,
    inclusive: bool,
) -> Result<Summary> {
    let saved = saved(data(timeline, through, id, "context-usage/recovered").await?)?;
    match saved.state.root {
        Some(root) => at(timeline, through, &root, end, inclusive).await,
        None => Ok(Summary::default()),
    }
}
