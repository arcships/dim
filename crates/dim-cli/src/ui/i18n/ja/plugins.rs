pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Plugins" => "プラグイン",
        "Enter on/off · r reload · Ctrl+R refresh · Esc close" => {
            "Enter オン/オフ · r 再読み込み · Ctrl+R 更新 · Esc 閉じる"
        }
        "Locked: Dim needs it, so it cannot be turned off." => {
            "ロック中：Dim に必要なため、オフにできません。"
        }
        "Loading plugins..." => "プラグインを読み込み中…",
        "No plugins." => "プラグインはありません。",
        "Working..." => "処理中…",
        "locked" => "ロック中",
        "Uses {use}" => "使用中 {use}",
        "Candidates: {candidates}" => "候補：{candidates}",
        "Waiting for: {names}" => "待機中：{names}",
        "Refused {kind}: {error}" => "{kind} が拒否されました：{error}",
        "Turn off {name}?" => "{name} をオフにしますか？",
        "Its tools and content stop in new turns until you turn it on again." => {
            "オンに戻すまで、新しいターンではそのツールと内容が使えなくなります。"
        }
        "Turn off" => "オフにする",
        "Turned on {name}." => "{name} をオンにしました。",
        "Turned off {name}." => "{name} をオフにしました。",
        "Reloaded {name}." => "{name} を再読み込みしました。",
        "Installed {name} ({id})." => "{name}（{id}）をインストールしました。",
        "Installed {name} ({id}); it is not turned on." => {
            "{name}（{id}）をインストールしました。オンにはなっていません。"
        }
        "Installing {path}..." => "{path} をインストール中…",
        "Restart required: it loads after the App Server restarts (dimcode daemon stop)." => {
            "再起動が必要です：App Server の再起動後に読み込まれます（dimcode daemon stop）。"
        }
        "Skipped {kinds}: their owner is not running." => {
            "{kinds} をスキップしました：担当モジュールが動いていません。"
        }
        "Usage: /plugins [install PATH]" => "使い方：/plugins [install パス]",
        "list plugins, turn them on or off, reload (install PATH)" => {
            "プラグインの一覧、オン/オフ、再読み込み（install パス）"
        }
        _ => return None,
    })
}
