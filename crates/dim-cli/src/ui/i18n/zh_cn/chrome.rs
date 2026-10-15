pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // tui.rs
        "Copied to clipboard" => "已复制到剪贴板",
        "Opened link" => "已打开链接",
        "Link: {url}" => "链接：{url}",
        "Not available" => "不可用",
        "files changed  {count}" => "文件已修改  {count}",
        "active session" => "活动会话",
        "new-session draft" => "新会话草稿",
        "Tool approvals: {title} ({target})" => "工具审批：{title}（{target}）",
        "Connection lost; reconnecting..." => "连接已断开，正在重新连接...",
        "Reconnecting... (attempt {attempt})" => "正在重新连接...（第 {attempt} 次）",
        "Reconnected" => "已重新连接",
        "reconnected" => "已重新连接",
        "Message queued" => "消息已加入队列",
        "Run a shell command · Esc to exit" => "输入 shell 命令 · Esc 退出",
        "! shell mode" => "! shell 模式",
        "!! shell mode · not sent to the model" => "!! shell 模式 · 不发送给模型",
        "A shell command is already running" => "已有 shell 命令在运行",
        "Running… · Ctrl+C to interrupt · {seconds}s" => "运行中… · Ctrl+C 中断 · {seconds}s",
        "exit code {code}" => "退出码 {code}",
        "killed by {signal}" => "被 {signal} 终止",
        "interrupted" => "已中断",
        "failed to start: {reason}" => "启动失败：{reason}",
        "not sent to the model" => "未发送给模型",
        "… {count} more lines" => "… 还有 {count} 行",
        "… {count} earlier lines" => "… 前面还有 {count} 行",
        "Interrupt requested; Ctrl+C again to exit" => "已请求中断；再次按 Ctrl+C 退出",
        "Press Ctrl+C again to exit" => "再次按 Ctrl+C 退出",
        "User interrupted the run." => "用户已中断本次运行。",
        "Could not save the language: {error}" => "无法保存语言：{error}",
        "Language set to {locale}" => "语言已设为 {locale}",
        "Language set to system default" => "语言已设为跟随系统默认值",
        "Connected {provider}" => "已连接 {provider}",
        "Tool details {state}" => "工具详情：{state}",
        "Tool details {state} (not saved: {error})" => "工具详情：{state}（未保存：{error}）",
        "/{name} needs a session — send a message first" => "/{name} 需要会话，请先发一条消息",
        "Nothing is running" => "当前没有运行",
        "No queued message" => "没有队列消息",
        "Only in fullscreen (/tui)" => "仅在全屏界面可用（/tui）",
        "/{name} is not yet available" => "/{name} 尚不可用",
        "new session" => "新会话",
        "resumed {id} · {count} messages" => "已恢复 {id} · {count} 条消息",
        " · earlier history: /history" => " · 更早历史：/history",
        "Error: {message}" => "错误：{message}",
        "working…  {elapsed}" => "处理中…  {elapsed}",
        "thinking…  {elapsed}" => "思考中…  {elapsed}",
        "Ask anything..." => "输入任何内容...",
        "Earlier history · {id} · {count} messages · /history for the previous page" => {
            "更早历史 · {id} · {count} 条消息 · /history 查看上一页"
        }
        "No active session" => "没有活动会话",
        "No earlier messages." => "没有更早的消息。",
        "answer required" => "需要回答",
        "permission required" => "需要授权",
        "esc interrupt" => "Esc 中断",
        "↑ edit · ctrl+s steer now · /queue" => "↑ 编辑 · ctrl+s 立即 steer · /queue",
        "+{count} more · ↑ edit · ctrl+s steer now · /queue" => {
            "还有 {count} 条 · ↑ 编辑 · ctrl+s 立即 steer · /queue"
        }
        "Queue ({count})" => "队列（{count}）",
        "Thinking…" => "思考中…",
        "Thought" => "已思考",
        "Thought for {duration}" => "已思考 {duration}",
        "Welcome to DIM {version}!" => "欢迎使用 DIM {version}！",
        "Type / for commands · Ctrl+P for the palette" => "输入 / 查看命令 · Ctrl+P 打开命令面板",
        "Workspace" => "工作区",
        "Git branch" => "Git 分支",
        "Bye!" => "再见！",
        "To resume this session:" => "恢复此会话：",

        // composer/commands.rs: categories
        "session" => "会话",
        "run" => "运行",
        "view" => "视图",
        "model" => "模型",
        "app" => "应用",
        "skill" => "技能",
        // composer/commands.rs: descriptions
        "start a new session" => "开始新会话",
        "resume a previous session" => "恢复之前的会话",
        "load the previous page of messages" => "加载上一页消息",
        "show context and plan usage" => "显示上下文和套餐用量",
        "interrupt the run" => "中断运行",
        "steer with the next queued message" => "用下一条队列消息 steer",
        "scroll to the top" => "滚动到顶部",
        "scroll to the bottom" => "滚动到底部",
        "pick an earlier message to edit" => "选择较早的消息进行编辑",
        "collapse or expand tool details and thoughts" => "折叠或展开工具详情与思考",
        "switch between fullscreen and scrollback" => "在全屏和回滚界面之间切换",
        "switch theme" => "切换主题",
        "switch language" => "切换语言",
        "select a model" => "选择 model",
        "connect a provider" => "连接 provider",
        "configure tool approvals" => "配置工具审批",
        "open settings" => "打开设置",
        "reconnect to the App Server" => "重新连接 App Server",
        "suspend to the shell" => "挂起到 Shell",
        "exit" => "退出应用",

        // composer/palette.rs
        "type filter · enter select · esc clear/close" => "输入筛选 · Enter 选择 · Esc 清除/关闭",
        "esc close" => "Esc 关闭",
        "Suggestions" => "建议",
        "Skills" => "技能",
        "Commands" => "命令",
        "Command Palette" => "命令面板",
        "No matches" => "无匹配项",
        " (unavailable)" => "（不可用）",

        // composer/mod.rs
        "[Text: {preview} ({count} chars)]" => "[文本：{preview}（{count} 字符）]",
        "[... {count} lines]" => "[... {count} 行]",
        "pasted text" => "粘贴的文本",

        // list_panel.rs
        "filter: {value}" => "筛选：{value}",
        "Total: {count}" => "总数：{count}",

        // settings_panel.rs
        "on" => "开启",
        "off" => "关闭",
        "Interface" => "界面",
        "Theme" => "主题",
        "Language" => "语言",
        "Startup UI form" => "启动界面形态",
        "fullscreen" => "全屏",
        "scrollback" => "回滚",
        "Tool details" => "工具详情",
        "expanded" => "展开",
        "collapsed" => "折叠",
        "Mouse (fullscreen)" => "鼠标（全屏）",
        "Permissions" => "权限",
        "Tool approvals" => "工具审批",
        "Models" => "模型",
        "Model" => "模型",
        "not set" => "未设置",
        "Settings" => "设置",
        "↑↓ move · enter select · ←→/space change · esc close" => {
            "↑↓ 移动 · Enter 选择 · ←→/Space 修改 · Esc 关闭"
        }

        // language_panel.rs
        "↑↓ preview · enter save · esc restore" => "↑↓ 预览 · Enter 保存 · Esc 恢复",
        "System default ({locale})" => "系统默认（{locale}）",

        // theme_panel.rs
        "↑↓ preview · enter apply · esc cancel" => "↑↓ 预览 · Enter 应用 · Esc 取消",
        "System" => "系统",
        "Could not save the theme: {error}" => "无法保存主题：{error}",

        // sessions_panel.rs
        "Sessions" => "会话",
        "enter open · ^R refresh" => "Enter 打开 · ^R 刷新",
        "Loading sessions..." => "正在加载会话...",
        "No saved sessions yet." => "暂无已保存的会话。",
        "No matching sessions." => "无匹配的会话。",
        "now" => "刚刚",
        "{count}m ago" => "{count}分钟前",
        "{count}h ago" => "{count}小时前",
        "{count}d ago" => "{count}天前",

        // models_panel.rs
        "Select a model" => "选择模型",
        "Loading models..." => "正在加载模型...",
        "No models available. Connect a provider first." => "无可用模型。请先连接 Provider。",
        "No matching models." => "无匹配的模型。",
        "current" => "当前",
        "Edit models" => "编辑模型",
        "Editing models is not available yet" => "暂不支持编辑模型",
        "drawing diagram… {count} lines" => "正在绘制图表…已收到 {count} 行",
        // notify.rs
        "Task complete" => "任务已完成",
        "Permission needed: {tool}" => "需要授权：{tool}",
        "Question waiting for your answer" => "有问题等你回答",
        "Notify when agent needs you" => "需要你时通知",
        // keys.rs
        "show the key bindings" => "显示快捷键",
        "Application keys" => "应用快捷键",
        "Enter submit · Shift+Enter or Ctrl+J newline · ↑↓ history" => {
            "Enter 提交 · Shift+Enter 或 Ctrl+J 换行 · ↑↓ 历史"
        }
        "Ctrl+O or Ctrl+P commands · Esc interrupt or clear · Ctrl+S steer queued · ↑ edit queued" => {
            "Ctrl+O 或 Ctrl+P 命令 · Esc 中断或清空 · Ctrl+S 立即 steer · ↑ 编辑队列消息"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C 清空，连按两次退出 · 草稿为空时 Ctrl+D 退出"
        }
        "Fullscreen" => "全屏",
        "PageUp/PageDown scroll · Home/End top or bottom · Tab pick a message, e edit it" => {
            "PageUp/PageDown 滚动 · Home/End 到顶部或底部 · Tab 选择消息，e 编辑"
        }
        "wheel scroll · drag select and copy · click a link to open it" => {
            "滚轮滚动 · 拖动选择并复制 · 点击链接打开"
        }
        "mouse off (/settings): the terminal selects and copies" => {
            "鼠标已关闭（/settings）：由终端选择和复制"
        }
        "Terminal-owned history" => "终端管理的历史记录",
        "scroll: {value}" => "滚动：{value}",
        "search: {value}" => "搜索：{value}",
        "copy: {value}" => "复制：{value}",
        "terminal search" => "终端搜索",
        "terminal scrollback" => "终端回滚",
        "terminal copy" => "终端复制",
        "drag to select" => "拖动选择",
        "drag or {shortcut}" => "拖动或 {shortcut}",
        _ => return None,
    })
}
