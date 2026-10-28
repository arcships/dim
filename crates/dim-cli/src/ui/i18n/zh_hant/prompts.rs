pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // prompts.rs: exit confirmation
        "The agent is still running." => "代理仍在執行。",
        "AskUser is waiting for an answer." => "AskUser 正在等待回答。",
        "A permission request is waiting." => "有授權要求正在等待。",
        "Background work may still be active." => "背景工作可能仍在執行。",
        "Exit dimcode?" => "離開 dimcode？",
        "Exit" => "離開",
        "Stay" => "留下",
        "y exit · n stay · ←→ move · enter confirm" => {
            "y 離開 · n 留下 · ←→ 移動 · enter 確認"
        }
        "Restart dimcode?" => "重新啟動 dimcode？",
        "Restart" => "重新啟動",
        "y restart · n stay · ←→ move · enter confirm" => {
            "y 重新啟動 · n 留下 · ←→ 移動 · enter 確認"
        }
        // upgrade_dialog.rs
        "Update available" => "有新版本",
        "Version {latest} is available (you have {current})." => {
            "{latest} 已推出（目前 {current}）。"
        }
        "Will run: {command}" => "將執行：{command}",
        "To update, run: {command}" => "更新請執行：{command}",
        "Update now" => "立即更新",
        "Skip this version" => "略過此版本",
        "Skipped {version}." => "已略過 {version}。",
        "Later" => "稍後",
        "Close" => "關閉",
        "Restart now" => "立即重新啟動",
        "Updating to {version}..." => "正在更新至 {version}…",
        "Updated to {version}. Restart DimCode to use it." => {
            "已更新至 {version}，重新啟動 DimCode 後生效。"
        }
        "Update failed: {error}" => "更新失敗：{error}",
        "←→ move · Enter confirm · Esc close" => "←→ 移動 · Enter 確認 · Esc 關閉",
        // prompts.rs: permission request
        "Permission required" => "需要授權",
        "Allow once" => "允許一次",
        "Allow always" => "一律允許",
        "Reject" => "拒絕",
        "Approve for me" => "智慧權限",
        "Full access" => "完整存取權限",
        "Run {tool}" => "執行 {tool}",
        "Changing the approvals mode from here is not yet available" => "尚不支援在此變更核准模式",
        "y allow · n reject · ←→/Tab move · Enter confirm" => {
            "y 允許 · n 拒絕 · ←→/Tab 移動 · Enter 確認"
        }
        "Enter submit · Tab edit · Esc decline" => "Enter 送出 · Tab 編輯 · Esc 不回答",
        "Space toggle · Tab next · Esc decline" => "Space 切換 · Tab 下一題 · Esc 不回答",
        "1-9 select · Tab next · Esc decline" => "1-9 選擇 · Tab 下一題 · Esc 不回答",
        "Question" => "問題",
        "Question on branch {branch}" => "來自分支 {branch} 的問題",
        "Permission on branch {branch}" => "來自分支 {branch} 的授權請求",
        "Sending answer…" => "正在傳送回答…",
        "Question not ready; press Enter to send again" => "問題尚未就緒；按 Enter 重送",
        // prompts.rs: AskUser
        "Confirm" => "確認",
        "Enter save · Esc back" => "Enter 儲存 · Esc 返回",
        "Enter submit · 1-9 edit · Tab/←→ back · Esc decline" => {
            "Enter 送出 · 1-9 修改 · Tab/←→ 返回 · Esc 不回答"
        }
        "↑↓ move · Space toggle · Enter next · Tab/←→ switch · Esc decline" => {
            "↑↓ 移動 · Space 勾選 · Enter 下一題 · Tab/←→ 切換 · Esc 不回答"
        }
        "↑↓ move · 1-9 select · Enter next · Tab/←→ switch · Esc decline" => {
            "↑↓ 移動 · 1-9 選擇 · Enter 下一題 · Tab/←→ 切換 · Esc 不回答"
        }
        "AskUserQuestion received an invalid payload." => "AskUserQuestion 收到無效的資料。",
        "Missing answer for Q{index}" => "第 {index} 題尚未回答",
        "Type your own answer" => "自行輸入回答",
        "Review your answers and press Enter to submit." => "確認答案後按 Enter 送出。",
        "Not answered" => "未回答",
        // approvals_panel.rs
        "Tool Approvals" => "工具核准",
        "full access" => "完整存取",
        "All tools run automatically, bypass permissions" => "所有工具自動執行，略過權限確認",
        "approve for me" => "智慧權限",
        "Agent automatically reviews commands and tool actions. Unsafe commands ask for confirmation; routine actions pass automatically, safe, smart, and convenient." => {
            "Agent 自動審查指令與工具操作，不安全指令會詢問，一般操作自動放行，安全智慧又方便。"
        }
        "Default permissions" => "預設權限",
        "default" => "預設",
        "Read tools run automatically, write tools require confirmation" => {
            "讀取類工具自動執行，寫入類工具需確認"
        }
        "current" => "目前",
        "↑↓/Tab move · Enter apply · Esc back" => "↑↓/Tab 移動 · Enter 套用 · Esc 返回",
        "Confirm Full Access" => "確認開啟完整存取",
        "Cancel" => "取消",
        "Tab switch  Enter confirm  Esc cancel" => "Tab 切換  Enter 確認  Esc 取消",
        "Full access reduces confirmation prompts and can run sensitive actions, file changes, or external commands directly. Use it only when you trust the current task." => {
            "完整存取會減少確認提示，並可直接執行敏感操作、檔案變更或外部指令。請僅在信任目前任務時使用。"
        }
        // transcript/tool_group.rs: summary verbs
        "ran" => "執行了",
        "running" => "正在執行",
        "read" => "讀取了",
        "reading" => "正在讀取",
        "explored" => "瀏覽了",
        "exploring" => "正在瀏覽",
        "edited" => "編輯了",
        "editing" => "正在編輯",
        "changed" => "變更了",
        "changing" => "正在變更",
        "checked on" => "查看了",
        "checking on" => "正在查看",
        "Explored" => "瀏覽了",
        "Exploring" => "正在瀏覽",
        "Edited" => "編輯了",
        "Editing" => "正在編輯",
        "Changed" => "變更了",
        "Changing" => "正在變更",
        // transcript/tool_group.rs: counts
        "1 file" => "1 個檔案",
        "{count} files" => "{count} 個檔案",
        "1 web page" => "1 個網頁",
        "{count} web pages" => "{count} 個網頁",
        "1 skill" => "1 個技能",
        "{count} skills" => "{count} 個技能",
        "1 search" => "1 次搜尋",
        "{count} searches" => "{count} 次搜尋",
        "1 command" => "1 個指令",
        "{count} commands" => "{count} 個指令",
        "1 task" => "1 個任務",
        "{count} tasks" => "{count} 個任務",
        "1 tool" => "1 個工具",
        "{count} tools" => "{count} 個工具",
        "{files} and {pages}" => "{files}和{pages}",
        ", " => "，",
        " · {count} failed" => " · {count} 個失敗",
        // transcript/tool_group.rs: call rows
        "Read" => "讀取",
        "List" => "列出",
        "Grep" => "文字搜尋",
        "Search the web" => "網頁搜尋",
        "Read skill" => "讀取技能",
        "Search git history" => "搜尋 git 歷程",
        "Edit" => "編輯",
        "Write" => "寫入",
        "Create" => "建立",
        " (detached)" => "（背景）",
        "Checked on background task" => "查看了背景工作",
        "Waiting…" => "等待中…",
        "… +{count} lines" => "… 還有 {count} 行",
        _ => return None,
    })
}
