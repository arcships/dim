pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "This daemon does not serve plugin runtimes ({version})" => {
            "このデーモンはプラグインランタイムを提供しません（{version}）"
        }
        "Plugin runtimes" => "プラグインランタイム",
        "Enter install or apply · s source · a auto · r retry · Esc close" => {
            "Enter インストール/適用 · s 取得元 · a 自動 · r 再試行 · Esc 閉じる"
        }
        "Esc close · the acquisition keeps running if you close this" => {
            "Esc 閉じる · 閉じても取得は続きます"
        }
        "Enter retry · s switch {name} to {source} · Esc close" => {
            "Enter 再試行 · s {name} を {source} に切り替え · Esc 閉じる"
        }
        "Loading plugin runtimes..." => "プラグインランタイムを読み込み中…",
        "acquiring" => "取得中",
        "starting" => "起動中",
        "down" => "停止",
        "1 plugin waits for {runtime}: {names}" => "1 個のプラグインが {runtime} を待機中：{names}",
        "{count} plugins wait for {runtime}: {names}" => {
            "{count} 個のプラグインが {runtime} を待機中：{names}"
        }
        "show, install or configure the plugin runtimes" => {
            "プラグインランタイムの表示、インストール、設定"
        }
        "Usage: /runtime [install NAME | source NAME download|system | auto NAME on|off]" => {
            "使い方：/runtime [install 名前 | source 名前 download|system | auto 名前 on|off]"
        }
        "Enter install or apply · s source · r retry · Esc close" => {
            "Enter インストール/適用 · s 取得元 · r 再試行 · Esc 閉じる"
        }
        "auto on" => "自動オン",
        "auto off" => "自動オフ",
        _ => return None,
    })
}
