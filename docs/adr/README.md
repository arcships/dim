# Architecture decisions

An ADR records one decision at the time it was made. **Accepted** means the direction was
approved; implementation status is tracked in issues, not here. An accepted ADR is a snapshot:
do not edit it. A superseding decision is a new ADR that preserves the reasoning of its
predecessor. Existing ADRs predate [ADR 008](008-documentation-ownership.md) and keep their
original text.

| Decision | Decision status | Relationship |
| --- | --- | --- |
| [001 — Extensible harness foundation](001-harness-foundation.md) | Recovery and configuration accepted; event and dependency-loss refinements proposed | Foundation |
| [002 — Plugin scope assignment and configuration placement](002-plugin-scope.md) | Proposed | Extended by [006](006-plugin-contract.md) |
| [003 — Approval covers agent decisions only](003-approval-scope.md) | Proposed | Supersedes the project MCP approval in 001 |
| [004 — Session-owned MCP and business configuration](004-loader-plugin-management.md) | Approved by the repository owner | Supersedes 001's MCP ownership; follows 002 D4 |
| [005 — Observe runs through the Timeline and process logs](005-observability.md) | Proposed | Supersedes the trace archive and idle-unload D9 |
| [006 — The plugin contract is concepts and conventions, not types](006-plugin-contract.md) | Accepted by the repository owner | Extends 002 D5; supersedes the 0.15 upgrade design's scope model; replaces the #377 draft; D3's first point corrected by 007 |
| [007 — PluginManager keeps plugin configuration; rutis and content owners keep state](007-plugin-configuration.md) | Accepted by the repository owner | Corrects 002 D4 and 006 D3; withdraws 001's direction for #3 |
| [008 — Documentation ownership and immutable ADRs](008-documentation-ownership.md) | Proposed | Supersedes 006 D9's sentence that puts delivery status in ADR tables and design documents |
| [009 — Permission decides; HostPlatform enforces](009-permission-decides.md) | Proposed | Supersedes the permission-preset clause of 003 D5 and the F5 escalation gate |
| [010 — The sandbox is opt-in](010-sandbox-opt-in.md) | Proposed | Extends 009; replaces the F5 default-on policy and the policy file |
| [015 — Sessions, branches and their content are plugins, not loader instances](015-scopes-are-plugins.md) | Proposed | Supersedes 004's mechanism for MCP connections; follows 006 D3 and D4 |

Source baseline: [initial foundation parent](https://github.com/arcships/dim/commit/aaaffa5768e031d781abb73e45b1915b05dc2aa0).
This baseline includes lifecycle fixes not necessarily present on the default branch.
