pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Loading..." => "読み込み中…",
        "No entries." => "項目がありません。",
        "No matching entry." => "一致する項目がありません。",
        "Issues" => "問題",
        "sources" => "ソース",
        "skills" => "スキル",
        "hooks" => "フック",
        "scheduled jobs" => "スケジュール済みジョブ",
        "command rules" => "コマンドルール",
        "enabled" => "有効",
        "shadowed by {name}" => "{name} が優先",
        "Reloaded." => "再読み込みしました。",
        "Enabled {name}." => "{name} を有効にしました。",
        "Disabled {name}." => "{name} を無効にしました。",
        "Reloaded {area}: {entries} entries, {disabled} disabled." => {
            "{area} を再読み込みしました：{entries} 件、{disabled} 件が無効。"
        }
        "Reloaded {area}: {entries} entries." => "{area} を再読み込みしました：{entries} 件。",
        "Hooks" => "フック",
        "invalid" => "無効",
        "Saved." => "保存しました。",
        "Saved {name}." => "{name} を保存しました。",
        "No change." => "変更はありません。",
        "Your text is kept in {path}." => "入力した内容は {path} に残しています。",
        "The user's own document; always in effect." => {
            "ユーザー自身のドキュメント。常に有効です。"
        }
        "There is no hooks file there." => "そこにフックファイルはありません。",
        "Owned by plugin {plugin}." => "プラグイン {plugin} が所有しています。",
        "The user document turns every hook off (disableAllHooks)." => {
            "ユーザードキュメントがすべてのフックを無効にしています（disableAllHooks）。"
        }
        "Enter toggle" => "Enter 切り替え",
        "Ctrl+E edit" => "Ctrl+E 編集",
        "Ctrl+N add" => "Ctrl+N 作成",
        "Ctrl+R reload" => "Ctrl+R 再読み込み",
        "Ctrl+T run" => "Ctrl+T 実行",
        "Ctrl+X remove" => "Ctrl+X 削除",
        "Tab inspect" => "Tab 詳細",
        "Remove {name}? Enter confirm · Esc cancel" => {
            "{name} を削除しますか？Enter で確定 · Esc でキャンセル"
        }
        "↑↓ scroll · Esc back" => "↑↓ スクロール · Esc で戻る",
        "Added." => "作成しました。",
        "Removed {name}." => "{name} を削除しました。",
        "Ran {name}." => "{name} を実行しました。",
        "The user document changed since it was read; nothing was written." => {
            "ユーザードキュメントは読み取り後に変更されています。今回は何も書き込んでいません。"
        }
        "The job changed since it was read; nothing was written." => {
            "ジョブは読み取り後に変更されています。今回は何も書き込んでいません。"
        }
        "The jobs changed since they were read; nothing was written." => {
            "ジョブは読み取り後に変更されています。今回は何も書き込んでいません。"
        }
        "Your next edit opens the current document." => {
            "次回の編集では現在のドキュメントを開きます。"
        }
        "Your next add reopens your text." => "次回の作成では入力した内容を開きます。",
        "Scheduler" => "スケジューラー",
        "jobs" => "ジョブ",
        "Added {name}." => "{name} を作成しました。",
        "Run {name}: {outcome}." => "{name} を実行しました：{outcome}。",
        "Recent runs of {name}" => "{name} の最近の実行",
        "No run yet." => "まだ実行はありません。",
        "No next run." => "次回の実行はありません。",
        "Next run {instant}." => "次回の実行 {instant}。",
        "Last run {outcome} at {instant}." => "前回の実行 {outcome}（{instant}）。",
        "every {minutes} minutes" => "{minutes} 分ごと",
        "at {instant}" => "{instant} に",
        "cron {expression}" => "cron {expression}",
        "cron {expression} in {zone}" => "cron {expression}（{zone}）",
        "The job changed since it was read; nothing was removed." => {
            "ジョブは読み取り後に変更されています。今回は何も削除していません。"
        }
        "The Scheduler has no area document." => "スケジューラーには領域ドキュメントがありません。",
        "No scheduled job has that id." => "その id のスケジュールジョブはありません。",
        "This daemon cannot manage {area}." => "{area} を管理できません。",
        "This daemon lists {area} but cannot toggle them." => {
            "{area} の一覧は表示できますが、切り替えはできません。"
        }
        "This daemon lists {area} but cannot edit them." => {
            "{area} の一覧は表示できますが、編集はできません。"
        }
        "This daemon lists {area} but cannot reload them." => {
            "{area} の一覧は表示できますが、再読み込みはできません。"
        }
        "This daemon lists {area} but cannot add them." => {
            "{area} の一覧は表示できますが、追加はできません。"
        }
        "This daemon lists {area} but cannot remove them." => {
            "{area} の一覧は表示できますが、削除はできません。"
        }
        "This daemon lists {area} but cannot run them." => {
            "{area} の一覧は表示できますが、実行はできません。"
        }
        "This daemon lists {area} but cannot inspect them." => {
            "{area} の一覧は表示できますが、詳細や履歴は見られません。"
        }
        "The document deleted {field}, which the owner cannot clear; nothing was written." => {
            "ドキュメントから {field} が削除されましたが、所有者は消去できません。何も書き込みませんでした。"
        }
        "The document changes no field; nothing was written." => {
            "ドキュメントはどのフィールドも変更していません。何も書き込みませんでした。"
        }
        "This workspace's hook settings changed since they were read; nothing was written." => {
            "このワークスペースのフック設定は読み取り後に変更されています。今回は何も書き込んでいません。"
        }
        "MCP" => "MCP",
        "MCP servers" => "MCP サーバー",
        "Ctrl+T test" => "Ctrl+T テスト",
        "sign-in required" => "サインインが必要",
        "signed in" => "サインイン済み",
        "sign-in expired" => "サインインの期限切れ",
        "missing scopes: {scopes}" => "不足スコープ: {scopes}",
        "Executable: {command}" => "実行ファイル: {command}",
        "Stored arguments (hidden): {count}" => "保存済み引数（非表示）: {count}",
        "Working directory: {cwd}" => "作業ディレクトリ: {cwd}",
        "Environment names: {names}" => "環境変数名: {names}",
        "Endpoint: {endpoint}" => "エンドポイント: {endpoint}",
        "Header names: {names}" => "ヘッダー名: {names}",
        "Bearer token: {state}" => "Bearer トークン: {state}",
        "OAuth: configured (client ID {id})" => "OAuth: 設定済み (クライアント ID {id})",
        "OAuth: configured" => "OAuth: 設定済み",
        "OAuth: not configured" => "OAuth: 未設定",
        "Metadata URL: {state}" => "メタデータ URL: {state}",
        "configured" => "設定済み",
        "Owned by a plugin; change it through Plugins." => {
            "プラグインが所有しています。プラグインから変更してください。"
        }
        "MCP entry {id}" => "MCP エントリ {id}",
        "Connection {number} · {status}" => "接続 {number} · {status}",
        "Tools have not been discovered." => "ツールはまだ検出されていません。",
        "No live session connections." => "有効なセッション接続はありません。",
        "Tools of {id}" => "{id} のツール",
        "The server exposes no tools." => "サーバーはツールを公開していません。",
        "The add document needs an id." => "追加ドキュメントには id が必要です。",
        "The add document needs a source of user or project." => {
            "追加ドキュメントの source は user または project です。"
        }
        "The add document needs a configuration object." => {
            "追加ドキュメントには configuration オブジェクトが必要です。"
        }
        "{id} is already in mcp list; remove it first or choose another id." => {
            "{id} はすでに mcp list にあります。先に削除するか別の id を選んでください。"
        }
        "{id} is already in mcp list; choose another id, or change it through its plugin." => {
            "{id} はすでに mcp list にあります。別の id を選ぶか、プラグイン経由で変更してください。"
        }
        "This entry's settings changed since they were read; nothing was written." => {
            "このエントリの設定は読み取り後に変更されました。何も書き込まれていません。"
        }
        "The MCP entry changed since it was read; nothing was written." => {
            "MCP エントリは読み取り後に変更されました。何も書き込まれていません。"
        }
        "The MCP entries changed since they were read; nothing was written." => {
            "MCP エントリは読み取り後に変更されました。何も書き込まれていません。"
        }
        "The MCP entry changed since it was read; nothing was removed." => {
            "MCP エントリは読み取り後に変更されました。何も削除されていません。"
        }
        "MCP's documents belong to one entry; this save names none." => {
            "MCP のドキュメントはエントリごとです。この保存はエントリを指定していません。"
        }
        "No MCP server has that id." => "その id の MCP サーバーはありません。",
        "This workspace has no project MCP source." => {
            "このワークスペースにはプロジェクトの MCP ソースがありません。"
        }
        "Enabled {id} ({source})." => "{id}（{source}）を有効にしました。",
        "Disabled {id} ({source})." => "{id}（{source}）を無効にしました。",
        "It runs this workspace's .mcp.json entry, including later edits to it, in active and new sessions." => {
            "アクティブなセッションと新しいセッションで、このワークスペースの .mcp.json エントリ（以降の編集を含む）を実行します。"
        }
        "{id} is not in mcp list; withdrew any stored enablement of it in this workspace." => {
            "{id} は mcp list にありません。このワークスペースに保存された有効化を取り消しました。"
        }
        "The MCP catalog could not be read ({error}); withdrew this workspace's enablement of {id}. A user entry with this ID is unchanged." => {
            "MCP カタログを読み取れませんでした（{error}）。このワークスペースでの {id} の有効化を取り消しました。同じ ID のユーザーエントリは変更されていません。"
        }
        "MCP configuration: {error}" => "MCP 設定: {error}",
        "The server rejected the stored credential; sign in again with mcp login." => {
            "サーバーが保存済みの資格情報を拒否しました。mcp login で再度サインインしてください。"
        }
        "The server requires additional scopes: {scopes}." => {
            "サーバーは追加のスコープを要求しています: {scopes}。"
        }
        "The server requires authorization; run mcp login ID." => {
            "サーバーは認証が必要です。mcp login ID を実行してください。"
        }
        "The probe ended: {status}." => "プローブが終了しました: {status}。",

        "Command rules" => "コマンドルール",
        "user" => "ユーザー",
        "Matches only this command." => "このコマンドだけに一致します。",
        "Matches commands whose first words are these." => {
            "先頭の語がこれと一致するコマンドに一致します。"
        }
        "Allows a simple command that would otherwise ask." => {
            "本来確認される単純なコマンドを許可します。"
        }
        "Denies the command under every preset." => {
            "すべてのプリセットでこのコマンドを拒否します。"
        }
        "Applies in every Session, in every workspace." => {
            "すべてのセッション、すべてのワークスペースで適用されます。"
        }
        "Rule added: {rule}." => "ルールを追加しました：{rule}。",
        "Rule already present: {rule}." => "ルールは既に存在します：{rule}。",
        "Rule removed: {rule}." => "ルールを削除しました：{rule}。",
        "That command rule is no longer listed; nothing was removed." => {
            "そのコマンドルールは一覧にありません。何も削除されていません。"
        }
        "Command rules changed since they were read; nothing was removed." => {
            "コマンドルールは読み込み後に変更されました。何も削除されていません。"
        }
        "A command rule is DECISION MATCH COMMAND, such as deny prefix git push." => {
            "コマンドルールは DECISION MATCH COMMAND の形式で、例：deny prefix git push。"
        }
        "Command: {command}" => "コマンド: {command}",
        "Use the line prompt or dimcode permissions." => {
            "行内プロンプトまたは dimcode permissions で使用してください。"
        }
        _ => return None,
    })
}
