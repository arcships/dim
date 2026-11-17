# Name

Scope: process | Session | branch.
Issue: #NNN. Delivery status belongs in the issue.

Keep this document proportional to the change. Link existing answers instead of repeating them;
no separate proof, model or argument is required. The [design guide](../design/capability-design-arguments.md)
offers optional help.

## Use and scope

The actual callers, input producers and intended behavior. What this part owns and what it does not.
Identify the parts that are Capabilities.

## Own contract and dependencies

Affected inputs, outputs, events, state, errors and public dependencies. Link the owners' contracts.
For changed events, explain their meaning, correlation and compatibility. Describe affected recovery
and lifecycle behavior under the existing contracts. Distinguish existing promises from proposed changes.

## Verification

Observable outcomes and relevant flow tests or review checks. State material uncertainties.
Add a transition table, argument or model only when it helps resolve a specific design question.
Run results belong in the issue or PR.

## Rule check

Answer [design rules §4](../architecture/rust-agent/principles.md), §1 and
[ADR 002](../adr/002-plugin-scope.md) D1–D5 as required by AGENTS.md; explain inapplicable rules.

## Requirements raised to mechanism owners

Use the format of [design rules §4 rule 8](../architecture/rust-agent/principles.md), or state none.
Link the owner's contract rather than redefine it here.

## Binding appendix, if needed

Exact Rust or TypeScript spelling. Semantics stay in the sections above.
