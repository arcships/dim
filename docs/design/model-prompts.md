# Model-facing prompts

Status: Draft ([#429](https://github.com/arcships/dim/issues/429)). Requirements, not a design: this document fixes what the product's model-facing text must achieve, which channel carries each text, which text is protocol and which is policy, who may change what, and how a change is judged. Each owner designs its own part (§9). Scope: every text the model reads from the product — the system prefix, rule and state blocks, tool declarations, the markers the runtime writes, the compaction replacement and the input guide — across a Session, its branches and its plugins. Chinese: [中文](../zh/design/model-prompts.md).

The model works the way the product intends when what it reads is true about the runtime, it can tell the operator's words from the runtime's, and the product can change its behaviour on evidence. This document answers [#429](https://github.com/arcships/dim/issues/429)'s five questions, which [Agent context §3](agent-context.md#3-给模型的提示词) raises, and hands the consequences to the owners of each text.

## 1. Answers

**Policy and protocol.** Read a text as the model reads it and edit only the words: if a sentence becomes false about the runtime, or a mechanism can no longer be used as it is written, the text is **protocol**; otherwise it is **policy**. Protocol is the model-facing side of a mechanism — channel and marker names and their attribute values, handles such as `[input <id>]`, refusal codes, tool schemas and result formats, and the sentences that state a runtime guarantee. Protocol text is recorded in facts, in history and in the adopted configuration, so a change to it keeps a history resumed with the older form understandable, or it is a record-format change for the text's owner; and it lands with the behaviour and the flow test that assert it. Policy is guidance about how to work: it is replaceable, and every change to it is evaluated (§8). A text may hold both, such as a declaration with a schema and a description; the class is per sentence (§4).

**Who may change what.** A scope reaches the model only through a channel it owns: the product through its built-in capabilities; the host or the profile through the Session system prompt and the trusted sources it loads; the project through its `AGENTS.md` and its skills; the user through their messages, their `AGENTS.md` and the settings that select values; a plugin through what it registers or writes, on the same channels as the built-ins. No central repository takes wording away from its author: Prompt, PromptRegistry and ToolRegistry carry and never own (principles §1, §5; ADR 002). No scope changes the protocol or another author's text, a tag never grants permission, the guide's frame is removable by none, and System holds only static text (§5).

**The input guide.** The input guide is the text that explains the other channels to the model: each channel's trust class, the channel rule, escaping, the restatement after a summary, and, for each marker, what it is, whose words they are, how to treat it, when it stops applying, and its trust rule (§6). Its frame belongs to the prompt layer, which is Prompt. Each marker's entry is contributed by the capability that writes the marker, through the route every contribution uses — registered from the contributor's own `apply` and withdrawn when it unloads — so the entry is in force exactly when its writer is loaded, contributed plugins included; one fixed text can never cover them. The entries in force are reproducible from facts, and an entry appears where its writer loads, so no entry forks the cached prefix mid-Session; how Prompt meets both properties is Prompt's design. A request a host prepares with no adopted configuration carries the same composition, and there the guide changes only where a writer loads or unloads (§6.2). Consistency is by construction: a writer that is not loaded contributes no entry; a loaded writer's own flow asserts that a request carrying its marker carries its entry; removing or changing a marker changes its entry in the same change. There is no central scan and no central marker list.

**Language.** Model-facing text, protocol and policy alike, is written in English, in one copy: each policy text carries one evaluation, a second language doubles every text and its evaluation, a text that varies by language forks the Session's cached prefix and its evidence, and the protocol names stay English in any translation. "Agent prompt language" is the language the model writes to the operator in, carried as a runtime state source; it is neither a translation of the prompt texts nor a choice between translated copies, and without it the base sentence — that the model answers in the user's language — is the default. Summaries and marker bodies are data: they keep the conversation's language and its identifiers verbatim (§7).

**Judging a change.** A change to a policy text is judged against the current text on the same cases, the same model with the same reasoning, the same budget, and one fixed judge for both arms: each comparison is tested for a regression against the current arm — a loss above `δ = 0.05` that a one-sided mid-p Fisher exact test on both arms' counts confirms, within a family fixed before any data and adjusted with Holm at a family-wise `α = 0.10` — the verdict is `fail`, `pass` or `inconclusive` with a floor of 20 judged runs per arm and case, the candidate text's own token cost is reported beside the scores, and no absolute pass line is used (§8.1). The candidate runs through the shipped assembly, put in place at its author's factory configuration and never by patching strings in an evaluation-only path. The summary prompt has `summary-eval --candidate`; a behavioural text is evaluated by scenarios in the shape of [Timeline branches §12.3](timeline-branches.md#123-evaluation-scenarios), and a change to a text with no harness yet brings its own scenario set, run locally and reported in the pull request. No text is frozen for lack of a harness (§8).

## 2. Basis

1. **What the model reads is a projection of facts.** Model context holds only the events consumed after handoff — adopted inputs, tool results and adopted prompts — and the configuration adopted for the branch carries the system prefix and the tool declarations (principles §4 rule 6). Every text therefore reaches the model through one of a small, fixed set of channels (§3), and nothing else is in its context.

2. **A channel has a carrier, and each text has an author.** The carrier delivers and never owns wording; the author registers or writes the text and owns it. Owning a text where its state is owned is the plugin contract's own rule (principles §5, ADR 002 D1): a central text store would take the wording away from the module whose behaviour it describes.

3. **The channels carry different trust.** A rule binds within its scope, state describes a situation, a marker is information and never permission, the operator's words are the operator's, and tool material is material. The model has to tell which one it reads, and the guide is where it is told (§6).

4. **Policy and protocol change on different licences.** Policy changes on evidence (§8); protocol changes with the behaviour it names, and the facts it is recorded in keep the older form readable (§4).

5. **The channels are closed, the authors are open.** A new channel would change the runtime's delivery and is an infrastructure change under principles §4 rule 8; a text is added by becoming its author inside an existing channel, built-in or contributed. Because the authors are open, the guide's entries are contributed by the writers rather than written once, so one fixed text never has to cover a contributed plugin's markers (§6).

## 3. Channels

Every model-visible text fits one channel below. The table names the carrier, the author, the lifetime and the trust class; §4 and §5 build on it.

| Channel | Carrier | Author | Lifetime | Trust |
| --- | --- | --- | --- | --- |
| System prefix | Prompt: registered System sources, adopted when the root branch starts | base: PromptSources; the Session system prompt: the host or the profile; the skills instructions: Skills | Static for the Session | Instructions |
| Rule and state blocks (`<dim-rule>`, `<dim-state>`) | Prompt: Append inputs, keyed, replayed after a compaction | the capability whose source it is; the body of user and project instructions is the user's or the project's own | Keyed: replaced, revoked or expired by key | Rules: binding within their scope. State: the situation |
| Tool declarations | ToolBinding and ToolRegistry: the configuration's tools part | each tool's owner, built in or contributed | The adopted configuration | Instructions for that tool |
| Runtime-written inputs (markers) | the input path: InputQueue, then Loop | the capability that writes them — TimelineTree, Collaboration, BackgroundExec, ShellInput, and every contributing plugin | History entries; a compaction may replace them | Information, never permission |
| Operator input | the input path | the operator | History | The operator's words |
| Tool results | Loop | material from outside; the refusals and handles the runtime writes into a result belong to the tool's owner | History | Material, never binding; the skill tool's result is the one exception |
| Compaction replacement | ContextPolicy | the summary: the strategy; the digest and the pinned inputs with their handles: ContextCompaction | Replaces a range of history | Background; the live conversation wins |
| The input guide | Prompt: part of the System it composes and the Session adopts, and the same composition on a host-prepared request | the frame: Prompt; each marker's entry: the capability that writes the marker (§6) | Fixed between loads of its writers; an entry takes effect where its writer loads and ends where it unloads | Instructions about the other channels |

**Verification.** The shipped texts fit the table without a new channel: base, the Session system prompt and the skills instructions in the system prefix; the environment and date state, the user and project instructions and the skills list as rule and state blocks; every built-in tool declaration, and the declarations of MCP servers and contributed plugins, in the configuration's tools part; the branch, goal, exec, collaboration, shell-input, Scheduler and Workflow markers as runtime-written inputs; the operator's messages; tool results, with the refusals, codes and handles the runtime writes into them; the summary, the digest and the pinned inputs in the compaction replacement; and the guide. A text that fits none of them would need a new channel, which is an infrastructure change (principles §4 rule 8); no text does.

## 4. Protocol and policy

**The test.** Read the text as the model reads it and edit only the words.

- If the edited sentence becomes false about the runtime, or a mechanism stops working — a marker the model cannot place, a call with the wrong shape, a code the model cannot act on — the text is **protocol**.
- If the runtime behaves exactly as before and only the model's choice of how to work changes, the text is **policy**.
- If the text is the operator's, the project's or a profile's own words, it is **content**: the product carries it, escaped, and never edits it.

**Protocol** is the model-facing side of a mechanism:

- channel and marker names, attributes and their values (`op`, `cleared`, …);
- handles such as `[input <id>]`;
- refusal codes;
- tool schemas and result formats;
- sentences that state a runtime guarantee.

Three requirements follow. Protocol changes only with the behaviour it describes, in the same change as that behaviour's flow test. Because it is recorded in facts — in history and in the adopted configuration — a change keeps a history resumed with the older form understandable; where it cannot, it is a record-format change for the text's owner. And an owner that changes it states the compatibility it keeps.

**Policy** is guidance about how to work: the base prompt's rules, the shell guidance, the usage guidance in tool declarations, the coordination text of the branch and collaboration tools, the Goal wording, the summary prompt's wording. It is replaceable, and every change to it is evaluated (§8).

**Mixed texts.** A text may hold both. A tool declaration is the usual case: its schema and result format are protocol, its description's guidance is policy. The class is per sentence, and the owner marks the split on its own design page (§9.2).

## 5. Scopes

| Scope | What it reaches the model through | Limits |
| --- | --- | --- |
| Product | the texts of its built-in capabilities | it changes no protocol without the behaviour, and no other author's text |
| Host or profile | the Session system prompt, and the trusted sources loaded through the host's prompt loading | the base and the skills instructions are not its to remove; the guide is appended after it |
| Project | its `AGENTS.md`, carried as project-scope rules, and its skills | it changes no protocol, and a project rule grants no permission |
| User | their own messages; `~/AGENTS.md` as user-scope rules; the settings that select values — the model, the execution shell, the answer language, the summary's reasoning setting ([ContextCompaction §13.2](plugins/ContextCompaction.md#132-summaryreasoning)) | it changes no protocol, and a user rule grants no permission |
| Plugin, built-in or contributed | the texts it registers or writes: its sources, its tool declarations, its markers, and their guide entries | the same limits, and no entry that is not its own marker's |

Three limits hold for every scope:

1. **No scope changes the protocol or another author's text.** The protocol changes with its behaviour (§4), and a text belongs to its author (§2, item 2).
2. **The guide's frame is not removable.** The frame — channels, trust, escaping, the channel rule, the restatement — is Prompt's, and no operator, project, plugin or profile removes or replaces it (§6).
3. **System holds only static text.** Text that changes during a Session arrives as a rule or state input, so the cached prefix stays one for the Session (§7).

A user's or a project's words shape how the model works within their scope; they never change what the runtime does and never grant permission. Content inside a marker is material, and a tag describes content — it does not authorize it (§6).

## 6. The input guide

The input guide is the text the model reads about its own input, on every request.

Today it is one fixed text — the frame and the entries of the built-in markers — that request assembly appends to the system field: ContextPreparation appends it to the System it publishes with the adopted configuration, and Loop appends it on the host-prepared path that has no adopted configuration ([Prompt lifecycle](../architecture/rust-agent/prompt.md)). Every request built from the adopted configuration carries it, the summarizer's included.

### 6.1 What it covers

- **Each channel's trust class**: instructions, rules binding within their scope, state describing a situation, information, material, and the operator's words.
- **The channel rule**: the operator's words appear only in the operator's own messages, and everything the runtime adds arrives separately, marked and escaped. The one exception is the `[input <id>]` handle a compaction appends to an input it shortened, which the recall tool opens for the full text. The guide's sentence that everything the runtime adds arrives in separate messages ([input-guide.txt:5](../../crates/dim-agent-capabilities/src/input-guide.txt)) contradicts that handle; the rule the requirement states is the corrected one.
- **Escaping**: content from outside the runtime cannot forge a marker, and unescaped tags inside a marker are the runtime's own.
- **The restatement after a summary**: the state and rules that follow a summary are what is still in force, not new changes.
- **For each marker**: what it is, whose words it carries, how to treat it, when it stops applying, and its trust rule.

### 6.2 Who owns which part

- **The frame** — the channels, the trust classes, escaping, the channel rule and the restatement — belongs to the prompt layer, which is Prompt.
- **Each marker's entry** is contributed by the capability that writes the marker. It takes the route every contribution takes: registered from the contributor's own `apply`, withdrawn when that contributor unloads, so the entry is in force exactly when its writer is loaded ([PromptRegistry](../architecture/rust-agent/plugins/PromptSources.md#promptregistry); [rutis usage R6](rutis-usage.md#r6-contributions-register-with-their-owner)). The route is a language-neutral contract (§9.1), so a plugin contributed through the plugin bridge contributes its entries the same way; one fixed text can never cover them.
- **Prompt provides the means.** It accepts the entries and composes them with the frame into the System it publishes; the entries in force are reproducible from facts, as the other sources are. The guide is part of that System: an entry takes effect when its writer loads and ends when it unloads, because Prompt adopts the guide on every open branch whose composition moved, and between those points the guide is fixed, so no entry forks the cached prefix mid-Session. A writer's tool declarations can reach a request one step before its entry: the tools bind through the tool adoption as soon as their source loads, while the entry arrives through the guide adoption, so a step in that window carries the new tools with the previous System and the next step moves the System once. The design accepts that one-step window rather than making the Loop wait for a pending adoption. The mechanism is Prompt's design (§9.1).
- **The guide arrives with the System.** Loop and ContextPreparation append nothing of their own: a request built from an adopted configuration carries the System Prompt composes, and ContextPolicy reads that System from the adopted configuration for the summarizer's request.
- **The host-prepared path carries the same composition.** A request a host prepares with no adopted configuration carries the guide Prompt composes for the writers loaded — the frame and their entries — the same composition an adopted System carries, so it loses nothing it carries today. There the guide changes only where a writer loads or unloads — the points at which that path's tool declarations already change the request, since it binds its tools per request through ToolBinding — the same points at which the adopted path adopts it, so no entry forks the cached prefix on either path. How both paths read one composition is Prompt's design (§9.1).

### 6.3 Consistency by construction

- A writer that is not loaded contributes no entry, so no entry outlives its writer.
- An entry appears where its writer loads: at the guide adoption that the load triggers for a request built from an adopted configuration, and at the load itself on the host-prepared path; no request assembles a guide of its own.
- A branch adopts the guide once at its open, because a fork inherits its parent's configuration and a restore carries the System it persisted: the adoption is part of the open, so the branch runs with the entries its writers registered at load from its first request on, at the cost of one context read per open.
- A closing branch adopts nothing: its writers withdraw because the branch is shutting down, so a System the branch never ran with is never recorded.
- A loaded writer's own flow asserts that a request carrying its marker carries its entry.
- Removing or changing a marker changes its entry in the same change.
- There is no central scan and no central marker list.

## 7. Language

**Model-facing text is English, in one copy, protocol and policy alike.**

- Each policy text carries one evaluation (§8); a second language doubles every text and its evaluation, and the two languages drift apart.
- The system prefix is the Session's cached prefix; a text that varies by language forks the prefix and its evidence with it.
- Protocol names, attribute values and codes are written in one spelling by the runtime, and a translation carries the English names anyway.
- Instruction-following is not equal across languages, and the product does not claim an evaluation it has not run.

**"Agent prompt language"** is the language the model writes to the operator in. It settles the case where the operator's environment language and the language of their messages differ — English logs or code pasted into a Chinese session, or a first message that is material rather than a request — so the answer language is an explicit choice instead of an inference from the last message. The requirements:

- The value reaches the model as one state source naming the language the model writes to the operator in; the block's form is PromptSources' design (§9.1).
- Without it, the base sentence's rule stands: the model answers in the user's language.
- It is the operator's setting; a project states its language in its own rules, which are content.

Rejected: translating the prompt texts per locale. It doubles the policy texts and their evaluations, forks the cached prefix, and leaves the protocol names in English anyway.

**Data keeps its language.** A summary, a goal objective, a branch's purpose and a tool result stay in the language they arrived in, and every identifier — a path, a symbol, a command, an error — is kept verbatim in every language ([Compaction requirements §3](compaction-quality.md#3-what-compaction-keeps)).

## 8. Judging a change

### 8.1 The rule

**The comparison.** A change to a policy text is judged against the current text, on the same cases, the same model with the same reasoning settings, and the same budget — the same window, tools, turn limit and starting workspace — with **one fixed judge** for both arms. One comparison is the candidate arm's loss against the current arm on one rate: a loss of at most `δ = 0.05` never rejects, and a larger loss is a regression when a one-sided mid-p Fisher exact test on the two arms' counts rejects "no loss". Each evaluation has one family of comparisons, fixed before any data:

- every (scenario, probe) — a pass rate for a `must_show` probe, a violation rate for a `must_not_show` probe;
- the pooled `must_show` pass rate and the pooled `must_not_show` violation rate, which catch a loss spread thinly over many probes;
- the failed-run rate and the unjudged-run rate.

A probe's rate counts judged, non-failed runs only, and failed runs are compared once, as the failed-run rate. Holm adjusts the family at a family-wise `α = 0.10`: the comparison at rank `i` of the ascending p-values is tested against `α / (m − i + 1)`, and none rejects after one fails. The candidate text's own token cost, which every request carries, is reported beside the scores. No absolute pass line is used: an absolute line — a task-success rate, or a judged score on a nondeterministic model — varies by scenario, model and run, and needs many runs to separate a difference from noise, while a relative comparison at the same budget holds across scenarios ([Compaction requirements §6](compaction-quality.md#6-how-we-judge-that-it-works)). The judge is named with the result, and a judge from the model's own family is reported as a limitation of the measurement.

**The verdict.** `fail` when any comparison is a regression, at any number of runs; otherwise `inconclusive` when the floor is unmet or a comparison cannot be made; otherwise `pass`. The floor counts judged runs: every arm needs at least 20 of every scenario or case, and a failed or unjudged run, which enters no probe rate, is compared in its own row but does not count toward the floor — the floor exists for the power of the probe-rate comparisons, and only judged runs contribute to them. A comparison cannot be made when an arm counted no run for it, so a probe the case set carries whose runs in an arm were all failed or unjudged makes the verdict `inconclusive`: no `pass` rests on data that was never judged. A probe kind absent from the whole case set is not that case; it stays out of the family, because there is nothing on either side to compare.

**The measured limit.** The rule was measured on 210 recorded runs, resampled whole — each run's probes together — with 10,000 trials per case at 20 runs per scenario per arm; the method and the measurements are in [#729](https://github.com/arcships/dim/issues/729)'s "Why this rule":

| Case | Reported as a regression |
| --- | --- |
| identical text | 0.7% |
| one `must_show` probe drops from 1.0 to 0.5 | 98% |
| every probe loses 0.15 | 87% |
| one `must_not_show` probe's violations rise from 0.1 to 0.4 | 36% |

The last row is the rule's limit at the floor: catching that loss 80% of the time needs about 44–51 runs per scenario per arm. The report therefore prints both arms' counts, both rates, the loss, the p-value and the Holm threshold for every comparison, so a reader sees a loss the rule did not flag as well as one it did. The measured table resamples whole runs, which keeps each run's probes together. The pooled rows do not: they count each probe of each run as an observation, and the probes of one run share a transcript, so those p-values are optimistic. That errs toward rejecting, which means failing a candidate, and the per-probe rows compare one probe over whole runs.

**Failed and unjudged runs.** A failed run is one that ended on an error or its wall-clock budget, or one whose judging did not happen: there was nothing the judge could be asked, or a judge call failed — a window overflow, a transport or an HTTP error, or a reply cut off at its output cap. An unjudged run is one the judge answered but gave no usable verdicts for after two re-asks, or one whose view does not fit the judge's budget, which is never asked: it enters none of its arm's probe rates, and the report counts it and lists it with its reason; the unjudged-run rate compares it. A failed run is out of the probe rates too, and is compared once, as the failed-run rate. A re-asked verdict comes from a longer conversation with the same judge at the same settings, and both arms are re-asked by the same rule. A run is judged through a view of its export: each request is given against the previous request of its line — the messages added since that request, or the whole message list after a history change — and every other field of the request appears in full only when it changed, or as an unchanged marker when it did not, so nothing is dropped. The judge reads the view in one call. A view that does not fit the judge's declared window — the window less the reply's `maxTokens` — is not split into parts: a probe that counts, orders or holds over a whole run cannot be answered from one part, so the run is unjudged before any call, and the report gives the estimate and the budget. The estimate is the runner's own and is taken before any call, so a predicted overflow is never asked. A call the provider refuses for a window overflow therefore means the declared window or the estimate was wrong, and it stays a failed run: a refusal is how a run outgrowing its model's real window shows up, and failed runs are compared between the arms. The scenario judge is asked for `present` or `absent`; the verdict parser the scenario and summary evaluations share also accepts `contradicted`, which a `must_not_show` probe treats as not shown. An evaluation whose judge declares no window fails before its first call.

**The candidate runs through the shipped assembly.** It is put in place at its author's factory configuration, as the summary strategy's optional system prompt member specified in [ContextCompaction §13.4](plugins/ContextCompaction.md#134-evaluation) allows a candidate prompt to replace the fixed one — the product profile never sets it — and never by patching strings in an evaluation-only path, so both arms run the shipped request building, chunking and failure handling.

**Where it runs.** Real-model runs are local and reported in the pull request. CI holds only deterministic protocol checks.

### 8.2 Harnesses

- **The summary prompt and its length sentence** have `summary-eval --candidate`, which runs the current and the candidate prompt on the same cases, model and target rule ([ContextCompaction §13.4](plugins/ContextCompaction.md#134-evaluation)).
- **A behavioural text** is evaluated by scenarios in the shape of [Timeline branches §12.3](timeline-branches.md#123-evaluation-scenarios): scripted input, the real tools, probes for what a run must show and must not show, both arms judged by the fixed judge. The requirement to the evaluation tooling is a scenario runner with the same candidate injection and the same test (§9.8).
- **Until a text has a harness, a change to it brings its own scenario set**, run locally current against candidate and reported in the pull request. No text is frozen for lack of a harness.
- **Protocol text is not evaluated.** Its flow test changes with it (§4).

## 9. Requirements raised to owners

The format follows [Timeline branches §17](timeline-branches.md#17-requirements-raised-to-mechanism-owners). Each owner changes its own document.

### 9.1 Prompt, PromptRegistry and PromptSources

**Owners:** Prompt, PromptRegistry and PromptSources; implemented under [#548](https://github.com/arcships/dim/issues/548).

- Prompt owns the guide's frame (§6.2) and reviews it; no scope removes or replaces it.
- The frame states the channel rule with the `[input <id>]` exception (§6.1).
- Prompt provides the means by which a capability contributes its markers' entries: registered from the contributor's own `apply` and withdrawn on unload; the entries in force reproducible from facts; an entry taking effect where its writer loads or unloads, with the guide fixed between those points, adopted once at each branch's open and on each open branch whose composition changes, a branch whose composition did not move being left as it is (§6.2). The mechanism is Prompt's design.
- **The contribution route is a contract.** Its form is language-neutral, so a plugin contributed through the plugin bridge contributes its markers' entries too, and Rust is one binding of it (ADR 002 D5, ADR 006). This document raises the contract and does not define it; its form is Prompt's design (§10).
- **One composition.** The frame and the entries in force are one composition, carried by the adopted System and by a host-prepared request alike; how the host-prepared path reads it is Prompt's design, within ADR 002 D3.
- PromptSources keeps `dim/base` and the shell guidance as policy, replaceable at its factory configuration so a candidate can run (§8.1); the environment and date sources state facts only; an instruction body is the user's or the project's words, carried escaped and never edited.
- PromptSources provides the "Agent prompt language" state source, and the block's form is its design (§7).
- Prompt and PromptRegistry carry text and own none of it (§2, item 2).

### 9.2 Every capability that writes model-facing text

**Owners:** every capability that writes a text — built-in or contributed; implemented under [#551](https://github.com/arcships/dim/issues/551).

- It lists its model-facing texts on its own design page, each marked protocol or policy, sentence by sentence where mixed. This document does not restate them, as [Timeline branches §12.1](timeline-branches.md#121-context-mapping) does for its own.
- It contributes the guide entry for each marker it writes, and its own flow asserts that a request carrying the marker carries the entry (§6.3).
- It exposes its policy texts at its factory configuration, so a candidate can run through the shipped assembly (§8.1). The install call registers the configuration's name and provides the value, and a part the profile places without that call declares and reads none, keeping the built-in text.
- It ships a protocol change with the behaviour, the flow test and the compatibility of histories resumed under the older form (§4).

SessionTitle is out of scope: its `SYSTEM` (`runtime/plugins/session-title/index.ts`) is the system text of an auxiliary title call, not text that enters a Session's model context — the title request is not a Session request (§3's channels).

### 9.3 ToolRegistry and ToolBinding

**Owners:** ToolRegistry and ToolBinding; implemented under [#549](https://github.com/arcships/dim/issues/549).

- They carry the declarations and own no wording.
- In a declaration, the description's guidance is policy; a schema, a result format and a refusal code are protocol (§4).
- A declaration's policy change follows §8; its protocol change follows §4.

### 9.4 TimelineTree

**Owner:** TimelineTree; implemented under [#552](https://github.com/arcships/dim/issues/552).

- **Protocol.** The marker names and attributes, the identity blocks' factual sentences, the branch tool's actions and modes, and its refusal codes change only with the branch mechanism, in the same change as the flow tests that assert them (§4).
- **The coordination text for `main` covers the items of [Timeline branches §12.2](timeline-branches.md#122-items-for-the-prompt-layer-429).**
- **Evaluation.** These texts are policy: a change is evaluated with the scenarios of [Timeline branches §12.3](timeline-branches.md#123-evaluation-scenarios) — redone work, over-splitting, flooding, never ending, sibling relay, ping-pong, identity after compaction, asking the user — current against candidate (§8).

### 9.5 Goal

**Owner:** Goal; implemented under [#553](https://github.com/arcships/dim/issues/553).

- **The wording.** A line is not delegated work: it is the same agent on another line of the Session ([Timeline branches §1](timeline-branches.md#1-what-branches-are-for)). The requirement: the objective block's and the continuation block's sentences ([Goal §A.5](plugins/Goal.md#a5-model-visible-texts)), and the statement in [Goal §3.4](plugins/Goal.md#34-a-new-branch-clears-the-inherited-goal) and in [Goal §5](plugins/Goal.md#5-context-mapping)'s row for child branches, say "on another line of this Session", and the meaning stays — the goal belongs to another line, and this line neither continues it nor calls `goal_update` for it. A change to either sentence is a policy change judged under §8 ([Goal §A.5.2](plugins/Goal.md#a52-evaluation)).
- **The classes.** The marker name, its `kind` values and its keys are protocol; the sentences above are policy.
- **The guide.** Goal contributes the entry for its marker when it writes it (§6.2, §9.2).
- **Evaluation.** A goal scenario set — a goal set on `main` continues; a line does not continue the goal it inherited; a completed or blocked goal stops — current against candidate (§8).

### 9.6 ContextCompaction

**Owner:** ContextCompaction; implemented under [#554](https://github.com/arcships/dim/issues/554).

- **The declared parts** are [Compaction requirements §7.1](compaction-quality.md#71-contextcompaction)'s, which carries the required parts of [§3](compaction-quality.md#3-what-compaction-keeps) and is implemented under [#528](https://github.com/arcships/dim/issues/528): the summary prompt declares them in its own words, and the substantive-summary check verifies that they are present ([ContextCompaction §5.2](plugins/ContextCompaction.md#52-productsummarystrategy)).
- **Language.** The prompt is English, and the summary it asks for keeps the conversation's language, identifiers verbatim (§7).
- **Policy.** The prompt and its length sentence are policy: a change is evaluated with `summary-eval --candidate` (§8.2).
- **The guide.** Each marker it writes has its entry: the summary envelope, the `[input <id>]` handle, and the digest message (§6.2, §9.2).

### 9.7 Loop and ContextPreparation

**Owners:** Loop and ContextPreparation; implemented under [#555](https://github.com/arcships/dim/issues/555).

- **No guide of their own.** ContextPreparation publishes for adoption the System Prompt composes, guide and entries included, and Loop carries the System of the request it assembles. Neither appends the fixed text or any entry.
- **Every request that carries the guide today still carries it**: a request built from an adopted configuration carries the guide its Session adopted, the summarizer's included, and the host-prepared path carries the same composition, so no request loses an entry it carries today (§6.2).
- **No entry forks the cached prefix.** A request never assembles a guide of its own; an entry appears where its writer loads or unloads, on both paths (§5 limit 3, §6.2).

### 9.8 The evaluation tooling

**Owner:** the evaluation tooling, `xtask`; implemented under [#550](https://github.com/arcships/dim/issues/550).

- **A scenario runner** with the candidate injection and the test of §8.1: the scenarios are the cases, both arms run the shipped assembly, and the result is reported in the pull request. It interleaves the arms run by run and accepts `--jobs` and repeated `--text`/`--candidate` pairs, so a change that spans two texts of one owner is judged as the one change it is (Goal §A.5.2).
- **A finished evaluation's saved runs are judged again or pooled.** `--rejudge DIR` judges them from the views they saved, with no model turn, keeping each run's own outcome and writing to a new `--out`; `--pool DIR` adds a finished evaluation's saved runs to a report under the same rule and format, and the flag may be repeated to pool several evaluations, writing to none of them, and an `--out` that resolves inside a source, through `..` or a link, is refused before anything is created. Such a report names its sources, has no text or candidate-token row, and in a pool its model and judge rows are the ones given, while each source's own report states the instrument its runs were judged with. Such a report also gives the declines the runner sent again and how many runs did not record it.
- **No absolute pass line and no CI run** (§8.1).

## 10. Rule check

Rules: [design rules](../architecture/rust-agent/principles.md) §1, §3, §4 rules 4, 6, 7 and 8, and §5; [ADR 002](../adr/002-plugin-scope.md); [ADR 008](../adr/008-documentation-ownership.md).

| Rule | How this document complies |
| --- | --- |
| §1 Plugins and capabilities bounded | The guide's frame is the prompt layer's, and each capability contributes its own entries. No part gains another's state |
| §3 Orthogonality | The text travels on existing contracts — sources, inputs and declarations — and the guide's entries on one more, raised to Prompt as a generic capability: it carries no text's business meaning, and any capability uses it, in any binding (step 3 of §3's order) |
| §4 rule 4 No business meaning in core | The delivery classes stay generic; the guide's content is the prompt layer's text and the entries are the writers' texts. Timeline and Dispatcher gain nothing |
| §4 rule 6 Context from consumed events | Every channel is a consumed event or the adopted configuration (§3); the document adds none |
| §4 rule 7 Public events and services | No new event or service: entries and texts travel on existing sources, inputs and declarations, and the guide's entry route is a registration inside Prompt's scope (§9.1) |
| §4 rule 8 Infrastructure unchanged | None is changed: the contribution route is a Prompt-level capability inside its own scope, not a port, checkpoint or adapter on infrastructure |
| §5 Ownership | Each text belongs to its author, and each requirement stays inside one owner (§5) |
| ADR 002 D1 Scope | Each text lives where its state lives; the guide's entries follow their writers' lifetimes |
| D2 No wiring modules | None is added |
| D3 Scope dependencies | Entries follow the existing registration direction. How a host-prepared request reads Prompt's composition is Prompt's design, within D3: a dependency stays at the reader's own or an enclosing scope and points from an extension to the core, never the reverse |
| D4 Configuration | No configuration is added; the candidate injection uses an author's existing factory configuration |
| D5 Language-neutral contracts | The contribution route is a contract and its form is language-neutral: a plugin contributed through the plugin bridge contributes its entries the same way, and Rust is one binding of it. This document raises the contract; Prompt's design defines it (§9.1) |
| ADR 008 D2 No delivery status | The document states requirements only |
| ADR 008 D3 Own scope only | Per-text inventories stay on each owner's design page (§9.2); this document names no per-string list |

Being requirements, the document adds no part, service, event, method or configuration.

## 11. Later options

- **Per-model variants of a policy text**: only with evidence, as a policy change under §8.
- **A harness for every policy text**: the scenario runner covers more texts over time; no text waits for it (§8.2).
- **A per-project answer language**: the answer language is the operator's; a project states its own in its rules, which are content.
- **A translated prompt set**: rejected in §7; it would need its own requirement and evaluation for every text.
