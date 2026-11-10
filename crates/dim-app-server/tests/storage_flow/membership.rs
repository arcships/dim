//! #1361: membership lists of any length. The native compiler turns an `In`
//! into SQL, so a test of its list limit runs on the native store:
//! `MemoryTimelineStore` evaluates the filter directly and cannot show it.

use super::*;
use dim_agent_runtime::{
    QueryFilter, QueryOperand, QueryOutput, QueryPage, QuerySelect, QuerySpec, QueryValue,
    TimelineReadScope, TimelineReadSegment,
};
use serde_json::{Value, json};
use std::num::NonZeroU64;

fn fields(names: &[&str]) -> QuerySpec {
    QuerySpec {
        filter: None,
        select: QuerySelect::Fields {
            fields: names.iter().map(|name| (*name).into()).collect(),
        },
        group_by: vec![],
        pick: None,
        order_by: vec![],
    }
}

fn scope(through: u64) -> TimelineReadScope {
    TimelineReadScope {
        key: key("membership", "main"),
        through,
        segments: vec![TimelineReadSegment {
            branch_id: "main".into(),
            from: 1,
            through,
            stored_through: through,
            tail: vec![],
        }],
    }
}

fn page(limit: u64) -> QueryPage {
    QueryPage {
        limit: NonZeroU64::new(limit).unwrap(),
        after: None,
    }
}

fn membership(values: Vec<Value>) -> QueryFilter {
    QueryFilter::In {
        value: QueryOperand::Field {
            path: "data.value".into(),
        },
        values,
    }
}

fn property(sequence: u64, value: Value) -> StoredEvent {
    record(
        sequence,
        EventKind::Extension(
            ExtensionEvent::new("property", "value", json!({"value":value})).unwrap(),
        ),
    )
}

/// The native store and the in-memory store over one history, so a result the
/// two agree on pins what the compiled filter means.
async fn stores() -> (Arc<SqliteTimelineStore>, MemoryTimelineStore, Database) {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let memory = MemoryTimelineStore::default();
    store
        .create_session("membership", &SessionConfig::default())
        .await
        .unwrap();
    memory
        .create_session("membership", &SessionConfig::default())
        .await
        .unwrap();
    (store, memory, database)
}

async fn append_both(
    store: &SqliteTimelineStore,
    memory: &MemoryTimelineStore,
    records: Vec<StoredEvent>,
) {
    append(store, key("membership", "main"), records.clone())
        .await
        .unwrap();
    memory
        .append(
            key("membership", "main"),
            records,
            Box::new(|result| result.unwrap()),
        )
        .unwrap();
}

/// The matching sequences, after checking the native store returns exactly
/// the in-memory store's reply.
async fn same_result(
    store: &SqliteTimelineStore,
    memory: &MemoryTimelineStore,
    scope: &TimelineReadScope,
    query: &QuerySpec,
) -> Vec<u64> {
    let native = store.query(scope, query, Some(&page(64))).await.unwrap();
    let in_memory = memory.query(scope, query, Some(&page(64))).await.unwrap();
    let QueryOutput::Rows { rows } = &native.result else {
        panic!("sequence rows")
    };
    assert_eq!(
        serde_json::to_value(&native).unwrap(),
        serde_json::to_value(&in_memory).unwrap(),
        "the native store returns the in-memory store's rows for {query:?}"
    );
    rows.iter()
        .map(|row| match &row.values[0] {
            QueryValue::Value { value } => value.as_u64().expect("a sequence is a number"),
            other => panic!("a sequence is not {other:?}"),
        })
        .collect()
}

/// An empty list selects nothing; it is not an absent condition.
#[tokio::test]
async fn an_empty_membership_list_selects_nothing() {
    let (store, memory, database) = stores().await;
    append_both(&store, &memory, vec![property(1, json!("first"))]).await;
    let mut query = fields(&["position.sequence"]);
    query.filter = Some(membership(vec![]));
    assert!(
        same_result(&store, &memory, &scope(1), &query)
            .await
            .is_empty()
    );
    drop(store);
    drop(memory);
    drop(database);
}

/// A candidate keeps its type: numbers compare as numbers, and a boolean, a
/// string, a null and a structured value each match only their own kind. A
/// structured candidate matches nothing.
#[tokio::test]
async fn a_mixed_membership_list_matches_each_candidate_by_its_type() {
    let (store, memory, database) = stores().await;
    let values = [
        json!("first"),
        json!(8),
        json!(-3.5),
        json!(true),
        json!(null),
        json!({"nested":1}),
        json!([1, 2]),
        json!("absent"),
    ];
    let records: Vec<_> = values
        .iter()
        .enumerate()
        .map(|(index, value)| property(index as u64 + 1, value.clone()))
        .collect();
    append_both(&store, &memory, records).await;
    let mut query = fields(&["position.sequence"]);
    query.filter = Some(membership(vec![
        json!("first"),
        json!(8),
        json!(true),
        json!(null),
        json!({"nested":1}),
    ]));
    assert_eq!(
        same_result(&store, &memory, &scope(8), &query).await,
        [1, 2, 4, 5],
        "a number, a boolean, a null and a string match their own kind; a \
         structured candidate matches nothing"
    );
    // A candidate with a NUL character keeps its bytes through the JSON
    // encoding the long-list path uses and the bound parameter the short one
    // does.
    append_both(
        &store,
        &memory,
        vec![property(9, json!("before\u{0}after"))],
    )
    .await;
    query.filter = Some(membership(vec![json!("before\u{0}after")]));
    assert_eq!(
        same_result(&store, &memory, &scope(9), &query).await,
        [9],
        "a candidate carrying a NUL character matches its own value"
    );
    query.filter = Some(membership(vec![json!("missing")]));
    assert!(
        same_result(&store, &memory, &scope(8), &query)
            .await
            .is_empty()
    );
    drop(store);
    drop(memory);
    drop(database);
}

/// A list far past SQLite's expression depth and bound variable count returns
/// exactly what the in-memory store returns.
#[tokio::test]
async fn a_membership_list_of_fifty_thousand_values_is_unbounded() {
    let (store, memory, database) = stores().await;
    let records: Vec<_> = (1..=4)
        .map(|sequence| property(sequence, json!(format!("present-{sequence}"))))
        .collect();
    append_both(&store, &memory, records).await;
    let mut values: Vec<Value> = (0..50_000)
        .map(|index| json!(format!("absent-{index}")))
        .collect();
    values.push(json!("present-3"));
    values.push(json!("present-1"));
    let mut query = fields(&["position.sequence"]);
    query.filter = Some(membership(values));
    assert_eq!(
        same_result(&store, &memory, &scope(4), &query).await,
        [1, 3],
        "the list length does not limit the query"
    );
    drop(store);
    drop(memory);
    drop(database);
}
