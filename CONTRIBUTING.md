# Contributing to Dim

## Set up and build

Follow the [build environment guide](docs/architecture/build.md) for the pinned
Rust toolchain and platform prerequisites. From the repository root, build the
native CLI with `cargo build --locked -p dimcode`. For an API-key setup and a
local run, see [Daily development](README.md#daily-development) and the
[development guide](docs/architecture/development.md). Run Dim with an isolated
`DIMCODE_HOME` or `--home` so your development sessions and credentials stay
separate from your everyday setup.

## Run checks

Use `./dev check` for local Rust formatting and Clippy checks. Run the checks
that cover your change and record the results in the PR. See the
[verification commands](AGENTS.md#verification-commands) for workspace tests and
checks for architecture, scripts and terminal UI changes.

The [PR workflow](.github/workflows/ci.yml) runs its jobs only for branches in
`arcships/dim`. If you open a PR from a fork, share your local verification
results and ask a maintainer how to proceed with the required checks.

## Open an issue or pull request

Search [open issues](https://github.com/arcships/dim/issues) and
[pull requests](https://github.com/arcships/dim/pulls) for the same change.
If an issue is already assigned, coordinate there before starting parallel work.
Use the [issue forms](https://github.com/arcships/dim/issues/new/choose) for bug
reports and proposals. Open an issue first when the scope, design or acceptance
criteria need agreement.

Keep the PR focused. Use the [PR template](.github/pull_request_template.md) to
explain what changed, link the affected design, and show how you verified the
behavior. Use `Fixes #N` if the PR completes the issue or `Refs #N` if it covers
part of it. Before requesting review, check correctness, security,
compatibility and whether each change is necessary. The repository's
[working conventions](AGENTS.md#working-conventions) and
[documentation index](docs/INDEX.md) point to the design rules reviewers use.
