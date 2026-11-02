pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Loading..." => "正在載入…",
        "No entries." => "沒有項目。",
        "No matching entry." => "沒有符合的項目。",
        "Issues" => "問題",
        "sources" => "來源",
        "skills" => "技能",
        "hooks" => "鉤子",
        "scheduled jobs" => "排程工作",
        "workflow definitions" => "工作流程定義",
        "command rules" => "命令規則",
        "enabled" => "已啟用",
        "shadowed by {name}" => "被 {name} 覆蓋",
        "Reloaded." => "已重新載入。",
        "Enabled {name}." => "已啟用 {name}。",
        "Disabled {name}." => "已停用 {name}。",
        "Reloaded {area}: {entries} entries, {disabled} disabled." => {
            "已重新載入{area}：共 {entries} 項，{disabled} 項已停用。"
        }
        "Reloaded {area}: {entries} entries." => "已重新載入{area}：共 {entries} 項。",
        "Hooks" => "鉤子",
        "invalid" => "無效",
        "Saved." => "已儲存。",
        "Saved {name}." => "已儲存 {name}。",
        "No change." => "沒有變更。",
        "Your text is kept in {path}." => "你的內容保留在 {path}。",
        "The user's own document; always in effect." => "使用者自己的文件；始終生效。",
        "There is no hooks file there." => "那裡沒有 hooks 檔案。",
        "Owned by plugin {plugin}." => "由外掛 {plugin} 提供。",
        "The user document turns every hook off (disableAllHooks)." => {
            "使用者文件關閉了所有鉤子（disableAllHooks）。"
        }
        "Enter toggle" => "Enter 切換",
        "Ctrl+E edit" => "Ctrl+E 編輯",
        "Ctrl+N add" => "Ctrl+N 新增",
        "Ctrl+R reload" => "Ctrl+R 重新載入",
        "Ctrl+T run" => "Ctrl+T 執行",
        "Ctrl+X remove" => "Ctrl+X 刪除",
        "Tab inspect" => "Tab 檢視",
        "Remove {name}? Enter confirm · Esc cancel" => "刪除 {name}？Enter 確認 · Esc 取消",
        "↑↓ scroll · Esc back" => "↑↓ 捲動 · Esc 返回",
        "Added." => "已新增。",
        "Removed {name}." => "已刪除 {name}。",
        "Ran {name}." => "已執行 {name}。",
        "The user document changed since it was read; nothing was written." => {
            "使用者文件在讀取後已被修改；這次沒有寫入。"
        }
        "The job changed since it was read; nothing was written." => {
            "工作在讀取後已被修改；這次沒有寫入。"
        }
        "The jobs changed since they were read; nothing was written." => {
            "工作在讀取後已被修改；這次沒有寫入。"
        }
        "Your next edit opens the current document." => "下次編輯會開啟目前文件。",
        "Your next add reopens your text." => "下次新增會重新開啟你的文字。",
        "Scheduler" => "排程器",
        "jobs" => "工作",
        "Added {name}." => "已新增 {name}。",
        "Run {name}: {outcome}." => "已執行 {name}：{outcome}。",
        "Recent runs of {name}" => "{name} 的最近執行",
        "No run yet." => "尚未執行。",
        "No next run." => "沒有下次執行。",
        "Next run {instant}." => "下次執行 {instant}。",
        "Last run {outcome} at {instant}." => "上次執行 {outcome}，於 {instant}。",
        "every {minutes} minutes" => "每 {minutes} 分鐘",
        "at {instant}" => "於 {instant}",
        "cron {expression}" => "cron {expression}",
        "cron {expression} in {zone}" => "cron {expression}（{zone}）",
        "The job changed since it was read; nothing was removed." => {
            "工作在讀取後已被修改；這次沒有刪除。"
        }
        "The Scheduler has no area document." => "排程器沒有領域文件。",
        "No scheduled job has that id." => "沒有該 id 的排程工作。",
        "A scheduled job's run takes no input document." => "定時任務的執行不接受輸入文件。",
        "An MCP server's test takes no input document." => "MCP 伺服器的測試不接受輸入文件。",
        "This daemon cannot manage {area}." => "無法管理{area}。",
        "This daemon lists {area} but cannot toggle them." => "可以列出{area}，但無法切換它們。",
        "This daemon lists {area} but cannot edit them." => "可以列出{area}，但無法編輯它們。",
        "This daemon lists {area} but cannot reload them." => {
            "可以列出{area}，但無法重新載入它們。"
        }
        "This daemon lists {area} but cannot add them." => "可以列出{area}，但無法新增它們。",
        "This daemon lists {area} but cannot remove them." => "可以列出{area}，但無法刪除它們。",
        "This daemon lists {area} but cannot run them." => "可以列出{area}，但無法執行它們。",
        "This daemon lists {area} but cannot inspect them." => {
            "可以列出{area}，但無法檢視它們的詳細資料或歷史。"
        }
        "The document deleted {field}, which the owner cannot clear; nothing was written." => {
            "文件刪除了 {field}，擁有者無法清除它；本次沒有寫入。"
        }
        "The document changes no field; nothing was written." => {
            "文件沒有變更任何欄位；本次沒有寫入。"
        }
        "This workspace's hook settings changed since they were read; nothing was written." => {
            "此工作區的鉤子設定已在讀取後變更；這次沒有寫入。"
        }
        "Workflow" => "工作流程",
        "Saved {name}. It stays disabled until you enable it." => {
            "已儲存 {name}。在你啟用它之前，它保持停用。"
        }
        "The definitions changed since they were read; nothing was written." => {
            "定義在讀取後已被修改；本次沒有寫入。"
        }
        "A definition's name is its key; add it to save it under another name." => {
            "定義的名稱就是它的鍵；要用另一個名稱儲存，請新增它。"
        }
        "No project definition is named {name}." => "沒有名為 {name} 的專案定義。",
        "No workflow definition is named {name}." => "沒有名為 {name} 的工作流程定義。",
        "Saved in the user workflow source; always enabled." => {
            "儲存在使用者工作流程來源中；始終啟用。"
        }
        "Starts only after you enable it." => "只有在你啟用它之後才會啟動。",
        "Defined by package {package}." => "由套件 {package} 定義。",
        "{name} — {description} (required)" => "{name} — {description}（必填）",
        "{name} — {description}" => "{name} — {description}",
        "{name} — required" => "{name} — 必填",
        "{name} — default: {default}" => "{name} — 預設：{default}",
        "and {count} more" => "另有 {count} 項",
        "Recent runs" => "最近的執行",
        "A workflow definition is saved as an object with a source and a document." => {
            "工作流程定義以包含來源和文件的物件儲存。"
        }
        "A definition is saved to the user source or the project source." => {
            "定義儲存到使用者來源或專案來源。"
        }
        "Started {name}: {run_id}." => "已啟動 {name}：{run_id}。",
        "Parameters are a JSON object of strings." => "參數是由字串組成的 JSON 物件。",
        "The list reached its size limit; later definitions are not shown." => {
            "列表已達到大小上限，之後的定義不會顯示。"
        }
        "No workflow definition named {name} is in the list, which reached its size limit." => {
            "達到大小上限的列表中沒有名為 {name} 的工作流定義。"
        }
        "MCP" => "MCP",
        "MCP servers" => "MCP 伺服器",
        "Ctrl+T test" => "Ctrl+T 測試",
        "sign-in required" => "需要登入",
        "signed in" => "已登入",
        "sign-in expired" => "登入已過期",
        "missing scopes: {scopes}" => "缺少權限範圍：{scopes}",
        "Executable: {command}" => "可執行檔：{command}",
        "Stored arguments (hidden): {count}" => "已存參數（隱藏）：{count}",
        "Working directory: {cwd}" => "工作目錄：{cwd}",
        "Environment names: {names}" => "環境變數名稱：{names}",
        "Endpoint: {endpoint}" => "端點：{endpoint}",
        "Header names: {names}" => "標頭名稱：{names}",
        "Bearer token: {state}" => "Bearer 權杖：{state}",
        "OAuth: configured (client ID {id})" => "OAuth：已設定（用戶端 ID {id}）",
        "OAuth: configured" => "OAuth：已設定",
        "OAuth: not configured" => "OAuth：未設定",
        "Metadata URL: {state}" => "中繼資料 URL：{state}",
        "configured" => "已設定",
        "Owned by a plugin; change it through Plugins." => "由外掛提供；請透過外掛修改。",
        "MCP entry {id}" => "MCP 項目 {id}",
        "Connection {number} · {status}" => "連線 {number} · {status}",
        "Tools have not been discovered." => "尚未發現工具。",
        "No live session connections." => "沒有作用中的工作階段連線。",
        "Tools of {id}" => "{id} 的工具",
        "The server exposes no tools." => "伺服器未提供工具。",
        "The add document needs an id." => "新建文件需要一個 id。",
        "The add document needs a source of user or project." => {
            "新建文件的來源必須是 user 或 project。"
        }
        "The add document needs a configuration object." => "新建文件需要一個 configuration 物件。",
        "{id} is already in mcp list; remove it first or choose another id." => {
            "{id} 已在 mcp list 中；請先移除它或換一個 id。"
        }
        "{id} is already in mcp list; choose another id, or change it through its plugin." => {
            "{id} 已在 mcp list 中；請換一個 id，或透過其外掛變更。"
        }
        "This entry's settings changed since they were read; nothing was written." => {
            "此項目的設定已在讀取後變更；本次沒有寫入。"
        }
        "The MCP entry changed since it was read; nothing was written." => {
            "MCP 項目已在讀取後變更；本次沒有寫入。"
        }
        "The MCP entries changed since they were read; nothing was written." => {
            "MCP 項目已在讀取後變更；本次沒有寫入。"
        }
        "The MCP entry changed since it was read; nothing was removed." => {
            "MCP 項目已在讀取後變更；本次沒有移除。"
        }
        "MCP's documents belong to one entry; this save names none." => {
            "MCP 的文件屬於單一項目；本次儲存沒有指定項目。"
        }
        "No MCP server has that id." => "沒有該 id 的 MCP 伺服器。",
        "This workspace has no project MCP source." => "此工作區沒有專案層級 MCP 來源。",
        "Enabled {id} ({source})." => "已啟用 {id}（{source}）。",
        "Disabled {id} ({source})." => "已停用 {id}（{source}）。",
        "It runs this workspace's .mcp.json entry, including later edits to it, in active and new sessions." => {
            "它會在目前和新的工作階段中執行此工作區的 .mcp.json 項目，包括之後對該檔案的修改。"
        }
        "{id} is not in mcp list; withdrew any stored enablement of it in this workspace." => {
            "{id} 不在 mcp list 中；已撤銷它在此工作區儲存的啟用狀態。"
        }
        "The MCP catalog could not be read ({error}); withdrew this workspace's enablement of {id}. A user entry with this ID is unchanged." => {
            "無法讀取 MCP 目錄（{error}）；已撤銷此工作區對 {id} 的啟用。同 ID 的使用者項目不受影響。"
        }
        "MCP configuration: {error}" => "MCP 設定：{error}",
        "The server rejected the stored credential; sign in again with mcp login." => {
            "伺服器拒絕了已儲存的憑證；請用 mcp login 重新登入。"
        }
        "The server requires additional scopes: {scopes}." => {
            "伺服器需要額外的權限範圍：{scopes}。"
        }
        "The server requires authorization; run mcp login ID." => {
            "伺服器需要授權；請執行 mcp login ID。"
        }
        "The probe ended: {status}." => "探測結束：{status}。",

        "Command rules" => "命令規則",
        "user" => "使用者",
        "Matches only this command." => "僅符合此命令。",
        "Matches commands whose first words are these." => "符合以此開頭的命令。",
        "Allows a simple command that would otherwise ask." => "允許原本會詢問的簡單命令。",
        "Denies the command under every preset." => "在任何預設下都拒絕該命令。",
        "Applies in every Session, in every workspace." => "在所有工作階段、所有工作區中生效。",
        "Rule added: {rule}." => "已新增規則：{rule}。",
        "Rule already present: {rule}." => "規則已存在：{rule}。",
        "Rule removed: {rule}." => "已刪除規則：{rule}。",
        "That command rule is no longer listed; nothing was removed." => {
            "該命令規則已不在清單中；未刪除任何內容。"
        }
        "Command rules changed since they were read; nothing was removed." => {
            "命令規則在讀取後已被修改；未刪除任何內容。"
        }
        "A command rule is DECISION MATCH COMMAND, such as deny prefix git push." => {
            "命令規則寫作 DECISION MATCH COMMAND，例如 deny prefix git push。"
        }
        "Command: {command}" => "指令：{command}",
        "Use the line prompt or dimcode permissions." => {
            "請在行內提示字元或 dimcode permissions 中使用。"
        }
        _ => return None,
    })
}
