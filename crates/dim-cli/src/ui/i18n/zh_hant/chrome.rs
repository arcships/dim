pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // tui.rs
        "Enter output / interrupt member · x stop command · Esc close" => {
            "Enter 輸出/中斷成員 · x 停止指令 · Esc 關閉"
        }
        "Enter output / interrupt member · x stop command · Ctrl+X stop all · Esc close" => {
            "Enter 輸出/中斷成員 · x 停止指令 · Ctrl+X 全部停止 · Esc 關閉"
        }
        "Loading branch history... Stop-all unavailable" => "正在讀取分支歷程… 暫無法全部停止",
        "Branch history failed; reopen to retry" => "讀取分支歷程失敗；重新開啟以重試",
        "Branch history is incomplete; stop-all unavailable" => "分支歷程尚未讀完，暫無法全部停止",
        "Running" => "執行中",
        "Lines" => "線",
        "Enter show the line · x end it · Esc close" => "Enter 查看該線 · x 結束 · Esc 關閉",
        "This session has no lines." => "這個工作階段還沒有線。",
        "No matching lines." => "沒有符合的線。",
        "the session itself" => "工作階段本身",
        "working" => "工作中",
        "ended itself" => "自行結束",
        "ended by an operation" => "由操作結束",
        "ended with {id}" => "隨 {id} 結束",
        "creation failed" => "建立失敗",
        "shown" => "目前顯示",
        "line" => "線",
        "line {id}" => "線 {id}",
        "jump to a message of the shown line" => "跳到目前線的某則訊息",
        "Timeline" => "時間線",
        "Enter jump · e edit · q quote · b branch · f fork · Esc close" => {
            "Enter 跳轉 · e 編輯 · q 引用 · b 分支 · f 派生 · Esc 關閉"
        }
        "No messages yet." => "還沒有訊息。",
        "Already on screen above; /tui jumps to it" => "已在上方；/tui 可直接跳轉",
        "{count} earlier messages · Enter or click to load" => {
            "較早的 {count} 則訊息 · Enter 或點擊載入"
        }
        "follow, talk to and end this session's lines" => "查看、對話並結束此工作階段的線",
        "End line" => "結束線",
        "y/N" => "y/N",
        "main is the session itself; a line ends, main does not" => {
            "main 就是工作階段本身；線可以結束，main 不能"
        }
        "Line has ended:" => "該線已結束：",
        "Esc close" => "Esc 關閉",
        "/{name} needs {argument}" => "/{name} 需要 {argument}",
        "/{name} takes text only; remove {label} to send" => {
            "/{name} 只接受文字；移除 {label} 後再傳送"
        }
        "e edit · b branch · f fork" => "e 編輯 · b 分支 · f 派生",
        "e edit" => "e 編輯",
        "Fork the session before “{message}”? (y/N)" => "在「{message}」之前派生會話？(y/N)",
        "Fork the session after the last turn? (y/N)" => "在最近一個回合之後派生會話？(y/N)",
        "⑂ new line from {id} · Esc cancel" => "⑂ 從 {id} 開新線 · Esc 取消",
        "A fork copies main; b opens a line here" => "派生只複製 main；按 b 在這裡開線",
        "A line opens from text; images and files were not included" => {
            "線只用文字開場；圖片和檔案未包含在內"
        }
        "No finished turn to cut at yet" => "還沒有已結束的回合可以切分",
        "forked {id} · {count} messages" => "已派生 {id} · {count} 則訊息",
        "after message {count}" => "第 {count} 則訊息之後",
        "open a line from the end of the shown line" => "從目前線的末尾開一條線",
        "fork the session at its last finished turn" => "在最近結束的回合處派生會話",
        "Starting" => "啟動中",
        "Delegation result" => "委派結果",
        "Enter details · x stop selected · Ctrl+X stop all · Esc close" => {
            "Enter 詳細資料 · x 停止所選 · Ctrl+X 全部停止 · Esc 關閉"
        }
        "x interrupt member · Esc back" => "x 中斷成員 · Esc 返回",
        "read-only" => "唯讀",
        "Member Session" => "成員工作階段",
        "Loading conversation..." => "正在讀取對話…",
        "No conversation yet" => "尚無對話內容",
        "Observing conversation" => "正在檢視對話",
        "Conversation unavailable" => "對話無法使用",
        "Session closed" => "工作階段已關閉",
        "Member conversation" => "成員對話",
        "x stop command · Esc back" => "x 停止指令 · Esc 返回",
        "Esc back" => "Esc 返回",
        "Tail preview - last 2,000 lines / 50 KiB" => "結尾預覽 - 最後 2,000 行 / 50 KiB",
        "Loading output..." => "正在讀取輸出…",
        "No output yet" => "尚無輸出",
        "Live output unavailable" => "即時輸出無法使用",
        "Live output unavailable in this profile" => "此設定檔不提供即時輸出",
        "Output read failed" => "讀取輸出失敗",
        "Completed" => "已完成",
        "Failed" => "失敗",
        "Cancelled" => "已取消",
        "workflow" => "工作流程",
        "Stopping" => "正在停止",
        "Stopped" => "已停止",
        "Interrupted" => "已中斷",
        "last observed" => "最後觀測",
        "Loading Workflow runs... Stop-all unavailable" => "正在讀取工作流程執行… 暫無法全部停止",
        "Workflow: last observed; refresh failed, reopen to retry" => {
            "工作流程：顯示最後結果；重新整理失敗，請重新開啟"
        }
        "Workflow runs unavailable; reopen to retry" => "無法取得工作流程執行；請重新開啟",
        "The selected Workflow run is no longer available" => "所選工作流程執行已失效",
        "Workflow inventory is incomplete; stop-all unavailable" => {
            "工作流程清單尚未讀完，暫無法全部停止"
        }
        "Workflow stop unavailable" => "無法停止工作流程",
        "Run no longer available; last observed record" => "執行已無法使用；顯示最後觀測紀錄",
        "x stop workflow · Esc back" => "x 停止工作流程 · Esc 返回",
        "Run" => "執行",
        "Source" => "來源",
        "Stages" => "階段",
        "result" => "結果",
        "Log" => "記錄",
        "Report" => "報告",
        "waiting" => "等待中",
        "launching" => "啟動中",
        "completed" => "已完成",
        "failed" => "失敗",
        "stopped" => "已停止",
        "skipped" => "已略過",
        "lost" => "已遺失",
        "none" => "無",
        "pending" => "待傳送",
        "delivered" => "已送達",
        "undeliverable" => "無法送達",
        "Agent is starting; no member Session to stop yet" => {
            "智慧代理正在啟動，尚無可停止的成員工作階段"
        }
        "Provider members are controlled by the leader; no stop here" => {
            "供應商成員由主代理控制，無法在此停止"
        }
        "Provider members have no conversation to open here" => "供應商成員沒有可在此開啟的對話",
        "That delegation is no longer in this transcript" => "該委派已不在此對話紀錄中",
        "Member model" => "成員模型",
        "No model observed" => "尚未觀測到模型",
        "Observing member..." => "正在觀測成員…",
        "{model} (observation unavailable)" => "{model}（無法觀測）",
        "{model} (Session closed)" => "{model}（工作階段已關閉）",
        "{model} (last observed)" => "{model}（最後觀測）",
        "Agent: {name}" => "智慧代理：{name}",
        "Provider member: {name}" => "供應商成員：{name}",
        "Task: {task}" => "任務：{task}",
        "Activity: {activity}" => "活動：{activity}",
        "Activity: not observed on this attachment" => "活動：本次連線未觀測到",
        "Result: {result}" => "結果：{result}",
        "Controlled by the leader" => "由主代理控制",
        "› Open member conversation" => "› 開啟成員對話",
        "Not started" => "未啟動",
        "Reason: {reason}" => "原因：{reason}",
        "Copied to clipboard" => "已複製到剪貼簿",
        "Opened link" => "已開啟連結",
        "Link: {url}" => "連結：{url}",
        "Not available" => "無法使用",
        "files changed  {count}" => "已變更檔案  {count}",
        "active session" => "目前工作階段",
        "new-session draft" => "新工作階段草稿",
        "Tool approvals: {title} ({target})" => "工具核准：{title}（{target}）",
        "Disconnected · draft kept" => "連線已中斷 · 草稿已保留",
        "The App Server closed this interface: {reason}" => "App Server 已關閉此介面：{reason}",
        "Disconnected; draft kept. Exit and resume this Session." => {
            "連線已中斷，草稿已保留。請離開後繼續此工作階段。"
        }
        "Disconnected; draft kept. Exit and run: {command}" => {
            "連線已中斷，草稿已保留。請離開後執行：{command}"
        }
        "Disconnected; command outcome unknown" => "連線已中斷；指令結果不明",
        "Message queued" => "訊息已加入佇列",
        "Run a shell command · Esc to exit" => "執行 shell 指令 · Esc 離開",
        "! shell mode" => "! shell 模式",
        "!! shell mode · not sent to the model" => "!! shell 模式 · 不傳送給模型",
        "Leave shell mode to quote a message" => "離開 shell 模式後才能引用訊息",
        "A shell command is already running" => "已有 shell 指令在執行",
        "Running… · Ctrl+C to interrupt · {seconds}s" => "執行中… · Ctrl+C 中斷 · {seconds}s",
        "exit code {code}" => "結束代碼 {code}",
        "killed by {signal}" => "被 {signal} 終止",
        "interrupted" => "已中斷",
        "done" => "完成",
        "failed: {reason}" => "失敗：{reason}",
        "Shell output was not recorded: {reason}" => "未記錄 Shell 輸出：{reason}",
        "Shell: {reason}" => "Shell：{reason}",
        "Reading clipboard…" => "正在讀取剪貼簿…",
        "Reading image…" => "正在讀取圖片…",
        "Wait for the image insertion before sending" => "請等待圖片插入後再傳送",
        "The image could not be read" => "無法讀取該圖片",
        "An image is still being inserted; paste again when it is in" => {
            "圖片正在插入，請稍後再貼上"
        }
        "The clipboard is still being inserted; paste again when it is in" => {
            "剪貼簿內容正在插入，請稍後再貼上"
        }
        "Clipboard: {reason}" => "剪貼簿：{reason}",
        "Wait for clipboard insertion before sending" => "請等剪貼簿內容插入後再傳送",
        "A submission accepts at most 20 images" => "每次送出最多 20 張圖片",
        "first frame" => "僅首幀",
        "re-encoded" => "已重新編碼",
        "The current model does not accept images; remove {label} to send" => {
            "目前的模型不支援圖片；請移除 {label} 再傳送"
        }
        "Files" => "檔案",
        "Searching…" => "正在搜尋…",
        "↑↓ select · enter/tab insert · esc close" => "↑↓ 選擇 · enter/tab 插入 · esc 關閉",
        "not sent to the model" => "未傳送給模型",
        "… {count} more lines" => "… 還有 {count} 行",
        "… {count} earlier lines" => "… 前面還有 {count} 行",
        "Interrupt requested; Ctrl+C again to exit" => "已要求中斷；再按 Ctrl+C 離開",
        "Press Ctrl+C again to exit" => "再按一次 Ctrl+C 離開",
        "User interrupted the run." => "使用者已中斷本次執行。",
        "Could not save the language: {error}" => "無法儲存語言：{error}",
        "Language set to {locale}" => "語言已設為 {locale}",
        "Language set to system default" => "語言已設為系統預設",
        "Connected {provider}" => "已連線 {provider}",
        "Opened {url}" => "已開啟 {url}",
        "Tool details not saved: {error}" => "工具詳情未儲存：{error}",
        "/{name} needs a session — send a message first" => "/{name} 需要工作階段，請先傳送訊息",
        "Nothing is running" => "沒有執行中項目",
        "No queued message" => "佇列中沒有訊息",
        "Only in fullscreen (/tui)" => "僅限全螢幕模式（/tui）",
        "new session" => "新工作階段",
        "resumed {id} · {count} messages" => "已繼續 {id} · {count} 則訊息",
        " · earlier history: /timeline" => " · 較早歷程：/timeline",
        "Error: {message}" => "錯誤：{message}",
        "working…  {elapsed}" => "處理中…  {elapsed}",
        "thinking…  {elapsed}" => "思考中…  {elapsed}",
        "Ask anything..." => "問點什麼…",
        "Earlier history · {id} · {count} messages" => "較早歷程 · {id} · {count} 則訊息",
        "No active session" => "無目前工作階段",
        "answer required" => "需要回答",
        "permission required" => "需要授權",
        "esc interrupt" => "esc 中斷",
        "↑ edit · ctrl+s steer now" => "↑ 編輯 · ctrl+s 立即 steer",
        "+{count} more · ↑ edit · ctrl+s steer now" => {
            "還有 {count} 則 · ↑ 編輯 · ctrl+s 立即 steer"
        }
        "Queue ({count})" => "佇列（{count}）",
        "Thinking…" => "思考中…",
        "Thought" => "已思考",
        "Thought for {duration}" => "已思考 {duration}",
        "Welcome to DIM {version}!" => "歡迎使用 DIM {version}！",
        "Type / for commands · Ctrl+O for the palette" => "輸入 / 查看指令 · Ctrl+O 開啟命令面板",
        "Workspace" => "工作區",
        "Git branch" => "Git 分支",
        "Bye!" => "再見！",
        "DIM Web is ready" => "DIM Web 已就緒",
        "Opened in your browser" => "已在瀏覽器中開啟",
        "Open the sign-in link below" => "請開啟下方的登入連結",
        "Web" => "Web",
        "Sign-in link:" => "登入連結：",
        "Resume" => "回到終端機",
        "Press Ctrl+C to stop the Web workspace." => "按 Ctrl+C 停止 Web 工作區。",
        "To resume this session:" => "繼續此工作階段：",

        // composer/commands.rs: categories
        "session" => "工作階段",
        "run" => "執行",
        "view" => "檢視",
        "model" => "模型",
        "app" => "應用程式",
        "skill" => "技能",
        // composer/commands.rs: descriptions
        "start a new session" => "開始新工作階段",
        "show or set the reasoning effort" => "檢視或設定推理強度",
        "show or set the priority service tier (on|off)" => "檢視或設定優先服務層級（on|off）",
        "list agents or switch this session's agent" => "列出 Agent 或切換此工作階段的 Agent",
        "list MCP servers, sign in or out (login|logout ID)" => {
            "列出 MCP 伺服器，登入或登出（login|logout ID）"
        }
        "resume a previous session" => "繼續先前工作階段",
        "open this session in the browser" => "在瀏覽器中開啟此工作階段",
        "show the context window usage" => "顯示上下文視窗用量",
        "interrupt the run" => "中斷執行",
        "steer with the next queued message" => "以下一則佇列訊息 steer",
        "scroll to the top" => "捲動到頂端",
        "scroll to the bottom" => "捲動到底端",
        "pick an earlier message to edit or quote" => "選擇較早的訊息來編輯或引用",
        "collapse or expand tool details, thoughts and compaction summaries" => {
            "收合或展開工具詳情、思考與壓縮摘要"
        }
        "switch between fullscreen and scrollback" => "在全螢幕與回捲模式間切換",
        "switch theme" => "切換主題",
        "switch language" => "切換語言",
        "select a model" => "選擇模型",
        "connect a provider" => "連線供應商",
        "sign in to DimAgent" => "登入 DimAgent",
        "DimAgent account · usage · sign out" => "DimAgent 帳號 · 用量 · 登出",
        "configure tool approvals" => "設定工具核准",
        "choose ask or full access" => "選擇詢問或完整存取",
        "open settings" => "開啟設定",
        "suspend to the shell" => "暫停到 Shell",
        "exit" => "離開",

        // composer/palette.rs
        "Enter select · ←/→ effort · Ctrl+X clear saved · Esc clear/close" => {
            "Enter 選擇 · ←/→ 推理強度 · Ctrl+X 清除已儲存 · Esc 清除/關閉"
        }
        "Enter select · Esc clear/close" => "Enter 選擇 · Esc 清除/關閉",
        "esc close" => "esc 關閉",
        "Suggestions" => "建議",
        "Skills" => "技能",
        "Commands" => "指令",
        "Command Palette" => "命令面板",
        "No matches" => "無相符項目",
        " (unavailable)" => "（無法使用）",

        // composer/mod.rs
        "[Text: {preview} ({count} chars)]" => "[文字：{preview}（{count} 個字元）]",
        "[... {count} lines]" => "[… {count} 行]",
        "pasted text" => "貼上的文字",

        // list_panel.rs

        // settings_panel.rs
        "on" => "開啟",
        "off" => "關閉",
        "Interface" => "介面",
        "Theme" => "主題",
        "Language" => "語言",
        "Startup UI form" => "啟動介面形式",
        "fullscreen" => "全螢幕",
        "scrollback" => "回捲",
        "Tool details" => "工具詳情",
        "expanded" => "展開",
        "collapsed" => "收合",
        "Mouse (fullscreen)" => "滑鼠（全螢幕）",
        "Permissions" => "權限",
        "Tool approvals" => "工具核准",
        "Execution" => "執行",
        "Models" => "模型",
        "Settings" => "設定",
        "↑↓ move · Enter select · ←→/Space change · Esc close" => {
            "↑↓ 移動 · Enter 選擇 · ←→/Space 變更 · Esc 關閉"
        }

        // language_panel.rs
        "↑↓ preview · Enter save · Esc restore" => "↑↓ 預覽 · Enter 儲存 · Esc 還原",
        "System default ({locale})" => "系統預設（{locale}）",

        // theme_panel.rs
        "↑↓ preview · Enter apply · Esc cancel" => "↑↓ 預覽 · Enter 套用 · Esc 取消",
        "System" => "系統",
        "Could not save the theme: {error}" => "無法儲存主題：{error}",

        // sessions_panel.rs
        "Sessions" => "工作階段",
        "Enter open · Ctrl+R refresh · Esc close" => "Enter 開啟 · Ctrl+R 重新整理 · Esc 關閉",
        "Loading sessions..." => "正在載入工作階段…",
        "No saved sessions yet." => "尚無已儲存的工作階段。",
        "No matching sessions." => "無相符工作階段。",
        "now" => "剛剛",
        "{count}m ago" => "{count} 分鐘前",
        "{count}h ago" => "{count} 小時前",
        "{count}d ago" => "{count} 天前",

        // models_panel.rs
        "Select a model" => "選擇模型",
        "Loading models..." => "正在載入模型…",
        "No models available. Connect a provider first." => "沒有可用的模型。請先連線供應商。",
        "No matching models." => "沒有相符的模型。",
        "current" => "目前",
        "Edit models" => "編輯模型",
        "Editing models is not available yet" => "尚不支援編輯模型",
        "{provider}: 1 model unavailable · dimcode models {id}" => {
            "{provider}：1 個模型無法使用 · dimcode models {id}"
        }
        "{provider}: {count} models unavailable · dimcode models {id}" => {
            "{provider}：{count} 個模型無法使用 · dimcode models {id}"
        }
        "{provider}: 1 model needs a window · dimcode models {id} --set-missing-limits" => {
            "{provider}：1 個模型需要視窗 · dimcode models {id} --set-missing-limits"
        }
        "{provider}: {count} models need a window · dimcode models {id} --set-missing-limits" => {
            "{provider}：{count} 個模型需要視窗 · dimcode models {id} --set-missing-limits"
        }
        "{provider}: no models · dimcode models {id} --refresh" => {
            "{provider}：沒有模型 · dimcode models {id} --refresh"
        }
        "{provider}: no models · add a model with provider.models.create" => {
            "{provider}：沒有模型 · 用 provider.models.create 新增模型"
        }
        "{provider}: {unavailable} unavailable and {disabled} disabled · dimcode models {id}" => {
            "{provider}：{unavailable} 個不可用、{disabled} 個已停用 · dimcode models {id}"
        }
        "{provider}: catalog unreadable · dimcode models {id}" => {
            "{provider}：無法讀取 catalog · dimcode models {id}"
        }
        "A model whose limits are undeclared cannot be selected; declare them with dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N." => {
            "上限未宣告的模型無法選擇；用 dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N 宣告。"
        }
        "Declare them with dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N." => {
            "用 dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N 宣告。"
        }
        "No cached catalog. Sign in and run models PROVIDER --refresh." => {
            "沒有快取的 catalog。請登入並執行 models PROVIDER --refresh。"
        }
        "Saved model preferences cleared" => "已清除儲存的模型偏好",
        "{provider}: 1 model disabled · dimcode models {id}" => {
            "{provider}：1 個模型已停用 · dimcode models {id}"
        }
        "{provider}: {count} models disabled · dimcode models {id}" => {
            "{provider}：{count} 個模型已停用 · dimcode models {id}"
        }
        "No models. Run dimcode models {id} --refresh to list them." => {
            "沒有模型。執行 dimcode models {id} --refresh 查看。"
        }
        "No models. The provider listed none." => "沒有模型。該 Provider 沒有列出任何模型。",
        "Enter select · Esc back" => "Enter 選擇 · Esc 返回",
        "Subagent models" => "子代理模型",
        "Subagent models: {policy}" => "子代理模型：{policy}",
        "Fixed subagent model" => "固定子代理模型",
        "Agent choice (default)" => "由 Agent 決定（預設）",
        "Agent choice" => "由 Agent 決定",
        "Inherit requesting branch" => "沿用發出要求的分支",
        "Fixed model" => "固定模型",
        "Applies to new members, including named Agents." => "適用於新成員，包括具名 Agent。",
        "Existing members keep their models." => "既有成員保留原模型。",
        "unknown" => "不明",
        "Subagent model policy saved" => "已儲存子代理模型原則",
        "Subagent model policy changed elsewhere; choose again to reread it" => {
            "子代理模型原則已在他處變更；請重新選擇"
        }
        "Subagent model policy: {error}" => "子代理模型原則：{error}",
        "drawing diagram… {count} lines" => "正在繪製圖表… {count} 行",
        // notify.rs
        "Task complete" => "任務已完成",
        "Permission needed: {tool}" => "需要授權：{tool}",
        "Question waiting for your answer" => "有問題等你回答",
        "Notify when agent needs you" => "需要你時通知",
        // keys.rs
        "show the key bindings" => "顯示快速鍵",
        "Application keys" => "應用程式快速鍵",
        "Enter submit · Shift+Enter or Ctrl+J newline · ↑↓ history" => {
            "Enter 送出 · Shift+Enter 或 Ctrl+J 換行 · ↑↓ 歷程"
        }
        "Ctrl+O or Ctrl+P commands · Esc interrupt or clear · Ctrl+S steer queued · ↑ edit queued" => {
            "Ctrl+O 或 Ctrl+P 指令 · Esc 中斷或清除 · Ctrl+S steer 佇列訊息 · ↑ 編輯佇列訊息"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C 清除，按兩次離開 · 草稿為空時 Ctrl+D 離開"
        }
        "Fullscreen" => "全螢幕",
        "PageUp/PageDown scroll · Home/End top or bottom (empty draft) · Tab pick a message, e edit it, q quote it" => {
            "PageUp/PageDown 捲動 · Home/End 至頂端或底端 (草稿為空時) · Tab 選擇訊息，e 編輯，q 引用"
        }
        "wheel scroll · drag select and copy · click a link to open it, a tool group to expand it" => {
            "滾輪捲動 · 拖曳選取並複製 · 點擊連結開啟，點擊工具群組展開"
        }
        "mouse off (/settings): the terminal selects and copies" => {
            "滑鼠已關閉（/settings）：由終端機選取與複製"
        }
        "Terminal-owned history" => "由終端機管理的歷程",
        "scroll: {value}" => "捲動：{value}",
        "search: {value}" => "搜尋：{value}",
        "copy: {value}" => "複製：{value}",
        "terminal search" => "終端機搜尋",
        "terminal scrollback" => "終端機回捲",
        "terminal copy" => "終端機複製",
        "drag to select" => "拖曳以選取",
        "drag or {shortcut}" => "拖曳或 {shortcut}",
        "Compacting context…" => "正在壓縮上下文…",
        "Context compacted" => "上下文已壓縮",
        "Context compaction skipped" => "上下文壓縮已跳過",
        "Context compaction failed" => "上下文壓縮失敗",
        "Context compaction cancelled" => "上下文壓縮已取消",
        "near the compaction threshold" => "接近壓縮閾值",
        "This daemon does not support context compaction" => "此守護進程不支援上下文壓縮",
        "compact the context of the shown line" => "壓縮目前顯示線的上下文",
        "Kept verbatim" => "原文保留",
        "Tool effects" => "工具操作紀錄",
        "insufficient space: {parts}" => "空間不足：{parts}",
        "system prompt" => "系統提示詞",
        "tools" => "工具",
        "appended messages" => "附加訊息",
        "keyed baseline" => "帶 key 的基線",
        "pinned inputs" => "釘住的輸入",
        "digest" => "清單",
        "summary" => "摘要",
        _ => return None,
    })
}
