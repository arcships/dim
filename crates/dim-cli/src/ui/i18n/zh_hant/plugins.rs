pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Plugins" => "外掛",
        "Enter on/off · r reload · Ctrl+R refresh · Esc close" => {
            "Enter 開/關 · r 重新載入 · Ctrl+R 重新整理 · Esc 關閉"
        }
        "Locked: Dim needs it, so it cannot be turned off." => "已鎖定：Dim 需要它，所以無法關閉。",
        "Loading plugins..." => "正在載入外掛…",
        "No plugins." => "沒有外掛。",
        "Working..." => "處理中…",
        "locked" => "已鎖定",
        "Uses {use}" => "使用 {use}",
        "Candidates: {candidates}" => "候選：{candidates}",
        "Waiting for: {names}" => "等待：{names}",
        "Refused {kind}: {error}" => "{kind} 被拒絕：{error}",
        "Turn off {name}?" => "關閉 {name}？",
        "Its tools and content stop in new turns until you turn it on again." => {
            "關閉後，新的輪次中不再有它的工具和內容，直到再次開啟。"
        }
        "Turn off" => "關閉",
        "Turned on {name}." => "已開啟 {name}。",
        "Turned off {name}." => "已關閉 {name}。",
        "Reloaded {name}." => "已重新載入 {name}。",
        "Installed {name} ({id})." => "已安裝 {name}（{id}）。",
        "Installed {name} ({id}); it is not turned on." => "已安裝 {name}（{id}）；未開啟。",
        "Installing {path}..." => "正在安裝 {path}…",
        "Restart required: it loads after the App Server restarts (dimcode daemon stop)." => {
            "需要重新啟動：App Server 重新啟動後才會載入（dimcode daemon stop）。"
        }
        "Skipped {kinds}: their owner is not running." => "已略過 {kinds}：其負責模組未執行。",
        "Usage: /plugins [install PATH]" => "用法：/plugins [install 路徑]",
        "list plugins, turn them on or off, reload (install PATH)" => {
            "列出外掛、開啟或關閉、重新載入（install 路徑）"
        }
        _ => return None,
    })
}
