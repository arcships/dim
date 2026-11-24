# ChatGPT plan provider

The free local Dim CLI uses OpenAI's [Sign in with ChatGPT][sign-in] to run
interactive coding sessions under the user's authorized ChatGPT plan. The
provider ID is `openai-chatgpt`, and the visible name is OpenAI ChatGPT.
OpenAI signs in the user in a system browser and grants plan usage separately
from identity. ChatGPT settings are the source of truth for plan limits,
credits and application access.

## Owner and interfaces

The App Server's managed account owner holds the local ChatGPT profiles,
authorization attempts and token renewal. It uses the existing FileStorage
binding, registers `account.chatgpt` through the process
[`managementMethods` entry](management-methods.md), and serves only protected
local management clients. Provider uses its existing managed catalog and
credential resolution. Aimux uses the normal OpenAI Responses implementation
with the ChatGPT plan request rules. The catalog and RPC replies carry data;
credential leases stay inside the model resolution path.

| Operation | Request | Reply |
| --- | --- | --- |
| `account.chatgpt.status` | Empty | Safe account snapshot |
| `account.chatgpt.profiles` | Empty | Saved connection IDs, labels and active/signed-in state |
| `account.chatgpt.select` | `profileId` | Safe account snapshot |
| `account.chatgpt.login.browser.begin` | `redirectUri`, optional `profileId` and `migrateLegacy` | Attempt ID, authorization URL and expiration, or `{migrated: true}` for an existing grant |
| `account.chatgpt.login.browser.complete` | Attempt ID and callback URL | Safe account snapshot |
| `account.chatgpt.login.cancel` | Attempt ID | Empty |
| `account.chatgpt.login.status` | Attempt ID | Login state |
| `account.chatgpt.logout` | Empty | Whether server-side revocation was confirmed |

A fresh registration uses the application name Dim, a locally generated stable
host ID and OpenAI's dynamic registration entry. The CLI binds a native
loopback listener before opening the browser. The account owner validates
state, PKCE, the issued client ID, and the ID token signature, issuer, audience,
expiry and nonce. A reauthorization must match the selected registration's
verified subject. Model access requires both direct plan permission and the
Responses resource grant. Signing out retains the registration and clears its
credentials after attempting revocation.

The product home holds a versioned, owner-protected `chatgpt-account.json`.
It records the installation ID, the account generation, and separate
registrations with their verified identity and credentials. FileStorage owns
atomic replacement and file exclusion. Refresh holds that exclusion through
rotation and persistence. Startup and safe status reads never start a refresh.
Account changes invalidate the old account's catalog. Leases from a previous
account generation cannot refresh or invalidate the current one. Untrusted
response bodies, URLs containing token hints, and tokens are absent from logs
and management replies.

## Switching from OpenAI Codex

Discovery offers `openai-codex` when the product home has a legacy grant,
a saved model edit or a saved default targeting it. Built-in entries and
signed-out markers alone do not qualify. An unavailable visibility read
omits the legacy entry. Explicit reads and resolution by a saved model ID
remain available so existing sessions and sign-out can finish.

The protected local browser-begin operation accepts `migrateLegacy: true`
from the Codex switch action. A connected ChatGPT profile completes the
switch with `{migrated: true}`. Otherwise the existing browser reply starts
an authorization attempt that carries the switch intent. The owner saves
the grant and the local `legacy_codex_hidden` flag in one atomic
replacement after identity and direct plan permission have been verified.
The flag survives logout and restart. A canceled or failed login leaves
visibility unchanged. Catalog refresh and model selection are subsequent
steps. Existing stores without the field begin with the flag unset.

## Model requests

The account model list comes from `GET https://api.openai.com/v1/models`. Dim
uses listed model slugs and display names in returned order and fills declared
capabilities through Provider's existing catalog metadata. Model entries that
lack required limits remain unselectable until the limits are declared. Model
resolution requires a native local client Session. This grant is scoped to the
connected user's Dim application, and the initial integration admits interactive
sessions only. It cannot be used from a Session-less auxiliary call, an
automation launch, a public endpoint or as a general-purpose API credential.

The [inference endpoint][inference] is `POST https://api.openai.com/v1/responses`. The
request uses SSE, disables server-side storage, and carries history in
`input`. System instructions use the developer role. Local functions use a
Responses tool namespace. Requests omit the [unsupported preview parameters][limits]
and hosted tools. Ordinary API-key traffic keeps its own Responses contract.
A limit error may arrive before or during the stream and points the user to
[ChatGPT usage](https://chatgpt.com/settings/usage). Account management and
inference never use the old Codex credentials.

## Rule check

The account is process state under ADR 002 D1–D5. ManagedProviders already
owns the account management operations and is the same process capability.
It directly registers the new methods with the existing management data
face. Provider uses the existing process `store` data face through its
Rust binding to check the Session creation origin. Neither account state
nor credentials are Timeline facts. Model requests and local tool results
use the existing Session path. Renewal must settle before teardown. These
changes require no extension to the Timeline, Dispatcher or Session faces.

## Requirements raised to mechanism owners

None.

## Verification

A disposable home and HTTP fixtures exercise dynamic registration, a signed ID
token, account-scoped model discovery, renewal after restart, logout, stale
account rejection and the legacy switch with both new and existing grants.
Capture Responses requests for a text turn and a tool-call
continuation. Run the CLI interaction through the local tmux suite. Validate
registration and subscription usage with a Plus or Pro account before retiring
the legacy Provider.

[sign-in]: https://developers.openai.com/siwc/token-sharing-open-source/sign-in
[inference]: https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference
[limits]: https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations
