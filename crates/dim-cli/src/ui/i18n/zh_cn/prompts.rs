pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
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
        "Restart dimcode?" => "重启 dimcode？",
        "Restart" => "重启",
        "y restart · n stay · ←→ move · enter confirm" => {
            "Y 重启 · N 留下 · ←→ 移动 · Enter 确认"
        }
        "Update available" => "有新版本",
        "Version {latest} is available (you have {current})." => {
            "{latest} 已发布（当前 {current}）。"
        }
        "Will run: {command}" => "将执行：{command}",
        "To update, run: {command}" => "升级请执行：{command}",
        "Update now" => "立即升级",
        "Skip this version" => "跳过此版本",
        "Skipped {version}." => "已跳过 {version}。",
        "Later" => "稍后",
        "Close" => "关闭",
        "Restart now" => "立即重启",
        "Updating to {version}..." => "正在升级到 {version}...",
        "Updated to {version}. Restart DimCode to use it." => {
            "已升级到 {version}，重启 DimCode 后生效。"
        }
        "Update failed: {error}" => "升级失败：{error}",
        "←→ move · Enter confirm · Esc close" => "←→ 移动 · Enter 确认 · Esc 关闭",
        "Permission required" => "需要授权",
        "Allow once" => "允许一次",
        "Allow always" => "始终允许",
        "Reject" => "拒绝",
        "Approve for me" => "智能权限",
        "Full access" => "完全访问权限",
        "Run {tool}" => "运行 {tool}",
        "Changing the approvals mode from here is not yet available" => {
            "暂不支持在此处更改 approvals 模式"
        }
        "y allow · n reject · ←→/Tab move · Enter confirm" => {
            "y 允许 · n 拒绝 · ←→/Tab 移动 · Enter 确认"
        }
        "Enter submit · Tab edit · Esc decline" => "Enter 提交 · Tab 编辑 · Esc 拒答",
        "Space toggle · Tab next · Esc decline" => "Space 切换 · Tab 下一题 · Esc 拒答",
        "1-9 select · Tab next · Esc decline" => "1-9 选择 · Tab 下一题 · Esc 拒答",
        "Question" => "问题",
        "Question on branch {branch}" => "来自分支 {branch} 的问题",
        "Sending answer…" => "正在发送回答…",
        "Question not ready; press Enter to send again" => "问题尚未就绪；按 Enter 重试",
        "Confirm" => "确认",
        "Enter save · Esc back" => "Enter 保存 · Esc 返回",
        "Enter submit · 1-9 edit · Tab/←→ back · Esc decline" => {
            "Enter 提交 · 1-9 修改 · Tab/←→ 返回 · Esc 拒答"
        }
        "↑↓ move · Space toggle · Enter next · Tab/←→ switch · Esc decline" => {
            "↑↓ 移动 · Space 勾选 · Enter 下一题 · Tab/←→ 切题 · Esc 拒答"
        }
        "↑↓ move · 1-9 select · Enter next · Tab/←→ switch · Esc decline" => {
            "↑↓ 移动 · 1-9 选择 · Enter 下一题 · Tab/←→ 切题 · Esc 拒答"
        }
        "AskUserQuestion received an invalid payload." => "AskUserQuestion 收到无效的数据。",
        "Missing answer for Q{index}" => "第 {index} 题缺少回答",
        "Type your own answer" => "输入自定义回答",
        "Review your answers and press Enter to submit." => "确认答案后按 Enter 提交。",
        "Not answered" => "未回答",
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
        "↑↓/Tab move · Enter apply · Esc back" => "↑↓/Tab 移动 · Enter 应用 · Esc 返回",
        "Confirm Full Access" => "确认开启完全授权",
        "Cancel" => "取消",
        "Tab switch  Enter confirm  Esc cancel" => "Tab 切换  Enter 确认  Esc 取消",
        "Full access reduces confirmation prompts and can run sensitive actions, file changes, or external commands directly. Use it only when you trust the current task." => {
            "完全授权会减少确认步骤，并可直接执行敏感操作、文件修改或外部命令。仅建议在你信任当前任务时使用。"
        }
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
        "Read" => "读取",
        "List" => "列出",
        "Grep" => "文本搜索",
        "Search the web" => "网页搜索",
        "Read skill" => "读取技能",
        "Search git history" => "搜索 git 历史",
        "Edit" => "编辑",
        "Write" => "写入",
        "Create" => "创建",
        " (detached)" => "（后台）",
        "Checked on background task" => "查看了后台任务",
        "Waiting…" => "等待中…",
        "… +{count} lines" => "… 还有 {count} 行",
        _ => return None,
    })
}
