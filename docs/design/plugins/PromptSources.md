# PromptSources

Status: Draft ([#548](https://github.com/arcships/dim/issues/548)). Requirements: [model-facing prompts §7 and §9.1](../model-prompts.md#7-language). Rules: [design rules](../../architecture/rust-agent/principles.md) §1, §4 and §5, [ADR 002](../../adr/002-plugin-scope.md). Scope: Session. Language: Rust. The current capability is on [PromptSources](../../architecture/rust-agent/plugins/PromptSources.md)'s capability page.

PromptSources provides the Session's named prompt sources and the Session's prompt registry. It owns the words of the sources it authors — the base prompt and the shell guidance — and carries, but does not own, the host's system prompt and the user's and the project's `AGENTS.md`. It states the runtime environment and the date as facts. It contributes no input-guide entry: the markers it feeds are Prompt's, which writes them and explains them ([Prompt §4](Prompt.md#4-the-built-in-entries-and-their-writers)).

## 1. Sources and their classes

Each source is one name with an owner, a delivery and a class ([model-facing prompts §4](../model-prompts.md#4-protocol-and-policy)). The class is per text, not per source, where a source's block mixes them.

| Source | Delivery | Order | Author | Class |
| --- | --- | --- | --- | --- |
| `dim/base` | System | 0 | PromptSources | Policy |
| `dim/session` | System | 50 | The host or the profile | Content |
| `dim/instructions/project` | Context (`<dim-rule>`) | 100 | The project | Content |
| `dim/instructions/user` | Context (`<dim-rule>`) | 110 | The user | Content |
| `dim/environment/context` | State (`<dim-state>`) | 200 | PromptSources | Facts, plus two guidance texts (§1.2) |
| `dim/environment/date` | State (`<dim-state>`) | 210 | PromptSources | Fact |
| `dim/language` | State (`<dim-state>`) | 220 | PromptSources | Fact (designed, §3) |

### 1.1 Policy

`dim/base` is policy: the identity sentence and the rules — safety, tone, objectivity, engineering, git, working method, ending a turn, and the linking conventions. It is guidance about how to work; the runtime behaves the same whatever it says.

The shell guidance is policy too. It is the sentence the environment source carries for the operator's shell (`Use POSIX shell syntax. …`, or the PowerShell, cmd, Git Bash and other editions), plus the file-encoding guidance on Windows: how to quote, how to invoke a path with spaces, which legacy encodings to ask the read tool for. None of it states a runtime fact.

Both are replaceable at PromptSources' factory configuration (§2); the product sets neither, so a candidate runs through the shipped assembly ([model-facing prompts §8.1](../model-prompts.md#81-the-rule)).

### 1.2 Facts, and the report on the environment source

The environment source states facts: the working directory, the home directory, the shell's executable and edition, its output encoding, the platform, and the time zone. The date source states the date, in the process's local time zone.

**Two texts in the environment source are guidance, not facts**: `shell_guidance` and `file_encoding_guidance`. They tell the model how to work; the runtime would behave identically if they said something else, and they are false of nothing. They stay where they are — inside the environment block, as the model reads them today — and they become policy members of the factory configuration. Moving them out of the state block would change what every request carries, which is a text change this issue does not make.

### 1.3 Content

`dim/session` is the host's or the profile's system prompt: carried, never edited. The user's and the project's `AGENTS.md` are the instruction bodies: read as files, escaped with the context-block escape rule so their text cannot forge a marker, and carried verbatim — the project's with a one-line header naming its path and that it applies to that directory and below, the user's with a header naming it as `~/AGENTS.md`. Neither is summarized, rewritten or trimmed beyond the surrounding whitespace. A missing or unreadable file produces no entry rather than an empty one.

## 2. Policy at the factory configuration

The factory configuration carries the three policy texts, in the shape [the summary strategy's](../plugins/ContextCompaction.md#134-evaluation) optional system prompt uses: an optional member each, `None` keeping the built-in text, and the product profile setting none of them.

| Member | Replaces |
| --- | --- |
| `base` | `dim/base`'s identity and rules |
| `shell_guidance` | The shell guidance the environment source carries, per shell edition |
| `file_encoding_guidance` | The file-encoding guidance the environment source carries |

A candidate is installed through the shipped assembly — the same plugin, constructed with a configuration — never by patching text at a request boundary ([model-facing prompts §8.1](../model-prompts.md#81-the-rule)).

### 2.1 The product's user override

The product profile sets no factory text, so the App Server may supply the operator's `base` from its user settings ([prompt overrides](../appserver/prompt-overrides.md)). The port is `BasePromptSettingsHost`, optional and read while the Session's sources are assembled: the factory `base` outranks it, a read failure keeps the built-in text, and the value applies to Sessions loaded after the change. The port replaces one field; the shell and file-encoding guidance stay factory-only, because their shape is per shell edition and platform.

## 3. The "Agent prompt language" state source

The source names the language the model writes to the operator in ([model-facing prompts §7](../model-prompts.md#7-language)). It settles the case where the operator's environment language and their messages' language disagree — English logs or code pasted into a Chinese session — by making the answer language an explicit choice instead of an inference from the last message.

**The block.** Source `dim/language`, delivery State, order 220, so it follows the environment and the date. Its body is one sentence, the language named in English:

```text
<dim-state source="dim/language">The operator set this session's reply language to Simplified Chinese.</dim-state>
```

The body states the setting itself, and asks for nothing: a state block is read as the situation, never as an instruction, and it names the language the model writes to the operator in ([model-facing prompts §7](../model-prompts.md#7-language)). The language names are the ones the operator selects — English, Simplified Chinese, Traditional Chinese, Japanese, Korean, Bahasa Indonesia, German, French, Spanish, Portuguese (Brazil), Portuguese (Portugal). The sentence's wording is policy: it is judged against the current text when the source is built ([model-facing prompts §8](../model-prompts.md#8-judging-a-change)), and this design does not build it.

**No selection, no block.** When the operator has chosen no language — the system default — the source produces no entry and the base sentence's rule stands.

**The carrier.** The client carries the operator's choice into the Session: `session.create {language?}` seeds it and `session.language.update` changes it, and the value is `SessionConfig.language`, an optional member of the Session's config record. The daemon never reads the client's interface-locale file. The PromptSources Session part provides the Session's live value as the `SessionLanguage` cell and reads it on every load; the cell's `get` is the `sessionLanguage` data face, which other languages read by name, and the branch part re-exposes that cell on each branch for the Session methods that change it, as the Skill list is re-exposed. The face has no write: an update also writes the Session record and has Prompt adopt the source. A Session created without a choice, including a record written by an older client, has no value and produces no entry. An update adopts the value on `main` and, before it replies, on every loaded branch, so that branch's next request carries it without a wake; a branch loaded later reads the current value when it loads, and each branch's periodic check remains the fallback.

## 4. Dependencies

| Service | Purpose |
| --- | --- |
| `S:PromptRegistry` | Register each source by name, until this part unloads |
| `P:HostPlatform` | Home, environment and the operator's shell |
| `S:SessionConfig` | Working directory and the host's system prompt |

The language source reads the part's own `SessionLanguage` cell rather than an injected service.

## 5. Cross-module requirements

1. **The operator's language for the Session** (§3): the Session's config carries it (`SessionConfig.language`), `session.create {language?}` seeds it and `session.language.get/update` read and change it, and this plugin reads the Session's live value. No configuration file carries it, and no other module decides it.
2. **The plugin bridge** ([Prompt §6.1](Prompt.md#61-the-plugin-bridge)): no further requirement reaches PromptSources; the markers its sources feed are Prompt's, and Prompt's page raises what they need.

## 6. Rule check

| Rule | How this design complies |
| --- | --- |
| §1 Plugins and capabilities bounded | The sources, their classes and their texts are PromptSources'; the guide's frame and the entries of the markers it writes are Prompt's |
| §3 Orthogonality | The sources travel on the existing prompt-source contract; this design adds no route of its own |
| §4 rule 1 Apply registers, cleanup releases | The sources register from `apply` and end with the part's generation |
| §4 rule 6 Context from consumed events | Sources keep reaching the model through adoption and Append inputs; this design adds no path |
| §4 rule 7 Public events and services | The source adds no protocol method or event of its own: it is one more name in an existing set. The Session's value is a service the owning part provides at the Session scope and re-exposes on each branch, as the Skill list does (ADR 002 D1, D2) |
| §4 rule 8 Infrastructure unchanged | None is changed. The language fact is raised, not built |
| ADR 002 D1 Scope | The sources live in the Session, where their state lives |
| D4 Configuration | No configuration file is added; the policy members are the plugin's factory configuration, as the summary strategy's system prompt is |
| D5 Language-neutral contracts | The language source is a state block, data on an existing channel |
| ADR 008 D2 No delivery status | The document states the design only |
| ADR 008 D3 Own scope only | The guide's route and the frame stay on [Prompt](Prompt.md)'s page |

## 7. Binding appendix

```rust
/// Factory configuration of the sources' policy texts. The product sets
/// none of them, so the built-in texts are in force.
#[derive(Clone, Default)]
pub struct PromptSourcesConfig {
    /// `dim/base` in place of the built-in identity and rules.
    pub base: Option<Arc<str>>,
    /// The shell guidance the environment source carries, per edition.
    pub shell_guidance: Option<Arc<dyn Fn(ShellEdition) -> String + Send + Sync>>,
    /// The file-encoding guidance the environment source carries.
    pub file_encoding_guidance: Option<Arc<str>>,
}

impl PromptSourcesPlugin {
    /// The plugin with the built-in policy texts in force.
    pub fn new(names: &Names<'_>, today: Today) -> rutis::Result<Self>;
    /// The plugin with a candidate's texts at its factory configuration.
    pub fn with_config(
        names: &Names<'_>,
        today: Today,
        config: PromptSourcesConfig,
    ) -> rutis::Result<Self>;
}
```

The language source's binding is one more arm of the source loader: name `dim/language`, delivery `State`, order 220, body from the operator's setting when one is carried and no entry when none is.
