pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Refresh model list" => "モデル一覧を更新",
        "Model list refreshed" => "モデル一覧を更新しました",
        "Model list refresh failed: {reason}" => "モデル一覧を更新できませんでした: {reason}",
        "Refreshing model list… · esc close" => "モデル一覧を更新中… · Esc で閉じる",
        // providers_panel.rs
        "Providers" => "プロバイダー",
        "Enter open · Esc clear/close" => "Enter 開く · Esc 消去/閉じる",
        "↑↓/Tab move · Enter activate · Esc back" => "↑↓/Tab 移動 · Enter 実行 · Esc 戻る",
        "Loading providers..." => "プロバイダーを読込中…",
        "No matching providers." => "一致するプロバイダーなし。",
        "Custom providers are not available yet" => "カスタムプロバイダーは未対応",
        "Testing connection..." => "接続テスト中…",
        "Connect {providerId}" => "{providerId} に接続",
        "(default)" => "（既定）",
        "(required)" => "（必須）",
        "Test + apply" => "テスト+適用",
        "Testing sends the stored credential to this URL." => {
            "テスト時、保存済み認証情報をこの URL に送信。"
        }
        "Disconnect" => "切断",
        "Enable provider" => "プロバイダー有効化",
        "Disable provider" => "プロバイダー無効化",
        "Error: {error}" => "エラー：{error}",
        "disabled" => "無効",
        "current" => "現在",
        "Sign in" => "サインイン",
        "ready" => "準備完了",
        "error" => "エラー",
        "not connected" => "未接続",
        "Add custom provider" => "カスタムプロバイダー追加",
        // oauth_view.rs
        "Checking account…" => "アカウント確認中…",
        "Not signed in" => "未サインイン",
        "Signed in as {account}" => "{account} でサインイン中",
        "Open models" => "モデル一覧を開く",
        "View plan" => "プランを表示",
        "Sign out" => "サインアウト",
        "Checking usage…" => "使用量確認中…",
        "Usage: {reason}" => "使用量：{reason}",
        "Credits" => "クレジット",
        "unlimited" => "無制限",
        "{hours}h window" => "{hours} 時間枠",
        "/month" => "/月",
        "/year" => "/年",
        "expires {date}" => "期限 {date}",
        "resets {date}" => "{date} にリセット",
        "↑↓ move · enter activate · ctrl+r refresh · esc close" => {
            "↑↓ 移動 · enter 実行 · ctrl+r 更新 · esc 閉じる"
        }
        "Login with DimAgent" => "DimAgent でログイン",
        "Enterprise login" => "企業ログイン",
        "Device login" => "デバイスログイン",
        "Email" => "メール",
        "Continue" => "続行",
        "Browser login" => "ブラウザログイン",
        "Restart device login" => "デバイスログインをやり直し",
        "Select to copy · Enter opens link" => "選択してコピー · Enter でリンクを開く",
        "Link shortened · Enter opens link" => "リンクを省略 · Enter で全体を開く",
        "Callback URL" => "コールバック URL",
        "Submit callback" => "コールバック送信",
        "The browser could not be opened. Open the link manually, then return here." => {
            "ブラウザを開けません。リンクを手動で開き、ここへ戻る"
        }
        "Waiting for authorization" => "認可待ち",
        "{name} device login" => "{name} デバイスログイン",
        "Open this URL: {url}" => "この URL を開く：{url}",
        "Enter code:    {code}" => "コードを入力：{code}",
        "Waiting after server slow-down" => "サーバー減速要求で待機中",
        "OAuth login cancelled" => "OAuth ログインをキャンセル",
        "Enter your work email" => "仕事用メールを入力",
        "Invalid OAuth state - possible CSRF attempt" => "無効な OAuth state - CSRF の可能性",
        "Preparing login... · esc cancel" => "ログイン準備中… · esc キャンセル",
        "Completing login... · esc cancel" => "ログイン処理中… · esc キャンセル",
        "enter restart · esc back" => "enter やり直し · esc 戻る",
        "type value · ↑↓/tab move · enter submit · esc cancel" => {
            "値を入力 · ↑↓/tab 移動 · enter 送信 · esc キャンセル"
        }
        "↑↓ move · enter activate · esc back" => "↑↓ 移動 · enter 実行 · esc 戻る",
        "↑↓ move · enter activate · esc close" => "↑↓ 移動 · enter 実行 · esc 閉じる",
        "(paste here)" => "（ここに貼り付け）",
        _ => return None,
    })
}
