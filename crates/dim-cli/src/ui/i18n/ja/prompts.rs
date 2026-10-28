pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // prompts.rs: exit confirmation
        "The agent is still running." => "エージェント実行中。",
        "AskUser is waiting for an answer." => "AskUser が回答待ち。",
        "A permission request is waiting." => "許可リクエスト待機中。",
        "Background work may still be active." => "バックグラウンド処理が実行中の可能性。",
        "Exit dimcode?" => "dimcode を終了しますか？",
        "Exit" => "終了",
        "Stay" => "戻る",
        "y exit · n stay · ←→ move · enter confirm" => {
            "y 終了 · n 戻る · ←→ 移動 · enter 確定"
        }
        "Restart dimcode?" => "dimcode を再起動しますか？",
        "Restart" => "再起動",
        "y restart · n stay · ←→ move · enter confirm" => {
            "y 再起動 · n 戻る · ←→ 移動 · enter 確定"
        }
        // upgrade_dialog.rs
        "Update available" => "アップデートあり",
        "Version {latest} is available (you have {current})." => {
            "{latest} が利用可能（現在 {current}）。"
        }
        "Will run: {command}" => "実行：{command}",
        "To update, run: {command}" => "更新コマンド：{command}",
        "Update now" => "今すぐ更新",
        "Skip this version" => "このバージョンをスキップ",
        "Skipped {version}." => "{version} をスキップ。",
        "Later" => "後で",
        "Close" => "閉じる",
        "Restart now" => "今すぐ再起動",
        "Updating to {version}..." => "{version} に更新中…",
        "Updated to {version}. Restart DimCode to use it." => {
            "{version} に更新。DimCode の再起動で反映。"
        }
        "Update failed: {error}" => "更新失敗：{error}",
        "←→ move · Enter confirm · Esc close" => "←→ 移動 · Enter 確定 · Esc 閉じる",
        // prompts.rs: permission request
        "Permission required" => "許可が必要",
        "Allow once" => "今回のみ許可",
        "Allow always" => "常に許可",
        "Reject" => "拒否",
        "Approve for me" => "自動承認",
        "Full access" => "フルアクセス",
        "Run {tool}" => "{tool} を実行",
        "Changing the approvals mode from here is not yet available" => {
            "ここでは承認モードを変更不可"
        }
        "y allow · n reject · ←→/Tab move · Enter confirm" => {
            "y 許可 · n 拒否 · ←→/Tab 移動 · Enter 確定"
        }
        "Enter submit · Tab edit · Esc decline" => "Enter 送信 · Tab 編集 · Esc 断る",
        "Space toggle · Tab next · Esc decline" => "Space 切替 · Tab 次へ · Esc 断る",
        "1-9 select · Tab next · Esc decline" => "1-9 選択 · Tab 次へ · Esc 断る",
        "Question" => "質問",
        "Question on branch {branch}" => "ブランチ {branch} からの質問",
        "Permission on branch {branch}" => "ブランチ {branch} への許可",
        "Sending answer…" => "回答を送信中…",
        "Question not ready; press Enter to send again" => "質問の準備未完了。Enter で再送信",
        // prompts.rs: AskUser
        "Confirm" => "確定",
        "Enter save · Esc back" => "Enter 保存 · Esc 戻る",
        "Enter submit · 1-9 edit · Tab/←→ back · Esc decline" => {
            "Enter 送信 · 1-9 編集 · Tab/←→ 戻る · Esc 断る"
        }
        "↑↓ move · Space toggle · Enter next · Tab/←→ switch · Esc decline" => {
            "↑↓ 移動 · Space 切替 · Enter 次へ · Tab/←→ 質問切替 · Esc 断る"
        }
        "↑↓ move · 1-9 select · Enter next · Tab/←→ switch · Esc decline" => {
            "↑↓ 移動 · 1-9 選択 · Enter 次へ · Tab/←→ 質問切替 · Esc 断る"
        }
        "AskUserQuestion received an invalid payload." => "AskUserQuestion のデータが無効。",
        "Missing answer for Q{index}" => "Q{index} の回答がありません",
        "Type your own answer" => "自由回答を入力",
        "Review your answers and press Enter to submit." => "回答を確認し Enter で送信。",
        "Not answered" => "未回答",
        // approvals_panel.rs
        "Tool Approvals" => "ツール承認",
        "full access" => "フルアクセス",
        "All tools run automatically, bypass permissions" => "全ツールを自動実行、許可確認なし",
        "approve for me" => "自動承認",
        "Agent automatically reviews commands and tool actions. Unsafe commands ask for confirmation; routine actions pass automatically, safe, smart, and convenient." => {
            "エージェントがコマンドとツール操作を自動審査。危険なコマンドは確認、通常操作は自動許可。安全・スマート・便利。"
        }
        "Default permissions" => "既定の権限",
        "default" => "既定",
        "Read tools run automatically, write tools require confirmation" => {
            "読取ツールは自動実行、書込ツールは要確認"
        }
        "current" => "現在",
        "↑↓/Tab move · Enter apply · Esc back" => "↑↓/Tab 移動 · Enter 適用 · Esc 戻る",
        "Confirm Full Access" => "フルアクセスを確認",
        "Cancel" => "キャンセル",
        "Tab switch  Enter confirm  Esc cancel" => "Tab 切替  Enter 確定  Esc キャンセル",
        "Full access reduces confirmation prompts and can run sensitive actions, file changes, or external commands directly. Use it only when you trust the current task." => {
            "フルアクセスでは確認が減り、機密操作・ファイル変更・外部コマンドを直接実行可能。現在のタスクを信頼できる場合のみ使用。"
        }
        // transcript/tool_group.rs: summary verbs
        "ran" => "実行",
        "running" => "実行中",
        "read" => "読取",
        "reading" => "読取中",
        "explored" => "探索",
        "exploring" => "探索中",
        "edited" => "編集",
        "editing" => "編集中",
        "changed" => "変更",
        "changing" => "変更中",
        "checked on" => "確認",
        "checking on" => "確認中",
        "Explored" => "探索",
        "Exploring" => "探索中",
        "Edited" => "編集",
        "Editing" => "編集中",
        "Changed" => "変更",
        "Changing" => "変更中",
        // transcript/tool_group.rs: counts
        "1 file" => "1 ファイル",
        "{count} files" => "{count} ファイル",
        "1 web page" => "1 Web ページ",
        "{count} web pages" => "{count} Web ページ",
        "1 skill" => "1 スキル",
        "{count} skills" => "{count} スキル",
        "1 search" => "1 件の検索",
        "{count} searches" => "{count} 件の検索",
        "1 command" => "1 コマンド",
        "{count} commands" => "{count} コマンド",
        "1 task" => "1 タスク",
        "{count} tasks" => "{count} タスク",
        "1 tool" => "1 ツール",
        "{count} tools" => "{count} ツール",
        "{files} and {pages}" => "{files}と{pages}",
        ", " => "、",
        " · {count} failed" => " · {count} 件失敗",
        // transcript/tool_group.rs: call rows
        "Read" => "読取",
        "List" => "一覧",
        "Grep" => "Grep",
        "Search the web" => "ウェブ検索",
        "Read skill" => "スキル読取",
        "Search git history" => "git 履歴検索",
        "Edit" => "編集",
        "Write" => "書込",
        "Create" => "作成",
        " (detached)" => "（バックグラウンド）",
        "Checked on background task" => "バックグラウンドタスクを確認",
        "Waiting…" => "待機中…",
        "… +{count} lines" => "… 残り {count} 行",
        _ => return None,
    })
}
