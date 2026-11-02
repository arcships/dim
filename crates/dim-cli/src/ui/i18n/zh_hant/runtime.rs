pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "This daemon does not serve plugin runtimes ({version})" => {
            "該守護進程不提供插件執行時（{version}）"
        }
        "Plugin runtimes" => "外掛執行階段",
        "Enter install or apply · s source · a auto · r retry · Esc close" => {
            "Enter 安裝或套用 · s 來源 · a 自動 · r 重試 · Esc 關閉"
        }
        "Esc close · the acquisition keeps running if you close this" => {
            "Esc 關閉 · 關閉面板不會中斷進行中的取得"
        }
        "Enter retry · s switch {name} to {source} · Esc close" => {
            "Enter 重試 · s 將 {name} 切換為 {source} · Esc 關閉"
        }
        "Loading plugin runtimes..." => "正在載入外掛執行階段…",
        "acquiring" => "取得中",
        "starting" => "啟動中",
        "down" => "已離線",
        "1 plugin waits for {runtime}: {names}" => "1 個外掛等待 {runtime}：{names}",
        "{count} plugins wait for {runtime}: {names}" => "{count} 個外掛等待 {runtime}：{names}",
        "show, install or configure the plugin runtimes" => "檢視、安裝或設定外掛執行階段",
        "Usage: /runtime [install NAME | source NAME download|system | auto NAME on|off]" => {
            "用法：/runtime [install 名稱 | source 名稱 download|system | auto 名稱 on|off]"
        }
        "Enter install or apply · s source · r retry · Esc close" => {
            "Enter 安裝或套用 · s 來源 · r 重試 · Esc 關閉"
        }
        "auto on" => "自動開",
        "auto off" => "自動關",
        _ => return None,
    })
}
