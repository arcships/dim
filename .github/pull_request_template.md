## Summary

<!-- What changes and why. Link the design document. -->

## Design rule checklist

Answer each item; write "n/a" with a reason where it does not apply. Rules: [design rules §4](../docs/architecture/rust-agent/principles.md), [ADR 002](../docs/adr/002-plugin-scope.md).

- [ ] Scope: each new part is placed by the owner of its state and lifecycle (process / Session / branch); no wiring module.
- [ ] Commands and results: changes to another module's state are command events; the owner writes the result; a command with no effect writes no event and the caller's reply says so.
- [ ] Handover: every handover point is an explicit event; nothing is revoked after handover.
- [ ] Timeline `before` callbacks only transform the draft synchronously: they never block the write or wait for external work (ask the event owner for a generic field when the draft cannot express it). Entries validate only shape and authorization; feature checks belong to the feature owner.
- [ ] Infrastructure (Timeline, Dispatcher, event envelope, assembly and lifecycle, Session and branch facades) is unchanged, or the change comes from and closes within the infrastructure itself.
- [ ] Core mechanisms and consumers gain no plugin-specific handling.
- [ ] Batch: facts that take effect together are written in one batch, with the deciding event last.
- [ ] Context: only handed-over, consumed facts reach the model context.
- [ ] No reading another plugin's internal state or deciding for it.
- [ ] Cross-language faces: new public services use data faces, existing dependencies state their current face, and a leaf injects and provides only data faces ([service faces](../docs/design/service-faces.md)).
- [ ] Business configuration stays out of loader rows; contracts are language-neutral.
- [ ] Missing mechanism capabilities are raised as requirements (problem, why existing mechanisms fail, generic capability, ordering impact, owner/issue).
- [ ] Timeline holds only facts later decisions use; UI-only changes go through Session broadcast notices; process-level state is changed through its owner's typed write operation, not the Timeline.

## Documentation

- [ ] No accepted ADR is modified or deleted; a changed decision is a new ADR, and a link or typo fix uses `adr-erratum` ([ADR 008](../docs/adr/008-documentation-ownership.md)).
- [ ] No other module's design is changed; a cross-module requirement is stated for its owner.
- [ ] Delivery status is not added to an ADR, index or roadmap; binding details are in a binding appendix.

## Verification

<!-- Tests and checks run. -->
