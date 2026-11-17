# Capability documentation responsibilities

Scope: the first current-document review for event/state-machine authoring, not a new runtime contract.
Issue: [#1615](https://github.com/arcships/dim/issues/1615).

This is a source-based review at `f1abce6e2`, not an exhaustive audit or a declaration that the
implementation satisfies every contract. Historical ADRs remain unchanged. Follow-up decisions
and completion belong in the issue. The proposed authoring method is in the
[design argument guide](capability-design-arguments.md).

## 1. Where each question belongs

| Question | Current authoritative source | Other documents should do |
| --- | --- | --- |
| What is a Capability and where does its state belong? | [Principles §1–2](../architecture/rust-agent/principles.md) | Identify each part and link the placement rule; do not infer scope from size or implementation language |
| What do order, state responsibility and work settlement mean? | [Time bus §1–5](../architecture/rust-agent/time-bus.md) | State the feature's transitions and consequences, not another global rule |
| What can a plugin read/write and how does it join live consumption? | [Plugin contract §5–7](plugin-contract.md) | State dependencies and use the contract; bindings describe spelling only |
| What does a query return, and how do cutoff, relations and continuation work? | [Property query §2–3](timeline-property-query.md) | Define business selection and arguments; do not reproduce query semantics |
| What survives a reload for one feature? | The [lifecycle contract](plugin-contract.md#72-generations-reload-and-rebuild) and its owner design, for example [Goal state](plugins/Goal-state-read.md), [Queue recovery](input-queue-recovery.md), [Prompt state](prompt-state-read.md), [Wait](../architecture/rust-agent/plugins/Wait.md) | Apply the existing lifecycle obligations and the owner's business recovery outcome; restoring state does not justify leaving old execution running |
| How do owner reads and saved results fit together? | [Saving boundaries](owner-state-indexed-reads.md), [Resume design](resume-read-design.md) | Keep cross-module dependencies here and detailed fields/algorithms in owner documents |
| How does a part mount, name dependencies and cross languages? | [rutis usage](rutis-usage.md), [service faces](service-faces.md) | Link the current binding; do not copy old API spellings into architecture rules |
| What exists in the actual assembly? | [Generated catalog](../architecture/rust-agent/generated/catalog.md) | Use as implementation evidence for that profile, not a closed universe of plugins |
| How can a design explain its use, promises and verification? | [Design argument guide](capability-design-arguments.md) | Reuse existing design material; reasoning tools are optional and run outcomes belong in issues/PRs |

## 2. Overlaps and ambiguity to resolve

These are review findings and proposed disposition, not silent amendments to the linked contracts.

| Location / overlap | Problem | Classification and disposition |
| --- | --- | --- |
| Plugin contract §1, `Owner`: “writes a piece of state or a domain” | Conflates state mutation with maintaining an event domain; could imply one exclusive event writer | Wording clarification. Owner of the plugin contract should distinguish state owner, contract maintainer and conforming writers, preserving [time bus §2](../architecture/rust-agent/time-bus.md#2-事件是总线上带序号的事实) and ADR 001's multiple-writer/history rules |
| Time bus §2 “which state machines must consume” and feature event lists | A list can be mistaken for a closed registry of consumers | Wording clarification. Express obligations by behavior, such as owning affected in-flight work; keep known consumers illustrative and each consumer's dependencies local |
| [Single-tree reload protocol](../architecture/rust-agent/single-tree.md#能力重载协议), plugin contract §5.2/§7.2, property-query §3 | Detailed recovery handoff appears in several places; single-tree still spells an old `bound: committed` form and broad kind folding | Duplicate-definition removal and binding correction proposed. Public handoff belongs to plugin contract, query semantics to property-query; architecture should describe current realization and link both |
| Time bus §3 equivalence and owner recovery outcomes | Reading "same next decision" without §3's qualification can conflate business state with in-flight execution | Scope clarification, not a new semantic choice: §3 already excludes settled I/O's local control state, and §5 requires prior-generation work to end. Apply those boundaries when interpreting owner recovery outcomes; a surviving old Wait is a compliance defect, not permission to weaken the lifecycle contract |
| Property-query §5 consumer recipes, Resume §3/§9, saving-boundaries §1/§5 and individual recovery pages | Multiple descriptions of the same owner's algorithm can drift | Duplicate-definition removal proposed. Keep owner-specific formulas/fields in the owner's page; higher-level documents retain integration requirements and links, not a second algorithm |
| [Timeline read entries](timeline-read-entries.md) and the current query contract | A migration audit at an old base contains proposed APIs and historical call sites | Scope clarification. Treat it as an explicitly based audit, never the authoring API; default authoring routes to the current property-query contract |
| AGENTS.md “Current Rust binding” and rutis usage appendix A | `TypedPlugin/Keyed` instructions disagree with the current `DynPlugin/Key` named-dependency binding | Binding clarification proposed in its own focused cleanup. Keep one binding source rather than introduce another copied summary here |
| [Design template](../templates/design.md) | The default guidance should help clarify contracts without imposing a proof workflow | Authoring guidance: actual use, affected public promises and verification; proofs and models are optional, with no runtime semantic change |

## 3. Review order and completion criteria

1. Agree the terminology and identify the authoritative source of each disputed statement.
2. Resolve semantic conflicts with the relevant owners before deleting either statement. An
   approved behavior change needs its own contract change; moving text cannot approve it.
3. Remove duplicate normative definitions in focused owner changes, replacing them with links.
   A precedence list alone leaves contradictory specifications in place.
4. Prioritize follow-up implementation checks by actual contract risk. Use scoped arguments when
   helpful; do not require every design to migrate or every Capability to acquire a model.

A finding is resolved when there is one definition at the correct layer, dependent documents
link it, and any semantic change has explicit agreement. This document is not a permanent
machine-readable ownership manifest. Current design locations continue to determine document type.
