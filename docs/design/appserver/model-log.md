# Model request log

The user's switch for recording the wire exchange of every model request
([#701](https://github.com/arcships/dim/issues/701)). The recording format is
aimux's RFC-0023 JSONL (`vendor/aimux/aimux-core/src/recording.rs`).

## Scope and owners

| Owner | Responsibility |
| --- | --- |
| ModelLog, process | Register the `model-log` Settings source, start and stop the recorder, follow its changes |
| Aimux, process | Bind a request to the recorder that is on when the request is built, and close the recording when its stream ends |
| Settings service | Validate and persist `model-log.json`, user scope only |

The switch is user scope only: recordings hold prompts, code and tool output, so
project content must not turn them on.

## Source

`model-log.json`:

```json
{
  "enabled": true
}
```

`enabled` defaults to false. When it is true every model request appends one
line to the daemon's `model-log/recordings.jsonl`; when it is false new requests
are not recorded. A request already in flight keeps the recorder it started with
and finishes into the file. The recorder is replaced on `settings.changed` and
stopped when the plugin unloads; one recorder serves the whole process, and the
file has no rotation or size limit.

The directory is created with mode 0700 and the file with 0600 (Unix). A product
daemon writes `<home>/model-log/recordings.jsonl`; one started from
`--model-config` writes `<home>/rust/model-log/recordings.jsonl`.

With file-backed settings the document is `<home>/model-log.json`; with native
storage it is the same document in the settings table, written through
`settings.update` with `source: "model-log"` and `scope: "user"`.

The line client and the terminal UI carry the switch:

- `dimcode recording` prints whether it is on; `dimcode recording on|off`
  switches it and prints the resulting state (the file path when on).
- `/settings recording` and `/settings recording on|off` switch it from the
  terminal UI without printing.

The line client reads the current revision, writes the new document and prints
the state the daemon stored; a daemon without the source reports the settings
error.

A line holds one call: the input prompt and options, the provider identity, one
entry per HTTP exchange (request headers and body, response status, headers and
body, chunk count, `latency_ms`, `ttfb_ms`) and the outcome (`success`, `error`,
`incomplete`, `cancelled`, usage). Aimux redacts a field whose name is `cookie`
or `set-cookie`, ends in `token`, `key` or `auth`, or contains `apikey`,
`authorization` or `secret`; configured connection headers that do not match are
recorded verbatim.

[Temporary startup connections](provider-management.md#session-local-startup-connections)
treat every header value as private, regardless of its name. Aimux binds a
per-request recorder that redacts all input, request and response header values
before handing them to the existing recorder. Inherited member and summary
requests carry the same source declaration and the same protection. Wire bodies,
timing and outcomes remain recorded; ordinary Provider recording is unchanged.
The JSONL schema and the process-wide switch do not change.

## Reading it

- `latency_ms` is the time from sending the request to receiving the response
  headers.
- `ttfb_ms` is set to the same value; it is not the arrival of the first stream
  chunk.
- `response.body` is the stream's SSE frames as received.

## Failure

When the switch is on and the recorder cannot start (for example an unwritable
home), the plugin fails at startup. A later settings change that cannot be
applied is logged as a warning and leaves the previous state.

## Observation layers

[ADR 005](../adr/005-observability.md) D1 keeps the Timeline and process logs;
[ADR 016](../adr/016-model-request-log.md) supersedes its "no third channel" for
this opt-in record and leaves the other two layers unchanged. The record is never
read back into Dim.

## Verification

- `settings_flow::model_log_switch_starts_the_recorder`: enabling the source
  starts the recorder and creates the file with the private modes.
- `model_flow::recorded_requests_write_their_wire_exchange`: a turn records its
  request, exchange, provider and outcome as one line; disabling and enabling
  records the next turn.
- `model_flow::recorded_failures_and_cancellations_close_their_record`: a 401
  and a cancel before response headers each end with one complete line.
- `cli_flow::recording_on_switches_the_model_log` and
  `recording_reports_whether_it_is_on`: the line client reads and writes the
  source and prints the state.
- `ui::tui::tests::settings_recording_sends_its_words`: `/settings recording`
  reaches the session host with its arguments.
