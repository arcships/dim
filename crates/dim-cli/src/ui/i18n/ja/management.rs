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
        "models" => "モデル",
        "workflow definitions" => "ワークフロー定義",
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
        "Remove {name}?" => "{name} を削除しますか？",
        "↑↓ scroll · Esc back" => "↑↓ スクロール · Esc で戻る",
        "Added." => "作成しました。",
        "Removed {name}." => "{name} を削除しました。",
        "unavailable" => "利用不可",
        "Reset {name}?" => "{name} をリセットしますか？",
        "Enter confirm" => "Enter で確定",
        "Esc cancel" => "Esc でキャンセル",
        "Reset {name} to its catalog entry." => "{name} をカタログ項目にリセットしました。",
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
        "A scheduled job's run takes no input document." => {
            "スケジュールされたジョブの実行は入力ドキュメントを取りません。"
        }
        "An MCP server's test takes no input document." => {
            "MCP サーバーのテストは入力ドキュメントを受け取りません。"
        }
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
        "Models" => "モデル",
        "catalog" => "カタログ",
        "overrides" => "オーバーライド",
        "added" => "追加",
        "Models · {name}" => "モデル · {name}",
        "undeclared" => "未宣言",
        "Context window {window} · max output {output}" => {
            "コンテキストウィンドウ {window} · 最大出力 {output}"
        }
        "Endpoint {endpoint}" => "エンドポイント {endpoint}",
        "Endpoint {endpoint} · override {value}" => {
            "エンドポイント {endpoint} · オーバーライド {value}"
        }
        "The provider reports it unavailable." => "プロバイダーは利用不可と報告しています。",
        "Limits undeclared; edit the model to declare them." => {
            "上限が未宣言です。モデルを編集して宣言してください。"
        }
        "Sign in to the account to use its models." => {
            "アカウントにサインインするとモデルを使えます。"
        }
        "The signed-in account changed; reload to list its models." => {
            "サインイン中のアカウントが変わりました。再読み込みでモデルを一覧します。"
        }
        "The provider has no catalog yet." => "このプロバイダーにはまだカタログがありません。",
        "The provider listed no models." => "プロバイダーはモデルを何も一覧しませんでした。",
        "Listed {count} models." => "{count} 個のモデルを一覧しました。",
        "Refreshed the models." => "モデルを更新しました。",
        "Could not list models: {error}. Showing the stored list." => {
            "モデルを一覧できませんでした：{error}。保存済みの一覧を表示します。"
        }
        "The signed-in account changed; the refresh cleared preferences, overrides and disables." => {
            "サインイン中のアカウントが変わりました。更新により設定、オーバーライド、無効化が消去されました。"
        }
        "The models changed since they were read; nothing was removed." => {
            "モデルは読み取り後に変更されています。今回は何も削除していません。"
        }
        "The models changed since they were read; nothing was written." => {
            "モデルは読み取り後に変更されています。今回は何も書き込んでいません。"
        }
        "No provider has that id." => "その id のプロバイダーはありません。",
        "{name} is unavailable ({reason}); its enablement is kept as it is." => {
            "{name} は利用できません（{reason}）。有効状態はそのまま維持されます。"
        }
        "No model has that id." => "その id のモデルはありません。",
        "This model cannot be removed." => "このモデルは削除できません。",
        "A Provider's models have no area document." => {
            "プロバイダーのモデルには領域ドキュメントがありません。"
        }
        "Models have no run operation." => "モデルに実行操作はありません。",
        "The document is not an object; nothing was written." => {
            "ドキュメントがオブジェクトではありません。今回は何も書き込んでいません。"
        }
        "The document changes no editable field; nothing was written." => {
            "ドキュメントは編集可能なフィールドを変更していません。今回は何も書き込んでいません。"
        }
        "The model document names no model." => "モデルドキュメントがモデルを指定していません。",
        "The provider has no model catalog to write." => {
            "このプロバイダーには書き込めるモデルカタログがありません。"
        }
        "ID {id}" => "ID {id}",
        "Source {source}" => "ソース {source}",
        "Status {status}" => "ステータス {status}",
        "Tools {value}" => "ツール {value}",
        "Vision {value}" => "ビジョン {value}",
        "Reasoning {value}" => "推論 {value}",
        "Modalities {list}" => "モダリティ {list}",
        "Description {text}" => "説明 {text}",
        "Release date {date}" => "リリース日 {date}",
        "Pricing {value}" => "価格 {value}",
        "Rate limit {value}" => "レート制限 {value}",
        "Matched provider {id}" => "一致プロバイダー {id}",
        "Matched model {id}" => "一致モデル {id}",
        "Match strategy {strategy}" => "一致戦略 {strategy}",
        "Capability overrides {fields}" => "ケイパビリティの上書き {fields}",
        "yes" => "はい",
        "no" => "いいえ",
        "not supported" => "非対応",
        "supported" => "対応",

        "Workflow" => "ワークフロー",
        "Saved {name}. It stays disabled until you enable it." => {
            "{name} を保存しました。有効にするまで無効のままです。"
        }
        "The definitions changed since they were read; nothing was written." => {
            "定義は読み取り後に変更されました。何も書き込まれていません。"
        }
        "A definition's name is its key; add it to save it under another name." => {
            "定義の名前はそのキーです。別の名前で保存するには追加してください。"
        }
        "No project definition is named {name}." => "{name} というプロジェクト定義はありません。",
        "No workflow definition is named {name}." => "{name} というワークフロー定義はありません。",
        "Saved in the user workflow source; always enabled." => {
            "ユーザーのワークフローソースに保存されています。常に有効です。"
        }
        "Starts only after you enable it." => "有効にした後にのみ開始します。",
        "Defined by package {package}." => "パッケージ {package} によって定義されています。",
        "{name} — {description} (required)" => "{name} — {description}（必須）",
        "{name} — {description}" => "{name} — {description}",
        "{name} — required" => "{name} — 必須",
        "{name} — default: {default}" => "{name} — 既定: {default}",
        "and {count} more" => "ほか {count} 件",
        "Recent runs" => "最近の実行",
        "A workflow definition is saved as an object with a source and a document." => {
            "ワークフロー定義は、ソースとドキュメントを持つオブジェクトとして保存します。"
        }
        "A definition is saved to the user source or the project source." => {
            "定義はユーザーソースまたはプロジェクトソースに保存します。"
        }
        "Started {name}: {run_id}." => "{name} を開始しました: {run_id}。",
        "Parameters are a JSON object of strings." => {
            "パラメータは文字列の JSON オブジェクトです。"
        }
        "The list reached its size limit; later definitions are not shown." => {
            "一覧はサイズ上限に達したため、以降の定義は表示されません。"
        }
        "No workflow definition named {name} is in the list, which reached its size limit." => {
            "サイズ上限に達した一覧に、{name} という名前のワークフロー定義はありません。"
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
        "{id} is in more than one MCP source: {sources}. Name one with --source." => {
            "{id} は複数の MCP ソースにあります: {sources}。--source でいずれかを指定してください。"
        }
        "The action names no MCP source; a row carries the source its owner reported." => {
            "この操作は MCP ソースを指定していません。行には所有者が報告したソースが付きます。"
        }
        "No MCP server {id} is in source {source}." => {
            "ソース {source} に MCP サーバー {id} はありません。"
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
        "Tool call timeout: {millis} ms" => "ツール呼び出しのタイムアウト：{millis} ミリ秒",
        "Variables: {names}" => "変数：{names}",
        "Unset variables: {names}" => "未設定の変数：{names}",
        "The MCP source changed since the preview; no entry was imported, and an entry it would have replaced may now be disabled. Preview again." => {
            "MCP ソースがプレビュー後に変更されました。エントリはインポートされておらず、置き換えるはずだったエントリは無効になっている可能性があります。もう一度プレビューしてください。"
        }
        "The project entries were written and their withheld values were not; replace them again to retry." => {
            "プロジェクトエントリは書き込まれましたが、保留された値は書き込まれていません。もう一度置き換えて再試行してください。"
        }
        "Replaced {id}." => "{id} を置き換えました。",
        "Added {id}." => "{id} を追加しました。",
        "Delivers to Session {session} branch {branch} ({mode})." => {
            "セッション {session}、ブランチ {branch}（{mode}）に配信します。"
        }
        "Launches a new Session in {cwd}." => "{cwd} で新しいセッションを起動します。",
        "Disabled: {code} at {instant}." => "無効：{instant} の {code}。",
        "The definition changed since it was read; nothing was written." => {
            "定義は読み取り後に変更されました。何も書き込まれていません。"
        }
        "Columns: ID, client, scope, path, state, entries or error." => {
            "列：ID、クライアント、スコープ、パス、状態、エントリ数またはエラー。"
        }
        "No value from a store is read out or printed; --from ID previews one inside the daemon." => {
            "ストアから値を読み出したり出力したりしません。--from ID はデーモン内で 1 件をプレビューします。"
        }
        "Columns: ID, action, destination ID.\nA renamed entry is written under its destination id; the source's entry with the old id is left as it was." => {
            "列：ID、操作、移動先 ID。\n名前を変更したエントリは移動先 id で書き込まれ、ソースにある古い id のエントリはそのまま残ります。"
        }
        "Imported and replaced entries start disabled: a user entry is written with enabled: false, and a project entry has no enablement record until you enable it locally." => {
            "インポートおよび置き換えられたエントリは無効な状態で始まります。ユーザーエントリは enabled: false で書き込まれ、プロジェクトエントリはローカルで有効にするまで有効化レコードを持ちません。"
        }
        "This daemon cannot list other clients' MCP stores." => {
            "このデーモンは他のクライアントの MCP ストアを一覧できません。"
        }
        "This daemon cannot import MCP servers." => {
            "このデーモンは MCP サーバーをインポートできません。"
        }
        "The document holds no mcpServers entries." => {
            "ドキュメントに mcpServers エントリはありません。"
        }
        "Columns: ID, status, transport, changed fields, unsupported fields, error.\nA changed field names what a write would change and the kind of value it holds; no value is printed.\nAn unsupported field is kept unused or dropped, as shown after the field." => {
            "列：ID、状態、トランスポート、変更フィールド、未対応フィールド、エラー。\n変更フィールドは、書き込みで何が変わるかと値の種類を示します。値は表示されません。\n未対応フィールドは、フィールドの後ろの表示どおり未使用のままか破棄されます。"
        }
        "A project entry is written as its public skeleton: the workspace file gets the command, declared cwd, URL origin and public OAuth fields. Its arguments, environment and header values, bearer token, a URL with userinfo, path, query or fragment, and unknown members go to your user-scope mcp-secrets record, never to the workspace file." => {
            "プロジェクトエントリは公開される骨格だけが書き込まれます。ワークスペースファイルにはコマンド、宣言された cwd、URL のオリジン、公開 OAuth フィールドが入ります。引数、環境変数とヘッダーの値、bearer トークン、userinfo・パス・クエリ・フラグメントを含む URL、未知のメンバーはユーザースコープの mcp-secrets 記録に書き込まれ、ワークスペースファイルには書き込まれません。"
        }
        "{id} has {count} more field names than the lists above show." => {
            "{id} には上の一覧に表示されないフィールド名がさらに {count} 個あります。"
        }
        "Top-level members the import does not carry: none." => {
            "インポートが引き継がないトップレベルメンバー：なし。"
        }
        "Top-level members the import does not carry: {names}." => {
            "インポートが引き継がないトップレベルメンバー：{names}。"
        }
        "Applying the selection at this revision." => "このリビジョンで選択内容を適用しています。",
        "An unchanged entry was already in the source and was not written." => {
            "変更のないエントリは既にソースにあり、書き込まれていません。"
        }
        _ => return None,
    })
}
