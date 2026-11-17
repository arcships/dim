# Tool origin read

ToolExecution owns the nonmutating `session.tool.get` method. Parameters are
`sessionId`, `branchId`, `requestId`, `callId` and optional `through`. The reply is
`branchId`, the fixed published `through`, and nullable `call`. A present call is
the existing `ToolUseData` object: its turn, request and complete `ToolCall`.
Null means no committed complete call has that identity at the cutoff; it does
not mean an empty argument object. A partial call that never supplied arguments
can have a terminal result and no complete call.

The Session part ToolMethods declares TreeCommands and SessionMethods. It queries
the addressed branch's own facts, including ended branches, by request and call id,
selects only the matching data and retains no event list. It never invokes the
historical tool. Request cancellation ends the query.

Clients keep request and call identity on a result whose origin is outside the
transcript window. That result renders with its existing status and content;
its missing name or arguments do not block display or current-state controls.
The native UI requests the origin only when the user opens tool details. ACP
emits the existing placeholder call with `rawInput: null` and its original call
identity, without automatically fetching the missing origin. No client extends
the transcript to find it. Reads belong to the attachment that started them;
replacement and row eviction discard their results. A failed detail read is
reported without blocking the existing result.

```text
window: result(c1) -> show result(c1)
expand details(c1) -> session.tool.get(r1, c1) -> install name and arguments
```

## Rule check

ToolMethods is a stateless Session read part, not a Capability. It owns no branch
business state, writes no fact, subscribes to no event, starts no background task
and introduces no context input. Its only work belongs to the admitted request.
It uses the existing facade/property-query mechanism and typed call object.
These choices satisfy principles §4 rules 1–9 and §1, and ADR 002 D1–D5; before
callbacks, execution, configuration and I/O effect settlement do not apply.

## Requirements raised to mechanism owners

None. Timeline's field and identity queries suffice. The client's requirement is
owned by [the bounded-window client contract](clients.md#opening-a-conversation-with-a-bounded-window).
