# Document templates

These files describe the shape of a document. They are not rules. The constraints and routing
are in [AGENTS.md](../../AGENTS.md).

| Type | Template | Example |
| --- | --- | --- |
| ADR | [adr.md](adr.md) | [ADR 008](../adr/008-documentation-ownership.md) |
| Feature or plugin design | [design.md](design.md) | [Web](../design/plugins/Web.md) |
| Contract | [contract.md](contract.md) | [Session access](../design/contracts/session-access.md) |
| Owner redesign | [owner-redesign.md](owner-redesign.md) | none yet |

The [design guide](../design/capability-design-arguments.md) offers help with use, public promises and verification.
The [detailed reference](../design/capability-design-reference.md) and [Wait example](../design/examples/wait-argument.md)
are optional reasoning aids for specific questions; neither is a required template or migration target.

Document type comes from the existing path; there is no manifest. Older examples still carry
status lines from before ADR 008; do not copy those lines.

Use the template for the document's own layer. An ADR records a decision; a design records what
one owner provides and requires; a contract records one cross-module interface. Do not copy
another module's mechanism into a design.

Technical detail stays. Put public terms and invariants in the contract or design body, language
spelling in a binding appendix, and current implementation facts in architecture documents.
