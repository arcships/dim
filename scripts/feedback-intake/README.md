# Internal feedback intake

This is a small, single-process dogfood receiver, not a public or production service. Run it as a dedicated, unprivileged operator account on a trusted POSIX host, using Python 3.9+ and the standard library. Keep the store on a local filesystem that honors SQLite locking and sync. The server always binds IPv4 `127.0.0.1`; use SSH forwarding, not a reverse proxy, public bind or shared tunnel. Anyone with the bearer can submit cases, but HTTP cannot retrieve or delete them.

## Run

Have your process launcher inject `DIM_FEEDBACK_TOKEN` into **the server's environment**. Use a random bearer of at least 32 ASCII bytes (for example, a URL-safe encoding of 32 random bytes). Do not pass the token on the command line or print it. Then, on the receiver host, run:

```sh
python3 scripts/feedback-intake/receiver.py --store "$HOME/feedback-intake-store" serve --port 8765
```

On the client, forward the port from a trusted host (replace `operator@receiver-host`):

```sh
ssh -N -o ExitOnForwardFailure=yes -L 127.0.0.1:8765:127.0.0.1:8765 operator@receiver-host
```

Send `POST /v1/feedback` to `http://127.0.0.1:8765` through that tunnel with `Authorization: Bearer …`, `Content-Type: application/json`, and a single explicit `Content-Length` of at most 65,536 bytes. Chunked and compressed bodies and `Expect: 100-continue` are not supported. The UTF-8 JSON body has exactly these fields, with no duplicate keys:

```json
{"schemaVersion":1,"caseId":"b66eff31-62e4-4119-b214-dd50faf01dfd","allowEval":false,"note":"synthetic example","sample":{"example":true}}
```

`caseId` is a lowercase, hyphenated UUID v4, `allowEval` is a boolean, `note` a string, and `sample` a nonempty object. A committed first write returns `201` with `{"caseId":"…"}`. Retrying precisely the same bytes while the case is live returns `200` with the same payload; different bytes for the same ID (even just whitespace) return `409`. Retries do not renew retention. Request text, headers, and bodies are not logged; error bodies contain only a status code.

## Local operator commands

Run under the same OS account, on the receiver host. No bearer is needed for these commands:

```sh
CASE_ID=b66eff31-62e4-4119-b214-dd50faf01dfd
python3 scripts/feedback-intake/receiver.py --store "$HOME/feedback-intake-store" get "$CASE_ID"
python3 scripts/feedback-intake/receiver.py --store "$HOME/feedback-intake-store" get "$CASE_ID" --for-eval
python3 scripts/feedback-intake/receiver.py --store "$HOME/feedback-intake-store" delete "$CASE_ID"
```

`get` writes the original raw bytes to stdout only for a live case. Use `get --for-eval` for evaluation exports; without explicit `allowEval: true` it fails without emitting any bytes. Plain `get` is for restricted operator review, not an alternative evaluation route. Avoid copying raw output into logs or shared files.

## Storage and withdrawal policy

- Use a symlink-free path with an existing trusted parent. The store is created as `0700`; the SQLite database and any rollback journal are private (`0600`). Existing unsafe paths/files are rejected, not repaired. Keep the store outside source control and do not grant access via ACLs. Same-UID processes, root, and administrators of the host are trusted.
- Raw bytes expire 30 days after the first accepted write, based on host wall-clock time. Live service checks for expiry about once a second when it can service its loop, including while idle. Startup and operator commands also purge; expired data is never returned by `get` even if cleanup has not run yet. While stopped, there is no background cleanup; keep it running if cleanup deadlines matter.
- Both expiration and `delete` clear raw bytes and consent and retain only the case ID as an indefinite tombstone in that store. `delete` is idempotent and even blocks a not-yet-received case, so delayed retries cannot resurrect a withdrawal. Every later upload with that ID gets `409`. Keep tombstones across restarts and do not reset the store or roll it back to an older copy while accepting retries. Do not resubmit withdrawn material under a new ID.
- SQLite secure deletion is enabled; this is not a guarantee of physical erasure from storage media, filesystem snapshots, crash artifacts, or separately exported copies. Exclude raw storage from routine backups/snapshots, keep it on an appropriately protected volume, and apply withdrawal and retention to any operator exports separately. If there is a crash, restart with the same store so SQLite can recover its rollback journal.

Run the integration tests (only local loopback and synthetic data):

```sh
python3 scripts/feedback-intake/test_receiver.py -v
```
