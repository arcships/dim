//! #1361: membership lists of any length. The native compiler turns an `In`
//! into SQL, so a test of its list limit runs on the native store:
//! `MemoryTimelineStore` evaluates the filter directly and cannot show it.
//!
//! A short list compiles into one comparison per candidate and a long one into
//! a keyed set membership, so every case here runs through both: the list as
//! it stands, and the same list padded past the threshold with candidates that
//! match nothing.

use super::*;
use dim_agent_runtime::{
    QueryFilter, QueryOperand, QueryOutput, QueryPage, QuerySelect, QuerySpec, QueryValue,
    TimelineReadScope, TimelineReadSegment,
};
use serde_json::{Value, json};
use std::num::NonZeroU64;

/// The compiler's threshold between the inlined form and the keyed one.
const MAX_LIST_PARAMETERS: usize = 64;

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

/// The same list padded past the threshold with candidates that match
/// nothing, so the query compiles into the long form.
fn padded(values: Vec<Value>) -> Vec<Value> {
    values
        .into_iter()
        .chain((0..MAX_LIST_PARAMETERS).map(|index| json!(format!("absent-padding-{index}"))))
        .collect()
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

/// The mixed-type history both forms are checked against: a string, an
/// integer, a float equal to that integer, a boolean, a null, a structured
/// value, an array, a string with a NUL character and a value with no
/// `value` property at all.
fn mixed() -> Vec<StoredEvent> {
    let mut records = vec![
        property(1, json!("first")),
        property(2, json!(8)),
        property(3, json!(8.0)),
        property(4, json!(true)),
        property(5, json!(null)),
        property(6, json!({"nested":1})),
        property(7, json!([1, 2])),
        property(8, json!("before\u{0}after")),
        record(
            9,
            EventKind::Extension(
                ExtensionEvent::new("property", "value", json!({"other":"absent"})).unwrap(),
            ),
        ),
        property(10, json!("missing")),
    ];
    records.sort_by_key(|record| record.event.sequence);
    records
}

/// A candidate keeps its type in both forms: numbers compare numerically
/// across integer and float, and a boolean, a string, a null and a structured
/// value each match by value. A structured candidate matches nothing.
#[tokio::test]
async fn a_mixed_membership_list_matches_each_candidate_by_its_type_in_both_forms() {
    let (store, memory, database) = stores().await;
    append_both(&store, &memory, mixed()).await;
    let candidates = vec![
        json!("first"),
        json!(8),
        json!(true),
        json!(null),
        json!({"nested":1}),
        json!("before\u{0}after"),
    ];
    let mut query = fields(&["position.sequence"]);
    query.filter = Some(membership(candidates.clone()));
    assert_eq!(
        same_result(&store, &memory, &scope(10), &query).await,
        [1, 2, 3, 4, 5, 8],
        "the inlined form matches each candidate by its type"
    );
    query.filter = Some(membership(padded(candidates)));
    assert_eq!(
        same_result(&store, &memory, &scope(10), &query).await,
        [1, 2, 3, 4, 5, 8],
        "the keyed form returns the same rows, including the integer and its \
         equal float"
    );
    drop(store);
    drop(memory);
    drop(database);
}

/// `Not` over a long list keeps the rows a membership test cannot decide: a
/// row with no property and a row with a structured value. An uncoalesced
/// `IN` would make both NULL and `NOT NULL` would drop them.
#[tokio::test]
async fn a_negated_long_membership_keeps_rows_without_a_scalar_value() {
    let (store, memory, database) = stores().await;
    append_both(&store, &memory, mixed()).await;
    let mut query = fields(&["position.sequence"]);
    // `["first", 8]` matches the string, the integer and the equal float; the
    // structured candidate matches nothing, so only the string leaves.
    for (candidates, kept) in [
        (vec![json!("first"), json!(8)], vec![4, 5, 6, 7, 8, 9, 10]),
        (
            vec![json!({"nested":1}), json!("first")],
            vec![2, 3, 4, 5, 6, 7, 8, 9, 10],
        ),
    ] {
        query.filter = Some(QueryFilter::Not {
            condition: Box::new(membership(padded(candidates))),
        });
        assert_eq!(
            same_result(&store, &memory, &scope(10), &query).await,
            kept,
            "a negated long list keeps the rows with no scalar value"
        );
    }
    drop(store);
    drop(memory);
    drop(database);
}

/// The keyed form's set is a list subquery built once, not a correlated
/// subquery walked for every row; and a long list's physical pre-filter still
/// reaches the index over `data.input_id`.
#[tokio::test]
async fn the_long_membership_forms_compile_into_a_list_subquery_and_keep_the_index() {
    let (store, memory, database) = stores().await;
    append_both(&store, &memory, mixed()).await;
    let mut query = fields(&["position.sequence"]);
    query.filter = Some(membership(padded(vec![json!("first")])));
    store.record_query_statistics();
    let _ = store
        .query(&scope(10), &query, Some(&page(8)))
        .await
        .unwrap();
    let statistics = store.take_query_statistics();
    let plans: Vec<&String> = statistics.iter().flat_map(|stats| &stats.plan).collect();
    // The r5 form walked the candidates inside a correlated `EXISTS`, which
    // the plan names as an `EXISTS VIRTUAL TABLE` step; the keyed form builds
    // the set once as a list subquery and reuses it per row.
    assert!(
        plans.iter().any(|step| step.contains("LIST SUBQUERY")),
        "the candidate set is a list subquery: {plans:?}"
    );
    assert!(
        !plans
            .iter()
            .any(|step| step.contains("EXISTS VIRTUAL TABLE")),
        "nothing walks the candidates per row: {plans:?}"
    );

    // Two consumed inputs sit after the mixed history, so the stored head
    // covers them.
    let inputs: Vec<StoredEvent> = (11..=12)
        .map(|sequence| {
            record(
                sequence,
                EventKind::InputConsumed(InputConsumedData {
                    input_id: format!("present-{}", sequence - 10),
                }),
            )
        })
        .collect();
    append_both(&store, &memory, inputs).await;

    // A long list no longer gives up its physical pre-filter at the
    // threshold, so neither length falls back to scanning the branch's
    // events; the planner still picks the index, so the assertion is that an
    // index search happens and no full scan does.
    let short: Vec<Value> = (0..MAX_LIST_PARAMETERS)
        .map(|index| json!(format!("present-{}", index % 2 + 1)))
        .collect();
    let long: Vec<Value> = short
        .iter()
        .cloned()
        .chain((0..MAX_LIST_PARAMETERS).map(|index| json!(format!("absent-{index}"))))
        .collect();
    // The keyed form builds the candidate set once: the long plan names a
    // list subquery and reuses it, while the r5 form walked the candidates
    // inside a correlated `EXISTS` for every row.
    let long_plan = input_id_plan(&store, long.clone()).await;
    assert!(
        long_plan.iter().any(|step| step.contains("LIST SUBQUERY")),
        "a long list builds its candidate set once: {long_plan:?}"
    );
    assert!(
        !long_plan
            .iter()
            .any(|step| step.contains("EXISTS VIRTUAL TABLE")),
        "a long list does not walk the candidates per row: {long_plan:?}"
    );

    for (length, plan) in [
        ("at the threshold", input_id_plan(&store, short).await),
        ("past the threshold", input_id_plan(&store, long).await),
    ] {
        assert!(
            plan.iter().any(|step| step.contains("SEARCH")),
            "a list {length} reaches the input rows by an index: {plan:?}"
        );
        assert!(
            !plan
                .iter()
                .any(|step| step == "SCAN e" || step.starts_with("SCAN e ")),
            "a list {length} does not scan the branch's events: {plan:?}"
        );
    }
    drop(store);
    drop(memory);
    drop(database);
}

/// The `data.input_id` membership plan's step shapes for one candidate list.
async fn input_id_plan(store: &SqliteTimelineStore, values: Vec<Value>) -> Vec<String> {
    let mut query = fields(&["position.sequence"]);
    query.filter = Some(QueryFilter::In {
        value: QueryOperand::Field {
            path: "data.input_id".into(),
        },
        values,
    });
    store.record_query_statistics();
    let _ = store
        .query(&scope(12), &query, Some(&page(8)))
        .await
        .unwrap();
    store
        .take_query_statistics()
        .iter()
        .flat_map(|stats| &stats.plan)
        .cloned()
        .collect()
}

/// A list over a context switch: the physical pre-filter carries an integer
/// per candidate there, and both lengths must return the same rows. This is
/// the shape a wrongly typed pre-filter breaks, because a real query over it
/// then matches nothing.
#[tokio::test]
async fn a_context_switch_list_matches_in_both_lengths() {
    let (store, memory, database) = stores().await;
    let mut records = Vec::new();
    for sequence in 1..=6u64 {
        records.push(record(
            sequence,
            EventKind::Text(DeltaData {
                turn_id: "turn".into(),
                request_id: "request".into(),
                delta: format!("delta {sequence}"),
            }),
        ));
    }
    append_both(&store, &memory, records).await;
    let mut query = fields(&["position.sequence"]);
    let switches = |values: Vec<Value>| QueryFilter::In {
        value: QueryOperand::Field {
            path: "context.render".into(),
        },
        values,
    };
    query.filter = Some(switches(vec![json!(true)]));
    let short = same_result(&store, &memory, &scope(6), &query).await;
    query.filter = Some(switches(vec![json!(true), json!(false)]));
    let both = same_result(&store, &memory, &scope(6), &query).await;
    assert_eq!(
        short.len() + both.len(),
        12,
        "the two lists select four and six rows: {short:?} {both:?}"
    );
    drop(store);
    drop(memory);
    drop(database);
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
