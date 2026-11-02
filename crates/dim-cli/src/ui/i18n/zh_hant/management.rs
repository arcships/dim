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
