pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Enter output / interrupt member · x stop command · Esc close" => {
            "Enter 输出/中断成员 · x 停止命令 · Esc 关闭"
        }
        "Enter output / interrupt member · x stop command · Ctrl+X stop all · Esc close" => {
            "Enter 输出/中断成员 · x 停止命令 · Ctrl+X 停止全部 · Esc 关闭"
        }
        "Loading branch history... Stop-all unavailable" => "正在读取分支历史… 暂不能停止全部",
        "Branch history failed; reopen to retry" => "读取分支历史失败；重新打开重试",
        "Branch history is incomplete; stop-all unavailable" => "分支历史尚未读完，暂不能停止全部",
        "Running" => "运行中",
        "Lines" => "线",
        "Enter show the line · x end it · Esc close" => "Enter 查看该线 · x 结束 · Esc 关闭",
        "This session has no lines." => "这个会话还没有线。",
        "No matching lines." => "没有匹配的线。",
        "the session itself" => "会话本身",
        "working" => "工作中",
        "ended itself" => "自行结束",
        "ended by an operation" => "由操作结束",
        "ended with {id}" => "随 {id} 结束",
        "creation failed" => "创建失败",
        "shown" => "当前显示",
        "line" => "线",
        "line {id}" => "线 {id}",
        "jump to a message of the shown line" => "跳到当前线的某条消息",
        "Timeline" => "时间线",
        "Enter jump · e edit · q quote · b branch · f fork · Esc close" => {
            "Enter 跳转 · e 编辑 · q 引用 · b 分支 · f 派生 · Esc 关闭"
        }
        "No messages yet." => "还没有消息。",
        "Already on screen above; /tui jumps to it" => "已在上方；/tui 可直接跳转",
        "{count} earlier messages · Enter or click to load" => {
            "更早的 {count} 条消息 · Enter 或点击加载"
        }
        "follow, talk to and end this session's lines" => "查看、对话并结束本会话的线",
        "End line" => "结束线",
        "y/N" => "y/N",
        "main is the session itself; a line ends, main does not" => {
            "main 就是会话本身；线可以结束，main 不能"
        }
        "Line has ended:" => "该线已结束：",
        "Esc close" => "Esc 关闭",
        "/{name} needs {argument}" => "/{name} 需要 {argument}",
        "/{name} takes text only; remove {label} to send" => {
            "/{name} 只接受文字；移除 {label} 后再发送"
        }
        "e edit · b branch · f fork" => "e 编辑 · b 分支 · f 派生",
        "e edit" => "e 编辑",
        "Fork the session before “{message}”? (y/N)" => "在“{message}”之前派生会话？(y/N)",
        "Fork the session after the last turn? (y/N)" => "在最近一个回合之后派生会话？(y/N)",
        "⑂ new line from {id} · Esc cancel" => "⑂ 从 {id} 开新线 · Esc 取消",
        "A fork copies main; b opens a line here" => "派生只复制 main；按 b 在这里开线",
        "A line opens from text; images and files were not included" => {
            "线只用文字开场；图片和文件未包含在内"
        }
        "No finished turn to cut at yet" => "还没有已结束的回合可以切分",
        "forked {id} · {count} messages" => "已派生 {id} · {count} 条消息",
        "after message {count}" => "第 {count} 条消息之后",
        "open a line from the end of the shown line" => "从当前线的末尾开一条线",
        "fork the session at its last finished turn" => "在最近结束的回合处派生会话",
        "rename this session" => "重命名此会话",
        "Renamed to {title}" => "已重命名为 {title}",
        "The Session is now untitled." => "会话现在没有标题。",
        "This daemon does not support renaming Sessions" => "此守护进程不支持重命名会话",
        "Renaming failed" => "重命名失败",
        "Starting" => "启动中",
        "Delegation result" => "委派结果",
        "Enter details · x stop selected · Ctrl+X stop all · Esc close" => {
            "Enter 详情 · x 停止所选任务 · Ctrl+X 停止全部 · Esc 关闭"
        }
        "x interrupt member · Esc back" => "x 中断成员 · Esc 返回",
        "read-only" => "只读",
        "Member Session" => "成员会话",
        "Loading conversation..." => "正在读取会话…",
        "No conversation yet" => "暂无会话内容",
        "Observing conversation" => "正在查看会话",
        "Conversation unavailable" => "会话不可用",
        "Session closed" => "会话已关闭",
        "Member conversation" => "成员会话内容",
        "x stop command · Esc back" => "x 停止命令 · Esc 返回",
        "Esc back" => "Esc 返回",
        "Tail preview - last 2,000 lines / 50 KiB" => "末尾预览 - 最近 2,000 行 / 50 KiB",
        "Loading output..." => "正在读取输出…",
        "No output yet" => "尚无输出",
        "Live output unavailable" => "实时输出不可用",
        "Live output unavailable in this profile" => "当前配置不提供实时输出",
        "Output read failed" => "读取输出失败",
        "Completed" => "已完成",
        "Failed" => "失败",
        "Cancelled" => "已取消",
        "workflow" => "工作流",
        "Stopping" => "正在停止",
        "Stopped" => "已停止",
        "Interrupted" => "已中断",
        "last observed" => "最后观测",
        "Loading Workflow runs... Stop-all unavailable" => "正在读取工作流运行… 暂不能停止全部",
        "Workflow: last observed; refresh failed, reopen to retry" => {
            "工作流：显示最后观测结果；刷新失败，重新打开重试"
        }
        "Workflow runs unavailable; reopen to retry" => "工作流运行不可用；重新打开重试",
        "The selected Workflow run is no longer available" => "所选工作流运行已不可用",
        "Workflow inventory is incomplete; stop-all unavailable" => {
            "工作流列表尚未读完，暂不能停止全部"
        }
        "Workflow stop unavailable" => "无法停止工作流",
        "Run no longer available; last observed record" => "运行已不可用；显示最后观测记录",
        "x stop workflow · Esc back" => "x 停止工作流 · Esc 返回",
        "Run" => "运行",
        "Source" => "来源",
        "Stages" => "阶段",
        "result" => "结果",
        "Log" => "日志",
        "Report" => "报告",
        "waiting" => "等待中",
        "launching" => "启动中",
        "completed" => "已完成",
        "failed" => "失败",
        "stopped" => "已停止",
        "skipped" => "已跳过",
        "lost" => "已丢失",
        "none" => "无",
        "pending" => "待发送",
        "delivered" => "已送达",
        "undeliverable" => "无法送达",
        "Agent is starting; no member Session to stop yet" => {
            "智能体正在启动，尚无可停止的成员会话"
        }
        "Provider members are controlled by the leader; no stop here" => {
            "提供方成员由主智能体控制，此处不能停止"
        }
        "Provider members have no conversation to open here" => "提供方成员没有可在此打开的会话",
        "That delegation is no longer in this transcript" => "该委派已不在当前对话记录中",
        "Member model" => "成员模型",
        "No model observed" => "尚未观察到模型",
        "Observing member..." => "正在观察成员…",
        "{model} (observation unavailable)" => "{model}（观察不可用）",
        "{model} (Session closed)" => "{model}（会话已关闭）",
        "{model} (last observed)" => "{model}（最后观察到）",
        "Agent: {name}" => "智能体：{name}",
        "Provider member: {name}" => "提供方成员：{name}",
        "Task: {task}" => "任务：{task}",
        "Activity: {activity}" => "活动：{activity}",
        "Activity: not observed on this attachment" => "活动：本次连接未观察到",
        "Result: {result}" => "结果：{result}",
        "Controlled by the leader" => "由主智能体控制",
        "› Open member conversation" => "› 打开成员会话",
        "Not started" => "未启动",
        "Reason: {reason}" => "原因：{reason}",
        "Copied to clipboard" => "已复制到剪贴板",
        "Opened link" => "已打开链接",
        "Link: {url}" => "链接：{url}",
        "Not available" => "不可用",
        "files changed  {count}" => "文件已修改  {count}",
        "active session" => "活动会话",
        "new-session draft" => "新会话草稿",
        "Tool approvals: {title} ({target})" => "工具审批：{title}（{target}）",
        "Disconnected · draft kept" => "连接已断开 · 草稿已保留",
        "The App Server closed this interface: {reason}" => "App Server 关闭了此界面：{reason}",
        "Disconnected; draft kept. Exit and resume this Session." => {
            "连接已断开，草稿已保留。请退出后恢复此会话。"
        }
        "Disconnected; draft kept. Exit and run: {command}" => {
            "连接已断开，草稿已保留。请退出后运行：{command}"
        }
        "Disconnected; command outcome unknown" => "连接已断开；命令结果未知",
        "Message queued" => "消息已加入队列",
        "Run a shell command · Esc to exit" => "输入 shell 命令 · Esc 退出",
        "! shell mode" => "! shell 模式",
        "!! shell mode · not sent to the model" => "!! shell 模式 · 不发送给模型",
        "Leave shell mode to quote a message" => "退出 shell 模式后才能引用消息",
        "A shell command is already running" => "已有 shell 命令在运行",
        "Running… · Ctrl+C to interrupt · {seconds}s" => "运行中… · Ctrl+C 中断 · {seconds}s",
        "exit code {code}" => "退出码 {code}",
        "killed by {signal}" => "被 {signal} 终止",
        "interrupted" => "已中断",
        "done" => "完成",
        "failed: {reason}" => "失败：{reason}",
        "Shell output was not recorded: {reason}" => "Shell 输出未记录：{reason}",
        "Shell: {reason}" => "Shell：{reason}",
        "Reading clipboard…" => "正在读取剪贴板…",
        "Reading image…" => "正在读取图片…",
        "Wait for the image insertion before sending" => "请等待图片插入后再发送",
        "The image could not be read" => "无法读取该图片",
        "An image is still being inserted; paste again when it is in" => {
            "图片正在插入，请稍后再粘贴"
        }
        "The clipboard is still being inserted; paste again when it is in" => {
            "剪贴板内容正在插入，请稍后再粘贴"
        }
        "Clipboard: {reason}" => "剪贴板：{reason}",
        "Wait for clipboard insertion before sending" => "请等待剪贴板内容插入后再发送",
        "A submission accepts at most 20 images" => "一次提交最多 20 张图片",
        "first frame" => "仅首帧",
        "re-encoded" => "已重新编码",
        "The current model does not accept images; remove {label} to send" => {
            "当前模型不支持图片输入；删除 {label} 后再发送"
        }
        "Files" => "文件",
        "Searching…" => "正在搜索…",
        "↑↓ select · enter/tab insert · esc close" => "↑↓ 选择 · enter/tab 插入 · esc 关闭",
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
        "Opened {url}" => "已打开 {url}",
        "Tool details not saved: {error}" => "工具详情未保存：{error}",
        "/{name} needs a session — send a message first" => "/{name} 需要会话，请先发一条消息",
        "Nothing is running" => "当前没有运行",
        "No queued message" => "没有队列消息",
        "Only in fullscreen (/tui)" => "仅在全屏界面可用（/tui）",
        "new session" => "新会话",
        "resumed {id} · {count} messages" => "已恢复 {id} · {count} 条消息",
        " · earlier history: /timeline" => " · 更早历史：/timeline",
        "Error: {message}" => "错误：{message}",
        "working…  {elapsed}" => "处理中…  {elapsed}",
        "thinking…  {elapsed}" => "思考中…  {elapsed}",
        "Ask anything..." => "输入任何内容...",
        "Earlier history · {id} · {count} messages" => "更早历史 · {id} · {count} 条消息",
        "No active session" => "没有活动会话",
        "answer required" => "需要回答",
        "permission required" => "需要授权",
        "esc interrupt" => "Esc 中断",
        "shift+← edit last · ctrl+s steer now" => "shift+← 编辑最后一条 · ctrl+s 立即 steer",
        "+{count} more · shift+← edit last · ctrl+s steer now" => {
            "还有 {count} 条 · shift+← 编辑最后一条 · ctrl+s 立即 steer"
        }
        "Queue ({count})" => "队列（{count}）",
        "Thinking…" => "思考中…",
        "Thought" => "已思考",
        "Thought for {duration}" => "已思考 {duration}",
        "Welcome to DIM {version}!" => "欢迎使用 DIM {version}！",
        "Type / for commands · Ctrl+O for the palette" => "输入 / 查看命令 · Ctrl+O 打开命令面板",
        "Workspace" => "工作区",
        "Git branch" => "Git 分支",
        "Bye!" => "再见！",
        "DIM Web is ready" => "DIM Web 已就绪",
        "Opened in your browser" => "已在浏览器中打开",
        "Open the sign-in link below" => "在浏览器中打开下面的登录链接",
        "Web" => "Web",
        "Sign-in link:" => "登录链接：",
        "Resume" => "回到终端",
        "Press Ctrl+C to stop the Web workspace." => "按 Ctrl+C 停止 Web 工作区。",
        "To resume this session:" => "恢复此会话：",

        "session" => "会话",
        "run" => "运行",
        "view" => "视图",
        "model" => "模型",
        "app" => "应用",
        "skill" => "技能",
        "start a new session" => "开始新会话",
        "show or set the reasoning effort" => "查看或设置推理强度",
        "show or set the priority service tier (on|off)" => "查看或设置优先服务层级（on|off）",
        "list agents or switch this session's agent" => "列出 Agent 或切换本会话的 Agent",
        "list MCP servers, sign in or out (login|logout ID)" => {
            "列出 MCP 服务器，登录或退出（login|logout ID）"
        }
        "resume a previous session" => "恢复之前的会话",
        "open this session in the browser" => "在浏览器中打开此会话",
        "show the context window usage" => "显示上下文窗口用量",
        "interrupt the run" => "中断运行",
        "steer with the next queued message" => "用下一条队列消息 steer",
        "scroll to the top" => "滚动到顶部",
        "scroll to the bottom" => "滚动到底部",
        "pick an earlier message to edit or quote" => "选择较早的消息进行编辑或引用",
        "collapse or expand tool details, thoughts and compaction summaries" => {
            "折叠或展开工具详情、思考与压缩摘要"
        }
        "switch between fullscreen and scrollback" => "在全屏和回滚界面之间切换",
        "switch theme" => "切换主题",
        "switch language" => "切换语言",
        "select a model" => "选择 model",
        "connect a provider" => "连接 provider",
        "sign in to DimAgent" => "登录 DimAgent",
        "DimAgent account · usage · sign out" => "DimAgent 账号 · 用量 · 退出登录",
        "configure tool approvals" => "配置工具审批",
        "choose ask or full access" => "选择询问或完全访问",
        "open settings" => "打开设置",
        "exit" => "退出应用",

        "Enter select · ←/→ effort · Ctrl+X clear saved · Esc clear/close" => {
            "Enter 选择 · ←/→ 推理强度 · Ctrl+X 清除保存的偏好 · Esc 清除/关闭"
        }
        "Enter select · Esc clear/close" => "Enter 选择 · Esc 清除/关闭",
        "esc close" => "Esc 关闭",
        "Suggestions" => "建议",
        "Skills" => "技能",
        "Commands" => "命令",
        "Command Palette" => "命令面板",
        "No matches" => "无匹配项",
        " (unavailable)" => "（不可用）",

        "[Text: {preview} ({count} chars)]" => "[文本：{preview}（{count} 字符）]",
        "[... {count} lines]" => "[... {count} 行]",
        "pasted text" => "粘贴的文本",

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
        "Execution" => "执行",
        "Models" => "模型",
        "Settings" => "设置",
        "↑↓ move · Enter select · ←→/Space change · Esc close" => {
            "↑↓ 移动 · Enter 选择 · ←→/Space 修改 · Esc 关闭"
        }

        "↑↓ preview · Enter save · Esc restore" => "↑↓ 预览 · Enter 保存 · Esc 恢复",
        "System default ({locale})" => "系统默认（{locale}）",

        "↑↓ preview · Enter apply · Esc cancel" => "↑↓ 预览 · Enter 应用 · Esc 取消",
        "System" => "系统",
        "Could not save the theme: {error}" => "无法保存主题：{error}",

        "Sessions" => "会话",
        "Enter open · Ctrl+R refresh · Esc close" => "Enter 打开 · Ctrl+R 刷新 · Esc 关闭",
        "Loading sessions..." => "正在加载会话...",
        "No saved sessions yet." => "暂无已保存的会话。",
        "No matching sessions." => "无匹配的会话。",
        "now" => "刚刚",
        "{count}m ago" => "{count}分钟前",
        "{count}h ago" => "{count}小时前",
        "{count}d ago" => "{count}天前",

        "Select a model" => "选择模型",
        "Loading models..." => "正在加载模型...",
        "No models available. Connect a provider first." => "无可用模型。请先连接 Provider。",
        "No matching models." => "无匹配的模型。",
        "current" => "当前",
        "Edit models" => "编辑模型",
        "Editing models is not available yet" => "暂不支持编辑模型",
        "{provider}: 1 model unavailable · dimcode models {id}" => {
            "{provider}：1 个模型不可用 · dimcode models {id}"
        }
        "{provider}: {count} models unavailable · dimcode models {id}" => {
            "{provider}：{count} 个模型不可用 · dimcode models {id}"
        }
        "{provider}: 1 model needs a window · dimcode models {id} --set-missing-limits" => {
            "{provider}：1 个模型需要窗口 · dimcode models {id} --set-missing-limits"
        }
        "{provider}: {count} models need a window · dimcode models {id} --set-missing-limits" => {
            "{provider}：{count} 个模型需要窗口 · dimcode models {id} --set-missing-limits"
        }
        "{provider}: no models · dimcode models {id} --refresh" => {
            "{provider}：没有模型 · dimcode models {id} --refresh"
        }
        "{provider}: no models · add a model with provider.models.create" => {
            "{provider}：没有模型 · 用 provider.models.create 添加模型"
        }
        "{provider}: {unavailable} unavailable and {disabled} disabled · dimcode models {id}" => {
            "{provider}：{unavailable} 个不可用、{disabled} 个已禁用 · dimcode models {id}"
        }
        "{provider}: catalog unreadable · dimcode models {id}" => {
            "{provider}：无法读取 catalog · dimcode models {id}"
        }
        "A model whose limits are undeclared cannot be selected; declare them with dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N." => {
            "上限未声明的模型无法选择；用 dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N 声明。"
        }
        "Declare them with dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N." => {
            "用 dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N 声明。"
        }
        "No cached catalog. Sign in and run models PROVIDER --refresh." => {
            "没有缓存的 catalog。请登录并运行 models PROVIDER --refresh。"
        }
        "Saved model preferences cleared" => "已清除保存的模型偏好",
        "{provider}: 1 model disabled · dimcode models {id}" => {
            "{provider}：1 个模型已禁用 · dimcode models {id}"
        }
        "{provider}: {count} models disabled · dimcode models {id}" => {
            "{provider}：{count} 个模型已禁用 · dimcode models {id}"
        }
        "No models. Run dimcode models {id} --refresh to list them." => {
            "没有模型。运行 dimcode models {id} --refresh 查看。"
        }
        "No models. The provider listed none." => "没有模型。该 Provider 没有列出任何模型。",
        "Enter select · Esc back" => "Enter 选择 · Esc 返回",
        "Subagent models" => "子代理模型",
        "Subagent models: {policy}" => "子代理模型：{policy}",
        "Fixed subagent model" => "固定子代理模型",
        "Agent choice (default)" => "由 Agent 决定（默认）",
        "Agent choice" => "由 Agent 决定",
        "Inherit requesting branch" => "继承发起请求的分支",
        "Fixed model" => "固定模型",
        "Applies to new members, including named Agents." => "适用于新成员，包括具名 Agent。",
        "Existing members keep their models." => "已有成员保留其模型。",
        "unknown" => "未知",
        "Subagent model policy saved" => "子代理模型策略已保存",
        "Subagent model policy changed elsewhere; choose again to reread it" => {
            "子代理模型策略已在别处更改；再次选择以重新读取"
        }
        "Subagent model policy: {error}" => "子代理模型策略：{error}",
        "drawing diagram… {count} lines" => "正在绘制图表…已收到 {count} 行",
        "Task complete" => "任务已完成",
        "Permission needed: {tool}" => "需要授权：{tool}",
        "Question waiting for your answer" => "有问题等你回答",
        "Notify when agent needs you" => "需要你时通知",
        "Show ACP sessions" => "显示 ACP 会话",
        "show the key bindings" => "显示快捷键",
        "Application keys" => "应用快捷键",
        "Enter submit or queue · empty draft: steer first queued · Shift+Enter or Ctrl+J newline · ↑↓ history" => {
            "Enter 提交或排队 · 草稿为空时 steer 第一条排队消息 · Shift+Enter 或 Ctrl+J 换行 · ↑↓ 历史"
        }
        "Ctrl+O or Ctrl+P commands · Esc interrupt, queued messages return · Ctrl+S steer draft or first queued · Shift+← edit last queued" => {
            "Ctrl+O 或 Ctrl+P 命令 · Esc 中断，排队消息回到输入框 · Ctrl+S steer 草稿或第一条排队消息 · Shift+← 编辑最后一条排队消息"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C 清空，连按两次退出 · 草稿为空时 Ctrl+D 退出"
        }
        "Fullscreen" => "全屏",
        "PageUp/PageDown scroll · Home/End top or bottom (empty draft) · Tab pick a message, e edit it, q quote it" => {
            "PageUp/PageDown 滚动 · Home/End 到顶部或底部 (草稿为空时) · Tab 选择消息，e 编辑，q 引用"
        }
        "wheel scroll · drag select and copy · click a link to open it, a tool group to expand it" => {
            "滚轮滚动 · 拖动选择并复制 · 点击链接打开，点击工具组展开"
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
        "Compacting context…" => "正在压缩上下文…",
        "Context compacted" => "上下文已压缩",
        "Context compaction skipped" => "上下文压缩已跳过",
        "Context compaction failed" => "上下文压缩失败",
        "Context compaction cancelled" => "上下文压缩已取消",
        "near the compaction threshold" => "接近压缩阈值",
        "This daemon does not support context compaction" => "此守护进程不支持上下文压缩",
        "compact the context of the shown line" => "压缩当前显示线的上下文",
        "follow or change the shown line's goal" => "跟随或修改当前显示线的目标",
        "◎ goal active" => "◎ 目标进行中",
        "◎ goal paused" => "◎ 目标已暂停",
        "Kept verbatim" => "原文保留",
        "Tool effects" => "工具操作记录",
        "insufficient space: {parts}" => "空间不足：{parts}",
        "system prompt" => "系统提示词",
        "tools" => "工具",
        "appended messages" => "附加消息",
        "keyed baseline" => "带 key 的基线",
        "pinned inputs" => "钉住的输入",
        "digest" => "清单",
        "summary" => "摘要",
        _ => return None,
    })
}
