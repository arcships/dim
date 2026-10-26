---
name: dim-dependency-audit
description: Manually review Dim's Rust dependency advisories, license metadata and duplicate versions when a developer requests an audit or pre-release dependency review.
---

# Dim dependency audit

Run when explicitly requested. This is a developer audit, with findings returned
in chat. It adds no product behavior, CI gate or scheduled task. Review dependency
changes or a release candidate when requested; the developer chooses the cadence.

## Inspect the checkout

- Record the source commit, working-tree changes and scan date. Read the current
  Cargo manifests, lockfile and any existing `deny.toml` policy.
- Use `cargo-deny` and record its version. If it needs setup, use the host's tool
  installation convention outside the workspace; never add the scanner to Dim's
  Cargo dependencies. Consult the installed command's help when flags differ.
- Scans may download advisory data and missing locked crate sources. A missing
  tool, unavailable database or incomplete dependency graph is an incomplete
  check, not a clean result. Do not build Dim or run its tests for this audit.
- Report the targets and features actually checked. A Rust workspace scan does
  not cover npm dependencies or a separate distribution's private dependency graph.

## Run the checks

From the workspace root, keep the lockfile unchanged:

```sh
cargo deny --locked check advisories
cargo deny --locked list --format json --layout crate
cargo metadata --locked --no-deps --format-version 1
cargo tree --locked --duplicates --target all
```

Use an existing approved license policy with `cargo deny --locked check licenses`
when one exists. Otherwise report the license inventory and items requiring
review; do not invent an allowlist or claim that license acceptance passed.
For a release review, match the actual release targets and feature selection.

## Interpret Dim's results

- Separate known vulnerability advisories from unmaintained/yanked notices.
  For an actionable finding, identify the crate/version, advisory, affected
  dependency path and available fixed version. Do not silently ignore findings.
- Multiple versions alone are not a vulnerability. Show which direct dependencies
  bring them in; propose consolidation only when it serves a concrete problem.
- Missing license metadata is not proof that the source has no license. Check
  the actual license files and inheritance. Dim-owned crates may need
  `license.workspace = true`; `publish = false` does not mean a crate is absent
  from the distributed executable or may be skipped.
- Keep vendored dependencies in scope. Read `vendor/aimux/PATCHES.md` and
  `vendor/grok-build/PATCHES.md` for upstream identity and local changes. Version
  advisory checks do not review those patches. Preserve upstream licenses and
  notices; check `scripts/stage-release.mjs` and `docs/release/README.md` when
  assessing what accompanies an artifact. Do not rewrite third-party licensing
  declarations or exempt an entire vendor tree to silence diagnostics.
- Report proposed upgrades, metadata corrections and missing notices. Apply
  repository changes only when the developer requests fixes; audit invocation
  alone does not authorize dependency upgrades or publishing.

## Report

Give a short result with the commit/date, tool and checked scope, actionable
findings and next steps. Name incomplete checks explicitly. Keep advisory status,
license questions and optional duplicate-version cleanup separate. Return no
report file unless requested, and do not equate a successful advisory lookup with
a source-code security review or a license inventory with legal acceptance.

Command references: [cargo-deny check](https://embarkstudios.github.io/cargo-deny/cli/check.html),
[license inventory](https://embarkstudios.github.io/cargo-deny/cli/list.html).
