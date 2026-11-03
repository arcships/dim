# The prompt-suggestion accuracy set

These scenarios drive `xtask`'s `prompt-eval`
([PromptSuggestion Appendix C](../../docs/design/plugins/PromptSuggestion.md#appendix-c-evaluation)).
Each directory is one scenario in `scenario-eval`'s format: `scenario.json`
and the `workspace/` the scripted turns work on.

`prompt-eval` submits the scenario's `message`, and after every completed
turn submits the next `steers` entry, so every scripted message owns one
turn boundary. The `afterSeconds` value only satisfies the format's rule
that a steer names exactly one trigger; `prompt-eval` does not wait it
out, because the boundaries it measures are the turns, not their wall
clock.

Twelve scenarios are conversations of four to six turns on a small
workspace, in English and Chinese. Three are long sessions on the
131,072-token window: `long-en-logs` and `long-mixed` read generated
English service logs, `long-zh-notes` reads generated Chinese records, and
each first turn reads six files in full, so every boundary falls in the
32k+ prefix bucket. The `workspace/` of each carries the generator its
first message runs.

`compaction-notes` runs on a 24,000-token window. Its first turn reads
four notes and answers one line per file, so its head stays under the
threshold; the next turn's long background message then pushes that
request over, and the Context compacts before it is sent, which the first
boundary's prefix check records. A later turn ends over the threshold, so
the `compaction_due` gate skips the boundary after it.
