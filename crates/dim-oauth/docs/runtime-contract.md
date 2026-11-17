# Runtime contract

The Dim account (`dimcode-api-oauth`) is the rutis plugin `DimOAuth` ([`src/lib.rs`](../src/lib.rs)). It runs in the App Server process of the account build; no Node runtime or plugin package is involved. It owns the account login (browser PKCE and device flow), the `auth.json` account record, token refresh, the relay model catalog, relay web search (`POST {relay}/web-search`) and request credential leases, and publishes them as the process's `ManagedProvider` (below).

The flow is a public OAuth client: there is no client secret. The `dimagent.com` directory uses `DIM_OAUTH_CLIENT_ID_GLOBAL` and every other directory `DIM_OAUTH_CLIENT_ID_CN`; `dimcode` compiles both in and passes them to `dim_oauth::row` as [`ClientIds`](../src/http.rs), and they never enter the repository. A realm discovered from the login email may name its own client ID, which wins.

Login selects a directory (`--directory` or the CLI entry's default) and optional enterprise email. Discovery reads the directory's `/.well-known/openid-configuration`, falling back to `/oauth/authorize`, `/oauth/token` and `/oauth/device_authorization` under the issuer, and the relay is `{issuer}/v1`. Browser redirects permit only HTTP localhost, 127.0.0.1 or ::1 at `/auth/callback`; the host owns the callback listener and browser, the provider owns code exchange and the `/oauth/userinfo` confirmation. Device polling continues in the provider after the initiating call returns.

## Session title generation

DimOAuth also provides the optional process service `sessionTitleGenerator`, implementing the
[SessionTitle contract](../../../docs/design/plugins/SessionTitle.md#33-generation-routing-and-optional-service).
It accepts only its own provider id; another provider returns `unsupported_provider` before
reading account data or making a request. It sends `POST {issuer}/v1/session/rename` with
`{"messages":[{"role":"user","content":text}]}`, the existing identity headers and the account
Bearer token. The caller cannot supply credentials or an issuer. Each HTTP exchange has a 10 s
timeout and uses the existing single controlled refresh after 401. The server owns its model,
prompt and quota. A successful reply must contain nonblank `title` text.

Missing or unusable accounts return `account_unavailable`. The server error codes
`session_rename_disabled`, `session_rename_not_entitled` and `session_rename_quota_exhausted`
map to the corresponding unavailable reasons; other rejection, transport and response failures
return `failed`. No upstream response, account secrets or error messages are returned. There is
no rejection cache: every request uses the current account. Closing cancels request I/O; an
already-started token rotation remains Store-owned and settles before unload. Cancellation is
terminal and returns `cancelled`. DimOAuth never writes Session metadata or chooses a fallback.

## Account record

`<product home>/auth.json` is the only persisted source of Dim credentials (`nextApiOauth`) and account state (`dimAccountState`, schema 1: `credentialEpoch`, `generation`, `credentialRevision`, `state`, and when authenticated `identity` and `tokenDigest`). It is plain JSON shared with the native subscription providers; other root fields belong to their owners and are preserved. Writes go through the product root's FileStorage lease, which also serializes concurrent processes, with Unix mode 0600. A lookup that finds a usable record reads the file atomically without that lease, so a fresh grant creates no `auth.json.lock` marker and no fsync; the lease is taken when a refresh, normalization or any other write is needed. `tokenDigest` is the SHA-256 of the grant's RFC 8785 canonical JSON.

The TypeScript plugin (0.2.0) wrote this file as a PluginFiles record `{"version":1,"text":...}`. Reading accepts that record, keeping fields other owners wrote beside it; the next write stores plain JSON. Its digests verify unchanged, so a signed-in account survives the upgrade with the same epoch and generation.

JSON or root errors and an unknown schema remain errors and are never shown as signed out. Only an explicit login or logout resets a corrupt schema-1 record: the complete original bytes are first written exclusively to `auth.json.dim-reset-backup`, then only the Dim fields are replaced by a new signed-out state. An existing backup blocks another reset and is never overwritten or removed. Explicit recovery (`account.recover`) accepts a legacy grant or a digest mismatch, confirms the identity online and starts a new epoch at generation 1.

Login captures the record's revision before starting and compares it when committing. Logout advances the signed-out generation, including when already signed out, and revokes the refresh token on a best-effort basis. Credentials are refreshed within a 30-second freshness window, under the account lock, and a rotated grant is persisted before the lock is released; 408, 429, 5xx and network failures keep the stored grant, and an explicit rejection signs out only the grant that holds the lock. Requests resolve their credential under this window, which is the correctness boundary: a fresh grant answers from the atomic read, and only a grant inside the window waits for the lock and refreshes. While the process runs, a background task also renews the grant as it enters the window, so an idle process does not let its refresh token lapse. It retries a transient failure after 60 seconds, and stops after a sign-out or an unusable record until this process signs in again. Quiesce ends it.

The public host contract (`ManagedProvider`, its DTOs, leases, quiesce and error mapping) is in the [App Server README](../../dim-app-server/README.md#managed-provider-contract).

## Account billing and referrals

The provider implements the host's usage, referral, reset-batch and reset-apply operations. It calls `GET {issuer}/api/me/usage`, `GET {issuer}/api/me/referral`, `GET {issuer}/api/me/usage-resets` and `POST {issuer}/api/me/usage-resets/apply` with the authenticated grant, identity headers and the same single 401 recovery as relay requests. No operation accepts an issuer or credentials from the browser. A 403 does not trigger refresh or sign-out, and upstream messages and unknown fields are not copied into public replies.

Usage projects the subscription ID, plan terms, credit bucket, rolling windows, feature meters and reset-credit summary. Referral projects the campaign and eligibility flags, invite code, counts, reward and rebate basis points; its invite URL is built with an encoded `aff` query under the grant's issuer. Subscription and pricing links use that issuer's `/console/subscription` and `/console/pricing` paths. Missing optional server fields stay absent. Invalid required referral, batch or apply fields fail the operation rather than manufacturing balances.

Reset apply sends `subscription_id`, `reset_type` and the caller's stable `idempotency_key` to the account service. It never selects a grant batch: the service owns consumption order and replay detection. The provider owns an accepted operation until it settles, even if the calling connection goes away. Retrying after an uncertain outcome uses the same key; a new confirmed action uses a new key. The typed result includes refreshed credits, windows, reset counts, the replay flag and apply time. The host contract describes these DTOs and the login directory metadata.

## On-demand model directory

Each catalog read resolves the current account and fetches its relay directory.
The plugin retains no completed directory result. Concurrent callers for the same
account generation join one in-flight request; the owner removes that flight when
it settles. A later read starts a new request. Credential resolution reads the
requested model from that on-demand directory and keeps only its request lease.

Cancellation ends the caller's wait. A shared relay request ends when its last
waiter leaves. Logout, account replacement, recovery and plugin quiesce cancel the
old flight; account identity and generation are checked before a result is handed
back. Network I/O is cancellable. An already started token rotation settles through
the Store before cancellation returns from the owned operation, so a rotated grant
is not lost. Refresh errors are shared by that flight and are not cached.
