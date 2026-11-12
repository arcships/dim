# AgentSettings

Status: Draft. Parent: [agent environment awareness](../agent-environment.md). Issue: [#1022](https://github.com/arcships/dim/issues/1022). Rules: [design rules](../../architecture/rust-agent/principles.md) §1, §4 and §5, [ADR 002](../../adr/002-plugin-scope.md), [ADR 003](../../adr/003-approval-scope.md), [ADR 009](../../adr/009-permission-decides.md), [settings service](../appserver/settings-service.md).

AgentSettings exposes one `settings` tool. In v1 the tool lists and reads user settings on demand; a write path is deferred. It is not a per-setting tool collection, not a general document editor and not a second Settings store. Automatic awareness is [EnvironmentSettings](EnvironmentSettings.md); this document owns only the tool.

## 1. Decisions

- Exactly one tool, named `settings`. No `settings.execution.set` and no per-setting tools.
- Parameters are `op` and `name`. No `source`, `session` or visibility parameter.
- The setting identity is the model-facing `name`; internal routing is hidden.
- `list` returns the available settings, their descriptions, types, current values and whether they are writable.
- `get` returns one setting.
- `set` is deferred until a setting is writable; v1 has no write path.
- A refusal is a tool result, not a missing tool.
- Secrets and `writeOnly` fields never appear in `list`, `get`, results or logs.
- No per-setting tool; no tool-level security level. The read tool uses the `session` risk, so it does not prompt under the product default.

## 2. Tool contract

```json
{
  "name": "settings",
  "description": "Read the user's settings. Call op=list first to see each setting's name, meaning, current value, type, and whether it is writable. op=get reads one setting. Never guess a setting name.",
  "parameters": {
    "type": "object",
    "properties": {
      "op": {
        "type": "string",
        "enum": ["list", "get"],
        "description": "list: show available settings. get: read one."
      },
      "name": {
        "type": "string",
        "description": "A setting name returned by list."
      }
    },
    "required": ["op"],
    "additionalProperties": false
  }
}
```

`list` takes no `name`. `get` requires `name`.

The tool has no `source`, `session`, revision, path or document parameter. The setting name is the only identity the model supplies.

## 3. Results

### list

```json
{
  "settings": [
    {
      "name": "execution.sandbox",
      "description": "Run shell commands inside the sandbox.",
      "type": "boolean",
      "value": true,
      "writable": false
    },
    {
      "name": "execution.network",
      "description": "Allow network access for new processes. Only meaningful when the sandbox is on.",
      "type": "boolean",
      "value": false,
      "writable": false
    }
  ]
}
```

Every setting carries a description and type. `allowed` is present when the value is an enum. In v1 every setting has `writable: false`; no write operation is exposed.

### get

```json
{"op": "get", "name": "execution.sandbox"}
```

```json
{
  "name": "execution.sandbox",
  "description": "Run shell commands inside the sandbox.",
  "type": "boolean",
  "value": true,
  "writable": false
}
```

### Errors

| Error | Meaning |
| --- | --- |
| `unknown` | No setting has this name; call `list` |
| `invalid` | The read arguments are malformed, the operation is unsupported, or `get` has no `name` |
| `denied` | Permission or approval refused the call |

A refused read changes nothing. The result states the reason; the model does not infer success from its own text.

## 4. Registration

- The process plugin that owns the first setting registers one Session part, `SettingsTool`, and one tool contribution named `settings`. In v1 that is `ExecutionSettingsPlugin`; a later shared settings owner can register the same single tool.
- Each setting module registers a handler with the dispatcher:

```text
name
description
type
allowed
get
writable
```

- The dispatcher maps `name` to the handler. `source` and document paths are internal and never exposed.
- One tool; no per-setting tool contributions.
- The tool is registered with the existing `toolRegistry` and `ToolContribution`; it is admitted and executed like every tool.
- The read handler calls the setting module's read port. A future write path adds a typed setter and separates the write risk.

Service faces follow the [owner catalog](../../architecture/rust-agent/generated/catalog.md#services): v1 consumes the process `executionSettingsHost`, `internal(legacy-no-consumer)`, and the Session `toolRegistry` data face. The tool's input and result are data; its Rust part is not leaf-ready.

## 5. Permission

- Permission sees one tool name: `settings`.
- `op=list` and `op=get` are normally allowed.
- A future write design must define its risk and approval behavior; v1 has no `set`.
- The tool carries the `session` risk. Permission decides whether the read is allowed; `writable: false` grants no write authority.
- A denied call returns `denied`.

## 6. Boundaries

- No arbitrary Settings document, JSON Pointer or file path.
- No second Settings store.
- No plugin reload; structural configuration is not this path.
- The tool result is a read at the time of the call. Automatic awareness comes from an [EnvironmentSettings](EnvironmentSettings.md) snapshot.
- The tool writes no setting, including when a call is denied or cancelled.

## 7. Requirements raised to mechanism owners

None to the runtime or rutis.

## 8. Rule check

| Rule | How this design complies |
| --- | --- |
| §1 Parts bounded; Capability | The setting module owns the value; `SettingsTool` is a normal Session part, not a Capability: it only provides a read tool. |
| §4 rule 1 Command events | The tool reads the setting module's state and performs no write. |
| §4 rule 2 Handover | No handover. |
| §4 rule 3 Before callbacks | No wait in an event handler; the tool call is its own task. |
| §4 rule 4 Core carries no business meaning | No core branch; the dispatcher and setting modules own the behavior. |
| §4 rule 5 Batch | No write or Timeline batch. |
| §4 rule 6 Context from consumed events | The tool result enters as a tool result; the state snapshot enters separately. |
| §4 rule 7 Public services | The setting module's read port; one tool registered with `toolRegistry`. |
| §4 rule 8 Infrastructure unchanged | No Timeline, Dispatcher, envelope, assembly or facade change. |
| §4 rule 9 Timeline holds decision facts | Configuration does not enter the Timeline. |
| ADR 002 D1 | The setting module owns the value; the tool is a Session entry. |
| ADR 002 D2 | No adapter plugin; one tool and a dispatcher, not a second store. |
| ADR 002 D3 | The tool depends on the setting module's action; no core depends on the tool. |
| ADR 002 D4 | Business configuration stays with its module; no loader row. |
| ADR 002 D5 | The tool input/result and the setting action are data; Rust is a binding. |
| ADR 003 / ADR 009 | The read is an agent action admitted by Approval and Permission with the `session` risk. |

## 9. Verification

| Scenario | Expected |
| --- | --- |
| `list` | Names, descriptions, types, values, writability; no secrets |
| `get` a known setting | The current value and type |
| `get` an unknown setting | `unknown`; not an empty value |
| `set` | Not in the v1 schema |
| Secret | Never in schema, result, log or snapshot |
| Two tools declared | Rejected; the design allows exactly one `settings` tool |

## 10. Open decisions

1. Which settings beyond execution to expose.
2. The write contract, risk and approval behavior if a writable setting is introduced.
