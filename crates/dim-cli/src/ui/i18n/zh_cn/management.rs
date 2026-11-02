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
        "Remove {name}? Enter confirm · Esc cancel" => "删除 {name}？Enter 确认 · Esc 取消",
        "↑↓ scroll · Esc back" => "↑↓ 滚动 · Esc 返回",
        "Added." => "已新建。",
        "Removed {name}." => "已删除 {name}。",
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
        _ => return None,
    })
}
