use super::sqlite::error;
use dim_agent_runtime::core::TraceStep;
use dim_agent_runtime::{Result, StoredEvent, TimelineTrace};
use dim_agent_types::{
    AgentEvent, EventDraft, EventKind, Message, ModelRequest, SystemPrompt, ToolDefinition,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WireEvent {
    pub event: Value,
    refs: Option<Refs>,
    trace: Option<WireTrace>,
    pub batch_end: u64,
}
/// The content a request record references: one reference per message
/// (format v16, still read) or its message list (v17, written).
#[derive(Serialize)]
#[serde(untagged)]
enum Refs {
    Messages(References),
    List(ListReferences),
}
#[derive(Serialize)]
struct References {
    messages: Vec<String>,
    tools: Option<String>,
    system: Option<String>,
}
/// A request's messages as the list ending at node `list`, `length` nodes
/// long, `None` exactly when it has no message; `tools` and `system` as in
/// `References`.
#[derive(Clone, Serialize)]
pub(super) struct ListReferences {
    pub list: Option<String>,
    pub length: usize,
    pub tools: Option<String>,
    pub system: Option<String>,
}
/// A `node` content row: one message and the address of the node of the
/// message before it, `None` for a first message.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Node<P = String, M = Arc<Message>> {
    parent: Option<P>,
    message: M,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDraft {
    event: Value,
    refs: Option<Refs>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTrace {
    event_id: String,
    original: WireDraft,
    steps: Vec<WireStep>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireStep {
    handler: String,
    result: Option<WireDraft>,
}

/// The fields present tell the two shapes apart, and each rejects the
/// other's and unknown fields. Written by hand because an untagged enum
/// would copy every reference of a v16 row to try its variants.
impl<'de> Deserialize<'de> for Refs {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        use serde::de::{Error, MapAccess, Visitor};
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "lowercase")]
        enum Field {
            Messages,
            List,
            Length,
            Tools,
            System,
        }
        fn set<T, E: Error>(
            slot: &mut Option<T>,
            value: T,
            name: &'static str,
        ) -> std::result::Result<(), E> {
            match slot.replace(value) {
                Some(_) => Err(E::duplicate_field(name)),
                None => Ok(()),
            }
        }
        struct Fields;
        impl<'de> Visitor<'de> for Fields {
            type Value = Refs;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("request references")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Refs, A::Error> {
                let (mut messages, mut list, mut length, mut tools, mut system) =
                    (None, None, None, None, None);
                while let Some(field) = map.next_key()? {
                    match field {
                        Field::Messages => set(&mut messages, map.next_value()?, "messages")?,
                        Field::List => set(&mut list, map.next_value()?, "list")?,
                        Field::Length => set(&mut length, map.next_value()?, "length")?,
                        Field::Tools => set(&mut tools, map.next_value()?, "tools")?,
                        Field::System => set(&mut system, map.next_value()?, "system")?,
                    }
                }
                let (tools, system) = (tools.flatten(), system.flatten());
                match (messages, list, length) {
                    (Some(messages), None, None) => Ok(Refs::Messages(References {
                        messages,
                        tools,
                        system,
                    })),
                    (None, Some(list), Some(length)) => Ok(Refs::List(ListReferences {
                        list,
                        length,
                        tools,
                        system,
                    })),
                    _ => Err(A::Error::custom(
                        "request references are neither messages nor a list",
                    )),
                }
            }
        }
        deserializer.deserialize_map(Fields)
    }
}

impl Refs {
    /// The content rows these references name directly; a list names its
    /// last node, which reaches the others.
    fn hashes(&self) -> Vec<&str> {
        let (messages, tools, system) = match self {
            Refs::Messages(refs) => (refs.messages.iter().collect(), &refs.tools, &refs.system),
            Refs::List(refs) => (
                refs.list.iter().collect::<Vec<_>>(),
                &refs.tools,
                &refs.system,
            ),
        };
        messages
            .into_iter()
            .chain(tools)
            .chain(system)
            .map(String::as_str)
            .collect()
    }
}
impl WireEvent {
    /// The list a stored request row references, `None` for a v16 row.
    pub(super) fn list(&self) -> Result<Option<&ListReferences>> {
        match &self.refs {
            Some(Refs::List(refs)) => Ok(Some(refs)),
            Some(Refs::Messages(_)) => Ok(None),
            None => Err(error("event request references do not match event kind")),
        }
    }
    /// The content rows this event and its before trace reference directly.
    pub(super) fn references(&self) -> impl Iterator<Item = &str> {
        self.lists().flatten().flat_map(Refs::hashes)
    }
    /// The references of the event, then of each draft of its trace, in
    /// the order `encode` writes and `Reader::decode` rebuilds them.
    fn lists(&self) -> impl Iterator<Item = Option<&Refs>> {
        let drafts = self.trace.iter().flat_map(|trace| {
            std::iter::once(&trace.original)
                .chain(trace.steps.iter().filter_map(|step| step.result.as_ref()))
        });
        std::iter::once(self.refs.as_ref()).chain(drafts.map(|draft| draft.refs.as_ref()))
    }
}

pub(super) fn hash(kind: &str, body: &str) -> String {
    format!(
        "{:x}",
        Sha256::new()
            .chain_update(kind)
            .chain_update([0])
            .chain_update(body)
            .finalize()
    )
}
pub(super) fn request(kind: &EventKind) -> Option<&Arc<ModelRequest>> {
    match kind {
        EventKind::ContextPrepare(data) => Some(&data.request),
        EventKind::ModelCall(data) => Some(&data.request),
        _ => None,
    }
}
fn request_mut(kind: &mut EventKind) -> Option<&mut Arc<ModelRequest>> {
    match kind {
        EventKind::ContextPrepare(data) => Some(&mut data.request),
        EventKind::ModelCall(data) => Some(&mut data.request),
        _ => None,
    }
}
/// The drafts of a before trace, in the order their references are stored.
pub(super) fn drafts(trace: &Option<TimelineTrace>) -> impl Iterator<Item = &EventDraft> {
    trace.iter().flat_map(|trace| {
        std::iter::once(&trace.original)
            .chain(trace.steps.iter().filter_map(|step| step.result.as_ref()))
    })
}

/// A content row a record adds, written with `INSERT OR IGNORE`.
pub(super) struct ContentRow {
    pub hash: String,
    pub kind: &'static str,
    pub body: String,
}
/// A request a list is encoded against (§6.1 of the request storage
/// design): the previous request in the logical history of a record's
/// branch, or a draft's record request.
#[derive(Clone)]
pub(super) struct Previous {
    pub sequence: u64,
    pub values: Values,
    pub refs: ListReferences,
}
#[derive(Clone)]
pub(super) enum Values {
    /// Still in memory: written earlier in the round or held by the writer
    /// memo.
    Written(Arc<ModelRequest>),
    /// Rebuilt from its stored list.
    Stored(Arc<Rebuilt>),
}
impl Values {
    fn messages(&self) -> &[Arc<Message>] {
        match self {
            Values::Written(request) => &request.messages,
            Values::Stored(request) => &request.messages,
        }
    }
    fn tools(&self) -> Option<&Arc<Vec<ToolDefinition>>> {
        match self {
            Values::Written(request) => request.tools.as_ref(),
            Values::Stored(request) => request.tools.as_ref(),
        }
    }
    fn system(&self) -> Option<&Arc<SystemPrompt>> {
        match self {
            Values::Written(request) => request.system.as_ref(),
            Values::Stored(request) => request.system.as_ref(),
        }
    }
}
/// The length of the common prefix of the previous request's messages and
/// `request`'s, by pointer and then by value.
pub(super) fn prefix(previous: &Previous, request: &ModelRequest) -> usize {
    previous
        .values
        .messages()
        .iter()
        .zip(&request.messages)
        .take_while(|(kept, message)| Arc::ptr_eq(kept, message) || kept == message)
        .count()
}
/// `value`'s address: the previous request's when equal, without
/// serializing; otherwise a content row of `kind`.
fn reuse<T: PartialEq + Serialize>(
    value: Option<&Arc<T>>,
    previous: Option<(&Arc<T>, &String)>,
    kind: &'static str,
    content: &mut Vec<ContentRow>,
) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if let Some((kept, address)) = previous
        && (Arc::ptr_eq(kept, value) || kept == value)
    {
        return Ok(Some(address.clone()));
    }
    let body = serde_json::to_string(value).map_err(error)?;
    let hash = hash(kind, &body);
    content.push(ContentRow {
        hash: hash.clone(),
        kind,
        body,
    });
    Ok(Some(hash))
}
/// Encodes `request` as a list that keeps the first `keep` messages of
/// `previous`, whose node at `keep - 1` is `parent`: a node for each later
/// message, and `tools` and `system` reused when equal. New rows go to
/// `content`; each new node's parent also goes to `fresh`, so that a later
/// record of the round can step back through nodes not committed yet.
pub(super) fn encode_list(
    request: &ModelRequest,
    keep: usize,
    mut parent: Option<String>,
    previous: Option<&Previous>,
    content: &mut Vec<ContentRow>,
    fresh: &mut HashMap<String, Option<String>>,
) -> Result<ListReferences> {
    for message in &request.messages[keep..] {
        let body = serde_json::to_string(&Node {
            parent: parent.as_deref(),
            message: &**message,
        })
        .map_err(error)?;
        let hash = hash("node", &body);
        fresh.insert(hash.clone(), parent);
        content.push(ContentRow {
            hash: hash.clone(),
            kind: "node",
            body,
        });
        parent = Some(hash);
    }
    Ok(ListReferences {
        list: parent,
        length: request.messages.len(),
        tools: reuse(
            request.tools.as_ref(),
            previous.and_then(|previous| previous.values.tools().zip(previous.refs.tools.as_ref())),
            "tools",
            content,
        )?,
        system: reuse(
            request.system.as_ref(),
            previous
                .and_then(|previous| previous.values.system().zip(previous.refs.system.as_ref())),
            "system",
            content,
        )?,
    })
}
/// A copy of `kind` whose request carries no content, so that the rest of
/// its event serializes without copying messages, `tools` or `system`;
/// `None` for a kind without a request. The copy has no messages, and an
/// empty `tools` and `system` where the request has them: `strip` removes
/// those keys from the same places, and `Map::remove` moves the last key
/// into each, so rows keep the key order they have always had.
fn bare(kind: &EventKind) -> Option<EventKind> {
    let request = request(kind)?;
    let mut kind = kind.clone();
    *request_mut(&mut kind)? = Arc::new(ModelRequest {
        provider_id: request.provider_id.clone(),
        model_id: request.model_id.clone(),
        capability_snapshot: request.capability_snapshot.clone(),
        messages: vec![],
        tools: request.tools.as_ref().map(|_| Arc::default()),
        system: request
            .system
            .as_ref()
            .map(|_| Arc::new(SystemPrompt::Text(String::new()))),
        config: request.config.clone(),
        metadata: request.metadata.clone(),
    });
    Some(kind)
}
/// The stored form of an event or draft, serialized from its `bare` copy:
/// its request moves to `refs`.
fn strip(kind: &EventKind, mut event: Value, refs: Option<ListReferences>) -> Result<WireDraft> {
    let refs = match (request(kind), refs) {
        (Some(_), Some(refs)) => {
            let data = event["data"]["request"]
                .as_object_mut()
                .ok_or_else(|| error("model request is missing in event encoding"))?;
            data.remove("tools");
            data.remove("system");
            Some(Refs::List(refs))
        }
        (None, None) => None,
        _ => return Err(error("request lists do not match event kinds")),
    };
    Ok(WireDraft { event, refs })
}
/// The stored form of `record`, with `lists` holding the list of its event
/// request and of each request draft of its trace, in `drafts` order, and
/// `None` for an event or draft without a request.
pub(super) fn encode(
    record: &StoredEvent,
    mut lists: impl Iterator<Item = Option<ListReferences>>,
) -> Result<WireEvent> {
    let mut next = |kind: &EventKind, value: Value| strip(kind, value, lists.next().flatten());
    let source = &record.event;
    let event = match bare(&source.kind) {
        Some(kind) => serde_json::to_value(AgentEvent {
            id: source.id.clone(),
            sequence: source.sequence,
            create_at: source.create_at,
            tags: source.tags.clone(),
            kind,
        }),
        None => serde_json::to_value(source),
    }
    .map_err(error)?;
    let event = next(&source.kind, event)?;
    let mut draft = |draft: &EventDraft| {
        let value = match bare(&draft.kind) {
            Some(kind) => serde_json::to_value(EventDraft {
                id: draft.id.clone(),
                tags: draft.tags.clone(),
                kind,
            }),
            None => serde_json::to_value(draft),
        }
        .map_err(error)?;
        next(&draft.kind, value)
    };
    let trace = record
        .trace
        .as_ref()
        .map(|trace| {
            Ok::<_, dim_agent_runtime::AgentError>(WireTrace {
                event_id: trace.event_id.clone(),
                original: draft(&trace.original)?,
                steps: trace
                    .steps
                    .iter()
                    .map(|step| {
                        Ok(WireStep {
                            handler: step.handler.clone(),
                            result: step.result.as_ref().map(&mut draft).transpose()?,
                        })
                    })
                    .collect::<Result<_>>()?,
            })
        })
        .transpose()?;
    Ok(WireEvent {
        event: event.event,
        refs: event.refs,
        trace,
        batch_end: record.batch_end,
    })
}

#[derive(Clone)]
enum Content {
    Message(Arc<Message>),
    Node(Arc<Node>),
    Tools(Arc<Vec<ToolDefinition>>),
    System(Arc<SystemPrompt>),
}
/// Bytes of content bodies one read or validation pass keeps decoded. Each
/// request repeats the content of the one before, so the budget must hold
/// one request's distinct content, or every request evicts what the next
/// one needs; 64 MiB is many times a 1M-token context.
pub(super) const CONTENT_BUDGET: usize = 64 << 20;
/// Decoded request content of one read or validation pass, by reference. A
/// miss that would take the bodies past the budget first drops every entry,
/// which also drops content no later request references, such as content a
/// compaction replaced; a body larger than the budget is not kept.
struct ContentCache {
    entries: foldhash::HashMap<String, Content>,
    bytes: usize,
    budget: usize,
}
impl ContentCache {
    fn new(budget: usize) -> Self {
        Self {
            entries: Default::default(),
            bytes: 0,
            budget,
        }
    }
    fn get(&self, reference: &str) -> Option<Content> {
        self.entries.get(reference).cloned()
    }
    fn load(&mut self, connection: &Connection, session: &str, reference: &str) -> Result<Content> {
        if let Some(content) = self.entries.get(reference) {
            return Ok(content.clone());
        }
        self.fetch(connection, session, reference)
    }
    /// Loads and checks `reference`'s row and keeps it decoded.
    fn fetch(
        &mut self,
        connection: &Connection,
        session: &str,
        reference: &str,
    ) -> Result<Content> {
        let (kind, body): (String, String) = connection
            .prepare_cached(
                "SELECT kind, body FROM timeline_content WHERE session_id = ?1 AND hash = ?2",
            )
            .map_err(error)?
            .query_row(params![session, reference], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
            .map_err(error)?
            .ok_or_else(|| error(format!("missing request content {reference}")))?;
        if hash(&kind, &body) != reference {
            return Err(error(format!("request content hash mismatch {reference}")));
        }
        let content = match kind.as_str() {
            "message" => Content::Message(Arc::new(serde_json::from_str(&body).map_err(error)?)),
            "node" => Content::Node(Arc::new(serde_json::from_str(&body).map_err(error)?)),
            "tools" => Content::Tools(Arc::new(serde_json::from_str(&body).map_err(error)?)),
            "system" => Content::System(Arc::new(serde_json::from_str(&body).map_err(error)?)),
            _ => return Err(error(format!("unknown request content kind {kind}"))),
        };
        if body.len() <= self.budget {
            if self.bytes + body.len() > self.budget {
                self.entries.clear();
                self.bytes = 0;
            }
            self.bytes += body.len();
            self.entries.insert(reference.into(), content.clone());
        }
        Ok(content)
    }
}

/// Node loads one connection hold makes at most: a list walk then releases
/// the connection and continues in the next hold (§7.2 of the request
/// storage design).
pub(super) const NODE_LOADS_PER_HOLD: usize = 256;
/// Node rows readers loaded from SQLite and how they were spread over
/// connection holds, and the nodes the writer encoded. Feature `testing`
/// only.
#[cfg(feature = "testing")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeLoads {
    pub loads: usize,
    /// Holds that loaded at least one node.
    pub holds: usize,
    pub max_per_hold: usize,
    /// Loads made inside a write transaction.
    pub in_transaction: usize,
    /// Nodes the writer serialized and hashed for new lists.
    pub encoded: usize,
}
#[cfg(feature = "testing")]
impl NodeLoads {
    pub(super) fn add(&mut self, other: NodeLoads) {
        self.loads += other.loads;
        self.holds += other.holds;
        self.in_transaction += other.in_transaction;
        self.encoded += other.encoded;
        self.max_per_hold = self.max_per_hold.max(other.max_per_hold);
    }
}
/// A request's messages, tools and system prompt, rebuilt from its row.
pub(super) struct Rebuilt {
    messages: Vec<Arc<Message>>,
    tools: Option<Arc<Vec<ToolDefinition>>>,
    system: Option<Arc<SystemPrompt>>,
}
/// A list walk that ran out of node loads in its hold: the list, the next
/// node to load and its position, and the messages loaded so far, last
/// first.
struct Walk {
    list: String,
    next: String,
    position: usize,
    loaded: Vec<(String, Arc<Message>)>,
}
/// Whether a read rebuilds the request bodies it returns: the `request` of
/// `context/prepare` and `model/call` records.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum RequestBodies {
    /// Rebuild every request from its references (§7 of the request storage
    /// design).
    Rebuild,
    /// Leave every request as its record holds it: no messages and, where
    /// the record has them, no `tools` or `system`. For readers that discard
    /// these records or use only their other fields (§7.5).
    Unused,
}
/// The decoding state of one read call or validation pass, carried across
/// its connection holds: what it does with request bodies, the content cache, the
/// last event request rebuilt from a list with its node addresses (§7.1 of
/// the request storage design), and, when a hold ran out of node loads, the
/// record being decoded and its interrupted walk.
pub(super) struct Reader {
    request_bodies: RequestBodies,
    cache: ContentCache,
    kept: Vec<Arc<Message>>,
    kept_nodes: Vec<String>,
    walk: Option<Walk>,
    /// The requests of the record being decoded rebuilt so far.
    rebuilt: Vec<Option<Rebuilt>>,
    /// Node loads in the current hold.
    hold: usize,
    #[cfg(feature = "testing")]
    loads: NodeLoads,
}
impl Default for Reader {
    fn default() -> Self {
        Self::new(CONTENT_BUDGET)
    }
}
impl Reader {
    pub(super) fn new(budget: usize) -> Self {
        Self::with_request_bodies(budget, RequestBodies::Rebuild)
    }
    /// A reader that does not rebuild the requests it returns.
    pub(super) fn without_request_bodies(budget: usize) -> Self {
        Self::with_request_bodies(budget, RequestBodies::Unused)
    }
    fn with_request_bodies(budget: usize, request_bodies: RequestBodies) -> Self {
        Self {
            request_bodies,
            cache: ContentCache::new(budget),
            kept: vec![],
            kept_nodes: vec![],
            walk: None,
            rebuilt: vec![],
            hold: 0,
            #[cfg(feature = "testing")]
            loads: NodeLoads::default(),
        }
    }
    /// Starts a connection hold with a full allowance of node loads.
    pub(super) fn begin_hold(&mut self) {
        #[cfg(feature = "testing")]
        if self.hold > 0 {
            self.loads.holds += 1;
            self.loads.max_per_hold = self.loads.max_per_hold.max(self.hold);
        }
        self.hold = 0;
    }
    /// The node loads of this reader's holds so far.
    #[cfg(feature = "testing")]
    pub(super) fn loads(&mut self) -> NodeLoads {
        self.begin_hold();
        self.loads
    }
    /// Node `address`, or `None` when this hold has no node load left.
    fn node(
        &mut self,
        connection: &Connection,
        session: &str,
        address: &str,
    ) -> Result<Option<Arc<Node>>> {
        let content = match self.cache.get(address) {
            Some(content) => content,
            None if self.hold == NODE_LOADS_PER_HOLD => return Ok(None),
            None => {
                self.hold += 1;
                #[cfg(feature = "testing")]
                {
                    self.loads.loads += 1;
                    self.loads.in_transaction += usize::from(!connection.is_autocommit());
                }
                self.cache.fetch(connection, session, address)?
            }
        };
        match content {
            Content::Node(node) => Ok(Some(node)),
            _ => Err(error("request list references wrong content kind")),
        }
    }
    /// The parent of node `address`, or `None` when this hold has no node
    /// load left; for stepping back through a list.
    pub(super) fn parent(
        &mut self,
        connection: &Connection,
        session: &str,
        address: &str,
    ) -> Result<Option<String>> {
        Ok(match self.node(connection, session, address)? {
            Some(node) => Some(
                node.parent
                    .clone()
                    .ok_or_else(|| error("request list length mismatch"))?,
            ),
            None => None,
        })
    }
    /// The messages of the list ending at `list`, `length` long. The walk
    /// goes from the last node towards the first and stops at the first
    /// node the kept request has at the same position: content addresses
    /// make the rest of the list the kept request's. An event request
    /// becomes the kept request. `None` when this hold ran out of node
    /// loads; the next call with the same list continues the walk.
    fn messages(
        &mut self,
        connection: &Connection,
        session: &str,
        list: Option<&String>,
        length: usize,
        event: bool,
    ) -> Result<Option<Vec<Arc<Message>>>> {
        let Some(last) = list.filter(|_| length > 0) else {
            if list.is_some() || length > 0 {
                return Err(error("request list length mismatch"));
            }
            if event {
                self.kept.clear();
                self.kept_nodes.clear();
            }
            return Ok(Some(vec![]));
        };
        let mut walk = match self.walk.take() {
            Some(walk) if walk.list == *last => walk,
            _ => Walk {
                list: last.clone(),
                next: last.clone(),
                position: length - 1,
                loaded: vec![],
            },
        };
        let shared = loop {
            if self.kept_nodes.get(walk.position) == Some(&walk.next) {
                break walk.position + 1;
            }
            let Some(node) = self.node(connection, session, &walk.next)? else {
                self.walk = Some(walk);
                return Ok(None);
            };
            match (&node.parent, walk.position) {
                (None, 0) => {
                    walk.loaded
                        .push((std::mem::take(&mut walk.next), node.message.clone()));
                    break 0;
                }
                (Some(parent), position) if position > 0 => {
                    let address = std::mem::replace(&mut walk.next, parent.clone());
                    walk.loaded.push((address, node.message.clone()));
                    walk.position -= 1;
                }
                _ => return Err(error("request list length mismatch")),
            }
        };
        let loaded = walk.loaded.into_iter().rev();
        if !event {
            let mut messages = self.kept[..shared].to_vec();
            messages.extend(loaded.map(|(_, message)| message));
            return Ok(Some(messages));
        }
        self.kept.truncate(shared);
        self.kept_nodes.truncate(shared);
        for (address, message) in loaded {
            self.kept_nodes.push(address);
            self.kept.push(message);
        }
        Ok(Some(self.kept.clone()))
    }
    fn tools(
        &mut self,
        connection: &Connection,
        session: &str,
        reference: &str,
    ) -> Result<Arc<Vec<ToolDefinition>>> {
        match self.cache.load(connection, session, reference)? {
            Content::Tools(value) => Ok(value),
            _ => Err(error("request tools reference wrong content kind")),
        }
    }
    fn system(
        &mut self,
        connection: &Connection,
        session: &str,
        reference: &str,
    ) -> Result<Arc<SystemPrompt>> {
        match self.cache.load(connection, session, reference)? {
            Content::System(value) => Ok(value),
            _ => Err(error("request system references wrong content kind")),
        }
    }
    /// Rebuilds the request `refs` reference, or `None` when this hold ran
    /// out of node loads. A v16 row resolves each reference through the
    /// cache, and leaves no kept request.
    fn rebuild(
        &mut self,
        connection: &Connection,
        session: &str,
        refs: &Refs,
        event: bool,
    ) -> Result<Option<Rebuilt>> {
        let (messages, tools, system) = match refs {
            Refs::Messages(refs) => {
                if event {
                    self.kept.clear();
                    self.kept_nodes.clear();
                }
                let messages = refs
                    .messages
                    .iter()
                    .map(
                        |reference| match self.cache.load(connection, session, reference)? {
                            Content::Message(value) => Ok(value),
                            _ => Err(error("request message references wrong content kind")),
                        },
                    )
                    .collect::<Result<_>>()?;
                (messages, &refs.tools, &refs.system)
            }
            Refs::List(refs) => {
                match self.messages(connection, session, refs.list.as_ref(), refs.length, event)? {
                    Some(messages) => (messages, &refs.tools, &refs.system),
                    None => return Ok(None),
                }
            }
        };
        Ok(Some(Rebuilt {
            messages,
            tools: tools
                .as_ref()
                .map(|reference| self.tools(connection, session, reference))
                .transpose()?,
            system: system
                .as_ref()
                .map(|reference| self.system(connection, session, reference))
                .transpose()?,
        }))
    }
    /// Rebuilds the request a stored request row references, as the writer
    /// encodes against it; `Ok(None)` when this hold ran out of node loads.
    pub(super) fn previous(
        &mut self,
        connection: &Connection,
        session: &str,
        sequence: u64,
        refs: &ListReferences,
    ) -> Result<Option<Previous>> {
        let Some(messages) =
            self.messages(connection, session, refs.list.as_ref(), refs.length, true)?
        else {
            return Ok(None);
        };
        Ok(Some(Previous {
            sequence,
            values: Values::Stored(Arc::new(Rebuilt {
                messages,
                tools: refs
                    .tools
                    .as_ref()
                    .map(|reference| self.tools(connection, session, reference))
                    .transpose()?,
                system: refs
                    .system
                    .as_ref()
                    .map(|reference| self.system(connection, session, reference))
                    .transpose()?,
            })),
            refs: refs.clone(),
        }))
    }
    /// Decodes `wire`, or hands it back when a list walk ran out of this
    /// hold's node loads: the next call with the same record continues
    /// where it stopped. The event request is rebuilt first and becomes
    /// the kept request; each draft is rebuilt against it. A reader that
    /// does not rebuild requests loads no content and leaves each request
    /// as its record holds it.
    pub(super) fn decode(
        &mut self,
        connection: &Connection,
        session: &str,
        wire: WireEvent,
    ) -> Result<std::result::Result<StoredEvent, WireEvent>> {
        if self.request_bodies == RequestBodies::Unused {
            let requests: Vec<_> = wire
                .lists()
                .map(|refs| refs.map_or(Request::None, |_| Request::Unused))
                .collect();
            return Ok(Ok(finish(wire, requests.into_iter())?));
        }
        loop {
            let index = self.rebuilt.len();
            let Some(refs) = wire.lists().nth(index) else {
                break;
            };
            let rebuilt = match refs {
                Some(refs) => match self.rebuild(connection, session, refs, index == 0)? {
                    Some(rebuilt) => Some(rebuilt),
                    None => return Ok(Err(wire)),
                },
                None => None,
            };
            self.rebuilt.push(rebuilt);
        }
        let requests = std::mem::take(&mut self.rebuilt)
            .into_iter()
            .map(|rebuilt| rebuilt.map_or(Request::None, Request::Rebuilt));
        Ok(Ok(finish(wire, requests)?))
    }
}
/// What a read made of one stored request.
enum Request {
    /// Rebuilt from its references.
    Rebuilt(Rebuilt),
    /// Present in the record, not rebuilt because the read does not use
    /// request bodies.
    Unused,
    /// The record or draft is not a request.
    None,
}
/// The record of `wire` with each request settled in order: the event's,
/// then each draft's of its trace.
fn finish(wire: WireEvent, mut requests: impl Iterator<Item = Request>) -> Result<StoredEvent> {
    let mut event: AgentEvent = serde_json::from_value(wire.event).map_err(error)?;
    settle(&mut event.kind, requests.next())?;
    let mut draft = |wire: WireDraft| {
        let mut draft: EventDraft = serde_json::from_value(wire.event).map_err(error)?;
        settle(&mut draft.kind, requests.next())?;
        Ok::<_, dim_agent_runtime::AgentError>(draft)
    };
    let trace = wire
        .trace
        .map(|trace| {
            Ok::<_, dim_agent_runtime::AgentError>(TimelineTrace {
                event_id: trace.event_id,
                original: draft(trace.original)?,
                steps: trace
                    .steps
                    .into_iter()
                    .map(|step| {
                        Ok(TraceStep {
                            handler: step.handler,
                            result: step.result.map(&mut draft).transpose()?,
                        })
                    })
                    .collect::<Result<_>>()?,
            })
        })
        .transpose()?;
    Ok(StoredEvent {
        event: Arc::new(event),
        trace,
        batch_end: wire.batch_end,
    })
}
fn settle(kind: &mut EventKind, request: Option<Request>) -> Result<()> {
    match (request_mut(kind), request) {
        (Some(target), Some(Request::Rebuilt(rebuilt))) => {
            let target = Arc::make_mut(target);
            target.messages = rebuilt.messages;
            target.tools = rebuilt.tools;
            target.system = rebuilt.system;
            Ok(())
        }
        (Some(_), Some(Request::Unused)) | (None, Some(Request::None)) => Ok(()),
        _ => Err(error("event request references do not match event kind")),
    }
}
