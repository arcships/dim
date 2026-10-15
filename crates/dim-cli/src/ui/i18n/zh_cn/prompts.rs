pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // prompts.rs: exit confirmation
        "The agent is still running." => "智能体仍在运行。",
        "AskUser is waiting for an answer." => "AskUser 正在等待回答。",
        "A permission request is waiting." => "有授权请求正在等待。",
        "Background work may still be active." => "后台任务可能仍在运行。",
        "Exit dimcode?" => "退出 dimcode？",
        "Exit" => "退出",
        "Stay" => "留下",
        "y exit · n stay · ←→ move · enter confirm" => {
            "Y 退出 · N 留下 · ←→ 移动 · Enter 确认"
        }
        // prompts.rs: permission request
        "Permission required" => "需要授权",
        "Allow once" => "允许一次",
        "Allow always" => "始终允许",
        "Reject" => "拒绝",
        "Approve for me" => "智能权限",
        "Full access" => "完全访问权限",
        "Run {tool}" => "运行 {tool}",
        "approved once: {subject}" => "已单次批准：{subject}",
        "approved always: {subject}" => "已始终批准：{subject}",
        "rejected: {subject}" => "已拒绝：{subject}",
        "Changing the approvals mode from here is not yet available" => {
            "暂不支持在此处更改 approvals 模式"
        }
        "y allow · n reject · ←→/tab move · enter confirm" => {
            "Y 允许 · N 拒绝 · ←→/Tab 移动 · Enter 确认"
        }
        // prompts.rs: AskUser
        "Confirm" => "确认",
        "type answer · enter save · esc back" => "输入回答 · Enter 保存 · Esc 返回",
        "enter submit · number edit · tab/←→ back · esc cancel" => {
            "Enter 提交 · 数字键修改 · Tab/←→ 返回 · Esc 取消"
        }
        "↑↓ move · space toggle · enter next · tab/←→ switch · esc cancel" => {
            "↑↓ 移动 · Space 勾选 · Enter 下一题 · Tab/←→ 切题 · Esc 取消"
        }
        "↑↓ move · number select · enter next · tab/←→ switch · esc cancel" => {
            "↑↓ 移动 · 数字键选择 · Enter 下一题 · Tab/←→ 切题 · Esc 取消"
        }
        "AskUserQuestion received an invalid payload." => "AskUserQuestion 收到无效的数据。",
        "Missing answer for Q{index}" => "第 {index} 题缺少回答",
        "Type your own answer" => "输入自定义回答",
        "Review your answers and press Enter to submit." => "确认答案后按 Enter 提交。",
        "Not answered" => "未回答",
        // approvals_panel.rs
        "Tool Approvals" => "工具审批",
        "full access" => "完全授权",
        "All tools run automatically, bypass permissions" => "所有工具自动执行，跳过权限确认",
        "approve for me" => "智能权限",
        "Agent automatically reviews commands and tool actions. Unsafe commands ask for confirmation; routine actions pass automatically, safe, smart, and convenient." => {
            "Agent 自动帮你审核命令和工具操作，不安全命令会询问，常规操作自动放行，安全智能又方便"
        }
        "Default permissions" => "默认权限",
        "default" => "默认",
        "Read tools run automatically, write tools require confirmation" => {
            "读取类工具自动执行，写入类工具需确认"
        }
        "current" => "当前",
        "↑↓/tab move · enter apply · esc back" => "↑↓/Tab 移动 · Enter 应用 · Esc 返回",
        "Confirm Full Access" => "确认开启完全授权",
        "Cancel" => "取消",
        "Tab switch  Enter confirm  Esc cancel" => "Tab 切换  Enter 确认  Esc 取消",
        "Full access reduces confirmation prompts and can run sensitive actions, file changes, or external commands directly. Use it only when you trust the current task." => {
            "完全授权会减少确认步骤，并可直接执行敏感操作、文件修改或外部命令。仅建议在你信任当前任务时使用。"
        }
        // transcript/tool_group.rs: summary verbs
        "ran" => "运行了",
        "running" => "正在运行",
        "read" => "读取了",
        "reading" => "正在读取",
        "explored" => "浏览了",
        "exploring" => "正在浏览",
        "edited" => "编辑了",
        "editing" => "正在编辑",
        "changed" => "修改了",
        "changing" => "正在修改",
        "checked on" => "查看了",
        "checking on" => "正在查看",
        "Explored" => "浏览了",
        "Exploring" => "正在浏览",
        "Edited" => "编辑了",
        "Editing" => "正在编辑",
        "Changed" => "修改了",
        "Changing" => "正在修改",
        // transcript/tool_group.rs: counts
        "1 file" => "1 个文件",
        "{count} files" => "{count} 个文件",
        "1 web page" => "1 个网页",
        "{count} web pages" => "{count} 个网页",
        "1 skill" => "1 个技能",
        "{count} skills" => "{count} 个技能",
        "1 search" => "1 次搜索",
        "{count} searches" => "{count} 次搜索",
        "1 command" => "1 个命令",
        "{count} commands" => "{count} 个命令",
        "1 task" => "1 个任务",
        "{count} tasks" => "{count} 个任务",
        "1 tool" => "1 个工具",
        "{count} tools" => "{count} 个工具",
        "{files} and {pages}" => "{files}和{pages}",
        ", " => "，",
        " · {count} failed" => " · {count} 个失败",
        // transcript/tool_group.rs: call rows
        "Read" => "读取",
        "List" => "列出",
        "Grep" => "文本搜索",
        "Search the web" => "网页搜索",
        "Read skill" => "读取技能",
        "Search git history" => "搜索 git 历史",
        "Edit" => "编辑",
        "Write" => "写入",
        " (detached)" => "（后台）",
        "Checked on background task" => "查看了后台任务",
        "Waiting…" => "等待中…",
        "… +{count} lines" => "… 还有 {count} 行",
        _ => return None,
    })
}
