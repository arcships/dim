# Summary evaluation: reasoning and target length

Status: research result for [#170](https://github.com/arcships/dim/issues/170). It evaluates the
two choices of [ContextCompaction](../plugins/ContextCompaction.md) §13; the decisions are
recorded there (§10, §13.2, §13.3). Chinese copy:
[中文](../../zh/design/research/summary-eval.md). The per-run records behind every number here are
in [`xtask/summary-eval/runs/2026-10-28/`](../../../xtask/summary-eval/runs/2026-10-28/).

Every statement carries its kind of evidence:

- **[Measured]**: measured by the evaluation runs; the method is in §2.
- **[Code]**: read from this repository.
- **[Judgment]**: the reasoning or inference of this note; not verified.

## 1. Result

| # | Question | Decision | Evidence |
| --- | --- | --- | --- |
| 1 | Reasoning of summarizer requests | `off` | §3: `off` passes on the measured reasoning class with no quality loss (`F` 0.972 vs 0.958, `X` 1.000 vs 0.958, `V` 0, no failures) |
| 2 | Introduce the setting? | No: `off` is a strategy constant and `compaction.json` is removed | §3.4: `off` passed the non-inferiority check on these cases, and no other arm's `F` is more than `δ` above it |
| 3 | Target length instruction | None | §4: every phase-2 arm passes, and the no-instruction arm's cut-offs and failures are no higher than any target arm's |

The rule and `δ = 0.05` are [model-facing prompts §8.1](../model-prompts.md#81-the-rule)'s test,
restated for this evaluation in §13.4: the family, the mid-p test, the Holm adjustment and the
floor of judged runs are all that section's. The decisions are taken on the class that ran, the
OpenAI-compatible one, and are provisional for every other class §13.4 requires: the OpenAI
Responses class could not run (§2.3), no model of the Anthropic budget-thinking class was in this
evaluation (§2.1), and the non-reasoning control is the same model on a second endpoint, whose
phase 1 counts and whose phase 2 does not.

## 2. Method

### 2.1 Classes, model and judge

| Class | Endpoint | Result |
| --- | --- | --- |
| OpenAI-compatible (effort-based reasoning) | OpenAI-compatible | ran, phases 1 and 2 |
| OpenAI Responses (effort-based reasoning) | OpenAI Responses | **could not run** (§2.3) |
| Anthropic budget thinking | Anthropic | **not run**: no model of that class in this evaluation |
| non-reasoning control | an Anthropic-compatible endpoint of the same model | phase 1 ran and counts; phase 2 is not usable (§4.2) |

[Measured] The model is DeepSeek v4.1 flash, reasoning max, through its OpenAI-compatible
endpoint. Its catalog entry declares a window of 1,048,576 tokens and a maximum output of 8,192.
Its saved preference for the conversation is reasoning effort `max`, and the `inherit` arm
inherited it (§2.4).

[Measured] The judge is the same model family on the OpenAI-compatible endpoint for every class,
including the control, at temperature 0 and reasoning effort `max` (§2.5). It sees the summary and
the probe list only, never the transcript. The judge and the summarizer being one family is a
limitation of this evaluation (§9).

[Measured] The control is the same model through its Anthropic-compatible endpoint, where the
requested effort produced no thinking: the three arms sent three request shapes (`inherit`
declared nothing, `off` the native `none` effort the mapping turns into a disabled-thinking
declaration, `low` an effort of `low`), and all 161 recorded replies of phase 1 contained no
thinking at each of the three values.

[Measured] The Anthropic budget-thinking class was not run: the evaluation used DeepSeek models
only, and none of them is of that class. The probes sent directly to the Anthropic-compatible
endpoint show why the model cannot stand in for it: with `thinking: {"type": "enabled"}` the
endpoint returns thinking blocks carrying a placeholder signature rather than a real one, and with
`thinking: {"type": "disabled"}`, or with an effort of `low` or `max` and no thinking declaration,
it returns no thinking at all. An effort on its own does not produce thinking there: thinking
appears only when a request declares it. The three arms declare none, so they never summon thinking
and cannot measure what a thinking summarizer does. What the default costs or buys a
budget-thinking model is therefore unmeasured.

### 2.2 Cases and metrics

[Measured] The eight committed cases of the runner (§13.4), including the five-step `chain`, and
three runs per case per arm. `F` is fact recall over the `fact` probes, `X` exact recall over the
`exact` probes, `V` violations of `absent` probes, `C` replies cut off at `max_tokens`, `T` the
summary's estimated tokens and `S` the target length of a target arm. The decision rule,
including `δ = 0.05`, is §13.4's.

### 2.3 The OpenAI Responses class could not run

[Code] The request builder asks for the native reasoning continuation data
(`include: ["reasoning.encrypted_content"]`, `crates/dim-agent-aimux/src/request/native.rs:233-234`)
and, for an effort other than `none`, a reasoning summary. [Measured] Of the 20 recorded replies,
19 carried a reasoning summary and none carried the encrypted content. [Code] The reply reader
fails any finished reply whose thinking parts lack that continuation data
(`crates/dim-agent-aimux/src/stream.rs:261-273`), so 18 of the 19 recorded runs of the `inherit`
arm failed with "Model thinking is missing native continuation metadata"; the one that completed
is the one whose reply carried no reasoning. The class therefore measures the endpoint-driver
pairing, not summary quality, and no arm of it enters a decision. The rule is rerun for that
class when it can run.

### 2.4 Recording, and the runner defects

[Measured] A pass-through recorder in front of the endpoint captured every request the runner
sent and every reply, removing credentials before writing. This shows, from the runner's own
requests, that `inherit` inherited `max` before each class ran: the first summarizer request of
the OpenAI-compatible class carried `reasoning_effort: "max"`, the one of the Responses class
carried `reasoning: {"effort": "max", "summary": "detailed"}`, and the control's carried neither
`thinking` nor `output_config`; each run's diagnostics recorded `reasoning: "inherit"`.

Two runner defects were fixed in [#545](https://github.com/arcships/dim/pull/545): the
conversation and judge configs were built with default preferences instead of the model's saved
ones, and `exact` probes were compared with the escaped summary, which undercounted `X`; every
`X` here is under the fixed rule, which escapes the probe with the producer's table (`&`, `<`,
`>`) and compares it, whitespace-normalized, against the summary as stored. The stored summaries
were re-scored with that matching; on these runs it agrees with the one-level unescape used when
the numbers were first taken, at every exact-probe check. Each run's recorded summary and its
`exact` verdicts, recorded and corrected, are in
[`xtask/summary-eval/runs/2026-10-28/`](../../../xtask/summary-eval/runs/2026-10-28/).

### 2.5 The judge's and the arms' configuration

The recorded runs predate the per-run record §13.4 now requires, so the instrument's configuration
is reconstructed here from the two records that exist: the isolated evaluation home's saved
preferences, and the pass-through recorder's request fields.

- **The saved preferences.** The home stored `reasoningEffort: max` for the model, and both the
  conversation and the judge selections were built from it (§2.4).
- **The judge.** Each of its 313 recorded requests carried `reasoning_effort: "max"` and
  `temperature: 0.0`. Not one was sent without the temperature, so no judge request fell back to
  no temperature, and none carried a service tier.
- **The arms.** `inherit` carried the conversation's own reasoning: `reasoning_effort: "max"` on
  the OpenAI-compatible endpoint, `reasoning: {"effort": "max", "summary": "detailed"}` on the
  Responses one, and no declaration on the Anthropic-compatible one. `off` carried the model's off
  declaration in place of an effort: `enable_thinking: false` on the OpenAI-compatible endpoint,
  `thinking: {"type": "disabled"}` on the Anthropic-compatible one. `low` carried
  `reasoning_effort: "low"`, and `output_config: {"effort": "low"}` on the Anthropic-compatible
  one. Phase 2 ran `off` throughout.
- **The service tier.** No recorded request — 1091 in all, every arm's summaries and the judge's
  — carried a service tier, so every run used the provider's default tier. Which tier that is
  cannot be recovered from the recording, and is the one field of §13.4's record this evaluation
  cannot state.

These are the fields the recorder saw on the wire: what each request carried, not a reading of
what the model did with it.

## 3. Phase 1: reasoning

### 3.1 OpenAI-compatible class

[Measured] After the hand corrections of §6 (`F` denominator 72, `X` denominator 24):

| Arm | fell back | F | X | V | C | failed | median T | median duration ms |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `inherit` | 0 | 69/72 = 0.958 | 23/24 = 0.958 | 0 (raw 1) | 0 | 0 | 394 | 3,235 |
| `off` | 0 | 70/72 = 0.972 | 24/24 = 1.000 | 0 (raw 1) | 0 | 0 | 428 | 1,460 |
| `low` | 0 | 69/72 = 0.958 | 24/24 = 1.000 | 0 (raw 1) | 0 | 0 | 502 | 2,237 |

The raw `V` of 1 in each arm is the same judge error on the `instructions` case (the summary
marked a withdrawn rule as initial, the judge counted it as current), corrected in §6. The
remaining `exact` miss in `inherit` is a real omission: the summary of one `cjk` run does not
contain the `targetTokens` identifier.

[Measured] The same scores per case, so that a reader sees the spread the pooled ratios hide: each
cell is the case's total over its three runs, and the range is the lowest and the highest single
run of that case (a case runs at most four `fact` probes and two `exact` probes per run; `–` marks
a case with no probe of that kind).

| Case | `inherit` `F` | `off` `F` | `low` `F` |
| --- | --- | --- | --- |
| `folding` | 9/9 (1.00–1.00) | 9/9 (1.00–1.00) | 9/9 (1.00–1.00) |
| `code` | 9/9 (1.00–1.00) | 9/9 (1.00–1.00) | 9/9 (1.00–1.00) |
| `injection` | 9/9 (1.00–1.00) | 9/9 (1.00–1.00) | 9/9 (1.00–1.00) |
| `instructions` | 6/6 (1.00–1.00) | 5/6 (0.50–1.00) | 6/6 (1.00–1.00) |
| `cjk` | 6/9 (0.33–1.00) | 8/9 (0.67–1.00) | 6/9 (0.33–1.00) |
| `chain` | 9/9 (1.00–1.00) | 9/9 (1.00–1.00) | 9/9 (1.00–1.00) |
| `long-tool-output` | 9/9 (1.00–1.00) | 9/9 (1.00–1.00) | 9/9 (1.00–1.00) |
| `merge` | 12/12 (1.00–1.00) | 12/12 (1.00–1.00) | 12/12 (1.00–1.00) |

| Case | `inherit` `X` | `off` `X` | `low` `X` |
| --- | --- | --- | --- |
| `folding` | 3/3 | 3/3 | 3/3 |
| `code` | 6/6 | 6/6 | 6/6 |
| `injection` | – | – | – |
| `instructions` | 3/3 | 3/3 | 3/3 |
| `cjk` | 2/3 | 3/3 | 3/3 |
| `chain` | – | – | – |
| `long-tool-output` | 6/6 | 6/6 | 6/6 |
| `merge` | 3/3 | 3/3 | 3/3 |

[Measured] Six cases are perfect in every arm and run. The pooled differences rest on the other
two: `instructions` loses one probe in one `off` run, and `cjk` moves by up to two probes between
runs of the same arm, one of which is the `inherit` `X` miss. `off`'s one-probe `F` lead over
`inherit` is a net result: it gains two probes on `cjk` and loses one on `instructions`.

[Measured] `δ = 0.05` on these denominators is 3.6 `F` probes (of 72) and 1.2 `X` probes (of 24):
one `F` probe is 0.014 and one `X` probe 0.042. The rule can see a loss of about four `F` probes
or two `X` probes and nothing finer, so a one-probe difference in either direction is inside what
it treats as no difference.

### 3.2 Control class

[Measured] The same rule, on the non-thinking endpoint:

| Arm | fell back | F | X | V | C | failed | median T | median duration ms |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `inherit` | 0 | 69/72 = 0.958 | 24/24 = 1.000 | 0 (raw 1) | 0 | 0 | 442 | 1,433 |
| `off` | 0 | 72/72 = 1.000 | 23/24 = 0.958 | 0 (raw 1) | 0 | 0 | 446 | 1,446 |
| `low` | 0 | 71/72 = 0.986 | 24/24 = 1.000 | 0 (raw 2) | 0 | 0 | 427 | 1,361 |

The arms spread by 0.042 in `F` and 0.042 in `X`, both below `δ`, with no cut-offs, failures or
unjudged probes: run-to-run noise stays under the threshold it is compared against. The `off`
arm's remaining `exact` miss is a real omission, as in §3.1.

### 3.3 Decision

A reasoning arm passes on a class when no comparison of its family against `inherit` — `F`, `X`,
`V`, the failed-run rate and the unjudged-run rate — is a regression by the rule of [model-facing
prompts §8.1](../model-prompts.md#81-the-rule): the family, `δ = 0.05`, the Holm adjustment at a
family-wise `α = 0.10` and the floor of judged runs are §13.4's. The default is the first of
`off`, `low`, `inherit` that passes on every class. The rule is stated for every class and is not
weakened here: what limits this evaluation is the evidence, so Coverage (§13.4, §2.1) says which
classes contributed and §1 states the scope of the decision. The decisions below were taken under
the rule in force then — each rate within `δ` of `inherit`, and no more violations or failures —
and stand as recorded under [#170](https://github.com/arcships/dim/issues/170); the rerun under
[#565](https://github.com/arcships/dim/issues/565) applies the current rule.

[Measured] On the OpenAI-compatible class (`inherit`: `F` 0.958, `X` 0.958, `V` 0, failures 0):

- `off`: `F` 0.972 (`+0.014`, threshold 0.908) ✓; `X` 1.000 (`+0.042`) ✓; `V` 0 ≤ 0 ✓;
  failures 0 ≤ 0 ✓ → passes.
- `low`: `F` 0.958 (`+0.000`) ✓; `X` 1.000 (`+0.042`) ✓; `V` 0 ≤ 0 ✓; failures 0 ≤ 0 ✓ →
  passes.
- `inherit` passes as itself.

The first passing value is **`off`**. The control agrees: `off` there has `F` 1.000 (`+0.042`),
`X` 0.958 (`−0.042 ≥ −0.05`), `V` 0 and no failures, so it passes there too.

### 3.4 Is the setting introduced?

[Measured] On the class that ran, `off` passed the non-inferiority check (§3.3) and no other arm's
`F` is more than `δ` above it. The setting is therefore **not introduced**: `off` becomes a
strategy constant, and `compaction.json`, which the main branch carried, is removed. The second
reasoning class could not run (§2.3) and the Anthropic budget-thinking class was not run (§2.1),
so this holds for the class that ran and is provisional for the others.

## 4. Phase 2: target length

Run with `--reasoning off`. `S = min(α × O, β × B)` rounded down to a multiple of 100, where
`O` is the output allowance and `B = window − O`. For the seven cases at the model's own window
(`O` 8,192, `B` 1,040,384) the `β` shares never bind at the tested values, so
`S(0.25, 0.05) = S(0.25, 0.10) = 2,000` and `S(0.5, 0.05) = S(0.5, 0.10) = 4,000`. The
`folding` case narrows the window to 8,000, so `O` is 2,000, `B` 6,000, and the four rules give
300, 500, 300 and 600.

### 4.1 OpenAI-compatible class

[Measured] After the hand corrections of §6:

| Arm | F | X | V | C | failed | median T | median T/S | p90 T/S |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| no instruction | 71/72 = 0.986 | 24/24 = 1.000 | 0 | 0 | 0 | 459 | – | – |
| 0.25/0.05 | 71/72 = 0.986 | 24/24 = 1.000 | 0 (raw 2) | 0 | 0 | 448 | 0.242 | 1.403 |
| 0.25/0.10 | 70/72 = 0.972 | 24/24 = 1.000 | 0 (raw 2) | 0 | 0 | 464 | 0.232 | 1.074 |
| 0.50/0.05 | 71/72 = 0.986 | 24/24 = 1.000 | 0 (raw 1) | 0 | 0 | 453 | 0.125 | 1.257 |
| 0.50/0.10 | 69/72 = 0.958 | 24/24 = 1.000 | 0 (raw 2) | 0 | 0 | 487 | 0.132 | 0.777 |

Every arm passes against the best arm: `F ≥ 0.986 − 0.05 = 0.936` (the lowest is 0.958),
`X ≥ 1.000 − 0.05 = 0.950` (every arm is 1.000), `V ≤ 0`. The no-instruction arm has `C` 0 and
0 failures, no higher than any passing target arm (all 0), so **no instruction is kept**; no
target is selected and the p90 rule does not bind.

### 4.2 Control phase 2

The control's phase 2 measures a change in the endpoint, not summary quality, and enters no
decision.

[Measured] From 16:04:06 UTC on 2026-10-28 the endpoint ignored `thinking: {"type": "disabled"}`
and returned thinking without a signature. Of the phase's 178 summary requests, 74 failed with
"Model thinking is missing native continuation metadata": the replies that carried thinking and the
failed runs match one to one, and each failed run sent one request: the reader's error carries no
failure facts, so the shared failure policy stops on it rather than retrying. The other 104
requests returned no thinking and completed.

[Measured] A probe after the run (2026-10-29) sent the endpoint two one-word requests, one
declaring thinking disabled and one enabling it with a budget of 1,024 tokens: both replies carried
thinking and no signature. In that state every request to the endpoint fails in the reply reader
(`crates/dim-agent-aimux/src/stream.rs:261-273`), whatever reasoning it asks for.

[Code] The strategy the runner calls is the product one, so a compaction on that endpoint in that
state is such a request and fails, with its outcome recorded as failed; the reader is the one every
request shares, so a conversation's own replies fail the same way. What the user sees is
[ContextCompaction](../plugins/ContextCompaction.md) §5.8: an automatic compaction fails and the
request continues uncompacted, a recovery compaction that fails fails the request, and a manual one
reports `compaction_failed`. The control is therefore taken from phase 1, which ran before the
change: 161 replies, none of them carrying thinking.

## 5. Planning-size evidence

[Measured] For the no-instruction arm of §4.1, each run's `T` against the planned
`replacement_size` at that run's window: `S` at α = 0.5, β = 0.10 (§13.3) plus the empty
summary wrapper, which is 84 tokens. At the model's own window the plan is 4,084 tokens, and on
the narrowed `folding` window 684. Over the 24 runs:

| Measure | Value |
| --- | --- |
| median `T / planned` | 0.11 |
| p90 `T / planned` | 0.67 |
| runs above the plan | 0 of 24 |
| maximum | 0.89 (`folding`, one run) |

Outside the narrowed case every ratio is at most 0.20; the p90 and the maximum come from the
`folding` case, whose 600-token plan is small. A run above the plan is not fatal: §5.3 step 4
re-plans once at the replacement's actual size before anything is committed.

## 6. Hand corrections

Before the decision, the operator read the lowest-scoring summaries and corrected judge errors
by hand. Every correction was the same `absent` probe in `instructions`, whose statement must
not appear as a current rule; the summaries quoted it as an initial, superseded instruction, and
the judge counted the appearance as a violation. In total 14 verdicts were corrected from
`present` to `absent`: 3 in the OpenAI-compatible phase 1, 4 in the control phase 1 and 7 in the
OpenAI-compatible phase 2. Representative quotes:

| Run | Quote from the summary |
| --- | --- |
| OpenAI-compatible phase 1, `inherit` | "The user initially said the runner could `unwrap` freely because it would never be called from production." |
| OpenAI-compatible phase 2, 0.50/0.05 | "**Initial instruction:** The user said the runner could use `unwrap` freely because it is a tool never called from production." |
| Control phase 1, `low` | "Initial instruction: the runner is tool code that will never be called from production, so it may use `unwrap` freely…" followed by "Correction from team review: no `unwrap` outside tests…" |

No other judge error survived the review. One observation is recorded but not corrected: the
`cjk` `constraint` probe asks for "no comments" while the transcript and the summary say "no
English comments", so the probe is stricter than the source, and the judge's `absent` is
literally correct.

These are the first two probes to revise in the next case revision: the `instructions` `absent`
probe, which a summary quoting the rule as superseded trips, and the `cjk` `constraint` probe,
which is stricter than its source. They stay unchanged here: the cases define the recorded runs,
and changing them now would make the runs incomparable. The 14 corrections, each with the run it
belongs to, are in [`xtask/summary-eval/runs/2026-10-28/`](../../../xtask/summary-eval/runs/2026-10-28/).

## 7. Per-item recall and the chain

[Measured] `constraint` is the only item class that is consistently below 1.000 (0.833–1.000
across arms and classes); `task` misses once in a few arms; `decision`, `openItem`, `judgment`
and `nextStep` are perfect everywhere. In the `chain` case, whose probes are judged on the
fifth summary, all 33 recorded chains completed their five steps: the standing constraint
survived in 31 of 33 (one OpenAI-compatible phase-2 run dropped it, one control phase-1 run wrote
it under "superseded/withdrawn"), the withdrawn rule was never presented as current (33 of 33),
and the replacement constraint and the refined task were present in all 33.

## 8. Cost

[Measured] Tokens are the prompt and completion counts the replies reported; three replies
reported none and count as zero tokens but as requests.

| Class / phase | summary requests | judge requests | prompt tokens | completion tokens | total tokens |
| --- | --- | --- | --- | --- | --- |
| OpenAI Responses, phase 1 (stopped arm, partial) | 20 | 1 | 34,697 | 12,270 | 46,967 |
| OpenAI-compatible, phase 1 | 157 | 72 | 486,598 | 164,956 | 651,554 |
| OpenAI-compatible, phase 2 | 262 | 121 | 822,725 | 251,450 | 1,074,175 |
| Control, phase 1 | 161 | 72 | 340,575 | 158,510 | 499,085 |
| Control, phase 2 (74 of 178 failed, §4.2) | 178 | 47 | 434,142 | 134,922 | 569,064 |
| **Total** | **778** | **313** | **2,118,737** | **722,108** | **2,840,845** |

Of the control's 178 phase-2 summary requests, 74 failed and 104 completed; the failed replies
carried 30,815 prompt and 39,180 completion tokens, the completed ones 223,199 and 51,637. The row
includes that phase's judge requests, as the others do (§4.2).

## 9. Limitations

- The judge is the same model family as the summarizer. §13.4 asks for a different family where
  possible; this evaluation used one family, so judge bias toward its own family is not
  excluded.
- One reasoning class was measured, the OpenAI-compatible one. The OpenAI Responses class is
  blocked by the endpoint-driver pairing (§2.3) and the Anthropic budget-thinking class had no
  model in this evaluation (§2.1), so the cross-class statement covers the class that ran: the
  default is `off` for every class, provisional for the classes that did not run, and the rule
  reruns for each of them when it can.
- The control's non-thinking behaviour is the endpoint's, at the three requested values during
  phase 1. From a point in phase 2 onward the endpoint returned thinking without a signature for
  every request, so that phase is not used (§4.2).
- `X` here is under the fixed runner rule; the earlier escaped-form readings undercounted it.
