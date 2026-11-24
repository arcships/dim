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
        "models" => "模型",
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
        "Remove {name}?" => "刪除 {name}？",
        "↑↓ scroll · Esc back" => "↑↓ 捲動 · Esc 返回",
        "Added." => "已新增。",
        "Removed {name}." => "已刪除 {name}。",
        "unavailable" => "無法使用",
        "Reset {name}?" => "重設 {name}？",
        "Enter confirm" => "Enter 確認",
        "Esc cancel" => "Esc 取消",
        "Reset {name} to its catalog entry." => "已將 {name} 重設為目錄項目。",
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
        "Models" => "模型",
        "catalog" => "目錄",
        "overrides" => "覆寫",
        "added" => "新增",
        "Models · {name}" => "模型 · {name}",
        "undeclared" => "未宣告",
        "Context window {window} · max output {output}" => {
            "上下文視窗 {window} · 最大輸出 {output}"
        }
        "Endpoint {endpoint}" => "端點 {endpoint}",
        "Endpoint {endpoint} · override {value}" => "端點 {endpoint} · 覆寫為 {value}",
        "The provider reports it unavailable." => "提供方回報它無法使用。",
        "Limits undeclared; edit the model to declare them." => "上限未宣告；編輯該模型以宣告。",
        "Sign in to the account to use its models." => "登入帳號後即可使用其模型。",
        "The signed-in account changed; reload to list its models." => {
            "登入的帳號已變更；重新載入以列出其模型。"
        }
        "The provider has no catalog yet." => "此提供方還沒有目錄。",
        "The provider listed no models." => "提供方沒有列出任何模型。",
        "Listed {count} models." => "已列出 {count} 個模型。",
        "Refreshed the models." => "模型已重新整理。",
        "Could not list models: {error}. Showing the stored list." => {
            "無法列出模型：{error}。顯示已儲存的清單。"
        }
        "The signed-in account changed; the refresh cleared preferences, overrides and disables." => {
            "登入的帳號已變更；重新整理已清除偏好、覆寫和停用項。"
        }
        "The models changed since they were read; nothing was removed." => {
            "模型在讀取後已變更；本次沒有移除任何東西。"
        }
        "The models changed since they were read; nothing was written." => {
            "模型在讀取後已變更；本次沒有寫入。"
        }
        "No provider has that id." => "沒有該 id 的提供方。",
        "{name} is unavailable ({reason}); its enablement is kept as it is." => {
            "{name} 無法使用（{reason}）；其啟用狀態保持不變。"
        }
        "No model has that id." => "沒有該 id 的模型。",
        "This model cannot be removed." => "此模型無法移除。",
        "A Provider's models have no area document." => "提供方的模型沒有領域文件。",
        "Models have no run operation." => "模型沒有執行操作。",
        "The document is not an object; nothing was written." => "文件不是物件；本次沒有寫入。",
        "The document changes no editable field; nothing was written." => {
            "文件沒有修改任何可編輯欄位；本次沒有寫入。"
        }
        "The model document names no model." => "模型文件沒有指明模型。",
        "The provider has no model catalog to write." => "此提供方沒有可寫入的模型目錄。",
        "ID {id}" => "ID {id}",
        "Source {source}" => "來源 {source}",
        "Status {status}" => "狀態 {status}",
        "Tools {value}" => "工具 {value}",
        "Vision {value}" => "視覺 {value}",
        "Reasoning {value}" => "推理 {value}",
        "Modalities {list}" => "模態 {list}",
        "Description {text}" => "說明 {text}",
        "Release date {date}" => "發佈日期 {date}",
        "Pricing {value}" => "價格 {value}",
        "Rate limit {value}" => "速率限制 {value}",
        "Matched provider {id}" => "符合提供方 {id}",
        "Matched model {id}" => "符合模型 {id}",
        "Match strategy {strategy}" => "符合策略 {strategy}",
        "Capability overrides {fields}" => "能力覆寫欄位 {fields}",
        "yes" => "是",
        "no" => "否",
        "not supported" => "不支援",
        "supported" => "支援",

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
        "{id} is in more than one MCP source: {sources}. Name one with --source." => {
            "「{id}」出現在多個 MCP 來源中：{sources}。請用 --source 指定其中一個。"
        }
        "The action names no MCP source; a row carries the source its owner reported." => {
            "此操作沒有指定 MCP 來源；每一列都帶有其擁有者回報的來源。"
        }
        "No MCP server {id} is in source {source}." => "來源 {source} 中沒有 MCP 伺服器 {id}。",
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
        "Tool call timeout: {millis} ms" => "工具呼叫逾時：{millis} 毫秒",
        "Variables: {names}" => "變數：{names}",
        "Unset variables: {names}" => "未設定的變數：{names}",
        "The MCP source changed since the preview; no entry was imported, and an entry it would have replaced may now be disabled. Preview again." => {
            "MCP 來源在預覽之後已變更；沒有匯入任何項目，原本會被它替換的項目現在可能已停用。請重新預覽。"
        }
        "The project entries were written and their withheld values were not; replace them again to retry." => {
            "專案項目已寫入，保留未寫的值沒有寫入；再次替換即可重試。"
        }
        "Replaced {id}." => "已替換 {id}。",
        "Added {id}." => "已新增 {id}。",
        "Delivers to Session {session} branch {branch} ({mode})." => {
            "投遞到工作階段 {session} 的分支 {branch}（{mode}）。"
        }
        "Launches a new Session in {cwd}." => "在 {cwd} 啟動新的工作階段。",
        "Disabled: {code} at {instant}." => "已停用：{code}，時間 {instant}。",
        "The definition changed since it was read; nothing was written." => {
            "定義在讀取之後已變更；沒有寫入任何內容。"
        }
        "Columns: ID, client, scope, path, state, entries or error." => {
            "欄位：ID、用戶端、範圍、路徑、狀態、項目數或錯誤。"
        }
        "No value from a store is read out or printed; --from ID previews one inside the daemon." => {
            "不會讀出或印出儲存中的任何值；--from ID 會在守護進程內預覽其中一個。"
        }
        "Columns: ID, action, destination ID.\nA renamed entry is written under its destination id; the source's entry with the old id is left as it was." => {
            "欄位：ID、動作、目標 ID。\n重新命名的項目會以其目標 id 寫入；來源中舊 id 的項目保持原狀。"
        }
        "Imported and replaced entries start disabled: a user entry is written with enabled: false, and a project entry has no enablement record until you enable it locally." => {
            "匯入和替換的項目一開始為停用：使用者項目寫入時帶 enabled: false，專案項目在你於本機啟用前沒有啟用記錄。"
        }
        "This daemon cannot list other clients' MCP stores." => {
            "此守護進程無法列出其他用戶端的 MCP 儲存。"
        }
        "This daemon cannot import MCP servers." => "此守護進程無法匯入 MCP 伺服器。",
        "The document holds no mcpServers entries." => "文件中沒有 mcpServers 項目。",
        "Columns: ID, status, transport, changed fields, unsupported fields, error.\nA changed field names what a write would change and the kind of value it holds; no value is printed.\nAn unsupported field is kept unused or dropped, as shown after the field." => {
            "欄位：ID、狀態、傳輸方式、變更的欄位、不受支援的欄位、錯誤。\n變更的欄位說明寫入會改變什麼以及值的類型；不印出任何值。\n不受支援的欄位會依欄位後面的說明保持未使用或被丟棄。"
        }
        "A project entry is written as its public skeleton: the workspace file gets the command, declared cwd, URL origin and public OAuth fields. Its arguments, environment and header values, bearer token, a URL with userinfo, path, query or fragment, and unknown members go to your user-scope mcp-secrets record, never to the workspace file." => {
            "專案項目只寫入其公開骨架：工作區檔案會得到指令、宣告的 cwd、URL 來源和公開的 OAuth 欄位。它的參數、環境變數和標頭值、bearer token、帶 userinfo、路徑、查詢或片段的 URL，以及未知成員，都會寫入你的使用者層級 mcp-secrets 記錄，絕不寫入工作區檔案。"
        }
        "{id} has {count} more field names than the lists above show." => {
            "{id} 還有 {count} 個欄位名稱未在上面的清單中顯示。"
        }
        "Top-level members the import does not carry: none." => "匯入未攜帶的頂層成員：無。",
        "Top-level members the import does not carry: {names}." => {
            "匯入未攜帶的頂層成員：{names}。"
        }
        "Applying the selection at this revision." => "正依此修訂套用所選內容。",
        "An unchanged entry was already in the source and was not written." => {
            "未變更的項目已存在於來源中，未寫入。"
        }
        _ => return None,
    })
}
