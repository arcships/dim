# Model startup

## Scope

The CLI accepts a temporary model choice and connection for one Session, using
Claude Code's startup conventions with Dim-owned environment names and explicit
multi-protocol selection. It does not change saved Provider records, model
preferences, workspace defaults or the user's Collaboration policy. Ordinary
startup without overrides keeps its existing behavior.

Tracked in [#997](https://github.com/arcships/dim/issues/997).

This design raises requirements to [Provider](appserver/provider-management.md),
[model configuration](model-configuration.md),
[Collaboration](subagent-model-policy.md) and
[ContextCompaction](plugins/ContextCompaction.md). Those owners retain their
contracts; this document does not introduce a second model backend.

## Inputs and precedence

| Input | Contract |
| --- | --- |
| `--model MODEL` / `DIM_MODEL` | Select the model; the explicit flag wins. A configured Provider can still be named with `--provider`. Without one, use the effective Provider if it offers the exact ID, otherwise require a unique available match. Never pick an arbitrary duplicate. |
| `--effort LEVEL` / `DIM_EFFORT_LEVEL` | Set reasoning effort; the explicit flag wins, then the environment, then the exact model's saved preference, then its declared configuration. Values are validated by the model owner, not a CLI list tied to one vendor. |
| `DIM_BASE_URL` | API base URL, including a gateway path prefix, not a completed request URL. |
| `DIM_API_FORMAT` | Explicit protocol for a direct connection: `anthropic`, `openai-chat`, `openai-responses`, `openai-compatible`, `gemini`, `xai` or `xai-responses`. Do not infer it from a model name or URL. |
| `DIM_API_KEY` | Key sent through the selected protocol's existing default authentication mapping. |
| `DIM_AUTH_TOKEN` | Bearer authentication, independent of the message format. |
| `DIM_CUSTOM_HEADERS` | Additional headers, one `Name: Value` per line. Parse names case-insensitively; reject invalid or repeated names and conflicting generated authentication headers. Split a line at its first colon only. |
| `DIM_MAX_CONTEXT_TOKENS` | Positive safe integer declaring the adopted context capacity, not a promise that the server supports a larger window. |
| `DIM_MAX_OUTPUT_TOKENS` | Positive safe integer declaring output capacity where the catalog does not declare it; it cannot exceed the adopted context capacity. |
| `--autocompact auto\|TOKENS` / `DIM_AUTO_COMPACT_WINDOW` | Separate compaction window, not model capacity. The explicit flag wins. Accept positive token counts with optional `k`/`m` suffixes; `auto` uses the existing model-derived threshold. Do not impose Claude's vendor-specific 100K minimum or 1M maximum on Dim models. Reserve the existing output allowance. |

Flags and environment overrides are read only for Session execution, not help,
version, login or management commands. `--help` and `-h` explain the supported
inputs, protocol-specific authentication, precedence and examples without
reading secrets or starting a daemon. The executable's name remains `dimcode`.

`DIM_BASE_URL` and `DIM_API_FORMAT` together select direct startup. A partial
connection is an error, not a request to alter an existing Provider. Reject an
explicit `--provider` or `--model-config` combined with direct startup. Both
credential variables set is an error; neither is required for an anonymous gateway
or one using explicit custom authentication headers. A custom address never
receives credentials from a saved Dim account or another Provider implicitly.

The CLI parses `DIM_CUSTOM_HEADERS` text into a header map; protected-local
Provider admission accepts that map. Both boundaries use the same HTTP header
name/value grammar, lowercase names, trim only surrounding space and tab from
values, preserve embedded colons and other valid content, and reject repeated
names and conflicts with generated authentication.

An explicit CLI value is applied once at startup. Later in-Session configuration
commands can replace it. Empty values, invalid URLs, headers and token counts
fail before Session creation. Missing capacity or reasoning declarations are
reported explicitly; unknown model capabilities are not guessed from its ID.
Use the existing model descriptor/configuration path for declarations the
protocol alone cannot supply. AWS-signed Bedrock startup is not part of the
simple HTTP credential contract; existing Provider configuration remains usable.

`--effort` without direct startup uses the existing `session.create`
`modelPreferences.reasoningEffort`. An explicit value replaces the saved effort
but preserves saved service tier and other independent preferences. It does not
write `provider.preferences.update` or `provider.select`. Resume reconfiguration
follows the explicit adoption contract below; startup options must not be
silently ignored.

`--model-config` uses the existing nonproduct model resolver, not the product
Provider catalog. Its ordinary creation and preference-update paths do not
support reasoning preference validation. Reject `--effort` / `DIM_EFFORT_LEVEL`
before daemon launch for both creation and resume, directing the user to the
API-key model's generation settings in the configuration file. Likewise reject
automatic-compaction and capacity startup controls with `--model-config` rather
than ignoring them or reporting a Provider reload. Ordinary configured-model
selection without these controls retains its existing behavior. A model override
with `--model-config` requires an explicit `--provider`, for both creation and
resume; reject a bare override before connecting instead of returning an RPC
field-pair error.

## Session ownership and security

Direct startup has a Session-owned connection binding. The CLI validates its
external input and passes it through the protected-local Session boundary to
Provider; it does not mutate the daemon's environment or generate a plaintext
configuration file containing secrets. An already running daemon accepts the
same boundary, so the current invocation's configuration is not lost.

Daemon spawning always strips the ten Session startup environment variables,
including for management invocations that do not parse them. This changes only
the spawned daemon's environment, not the current CLI process or its hosted
workers. Other environment references used by legacy model configurations are
preserved.

Provider validates the protocol, connection and model declarations and produces
the existing complete model selection. Connection resources and credentials are
I/O state, not Timeline business facts. Register their cleanup when acquired;
failed Session creation releases them. Closing the owning Session cancels and
settles its requests before releasing credentials. Idle unload must not revoke
an open Session's ability to continue.

Only the non-secret adopted model configuration and a durable connection-source
identity may be persisted. Do not persist a runtime handle as that identity.
Provider defines its source declaration/rebinding contract before implementation;
a later launch supplies the matching credentials before a restored Session can
run. A missing binding fails explicitly, without falling back to saved account
credentials. Credential rotation cannot silently change the declared destination.
Headers may contain secrets and are never assumed safe merely because they are
not named Authorization. No credential or header value enters Timeline events,
request metadata, tracing, model logs, management readback or exports.

## Inheritance

This startup mode makes newly created native Collaboration members inherit the
requesting branch's complete adopted selection and connection source, regardless
of the ordinary user `agent`/`fixed` policy or a named Agent's model. It does not
write the user policy. Member prompts, tools and permissions retain their
existing contracts. Existing members are not reconfigured. Nested delegation
remains disallowed where Collaboration already disallows it.

New branches inherit the configuration at their parent cut. Later changes on
main affect newly created work from main, not existing branches. Compression
uses its branch's model, adopted capacity and connection source. The summary
retains its own prompt, chunking, output allowance and reasoning policy: it may
override the branch effort, including the existing default of turning thinking off.
Automatic and manual compaction follow the same contract.

Resuming without overrides keeps the recorded adopted configuration; a missing
connection requires explicit rebinding. Resuming with explicit overrides adopts
them through the existing configuration path before accepting the first input.
It does not rewrite existing branch history or reconfigure existing members.

When a daemon does not advertise `session.create.modelFromSession`, a new
Session may use the ordinary configured-model fallback only for a saved
Provider. Read the public management listing to establish that membership;
never infer a temporary source from the spelling of its identity or query that
identity as a saved Provider. If it cannot be copied, explain how to restart a
daemon supporting adopted-model copying and rebind the temporary connection,
or select a configured Provider model when Provider management is available.

Automatic title generation still uses the auxiliary model path's catalog
`ModelId`, which carries no startup connection binding. A temporary direct
Session may therefore have no generated title; manual `/rename` remains
available. The TUI's image/service-tier hints also still use the catalog and may
be incomplete for temporary sources. This does not remove the adopted
capability declaration used by `/effort` and `/fast` preference commands.

```text
CLI flags + DIM_* -> protected-local Session creation
                    -> Provider validates model + connection
                    -> main adopts complete selection
                       +-- new branch: parent's cut
                       +-- new member: requesting branch
                       `-- compression: owning branch, own thinking policy
```

## Requirements raised to mechanism owners

| Owner | Requirement and boundary |
| --- | --- |
| Provider / Session creation | Existing Provider records and configured daemon profiles are shared, so neither can implement a Session-local connection override. Define one typed direct-connection admission and durable non-secret rebinding contract, using the existing connection resolver and request driver. Reject direct-connection secrets over remote/public transports. |
| Provider | Resolve a bare model ID without ambiguity; apply explicit capacity and effort at the external boundary. Keep the adopted capability snapshot and route metadata consistent. |
| Collaboration | Add a Session-local startup inheritance choice ahead of ordinary user policy, using the existing captured requesting-branch selection. Preserve ordinary startup behavior. |
| ContextCompaction | Carry the branch's model and connection source into summary requests while retaining the strategy's reasoning policy. Provide the temporary compaction-window input without changing model capacity or the ordinary off default. |
| CLI / Prompt | Read startup inputs once, expose help, and complete explicit resume adoption before input. Reuse existing create/configure paths and typed preferences. |
| Model log | Redact every header value for direct startup requests, including input options and wire exchanges, without changing sent headers or ordinary Provider recording. Keep the existing recording format, bodies and timing. |

No new Timeline event, core dispatch mechanism, plugin registry, HTTP backend or
global environment mutation is required. Any persisted schema addition still
requires the owning module's explicit compatibility decision.

## Verification

Exercise actual CLI/transport/model flows, not just argument parsing:

- Flags override environment and saved effort; unrelated saved preferences stay.
- Unsupported effort fails without a Session or persisted preference change.
- Each supported format reaches a local fixture with the correct path, body,
  authentication and headers. Include a path prefix and colons in header values.
- Invalid/partial connections, repeated headers, auth conflicts and invalid
  capacity fail without exposing secret values or starting external work.
- Two Sessions on one daemon retain different connections. A preexisting daemon
  receives new startup input; a missing restored binding never changes endpoint.
- A conflicting Agent model and user fixed policy cannot override this mode's
  member selection. Parent-cut branches retain their own configuration.
- Automatic and manual summaries use the branch's model/connection, their own
  reasoning policy and intended output allowance. Check persisted/exported data
  for secret leakage.
- Help/version work with invalid DIM_* input and create no product state.

## Rule check

| Rule | Application |
| --- | --- |
| Principles §1 / ADR 002 D1 | CLI owns parsing, Provider owns connection I/O, Prompt owns adopted model state, Collaboration owns member selection and ContextCompaction owns summaries. No new Capability is introduced solely for CLI flags. |
| §4.1–3 | Configuration uses existing commands, handoff and adoption, not a before callback; already admitted calls keep their connection. |
| §4.4–5 | No business branch in the core; adopted configuration retains its existing atomic fact publication. |
| §4.6–7 | No new context-input path; consumers use their owners' public typed interfaces. |
| §4.8 | Cross-module requirements are named above; the owning designs must be updated with their implementation. |
| §4.9 | Persist required non-secret adopted configuration only; credentials are not facts. |
| ADR 002 D2–3 | No wiring plugin or second catalog; existing owner dependencies remain. |
| ADR 002 D4–5 | Startup is business input, not loader configuration; boundary data and errors are language-neutral. |

Ablation: reuse the existing preferences request, complete model selection,
branch cut, Collaboration inherit path and summary reasoning mode. Only the
Session-local connection binding and its source/rebinding boundary add a new
responsibility; shared Provider mutation cannot substitute for them safely.
