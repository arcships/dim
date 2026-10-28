# Rust 会话存储格式

The current native format is **v20**, with source-owned `settings_documents`,
SessionStore's Session-file rows, its title metadata, request messages stored
as lists of content-addressed nodes, extension event rows, Session
configurations and Agent profiles that may omit their mount list, and the two
branch end events. Startup requires the exact v20 SQL schema and a single
format marker of 20 before writable PRAGMAs, or the exact v19, v18, v17, v16,
v15 or v13 schema with its own marker, which the same startup transaction
upgrades in place to v20 one step after another ([S5a v15 decision](#s5a-v15-格式决定),
[S5b v16 decision](#s5b-v16-格式决定), [#193 v17 decision](#193-v17-格式决定),
[#2 v18 decision](#2-v18-格式决定), [#364 v19 decision](#364-v19-格式决定),
[#427 v20 decision](#427-v20-格式决定)). Older databases and the removed
branch-only v14 `user_configuration` format are unsupported and preserved, never
migrated, downgraded or interpreted as v13, v15, v16, v17, v18, v19 or v20. No
migration API or startup option is provided. S5c (Session deletion and fork) kept
v15 unchanged ([S5c decision](#s5c-格式决定)). v13 document bytes remain valid in
v20, and v16 request rows stay readable as written.
Storage versions are independent of RPC versions. Private MCP/Skill business documents do not belong
in loader rows, events, diagnostics, Sessions or Timelines.

## Current v20 tables

下表逐列对应 `SqliteStorage::SCHEMA`。新表按业务实体命名，其中 `agent_sessions` 与旧 TypeScript 的 `sessions` 区分；当前启动门禁仍只接受本格式，不因此支持旧表同库存取。`—` 表示 SQL 中没有显式 `DEFAULT`；可空列若省略会得到 SQLite 的隐式 `NULL`。SQLite rowid 表中的 `TEXT PRIMARY KEY` 不自动保证非空，因此下表如实标出 v1 的两个缺口；正常写入仍必须给出 ID。

| 表.列 | SQLite 类型 | 可空 | 默认 | 约束与含义 |
| --- | --- | --- | --- | --- |
| `agent_store_format.version` | `INTEGER` | No | — | Primary key, `CHECK(version = 20)`; exactly one row |
| `settings_documents.source` | `TEXT` | No | — | Primary key; `user/mcp`, `user/skills`, `user/mcp-projects` or `user/mcp-plugins`; workspace files are not mirrored |
| `settings_documents.version` | `INTEGER` | No | — | Positive per-source CAS revision; token `native:<source>:<version>` |
| `settings_documents.document` | `TEXT` | No | — | Plain business payload JSON, without a `{kind, document}` envelope; unknown business fields are preserved |
| `settings_documents.imported_revision` | `TEXT` | No | — | Original file SHA-256 or `missing`; import provenance, not a CAS token |
| `provider_imports.kind` | `TEXT` | 否 | — | 联合主键；`provider` 或 `selection` |
| `provider_imports.id` | `TEXT` | 否 | — | 联合主键；来源业务 ID，删除 Provider 不删除标记 |
| `provider_imports.source_schema` | `TEXT` | 否 | — | 已检查的来源两表结构 SHA-256 |
| `provider_imports.source_version` | `INTEGER` | 否 | — | 来源行版本，不作目标 CAS 版本 |
| `provider_imports.source_digest` | `TEXT` | 否 | — | 来源完整行 SHA-256，不包含源文件路径，不通过 RPC 返回 |
| `providers.provider_id` | `TEXT` | 否 | — | 主键；managed、bundled 与 custom 共用 |
| `providers.version` | `INTEGER` | 否 | — | 正整数，目录与启用状态的 CAS 版本 |
| `providers.record` | `TEXT` | 否 | — | authority-specific `ProviderRecord` JSON；managed 记录不含凭据，静态记录可含请求凭据且只允许非秘密投影读取 |
| `provider_selections.scope` | `TEXT` | 否 | — | 主键，`global` 或 `workspace:<absolute cwd>` |
| `provider_selections.version` | `INTEGER` | 否 | — | 正整数，选择的 CAS 版本 |
| `provider_selections.provider_id` | `TEXT` | 否 | — | Provider ID；与 model_id 共同标识目标 |
| `provider_selections.model_id` | `TEXT` | 否 | — | 对应 Provider 的模型 ID；模型暂不可用时保留选择 |
| `agents.agent_id` | `TEXT` | **SQL 允许** | — | 主键；正常写入必须是非空 Agent ID |
| `agents.profile` | `TEXT` | 否 | — | `AgentProfile` JSON，创建后固定 |
| `agents.is_default` | `INTEGER` | 否 | — | `CHECK(is_default IN (0, 1))`；唯一的 1 由 `agents_default_unique` 保证至多一个，至少一个由目录校验保证 |
| `agent_sessions.session_id` | `TEXT` | 否 | — | 主键，原 Session ID |
| `agent_sessions.created_at_ms` | `INTEGER` | 否 | — | `CHECK(created_at_ms >= 0)`；创建事务写入真实 Unix 毫秒 |
| `agent_sessions.config` | `TEXT` | 否 | — | `SessionConfig` JSON，创建时的配置 |
| `session_owners.session_id` | `TEXT` | **SQL 允许** | — | 主键，外键到 `agent_sessions(session_id)`；业务要求每个 Session 恰有一条非空 owner |
| `session_owners.agent_id` | `TEXT` | 否 | — | 外键到 `agents(agent_id)` |
| `session_owners.protocol_version` | `INTEGER` | 否 | — | `CHECK(protocol_version IN (1, 2, 3))`；Session 所属协议版本 |
| `timeline_branches.session_id` | `TEXT` | 否 | — | 联合主键第 1 列；外键到 `agent_sessions(session_id)` |
| `timeline_branches.branch_id` | `TEXT` | 否 | — | 联合主键第 2 列；同 Session 内唯一 |
| `timeline_branches.info` | `TEXT` | 否 | — | `TimelineInfo` JSON：`id`、可选 `parent`、`branch_point`、`create_anchor` |
| `timeline_branches.sequence` | `INTEGER` | 否 | — | `CHECK(sequence >= 0)`；本分支最新已提交事件序号，派生水位 |
| `timeline_content.session_id` | `TEXT` | 否 | — | 联合主键第 1 列；外键到 `agent_sessions(session_id)` |
| `timeline_content.hash` | `TEXT` | 否 | — | 联合主键第 2 列；同 Session 内内容地址 |
| `timeline_content.kind` | `TEXT` | 否 | — | `node`、`tools` 或 `system`，以及 v16 写入的 `message`；v1 无 SQL 枚举 `CHECK`，读取时校验 |
| `timeline_content.body` | `TEXT` | 否 | — | 对应类型的完整 JSON 正文；写入后不可原地修改 |
| `timeline_events.session_id` | `TEXT` | 否 | — | 联合主键第 1 列；与 `branch_id` 一起外键到 `timeline_branches` |
| `timeline_events.branch_id` | `TEXT` | 否 | — | 联合主键第 2 列 |
| `timeline_events.sequence` | `INTEGER` | 否 | — | 联合主键第 3 列，`CHECK(sequence > 0)`；物理行首事件的本分支序号 |
| `timeline_events.last_sequence` | `INTEGER` | 否 | — | `CHECK(last_sequence >= sequence)`；物理行末事件序号，合并行可大于 `sequence` |
| `timeline_events.kind` | `TEXT` | 否 | — | `EventKind::key()`，如 `model/call`、`step/text`；与正文逐事件核对 |
| `timeline_events.body` | `TEXT` | 否 | — | 下节定义的 `Row` JSON，包含事件、引用、before 轨迹及批次终点 |
| `timeline_events.event_id` | `TEXT` | 是 | 隐式 `NULL` | 普通行保存原事件 ID；输出合并行为空；恢复时核对正文 |
| `timeline_snapshots.session_id` | `TEXT` | 否 | — | 联合主键第 1 列；与 branch_id、sequence 一起外键到事件锚点 |
| `timeline_snapshots.branch_id` | `TEXT` | 否 | — | 联合主键第 2 列 |
| `timeline_snapshots.sequence` | `INTEGER` | 否 | — | 联合主键第 4 列，`CHECK(sequence > 0)`；已提交 completed 压缩事件序号 |
| `timeline_snapshots.interpreter` | `TEXT` | 否 | — | 联合主键第 3 列；解码前可检查的解释器兼容合同 |
| `timeline_snapshots.event_id` | `TEXT` | 否 | — | 锚点的精确事件 ID，写入和候选查询校验 |
| `timeline_snapshots.state` | `TEXT` | 否 | — | 不透明原始字符串；SQLite 不解析或反序列化 |
| `session_files.file_id` | `TEXT` | 否 | — | 主键；小写带连字符的 UUID v4，也是 `<db>.session-files/` 下的目录名（v15） |
| `session_files.node` | `TEXT` | 是 | 隐式 `NULL` | 写入时 Session 的资源节点；`NULL` 表示控制节点。owner 全部删除后仍保留，供清扫定位 |
| `session_files.writer` | `TEXT` | 是 | 隐式 `NULL` | 写入未完成时是 SessionStore 代次 ID；提交或放弃后为 `NULL` |
| `session_file_owners.session_id` | `TEXT` | 否 | — | 联合主键第 1 列；外键到 `agent_sessions(session_id)` |
| `session_file_owners.file_id` | `TEXT` | 否 | — | 联合主键第 2 列；外键到 `session_files(file_id)` |
| `session_metadata.session_id` | `TEXT` | 否 | — | 主键；外键到 `agent_sessions(session_id)`；每个 Session 至多一行，没有行表示修订号 0、没有标题（v16） |
| `session_metadata.revision` | `INTEGER` | 否 | — | `CHECK(revision > 0)`；该 Session 已提交 `setTitle` 的次数，`setTitle` 的 CAS 修订号 |
| `session_metadata.title` | `TEXT` | 是 | 隐式 `NULL` | 标题文本，原样保存；清除后为 `NULL`，行保留 |
| `session_metadata.title_source` | `TEXT` | 是 | 隐式 `NULL` | `CHECK(title_source IN ('auto', 'manual'))`；表级 `CHECK((title IS NULL) = (title_source IS NULL))` |

v1 的外键只约束上述表列；`info.parent`、`body` 中的内容哈希、before 轨迹和检查点位置都是 JSON 内关系，必须在存储读取边界校验。`timeline_content` 的地址是小写十六进制 `SHA-256(kind + NUL + body)`，其中 `body` 是写入的原始 JSON 字符串字节；同一内容只在同一 Session 内共享。创建 Session、owner 和 `main` 分支是同一事务；写入轮把事件、首次出现的内容、由 `branch/create` 生成的分支目录及分支 head 放在同一事务，提交后才发布。

## 持久化 JSON 类型

### 配置来源与单写者

Settings owns each source independently. Original v13 rows under `user/mcp` or
`user/skills` contain the plain MCP or Skill document JSON, not a tagged envelope.
`user/mcp-projects` holds MCP's user-local project enablement
(`{"enabled":{"<canonical workspace>":["<server id>"]}}`, ADR 003 D3). Its import
file is `mcp-projects.json`, which only the standalone file backend writes; a
native home normally has none, so the first read imports `missing` and an empty
record. The row is an additive source within
v13: the schema and format version are unchanged, and older binaries ignore it.
`user/mcp-plugins` holds the MCP documents plugins registered with Mcp
(`{"plugins":{"<plugin id>":{"dir":"<plugin directory>","mcpServers":{…}}}}`,
[PluginManager §3.7](../plugins/PluginManager.md#37-content-registration)), and
`user/skills` may carry `pluginSkills` (`{"<plugin id>":"<skills root>"}`); both
are additive in the same way.
Generic Settings persistence retains these exact payload shapes, not loader
patches or UUID snapshots. Business interpretation and validation belong to the namespace owner.
Unknown extension fields and disabled Skill IDs are preserved.

For example, these original v13 rows store `document` as TEXT containing the
plain payload, not a serialized SettingsDocument wrapper:

```json
[
  {"source":"user/mcp","version":1,"document":"{\"mcpServers\":{}}","imported_revision":"missing"},
  {"source":"user/skills","version":2,"document":"{\"disabledSkillIds\":[\"example\"]}","imported_revision":"missing"}
]
```

Their CAS tokens are `native:user/mcp:1` and `native:user/skills:2`. Updating
Skills increments only its version and preserves `imported_revision`.

The first successful source read imports its legacy file into SQLite. Document
and import provenance commit atomically in one row; concurrent imports adopt
one winner. Invalid files or failed writes create no authoritative row and do
not fall back to empty settings. Originals remain available for repair/retry.
After import, SQLite is authoritative: edits CAS the named source revision,
increment only that source's version, and never rewrite or reread the old file
as runtime authority. Import provenance does not change on ordinary edits.
Project `.mcp.json` keeps file authority and content CAS; project enablement is
never written there. Standalone hosts may explicitly use the file backend.

Revision notifications follow successful persistence and contain no business
document bodies. They certify neither loader adoption nor generation settlement.
Loader rows represent runtime instances, not the MCP/Skill document authority.
Importing legacy settings files does not authorize migrating users' databases.

### 目录与内容

Managed `providers.record` is `{kind:"managed", account, models, modelConfigOverrides, disabledModelIds, version, modelPreferences?}`. The optional `modelPreferences` stores per-model client preferences under the #329 format decision; missing means none. Each model is `{entry, protocol, unavailableReason, zdr}`; the plugin returns the normalized `ModelEntry` and public host protocol. Only non-secret account identity is stored here. Refresh and access tokens remain in the separately supplied plugin's account store. Managed and static variants share the existing `providers` table; the v10 table and column layout is unchanged, with the format marker advanced to v11. Provider catalog replacement and user overrides are owned by the App Server, as described in the [Provider contract](../../../crates/dim-app-server/README.md).

静态目录的 record 为 `{kind, displayName, driverKind, auth, credentialAuthority, baseUrl, network, headers, credential, models, remoteModels, endpointTypes, modelConfigOverrides, disabledModelIds, enabled, definitionFingerprint, version, userModels?, modelPreferences?, endpointOverrides?}`。可选的 `userModels`、`modelPreferences`、`endpointOverrides` 沿 #329 格式决定保存用户模型、偏好和端点覆盖，缺失表示无对应配置。`kind` 为 `builtin` 或 `custom`；models 和覆盖均为完整 `ModelEntry`；`definitionFingerprint` 是 bundled 定义的 SHA-256，自定义记录为 null。`credentialAuthority` 为 `static`、`openai_codex` 或 `grok`，缺省仅解释为静态来源；后两者只允许对应内置 Provider 的固定非秘密绑定，credential 必须为空。`credential` 可保存静态凭据或不透明秘密引用，但不写回引用解析结果；`network` 保存逐连接代理与超时，只读 `provider.list` 不返回代理 URL；`provider.management` 只返回去除 user info 的 `proxyUrl`，代理认证仍不返回，展示值不得自动写回。`remoteModels` 标记已取得的独立模型快照，避免 bundled 升级覆盖；`endpointTypes` 保存显式协议映射，派生路由不落盘。凭据和 header 值只由 resolver 使用，管理面仅返回非秘密摘要。写入及读取边界校验模型、地址和请求头，外层 SQL 版本须与 record.version 一致；seed、编辑和删除规则见[Provider合同](../../../crates/dim-app-server/README.md)。

`agents.profile` 是 `AgentProfile`：`{name, description, systemPrompt, tools?, model?}`；`model` 若存在为 `{providerId, modelId}`。`agent_sessions.config` 是 `SessionConfig`：`{permissionPolicy, cwd, model, systemPrompt, tools?, collaborationRole, interactive?}`；两者的 `tools` 是字符串数组形式的挂载列表，没有挂载列表时省略（[Session 访问 A.3](../contracts/session-access.md#a3-sessionconfig-fields)，格式决定见 [#364 v19 格式决定](#364-v19-格式决定)）；其中 `model` 是 `ModelSelection {providerId, modelId, config?, metadata?}`，创建时固定完整模型参数，省略参数的身份记录仍可读取。`permissionPolicy` 为 `{preset, allowedTools, disallowedTools}`，`collaborationRole` 为 `leader` 或 `member`。可选字段 `interactive`（布尔，缺省 `true`）由[路线图](../roadmap.md) C2 加入，已实现，格式决定如下：保持 v13，不升级到 v14，这是配置 JSON 中增量的可选字段；缺少它的已存储记录表示 `true`，因为该字段出现之前创建的每个 Session 都是客户端 Session；只写入 `false`，交互式记录不含该字段，与该字段出现之前写入的记录格式相同；这一省略规则写在 `SessionConfig` 的 serde 定义上，原生存储是它唯一的序列化方，在其他地方暴露 `SessionConfig` 须另行决定；旧版守护进程忽略该字段，会把非交互记录读成交互式，这一点可以接受。字段合同见 [Session 访问 A.3](../contracts/session-access.md#a3-sessionconfig-fields)。`timeline_branches.info` 是 `BranchRecord`：`main` 为 `{id:"main"}`，子分支为 `{id, parent, branch_point, create_anchor}`；后两项是父分支的本地序号。运行时的 `TimelineInfo` 是它加上 `source`，该字段不落盘。JSON 字段名按各类型的 serde 定义，不能由 SQL 列名的拼写推断。

`context/configured` 的 `ModelConfiguration` 分别固定模型与工具部分：模型使用上述参数，`tools` 为已采用声明，可选 `toolSources` 为 `{key,version}[]` 的来源版本；没有来源时省略。模型更新保留已采用的工具部分，工具采用保留模型部分，完整合同见[App Server合同](../../../crates/dim-app-server/README.md)。

`timeline_content.body` 的 `node` 是 `{parent,message}`：`message` 是一条消息，`parent` 是前一条消息所在节点的地址，首条消息为 `null`，所以一个节点标识从首条到它的整段消息前缀（v17）。消息正文 `{role,content}` 与 v16 的 `message` 种类相同，`role` 为 `user`/`assistant`/`tool_result`，`content` 为文本或 `MessagePart[]`；v16 写入的 `message` 行照常可读，v17 不再写入。`tools` 是 `ToolDefinition[]`，每项为 `{name,description,inputSchema,source,risk?,metadata?,fileChanges?}`，嵌套的 schema/metadata/fileChanges 保持现有类型定义；`system` 是文本或 `SystemPromptPart[]`，当前部件为 `{type:"text",text,cache_control?}`。各类正文均直接按对应运行时类型的 JSON 序列化，节点只多出 `parent`。

### 事件封装

`timeline_events.body` 的完整外层联合为：

```text
Row =
  { encoding: "event", record: WireEvent }
  | { encoding: "output", turn_id: string, request_id: string,
      started_at: integer, records: WireEvent[] }
WireEvent = { event: AgentEvent, refs: ListReferences | References | null,
              trace: WireTrace | null, batch_end: integer }
ListReferences = { list: string | null, length: integer,
                   tools: string | null, system: string | null }        v17
References = { messages: string[], tools: string | null, system: string | null }   v16, read only
WireTrace = { event_id: string, original: WireDraft, steps: WireStep[] }
WireDraft = { event: EventDraft, refs: ListReferences | References | null }
WireStep = { handler: string, result: WireDraft | null }
AgentEvent = { id: string, sequence: integer, create_at: integer,
               tags?: string[], type: string, name: string, data: object }
EventDraft = { tags?: string[], type: string, name: string, data: object }
```

`AgentEvent` 与 `EventDraft` 的 `{type,name,data}` 是 `EventKind` 标记联合，核心定义的事件由该变体的类型定义（如 `ModelCallData`、`ContextCompactionData`）校验；存储没有第二套业务事件定义。核心未定义领域的扩展事件（`EventKind::Extension`，[ADR 001](../../adr/001-harness-foundation.md#events-and-ownership-issue-2)）以同一信封原样保存 `data`，`kind` 为 `<type>/<name>`；核心保留领域的行只按其类型解码，解码失败即报错，不当作扩展事件。格式决定见 [#2 v18 格式决定](#2-v18-格式决定)。`ModelRequest` 正文为 `{providerId,modelId,messages,tools?,system?,config?,metadata?}`，末尾四项是可选字段，缺省时省略，不能写 `null`。`event` 行有且仅有一个 `record`，其 `sequence = last_sequence = timeline_events.sequence`；`output` 行的 `records` 非空、同一 `kind`/turn/request，逐条保留 ID、sequence、`create_at`、tags、delta、refs、trace 和各自 `batch_end`。输出行只在 `step/text` 或 `step/thinking` 出现，`turn_id`/`request_id` 提到行级并从各 `record.event.data` 移除，读取时补回；`started_at` 是首事件的 `create_at`。合并不改变逻辑事件或原子批次，`last_sequence` 必须等于最后一条的序号。

写入时只有前一物理行与当前增量同 `kind`、同 `turn_id`/`request_id`，前一行编码长度小于 4096 字节，且当前事件的 `create_at - started_at < 1000` 毫秒，才向该行追加；否则新建 `output` 行。阈值检查发生在追加前，故追加后的行可以超过 4096 字节。读取现有输出行不依赖这些写入侧阈值，只校验行内事件与区间。

### 请求引用与 before 轨迹

仅 `context/prepare` 与 `model/call` 的 `data.request` 可带 `refs`；存储中的 `request.messages` 为 `[]`，`tools` 和 `system` 字段移除，其余请求字段保持原样。其他事件及非请求 before 草稿的 `refs` 为 `null`，字节与 v16 相同。v17 写入 `ListReferences`：

- `list` 是请求消息列表最后一个节点的地址，`length` 是消息条数；`list` 恰在 `length` 为 0 时为 `null`。
- 写入方取该记录所在分支逻辑历史中的上一个请求（分支自己在它之前的最后一个请求事件，否则依次是各祖先在切点及以前的最后一个），两者 `messages` 的最长公共前缀（先比指针，再用 `Message` 的 `==`）沿用上一个请求的节点，其余每条消息写一个新节点，父节点为它前一条消息的节点。没有上一个请求、上一个请求是 v16 行，或压缩之后前缀为空时，从头写一条新列表。节点以 `INSERT OR IGNORE` 写入。
- `tools`/`system` 缺席为 `null`；与上一个请求相等（先比指针，再比 `==`）时沿用其地址，否则写成 `kind=tools`/`system` 的内容行。
- `WireTrace.original` 和每个非空的 `WireStep.result` 中的请求按同一规则编码，以记录自身的事件请求为上一个请求；没有 before 轨迹时 `trace` 为 `null`。

v16 写入的 `References` 照常读取：`messages` 按顺序引用 `kind=message` 的内容行，即使为空也存在。两种形状按字段区分，各自拒绝对方的字段和未知字段。这些 `null` 属于存储封装，事件类型自己声明为可选的字段仍按原 serde 规则省略。字节只由分支的逻辑历史和事件决定，与写入轮次、写入方缓存或重启无关。

### 可丢弃恢复快照

Completed compaction facts contain source through, `retain_from` (the Timeline
event id of the first event kept verbatim), `replacement`, `strategy`, baseline
and diagnostics, not snapshots. Disposable snapshots commit separately without
affecting fact durability. `RecoverySnapshot={sequence,event_id,interpreter,state}`;
the default `dim/context-projection/v3` interpreter encodes `{payload,sha256}`,
with the complete ProjectionCheckpoint JSON string below as payload. SQLite
continues to treat state as opaque TEXT; the snapshot change needed no new database format.

摘要算法为 SHA-256：先写入 UTF-8 `dim/recovery-snapshot/v1` 和一个 NUL，再写入 sequence 的 8 字节大端编码，随后依次写入 event_id、interpreter、payload；每个字符串先写入 UTF-8 字节长度的 8 字节大端编码，再写入原始字节。sha256 保存小写十六进制。解释器在解码 payload 前校验摘要，覆盖 resume、全部状态及锚点身份。旧 v1 无校验快照及按条目 ID 记录切点的 v2 快照直接绕过，不通过补算摘要升级；v3 不接受裸检查点或缺失摘要。历史事实及摘要文本不受影响。

`HistoryRenderer` 同时拥有 `encode_checkpoint` 和 `decode_checkpoint`，自定义格式必须成对提供并管理自己的 interpreter 标识。完整性校验检测编码后发生的损坏，不是认证或生产者语义证明：可信 builder 必须生成与完整事实重放等价的状态，并通过差分测试验证。已知 builder 语义缺陷需要修复并更换解释器版本，不能给错误状态重算摘要。有效快照仍只读取恢复位置以后的事实，不在每次读取时重复完整重放。

候选查询只选择相同 interpreter 且 sequence 不大于固定 through 的最新快照，并连接事件锚点验证 ID 与序号，不读取事件种类或内容；孤立或元数据不匹配的快照被忽略。锚点能否承载状态由快照所有者的解码回调判断：默认解释器只接受自己完成的 compaction 事实，且其 `retain_from` 与状态一致、through 早于锚点（#130）。Session 恢复不校验可选 state；缺失、不兼容、损坏、越界或锚点不符的快照回退事实重放，replacement 仍保留。运行时在裁剪前校验解码后的 resume 属于固定祖先链和 source through。`Timeline.context` 默认完整重放；加速显式调用 `context_with_checkpoint`，默认能力使用 `history::read_context`。事实或必需引用损坏仍失败并保留原字节。回滚先删除 snapshots 再删除 events。

```text
ProjectionCheckpoint = {
  resume: { branch_id: string, sequence: integer },
  state: {
    compacted_through: string,
    prompts: { entries: PromptEntry[], revision: PromptReference | null,
      system_revision: PromptReference | null, system: string,
      turns: string[], tasks: PromptScope[] },
    delivered: ContextBaselineEntry[],
    inputs: [string, InputMessageData][],
    configuration: ContextConfiguredData | null,
    output: MessagePart[], open_tools: string[], output_open: boolean
  }
}
```

上述检查点中的叶子类型仍是事件类型的原 JSON 形状，不另做存储转换：`PromptReference={timeline,revision}`；`PromptScope={kind:"branch",timeline}` 或 `{kind:"turn"|"task",timeline,id}`；`PromptEntry={source,version,body,scope,delivery,order}`，其中 `delivery` 为 `system`/`context`/`state`。

`ContextBaselineEntry={key,sourceEvent,message}`；`InputMessageData={submission_id?,user?,blocks?,mode?}`，`user.content` 是文本/图片块列表。`blocks` 项只有原文一种：`{type:"raw",text,key?,retire?}`，`text` 是能力写入的最终文本，`key` 表示仍然有效的内容，`retire:true` 结束该 key；语义见[架构约定](../../architecture/README.md)。

`ContextConfiguredData={timeline,revision,previous?,prompt?,settings,system?}`，`settings={providerId,modelId,tools?,toolSources?,config?,metadata?}`；`toolSources` 是 `{key,version}[]`，为空时省略。`Message` 是 `{role,content}`，`content` 是文本或 `MessagePart[]`；`MessagePart` 保持 `text`、`image`、`tool_use`、`tool_result`、`thinking` 的标记联合；text / thinking / tool_use 可保存闭合 `providerMetadata`，原生签名、Responses item 身份及 summary 分组见 [模型宿主](../../../crates/dim-agent-aimux/README.md)。内部 `step/model-content` 保存成功输出的完整有序 parts；增量事件仍服务流式显示。

### 读取校验

读取时先解析 `Row` 和运行时事件，再按 `(session_id, hash)` 查请求引用的内容、复核 hash 与 kind，并重建 `Arc<ModelRequest>`；同一次读取或恢复校验中，内容行在加载时复核一次，此后复用解码结果，被缓存上限清出后重新加载并复核。`ListReferences` 从 `list` 向首个节点遍历：每个节点须是 `kind=node`、hash 匹配、正文可解析为 `{parent,message}`，遍历恰好经过 `length` 个节点，最后一个的 `parent` 为 `null`，其余都有父节点。同一次读取保留上一个由列表重建的事件请求及其各消息的节点地址，遍历遇到与它同位置的同一节点即停止，其余前缀取自该请求；共用节点已在重建该请求时复核。遍历每加载至多 256 个节点释放一次连接，下一次持有从中断处继续。随后校验序号、行 kind、批次、before 的 `event_id`、分支关系和所有 completed 事实的 through 边界。可选 snapshot state 不参与 Session 恢复校验。坏行保留原字节并归属该 Session；缺失或损坏的节点以 "missing request content"、"request content hash mismatch" 或 "request list length mismatch" 明确失败，不得通过默认值把损坏请求还原为空请求或部分请求。

## 格式版本

`agent_store_format.version` 同时标识 SQL 结构、持久化 JSON 和内容寻址规则。已发布格式发生不兼容变化时，必须定义新版本及读取或转换路径。保持版本的条件是旧读取器能正确解释新值、旧写入器不会重写丢失新值，且存储不变量不变。纯运行时重构不改变格式版本。

首版定稿前的开发库兼容范围由上层持久化设计明确，不预设下一版本或迁移字段；S5a 的 Session 文件、S5b 的标题元数据、#193 的请求列表、#2 的扩展事件、#364 的挂载列表与 #427 的分支结束事件各自为下一版本作出的决定见下文。

### S5a v15 格式决定

[路线图](../roadmap.md) S5a（[Session 文件](session-files.md#8-storage-format)，已实现）的格式决定：升级到 **v15**，不是 v13 内的增量，也不用 14。

- **新增两张表与一个索引**：`session_files(file_id TEXT NOT NULL PRIMARY KEY, node TEXT, writer TEXT)` 与 `session_file_owners(session_id TEXT NOT NULL REFERENCES agent_sessions(session_id), file_id TEXT NOT NULL REFERENCES session_files(file_id), PRIMARY KEY (session_id, file_id))`，索引 `session_file_owners_by_file(file_id)`。`agent_store_format` 的约束改为 `CHECK (version = 15)`。
  - `file_id` 是小写带连字符的 UUID v4，也是目录名。
  - `node` 是写入时 Session 的 `resourceNode`，NULL 表示控制节点。
  - `writer` 在写入未完成时是 SessionStore 代次 ID，提交或放弃后为 NULL。
  - 删除 Session 时，先删其 `session_file_owners` 行，再删 `agent_sessions`。
  - S5c 的分叉与删除只增删 `session_file_owners` 行，清扫只读 `session_files`，不为 Session 文件再改格式。
- **不能留在 v13**：启动要求精确的 v13 SQL 结构，v13 守护进程会拒绝多出表的库。旧守护进程无法打开新库，不满足本节"旧读取器能正确解释新值"的保持条件。C2 和 S8a 留在 v13，是因为它们只在现有表内增加 JSON 字段或行。
- **不用 14**：分支专用的 v14（`user_configuration`）曾经提交（e41ceaf），后被移除（fb3748e）；带 14 标记的开发库可能存在，本文承诺永不解释它们。复用 14 就是给不兼容的已提交格式复用版本号。
- **读取或转换路径**：在现有的启动准入事务（`IMMEDIATE`，写入型 PRAGMA 之前）内处理：
  - 空库直接建 v15。
  - 结构恰为 v13 且标记为 13 的库，在同一事务内重建 `agent_store_format`、写入 15，并建新表和索引。不读取、不改写任何已有行；失败则回滚，原 v13 库保持不变。
  - 恰为 v15 的库直接打开。
  - 其他库（更旧的格式、分支专用 v14、未知结构）照旧拒绝并保留。
- **单向**：v13 守护进程以现有的"unsupported SQLite schema … preserved"错误拒绝升级后的库。这是增量改动，代价可以接受；另一种做法是拒绝所有 v13 库，会丢失用户的 Session。之后的格式各自追加从前一版本出发的一步。
- **升级日志**：升级事务提交后，打开存储的调用方得知已升级，守护进程向 stderr 写一行 `Upgraded native store <path> from format v13 to v15; earlier dimcode versions cannot open it.`。只有执行升级的那次启动写这一行。它直接写 stderr，不走 `tracing`，因为守护进程默认的日志过滤只保留错误。CLI 启动的守护进程的 stderr 就是数据库目录下私有的 `daemon.log`，回退旧版本的用户能在旧守护进程的拒绝信息旁找到原因。
- **验证**：测试用 v13 结构的冻结副本和下面的完整记录示例构建一次性 v13 库，升级后核对结构恰为 v15、业务行逐行不变，从不打开用户已有的库（`storage_flow.rs` 的 `a_v13_store_upgrades_in_place_once`）。

### S5c 格式决定

[路线图](../roadmap.md) S5c（[Session 删除、清理与分叉](session-deletion-and-fork.md#8-storage-format)，已实现）的格式决定：**S5c 本身不改格式**。#222 在 v16 下让分叉在复制的行之后再写一行 `session/forked`（数据为空），没有升版本号；[v17](#193-v17-格式决定) 原样保留这一行。

- **删除**：在一个事务内按 `session_id` 删除该 Session 在各表中的全部行（顺序为 `session_file_owners`、`timeline_snapshots`、`timeline_events`、`timeline_branches`、`timeline_content`、`session_owners`、`agent_sessions`），与创建回滚是同一操作。不写墓碑行：命令输出目录在提交之前移除，Session 文件由 S5a 的行和清扫处理。
- **分叉**：在一个事务内为新 Session 插入现有表的行。`agent_sessions.config` 按存储的文本原样复制；`main` 截至分叉点的事件行、被复制事件引用的内容行、锚定在被复制事件上的快照，以及 S5a 的 `session_file_owners` 行，都按原字节复制。截至分叉点由 `main` 创建的分支只复制目录行（保留存储的 `info`，头部为 0，不含事件），其下属分支不复制；没有事件的子分支目录是存储已经接受的状态，即子分支尚未开始的创建。持久化 JSON 类型不变；随后在 `at + 1` 写入一行 `session/forked`，`main` 的头部为 `at + 1`（在 `0` 处分叉不写，头部为 `0`）。
- **事件 ID**：事件 ID 在所属 Session 内唯一；分叉在另一个 Session 中重复源 Session 的 ID。现有校验和 `timeline_events_by_id` 索引本就按 Session 划分，这是对不变量的说明，不是格式变化。
- **不记来源**：不加记录分叉来源的列，`session/forked` 不带数据；目前没有任何决定使用它，[Session 访问 A.3](../contracts/session-access.md#a3-sessionconfig-fields) 规定 Session 来源在有使用方时连同其格式决定一起加入。
- **旧版本（开发阶段兼容范围，随 #222 决定）**：`EventKind` 是封闭列表，在 `session/forked` 出现之前构建的守护进程无法解码这一行，因而无法恢复新版守护进程分叉出的 Session；首次发布前原生格式不承诺兼容，所以不升版本号、不加回退解码或迁移。没有这一行的 Session（从未分叉、或在 `0` 处分叉）照常读取；已删除的 Session 当作从未存在。v17 起，在这一行出现之前构建的守护进程都以现有错误拒绝 v17 库，能打开 v17 库的守护进程都能解码它，上述差异只留在 v16 库中。

### S5b v16 格式决定

[路线图](../roadmap.md) S5b（[标题元数据](session-title-metadata.md#11-storage-format)）的格式决定：升级到 **v16**，从 v15 原地升级。

- **新增一张表**：`session_metadata(session_id TEXT NOT NULL PRIMARY KEY REFERENCES agent_sessions(session_id), revision INTEGER NOT NULL CHECK (revision > 0), title TEXT, title_source TEXT CHECK (title_source IN ('auto', 'manual')), CHECK ((title IS NULL) = (title_source IS NULL)))`。`agent_store_format` 的约束改为 `CHECK (version = 16)`。
  - 每个 Session 至多一行，没有行表示修订号 0、没有标题。
  - `revision` 是该 Session 已提交 `setTitle` 的次数；清除标题时保留该行，`title` 与 `title_source` 置为 NULL，修订号不会回退。
  - 删除 Session 时，在 S5c 的同一删除事务里、在 `agent_sessions` 之前删除这一行；分叉不复制它。
- **不能留在 v15**：启动要求精确的 SQL 结构，v15 守护进程会拒绝多出表的库，不满足"旧读取器能正确解释新值"。标题不能放进已有的行：`agent_sessions.config` 是分叉整体复制的不可变创建配置，`settings_documents` 属于 Settings；给 `agent_sessions` 加列同样改变存储的 SQL。
- **读取或转换路径**：在现有的启动准入事务内处理：
  - 空库直接建 v16。
  - 结构恰为 v15 且标记为 15 的库，在同一事务内重建 `agent_store_format`、写入 16，并建 `session_metadata`。不读取、不改写任何已有行；失败则回滚，原 v15 库保持不变。
  - 结构恰为 v13 且标记为 13 的库，在同一事务内先走 S5a 的一步，再走本步，直接成为 v16。
  - 恰为 v16 的库直接打开。
  - 其他库（标记不符的 v15 结构、升级到一半的库、更旧的格式、分支专用 v14、未知结构）照旧拒绝并保留。
- **单向**：不做备份，与 S5a 相同（开发阶段的决定）。v15 守护进程以现有的"unsupported SQLite schema … preserved"错误拒绝升级后的库。升级日志沿用 S5a 的那一行，写明原版本：`Upgraded native store <path> from format v15 to v16; earlier dimcode versions cannot open it.`
- **验证**：测试用 v15 结构的冻结副本和完整记录示例构建一次性 v15 库，升级后核对结构恰为 v16、业务行逐行不变、新表为空；v13 库一次升级到 v16；标记不符或升级到一半的 v15、v16 结构被拒绝并保留；从不打开用户已有的库（`storage_flow.rs` 的 `a_v15_store_upgrades_in_place_once`、`a_v13_store_upgrades_in_place_once` 与 `unsupported_legacy_or_changed_schema_is_rejected_without_migration`）。

### #193 v17 格式决定

[请求增量存储](request-delta-storage.md#11-format-v17)（#193）的格式决定：升级到 **v17**，从 v16 原地升级。

- **只改持久化 JSON**：消息以列表节点存入 `timeline_content`（新种类 `node`，正文 `{parent, message}`，地址照旧为 `SHA-256(kind NUL body)`），消息前缀相同的请求共用节点。`context/prepare` 与 `model/call` 记录及其请求草稿以 `ListReferences {list, length, tools, system}` 引用最后一个节点、列表长度与 v16 的 `tools`、`system` 内容，不再为每条消息保存一个引用；其他记录的字节不变（见上文[请求引用与 before 轨迹](#请求引用与-before-轨迹)）。请求只由自己的行与其到达的内容行还原，不经过其他事件行，所以检查点读取仍不读取恢复位置之前的请求行；遍历列表每加载至多 256 个节点释放一次连接。
- **不能留在 v16**：v16 的 `References` 要求 `messages` 并拒绝 `list`、`length`，v16 读取器也不认识 `node` 内容，不满足"旧读取器能正确解释新值"。
- **SQL 不变**：表、列与索引不变（`timeline_content.kind` 本无 SQL 枚举 CHECK），`agent_store_format` 的约束改为 `CHECK (version = 17)`。
- **读取或转换路径**：在现有的启动准入事务内处理：
  - 空库直接建 v17。
  - 结构恰为 v16 且标记为 16 的库，在同一事务内只重建 `agent_store_format` 并写入 17，不读取、不改写任何业务行；失败则回滚，原 v16 库保持不变。
  - 结构恰为 v15 或 v13 且标记相符的库，在同一事务内先走已有各步，再走本步，直接成为 v17。
  - 恰为 v17 的库直接打开。
  - 其他库（标记不符的 v16 结构、升级到一半的库、更旧的格式、分支专用 v14、未知结构）照旧拒绝并保留。
- **混存**：v16 行保持原样且永久可读，不做回填。升级后一个分支写下的第一条请求行，其上一个请求是没有节点的 v16 行，于是为每条消息写一个节点：每个分支一次完整列表，之后的请求共用它。两种行可混存于同一分支与同一库，分叉照原样复制。
- **分叉**：除被复制事件直接引用的内容行外，还复制每个被引用列表从最后一个节点到首个节点途经的全部节点，在分叉事务内用一条对 `(session_id, hash)` 的递归查询完成；切点之后的请求新增、且没有更早请求共用的节点不复制。分叉随后写下的 `session/forked` 不带请求、不引用内容，闭包不因它复制任何行。
- **单向**：不做备份，与 S5a、S5b 相同。v16 守护进程以现有的"unsupported SQLite schema … preserved"错误拒绝升级后的库：其启动门禁比较精确的 SQL 结构，`agent_store_format` 的 `CHECK (version = 17)` 已不同。升级日志沿用 S5a 的那一行：`Upgraded native store <path> from format v16 to v17; earlier dimcode versions cannot open it.`
- **验证**：测试用 v16 结构的冻结副本和完整记录示例构建一次性 v16 库，升级后核对结构恰为 v17、业务行逐行不变，再续写一条请求行，核对它写出完整列表、旧行仍可读；v15、v13 库一次升级到 v17；标记不符或升级到一半的 v16、v17 结构被拒绝并保留；从不打开用户已有的库（`storage_flow.rs` 的 `a_v16_store_upgrades_in_place_once`、`a_v15_store_upgrades_in_place_once`、`a_v13_store_upgrades_in_place_once` 与 `unsupported_legacy_or_changed_schema_is_rejected_without_migration`）。

### #2 v18 格式决定

[扩展事件](../../adr/001-harness-foundation.md#events-and-ownership-issue-2)（#2）的格式决定：升级到 **v18**，从 v17 原地升级。

- **只增一种行**：核心未定义领域的事件以 `EventKind::Extension` 写入普通 `event` 行，`kind` 为 `<type>/<name>`，`data` 原样保存（见上文[事件封装](#事件封装)）。它不进输出行、不带请求引用；行结构、内容寻址和其他记录的字节不变。
- **不能留在 v17**：v17 读取器的 `EventKind` 是封闭列表，解码不了扩展事件行，不满足"旧读取器能正确解释新值"。#193 的 v17 起，能打开某版本库的守护进程都能解码其中每一行（见上文 [S5c 决定](#s5c-格式决定)）；留在 v17 就打破这一点，为不兼容的已提交格式沿用了版本号。此后的读取器按领域解码：核心保留领域只按其类型解码，其他领域都读作扩展事件，所以新的扩展领域不再改变格式；新增核心事件仍需各自的格式决定。
- **SQL 不变**：表、列与索引不变，`agent_store_format` 的约束改为 `CHECK (version = 18)`。
- **读取或转换路径**：在现有的启动准入事务内处理：
  - 空库直接建 v18。
  - 结构恰为 v17 且标记为 17 的库，在同一事务内只重建 `agent_store_format` 并写入 18，不读取、不改写任何业务行；失败则回滚，原 v17 库保持不变。
  - 结构恰为 v16、v15 或 v13 且标记相符的库，在同一事务内先走已有各步，再走本步，直接成为 v18。
  - 恰为 v18 的库直接打开。
  - 其他库（标记不符的 v17 结构、升级到一半的库、更旧的格式、分支专用 v14、未知结构）照旧拒绝并保留。
- **旧版本的失败方式**：v17 守护进程以现有的"unsupported SQLite schema … preserved"错误在启动时拒绝整个 v18 库：其启动门禁比较精确的 SQL 结构，`agent_store_format` 的 `CHECK (version = 18)` 已不同。留在 v17 时，它会打开该库，到恢复含扩展事件的 Session 时才按[格式不可读](../../architecture/rust-agent/timeline-storage.md#6-故障)拒绝该 Session 并保留数据；v18 让这一失败在启动时出现，并由版本号说明原因。
- **单向**：不做备份，与 S5a、S5b、#193 相同。升级日志沿用 S5a 的那一行：`Upgraded native store <path> from format v17 to v18; earlier dimcode versions cannot open it.`
- **验证**：测试用 v17 结构和完整记录示例构建一次性 v17 库，升级后核对结构恰为 v18、标记为 18、业务行逐行不变；v16、v15、v13 库一次升级到 v18；标记不符的 v17、v18 结构被拒绝并保留；扩展事件各占一行、保存 `data` 的原字节、两侧的输出增量不跨它合并，经 Session 恢复原样读出；从不打开用户已有的库（`storage_flow.rs` 的 `a_v17_store_upgrades_in_place_once`、`a_v16_store_upgrades_in_place_once`、`a_v15_store_upgrades_in_place_once`、`a_v13_store_upgrades_in_place_once`、`unsupported_legacy_or_changed_schema_is_rejected_without_migration` 与 `extension_events_keep_their_own_rows_and_restore_unchanged`）。

### #364 v19 格式决定

[挂载列表](../contracts/session-access.md#a3-sessionconfig-fields)（#364）的格式决定：升级到 **v19**，从 v18 原地升级。

- **只改一个字段的写法**：`agent_sessions.config`（`SessionConfig`）与 `agents.profile`（`AgentProfile`）的 `tools` 成为可选。有挂载列表时照旧写成字符串数组；没有挂载列表时省略该字段，表示挂载进程提供的全部工具。已存储的记录都带 `tools`，读作有挂载列表，含义不变；例外是默认 Agent：它由进程按默认档案建立，不能由客户端编辑，存储的档案只是建立它的那个版本的进程默认值，因此启动时可读的默认 Agent 一律采用当前进程默认档案并改写该行（v19 内的同格式写入，不升级格式），否则 #364 之前建立的库会一直冻结在旧的挂载列表上，之后新增的工具（如 `ask_user`）永远不会挂载；读不出或不合法的默认 Agent 仍保持不可用，不被替换。表、列、索引、内容寻址和其他记录的字节不变。
- **不能留在 v18**：v18 读取器要求 `tools` 存在，读不了省略它的记录，不满足"旧读取器能正确解释新值"；也不能改写成 `null` 或空数组，那会把"全部挂载"读成"不挂载"。
- **SQL 不变**：`agent_store_format` 的约束改为 `CHECK (version = 19)`。
- **读取或转换路径**：在现有的启动准入事务内处理：
  - 空库直接建 v19。
  - 结构恰为 v18 且标记为 18 的库，在同一事务内只重建 `agent_store_format` 并写入 19，不读取、不改写任何业务行；失败则回滚，原 v18 库保持不变。
  - 结构恰为 v17、v16、v15 或 v13 且标记相符的库，在同一事务内先走已有各步，再走本步，直接成为 v19。
  - 恰为 v19 的库直接打开。
  - 其他库照旧拒绝并保留。
- **旧版本的失败方式**：v18 守护进程以现有的"unsupported SQLite schema … preserved"错误在启动时拒绝整个 v19 库，不会到恢复某个 Session 或读取某个 Agent 时才失败。
- **单向**：不做备份，与之前各步相同。升级日志沿用同一行：`Upgraded native store <path> from format v18 to v19; earlier dimcode versions cannot open it.`
- **验证**：测试用 v18 结构和完整记录示例构建一次性 v18 库，升级后核对结构恰为 v19、标记为 19、业务行逐行不变；v17、v16、v15、v13 库一次升级到 v19；标记不符的 v18、v19 结构被拒绝并保留；从不打开用户已有的库（`storage_flow.rs` 的 `a_v18_store_upgrades_in_place_once` 及其他各步的同名测试与 `unsupported_legacy_or_changed_schema_is_rejected_without_migration`）。

### #32 格式决定

[#32](https://github.com/arcships/dim/issues/32) 的格式决定：**不改格式**，v18 不变。`submission_id` 留在 `input/message` 的 `data` 中，不另建提交记录表（[sessions 服务 §6.3](sessions-service.md#where-the-submission-identity-lives)）。

- **已有库**：已写入的行原样有效，不迁移，不改写。
- **冻结样本**：`crates/dim-agent-types/tests/fixtures/` 不变。v1 wire 的 `input/message` 只有 `{text,mode?}`，没有该字段的视图。
- **以后加索引时**：若查找延迟需要索引，按 `timeline_tool_use_starts` 的方式从事件正文派生表达式索引，字段仍留在事件中。SQL 结构随之改变，届时单独做格式决定。

### #329 格式决定

[Provider 完整管理 #329](https://github.com/arcships/dim/issues/329) 的格式决定沿用当时当前的 **v19，不额外改变格式号或 SQL 结构**（#364 已决定 v18 → v19；其后 #427 升到 v20，本决定随之保持）；具体合同见[Provider 管理 §6](provider-management.md#6-persistence-existing-sessions-and-migration)。
这是对上文保持版本条件中“旧写入器不会重写丢失新值”的明确例外，按首次发布前的开发阶段兼容范围接受。

- 只增加三个可选顶层 JSON 字段：静态记录的 `userModels: ModelEntry[]`、静态/托管记录的 `modelPreferences: Record<modelId, ModelPreferences>`，以及静态记录的 `endpointOverrides: Record<modelId, NewApiEndpoint>`。缺失表示无对应用户数据；默认模型与用户新增列表分开。
- 旧 v19 二进制忽略这些未知顶层字段，重写时会丢失；明确接受该降级损失，包括 fingerprint 改变时自动 `seed_bundled` 重写。不得只验证读取成功；实施须在一次性数据库中运行旧 v19 的实际读取和重写路径。
- 不扩展既有严格嵌套类型。`ModelPreferences` 拒绝未知字段，`NewApiEndpoint` 是闭合枚举；已支持新映射的旧二进制会拒绝后续新增的内部字段/枚举值，增加前须另作显式格式决定。
- 新版本 reseed 若因用户模型与新默认项重名，保留该记录已准入定义、fingerprint 和 version，继续处理其他记录，管理面沿既有逐项错误提示。用户删除冲突模型后，下次启动采用新定义；不以 Provider 激活失败阻断修复入口。其他非法记录和未知数据库格式沿原规则拒绝并保留。
- 验证在一次性 v19 库中运行 Provider 管理实现之前的实际 App Server 二进制：只读不改变自定义记录字节；旧 `provider.save` 重写丢失三个新增字段，旧启动 reseed 丢失内置记录的用户模型和偏好，库标记不因本决定改变。新增字段的缺失值、重启保留、用户模型冲突、删除和重新装载由 Provider 流程测试保护。

### #427 v20 格式决定

[Timeline 分支](../timeline-branches.md#16-format)（#427）的格式决定：升级到 **v20**，从 v19 原地升级。

- **状态**：当前格式是 v20。
- **只增两种核心事件**：两者都以 `encoding: "event"` 写入普通 `event` 行，不进输出行、不带请求引用。行结构、内容寻址和其他记录的字节不变。
  - `branch/end` 写在要结束的分支上，`data` 为 `{source, ancestor?}`。`source` 是 `branch/create` 已在用的 `BranchSource`：`{kind: "tool", turn_id, request_id, call_id}` 或 `{kind: "host", operation_id}`。`ancestor` 是分支 ID，只在因祖先结束而结束时出现。
  - `branch/ended` 写在其父分支上，`data` 为 `{branch, endEvent, source, ancestor?}`。`endEvent` 是 `branch/end` 在结束分支上的序号（安全整数），`source` 与 `ancestor` 取自 `branch/end`（[Timeline 分支 §10](../timeline-branches.md#10-ending-a-line)）。
- **目录行不变**：`timeline_branches.info` 照旧是不含 `source` 的 JSON，类型是 `BranchRecord`（运行时视图 `TimelineInfo` 是它再加上 `source`）。运行时的 `TimelineInfo` 新增 `source`，但存储写目录行时不写它；恢复时由 TimelineTree 从父分支的 `branch/create` 补齐（[Timeline 分支 §5](../timeline-branches.md#5-parts)）。
- **不能留在 v19**：
  - v19 读取器对核心保留领域只按类型解码（见上文 [#2 v18 决定](#2-v18-格式决定)），解码不了 `branch/end` 与 `branch/ended` 行。
  - #193 的 v17 起，能打开某版本库的守护进程都能解码其中每一行；留在 v19 就打破这一点，为不兼容的已提交格式沿用了版本号。
  - #2 的 v18 决定写明，新增核心事件需要各自的格式决定。
- **SQL 不变**：表、列与索引不变，`agent_store_format` 的约束改为 `CHECK (version = 20)`。
- **读取或转换路径**：在现有的启动准入事务内处理：
  - 空库直接建 v20。
  - 结构恰为 v19 且标记为 19 的库，在同一事务内只重建 `agent_store_format` 并写入 20，不读取、不改写任何业务行；失败则回滚，原 v19 库保持不变。
  - 结构恰为 v18、v17、v16、v15 或 v13 且标记相符的库，在同一事务内先走已有各步，再走本步，直接成为 v20。
  - 恰为 v20 的库直接打开。
  - 其他库（标记不符的 v19 结构、升级到一半的库、更旧的格式、分支专用 v14、未知结构）照旧拒绝并保留。
- **旧版本的失败方式**：v19 守护进程以现有的"unsupported SQLite schema … preserved"错误在启动时拒绝整个 v20 库，因为其启动门禁比较精确的 SQL 结构，而 `agent_store_format` 的 `CHECK (version = 20)` 已不同。它不会等到恢复含这两种事件的 Session 时才失败。
- **单向**：不做备份，与之前各步相同。升级日志沿用 S5a 的那一行：`Upgraded native store <path> from format v19 to v20; earlier dimcode versions cannot open it.`
- **验证**：
  - 用 v19 结构和完整记录示例构建一次性 v19 库，升级后核对结构恰为 v20、标记为 20、业务行逐行不变。
  - v18、v17、v16、v15、v13 库一次升级到 v20。
  - 标记不符的 v19、v20 结构和升级到一半的库被拒绝并保留。
  - `branch/end` 与 `branch/ended` 各占一行，经 Session 恢复原样读出；已结束的分支恢复后不再打开。
  - 新子分支的目录行不含 `source`，与 v19 的写法逐字节相同。
  - 从不打开用户已有的库。

  对应 `storage_flow.rs` 的 `a_v19_store_upgrades_in_place_once` 及其他各步的同名测试、`unsupported_legacy_or_changed_schema_is_rejected_without_migration` 与 `branch_end_events_restore_unchanged_and_the_line_stays_ended`。


## 索引与实际查询

| 版本与索引 | 精确列序/条件 | 服务的查询 |
| --- | --- | --- |
| v1 主键 `agents`、`agent_sessions` | 各自 ID | 单 Agent/Session 查找；Agent 全目录目前仍按 `is_default DESC, agent_id` 扫描排序 |
| v1 主键 `session_owners` | `(session_id)` | Session 到 Agent/协议版本；v1 全 owner 目录按 `session_id` 扫描 |
| v1 主键 `timeline_branches` | `(session_id, branch_id)` | 分支 head、整 Session 分支目录 |
| v1 主键 `timeline_events` | `(session_id, branch_id, sequence)` | 追加前读取最后一行、单分支顺序恢复、从起点读取历史 |
| v1 主键 `timeline_content` | `(session_id, hash)` | 请求引用的定点读取和 Session 内去重；v17 列表节点逐个加载与分叉复制的节点闭包 |
| v1 唯一 `agents_default_unique` | `(is_default) WHERE is_default = 1` | 至多一个默认 Agent；不保证其存在 |
| v12 主键 `timeline_snapshots` | `(session_id, branch_id, interpreter, sequence)` | 兼容 interpreter，`sequence<=through ORDER BY sequence DESC LIMIT 1`；连接事件锚点 |
| v15 主键 `session_files` | `(file_id)` | put 的两次事务与放弃、清扫按文件定点更新和删除 |
| v15 主键 `session_file_owners` | `(session_id, file_id)` | put 写入 owner；fork 复制、删除和创建回滚按 Session 读取和删除 |
| v15 `session_file_owners_by_file` | `(file_id)` | 清扫判断文件是否已无 owner |
| v16 主键 `session_metadata` | `(session_id)` | `read`、`setTitle`、列表联结标题、删除与创建回滚按 Session 删除 |

## 分页查询

以下索引已与表结构一起创建，供目录过滤、事件分页和跨页 ToolCall 读取使用。事件 ID 索引非唯一，恢复时逐事件查重并核对索引值。

| 索引 | 精确列序/条件 | 查询 |
| --- | --- | --- |
| `session_owners_by_agent` | `(agent_id, protocol_version, session_id)` | 按 Agent 和协议过滤目录 |
| `timeline_events_by_end` | `(session_id, branch_id, last_sequence, sequence)` | 正向页 `last_sequence >= after+1`；反向页同时处理跨界行 |
| `timeline_events_by_id` | `(session_id, event_id) WHERE event_id IS NOT NULL`，非唯一 | 按事件 ID 查找跨页 ToolCall |
| `timeline_tool_use_starts` | `(session_id, json_extract(body, '$.record.event.data.request_id'), json_extract(body, '$.record.event.data.call_id')) WHERE branch_id = 'main' AND kind = 'step/tool-use-start'` | 按请求与调用 ID 查找跨页 `step/tool-use-start`，为不带 `call_event_id` 的结果取调用名；查询须使用相同表达式与条件 |

正向页的 `after` 与 `through` 须为批次边界；`limit` 为目标事件数，末批超出时完整返回。页查询以 `last_sequence` 为 seek/排序列。合并行覆盖页起点时完整解码，再按内部 sequence 过滤；反向页须纳入 `sequence < before <= last_sequence` 的跨界行，并停在原子批次边界。正向读取按 256 个逻辑序号分段，恢复校验按 64 条物理行分段，每段结束释放连接锁；请求列表的遍历每加载至多 256 个节点也释放一次，下一次持有从中断处继续。单条物理行完整解码，允许超过通常页大小。流程测试用 `EXPLAIN QUERY PLAN` 核对正向读取索引；`timeline_acceptance paged-reads` 测量长历史读/恢复与并发提交延迟；`restore_cost` 在一次性库上经 mock 模型轮次增长 Session，按历史规模测量恢复耗时，并拆分目录校验、事件读取与 `branch/resume` 写入，同时报告每条事件的存储字节数（`cargo run --release --locked -p dim-app-server --example restore_cost -- [plain|tools|product] [1001,3001,10001] [7]`；`product` 使用产品上下文窗口与摘要压缩）；`single-append` 与 `single-partial` 测量单个请求的冷读与重开后第一次写入（`-- [single-append|single-partial] [2000,20000] [7] [chain|last]`）。`timeline_acceptance crashes` 与 `powerloss` 的中断批次含一个请求，恢复后校验行可读、序号连续、每个列表的节点俱在，并核对重启后写入与未中断写入的行逐字节一致。反向历史接口仍为独立协议草案。

当前两个 ID 列的 SQL 可空缺口见表结构。永久关闭字段、协作表和旧记录回填在业务范围确定后另行设计。

## 完整记录示例（v20）

下例是一条 Session `s1`、主分支 `main` 和子分支 `child` 的全部原生表记录，用于核对存储关系，不代表完整的对话执行序列。为便于阅读，`profile`、`config`、`info` 和事件 `body` 这几个实际为 `TEXT` 的列在展示时解析为 JSON；写入时各自是完整 JSON 字符串，类型规定省略的可选字段照常省略。ID/时间是示例值。`timeline_content.body` 保留实际写入的字符串形式，因此下面各 hash 可以按上文算法复核。`main` 第 4 条 `model/call` 是升级前 v16 写入的行，`child` 第 3 条是升级后 v17 写入的行，示例同时展示两种行混存。

The example retains an old `dim/context-projection/v1` snapshot: the default v3
interpreter bypasses that unchecked cache and restores from facts. Existing v13
databases needed no migration for this snapshot interpreter change. `s1` owns
one Session file, stored at `<db>.session-files/<file_id>/<name>`; its name and
bytes live only on disk. Its title was written twice; the second write is the
current manual title.

```json
{
  "agent_store_format": [{"version": 20}],
  "settings_documents": [],
  "providers": [],
  "provider_selections": [],
  "provider_imports": [],
  "agents": [{"agent_id": "a1", "is_default": 1, "profile": {
    "name": "Example", "description": "Example agent", "systemPrompt": "You are helpful.",
    "tools": [], "model": {"providerId": "mock", "modelId": "deterministic"}
  }}],
  "agent_sessions": [{"session_id": "s1", "created_at_ms": 1000, "config": {
    "permissionPolicy": {"preset": "read-only", "allowedTools": [], "disallowedTools": []},
    "cwd": "/work", "model": {"providerId": "mock", "modelId": "deterministic"},
    "systemPrompt": "You are helpful.", "tools": [], "collaborationRole": "leader"
  }}],
  "session_owners": [{"session_id": "s1", "agent_id": "a1", "protocol_version": 3}],
  "timeline_branches": [
    {"session_id": "s1", "branch_id": "main", "info": {"id": "main"}, "sequence": 8},
    {"session_id": "s1", "branch_id": "child", "info": {
      "id": "child", "parent": "main", "branch_point": 7, "create_anchor": 8
    }, "sequence": 3}
  ],
  "timeline_content": [
    {"session_id": "s1", "hash": "59cdf8dcbefa00e8fa27452182332492a68dccb11804c2bd571c53f1750c4e69",
     "kind": "message", "body": "{\"role\":\"user\",\"content\":\"Hi\"}"},
    {"session_id": "s1", "hash": "eed2128b01236e0436f9b7ae565767c078e5e6f1d13dcacd51fa8399564c6288",
     "kind": "system", "body": "\"You are helpful.\""},
    {"session_id": "s1", "hash": "a081b4c298123f73baa9bc6dfd71782ef548fd8298a097b00eeea206f8d28ffe",
     "kind": "node", "body": "{\"parent\":null,\"message\":{\"role\":\"user\",\"content\":\"Hi\"}}"},
    {"session_id": "s1", "hash": "c5ef63e65386b0f98fd778a499f4c73964afe57473de5f71c8f727a9b1a166ea",
     "kind": "node", "body": "{\"parent\":\"a081b4c298123f73baa9bc6dfd71782ef548fd8298a097b00eeea206f8d28ffe\",\"message\":{\"role\":\"assistant\",\"content\":\"Hello\"}}"},
    {"session_id": "s1", "hash": "03e36a3f41a953b3d046e6ed7eb0b2c7da95c26a166402c5bce7970f55ec447d",
     "kind": "node", "body": "{\"parent\":\"c5ef63e65386b0f98fd778a499f4c73964afe57473de5f71c8f727a9b1a166ea\",\"message\":{\"role\":\"user\",\"content\":\"Follow up\"}}"}
  ],
  "timeline_events": [
    {"session_id": "s1", "branch_id": "main", "sequence": 1, "last_sequence": 1,
     "event_id": "e1",
     "kind": "input/message", "body": {"encoding": "event", "record": {
       "event": {"id": "e1", "sequence": 1, "create_at": 1001, "type": "input", "name": "message", "data": {"user": {"content": [{"type": "text", "text": "Hi"}]}}},
       "refs": null, "trace": null, "batch_end": 1
     }}},
    {"session_id": "s1", "branch_id": "main", "sequence": 2, "last_sequence": 2,
     "event_id": "e2",
     "kind": "input/consumed", "body": {"encoding": "event", "record": {
       "event": {"id": "e2", "sequence": 2, "create_at": 1002, "type": "input", "name": "consumed",
                 "data": {"input_id": "e1"}},
       "refs": null, "trace": null, "batch_end": 2
     }}},
    {"session_id": "s1", "branch_id": "main", "sequence": 3, "last_sequence": 3,
     "event_id": "e3",
     "kind": "context/compaction", "body": {"encoding": "event", "record": {
       "event": {"id": "e3", "sequence": 3, "create_at": 1003, "type": "context", "name": "compaction", "data": {
         "turn_id": "t1", "request_id": "r1", "through": 2, "status": "completed",
         "retain_from": "e2", "replacement": [{"role": "user", "content": "Earlier: greeting"}],
         "strategy": "summary",
         "diagnostics": {
           "reason": "auto", "trigger": "threshold", "retainFrom": "e2",
           "estimatedInputTokens": 1.0, "estimatedOutputTokens": 1.0,
           "entriesReplaced": 0, "modelCallCount": 1, "durationMs": 1,
           "strategy": {"kind": "summary", "summaryChars": 17, "estimatedSummaryTokens": 5,
                        "chunkCount": 1, "compactedToolPayloadCount": 0, "droppedMessageCount": 0}
         }
       }},
       "refs": null, "trace": null, "batch_end": 3
     }}},
    {"session_id": "s1", "branch_id": "main", "sequence": 4, "last_sequence": 4,
     "event_id": "e4",
     "kind": "model/call", "body": {"encoding": "event", "record": {
       "event": {"id": "e4", "sequence": 4, "create_at": 1004, "type": "model", "name": "call", "data": {
         "turn_id": "t1", "request_id": "r1", "request": {
           "providerId": "mock", "modelId": "deterministic", "messages": []
         }
       }},
       "refs": {"messages": ["59cdf8dcbefa00e8fa27452182332492a68dccb11804c2bd571c53f1750c4e69"],
                "tools": null, "system": "eed2128b01236e0436f9b7ae565767c078e5e6f1d13dcacd51fa8399564c6288"},
       "trace": null, "batch_end": 4
     }}},
    {"session_id": "s1", "branch_id": "main", "sequence": 5, "last_sequence": 6,
     "kind": "step/text", "body": {
       "encoding": "output", "turn_id": "t1", "request_id": "r1", "started_at": 1005,
       "records": [
         {"event": {"id": "e5", "sequence": 5, "create_at": 1005,
           "type": "step", "name": "text", "data": {"delta": "He"}},
          "refs": null, "trace": null, "batch_end": 6},
         {"event": {"id": "e6", "sequence": 6, "create_at": 1006,
           "type": "step", "name": "text", "data": {"delta": "llo"}},
          "refs": null, "trace": null, "batch_end": 6}
       ]
     }},
    {"session_id": "s1", "branch_id": "main", "sequence": 7, "last_sequence": 7,
     "event_id": "e7",
     "kind": "step/model-output-end", "body": {"encoding": "event", "record": {
       "event": {"id": "e7", "sequence": 7, "create_at": 1007, "type": "step", "name": "model-output-end",
                 "data": {"turn_id": "t1", "request_id": "r1", "status": "completed", "finish_reason": "end_turn"}},
       "refs": null, "trace": null, "batch_end": 7
     }}},
    {"session_id": "s1", "branch_id": "main", "sequence": 8, "last_sequence": 8,
     "event_id": "e8",
     "kind": "branch/create", "body": {"encoding": "event", "record": {
       "event": {"id": "e8", "sequence": 8, "create_at": 1008, "type": "branch", "name": "create", "data": {
         "at": {"kind": "sequence", "sequence": 7},
         "source": {"kind": "host", "operation_id": "op1"},
         "branch": "child", "prompt": "Follow up"
       }},
       "refs": null, "trace": null, "batch_end": 8
     }}},
    {"session_id": "s1", "branch_id": "child", "sequence": 1, "last_sequence": 1,
     "event_id": "child-e1",
     "kind": "branch/start", "body": {"encoding": "event", "record": {
       "event": {"id": "child-e1", "sequence": 1, "create_at": 1009, "type": "branch", "name": "start", "data": {
         "parent": "main", "createEvent": 8
       }},
       "refs": null,
       "trace": {"event_id": "child-e1", "original": {
         "event": {"type": "branch", "name": "start", "data": {"parent": "main", "createEvent": 8}},
         "refs": null
       }, "steps": [{"handler": "example", "result": null}]},
       "batch_end": 1
     }}},
    {"session_id": "s1", "branch_id": "child", "sequence": 2, "last_sequence": 2,
     "event_id": "child-e2",
     "kind": "input/message", "body": {"encoding": "event", "record": {
       "event": {"id": "child-e2", "sequence": 2, "create_at": 1010, "type": "input", "name": "message",
                 "data": {"user": {"content": [{"type": "text", "text": "Follow up"}]}}},
       "refs": null, "trace": null, "batch_end": 2
     }}},
    {"session_id": "s1", "branch_id": "child", "sequence": 3, "last_sequence": 3,
     "event_id": "child-e3",
     "kind": "model/call", "body": {"encoding": "event", "record": {
       "event": {"id": "child-e3", "sequence": 3, "create_at": 1011, "type": "model", "name": "call", "data": {
         "turn_id": "t2", "request_id": "r2", "request": {
           "providerId": "mock", "modelId": "deterministic", "messages": []
         }
       }},
       "refs": {"list": "03e36a3f41a953b3d046e6ed7eb0b2c7da95c26a166402c5bce7970f55ec447d", "length": 3,
                "tools": null, "system": "eed2128b01236e0436f9b7ae565767c078e5e6f1d13dcacd51fa8399564c6288"},
       "trace": null, "batch_end": 3
     }}}
  ],
  "timeline_snapshots": [{"session_id": "s1", "branch_id": "main", "sequence": 3,
    "interpreter": "dim/context-projection/v1", "event_id": "e3",
    "state": "{\"resume\":{\"branch_id\":\"main\",\"sequence\":2},\"state\":{\"retain_from\":\"e2\",\"prompts\":{\"entries\":[],\"revision\":null,\"system_revision\":null,\"system\":\"\",\"turns\":[],\"tasks\":[]},\"delivered\":[],\"inputs\":[[\"e1\",{\"user\":{\"content\":[{\"type\":\"text\",\"text\":\"Hi\"}]}}]],\"configuration\":null,\"output\":[],\"open_tools\":[],\"output_open\":false}}"
  }],
  "session_files": [{"file_id": "0b4e7d2c-5a1f-4c3e-9f6a-2d8b1e7c4a90", "node": null, "writer": null}],
  "session_file_owners": [{"session_id": "s1", "file_id": "0b4e7d2c-5a1f-4c3e-9f6a-2d8b1e7c4a90"}],
  "session_metadata": [{"session_id": "s1", "revision": 2, "title": "Example conversation", "title_source": "manual"}]
}
```

这十一条逻辑事件占十条物理事件行；`main` 的已发布 head 为 8，`child` 为 3。`input/consumed` 把 `e1` 转成 ID 为 `e2` 的用户历史条目；压缩的 `retain_from` 指向事件 `e2`，即从该事件放置的条目起原样保留；可选快照从第 2 条事件重放，所保存的前缀状态含尚未采纳的 `e1` 输入。`main` 的输出行合并两个增量，读出后各自恢复 `turn_id`/`request_id`，两者的原子批次终点都是 6。子分支通过 `branch_point=7` 共享 `main` 的前缀 1–7，不复制父事件；`create_anchor=8` 指向父分支的 `branch/create`。`main` 的 v16 `model/call` 的两个 hash 在同一 Session 的 `timeline_content` 命中。`child` 的 v17 `model/call` 以 `list` 引用三条消息的最后一个节点，`length` 为 3；它在逻辑历史中的上一个请求是 `main` 第 4 条 v16 行，没有可共用的节点，所以三条消息各写一个节点，`system` 按 v16 的写法得到同一地址。`timeline_snapshots` 的独立行锚定主分支第 3 条 completed 事实，事件本身没有快照数据；删除该快照不影响事实重放或 replacement。回滚 Session 时先删除快照，再删除事件、分支、内容、owner 和 Session 文件 owner 行，最后删除 `agent_sessions`；Agent 只有在不再被任何 Session 引用时才能删除。
