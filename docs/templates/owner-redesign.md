# Owner redesign

Use this shape when an existing owner's public surface is redesigned under
[design rules §7](../architecture/rust-agent/principles.md#7-所有者与操作). It has two parts. The design
part is durable and goes into the document that holds the owner's design. When the plugin's page
under `docs/architecture/rust-agent/plugins/` links a design under `docs/design/plugins/`, the
linked design is that document. Otherwise it is the plugin's architecture page, or its page under
`docs/design/plugins/` when it has no architecture page. An owner with neither gets
`docs/design/plugins/<Owner>.md`,
created from the [design template](design.md). A plugin with several owners has one document per
owner; the plugin's row in the index of the tree holding its design links each of them. The delivery part is
temporary and goes into the owner's issue. A new owner uses the [design template](design.md) alone.

Write conclusions first. Name things directly instead of referring back. Do not restate rules or
describe how the document was produced. Link evidence next to the claim it supports.

## Design part

### Use and scope: responsibility split

- The owner's responsibilities, in the owner's own vocabulary.
- The responsibilities that stay with callers as business logic.
- One row per caller requirement:

| Requirement | Can callers compose it from existing operations without breaking the owner's invariants? | Handling |
| --- | --- | --- |

An operation or parameter exists only where the answer is no.

### Caller example

The caller code after the change for each main intent. It shows the owner operations it calls and
the errors it handles, and nothing of the owner's internal mechanisms.

### Own contract

Each operation: the intent it serves, request and reply data, cancellation, and error kinds named by
what the caller can do next. For each operation that holds a resource or waits, a flow covering the
normal path, the caller cancelling or unloading, and the owner closing or reloading, with the party
that settles each resource.

Ids, values, read-only views and callbacks cross owners. Where an object still has to be handed out,
say why none of those works, and link the design shared by the plugins involved that fixes who
creates, holds, hands over and cleans up the object, and what happens when any side closes or reloads.
A resource handle crosses only as a whole transfer: the receiver becomes its owner and settles it.

The client protocol and user-visible behavior stay as they are. List any change with its reason and
the affected clients.

### Supersedes

The clauses of other designs that define this owner's surface, each replaced by a link to this
contract or removed in the same change. Other owners' clauses stay.

### Exceptions with reasons

One line each, only for a deviation §7 allows, such as an object handed out with its shared
lifecycle design. A current shape that differs from §7 is a gap for the delivery part, not an
exception.

### Rule check and requirements raised to mechanism owners

As in the [design template](design.md).

### Binding appendix

The `#[rutis::service]` block on the owner type for the owner's own face; for what the owner
registers with a mechanism registration point, the registration call instead. Then the request,
reply and error types. Error types serialize to the codes in the error table, callbacks are
direct parameters, and an operation bound to the calling row takes the caller.

## Delivery part

Written in the owner's issue; it is history once the change lands.

- **Deletion list**: public methods, types, wrappers and catalog entries that must not exist after the
  change.
- **Caller migration**: one row per call site, `caller | current call | target operation`.
- **Implementation task**: rewrite the owner's public surface; do not add an interface beside it.
  Read the responsibility split, write the callers' target code as flow tests, make the old public
  methods `pub(crate)` and use the compile errors as the migration list, implement the operations,
  then migrate every caller. The owner's new operations, every caller and the removal of the old
  surface are one change, with no period where old and new coexist.
- **Acceptance**: the flow tests pass; no cross-crate reference to a deleted name remains; the
  catalog has no `internal(reason)` entry for the owner; the diff removes the old surface.
