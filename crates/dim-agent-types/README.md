# Agent wire types

Typed JSON contracts frozen at agent-v4 commit
`8565538d5ff70a63bbf10cb9ad9198107244bac0` (!1912).

This deliberately uses the new !1911/!1912 contract: `branch/create` and
`branch/start.data.createEvent`. The target branch has since merged that
contract and moved ahead of the frozen baseline. Live `events.ts` additionally
declares new events (`prompt/changed`, `context/scope`, `context/configured`,
`branch/create-failed`, `control/stop`) and payload-level changes on existing
events: `input/message` moves to `ContextInput` (optional `text`, new optional
`context`), `context/prepare` and `model/call` add optional
`configuration`/`prompt` references, `context/compaction` adds optional
`sourceEvent`/`baseline`, and `ToolCallResult.result` (the `tool-result`/
`tool-completed` payload) becomes the `ToolResult` subtype `PromptToolResult`
with an optional `prompts` field. These added optional fields can fail the
frozen `deny_unknown_fields` deserialization. The frozen compatibility checks
do not assert compatibility with that live checkout.
Re-pin the baseline after this prototype merges and review the additional
declarations explicitly.

The crate represents all 22 original `AgentEvent` kinds and their request, message,
tool, usage, budget, and compaction payloads. Known fields use Rust types;
only the original extension fields accept arbitrary JSON. Optional fields
preserve omission and reject explicit null. Event sequences start at one;
sequence and Unix-millisecond fields enforce JavaScript's safe integer range.
Other TypeScript `number` fields use finite `f64` without inventing integer
constraints. JSON object order and equivalent numeric spellings are immaterial.

`ModelRequest.messages` is `Vec<Message>`. Call and preparation events hold
`Arc<ModelRequest>` so the same fixed request can be shared without duplicating
it. This crate has no runtime or history cache. Wire serialization uses the
original JSON arrays and objects. Large, uncommon compaction diagnostics are
boxed; `cargo run -p dim-agent-types --example layout` reports actual type sizes.

## Public API

Internal types are re-exported at the crate root. `EventKind` is a closed union of
27 typed payload variants, with `key()` returning names such as `input/message`
and `step/model-start`. `EventDraft::new(kind)` sets omitted tags. The Timeline
producer assigns `AgentEvent { id, sequence, create_at, tags, parent_call_id?, kind }`; the crate
does not allocate event identities or read the clock. Serde preserves the
original flat `type/name/data` object.

The Rust-internal controls are `stop/interrupt`,
`approval/request`, `approval/respond`, `approval/cancelled`,
`prompt/configure-request`, and `prompt/configure-result`. They use the common
event identity, sequence and timestamp; `is_control()` identifies them. They
are not members of the frozen TypeScript union. Their boundary contract is
tested separately in `tests/control_contract.rs`.

Internal `BranchCreateData.at` is required and selects a fixed sequence or the
committed create event. The `v1` module retains the original 22-kind wire union
and original branch payload. Its borrowed output adapter filters controls and
omits `at` without creating another history or copying event bodies. Frozen
fixtures use these v1 types: 42 samples covering 22 kinds. Old wire events are
not converted into executable internal branch commands.

`Message::text(Role::User, text)` and `ToolResult::text(content)` are plain value
constructors. `ToolResult::error(content)` explicitly sets `isError: true`.
Model and turn terminal data contain typed outcome enums; their `status` and
associated fields remain flat on the wire. `Option` fields represent genuine
omission, while an omitted input mode has the documented queue meaning.

Sequence and timestamp fields are `u64`; boundary serialization and parsing
check their permitted ranges. Producers must maintain valid values internally.
Equivalent JSON number spellings such as `1`, `1.0` and `1e0` are accepted for
safe integer fields.

## Compatibility checks

Run the regular Rust contract tests from the Rust 1.100.0 workspace:

```sh
cargo test --locked -p dim-agent-types
```

The frozen fixtures in `tests/fixtures/baseline/` preserve the original public wire samples and provenance. Rust contract tests deserialize and serialize those samples, reject malformed input and check public representation. The monorepo TypeScript generator is not shipped or required.

Provider diagnostics use generic `ProviderPresentation` strings. Provider-specific reason classification and account authorization policy belong to an external managed plugin.
