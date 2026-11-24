pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Loading..." => "正在加载…",
        "No entries." => "没有条目。",
        "No matching entry." => "没有匹配的条目。",
        "Issues" => "问题",
        "sources" => "来源",
        "skills" => "技能",
        "hooks" => "钩子",
        "scheduled jobs" => "定时任务",
        "models" => "模型",
        "workflow definitions" => "工作流定义",
        "command rules" => "命令规则",
        "enabled" => "已启用",
        "shadowed by {name}" => "被 {name} 覆盖",
        "Reloaded." => "已重新加载。",
        "Enabled {name}." => "已启用 {name}。",
        "Disabled {name}." => "已禁用 {name}。",
        "Reloaded {area}: {entries} entries, {disabled} disabled." => {
            "已重新加载{area}：共 {entries} 项，{disabled} 项已禁用。"
        }
        "Reloaded {area}: {entries} entries." => "已重新加载{area}：共 {entries} 项。",
        "Hooks" => "钩子",
        "invalid" => "无效",
        "Saved." => "已保存。",
        "Saved {name}." => "已保存 {name}。",
        "No change." => "没有变化。",
        "Your text is kept in {path}." => "你的内容保留在 {path}。",
        "The user's own document; always in effect." => "用户自己的文档；始终生效。",
        "There is no hooks file there." => "那里没有 hooks 文件。",
        "Owned by plugin {plugin}." => "由插件 {plugin} 提供。",
        "The user document turns every hook off (disableAllHooks)." => {
            "用户文档关闭了所有钩子（disableAllHooks）。"
        }
        "Enter toggle" => "Enter 切换",
        "Ctrl+E edit" => "Ctrl+E 编辑",
        "Ctrl+N add" => "Ctrl+N 新建",
        "Ctrl+R reload" => "Ctrl+R 重新加载",
        "Ctrl+T run" => "Ctrl+T 运行",
        "Ctrl+X remove" => "Ctrl+X 删除",
        "Tab inspect" => "Tab 查看",
        "Remove {name}?" => "删除 {name}？",
        "↑↓ scroll · Esc back" => "↑↓ 滚动 · Esc 返回",
        "Added." => "已新建。",
        "Removed {name}." => "已删除 {name}。",
        "unavailable" => "不可用",
        "Reset {name}?" => "重置 {name}？",
        "Enter confirm" => "Enter 确认",
        "Esc cancel" => "Esc 取消",
        "Reset {name} to its catalog entry." => "已将 {name} 重置为目录条目。",
        "Ran {name}." => "已运行 {name}。",
        "The user document changed since it was read; nothing was written." => {
            "用户文档在读取后已被修改；本次没有写入。"
        }
        "The job changed since it was read; nothing was written." => {
            "任务在读取后已被修改；本次没有写入。"
        }
        "The jobs changed since they were read; nothing was written." => {
            "任务在读取后已被修改；本次没有写入。"
        }
        "Your next edit opens the current document." => "下次编辑会打开当前文档。",
        "Your next add reopens your text." => "下次新建会重新打开你的文本。",
        "Scheduler" => "调度器",
        "jobs" => "任务",
        "Added {name}." => "已新建 {name}。",
        "Run {name}: {outcome}." => "已运行 {name}：{outcome}。",
        "Recent runs of {name}" => "{name} 的最近运行",
        "No run yet." => "尚未运行。",
        "No next run." => "没有下次运行。",
        "Next run {instant}." => "下次运行 {instant}。",
        "Last run {outcome} at {instant}." => "上次运行 {outcome}，于 {instant}。",
        "every {minutes} minutes" => "每 {minutes} 分钟",
        "at {instant}" => "于 {instant}",
        "cron {expression}" => "cron {expression}",
        "cron {expression} in {zone}" => "cron {expression}（{zone}）",
        "The job changed since it was read; nothing was removed." => {
            "任务在读取后已被修改；本次没有删除。"
        }
        "The Scheduler has no area document." => "调度器没有领域文档。",
        "No scheduled job has that id." => "没有该 id 的定时任务。",
        "A scheduled job's run takes no input document." => "定时任务的运行不接受输入文档。",
        "An MCP server's test takes no input document." => "MCP 服务器的测试不接受输入文档。",
        "This daemon cannot manage {area}." => "无法管理{area}。",
        "This daemon lists {area} but cannot toggle them." => "可以列出{area}，但无法切换它们。",
        "This daemon lists {area} but cannot edit them." => "可以列出{area}，但无法编辑它们。",
        "This daemon lists {area} but cannot reload them." => {
            "可以列出{area}，但无法重新加载它们。"
        }
        "This daemon lists {area} but cannot add them." => "可以列出{area}，但无法添加它们。",
        "This daemon lists {area} but cannot remove them." => "可以列出{area}，但无法删除它们。",
        "This daemon lists {area} but cannot run them." => "可以列出{area}，但无法运行它们。",
        "This daemon lists {area} but cannot inspect them." => {
            "可以列出{area}，但无法查看它们的详情或历史。"
        }
        "The document deleted {field}, which the owner cannot clear; nothing was written." => {
            "文档删除了 {field}，所有者无法清除它；本次没有写入。"
        }
        "The document changes no field; nothing was written." => {
            "文档没有改动任何字段；本次没有写入。"
        }
        "This workspace's hook settings changed since they were read; nothing was written." => {
            "此工作区的钩子设置已在读取后发生变化；本次没有写入。"
        }
        "Models" => "模型",
        "catalog" => "目录",
        "overrides" => "覆盖",
        "added" => "新增",
        "Models · {name}" => "模型 · {name}",
        "undeclared" => "未声明",
        "Context window {window} · max output {output}" => {
            "上下文窗口 {window} · 最大输出 {output}"
        }
        "Endpoint {endpoint}" => "端点 {endpoint}",
        "Endpoint {endpoint} · override {value}" => "端点 {endpoint} · 覆盖为 {value}",
        "The provider reports it unavailable." => "提供方报告它不可用。",
        "Limits undeclared; edit the model to declare them." => "上限未声明；编辑该模型以声明。",
        "Sign in to the account to use its models." => "登录账号后即可使用其模型。",
        "The signed-in account changed; reload to list its models." => {
            "登录的账号已变更；重新加载以列出其模型。"
        }
        "The provider has no catalog yet." => "此提供方还没有目录。",
        "The provider listed no models." => "提供方没有列出任何模型。",
        "Listed {count} models." => "已列出 {count} 个模型。",
        "Refreshed the models." => "模型已刷新。",
        "Could not list models: {error}. Showing the stored list." => {
            "无法列出模型：{error}。显示已存储的列表。"
        }
        "The signed-in account changed; the refresh cleared preferences, overrides and disables." => {
            "登录的账号已变更；刷新已清除偏好、覆盖和禁用项。"
        }
        "The models changed since they were read; nothing was removed." => {
            "模型在读取后已变更；本次没有移除任何东西。"
        }
        "The models changed since they were read; nothing was written." => {
            "模型在读取后已变更；本次没有写入。"
        }
        "No provider has that id." => "没有该 id 的提供方。",
        "{name} is unavailable ({reason}); its enablement is kept as it is." => {
            "{name} 不可用（{reason}）；其启用状态保持不变。"
        }
        "No model has that id." => "没有该 id 的模型。",
        "This model cannot be removed." => "此模型无法移除。",
        "A Provider's models have no area document." => "提供方的模型没有领域文档。",
        "Models have no run operation." => "模型没有运行操作。",
        "The document is not an object; nothing was written." => "文档不是对象；本次没有写入。",
        "The document changes no editable field; nothing was written." => {
            "文档没有修改任何可编辑字段；本次没有写入。"
        }
        "The model document names no model." => "模型文档没有指明模型。",
        "The provider has no model catalog to write." => "此提供方没有可写入的模型目录。",
        "ID {id}" => "ID {id}",
        "Source {source}" => "来源 {source}",
        "Status {status}" => "状态 {status}",
        "Tools {value}" => "工具 {value}",
        "Vision {value}" => "视觉 {value}",
        "Reasoning {value}" => "推理 {value}",
        "Modalities {list}" => "模态 {list}",
        "Description {text}" => "描述 {text}",
        "Release date {date}" => "发布日期 {date}",
        "Pricing {value}" => "价格 {value}",
        "Rate limit {value}" => "速率限制 {value}",
        "Matched provider {id}" => "匹配提供方 {id}",
        "Matched model {id}" => "匹配模型 {id}",
        "Match strategy {strategy}" => "匹配策略 {strategy}",
        "Capability overrides {fields}" => "能力覆盖字段 {fields}",
        "yes" => "是",
        "no" => "否",
        "not supported" => "不支持",
        "supported" => "支持",

        "Workflow" => "工作流",
        "Saved {name}. It stays disabled until you enable it." => {
            "已保存 {name}。在你启用它之前，它保持禁用。"
        }
        "The definitions changed since they were read; nothing was written." => {
            "定义在读取后已被修改；本次没有写入。"
        }
        "A definition's name is its key; add it to save it under another name." => {
            "定义的名称就是它的键；要用另一个名称保存，请新建它。"
        }
        "No project definition is named {name}." => "没有名为 {name} 的项目定义。",
        "No workflow definition is named {name}." => "没有名为 {name} 的工作流定义。",
        "Saved in the user workflow source; always enabled." => {
            "保存在用户工作流来源中；始终启用。"
        }
        "Starts only after you enable it." => "只有在你启用它之后才会启动。",
        "Defined by package {package}." => "由包 {package} 定义。",
        "{name} — {description} (required)" => "{name} — {description}（必填）",
        "{name} — {description}" => "{name} — {description}",
        "{name} — required" => "{name} — 必填",
        "{name} — default: {default}" => "{name} — 默认：{default}",
        "and {count} more" => "另有 {count} 项",
        "Recent runs" => "最近的运行",
        "A workflow definition is saved as an object with a source and a document." => {
            "工作流定义以包含来源和文档的对象保存。"
        }
        "A definition is saved to the user source or the project source." => {
            "定义保存到用户来源或项目来源。"
        }
        "Started {name}: {run_id}." => "已启动 {name}：{run_id}。",
        "Parameters are a JSON object of strings." => "参数是字符串组成的 JSON 对象。",
        "The list reached its size limit; later definitions are not shown." => {
            "列表已达到大小上限，之后的定义不会显示。"
        }
        "No workflow definition named {name} is in the list, which reached its size limit." => {
            "达到大小上限的列表中没有名为 {name} 的工作流定义。"
        }
        "MCP" => "MCP",
        "MCP servers" => "MCP 服务器",
        "Ctrl+T test" => "Ctrl+T 测试",
        "sign-in required" => "需要登录",
        "signed in" => "已登录",
        "sign-in expired" => "登录已过期",
        "missing scopes: {scopes}" => "缺少权限范围：{scopes}",
        "Executable: {command}" => "可执行文件：{command}",
        "Stored arguments (hidden): {count}" => "已存参数（隐藏）：{count}",
        "Working directory: {cwd}" => "工作目录：{cwd}",
        "Environment names: {names}" => "环境变量名：{names}",
        "Endpoint: {endpoint}" => "端点：{endpoint}",
        "Header names: {names}" => "请求头名称：{names}",
        "Bearer token: {state}" => "Bearer 令牌：{state}",
        "OAuth: configured (client ID {id})" => "OAuth：已配置（客户端 ID {id}）",
        "OAuth: configured" => "OAuth：已配置",
        "OAuth: not configured" => "OAuth：未配置",
        "Metadata URL: {state}" => "元数据 URL：{state}",
        "configured" => "已配置",
        "Owned by a plugin; change it through Plugins." => "由插件提供；请通过插件修改。",
        "MCP entry {id}" => "MCP 条目 {id}",
        "Connection {number} · {status}" => "连接 {number} · {status}",
        "Tools have not been discovered." => "尚未发现工具。",
        "No live session connections." => "没有活动的会话连接。",
        "Tools of {id}" => "{id} 的工具",
        "The server exposes no tools." => "服务器没有提供工具。",
        "The add document needs an id." => "新建文档需要一个 id。",
        "The add document needs a source of user or project." => {
            "新建文档的来源必须是 user 或 project。"
        }
        "The add document needs a configuration object." => "新建文档需要一个 configuration 对象。",
        "{id} is already in mcp list; remove it first or choose another id." => {
            "{id} 已在 mcp list 中；请先删除它或换一个 id。"
        }
        "{id} is already in mcp list; choose another id, or change it through its plugin." => {
            "{id} 已在 mcp list 中；请换一个 id，或通过其插件修改。"
        }
        "This entry's settings changed since they were read; nothing was written." => {
            "该条目的设置已在读取后发生变化；本次没有写入。"
        }
        "The MCP entry changed since it was read; nothing was written." => {
            "MCP 条目已在读取后发生变化；本次没有写入。"
        }
        "The MCP entries changed since they were read; nothing was written." => {
            "MCP 条目已在读取后发生变化；本次没有写入。"
        }
        "The MCP entry changed since it was read; nothing was removed." => {
            "MCP 条目已在读取后发生变化；本次没有删除。"
        }
        "MCP's documents belong to one entry; this save names none." => {
            "MCP 的文档属于单个条目；本次保存没有指定条目。"
        }
        "{id} is in more than one MCP source: {sources}. Name one with --source." => {
            "「{id}」出现在多个 MCP 来源中：{sources}。请用 --source 指定其中一个。"
        }
        "The action names no MCP source; a row carries the source its owner reported." => {
            "该操作没有指定 MCP 来源；每一行都带有其所有者报告的来源。"
        }
        "No MCP server {id} is in source {source}." => "来源 {source} 中没有 MCP 服务器 {id}。",
        "No MCP server has that id." => "没有该 id 的 MCP 服务器。",
        "This workspace has no project MCP source." => "此工作区没有项目级 MCP 来源。",
        "Enabled {id} ({source})." => "已启用 {id}（{source}）。",
        "Disabled {id} ({source})." => "已禁用 {id}（{source}）。",
        "It runs this workspace's .mcp.json entry, including later edits to it, in active and new sessions." => {
            "它会在当前和新的会话中运行此工作区的 .mcp.json 条目，包括之后对该文件的修改。"
        }
        "{id} is not in mcp list; withdrew any stored enablement of it in this workspace." => {
            "{id} 不在 mcp list 中；已撤销它在此工作区存储的启用状态。"
        }
        "The MCP catalog could not be read ({error}); withdrew this workspace's enablement of {id}. A user entry with this ID is unchanged." => {
            "无法读取 MCP 目录（{error}）；已撤销此工作区对 {id} 的启用。同 ID 的用户条目不受影响。"
        }
        "MCP configuration: {error}" => "MCP 配置：{error}",
        "The server rejected the stored credential; sign in again with mcp login." => {
            "服务器拒绝了已存储的凭据；请用 mcp login 重新登录。"
        }
        "The server requires additional scopes: {scopes}." => {
            "服务器需要额外的权限范围：{scopes}。"
        }
        "The server requires authorization; run mcp login ID." => {
            "服务器需要授权；请运行 mcp login ID。"
        }
        "The probe ended: {status}." => "探测结束：{status}。",

        "Command rules" => "命令规则",
        "user" => "用户",
        "Matches only this command." => "仅匹配此命令。",
        "Matches commands whose first words are these." => "匹配以此开头的命令。",
        "Allows a simple command that would otherwise ask." => "允许本会询问的简单命令。",
        "Denies the command under every preset." => "在任何预设下都拒绝该命令。",
        "Applies in every Session, in every workspace." => "在所有会话、所有工作区中生效。",
        "Rule added: {rule}." => "已添加规则：{rule}。",
        "Rule already present: {rule}." => "规则已存在：{rule}。",
        "Rule removed: {rule}." => "已删除规则：{rule}。",
        "That command rule is no longer listed; nothing was removed." => {
            "该命令规则已不在列表中；未删除任何内容。"
        }
        "Command rules changed since they were read; nothing was removed." => {
            "命令规则在读取后已被修改；未删除任何内容。"
        }
        "A command rule is DECISION MATCH COMMAND, such as deny prefix git push." => {
            "命令规则写作 DECISION MATCH COMMAND，例如 deny prefix git push。"
        }
        "Command: {command}" => "命令：{command}",
        "Use the line prompt or dimcode permissions." => {
            "请在行内提示符或 dimcode permissions 中使用。"
        }
        "Tool call timeout: {millis} ms" => "工具调用超时：{millis} 毫秒",
        "Variables: {names}" => "变量：{names}",
        "Unset variables: {names}" => "未设置的变量：{names}",
        "The MCP source changed since the preview; no entry was imported, and an entry it would have replaced may now be disabled. Preview again." => {
            "MCP 来源在预览之后已更改；没有导入任何条目，原本会被它替换的条目现在可能已被禁用。请重新预览。"
        }
        "The project entries were written and their withheld values were not; replace them again to retry." => {
            "项目条目已写入，保留未写的取值没有写入；再次替换即可重试。"
        }
        "Replaced {id}." => "已替换 {id}。",
        "Added {id}." => "已添加 {id}。",
        "Delivers to Session {session} branch {branch} ({mode})." => {
            "投递到会话 {session} 的分支 {branch}（{mode}）。"
        }
        "Launches a new Session in {cwd}." => "在 {cwd} 启动新会话。",
        "Disabled: {code} at {instant}." => "已禁用：{code}，时间 {instant}。",
        "The definition changed since it was read; nothing was written." => {
            "定义在读取之后已更改；没有写入任何内容。"
        }
        "Columns: ID, client, scope, path, state, entries or error." => {
            "列：ID、客户端、范围、路径、状态、条目数或错误。"
        }
        "No value from a store is read out or printed; --from ID previews one inside the daemon." => {
            "不会读出或打印存储中的任何取值；--from ID 会在守护进程内预览其中一个。"
        }
        "Columns: ID, action, destination ID.\nA renamed entry is written under its destination id; the source's entry with the old id is left as it was." => {
            "列：ID、操作、目标 ID。\n重命名的条目会以其目标 id 写入；来源中旧 id 的条目保持原样。"
        }
        "Imported and replaced entries start disabled: a user entry is written with enabled: false, and a project entry has no enablement record until you enable it locally." => {
            "导入和替换的条目初始为禁用：用户条目写入时带 enabled: false，项目条目在你本地启用前没有启用记录。"
        }
        "This daemon cannot list other clients' MCP stores." => {
            "此守护进程无法列出其他客户端的 MCP 存储。"
        }
        "This daemon cannot import MCP servers." => "此守护进程无法导入 MCP 服务器。",
        "The document holds no mcpServers entries." => "文档中没有 mcpServers 条目。",
        "Columns: ID, status, transport, changed fields, unsupported fields, error.\nA changed field names what a write would change and the kind of value it holds; no value is printed.\nAn unsupported field is kept unused or dropped, as shown after the field." => {
            "列：ID、状态、传输方式、更改的字段、不支持的字段、错误。\n更改的字段说明写入会改变什么以及取值的类型；不打印任何取值。\n不支持的字段按字段后的说明保持未使用或被丢弃。"
        }
        "A project entry is written as its public skeleton: the workspace file gets the command, declared cwd, URL origin and public OAuth fields. Its arguments, environment and header values, bearer token, a URL with userinfo, path, query or fragment, and unknown members go to your user-scope mcp-secrets record, never to the workspace file." => {
            "项目条目只写入其公开骨架：工作区文件得到命令、声明的 cwd、URL 来源和公开的 OAuth 字段。它的参数、环境变量和请求头取值、bearer token、带 userinfo、路径、查询或片段的 URL，以及未知成员，都写入你的用户级 mcp-secrets 记录，绝不写入工作区文件。"
        }
        "{id} has {count} more field names than the lists above show." => {
            "{id} 还有 {count} 个字段名未在上面的列表中显示。"
        }
        "Top-level members the import does not carry: none." => "导入未携带的顶层成员：无。",
        "Top-level members the import does not carry: {names}." => {
            "导入未携带的顶层成员：{names}。"
        }
        "Applying the selection at this revision." => "正按此修订应用所选内容。",
        "An unchanged entry was already in the source and was not written." => {
            "未改动的条目已存在于来源中，未写入。"
        }
        _ => return None,
    })
}
