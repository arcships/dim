# DimOAuth design

Status: implemented. The Dim account (`dimcode-api-oauth`) as a Rust process plugin of the App Server, replacing the separately packaged TypeScript plugin (`dim-oauth-plugin` 0.2.0) and the Node runtime the account build embedded for it. The runtime contract is in [runtime-contract.md](runtime-contract.md); this page records the structure and the decisions.

## 1. Structure

```text
dim-app-server process tree (account build `dimcode`, product home set)

  FileStorage ──▶ DimOAuth ──provides──▶ Arc<dyn ManagedProvider>
                                               │
  ManagedProviders ◀── ManagedProviderBinding ─┘
        │
        ├── account.* RPC (management)
        ├── Provider service: catalog cache, model credentials
        └── webSearch (rutis service for Web plugin)
```

| Part | Owns |
| --- | --- |
| `DimOAuth` (this repository) | Login attempts, `auth.json` account record, token refresh, relay catalog projection, usage projection, relay web search, credential leases; its accepted work |
| `ManagedProvider` (dim `managed_provider.rs`) | The typed service contract and its DTOs |
| `ManagedProviderBinding` | Publishing the provider to `ManagedProviders` while both are active; quiesce on unload |
| `ManagedProviders` | Process access for RPC, the provider service and `webSearch`; public error mapping |

## 2. Decisions

- **In-process Rust instead of a Node row.** The account path no longer needs an embedded Node, rutis JS runtime, runtime archive extraction or a private package download and verification. First install depends on no runtime besides the executable.
- **Typed trait instead of `HostDispatch`.** Host and provider exchange Rust types: no JSON envelope, error strings or second validation of provider output at the host. The contract is Rust-only (ADR 002 D5 below).
- **Public source, secret client IDs.** The plugin lives in the dim workspace; only its OAuth client IDs stay out of the repository. `dimcode` compiles them in from `DIM_OAUTH_CLIENT_ID_CN` and `DIM_OAUTH_CLIENT_ID_GLOBAL` and installs the plugin through `ManagedProviderRow` and `cli::run_with`; release CI reads them from repository secrets.
- **One bundled model catalog.** Relay metadata wins; missing metadata comes from the provider service's bundled catalog, which is refreshed before each release, rather than a second copy.
- **Plain JSON `auth.json`.** The TypeScript plugin wrote the file as a PluginFiles record, which the native subscription providers sharing `auth.json` read as an opaque root and the plugin then rejected once they had written beside it. DimOAuth reads that record (merging fields written beside it) and writes plain JSON; token digests are unchanged, so signed-in accounts keep their epoch and generation.

## 3. Rule check

| Rule | How this design complies |
| --- | --- |
| Principles §4 rules 1–6, 9 | No Timeline events, handovers, before callbacks or context changes; account state is process state outside the Timeline. |
| Principles §4 rule 7 | Uses FileStorage and the public ManagedProviders/Provider services only. |
| Principles §4 rule 8 | No new mechanism requirement (§4). |
| Principles §1 | A process plugin of the App Server; RPC methods only dispatch to `ManagedProviders`. |
| ADR 002 D1–D3 | Process scope; its only dependency, FileStorage, is a declared process service. |
| ADR 002 D4 | Registration is structural (build inputs and product home); there are no business settings. |
| ADR 002 D5 | Deviation, accepted in review: `ManagedProvider` is a Rust trait, so a non-Rust provider cannot implement it. If one is needed, export the service with `#[rutis::service]` after moving to rutis 0.15. |

## 4. Requirements raised to mechanism owners

None.

## Appendix D. Acceptance

`dim_oauth_flow` (device and browser login, identity, catalog projection, leases, usage, logout and revocation, recovery and compare-and-delete, rotation across two processes, TypeScript record compatibility, reset backup and unknown schema), `managed_provider_flow` (host binding and lease release) and `transport_flow::managed_provider` (RPC login and a model request recovered after a 401) run against a local account service, without Node.
