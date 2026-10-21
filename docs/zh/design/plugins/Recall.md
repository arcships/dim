# Recall

状态：Ready（2026-10-21 批准，见 §15）。Issue：[#129](https://github.com/arcships/dim/issues/129)。里程碑：M2。语言：Rust。作用域：[ADR 002](../../adr/002-plugin-scope.md)（Session）。依赖 [ContextCompaction](ContextCompaction.md)（F1，已实现）与 `toolContribution` 挂接规则（C3，已实现）。

Recall 是一个 Session 插件，给模型提供一个只读工具 `recall`。它把模型所在分支的历史讲给模型听，讲的是经历：用户说了什么，模型说了什么、做了什么、得到了什么，以及期间还发生了什么。它经 Branch 门面读取已发布的 Timeline 事件，并把有界的文本作为工具结果返回。它不写事件、不持有状态，也不改动任何其他模块或类型。

## 1. 问题与范围

压缩把早期对话替换为摘要（[ContextCompaction §4.3](ContextCompaction.md#43-事件)）。摘要只保留转述；用户的原话和示例、之前抓取的内容，都从模型的视野中消失了。Timeline 仍保存着每一个事件。

Recall 不是"切点之后"的视图，而是对整个分支历史的回忆，就像人回忆一天的经历：压缩是其中发生过的一件事，不是边界。这段历史包括 fork 之前发生的事：子分支回忆父分支到 fork 点为止的事件（它的继承前缀），fork 出的 Session 的 `main` 回忆从源 Session 复制来的事实，事件 id 不变。

**范围外：** 其他分支和 Session、原始事件数据，以及对压缩、摘要或上下文拼装的任何改动。

## 2. 模型怎么回忆

Recall 就像客户端里的会话视图，只是讲给模型而不是展示给用户。它分三层：

1. **轮次。** 每轮一行：什么时候、用户要什么、模型做了什么、这一轮怎么结束。从最近一轮往前分页。
2. **一轮。** 按顺序讲这一轮：发生的每件事一行，各带时间和 id。
3. **一件事。** 按 id 给出全文。

检索（`about`、`tool`）从所有轮次中返回命中的第 2 层的行，id 相同。`about` 匹配每件事的全文（调用的完整输入和结果 `content`、回复的完整文本），而不是缩短后的那一行，所以抓取页面或子 agent 报告深处的词也能找到。每个结果最后都给出下一步的参数，所以第一次调用不需要任何先验知识。

```
 recall()                         recall(open="e41c07d2")              recall(open="3f9a2c1b")
 +----------------------------+   +-------------------------------+    +-------------------------+
 | Turn 14:21 e41c07d2        |   | 14:03 9b1e4d70 user: Use the  |    | Rate limits: 600        |
 |   user: "Stop. Don't …"    |-->|   staging bucket, never prod… |--->| requests per minute per |
 | Turn 14:03 c2d8a915        |   | 14:04 3f9a2c1b you called     |    | key; bursts of 50 …     |
 |   user: "Use the staging…" |   |   web_fetch → ok, "Rate lim…" |    | End.                    |
 +----------------------------+   +-------------------------------+    +-------------------------+
```

图中把 id 缩短为 8 个字符；结果中显示和接受的都是完整 id（§4）。

## 3. 叙事投影

`project_history` 讲不了这个故事：它是为拼装模型请求服务的，把一次调用拆成一个 `ToolUse` 部分和一条单独的 `ToolResult` 消息，其余什么都不保留。审批、后台执行、子 agent、从未执行的调用都不在其中。所以 Recall 在事件之上有自己的投影。它用与上下文拼装相同的规则判断什么算数（§3.3），再以自己的形式讲出来。

### 3.1 固定的读取

每次调用开始时，工具读取 `head = branch.sequence()`（已发布序号）和 `events = branch.history(Some(head))`：对子分支，是父分支到 fork 点为止的事件（继承前缀）加上它自己的事件；对 fork 出的 Session 的 `main`，是复制来的事实、它的 `session/forked`，再加上它自己的事件。两者都是 fork 之前的历史，Recall 像读其他历史一样读取。其余一切都基于这一份固定的列表计算，所以一次调用不会混用两个 head。这与 `wait` 已有的做法一致（[wait.rs](../../../../crates/dim-agent-capabilities/src/wait.rs)）：一个 Session 工具从 `ToolExecutionContext.branch_id` 经 `TreeCommands` 找到调用方分支。Recall 不应用压缩，遍历的是整个列表。

### 3.2 事：一个起点事件和挂到它上面的事件

投影把事件折叠成**事**。一件事有一个**起点事件**，以及之后通过事件本来就带着的 id 挂到它上面的事件。一件事的那一行显示起点的时间和 id；它的结局是折叠完所有挂上来的事件之后剩下的状态。

| 事 | 起点事件 | 挂上来的事件（关联键） |
| --- | --- | --- |
| 输入 | `input/message`：它的用户内容和它的所有块，按顺序 | `input/consumed`（`input_id`） |
| 你的回复 | `step/model-start` | `step/text` 片段，然后可选的 `step/model-content`（最终内容），然后 `step/model-output-end`（`request_id`） |
| 工具调用 | `step/tool-use-start` | 若已到达，带完整参数的 `step/tool-use`（`call_id`）；来源为 `tool` 的 `approval/request`（`call_id`），然后是 `approval/respond` 或 `approval/cancelled`（`approval_id`）；`step/tool-result` 中对应的那一项（`call_id`）；`exec/started`（`call_id`）与 `exec/ended`（`task_id`）；`collaboration/request`（`call_id`），然后是 `collaboration/message`（`call_id`）与 `collaboration/result`（`request_event_id`）；来源为 `tool` 的 `branch/create`（`call_id`） |
| 中断 | `stop/interrupt` | — |
| 权限变更 | `permission/changed` | — |
| 压缩 | 结果为 `completed` 的 `context/compaction` | — |
| fork | 本分支带 `parent` 的 `branch/start` | — |
| Session fork | `session/forked` | — |

一个输入事件是一件事。它的用户内容和它携带的每个 `InputBlock::Raw` 共用它的事件 id，并一起讲述，所以一个 id 不会指向多个条目；没有块序号，也没有子把手。块是否仍然生效，由之后同一 `key` 的块判断（§3.5）。

后五种从未进入模型上下文，但它们属于发生过的事：用户的决定，以及对话的形状。

起点是每个输出、每次调用的第一个事件，而不是它们的完成事件，因为中断或失败可能让二者在完成前结束。这与 `project_history` 已有的折叠一致（[history.rs](../../../../crates/dim-agent-capabilities/src/history.rs)）：

- 回复收集它的 `step/text` 片段；之后的 `step/model-content` 用最终内容替换它们。`step/model-output-end` 以 completed、failed 或 cancelled 结束它。thinking 丢弃。折叠后文本为空的请求（只有调用或只有 thinking）不成为回复行；其调用各自讲述。
- 调用从 `step/tool-use-start` 取得名字。若 `step/tool-use` 已到达，参数取自它；否则参数从未提交，不显示。因输出失败而被关闭的调用会得到 `model_provider` 写入的、不带 `call_event_id` 的关闭结果，因此结局为 `not run`（§3.4）。

**Recall 只讲它点名的类型。** 它读取上表中的起点事件和挂上来的事件（按表中所列的形态），以及划分轮次的 `turn/start` 与 `turn/end`（§3.6）。其他任何事件按规则都不讲：无论是核心事件还是不透明事件（[ADR 001](../../../adr/001-harness-foundation.md#events-and-ownership-issue-2)），无论已有还是以后新增。事件类型是动态增加的，所以新类型不需要改 Recall，也绝不会出现在结果中；想让模型看到内容的能力会写已采用的输入或工具结果，Recall 讲的正是这些。

### 3.3 什么算数

投影用上下文拼装的规则判断什么算数，所以"模型经历过什么"只有一个定义：

- 输入在被消费后才算；待处理、已撤回或已删除的输入不是一件事；
- 回复在其输出完成时才算，或者作为中断或失败留下的部分文本；
- 工具结果按模型收到的样子讲：它的 `content`，而不是 `structuredContent`。

上下文拼装本来就是白名单：`project_history` 只匹配它认识的类型，其余忽略。一个流程测试把两种投影绑在一起：`project_history` 放进上下文的每个事件，都是某件事的起点或挂在某件事上（§14）。

### 3.4 每件事怎么讲

| 事 | 行 |
| --- | --- |
| 输入 | 有用户内容时：`user: <文本>`（图片为 `[image]`；`steer` 输入为 `user (while you were working): <文本>`），同时带块时后接 `+ <blocks>`；只有块时：`<blocks>: <第一个块正文的第一行>`。`<blocks>` 按顺序列出每个块的开头标签和属性（照写），文本不以标签开头的块写作 `block`；生效中的块加 `(current)`，让其 `key` 失效的块加 `(ended)` |
| 你的回复 | `you: <文本>`，省略 thinking；未完成的输出为 `your reply was cut off (<原因>): <部分文本>`，带已提交的文本和其 `step/model-output-end` 的结果；head 停在其中的输出为 `you (still writing): <目前的文本>` |
| 工具调用 | `you called <tool> <参数简述> → <结局>`；参数从未到达的调用为 `you started calling <tool> (arguments not received) → <结局>` |
| 中断 | `the user interrupted the turn` |
| 权限变更 | `permission mode changed to <mode>` |
| 压缩 | `the conversation up to here was summarized` |
| fork | `this branch was forked from <parent branch>`；它的第一个输入紧接着讲出，就是它的任务 |
| Session fork | `this conversation was copied from another session up to here` |

Recall 按块文本的开头标签与属性，以及 `InputBlock::Raw` 的 `key` 和 `retire` 字段，通用地读取运行时块；它不认识任何生产者，所以新增一种块不需要改它。例如带一条 Prompt 规则的输入读作 `dim-rule source=AGENTS.md scope=branch op=replace (current): …`，Goal 的续行读作 `dim-goal kind=continuation count=2 limit=20 …: …`，都来自这些生产者已经写出的文本。展开一个输入时，先给出其用户内容全文，再按顺序给出每个块的全文，适用时各带 `(current version in your context)` 或 `(ended)`，与其他事一样按字符分页。

**工具调用的结局**是折叠后的状态，取第一个适用的：

| 挂上来的事件 | 结局 |
| --- | --- |
| `approval/respond` 为 deny | `denied by the user` |
| `approval/cancelled` | `approval cancelled: <reason>` |
| 关闭一个从未派发的调用的结果（没有 `call_event_id`） | `not run: <结果文本>` |
| `collaboration/result` | `sub-agent <agent> answered: <文本>`（或其失败） |
| `exec/ended` | `ran in background task <task>; finished at HH:MM: ok|error|cancelled: <文本>` |
| 有 `exec/started` 但没有 `exec/ended` | `running in background task <task>` |
| 带 `is_error` 的结果 | `error: <文本>` |
| 结果 | `ok: <文本>` |
| 都没有（head 停在调用中间） | `no result yet` |

**参数简述**对所有工具通用：`input` 的标量字段写作 `key=value`，嵌套的值显示为 `{…}`。展开这次调用时给出完整参数。

**每行都有固定的最大长度。** 一行就是一行：引用的文本取其第一行。行中每个可变部分超过 120 个字符时截断并加 `…`：引用的文本（用户文本、回复、块的正文、结果）、整个参数简述、`<blocks>`、工具名、一轮的工具汇总、原因、任务 id、agent 名、模式或父分支。一行最多有四个可变部分（最多的是工具调用：工具名、参数简述、任务和结果文本），其时间、id 和固定文字最多占 120 个字符，所以**一行绝不超过 600 个字符**。因此一页 20 个条目，连同日期行和最后一行，不超过 13,000 个字符。每个部分的全文都可用 `open` 展开。这是行格式，不是可调的限制。

**结果**是 `content` 的文本；blob 和图片为 `[file]` 与 `[image]`；工具带入的上下文消息为 `with <n> context messages`。展开这次调用时给出全部内容。结果比调用晚到不同的分钟时，结局带上结果的时间。

### 3.5 不重复讲

有些经历此刻仍在模型眼前。Recall 指向它们，而不是重复：

- 仍然生效的块，即固定历史中其 `key` 的最新一块且不是 retire 块（与上下文拼装重放 keyed baseline 的规则相同），在其输入那一行标为 `(current)`，展开时标为 `(current version in your context)`；展开带有同一 key 更早的块的输入，显示当时投递的那个版本；
- 最近一次的摘要就在上下文中；它的压缩那一行指出这一点，而不重复它。

### 3.6 轮次

一轮是同一个 `turn_id` 从 `turn/start` 到 `turn/end` 的跨度；它消费的输入属于它。任何轮次之外的事（两轮之间的压缩、fork、Session fork）是轮次之间独立的行。索引中一轮的那一行给出它的第一段用户文本（由没有用户文本的输入开启的一轮，例如 Goal 续行开启的一轮，给出该输入的 `<blocks>`）、调用过的工具及次数，以及 `turn/end` 中的结局（`completed`、`failed`、`cancelled`）。固定历史中没有 `turn/end` 的一轮：若是发起调用的这一轮，结局为 `in progress`；若是子分支继承前缀截断的父分支那一轮，结局为 `continued on the parent branch`。发起调用的这次 `recall` 本身也在这一轮中，结局为 `no result yet`。

索引按最近在前列出各轮和轮次之间的行。检索按最近在前列出所有轮次中命中的行。

### 3.7 事件声明

[ADR 001](../../../adr/001-harness-foundation.md#events-and-ownership-issue-2) 要求的声明，仅为描述：

| | 事件类型 |
| --- | --- |
| 写入 | 无。调用本身的事实由 ToolExecutor 写。 |
| 依赖（每次调用读取历史；不读其他事件） | `input/message`、`input/consumed`、`turn/start`、`turn/end`、`step/model-start`、`step/text`、`step/model-content`、`step/model-output-end`、`step/tool-use-start`、`step/tool-use`、`step/tool-result`、`approval/request`、`approval/respond`、`approval/cancelled`、`exec/started`、`exec/ended`、`collaboration/request`、`collaboration/message`、`collaboration/result`、`branch/create`、`branch/start`、`stop/interrupt`、`permission/changed`、`context/compaction`、`session/forked` |

它们都是核心事件，这份列表就是 §3.2 的白名单。Recall 不据此做决定，只讲述它们。若这些事件的数据合同去掉了 §3.2 的某个关联 id，Recall 就会失效。

## 4. 把手：时间与事件 id

每一行都带着其事件的时间（`create_at`）和事件 id。模型用 id 展开它。

- 每行显示其事件的完整 id，与存储的一致；事件 id 本来就唯一。一行的 id 是一轮的 `turn/start` 或一件事的起点事件的 id，没有两个条目共用一个 id，因为带多个块的输入事件是一个条目（§3.2）。`open` 和 `before` 接受某一行显示的 id；其他 id 都找不到。
- id 在本分支、共享前缀的子分支，以及从它 fork 出的 Session（复制来的事实保留事件 id，见[Session fork §6.5](../../../design/appserver/session-deletion-and-fork.md#65-opening-the-fork)）上永远有效。Timeline 序号做不到：fork 后历史的每一段都从 1 开始编号。

时间以 UTC 显示：每页开头和每次换日时有一行 `YYYY-MM-DD (UTC)`，各行只带 `HH:MM`。

## 5. 工具合同

### 5.1 声明

`toolContribution`，Session 作用域，键 `recall`（[session-boundary](../contracts/session-boundary.md)）：

| 字段 | 值 |
| --- | --- |
| `name` | `recall` |
| `risk` | `session` |
| `source` | `extension` |
| `description` | "Recall what happened earlier in this conversation: what the user said, what you did and what came back. With no arguments it lists past turns; open a turn or anything in it by its id, or search with about." |

工具声明是唯一的提示词。摘要、replacement 和系统提示词都不提这个工具（§7）。

### 5.2 输入

```json
{
  "type": "object",
  "properties": {
    "about":  {"type": "string", "minLength": 1, "maxLength": 200,
               "description": "Words to look for; all must appear, in any order, ignoring case."},
    "tool":   {"type": "string", "minLength": 1,
               "description": "Only calls to this tool."},
    "before": {"type": "string", "description": "An id; show what came before it."},
    "open":   {"type": "string", "description": "An id; show that turn, or that thing in full."},
    "offset": {"type": "integer", "minimum": 0,
               "description": "With open: continue from here, a line of a turn or a character of one thing."}
  },
  "additionalProperties": false
}
```

由参数路由调用：

| 参数 | 结果 |
| --- | --- |
| 无，或 `before` | 轮次索引，从最近几轮那一页开始 |
| `about` 和/或 `tool`，可选 `before` | 所有轮次中命中的行 |
| `open` = 一轮的 id，可选 `offset` | 从第 `offset` 行（0 为第一行）起逐行讲述那一轮 |
| `open` = 一行的 id，可选 `offset` | 那件事的全文 |

`before` 是固定历史中的一个位置：结果从事件位于该 id 的事件之前的最新条目开始。它接受任何一行的 id，所以结果返回的 `before=` 无论指向一轮、轮次之间的一行还是命中的一行都能用。`tool` 精确比较工具名；没有被调用过的名字给出 `Nothing matches …`。

`open` 与 `about`、`tool` 或 `before` 同时出现，或没有 `open` 却给了 `offset`，都是参数错误。ToolExecutor 不按 schema 检查输入，所以处理函数像 `wait` 一样自己检查参数，对类型错误、空字符串、超过 200 字符的 `about` 或未知字段返回 `ToolResult::error`。

### 5.3 分页

两个工具常量，不是配置（ADR 002 D4）：

| 视图 | 分页单位 | 每页 |
| --- | --- | --- |
| 轮次索引、检索、一轮 | 条目（行） | 20 条 |
| 一件事的全文 | 字符 | 8,000 字符 |

列表没有总字符上限。每行按其格式最长 600 个字符（§3.4），所以一页 20 个条目不超过 13,000 个字符。轮次索引和检索用 `before=<id>` 往前翻；一轮和一件事用 `offset` 续读，单位随视图：一轮按条目（0 为第一条），一件事按字符。字符按 Unicode 标量值计，所以 CJK 文本不会在字符中间被截断。一轮的行只会在末尾增长，所以返回过的 offset 始终指向同一行。

## 6. 模型看到什么

文本是普通的行，不带外层包装标签。`structuredContent` 只用于 UI 和持久化：`{level: "turns" | "turn" | "search" | "full", ids: [...], next?: {...}}`。

**轮次索引：**

```
2026-10-17 (UTC)
14:21 e41c07d2-5b6a-4f1e-9c3d-2a7b8e0f1c44 turn: user "Stop. Don't upload anything yet, only write the files locally." — you called shell ×2 — completed
14:03 c2d8a915-0e3b-4a77-8f21-6d5c9b13e0a8 turn: user "Use the staging bucket, never prod. Name files YYYY-MM-DD-report.csv…" — you called web_fetch, shell ×4, edit ×2 — cancelled
13:40 a7730e5f-91c4-4d2b-b6e8-3f0a7c2d5e19 the conversation up to here was summarized (that summary is in your context)
...
Earlier: before=a7730e5f-91c4-4d2b-b6e8-3f0a7c2d5e19. Open one: open=<id>.
```

**一轮（`open=c2d8a915-0e3b-4a77-8f21-6d5c9b13e0a8`）：**

```
2026-10-17 (UTC)
14:03 9b1e4d70-2c8f-4e15-a3b7-8d6f0e1c9a52 user: Use the staging bucket, never prod. Name files YYYY-MM-DD-report.csv, e.g. 2026-10-01-report.csv.
14:03 51c0aa3e-7d29-4b8c-9e04-1f6a3c8b2d71 you: I'll check the API limits first.
14:04 3f9a2c1b-6e4d-4f80-b2a9-0c7e5d1f8b36 you called web_fetch url=https://example.com/api/limits → ok: "Rate limits: 600 requests per minute per key…"
14:10 8d2f6b04-3a1e-4c97-8b5d-e2f0a6c4d913 dim-rule source=AGENTS.md scope=branch op=replace (current): Use pnpm, not npm.
14:12 c0e95a71-8b3f-4d26-a1c8-5e9d7f0b3a64 you called shell command="aws s3 cp report.csv s3://prod-reports/…" → denied by the user
14:20 77ab3e19-4f0c-4e8a-9d2b-6c1e8a5f7b30 the user interrupted the turn
End of turn. Open one: open=<id>.
```

**一件事（`open=3f9a2c1b-6e4d-4f80-b2a9-0c7e5d1f8b36`）：** 一行标题 `14:04 3f9a2c1b-6e4d-4f80-b2a9-0c7e5d1f8b36 you called web_fetch`，完整输入，然后完整结果；最后是 `Continue: open=3f9a2c1b-6e4d-4f80-b2a9-0c7e5d1f8b36 offset=<next>.` 或 `End.`

| 结果 | 最后一行 |
| --- | --- |
| 轮次索引 | `Earlier: before=<id>. Open one: open=<id>.` 或 `Nothing earlier.` |
| 检索 | `More: before=<id>.` 或 `No more.`；完全没有命中：`Nothing matches <复述的过滤条件>.` |
| 一轮 | `End of turn. Open one: open=<id>.` 或 `More of this turn: open=<id> offset=<next>.` |
| 一件事 | `Continue: open=<id> offset=<next>.` 或 `End.` |

错误（`ToolResult::error`）说明怎么改正调用：

| 情况 | 文本 |
| --- | --- |
| `open` 与 `about`、`tool` 或 `before` 同时出现 | `recall: open shows one turn or thing; drop about, tool and before.` |
| 没有 `open` 却给了 `offset` | `recall: offset needs open.` |
| 类型错误、空字符串、`about` 超过 200 字符、未知字段 | `recall: <field> must be <what>.` |
| `open` 或 `before` 不是某一行的 id | `No turn or line has id <id>.` |
| `offset` 超出一轮或一件事的末尾 | `Turn <id> has <n> lines.` / `<id> has <len> characters.` |

宿主错误按 `wait` 的映射：Session 正在重载导致 Branch 门面读取失败时返回 `HostError::Cancelled(CAPABILITY_RELOADING)`，其他门面错误（未知或已关闭的分支）返回带其文本的 `HostError::failed`，被取消的调用返回 `HostError::Cancelled`。ToolExecutor 把它们各自变成该调用的终态结果。

旧内容以模型阅读的文本出现，而不是给它的指令：每行都标明是谁说了或做了什么，模型像对待任何工具结果一样对待它们。

## 7. 发现

模型通过工具声明知道这个工具，声明出现在配置了该工具的每个请求中。否决的方案：

- **在摘要提示词或 `summary_message` 中加一行，写明工具。** replacement 保存在持久的 `context/compaction` 记录中，工具名会被冻结进历史；摘要策略作为核心部件会因此依赖一个可选工具（设计规则 §3、ADR 002 D3）。
- **只在压缩之后才声明工具。** 工具表是 Session 级的，压缩是分支级的；这需要为 ToolRegistry 增加新机制（设计规则 §4 第 8 条）。而且 Recall 在压缩之前也有用：在很长的上下文里找出某条早先消息的原文。

## 8. 交互

| 情况 | 行为 |
| --- | --- |
| 后续压缩 | 什么都不移动：Recall 覆盖整个历史，id 永不改变。每次压缩只是多一行 |
| 调用期间发生压缩 | 在调用方分支上不会发生：压缩只在请求的准备阶段或作为 Loop 的一步运行，从不在工具批次运行时发生（[ContextCompaction §5.4](ContextCompaction.md#54-手动压缩)）。无论如何，固定的 head 保证结果一致 |
| Session 内的分支 fork | 子分支以相同的 id 回忆继承前缀，然后是它的 fork 那一行和它自己的事件 |
| Session fork | fork 的 `main` 先是带原事件 id 的复制事实，然后是它的 `session/forked` 那一行，再是它自己的事件；源 Session 的 id 展开同样的内容 |
| Session 关闭与删除 | 关闭 Session 通过工具的取消令牌取消进行中的调用 |
| Goal | Goal 的块、续行和新分支开头的 `clear` 都是运行时块，按通用方式讲述（§3.4）；由续行开启的一轮以该块编入索引。Recall 不读 Goal 的状态 |
| 中断 | `stop/interrupt` 取消调用的令牌。工具在读取之后、渲染之前检查它，并返回 `HostError::Cancelled` |
| 重载 | 工具不持有状态。重载期间的门面读取返回 `HostError::Cancelled(CAPABILITY_RELOADING)`（§6）；进行中的调用结束，ToolExecutor 写其终态结果 |
| 回忆结果本身 | 一次回忆本身就是一次工具调用，以后的回忆像讲其他调用一样讲它 |
| 上下文增长 | 每次调用只返回一页（§5.3）；其余由阈值压缩限制 |

## 9. 不做什么

- **不写入，不改类型。** 没有事件、`StateCell`、快照或广播。叙事投影是 Recall 自己的；`project_history` 及其类型不变。
- **不新增上下文输入路径。** 结果只以工具结果进入模型（AGENTS 第 10 条、设计规则 §4 第 6 条）。
- **不给原始事件，不讲 §3.2 之外的类型。** 只讲 §3 列出的内容。
- **不读其他分支或 Session。** 分支取自 `ToolExecutionContext.branch_id`。
- **不做缓存。** 每次调用都读取固定的历史（§13）。

## 10. 安全与权限

- **风险类别 `session`。** 在 read-only 与 workspace-write 预设下直接允许，无需询问（[permission.rs](../../../../crates/dim-agent-capabilities/src/permission.rs)），与 `wait` 相同。它返回已经进入过本分支模型上下文的内容，以及 §3.2 中少数经历事件；不涉及文件、进程或网络。
- **信任边界：分支当前配置的模型。** 结果发往调用时所配置的 provider 和模型，它不一定是最初接收这些内容的那一个；分支可以采纳另一个模型（`session.agent.update`；[configuration.rs](../../../../crates/dim-app-server/src/agents/configuration.rs)）。这与分支已有的边界相同：切换之后，下一个请求会把整个投影上下文发给新的 provider。Recall 把这一边界扩展到已不在上下文中的事件，每次一个有界的调用。必须让早期内容不被另一个 provider 看到的用户，应为它新开一个 Session。
- **不可信内容。** 旧事件可能包含抓取的网页或注入的文本。它们以工具结果中带标签的行返回，与首次读取时的地位相同。
- **Tracing。** 除工具调用和结果事实外，没有其他内容。

## 11. 规则检查

| 规则 | 回答 |
| --- | --- |
| 设计规则 §4 第 1 条（改别人的状态写指令） | Recall 不改任何其他模块的状态。其结果由 ToolExecutor 写入。 |
| 设计规则 §4 第 2 条（交接） | 不适用：它不接手任何对象。 |
| 设计规则 §4 第 3 条（before 回调） | 没有。参数检查在工具处理函数内。 |
| 设计规则 §4 第 4 条（核心不带业务含义） | 不改核心。Recall 自己也不认识生产者：它按块文本的标签与属性以及 `key`/`retire` 讲述运行时块，Prompt、Goal 和后台通知的块都不需要专门处理。 |
| 设计规则 §4 第 5 条（原子批次） | 不适用：它不写入；读取固定在一个已发布的 head 上。 |
| 设计规则 §4 第 6 条（上下文只来自交接后的事件） | 它按上下文拼装的规则选取，只额外加入它点名的公开经历事件；其他类型都不讲。结果只以工具结果进入模型。 |
| 设计规则 §4 第 7 条（公开事件与服务） | 使用 `TreeCommands`、Branch 门面，以及公开事件和它们的关联 id。不读任何插件的内部状态。 |
| 设计规则 §4 第 8 条（基础设施不变） | Timeline、Dispatcher、信封、装配、门面和投影类型都不变。 |
| 设计规则 §4 第 9 条（只有后续决定要用的事实进 Timeline） | 不新增事件。 |
| 设计规则 §1（Capability 与 App Server 边界） | 是 `dim-agent-capabilities` 中的 Session Capability，装在 `defaults::install_local_execution` 中、与 `wait` 并列，所以 App Server 的产品 profile 和 `--models` profile 都有它。 |
| ADR 002 D1（作用域随状态） | Session：它提供工具，不持有状态；数据属于分支，每次调用读取。 |
| ADR 002 D2（不设接线模块） | 一个 `toolContribution`。 |
| ADR 002 D3（依赖方向） | 只依赖核心。它缺席时只少了这个工具（C3 省略规则）。 |
| ADR 002 D4（配置分离） | 没有业务配置；两个每页大小是工具的常量（§5.3）。 |
| ADR 002 D5（合同语言中立） | 合同（§5、§6）是中立的声明和文本；Rust 形式见附录 A。 |
| ADR 001 事件声明 | 见 §3.7：不写事件；依赖它从历史中读取的核心事件。 |

**影响面（公开内容）：** 一个工具名 `recall`。不新增事件、方法、wire 字段、服务，也不改类型。

## 12. 向机制所有者提出的需求

无。考虑过但不需要：

- 门面上的分页或过滤历史读取：选取、key 是否生效以及关联都本来就需要整个固定前缀，而 `Branch::history(Some(head))` 已经返回它；
- 按分支的工具声明（§7）；
- 在 ToolExecutor 中按 schema 检查工具输入：处理函数像 `wait` 一样自己检查参数（§5.2）；
- 为运行时块标签提供类型化的访问方法：标签和属性从 `context_block` 生产者已经写出的块文本中读取，不需要新类型。

## 13. 决定

| 项 | 决定 |
| --- | --- |
| 是什么 | 把分支历史讲给模型听，就像客户端里的会话视图 |
| 范围 | 整个分支历史，包括 fork 之前的历史（子分支的继承前缀、fork 出的 Session 复制来的事实）；压缩是一个事件，不是边界 |
| 投影 | 自己的投影：由一个起点事件和通过已有 id 挂上来的事件折叠成的事（§3.2）；按上下文拼装的规则判断是否算数，加上中断、权限变更、压缩、fork 和 Session fork。白名单：只讲它点名的类型；其他任何类型，核心或不透明、已有或以后新增，都不讲 |
| 工具调用 | 每次调用一行，带通用的参数简述和折叠后的结局：审批、未执行、后台任务、子 agent、错误或成功（§3.4） |
| 层级 | 轮次索引 → 一轮 → 一件事的全文；检索返回第 2 层的行 |
| 把手 | 每行带时间和完整事件 id；`open`/`before` 接受某一行显示的 id |
| 不重复 | 生效中的规则和 state、最近的摘要、流式片段、重复投递 |
| 提示词 | 只有工具声明（§5.1） |
| 风险类别 | `session` |
| 装配 | 所有真实模型 profile：装在 `defaults::install_local_execution` 中、与 `wait` 并列，产品 profile 和 `--models` profile 都用它。Recall 不依赖压缩，所以没有压缩的 `--models` 分支也有它 |
| 状态、写入、类型改动 | 无 |
| 成本 | 不做缓存：每次调用都读取并遍历固定的历史。只有测量表明需要时，才在工具内按分支和 head 加缓存；门面不变 |
| 交付 | 里程碑 M2（#129）；实现按 M2 的计划进行 |

## 14. 验收（流程测试）

流程测试放在 `crates/dim-agent-capabilities/tests`，使用测试策略；另加一个 `crates/dim-app-server/tests` 中的 App Server 测试，确认产品 profile 和 `--models` profile 的工具表中都有 `recall`：

1. **轮次索引。** 无参数调用按最近在前列出各轮，带时间、id、第一段用户文本、工具及次数和结局；沿 `before=` 往前翻直到 `Nothing earlier.`。
2. **一轮。** 展开一轮时按顺序讲出用户原文、回复、每次调用连同其结局的一行，以及一次中断，各带时间和 id。
3. **一件事。** 展开一次调用的 id 显示其完整输入和结果，每页 8,000 字符，用 `offset` 分页直到 `End.`；CJK 文本从不被拆开。展开一个带用户内容和三个块的输入，先显示用户内容，再按顺序显示三个块，跨块按字符分页。
4. **选取。** 已撤回和已删除的输入和 thinking 从不出现，§3.2 未点名的任何类型的事件（如 `context/prepare` 或 `step/tool-start`）也从不出现；Prompt 规则、Prompt state 和后台通知块各按其标签和属性出现，key 仍生效的块标为 `(current)`，retire 块标为 `(ended)`。带用户内容和多个块的输入是一个条目，结束在它上面的检索页以 `before=<它的 id>` 从下一个更早的条目继续，不漏掉任何条目。`project_history` 放进上下文的每个事件都属于某件事。
5. **压缩。** 两次压缩之后，Recall 仍能回到第一轮；每次压缩是一行；压缩前返回的 id 在压缩后仍展开同样的内容。
6. **检索。** 顺序和大小写都不同的词作为 `about`，能找到第一次压缩之前的一条用户消息，长工具结果深处的一个词能找到那次调用；`tool` 只保留该工具的调用。
7. **工具调用的结局。** 被拒绝、从未派发、转入后台任务且之后结束、由子 agent 回答、失败和成功的调用各一次，都按 §3.4 的结局讲成一行；steer 输入讲作用户在模型工作时插话。
8. **id 与 fork。** 一行的完整 id 可以展开它；不是某一行的 id 的找不到。由 `branch` 工具创建的子分支可以展开继承的 id，用它自己的 `branch/start` 讲出 fork 那一行，把前缀截断的父分支那一轮讲作 `continued on the parent branch`；发起调用的这一轮为 `in progress`。在从该分支所在 Session fork 出的 Session 中，fork 前返回的 id 展开同样的内容，`session/forked` 那一行跟在复制来的轮次之后。
9. **分页与无副作用。** 轮次索引、检索和一轮每页最多 20 条。带 1,000 个标量参数和 1,000 字符工具名的调用、带 50 个块的输入、调用过 100 种工具的一轮，各自的一行都不超过 600 个字符，每个可变部分在 120 个字符处以 `…` 截断；Timeline 只新增 ToolExecutor 为任何工具调用写的事件。
10. **权限。** 在 read-only 预设下，工具不经审批请求即运行。
11. **长的一轮。** 超过一页的一轮以 `More of this turn: open=<轮> offset=<n>.` 结尾；沿每次返回的续读参数一直读到这一轮结束，每行恰好出现一次，每页 20 条。
12. **被中断的输出。** 用现有的取消与失败形态（如 `context_flow/checkpoint_projection.rs`）：部分 `step/text` 后接 failed 或 cancelled 的 `step/model-output-end`、没有 `step/model-content`，讲作被截断的回复，文本恰为已提交的部分；有 `step/tool-use-start` 而无 `step/tool-use` 的调用讲作已开始、不带参数、结局为 `not run`；thinking 从不出现。
13. **参数。** §6 错误表的每一行都以错误结果返回其文本；从索引行、轮次之间的行和检索行取得的 `before=` 各自从该位置继续。

## 15. Ready

以上每条规则和引用的合同都已在 main 的 `bf95eaf6` 上重新核对（事件、Branch 门面、`TreeCommands`、`wait`、ToolExecutor 的取消、块的生产者、`session/forked`、2026-10-20 的 ADR 001）。

维护者已于 2026-10-21 批准本文为 Ready，并作出以下决定：

- 运行时块按通用方式讲述（§3.4），即开头标签及其属性照写，其他块为 `block:`，retire 块加 `(ended)`。
- 分页（§5.3）：轮次索引、检索和一轮按条目分页，每页 20 条，没有总字符上限；只有一件事的全文按字符分页，每页 8,000 字符。原先的整体结果上限和单独的每行上限已去掉。
- 一行由其格式限定长度（§3.4）：每个可变部分在 120 个字符处截断，所以一行绝不超过 600 个字符。
- 一个输入事件是一个条目（§3.2）：它的用户内容和所有块共用它的 id；没有块序号，也没有子把手。

- 装配（§13）：所有真实模型 profile，包括 `--models` profile，因为 Recall 不依赖压缩。

没有遗留的待决事项。

## 附录 A. Rust 绑定

```rust
// dim-agent-capabilities/src/recall.rs（新增）
pub const RECALL_TOOL: &str = "recall";
pub struct RecallPlugin { scope: SessionScope }
//   Deps = (InSession<TreeCommands>,)
//   apply: ctx.provide_as(scope.named_key::<ToolContribution>(RECALL_TOOL),
//                         ToolContribution { definition, handler: RecallTool(tree) })
struct RecallTool(Arc<TreeCommands>);
impl ToolHandler for RecallTool { /* §3–§6 */ }

// dim-agent-capabilities/src/defaults.rs，install_local_execution，与 wait::install 并列
builder.session_plugin(RecallPlugin::new)
       .session_tool(RECALL_TOOL);
```
