# Local patches

Each change to vendored source is its own commit, with the reason in the
message, so it can be compared against upstream at `SOURCE_REV`. Modified files
carry a `// Modified by dim-agent:` line at the top.

| # | File | Change | Reason |
| --- | --- | --- | --- |
| 1 | `xai-grok-markdown/src/style.rs`, `src/parse.rs` | New `MarkdownStyle::table_compact: bool` (default `false` = upstream). When set, the horizontal divider between body rows of a table is not drawn. | The current dim scrollback contract draws a table with the header separator and outer borders only; upstream draws a divider after every body row. |
