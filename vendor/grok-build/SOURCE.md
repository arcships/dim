# grok-build vendored crates

| Item | Value |
| --- | --- |
| Upstream | https://github.com/xai-org/grok-build |
| `SOURCE_REV` | `559751fdcec02d413e4c57c8832ab275e4f44980` (see `SOURCE_REV`) |
| Upstream sync commit | `2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8` ("Synced from monorepo", 2026-09-29) |
| Vendored on | 2026-10-12 |
| License | Apache-2.0 (`LICENSE`); this repository is MIT |
| Design | `docs/design/appserver/cli-native-rust.md` §7 |

## Crates

Copied from `crates/codegen/<crate>` with the original names and layout.

| Crate | Role in dim | Use |
| --- | --- | --- |
| `xai-ratatui-textarea` | input box base | direct dependency |
| `xai-grok-markdown`, `xai-grok-markdown-core` | Markdown rendering base | direct dependency |
| `xai-ratatui-inline` | write path | use `segment.rs` and `scrollback.rs`; `terminal.rs` and `resize.rs` are kept unmodified for diffing but are not wired in |

Not vendored (reference only, never copied): `xai-grok-pager-render`,
`xai-grok-pager-minimal`, `xai-grok-pager-pty-harness`, `ptyctl`.

## Provenance review

- The four crates carry no Codex or OpenCode attribution. `OpenCodeHighlighter`
  in `xai-grok-markdown` is a name for the "open code block" highlighter, not a
  derived work.
- `xai-ratatui-inline/NOTICE`: the forked `Terminal`/viewport in `terminal.rs`
  is based on Ratatui source (MIT OR Apache-2.0). Kept as is.
- Bundled assets of `xai-grok-markdown`:
  - `assets/tokyo-night.tmTheme`: Tokyo Night, Apache-2.0 per upstream
    THIRD-PARTY-NOTICES (folke/tokyonight.nvim; VS Code lineage MIT).
  - `assets/Swift.sublime-syntax`: "vendored from bat v0.25.0 extra syntaxes
    (quiqueg/Swift-Sublime-Package)". Upstream THIRD-PARTY-NOTICES has no entry
    for it; its license has not been verified here. Open item before release.

## Layout deviations from upstream

- This directory is its own Cargo workspace (`Cargo.toml`), like `vendor/aimux`.
  Upstream's workspace-level `[workspace.dependencies]`, `edition` and lints are
  reproduced there with the same versions, so member manifests are unchanged.
- `xai-grok-markdown/fuzz/` is not vendored (separate cargo-fuzz workspace).
- Local changes are listed in `PATCHES.md`.
