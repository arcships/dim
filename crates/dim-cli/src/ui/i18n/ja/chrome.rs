pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Queue" => "キュー",
        "Export session" => "セッションをエクスポート",
        "Enter steer · x delete held · Esc close" => {
            "Enter steer · x 保留中のメッセージを削除 · Esc 閉じる"
        }
        "No queued messages." => "キューにメッセージはありません。",
        "Held · awaiting application" => "保留中 · 適用待ち",
        "Incorporating · awaiting model consumption" => "取り込み中 · モデルの読み取り待ち",
        "Held" => "保留中",
        "Incorporating" => "取り込み中",
        "Enter export · Esc cancel" => "Enter エクスポート · Esc キャンセル",
        "Messages with attachments stay queued; use /queue to steer or delete them" => {
            "添付ファイル付きのメッセージはキューに残ります。/queue で steer または削除してください"
        }
        "/queue manage · shift+← edit last · ctrl+s steer now" => {
            "/queue 管理 · shift+← 最後を編集 · ctrl+s 今すぐ steer"
        }
        "+{count} more · /queue manage · shift+← edit last · ctrl+s steer now" => {
            "+{count} 件 · /queue 管理 · shift+← 最後を編集 · ctrl+s 今すぐ steer"
        }
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
        "Ask anything..." => "何でも質問…",
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
        "To resume this session:" => "セッションの再開：",

        // composer/commands.rs: descriptions
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
        "Enter select · ←/→ effort · Ctrl+X clear saved · Esc clear/close" => {
            "Enter 選択 · ←/→ 推論強度 · Ctrl+X 保存消去 · Esc 消去/閉じる"
        }
        "Enter select · Esc clear/close" => "Enter 選択 · Esc 消去/閉じる",
        "esc close" => "esc 閉じる",
        "Suggestions" => "候補",
        "Skills" => "スキル",
        "Skill loaded: {name}" => "スキル読み込み：{name}",
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
        "Enter open · Ctrl+S scope · Ctrl+R refresh · Esc close" => {
            "Enter 開く · Ctrl+S 範囲 · Ctrl+R 更新 · Esc 閉じる"
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
        "unknown" => "不明",
        "Subagent model policy saved" => "サブエージェントモデル方針を保存",
        "Subagent model policy changed elsewhere; choose again to reread it" => {
            "サブエージェントモデル方針が他で変更済み。再選択で再読込"
        }
        "Subagent model policy: {error}" => "サブエージェントモデル方針：{error}",
        "drawing diagram… {count} lines" => "図を描画中… {count} 行",
        // notify.rs
        "Task complete" => "タスク完了",
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
        "Application keys" => "アプリキー",
        "Enter submit or queue · empty draft: steer first queued · Shift+Enter or Ctrl+J newline · ↑↓ history" => {
            "Enter 送信またはキュー · 空の下書き: 最初のキューを steer · Shift+Enter/Ctrl+J 改行 · ↑↓ 履歴"
        }
        "Ctrl+O or Ctrl+P commands · Esc interrupt, queued messages return · Ctrl+S steer draft or first queued · Shift+← edit last queued" => {
            "Ctrl+O/Ctrl+P コマンド · Esc 中断、キューのメッセージは入力欄へ戻る · Ctrl+S 下書きまたは最初のキューを steer · Shift+← 最後のキューを編集"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C クリア、2 回で終了 · 空なら Ctrl+D で終了"
        }
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
        _ => return None,
    })
}
