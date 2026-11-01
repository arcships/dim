# 015: Sessions, branches and their content are plugins, not loader instances

Decision status: Proposed. The design is [Sessions and branches are plugins, not loader
instances](../design/thin-instances.md), tracked by [#594](https://github.com/arcships/dim/issues/594).

Relation to other decisions:

- Supersedes the mechanism [ADR 004](004-loader-plugin-management.md) gives MCP connections: "Each
  connection generation is an instanced group in the existing Session loader subtree", "Immutable
  private configuration passes through `create_instance().with(...)`" and "The loader owns instances
  and disposal". ADR 004's ownership and update rules stand, and so does "No second loader or
  lifecycle manager is introduced". [ADR 001](001-harness-foundation.md)'s "Loader-owned MCP
  instances" row describes the superseded mechanism.
- Removes the `SessionExtension` mechanism, which [ADR 005](005-observability.md) (Proposed) lists as
  unchanged.
- Follows [ADR 006](006-plugin-contract.md) D3 and D4. D3's "Inside a part" ("as McpSession does for
  each server and Skills for each content") holds literally from now on. D3's first point, as
  [ADR 007](007-plugin-configuration.md) corrects it, is unchanged for an installed plugin: its code is
  one process row.

## Context

A Session and a branch are instances of instanced loader groups. Only foreign parts need that:
rutis-loader resolves the instance names a foreign copy injects by finding the copy's instance in the
loader's own instance table. Being a loader instance costs the rest of the assembly:

- the mounters, the extension launcher and the extension plugins are rows of the instance, and
  creation finds them, and judges readiness, by scanning every loader entry (#594);
- per-Session content (Skills contents, MCP connection generations) is a nested loader instance. The
  loader creates an instance only from a row's context, so the parts create it through
  `SessionLoader`, from the Session scope row.

rutis 0.19.3, which the 0.19 upgrade pins, resolves a foreign copy's instances from its position in
the plugin tree, and lets a host mark a plain plugin as the scope of a group (`mark_scope`,
[rutis#948](https://github.com/arcships/rutis/issues/948)).

## Decision

- **D1. A Session and a branch are plugins.** A Session is a plain plugin its Agent mounts with
  `ctx.plugin`, and a branch one its Session mounts. Each marks itself as the scope of its group. The
  catalog's groups, the names, the part registry and the mounters' contract stay as they are. Nothing
  of a Session or branch is a loader row. This ADR uses "plugin" in rutis's sense for the scopes and
  the content they mount: neither is a loader row, and neither is an installed plugin of ADR 006 D3's
  first point.
- **D2. Per-Session content is its part's children.** Each Skills content and each MCP connection
  generation is a plain plugin its part mounts as its own child. Its private immutable configuration
  is the plugin's constructor argument. It closes as its part decides and with its part, through
  rutis's ownership of children, so closing a Session closes only its content.
- **D3. Creation waits on what it is handed.** A Session or branch publishes once its scope is Active,
  its mounters and requested extensions are Active (the optional mounter settled), and the parts they
  hand are Active (the optional ones settled). It reads no loader table.

The Agent group stays an instanced loader group: its row isolates the Agent's process values.

## Consequences

- Creation reads no loader entries, and `Loader::entries()` lists process rows and the Agent group's
  rows only.
- Dim builds on rutis 0.19.3, as the 0.19 upgrade pins it; nothing is asked of rutis. Foreign copies
  resolve through 0.19.3's mounter. Rust copies qualify their keys with their scopes' instance ids,
  as they can today.
- A part's nearest row is its Agent's anchor row.
- A Session is a child of its Agent's anchor, so the anchor must not reload: a reload would dispose
  the Agent's Sessions without `Agent::close`'s settlement.

## Alternatives

| Alternative | Why not |
| --- | --- |
| Keep the loader instance and thin it to one row (the design's first draft) | The launcher and the extension plugins stay rows or rows' children, and creation still reads the loader's report |
| A host-side `PluginMounter` | It would rebuild rutis-loader's row plugin for foreign definitions (gating, host leases, refused restarts) in Dim: the shim the 0.19 upgrade rules out |
| Create content instances from the Agent's anchor row | They fall outside the Session's subtree, and Session close has to find them |
