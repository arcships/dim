pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Queue" => "キュー",
        "Export session" => "セッションをエクスポート",
        "Enter steer · x delete held · Esc close" => {
            "Enter steer · x 保留中のメッセージを削除 · Esc 閉じる"
        }
        "No queued messages." => "キューにメッセージはありません。",
        "Held · awaiting application" => "保留中 · 適用待ち",
        "Held" => "保留中",
        "Unread" => "未読",
        "Enter export · Esc cancel" => "Enter エクスポート · Esc キャンセル",
        "Messages with attachments stay queued; use /queue to steer or delete them" => {
            "添付ファイル付きのメッセージはキューに残ります。/queue で steer または削除してください"
        }
        "/queue · Shift+← edit last · empty: {key} steer · Ctrl+Enter send now" => {
            "/queue · Shift+← 最後を編集 · 下書きが空なら {key} で steer · Ctrl+Enter で即送信"
        }
        "+{count} more · {hint}" => "+{count} 件 · {hint}",
        "steer or delete queued messages" => "キューのメッセージを steer または削除",
        "export this conversation" => "この会話をエクスポート",
        "That message has already left the queue." => {
            "そのメッセージはすでにキューから外れています。"
        }
        "Exported {format}: {path}" => "{format} をエクスポートしました: {path}",
        // tui.rs
        "Enter output / interrupt member · x stop command · Esc close" => {
            "Enter 出力/メンバー中断 · x コマンド停止 · Esc 閉じる"
        }
        "Enter output / interrupt member · x stop command · Ctrl+X stop all · Esc close" => {
            "Enter 出力/メンバー中断 · x コマンド停止 · Ctrl+X 全停止 · Esc 閉じる"
        }
        "Loading branch history... Stop-all unavailable" => "ブランチ履歴を読込中… 全停止不可",
        "Branch history failed; reopen to retry" => "ブランチ履歴の取得失敗。開き直して再試行",
        "Branch history is incomplete; stop-all unavailable" => "ブランチ履歴が不完全。全停止不可",
        "Running" => "実行中",
        "Lines" => "ライン",
        "Enter show the line · x end it · Esc close" => "Enter ライン表示 · x 終了 · Esc 閉じる",
        "This session has no lines." => "このセッションにはまだラインがありません。",
        "No matching lines." => "一致するラインがありません。",
        "the session itself" => "セッション自体",
        "working" => "作業中",
        "ended itself" => "自ら終了",
        "ended by an operation" => "操作で終了",
        "ended with {id}" => "{id} とともに終了",
        "creation failed" => "作成失敗",
        "shown" => "表示中",
        "line" => "ライン",
        "line {id}" => "ライン {id}",
        "jump to a message of the shown line" => "表示中のラインのメッセージへ移動",
        "Timeline" => "タイムライン",
        "Enter actions · Esc close" => "Enter 操作 · Esc 閉じる",
        "Timeline actions" => "タイムライン操作",
        "↑↓ select · Enter confirm · Esc cancel" => "↑↓ 選択 · Enter 確認 · Esc キャンセル",
        "Jump to message" => "メッセージへ移動",
        "Edit as a new draft" => "新しい下書きとして編集",
        "Quote message" => "メッセージを引用",
        "Create branch from here" => "ここからブランチを作成",
        "Before this message" => "このメッセージの前",
        "Fork session" => "セッションをフォーク",
        "1. Create fork" => "1. フォークを作成",
        "2. Cancel" => "2. キャンセル",
        "Creates a new session; the current session stays unchanged." => {
            "新しいセッションを作成します。現在のセッションは変更されません。"
        }
        "No messages yet." => "メッセージはまだありません。",
        "Already on screen above; /tui jumps to it" => "すでに上に表示中。/tui で移動できます",
        "{count} earlier messages · Enter or click to load" => {
            "過去のメッセージ {count} 件 · Enter かクリックで読込"
        }
        "follow, talk to and end this session's lines" => {
            "このセッションのラインを追い、話しかけ、終了"
        }
        "End line {id}?" => "{id} のラインを終了しますか？",
        "y end it" => "y 終了",
        "n cancel" => "n キャンセル",
        "main is the session itself; a line ends, main does not" => {
            "main はセッション自体です。ラインは終了できますが main は終了しません"
        }
        "Line has ended:" => "ラインは終了しました:",
        "Esc close" => "Esc 閉じる",
        "/{name} needs {argument}" => "/{name} には {argument} が必要です",
        "/{name} takes text only; remove {label} to send" => {
            "/{name} はテキストのみです。{label} を削除してから送信してください"
        }
        "e edit · b branch · f fork" => "e 編集 · b ブランチ · f フォーク",
        "e edit" => "e 編集",
        "Fork the session before “{message}”?" => {
            "「{message}」の前でセッションをフォークしますか？"
        }
        "Fork the session after the last turn?" => {
            "最後のターンの後でセッションをフォークしますか？"
        }
        "⑂ new line from {id} · Esc cancel" => "⑂ {id} から新しいライン · Esc キャンセル",
        "A fork copies main; b opens a line here" => {
            "フォークは main のみ複製します。b でここにラインを開けます"
        }
        "A line opens from text; images and files were not included" => {
            "ラインはテキストで開きます。画像とファイルは含まれません"
        }
        "No finished turn to cut at yet" => "切り出せる終了済みのターンがまだありません",
        "forked {id} · {count} messages" => "{id} をフォーク · {count} 件",
        "after message {count}" => "{count} 件目のメッセージの後",
        "open a line from the end of the shown line" => "表示中のラインの末尾からラインを開く",
        "fork the session at its last finished turn" => {
            "最後に終了したターンでセッションをフォーク"
        }
        "rename this session" => "このセッションの名前を変更",
        "Renamed to {title}" => "{title} に名前を変更しました",
        "The Session is now untitled." => "セッションはタイトルなしになりました。",
        "This daemon does not support renaming Sessions" => {
            "このデーモンはセッションの名前変更に対応していません"
        }
        "Renaming failed" => "名前の変更に失敗しました",
        "Delete \"{name}\"?" => "「{name}」を削除しますか?",
        "Deleted {name}." => "{name} を削除しました。",
        "Nothing was removed: no stored Session {name}." => {
            "何も削除されませんでした: 保存されたセッション {name} はありません。"
        }
        "Delete failed" => "削除に失敗しました",
        "This daemon does not support deleting Sessions" => {
            "このデーモンはセッションの削除に対応していません"
        }
        "Starting" => "起動中",
        "Delegation result" => "委任結果",
        "Enter details · x stop selected · Ctrl+X stop all · Esc close" => {
            "Enter 詳細 · x 選択を停止 · Ctrl+X 全停止 · Esc 閉じる"
        }
        "x interrupt member · Esc back" => "x メンバー中断 · Esc 戻る",
        "read-only" => "読取専用",
        "Member Session" => "メンバーセッション",
        "Loading conversation..." => "会話を読込中…",
        "No conversation yet" => "会話なし",
        "Observing conversation" => "会話を表示中",
        "Conversation unavailable" => "会話は利用不可",
        "Session closed" => "セッション終了",
        "Member conversation" => "メンバーの会話",
        "x stop command · Esc back" => "x コマンド停止 · Esc 戻る",
        "x stop command · Esc close" => "x コマンド停止 · Esc 閉じる",
        "x stop workflow · Esc close" => "x ワークフロー停止 · Esc 閉じる",
        "Esc back" => "Esc 戻る",
        "Tail preview - last 2,000 lines / 50 KiB" => "末尾プレビュー - 直近 2,000 行 / 50 KiB",
        "Loading output..." => "出力を読込中…",
        "No output yet" => "出力なし",
        "Live output unavailable" => "ライブ出力不可",
        "Live output unavailable in this profile" => "このプロファイルではライブ出力不可",
        "Output read failed" => "出力の読込失敗",
        "Completed" => "完了",
        "Failed" => "失敗",
        "Cancelled" => "キャンセル済み",
        "workflow" => "ワークフロー",
        "Stopping" => "停止中",
        "Stopped" => "停止済み",
        "Interrupted" => "中断済み",
        "last observed" => "最終観測",
        "Loading Workflow runs... Stop-all unavailable" => "ワークフロー実行を読込中… 全停止不可",
        "Workflow: last observed; refresh failed, reopen to retry" => {
            "ワークフロー：最終観測。更新失敗、開き直して再試行"
        }
        "Workflow runs unavailable; reopen to retry" => {
            "ワークフロー実行を取得不可。開き直して再試行"
        }
        "The selected Workflow run is no longer available" => {
            "選択したワークフロー実行はもうありません"
        }
        "Workflow inventory is incomplete; stop-all unavailable" => {
            "ワークフロー一覧が不完全。全停止不可"
        }
        "Workflow stop unavailable" => "ワークフロー停止不可",
        "Run no longer available; last observed record" => "実行はもうありません。最終観測の記録",
        "x stop workflow · Esc back" => "x ワークフロー停止 · Esc 戻る",
        "Run" => "実行",
        "Source" => "ソース",
        "Stages" => "ステージ",
        "result" => "結果",
        "Log" => "ログ",
        "Report" => "レポート",
        "waiting" => "待機中",
        "launching" => "起動中",
        "completed" => "完了",
        "failed" => "失敗",
        "stopped" => "停止済み",
        "skipped" => "スキップ",
        "lost" => "消失",
        "none" => "なし",
        "pending" => "保留中",
        "delivered" => "配信済み",
        "undeliverable" => "配信不可",
        "Agent is starting; no member Session to stop yet" => {
            "エージェント起動中。停止対象のメンバーはまだなし"
        }
        "Provider members are controlled by the leader; no stop here" => {
            "プロバイダーメンバーはリーダー管理。ここでは停止不可"
        }
        "Provider members have no conversation to open here" => {
            "プロバイダーメンバーの会話はここで開けません"
        }
        "That delegation is no longer in this transcript" => {
            "その委任はトランスクリプトにもうありません"
        }
        "Member model" => "メンバーモデル",
        "No model observed" => "モデル未観測",
        "Observing member..." => "メンバーを観測中…",
        "{model} (observation unavailable)" => "{model}（観測不可）",
        "{model} (Session closed)" => "{model}（セッション終了）",
        "{model} (last observed)" => "{model}（最終観測）",
        "Agent: {name}" => "エージェント：{name}",
        "Provider member: {name}" => "プロバイダーメンバー：{name}",
        "Task: {task}" => "タスク：{task}",
        "Activity: {activity}" => "活動：{activity}",
        "Activity: not observed on this attachment" => "活動：この接続では未観測",
        "Result: {result}" => "結果：{result}",
        "Controlled by the leader" => "リーダーが管理",
        "› Open member conversation" => "› メンバーの会話を開く",
        "Not started" => "未開始",
        "Reason: {reason}" => "理由：{reason}",
        "Copied to clipboard" => "コピーしました",
        "Opened link" => "リンクを開きました",
        "Link: {url}" => "リンク：{url}",
        "Not available" => "利用不可",
        "files changed  {count}" => "変更ファイル  {count}",
        "active session" => "アクティブセッション",
        "new-session draft" => "新規セッション下書き",
        "Tool approvals: {title} ({target})" => "ツール承認：{title}（{target}）",
        "Disconnected · draft kept" => "切断 · 下書き保持",
        "The App Server closed this interface: {reason}" => {
            "App Server が画面を閉じました：{reason}"
        }
        "Disconnected; draft kept. Exit and resume this Session." => {
            "切断。下書きは保持。終了してセッションを再開"
        }
        "Cannot open this Session; draft kept." => "このセッションは開けません。下書きは保持。",
        "Disconnected; draft kept. Exit and run: {command}" => {
            "切断。下書きは保持。終了して実行：{command}"
        }
        "Disconnected; command outcome unknown" => "切断。コマンド結果は不明",
        "Message queued" => "キューに追加",
        "Run a shell command · Esc to exit" => "shell コマンドを実行 · Esc 終了",
        "! shell mode" => "! shell モード",
        "!! shell mode · not sent to the model" => "!! shell モード · モデルへ未送信",
        "Leave shell mode to quote a message" => "引用するには shell モードを終了",
        "A shell command is already running" => "shell コマンド実行中",
        "Running… · Ctrl+C to interrupt · {seconds}s" => "実行中… · Ctrl+C 中断 · {seconds}s",
        "exit code {code}" => "終了コード {code}",
        "killed by {signal}" => "{signal} で強制終了",
        "interrupted" => "中断済み",
        "done" => "完了",
        "failed: {reason}" => "失敗：{reason}",
        "Shell output was not recorded: {reason}" => "Shell 出力は未記録：{reason}",
        "Shell: {reason}" => "Shell：{reason}",
        "Reading clipboard…" => "クリップボード読込中…",
        "Reading image…" => "画像を読み込み中…",
        "Wait for the image insertion before sending" => "画像の挿入完了後に送信",
        "The image could not be read" => "画像を読み込めませんでした",
        "An image is still being inserted; paste again when it is in" => {
            "画像を挿入中です。完了後にもう一度貼り付けてください"
        }
        "The clipboard is still being inserted; paste again when it is in" => {
            "クリップボードを挿入中です。完了後にもう一度貼り付けてください"
        }
        "Clipboard: {reason}" => "クリップボード：{reason}",
        "Wait for clipboard insertion before sending" => "クリップボード挿入の完了後に送信",
        "A submission accepts at most 20 images" => "送信できる画像は最大 20 枚",
        "first frame" => "先頭フレームのみ",
        "re-encoded" => "再エンコード",
        "The current model does not accept images; remove {label} to send" => {
            "このモデルは画像非対応。{label} を削除して送信"
        }
        "Files" => "ファイル",
        "Searching…" => "検索中…",
        "type filter · enter select · esc clear/close" => {
            "フィルタを入力 · enter 選択 · esc 消去/閉じる"
        }
        "↑↓ select · enter/tab insert · esc close" => "↑↓ 選択 · enter/tab 挿入 · esc 閉じる",
        "not sent to the model" => "モデルへ未送信",
        "… {count} more lines" => "… 残り {count} 行",
        "… {count} earlier lines" => "… 前に {count} 行",
        "Interrupt requested; Ctrl+C again to exit" => "中断を要求。再度 Ctrl+C で終了",
        "Press Ctrl+C again to exit" => "再度 Ctrl+C で終了",
        "User interrupted the run." => "ユーザーが中断しました。",
        "Could not save the language: {error}" => "言語を保存できません：{error}",
        "Language set to {locale}" => "言語を {locale} に設定",
        "Language set to system default" => "言語をシステム既定に設定",
        "Connected {provider}" => "{provider} に接続済み",
        "Opened {url}" => "{url} を開きました",
        "Tool details not saved: {error}" => "ツール詳細の保存失敗：{error}",
        "Thinking details not saved: {error}" => "思考詳細の保存失敗：{error}",
        "/{name} needs a session — send a message first" => {
            "/{name} はセッションが必要。先にメッセージを送信"
        }
        "Nothing is running" => "実行中の処理なし",
        "No queued message" => "キューは空",
        "Only in fullscreen (/tui)" => "全画面のみ（/tui）",
        "new session" => "新規セッション",
        "resumed {id} · {count} messages" => "{id} を再開 · {count} 件",
        " · earlier history: /timeline" => " · 過去の履歴：/timeline",
        "Error: {message}" => "エラー：{message}",
        "working…  {elapsed}" => "処理中…  {elapsed}",
        "thinking…  {elapsed}" => "思考中…  {elapsed}",
        "Ask anything... · Ctrl+J newline" => "何でも質問… · Ctrl+J 改行",
        "Earlier history · {id} · {count} messages" => "過去の履歴 · {id} · {count} 件",
        "No active session" => "アクティブセッションなし",
        "answer required" => "要回答",
        "permission required" => "要許可",
        "esc interrupt" => "esc 中断",
        "shift+← edit last · ctrl+s steer now" => "shift+← 最後をedit · ctrl+s 今すぐ steer",
        "+{count} more · shift+← edit last · ctrl+s steer now" => {
            "+{count} 件 · shift+← 最後をedit · ctrl+s 今すぐ steer"
        }
        "Queue ({count})" => "キュー（{count}）",
        "Thinking…" => "思考中…",
        "Thought" => "思考済み",
        "Thought for {duration}" => "{duration} 思考",
        "Welcome to DIM {version}!" => "DIM {version} へようこそ！",
        "Type / for commands · Ctrl+O for the palette" => "/ でコマンド · Ctrl+O でパレット",
        "Workspace" => "ワークスペース",
        "Git branch" => "Git ブランチ",
        "Bye!" => "さようなら！",
        "DIM Web is ready" => "DIM Web 準備完了",
        "Opened in your browser" => "ブラウザで開きました",
        "Open the sign-in link below" => "下のサインインリンクを開く",
        "Web" => "Web",
        "Sign-in link:" => "サインインリンク：",
        "Resume" => "再開",
        "Press Ctrl+C to stop the Web workspace." => "Ctrl+C で Web ワークスペースを停止。",
        "Feedback or ideas? Join us on Discord:" => "フィードバックやアイデアは Discord へ：",
        "To resume this session:" => "セッションの再開：",

        // commands.rs: descriptions
        "start a new session" => "新規セッションを開始",
        "show or set the reasoning effort" => "推論強度を表示・設定",
        "show or set the priority service tier (on|off)" => {
            "優先サービス階層を表示・設定（on|off）"
        }
        "list agents or switch this session's agent" => "エージェント一覧・切替",
        "list MCP servers, sign in or out (login|logout ID)" => {
            "MCP サーバー一覧・サインイン/アウト（login|logout ID）"
        }
        "resume a previous session" => "過去のセッションを再開",
        "turn the web interface on or off" => "Web インターフェースをオン・オフ",
        "show the context window usage" => "コンテキスト使用量を表示",
        "interrupt the run" => "実行を中断",
        "steer with the next queued message" => "次のキューメッセージで steer",
        "scroll to the top" => "先頭へスクロール",
        "scroll to the bottom" => "末尾へスクロール",
        "pick an earlier message to edit or quote" => "過去のメッセージを編集・引用",
        "cycle tool details and compaction summaries" => "ツール詳細と圧縮サマリーの表示を順に切替",
        "switch between fullscreen and scrollback" => "全画面/スクロールバック切替",
        "switch theme" => "テーマを切替",
        "switch language" => "言語を切替",
        "select a model" => "モデルを選択",
        "connect a provider" => "プロバイダーに接続",
        "sign in to DimAgent" => "DimAgent にサインイン",
        "DimAgent account · usage · sign out" => "DimAgent アカウント · 使用量 · サインアウト",
        "configure tool approvals" => "ツール承認を設定",
        "choose ask or full access" => "確認かフルアクセスを選ぶ",
        "open settings" => "設定を開く",
        "exit" => "終了",

        // composer/palette.rs
        "Default for new sessions in {folder}: {model}" => {
            "新規セッションの {folder} での既定モデル：{model}"
        }
        "Default for new sessions: {model}" => "新規セッションの既定モデル：{model}",
        "Enter select · Esc clear/close" => "Enter 選択 · Esc 消去/閉じる",
        "esc close" => "esc 閉じる",
        "Suggestions" => "候補",
        "Skills" => "スキル",
        "Skill loaded: {name}" => "スキル読み込み：{name}",
        "Hook context · {count} line" => "フックのコンテキスト · {count} 行",
        "Hook context · {count} lines" => "フックのコンテキスト · {count} 行",
        "Stop hook follow-up · {count} line" => "Stop フックのフォローアップ入力 · {count} 行",
        "Stop hook follow-up · {count} lines" => "Stop フックのフォローアップ入力 · {count} 行",
        "Commands" => "コマンド",
        "Command Palette" => "コマンドパレット",
        "No matches" => "一致なし",
        " (unavailable)" => "（利用不可）",

        // composer/mod.rs
        "[Text: {preview} ({count} chars)]" => "[テキスト：{preview}（{count} 文字）]",
        "[... {count} lines]" => "[... {count} 行]",
        "pasted text" => "貼り付けテキスト",

        // list_panel.rs

        // settings_panel.rs
        "on" => "オン",
        "off" => "オフ",
        "Interface" => "UI",
        "Theme" => "テーマ",
        "Language" => "言語",
        "Startup UI form" => "起動時の UI",
        "Default send mode" => "既定の送信モード",
        "fullscreen" => "全画面",
        "scrollback" => "スクロールバック",
        "Tool details" => "ツール詳細",
        "Thinking details" => "思考詳細",
        "expanded" => "展開",
        "collapsed" => "折りたたみ",
        "Mouse (fullscreen)" => "マウス（全画面）",
        "Permissions" => "権限",
        "Tool approvals" => "ツール承認",
        "Execution" => "実行",
        "Models" => "モデル",
        "Settings" => "設定",
        "↑↓ move · Enter select · ←→/Space change · Esc close" => {
            "↑↓ 移動 · Enter 選択 · ←→/Space 変更 · Esc 閉じる"
        }

        // language_panel.rs
        "↑↓ preview · Enter save · Esc restore" => "↑↓ プレビュー · Enter 保存 · Esc 復元",
        "System default ({locale})" => "システム既定（{locale}）",

        // theme_panel.rs
        "↑↓ preview · Enter apply · Esc cancel" => {
            "↑↓ プレビュー · Enter 適用 · Esc キャンセル"
        }
        "System" => "システム",
        "Could not save the theme: {error}" => "テーマを保存できません：{error}",

        // sessions_panel.rs
        "Sessions" => "セッション",
        "Sessions · This workspace" => "セッション · このワークスペース",
        "Sessions · This repository" => "セッション · このリポジトリ",
        "Sessions · All workspaces" => "セッション · すべてのワークスペース",
        "No saved sessions in this workspace. Ctrl+S shows more." => {
            "このワークスペースに保存済みセッションはありません。Ctrl+S で他のワークスペースを表示します。"
        }
        "Enter open · Ctrl+X delete · Ctrl+S scope · Ctrl+R refresh · Esc close" => {
            "Enter 開く · Ctrl+X 削除 · Ctrl+S 範囲 · Ctrl+R 更新 · Esc 閉じる"
        }
        "Loading sessions..." => "セッションを読込中…",
        "No saved sessions yet." => "保存済みセッションなし。",
        "No matching sessions." => "一致するセッションなし。",
        "now" => "たった今",
        "{count}m ago" => "{count}分前",
        "{count}h ago" => "{count}時間前",
        "{count}d ago" => "{count}日前",

        // models_panel.rs
        "Select a model" => "モデルを選択",
        "Loading models..." => "モデルを読込中…",
        "Previous page" => "前のページ",
        "Next page" => "次のページ",
        "No models available. Connect a provider first." => {
            "利用可能なモデルなし。先にプロバイダーに接続"
        }
        "No matching models." => "一致するモデルなし。",
        "current" => "現在",
        "Edit models" => "モデルを編集",
        "{provider}: 1 model unavailable · dimcode models {id}" => {
            "{provider}：1 モデル利用不可 · dimcode models {id}"
        }
        "{provider}: {count} models unavailable · dimcode models {id}" => {
            "{provider}：{count} モデル利用不可 · dimcode models {id}"
        }
        "{provider}: 1 model needs a window · dimcode models {id} --set-missing-limits" => {
            "{provider}：1 モデルにウィンドウが必要です · dimcode models {id} --set-missing-limits"
        }
        "{provider}: {count} models need a window · dimcode models {id} --set-missing-limits" => {
            "{provider}：{count} モデルにウィンドウが必要です · dimcode models {id} --set-missing-limits"
        }
        "{provider}: no models · dimcode models {id} --refresh" => {
            "{provider}：モデルなし · dimcode models {id} --refresh"
        }
        "{provider}: no models · add a model with provider.models.create" => {
            "{provider}：モデルなし · provider.models.create でモデルを追加"
        }
        "{provider}: {unavailable} unavailable and {disabled} disabled · dimcode models {id}" => {
            "{provider}：{unavailable} 個利用不可、{disabled} 個無効 · dimcode models {id}"
        }
        "{provider}: catalog unreadable · dimcode models {id}" => {
            "{provider}：カタログを読み取れません · dimcode models {id}"
        }
        "A model whose limits are undeclared cannot be selected; declare them with dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N." => {
            "上限が未宣言のモデルは選択できません。dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N で宣言してください。"
        }
        "Declare them with dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N." => {
            "dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N で宣言してください。"
        }
        "No cached catalog. Sign in and run models PROVIDER --refresh." => {
            "キャッシュされたカタログがありません。サインインして models PROVIDER --refresh を実行してください。"
        }
        "Saved model preferences cleared" => "保存したモデル設定を消去しました",
        "{provider}: 1 model disabled · dimcode models {id}" => {
            "{provider}：1 モデル無効 · dimcode models {id}"
        }
        "{provider}: {count} models disabled · dimcode models {id}" => {
            "{provider}：{count} モデル無効 · dimcode models {id}"
        }
        "No models. Run dimcode models {id} --refresh to list them." => {
            "モデルがありません。dimcode models {id} --refresh で一覧できます。"
        }
        "No models. The provider listed none." => {
            "モデルがありません。このプロバイダーは何も一覧しませんでした。"
        }
        "Enter select · Esc back" => "Enter 選択 · Esc 戻る",
        "Subagent models" => "サブエージェントモデル",
        "Subagent models: {policy}" => "サブエージェントモデル：{policy}",
        "Fixed subagent model" => "サブエージェントモデル固定",
        "Agent choice (default)" => "エージェント選択（既定）",
        "Agent choice" => "エージェント選択",
        "Inherit requesting branch" => "依頼元ブランチを継承",
        "Fixed model" => "固定モデル",
        "Applies to new members, including named Agents." => {
            "名前付きエージェント含む新規メンバーに適用。"
        }
        "Existing members keep their models." => "既存メンバーはモデルを維持。",
        "Conversation model" => "会話モデル",
        "Selected: {model}" => "選択中：{model}",
        "Model management" => "モデル管理",
        "Manage models" => "モデルを管理",
        "1 provider" => "1 Provider",
        "{count} providers" => "{count} Providers",
        "ready" => "準備完了",
        "no models" => "モデルなし",
        "catalog unreadable" => "catalog を読めません",
        "{count} with missing limits" => "{count} 件が上限未宣言",
        "{count} unavailable" => "{count} 件が利用不可",
        "{count} disabled" => "{count} 件が無効",
        "{count} without models" => "{count} 件がモデルなし",
        "{count} unreadable" => "{count} 件が読み取り不可",
        "{unavailable} unavailable, {disabled} disabled" => {
            "{unavailable} 件利用不可、{disabled} 件無効"
        }
        "Enter select + default · ←/→ effort · Tab section · Esc clear/close" => {
            "Enter 選択して既定に設定 · ←/→ 推論強度 · Tab セクション · Esc 消去/閉じる"
        }
        "Enter change · Tab section · Esc clear/close" => {
            "Enter 変更 · Tab セクション · Esc 消去/閉じる"
        }
        "Enter manage · Tab section · Esc clear/close" => {
            "Enter 管理 · Tab セクション · Esc 消去/閉じる"
        }
        "Enter manage · Esc back" => "Enter 管理 · Esc 戻る",
        "unknown" => "不明",
        "Subagent model policy saved" => "サブエージェントモデル方針を保存",
        "Subagent model policy changed elsewhere; choose again to reread it" => {
            "サブエージェントモデル方針が他で変更済み。再選択で再読込"
        }
        "Subagent model policy: {error}" => "サブエージェントモデル方針：{error}",
        "drawing diagram… {count} lines" => "図を描画中… {count} 行",
        // notify.rs
        "Task complete" => "タスク完了",
        "Background task finished: {label}" => "バックグラウンドタスク完了：{label}",
        "Background task failed: {label}" => "バックグラウンドタスク失敗：{label}",
        "Background task stopped: {label}" => "バックグラウンドタスク停止：{label}",
        "Background tasks" => "バックグラウンドタスク",
        "Nothing runs in the background." => "バックグラウンドで実行中のものはありません。",
        "No matching tasks." => "一致するタスクがありません。",
        "Permission needed: {tool}" => "許可が必要：{tool}",
        "Question waiting for your answer" => "回答待ちの質問あり",
        "Notify when agent needs you" => "要対応時に通知",
        "Show ACP sessions" => "ACP セッションを表示",
        "Prompt suggestion" => "プロンプト候補",
        "Prompt suggestion: {error}" => "プロンプト候補：{error}",
        "Suggestions are paused in this session because offers went unused. Turn them off and on in /settings to resume." => {
            "このセッションでは提案が使われなかったため、プロンプト候補を一時停止しました。再開するには /settings でオフにしてからオンにします。"
        }
        // keys.rs
        "show the key bindings" => "キーバインドを表示",
        "check for a newer version" => "新しいバージョンを確認",
        "Application keys" => "アプリキー",
        "Enter submit when idle · Shift+Enter / Alt+Enter / Ctrl+J newline · ↑↓ history" => {
            "待機中は Enter で送信 · Shift+Enter / Alt+Enter / Ctrl+J で改行 · ↑↓ 履歴"
        }
        "Running with a draft: Enter {default} · Ctrl+S {alternate}" => {
            "実行中の下書き: Enter {default} · Ctrl+S {alternate}"
        }
        "Empty draft: {keys} steer first queued" => "下書きが空なら {keys} でキューの先頭を steer",
        "Ctrl+O or Ctrl+P commands · Esc interrupt, queued messages return · Shift+← edit last queued" => {
            "Ctrl+O/Ctrl+P コマンド · Esc 中断、キューのメッセージは入力欄へ戻る · Shift+← 最後のキューを編集"
        }
        "Ctrl+Enter interrupt and send now: the draft and every held message, as one message" => {
            "Ctrl+Enter で中断して即送信：下書きと保留中の全メッセージを 1 通にまとめます"
        }
        "Send now failed: {error}; the draft was kept" => {
            "即時送信に失敗しました: {error}。下書きは残しました"
        }
        "Send now cancelled; the draft was kept" => "即時送信を取り消しました。下書きは残しました",
        "Send now is still stopping the run; the draft was kept" => {
            "送信即時はまだ実行を停止中です。下書きは保持されました"
        }
        "Send now is stopping the run; paste again when the draft is sent" => {
            "送信即時は実行を停止中です。下書きの送信後に貼り付けてください"
        }
        "Send now is waiting for the insertion before sending" => {
            "送信即時は挿入を待ってから送信します"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C クリア、2 回で終了 · 空なら Ctrl+D で終了"
        }
        "Shift+Tab cycle permissions (composer focused)" => {
            "Shift+Tab 権限を切り替え（入力欄にフォーカス時）"
        }
        "Shift+Tab newer target while picked · Esc return to composer" => {
            "選択中は Shift+Tab で新しい項目へ · Esc 入力欄に戻る"
        }
        "(shift+tab to cycle)" => "(Shift+Tab で切替)",
        "Fullscreen" => "全画面",
        "PageUp/PageDown scroll · Home/End top or bottom (empty draft) · Tab pick a message, e edit it, q quote it" => {
            "PageUp/PageDown スクロール · Home/End 先頭/末尾 (下書きが空の場合) · Tab メッセージ選択、e 編集、q 引用"
        }
        "wheel scroll · drag select and copy · click a link to open it, a tool group to expand it" => {
            "ホイール スクロール · ドラッグ 選択・コピー · クリック リンクを開く/ツールグループ展開"
        }
        "mouse off (/settings): the terminal selects and copies" => {
            "マウスオフ（/settings）：端末で選択・コピー"
        }
        "Terminal-owned history" => "端末管理の履歴",
        "scroll: {value}" => "スクロール：{value}",
        "search: {value}" => "検索：{value}",
        "copy: {value}" => "コピー：{value}",
        "terminal search" => "端末検索",
        "terminal scrollback" => "端末のスクロールバック",
        "terminal copy" => "端末コピー",
        "drag to select" => "ドラッグで選択",
        "drag or {shortcut}" => "ドラッグまたは {shortcut}",
        "Compacting context…" => "コンテキストを圧縮しています…",
        "Context compacted" => "コンテキストを圧縮しました",
        "Context compaction skipped" => "コンテキストの圧縮をスキップしました",
        "Context compaction failed" => "コンテキストの圧縮に失敗しました",
        "Context compaction cancelled" => "コンテキストの圧縮をキャンセルしました",
        "near the compaction threshold" => "圧縮しきい値に接近",
        "This daemon does not support context compaction" => {
            "このデーモンはコンテキストの圧縮に対応していません"
        }
        "compact the context of the shown line" => "表示中のラインのコンテキストを圧縮",
        "follow or change the shown line's goal" => "表示中のラインのゴールを追従または変更",
        "◎ goal active" => "◎ ゴール実行中",
        "◎ goal paused" => "◎ ゴール一時停止",
        "insufficient space: {parts}" => "空き不足：{parts}",
        "system prompt" => "システムプロンプト",
        "tools" => "ツール",
        "appended messages" => "追加メッセージ",
        "keyed baseline" => "キー付きベースライン",
        "pinned inputs" => "固定された入力",
        "digest" => "ダイジェスト",
        "summary" => "要約",
        "The first message was not sent; your new draft was kept." => {
            "最初のメッセージは送信されませんでした。新しい下書きは保持されています。"
        }
        "The App Server is still {server} because other clients are using it; this {client} client uses it too. Run `dimcode daemon stop` when they are done, and the next start runs {client}." => {
            "App Server はまだ {server} です。他のクライアントが使用中のため、この {client} クライアントもそれを使います。終わったら `dimcode daemon stop` を実行してください。次回の起動では {client} が動きます。"
        }
        "The App Server is still {server} because other clients are using it, and it speaks protocol v{protocol}, which this {client} client cannot use. No Session was started. Run `dimcode daemon stop` when they are done, and the next start runs {client}." => {
            "App Server はまだ {server} です。他のクライアントが使用中で、プロトコル v{protocol} を使用しており、この {client} クライアントはそれを使用できません。セッションは開始していません。終わったら `dimcode daemon stop` を実行してください。次回の起動では {client} が動きます。"
        }
        "The App Server is still {server} because other clients are using it (connections: {connections}, attached Sessions: {attached}, running turns: {running}), and it speaks protocol v{protocol}, which this {client} client cannot use. No Session was started. Run `dimcode daemon stop` when they are done, and the next start runs {client}." => {
            "App Server はまだ {server} です。他のクライアントが使用中で（接続: {connections}、アタッチ中のセッション: {attached}、実行中のターン: {running}）、プロトコル v{protocol} を使用しており、この {client} クライアントはそれを使用できません。セッションは開始していません。終わったら `dimcode daemon stop` を実行してください。次回の起動では {client} が動きます。"
        }
        "The App Server is still {server} because other clients are using it (connections: {connections}, attached Sessions: {attached}, running turns: {running}); this {client} client uses it too. Run `dimcode daemon stop` when they are done, and the next start runs {client}." => {
            "App Server はまだ {server} です。他のクライアントが使用中です（接続: {connections}、アタッチ中のセッション: {attached}、実行中のターン: {running}）。この {client} クライアントもそれを使います。終わったら `dimcode daemon stop` を実行してください。次回の起動では {client} が動きます。"
        }
        "The App Server is still using a different build of {server}. Some features may be unavailable. After active work finishes, exit this client, run `{command}`, then reopen it." => {
            "App Server はまだ {server} の別のビルドを使用しています。一部の機能が使えない場合があります。作業が終わったらこのクライアントを終了し、`{command}` を実行してから再度開いてください。"
        }
        "The App Server's build could not be verified as this client's. If features are missing after an update, wait for active work to finish, exit this client, run `{command}`, then reopen it." => {
            "App Server のビルドがこのクライアントと同じか確認できませんでした。更新後に機能が足りない場合は、作業が終わるのを待ってこのクライアントを終了し、`{command}` を実行してから再度開いてください。"
        }
        "Upgrading the local store from format v{from} to v{to}; this can take a while on a large store." => {
            "ローカルストアをフォーマット v{from} から v{to} にアップグレードしています。大きなストアでは時間がかかることがあります。"
        }
        "Upgrading the local store · {seconds} s · {stage}" => {
            "ローカルストアをアップグレード中 · {seconds} 秒経過 · {stage}"
        }
        "moving retained images into Session files" => {
            "保持された画像をセッションファイルに移しています"
        }
        "updating the text index of the rows that changed" => {
            "変更された行のテキストインデックスを更新しています"
        }
        "checking and committing the upgraded store" => {
            "アップグレードしたストアを検証してコミットしています"
        }
        "A prompt save is still in progress" => "プロンプトの保存がまだ進行中です",
        "Base prompt" => "基本プロンプト",
        "Before/after comparison is unavailable for this record." => {
            "この記録には変更前後の比較がありません。"
        }
        "Cancelling generation…" => "生成をキャンセルしています…",
        "Changes" => "変更",
        "Compaction prompt" => "圧縮プロンプト",
        "Could not open the share destination" => "共有先を開けませんでした",
        "Could not save ZIP" => "ZIP を保存できませんでした",
        "Default: current-session diagnostics only." => "既定：現在のセッションの診断情報のみ。",
        "Draft a GitHub issue" => "GitHub Issue を下書き",
        "Email the ZIP to {email}" => "ZIP を {email} にメール送信",
        "Enter apply · Esc back" => "Enter 適用 · Esc 戻る",
        "Error diagnostics only; no session text is available." => {
            "エラー診断情報のみ。セッション本文は取得できません。"
        }
        "Esc back · x stop agent · ↑↓ PgUp PgDn scroll · Home/End jump" => {
            "Esc 戻る · x エージェントを停止 · ↑↓ PgUp PgDn スクロール · Home/End 移動"
        }
        "Esc back · ↑↓ PgUp PgDn scroll · Home/End jump" => {
            "Esc 戻る · ↑↓ PgUp PgDn スクロール · Home/End 移動"
        }
        "Evidence · No recent session activity" => "証拠 · 最近のセッション活動なし",
        "Evidence · Observed error only" => "証拠 · 観測されたエラーのみ",
        "Evidence · Recent session activity" => "証拠 · 最近のセッション活動",
        "Evidence · Timeline omitted" => "証拠 · タイムラインを省略",
        "Evidence · Timeline read failed" => "証拠 · タイムラインを読み取れません",
        "Evidence · Timeline unavailable" => "証拠 · タイムラインは利用できません",
        "Generate report" => "レポートを生成",
        "Generate/save never uploads. Share manually after saving." => {
            "生成・保存はアップロードしません。保存後に手動で共有してください。"
        }
        "Generating report…" => "レポートを生成しています…",
        "Give feedback on Discord" => "Discord でフィードバック",
        "Input" => "入力",
        "Invalid report ZIP" => "レポート ZIP が無効です",
        "Local generation makes zero model requests." => {
            "ローカル生成ではモデルへのリクエストは発生しません。"
        }
        "Model · Generate locally" => "モデル · ローカルで生成",
        "New" => "新規",
        "No content changes." => "内容の変更はありません。",
        "No session to report." => "報告するセッションがありません。",
        "Old" => "旧",
        "Opened Discord" => "Discord を開きました",
        "Opened GitHub issue draft" => "GitHub Issue の下書きを開きました",
        "Options" => "オプション",
        "Options must fit within 1 KiB" => "オプションは 1 KiB 以内に収めてください",
        "Options · for example: omit timeline, keep error codes" => {
            "オプション · 例：omit timeline, keep error codes"
        }
        "Prepare report again" => "レポートを再準備",
        "Preparing report…" => "レポートを準備しています…",
        "Prompt saved; applies to new Sessions" => {
            "プロンプトを保存しました。新しいセッションに適用されます"
        }
        "Prompt settings are not available" => "プロンプト設定は利用できません",
        "Prompts" => "プロンプト",
        "Read-only: this agent has finished." => "読み取り専用：このエージェントは終了しました。",
        "Read-only: you can watch this agent or stop it." => {
            "読み取り専用：このエージェントを観察するか停止できます。"
        }
        "Redact [ ]" => "秘匿 [ ]",
        "Redact [x]" => "秘匿 [x]",
        "Redact is off. The ZIP may contain sensitive text from recent messages." => {
            "秘匿はオフです。ZIP に最近のメッセージの機密テキストが含まれる可能性があります。"
        }
        "Refresh report plan" => "レポート計画を更新",
        "Report ready" => "レポートの準備ができました",
        "Report this error" => "このエラーを報告",
        "Report this session" => "このセッションを報告",
        "Resetting {field}; the old text is kept in {path}" => {
            "{field} をリセットしています。元のテキストは {path} に保持されます"
        }
        "Result" => "結果",
        "Run /report to create a report." => "/report でレポートを作成できます。",
        "Running…" => "実行中…",
        "Save ZIP" => "ZIP を保存",
        "Saved ZIP" => "ZIP を保存しました",
        "Session model" => "セッションモデル",
        "Skills instructions" => "スキル指示",
        "Some options were unsupported." => "一部のオプションはサポートされていません。",
        "Tab also picks tool headers · Enter/Space toggle the focused tool" => {
            "Tab でもツール見出しを選択 · Enter/スペースで対象ツールを切り替え"
        }
        "This session is closed and cannot be resumed. Read its stored history with dimcode export {id} using the same --home." => {
            "このセッションは閉じられており、再開できません。同じ --home で dimcode export {id} を実行すると保存済みの履歴を読めます。"
        }
        "Turn Redact off to attach recent user/assistant text to ZIP; it may contain sensitive data." => {
            "秘匿をオフにすると、最近のユーザーとアシスタントの本文が ZIP に添付されます。機密情報が含まれる可能性があります。"
        }
        "Updating report plan…" => "レポート計画を更新しています…",
        "Usage: /report" => "使い方：/report",
        "Waiting for result…" => "結果を待っています…",
        "ZIP contents: report.md, diagnostics.json" => "ZIP の内容：report.md、diagnostics.json",
        "agent result" => "エージェント結果",
        "built-in" => "組み込み",
        "custom" => "カスタム",
        "report the current session" => "現在のセッションを報告",
        "skills replaces the marker and call shape; the skill tool and list stay · r reset" => {
            "skills はマーカーと呼び出し形式を置き換えます。スキルツールと一覧はそのまま · r リセット"
        }
        "… {count} unchanged lines" => "… {count} 行が未変更",
        "↑↓ move · Enter edit · r reset · Esc close" => {
            "↑↓ 移動 · Enter 編集 · r リセット · Esc 閉じる"
        }
        "↑↓ move · Enter select · ←→/Space change · r reset · Esc close" => {
            "↑↓ 移動 · Enter 選択 · ←→/スペース 変更 · r リセット · Esc 閉じる"
        }
        "↑↓/Tab move · Enter select · Esc close" => "↑↓/Tab 移動 · Enter 選択 · Esc 閉じる",
        "Permission mode" => "権限モード",
        "Related settings" => "関連設定",
        "Process isolation" => "プロセス分離",
        "open" => "開く",
        "User-wide command rules. Allow skips asks, never denials." => {
            "ユーザー全体のコマンドルール。許可は確認を省略しますが、拒否を越えることはありません。"
        }
        "Opens Settings; user-wide, for new processes; independent of permission mode." => {
            "設定を開きます。ユーザー全体、新しいプロセス向けで、権限モードとは独立しています。"
        }
        "keep mode" => "モードを維持",
        "use mode" => "モードを使用",
        "Current session; remembered for new sessions. {consequence}" => {
            "現在のセッションのみ。新しいセッション用に記憶されます。{consequence}"
        }
        "↑↓/Tab move · Enter {action} · Esc close" => {
            "↑↓/Tab 移動 · Enter {action} · Esc 閉じる"
        }
        "Writes and commands denied; external reads may ask." => {
            "書き込みとコマンドは拒否されます。外部の読み取りは確認を求めることがあります。"
        }
        "Workspace writes allowed; commands, network and ungranted outside access usually ask." => {
            "ワークスペースへの書き込みは許可されます。コマンド、ネットワーク、未許可の外部アクセスは通常確認を求めます。"
        }
        "Skips preset approval; pending calls on this branch are reevaluated. Explicit denies and isolation remain." => {
            "プリセットの承認を省略します。このブランチの保留中の呼び出しは再評価されます。明示的な拒否と分離は残ります。"
        }
        "↑↓ move · Enter select · Esc close" => "↑↓ 移動 · Enter 選択 · Esc 閉じる",
        "Loading process isolation…" => "プロセス分離を読み込んでいます…",
        "Sandbox is unavailable on this host: {reason}" => {
            "このホストではサンドボックスを利用できません：{reason}"
        }
        "Applies to new processes and shells." => "新しいプロセスとシェルに適用されます。",
        "Off: commands use your OS permissions, not file-tool limits." => {
            "オフ：コマンドはファイルツールの制限ではなく OS の権限を使います。"
        }
        "Off" => "オフ",
        "Sandboxed · No network" => "サンドボックス · ネットワークなし",
        "Sandboxed · Network allowed" => "サンドボックス · ネットワーク許可",
        "selected" => "選択中",
        "Save" => "保存",
        "Reset to defaults" => "既定値にリセット",
        "Reload" => "再読み込み",
        "Discard edits and read the current settings" => "編集を破棄して現在の設定を読み込む",
        "Reset the execution switches to defaults?" => "実行スイッチを既定値にリセットしますか？",
        "Discard unsaved changes?" => "保存されていない変更を破棄しますか？",
        "Yes (y)" => "はい (y)",
        "Cancel (n)" => "キャンセル (n)",
        "y confirm · Enter/Esc cancel" => "y 確認 · Enter/Esc キャンセル",
        "Read only" => "読み取り専用",
        "Workspace write" => "ワークスペース書き込み",
        "Agent" => "エージェント",
        "agent" => "エージェント",
        "command" => "コマンド",
        "ended" => "終了",
        "Signed in" => "サインイン済み",
        "Retry" => "再試行",
        "Provider changed; review the retained draft and press Apply again" => {
            "プロバイダーが変更されました。保持された下書きを確認して、もう一度「適用」を押してください"
        }
        "y allow · a always · n reject · ←→/Tab move · Enter confirm" => {
            "y 許可 · a 常に許可 · n 拒否 · ←→/Tab 移動 · Enter 確定"
        }
        "{count} in background · /tasks" => "バックグラウンド {count} 件 · /tasks",
        "{field}: {error} Your text is kept in {path}" => {
            "{field}：{error} 入力したテキストは {path} に保持されています"
        }
        "{field}: {error}" => "{field}：{error}",
        "Prompts: {error}" => "プロンプト：{error}",
        _ => return None,
    })
}
