pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Plugins" => "插件",
        "Enter on/off · r reload · Ctrl+R refresh · Esc close" => {
            "Enter 开/关 · r 重载 · Ctrl+R 刷新 · Esc 关闭"
        }
        "Locked: Dim needs it, so it cannot be turned off." => "已锁定：Dim 需要它，所以不能关闭。",
        "Loading plugins..." => "正在加载插件…",
        "No plugins." => "没有插件。",
        "Working..." => "处理中…",
        "locked" => "已锁定",
        "Uses {use}" => "使用 {use}",
        "Candidates: {candidates}" => "候选：{candidates}",
        "Waiting for: {names}" => "等待：{names}",
        "Refused {kind}: {error}" => "{kind} 被拒绝：{error}",
        "Turn off {name}?" => "关闭 {name}？",
        "Its tools and content stop in new turns until you turn it on again." => {
            "关闭后，新的轮次里不再有它的工具和内容，直到再次开启。"
        }
        "Turn off" => "关闭",
        "Turned on {name}." => "已开启 {name}。",
        "Turned off {name}." => "已关闭 {name}。",
        "Reloaded {name}." => "已重载 {name}。",
        "Installed {name} ({id})." => "已安装 {name}（{id}）。",
        "Installed {name} ({id}); it is not turned on." => "已安装 {name}（{id}）；未开启。",
        "Installing {path}..." => "正在安装 {path}…",
        "Restart required: it loads after the App Server restarts (dimcode daemon stop)." => {
            "需要重启：App Server 重启后才会加载（dimcode daemon stop）。"
        }
        "Skipped {kinds}: their owner is not running." => "已跳过 {kinds}：其负责模块未运行。",
        "Usage: /plugins [install PATH]" => "用法：/plugins [install 路径]",
        "list plugins, turn them on or off, reload (install PATH)" => {
            "列出插件、开启或关闭、重载（install 路径）"
        }
        _ => return None,
    })
}
