# Private feedback pilot

Use only with authorized internal participants and a trusted operator. Ordinary `/report` remains available without an inbox. Nothing below configures a public upload endpoint, and none of the intake data belongs in Git, a public issue, a CI artifact or a shared log.

## Set up the pilot

1. On a trusted POSIX intake host, provision a private directory **outside the checkout, workspace, cloud sync and backups**. Use a dedicated account on an encrypted volume; the case store is not application-encrypted. Generate a random write-only intake credential (e.g. `openssl rand -hex 32`) into an owner-only file, not terminal output, shell history, or a project `.env`. Distribute it to participants through your internal secret channel. Raw storage and operator commands stay on this host.

   ```sh
   umask 077
   INBOX="$HOME/.dim-feedback-pilot"
   mkdir -m 700 "$INBOX"
   openssl rand -hex 32 > "$INBOX/token"
   DIM_FEEDBACK_TOKEN="$(cat "$INBOX/token")" python3 scripts/feedback-intake/receiver.py \
     --store "$INBOX/cases" serve --port 8787
   ```

2. For a participant on another trusted workstation, forward the intake port through authenticated SSH. Verify the host key and keep this tunnel open. Give participants a forwarding-only SSH identity restricted to the intake port, **not** a shell or the account that owns the store. Do not expose the API on a LAN interface, use a shared workstation, or publish it through an HTTP proxy. The CLI addresses only numeric loopback, ignores proxy configuration, and does not follow redirects.

   ```sh
   ssh -N -o ExitOnForwardFailure=yes -L 127.0.0.1:18787:127.0.0.1:8787 intake-host
   ```

3. On the participant's trusted POSIX workstation, install the supplied credential outside any workspace as an owner-only file (`chmod 600`). The pilot is disabled on clients that cannot check POSIX file permissions. Start the **pilot build** of the CLI, setting the port and absolute token-file path only for this invocation. For a single-machine synthetic rehearsal use port `8787` instead. Neither a product upgrade nor a public release enables collection by default.

   ```sh
   DIM_FEEDBACK_PORT=18787 DIM_FEEDBACK_TOKEN_FILE="$HOME/.dim-pilot-upload-token" dimcode
   ```

4. In an attached conversation run `/report`, choose private feedback, and inspect the excerpt. A one-line note is optional. Leave evaluation reuse unchecked for diagnosis only. Closing the preview without sending delivers nothing. Save the case ID on confirmation; an unconfirmed delivery can be retried from the same panel. The regular **Generate report / Save ZIP** action still produces only `report.md` and `diagnostics.json` and does not contact the inbox.

## From a receipt to an evaluation

The participant shares the receipt ID, **not the conversation**, with the pilot operator. The receipt is not a credential. On the intake host:

```sh
python3 scripts/feedback-intake/receiver.py --store "$INBOX/cases" get "$CASE_ID"             # diagnosis
python3 scripts/feedback-intake/receiver.py --store "$INBOX/cases" get "$CASE_ID" --for-eval  # requires allowEval
python3 scripts/feedback-intake/receiver.py --store "$INBOX/cases" delete "$CASE_ID"          # withdraw
```

Use a trusted terminal without shell tracing or output capture. The operator may inspect diagnosis-only cases for triage, **not** use them for evaluation. For an eval-authorized case, give the reviewer only the retrieved sample on the private host, not the original session. Record one outcome:

| Outcome | Evidence |
| --- | --- |
| Reproducible | State the starting condition, user request and checkable expectation. Try one known wrong result and one correct result. Use synthetic fixtures and link the private task to its case ID. |
| More evidence needed | Name the missing fact (for example, the original file or a constraint outside the excerpt). Do not silently enrich the case with unrelated workspace data. |
| Not a test | Keep it as diagnosis; explain why a stable pass condition cannot be specified. |

The pilot does not enroll cases automatically, call an external model, publish samples or train a model. Keep private derivatives indexed by case ID, not just a copied prompt. Raw samples have a 30-day retention window. On withdrawal, remove the raw sample, locate and disable any derived private test within 48 hours, and delete operator-made extracts. Removing a raw case cannot by itself recall manual copies. Store expiry needs a running receiver; when decommissioning it, remove the store and any authorized copies rather than leaving an offline volume indefinitely. Before onboarding, give participants a named internal operator for withdrawal and confirm someone owns these steps.
