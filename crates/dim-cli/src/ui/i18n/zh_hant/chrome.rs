pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Queue" => "佇列",
        "Export session" => "匯出工作階段",
        "Enter steer · x delete held · Esc close" => "Enter steer · x 刪除暫存訊息 · Esc 關閉",
        "No queued messages." => "佇列中沒有訊息。",
        "Held · awaiting application" => "暫存中 · 等待套用",
        "Unread" => "未讀取",
        "Held" => "暫存中",
        "Enter export · Esc cancel" => "Enter 匯出 · Esc 取消",
        "Messages with attachments stay queued; use /queue to steer or delete them" => {
            "附有附件的訊息會留在佇列中；使用 /queue 對其執行 steer 或刪除"
        }
        "/queue · Shift+← edit last · empty: {key} steer · Ctrl+Enter send now" => {
            "/queue · Shift+← 編輯最後一則 · 草稿為空時按 {key} steer · Ctrl+Enter 立即傳送"
        }
        "+{count} more · {hint}" => "還有 {count} 則 · {hint}",
        "steer or delete queued messages" => "對佇列訊息執行 steer 或刪除",
        "export this conversation" => "匯出此對話",
        "That message has already left the queue." => "該訊息已離開佇列。",
        "Exported {format}: {path}" => "已匯出 {format}：{path}",
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
        "Enter actions · Esc close" => "Enter 操作 · Esc 關閉",
        "Timeline actions" => "時間線操作",
        "↑↓ select · Enter confirm · Esc cancel" => "↑↓ 選擇 · Enter 確認 · Esc 取消",
        "Jump to message" => "跳轉到訊息",
        "Edit as a new draft" => "編輯為新草稿",
        "Quote message" => "引用訊息",
        "Create branch from here" => "從這裡建立分支",
        "Before this message" => "在這則訊息之前",
        "Fork session" => "派生會話",
        "1. Create fork" => "1. 建立派生會話",
        "2. Cancel" => "2. 取消",
        "Creates a new session; the current session stays unchanged." => {
            "建立新會話，目前會話保持不變。"
        }
        "No messages yet." => "還沒有訊息。",
        "Already on screen above; /tui jumps to it" => "已在上方；/tui 可直接跳轉",
        "{count} earlier messages · Enter or click to load" => {
            "較早的 {count} 則訊息 · Enter 或點擊載入"
        }
        "follow, talk to and end this session's lines" => "查看、對話並結束此工作階段的線",
        "End line {id}?" => "結束線 {id}？",
        "y end it" => "y 結束",
        "n cancel" => "n 取消",
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
        "Fork the session before “{message}”?" => "在「{message}」之前派生會話？",
        "Fork the session after the last turn?" => "在最近一個回合之後派生會話？",
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
        "rename this session" => "重新命名此工作階段",
        "Renamed to {title}" => "已重新命名為 {title}",
        "The Session is now untitled." => "工作階段現在沒有標題。",
        "This daemon does not support renaming Sessions" => "此守護進程不支援重新命名工作階段",
        "Renaming failed" => "重新命名失敗",
        "Delete \"{name}\"?" => "刪除「{name}」？",
        "Deleted {name}." => "已刪除 {name}。",
        "Nothing was removed: no stored Session {name}." => {
            "沒有刪除任何內容：沒有已儲存的工作階段 {name}。"
        }
        "Delete failed" => "刪除失敗",
        "This daemon does not support deleting Sessions" => "此守護進程不支援刪除工作階段",
        "Starting" => "啟動中",
        "agent result" => "子 agent 結果",
        "Read-only: you can watch this agent or stop it." => "唯讀：可以查看這個 agent，或終止它。",
        "Read-only: this agent has finished." => "唯讀：這個 agent 已結束。",
        "Esc back · x stop agent · ↑↓ PgUp PgDn scroll · Home/End jump" => {
            "Esc 返回 · x 終止 agent · ↑↓ PgUp PgDn 捲動 · Home/End 跳轉"
        }
        "Esc back · ↑↓ PgUp PgDn scroll · Home/End jump" => {
            "Esc 返回 · ↑↓ PgUp PgDn 捲動 · Home/End 跳轉"
        }
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
        "This session is closed and cannot be resumed. Read its stored history with dimcode export {id} using the same --home." => {
            "此工作階段已關閉，無法繼續對話。使用相同的 --home 執行 dimcode export {id} 可讀取已儲存的歷史。"
        }
        "Member conversation" => "成員對話",
        "x stop command · Esc back" => "x 停止指令 · Esc 返回",
        "x stop command · Esc close" => "x 停止指令 · Esc 關閉",
        "x stop workflow · Esc close" => "x 停止工作流程 · Esc 關閉",
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
        "Cannot open this Session; draft kept." => "無法開啟此工作階段，草稿已保留。",
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
        "type filter · enter select · esc clear/close" => "輸入篩選 · enter 選擇 · esc 清除/關閉",
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
        "Thinking details not saved: {error}" => "思考詳情未儲存：{error}",
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
        "Ask anything... · Ctrl+J newline" => "問點什麼… · Ctrl+J 換行",
        "Earlier history · {id} · {count} messages" => "較早歷程 · {id} · {count} 則訊息",
        "No active session" => "無目前工作階段",
        "answer required" => "需要回答",
        "permission required" => "需要授權",
        "esc interrupt" => "esc 中斷",
        "shift+← edit last · ctrl+s steer now" => "shift+← 編輯最後一則 · ctrl+s 立即 steer",
        "+{count} more · shift+← edit last · ctrl+s steer now" => {
            "還有 {count} 則 · shift+← 編輯最後一則 · ctrl+s 立即 steer"
        }
        "Queue ({count})" => "佇列（{count}）",
        "Thinking…" => "正在思考…",
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
        "Feedback or ideas? Join us on Discord:" => "有意見或想法？加入我們的 Discord：",
        "To resume this session:" => "繼續此工作階段：",

        // commands.rs: descriptions
        "start a new session" => "開始新工作階段",
        "show or set the reasoning effort" => "檢視或設定推理強度",
        "show or set the priority service tier (on|off)" => "檢視或設定優先服務層級（on|off）",
        "list agents or switch this session's agent" => "列出 Agent 或切換此工作階段的 Agent",
        "list MCP servers, sign in or out (login|logout ID)" => {
            "列出 MCP 伺服器，登入或登出（login|logout ID）"
        }
        "resume a previous session" => "繼續先前工作階段",
        "turn the web interface on or off" => "開啟或關閉 Web 介面",
        "show the context window usage" => "顯示上下文視窗用量",
        "interrupt the run" => "中斷執行",
        "steer with the next queued message" => "以下一則佇列訊息 steer",
        "scroll to the top" => "捲動到頂端",
        "scroll to the bottom" => "捲動到底端",
        "pick an earlier message to edit or quote" => "選擇較早的訊息來編輯或引用",
        "cycle tool details and compaction summaries" => "循環切換工具詳情與壓縮摘要",
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
        "exit" => "離開",

        // composer/palette.rs
        "Default for new sessions in {folder}: {model}" => {
            "新工作階段在 {folder} 的預設模型：{model}"
        }
        "Default for new sessions: {model}" => "新工作階段的預設模型：{model}",
        "Enter select · Esc clear/close" => "Enter 選擇 · Esc 清除/關閉",
        "esc close" => "esc 關閉",
        "Suggestions" => "建議",
        "Skills" => "技能",
        "Skill loaded: {name}" => "已載入技能：{name}",
        "Hook context · {count} line" => "鉤子上下文 · {count} 行",
        "Hook context · {count} lines" => "鉤子上下文 · {count} 行",
        "Stop hook follow-up · {count} line" => "Stop 鉤子後續輸入 · {count} 行",
        "Stop hook follow-up · {count} lines" => "Stop 鉤子後續輸入 · {count} 行",
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
        "Default send mode" => "預設傳送模式",
        "fullscreen" => "全螢幕",
        "scrollback" => "回捲",
        "Tool details" => "工具詳情",
        "Thinking details" => "思考詳情",
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
        "Sessions · This workspace" => "工作階段 · 目前工作區",
        "Sessions · This repository" => "工作階段 · 目前儲存庫",
        "Sessions · All workspaces" => "工作階段 · 所有工作區",
        "No saved sessions in this workspace. Ctrl+S shows more." => {
            "目前工作區沒有已儲存的工作階段。Ctrl+S 顯示更多。"
        }
        "Enter open · Ctrl+X delete · Ctrl+S scope · Ctrl+R refresh · Esc close" => {
            "Enter 開啟 · Ctrl+X 刪除 · Ctrl+S 切換範圍 · Ctrl+R 重新整理 · Esc 關閉"
        }
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
        "Previous page" => "上一頁",
        "Next page" => "下一頁",
        "No models available. Connect a provider first." => "沒有可用的模型。請先連線供應商。",
        "No matching models." => "沒有相符的模型。",
        "current" => "目前",
        "Edit models" => "編輯模型",
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
        "Conversation model" => "對話模型",
        "Selected: {model}" => "已選擇：{model}",
        "Model management" => "模型管理",
        "Manage models" => "管理模型",
        "1 provider" => "1 個 Provider",
        "{count} providers" => "{count} 個 Provider",
        "ready" => "就緒",
        "no models" => "沒有模型",
        "catalog unreadable" => "無法讀取 catalog",
        "{count} with missing limits" => "{count} 個缺少上限",
        "{count} unavailable" => "{count} 個不可用",
        "{count} disabled" => "{count} 個已停用",
        "{count} without models" => "{count} 個沒有模型",
        "{count} unreadable" => "{count} 個無法讀取",
        "{unavailable} unavailable, {disabled} disabled" => {
            "{unavailable} 個不可用、{disabled} 個已停用"
        }
        "Enter select + default · ←/→ effort · Tab section · Esc clear/close" => {
            "Enter 選擇並設為預設 · ←/→ 推理強度 · Tab 切換區段 · Esc 清除/關閉"
        }
        "Enter change · Tab section · Esc clear/close" => {
            "Enter 變更 · Tab 切換區段 · Esc 清除/關閉"
        }
        "Enter manage · Tab section · Esc clear/close" => {
            "Enter 管理 · Tab 切換區段 · Esc 清除/關閉"
        }
        "Enter manage · Esc back" => "Enter 管理 · Esc 返回",
        "unknown" => "不明",
        "Subagent model policy saved" => "已儲存子代理模型原則",
        "Subagent model policy changed elsewhere; choose again to reread it" => {
            "子代理模型原則已在他處變更；請重新選擇"
        }
        "Subagent model policy: {error}" => "子代理模型原則：{error}",
        "drawing diagram… {count} lines" => "正在繪製圖表… {count} 行",
        // notify.rs
        "Task complete" => "任務已完成",
        "Background task finished: {label}" => "後台任務已完成：{label}",
        "Background task failed: {label}" => "後台任務失敗：{label}",
        "Background task stopped: {label}" => "後台任務已停止：{label}",
        "Background tasks" => "後台任務",
        "Nothing runs in the background." => "後台沒有正在執行的任務。",
        "No matching tasks." => "沒有符合的任務。",
        "Permission needed: {tool}" => "需要授權：{tool}",
        "Question waiting for your answer" => "有問題等你回答",
        "Notify when agent needs you" => "需要你時通知",
        "Show ACP sessions" => "顯示 ACP 工作階段",
        "Prompt suggestion" => "提示建議",
        "Prompt suggestion: {error}" => "提示建議：{error}",
        "Suggestions are paused in this session because offers went unused. Turn them off and on in /settings to resume." => {
            "本工作階段因提示建議未被採用而暫停。在 /settings 中關閉再開啟即可恢復。"
        }
        // keys.rs
        "show the key bindings" => "顯示快速鍵",
        "check for a newer version" => "檢查新版本",
        "Application keys" => "應用程式快速鍵",
        "Enter submit when idle · Shift+Enter / Alt+Enter / Ctrl+J newline · ↑↓ history" => {
            "閒置時 Enter 送出 · Shift+Enter / Alt+Enter / Ctrl+J 換行 · ↑↓ 歷程"
        }
        "Running with a draft: Enter {default} · Ctrl+S {alternate}" => {
            "執行中有草稿：Enter {default} · Ctrl+S {alternate}"
        }
        "Empty draft: {keys} steer first queued" => "草稿為空時按 {keys} steer 第一則佇列訊息",
        "Ctrl+O or Ctrl+P commands · Esc interrupt, queued messages return · Shift+← edit last queued" => {
            "Ctrl+O 或 Ctrl+P 指令 · Esc 中斷，佇列訊息回到輸入框 · Shift+← 編輯最後一則佇列訊息"
        }
        "Ctrl+Enter interrupt and send now: the draft and every held message, as one message" => {
            "Ctrl+Enter 中斷並立即傳送：草稿與所有暫存訊息合成一則送出"
        }
        "Send now failed: {error}; the draft was kept" => "立即傳送失敗：{error}；草稿已保留",
        "Send now cancelled; the draft was kept" => "已取消立即傳送；草稿保留",
        "Send now is still stopping the run; the draft was kept" => {
            "立即傳送仍在停止執行；草稿已保留"
        }
        "Send now is stopping the run; paste again when the draft is sent" => {
            "立即傳送正在停止執行；草稿送出後再貼上"
        }
        "Send now is waiting for the insertion before sending" => {
            "立即傳送正在等待插入內容後再送出"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C 清除，按兩次離開 · 草稿為空時 Ctrl+D 離開"
        }
        "Shift+Tab cycle permissions (composer focused)" => {
            "Shift+Tab 循環切換權限（輸入框聚焦時）"
        }
        "Shift+Tab newer target while picked · Esc return to composer" => {
            "選取歷史項時 Shift+Tab 選擇較新項 · Esc 返回輸入框"
        }
        "(shift+tab to cycle)" => "(Shift+Tab 切換)",
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
        "follow or change the shown line's goal" => "跟隨或修改目前顯示線的目標",
        "◎ goal active" => "◎ 目標進行中",
        "◎ goal paused" => "◎ 目標已暫停",
        "insufficient space: {parts}" => "空間不足：{parts}",
        "system prompt" => "系統提示詞",
        "tools" => "工具",
        "appended messages" => "附加訊息",
        "keyed baseline" => "帶 key 的基線",
        "pinned inputs" => "釘住的輸入",
        "digest" => "清單",
        "summary" => "摘要",
        "The first message was not sent; your new draft was kept." => {
            "首條訊息未傳送；已保留你的新草稿。"
        }
        "The App Server is still {server} because other clients are using it; this {client} client uses it too. Run `dimcode daemon stop` when they are done, and the next start runs {client}." => {
            "App Server 仍是 {server}，因為其他用戶端正在使用它；這個 {client} 用戶端也使用它。等它們結束後執行 `dimcode daemon stop`，下次啟動就會執行 {client}。"
        }
        "The App Server is still {server} because other clients are using it, and it speaks protocol v{protocol}, which this {client} client cannot use. No Session was started. Run `dimcode daemon stop` when they are done, and the next start runs {client}." => {
            "App Server 仍是 {server}，因為其他用戶端正在使用它，而它使用協定 v{protocol}，這個 {client} 用戶端無法使用。沒有啟動工作階段。等它們結束後執行 `dimcode daemon stop`，下次啟動就會執行 {client}。"
        }
        "The App Server is still {server} because other clients are using it (connections: {connections}, attached Sessions: {attached}, running turns: {running}), and it speaks protocol v{protocol}, which this {client} client cannot use. No Session was started. Run `dimcode daemon stop` when they are done, and the next start runs {client}." => {
            "App Server 仍是 {server}，因為其他用戶端正在使用它（連線：{connections}，已附加工作階段：{attached}，執行中的回合：{running}），而它使用協定 v{protocol}，這個 {client} 用戶端無法使用。沒有啟動工作階段。等它們結束後執行 `dimcode daemon stop`，下次啟動就會執行 {client}。"
        }
        "The App Server is still {server} because other clients are using it (connections: {connections}, attached Sessions: {attached}, running turns: {running}); this {client} client uses it too. Run `dimcode daemon stop` when they are done, and the next start runs {client}." => {
            "App Server 仍是 {server}，因為其他用戶端正在使用它（連線：{connections}，已附加工作階段：{attached}，執行中的回合：{running}）；這個 {client} 用戶端也使用它。等它們結束後執行 `dimcode daemon stop`，下次啟動就會執行 {client}。"
        }
        "The App Server is still using a different build of {server}. Some features may be unavailable. After active work finishes, exit this client, run `{command}`, then reopen it." => {
            "App Server 仍在執行 {server} 的另一個建置。部分功能可能無法使用。目前工作結束後，請結束此用戶端，執行 `{command}`，然後重新開啟。"
        }
        "The App Server's build could not be verified as this client's. If features are missing after an update, wait for active work to finish, exit this client, run `{command}`, then reopen it." => {
            "無法確認 App Server 的建置與此用戶端一致。如果更新後缺少功能，請等目前工作結束後結束此用戶端，執行 `{command}`，然後重新開啟。"
        }
        "Upgrading the local store from format v{from} to v{to}; this can take a while on a large store." => {
            "正在把本機資料庫從格式 v{from} 升級到 v{to}；大型資料庫可能需要一段時間。"
        }
        "Upgrading the local store · {seconds} s · {stage}" => {
            "正在升級本機資料庫 · 已用 {seconds} 秒 · {stage}"
        }
        "moving retained images into Session files" => "把保留的圖片寫入工作階段檔案",
        "updating the text index of the rows that changed" => "更新已改動列的文字索引",
        "checking and committing the upgraded store" => "校驗並提交升級後的資料庫",
        "A prompt save is still in progress" => "提示詞儲存尚未完成",
        "Base prompt" => "基礎提示詞",
        "Before/after comparison is unavailable for this record." => {
            "此記錄沒有可用的修改前後對照。"
        }
        "Cancelling generation…" => "正在取消產生…",
        "Changes" => "修改內容",
        "Compaction prompt" => "壓縮提示詞",
        "Could not open the share destination" => "無法開啟分享頁面",
        "Could not save ZIP" => "無法儲存 ZIP",
        "Default: current-session diagnostics only." => "預設只打包目前工作階段的診斷資訊。",
        "Draft a GitHub issue" => "在 GitHub 起草 Issue",
        "Email the ZIP to {email}" => "可將 ZIP 寄至 {email}",
        "Enter apply · Esc back" => "Enter 套用 · Esc 返回",
        "Error diagnostics only; no session text is available." => {
            "僅打包錯誤診斷資訊；沒有可收集的工作階段內容。"
        }
        "Evidence · No recent session activity" => "證據 · 近期沒有可回報的工作階段活動",
        "Evidence · Observed error only" => "證據 · 只有觀測到的錯誤",
        "Evidence · Recent session activity" => "證據 · 最近的工作階段活動",
        "Evidence · Timeline omitted" => "證據 · 已省略時間線",
        "Evidence · Timeline read failed" => "證據 · 無法讀取時間線",
        "Evidence · Timeline unavailable" => "證據 · 時間線不可用",
        "Generate report" => "產生報告",
        "Generate/save never uploads. Share manually after saving." => {
            "產生和儲存都不會上傳。儲存後再手動分享。"
        }
        "Generating report…" => "正在產生報告…",
        "Give feedback on Discord" => "到 Discord 提供意見",
        "Input" => "參數",
        "Invalid report ZIP" => "報告 ZIP 無效",
        "Local generation makes zero model requests." => "本機產生不會呼叫模型。",
        "Model · Generate locally" => "模型 · 本機產生",
        "New" => "新",
        "No content changes." => "內容沒有變化。",
        "No session to report." => "沒有可回報的工作階段。",
        "Old" => "舊",
        "Opened Discord" => "已開啟 Discord",
        "Opened GitHub issue draft" => "已開啟 GitHub Issue 草稿",
        "Options" => "選項",
        "Options must fit within 1 KiB" => "選項不得超過 1 KiB",
        "Options · for example: omit timeline, keep error codes" => {
            "選項 · 例如 omit timeline, keep error codes"
        }
        "Prepare report again" => "重新準備報告",
        "Preparing report…" => "正在準備報告…",
        "Prompt saved; applies to new Sessions" => "提示詞已儲存；對新工作階段生效",
        "Prompt settings are not available" => "提示詞設定不可用",
        "Prompts" => "提示詞",
        "Redact [ ]" => "遮蔽 [ ]",
        "Redact [x]" => "遮蔽 [x]",
        "Redact is off. The ZIP may contain sensitive text from recent messages." => {
            "遮蔽已關閉。ZIP 可能包含近期對話中的敏感內容。"
        }
        "Refresh report plan" => "重新整理報告計畫",
        "Report ready" => "報告已產生",
        "Report this error" => "回報此錯誤",
        "Report this session" => "回報目前工作階段",
        "Resetting {field}; the old text is kept in {path}" => {
            "正在重設 {field}；舊文字保留在 {path}"
        }
        "Result" => "結果",
        "Run /report to create a report." => "輸入 /report 產生報告。",
        "Running…" => "執行中…",
        "Save ZIP" => "儲存 ZIP",
        "Saved ZIP" => "ZIP 已儲存至",
        "Session model" => "工作階段模型",
        "Skills instructions" => "技能說明",
        "Some options were unsupported." => "部分選項不受支援。",
        "Tab also picks tool headers · Enter/Space toggle the focused tool" => {
            "Tab 也可選擇工具標題 · Enter/空白鍵展開或收合目前工具"
        }
        "Turn Redact off to attach recent user/assistant text to ZIP; it may contain sensitive data." => {
            "只有主動關閉遮蔽，才會將近期使用者和助手內容附加到 ZIP；可能含敏感資訊。"
        }
        "Updating report plan…" => "正在更新報告計畫…",
        "Usage: /report" => "用法：/report",
        "Waiting for result…" => "等待結果…",
        "ZIP contents: report.md, diagnostics.json" => "ZIP 內容：report.md、diagnostics.json",
        "built-in" => "內建",
        "custom" => "自訂",
        "report the current session" => "回報目前工作階段",
        "skills replaces the marker and call shape; the skill tool and list stay · r reset" => {
            "skills 會替換標記與呼叫形狀；skill 工具和清單不受影響 · r 重設"
        }
        "… {count} unchanged lines" => "… {count} 行未修改",
        "↑↓ move · Enter edit · r reset · Esc close" => {
            "↑↓ 移動 · Enter 編輯 · r 重設 · Esc 關閉"
        }
        "↑↓ move · Enter select · ←→/Space change · r reset · Esc close" => {
            "↑↓ 移動 · Enter 選擇 · ←→/空白鍵 變更 · r 重設 · Esc 關閉"
        }
        "↑↓/Tab move · Enter select · Esc close" => "↑↓/Tab 切換 · Enter 選擇 · Esc 關閉",
        "Permission mode" => "權限模式",
        "Related settings" => "相關設定",
        "Process isolation" => "進程隔離",
        "open" => "開啟",
        "User-wide command rules. Allow skips asks, never denials." => {
            "使用者層級指令規則。允許只會跳過詢問，不會越過拒絕。"
        }
        "Opens Settings; user-wide, for new processes; independent of permission mode." => {
            "開啟設定；使用者層級，作用於新進程；與權限模式無關。"
        }
        "keep mode" => "保持模式",
        "use mode" => "使用模式",
        "Current session; remembered for new sessions. {consequence}" => {
            "僅目前工作階段；會記住用於新工作階段。{consequence}"
        }
        "↑↓/Tab move · Enter {action} · Esc close" => {
            "↑↓/Tab 移動 · Enter {action} · Esc 關閉"
        }
        "Writes and commands denied; external reads may ask." => {
            "寫入和指令一律拒絕；外部讀取可能會詢問。"
        }
        "Workspace writes allowed; commands, network and ungranted outside access usually ask." => {
            "允許工作區寫入；指令、網路和未獲授權的外部存取通常會詢問。"
        }
        "Skips preset approval; pending calls on this branch are reevaluated. Explicit denies and isolation remain." => {
            "跳過預設核准；此分支上待處理的呼叫會重新評估。明確的拒絕和隔離仍然有效。"
        }
        "↑↓ move · Enter select · Esc close" => "↑↓ 移動 · Enter 選擇 · Esc 關閉",
        "Loading process isolation…" => "正在讀取進程隔離…",
        "Sandbox is unavailable on this host: {reason}" => "此主機上沙箱不可用：{reason}",
        "Applies to new processes and shells." => "作用於新進程和 shell。",
        "Off: commands use your OS permissions, not file-tool limits." => {
            "關閉：指令使用作業系統的權限，不受檔案工具限制。"
        }
        "Off" => "關閉",
        "Sandboxed · No network" => "沙箱 · 無網路",
        "Sandboxed · Network allowed" => "沙箱 · 允許網路",
        "selected" => "已選",
        "Save" => "儲存",
        "Reset to defaults" => "重設為預設值",
        "Reload" => "重新載入",
        "Discard edits and read the current settings" => "捨棄修改並讀取目前設定",
        "Reset the execution switches to defaults?" => "將執行開關重設為預設值？",
        "Discard unsaved changes?" => "捨棄未儲存的修改？",
        "Yes (y)" => "是 (y)",
        "Cancel (n)" => "取消 (n)",
        "y confirm · Enter/Esc cancel" => "y 確認 · Enter/Esc 取消",
        "Read only" => "唯讀",
        "Workspace write" => "工作區寫入",
        "Agent" => "智慧代理",
        "agent" => "智慧代理",
        "command" => "指令",
        "ended" => "已結束",
        "Signed in" => "已登入",
        "Retry" => "重試",
        "Provider changed; review the retained draft and press Apply again" => {
            "提供方已變更；請檢查保留的草稿並再次按套用"
        }
        "y allow · a always · n reject · ←→/Tab move · Enter confirm" => {
            "y 允許 · a 永遠允許 · n 拒絕 · ←→/Tab 切換 · Enter 確認"
        }
        "{count} in background · /tasks" => "背景 {count} 項 · /tasks",
        "{field}: {error} Your text is kept in {path}" => {
            "{field}：{error} 你的文字已保留在 {path}"
        }
        "{field}: {error}" => "{field}：{error}",
        "Prompts: {error}" => "提示詞：{error}",
        _ => return None,
    })
}
