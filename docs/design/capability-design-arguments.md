# Capability design guide

Scope: help with a plugin's Timeline events, state and lifecycle design.
Issue: [#1615](https://github.com/arcships/dim/issues/1615).

Describe the actual use, the affected public promises and how to verify them. Existing design
material can supply these answers by link; a small change needs only a short explanation in its PR.
This guide adds no proof prerequisite, mandatory model or separate design deliverable.

- **Actual use:** who calls this part and produces its inputs? Use the relevant integration and
  contracts to establish the scenario. Known plugins are examples, not a closed list of consumers.
- **Public promises:** what behavior changes, and what must remain true? Describe only the affected
  inputs, outputs, events, state and lifecycle responsibilities. Link other owners' contracts.
  Recovery and old-work settlement follow the existing contracts; they are different responsibilities.
- **Verification:** what observable outcome would show that the change meets those promises?
  Reuse relevant flow tests and review the affected boundaries. State material uncertainties.

Do not invent requirements from hypothetical traces without establishing their relevance to the
intended use and agreed promises. An observed contract violation is a defect, not a new expected
behavior. Changes to a promise follow the existing owner-confirmation process in AGENTS.md.

## Optional reasoning tools

When ordering, concurrency or recovery is difficult to assess by ordinary review, a transition
table, case analysis or formal model may help resolve a specific question. Choose the smallest
useful technique. Do not split Capabilities or add runtime state to fit a model or template.

The [reference](capability-design-reference.md) describes these techniques and their limits.
The [Wait example](examples/wait-argument.md), [implementation experiment](examples/wait-contract-experiment.md)
and [usage context](examples/wait-usage-context.md) are scoped examples, not a standard shape for
every Capability. A proof supports its stated claims under its assumptions; it does not establish
that the requirements are complete or that the implementation is correct.

The [document responsibility review](capability-document-review.md) locates existing contracts.
It is not a requirement to migrate all designs or formally verify every Capability.
