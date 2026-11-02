pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // tui.rs
        "Enter output / interrupt member · x stop command · Esc close" => {
            "Enter 출력/멤버 중단 · x 명령 중지 · Esc 닫기"
        }
        "Enter output / interrupt member · x stop command · Ctrl+X stop all · Esc close" => {
            "Enter 출력/멤버 중단 · x 명령 중지 · Ctrl+X 모두 중지 · Esc 닫기"
        }
        "Loading branch history... Stop-all unavailable" => {
            "브랜치 기록 불러오는 중… 모두 중지 불가"
        }
        "Branch history failed; reopen to retry" => "브랜치 기록 실패, 다시 열어 재시도",
        "Branch history is incomplete; stop-all unavailable" => {
            "브랜치 기록 불완전, 모두 중지 불가"
        }
        "Running" => "실행 중",
        "Lines" => "라인",
        "Enter show the line · x end it · Esc close" => "Enter 라인 보기 · x 종료 · Esc 닫기",
        "This session has no lines." => "이 세션에는 아직 라인이 없습니다.",
        "No matching lines." => "일치하는 라인이 없습니다.",
        "the session itself" => "세션 자체",
        "working" => "작업 중",
        "ended itself" => "스스로 종료됨",
        "ended by an operation" => "작업으로 종료됨",
        "ended with {id}" => "{id}와 함께 종료됨",
        "creation failed" => "생성 실패",
        "shown" => "표시 중",
        "line" => "라인",
        "line {id}" => "라인 {id}",
        "jump to a message of the shown line" => "표시 중인 라인의 메시지로 이동",
        "Timeline" => "타임라인",
        "Enter jump · e edit · q quote · b branch · f fork · Esc close" => {
            "Enter 이동 · e 편집 · q 인용 · b 브랜치 · f 포크 · Esc 닫기"
        }
        "No messages yet." => "아직 메시지가 없습니다.",
        "Already on screen above; /tui jumps to it" => "이미 위에 있음. /tui 에서 바로 이동",
        "{count} earlier messages · Enter or click to load" => {
            "이전 메시지 {count}개 · Enter 또는 클릭으로 불러오기"
        }
        "follow, talk to and end this session's lines" => "이 세션의 라인을 보고, 대화하고, 종료",
        "End line" => "라인 종료",
        "y/N" => "y/N",
        "main is the session itself; a line ends, main does not" => {
            "main은 세션 자체입니다. 라인은 종료되지만 main은 그렇지 않습니다"
        }
        "Line has ended:" => "라인이 종료되었습니다:",
        "Esc close" => "Esc 닫기",
        "/{name} needs {argument}" => "/{name}에는 {argument}이(가) 필요합니다",
        "/{name} takes text only; remove {label} to send" => {
            "/{name}은(는) 텍스트만 받습니다. {label}을(를) 지우고 보내세요"
        }
        "e edit · b branch · f fork" => "e 편집 · b 브랜치 · f 포크",
        "e edit" => "e 편집",
        "Fork the session before “{message}”? (y/N)" => {
            "“{message}” 앞에서 세션을 포크할까요? (y/N)"
        }
        "Fork the session after the last turn? (y/N)" => {
            "마지막 턴 뒤에서 세션을 포크할까요? (y/N)"
        }
        "⑂ new line from {id} · Esc cancel" => "⑂ {id}에서 새 라인 · Esc 취소",
        "A fork copies main; b opens a line here" => {
            "포크는 main만 복사합니다. b로 여기서 라인을 여세요"
        }
        "A line opens from text; images and files were not included" => {
            "라인은 텍스트로 열립니다. 이미지와 파일은 포함되지 않았습니다"
        }
        "No finished turn to cut at yet" => "잘라낼 끝난 턴이 아직 없습니다",
        "forked {id} · {count} messages" => "{id} 포크됨 · 메시지 {count}개",
        "after message {count}" => "메시지 {count}개 이후",
        "open a line from the end of the shown line" => "표시 중인 라인의 끝에서 라인 열기",
        "fork the session at its last finished turn" => "마지막으로 끝난 턴에서 세션 포크",
        "rename this session" => "이 세션 이름 바꾸기",
        "Renamed to {title}" => "{title}(으)로 이름을 바꿨습니다",
        "The Session is now untitled." => "세션에 이제 제목이 없습니다.",
        "This daemon does not support renaming Sessions" => {
            "이 데몬은 세션 이름 바꾸기를 지원하지 않습니다"
        }
        "Renaming failed" => "이름 바꾸기에 실패했습니다",
        "Starting" => "시작 중",
        "Delegation result" => "위임 결과",
        "Enter details · x stop selected · Ctrl+X stop all · Esc close" => {
            "Enter 상세 · x 선택 중지 · Ctrl+X 모두 중지 · Esc 닫기"
        }
        "x interrupt member · Esc back" => "x 멤버 중단 · Esc 뒤로",
        "read-only" => "읽기 전용",
        "Member Session" => "멤버 세션",
        "Loading conversation..." => "대화 불러오는 중…",
        "No conversation yet" => "아직 대화 없음",
        "Observing conversation" => "대화 관찰 중",
        "Conversation unavailable" => "대화 볼 수 없음",
        "Session closed" => "세션 종료됨",
        "Member conversation" => "멤버 대화",
        "x stop command · Esc back" => "x 명령 중지 · Esc 뒤로",
        "Esc back" => "Esc 뒤로",
        "Tail preview - last 2,000 lines / 50 KiB" => "미리보기 - 마지막 2,000줄 / 50 KiB",
        "Loading output..." => "출력 불러오는 중…",
        "No output yet" => "아직 출력 없음",
        "Live output unavailable" => "실시간 출력 불가",
        "Live output unavailable in this profile" => "이 프로필은 실시간 출력 불가",
        "Output read failed" => "출력 읽기 실패",
        "Completed" => "완료",
        "Failed" => "실패",
        "Cancelled" => "취소됨",
        "workflow" => "워크플로",
        "Stopping" => "중지 중",
        "Stopped" => "중지됨",
        "Interrupted" => "중단됨",
        "last observed" => "마지막 관찰",
        "Loading Workflow runs... Stop-all unavailable" => {
            "워크플로 실행 불러오는 중… 모두 중지 불가"
        }
        "Workflow: last observed; refresh failed, reopen to retry" => {
            "워크플로: 마지막 관찰, 새로고침 실패, 다시 열어 재시도"
        }
        "Workflow runs unavailable; reopen to retry" => "워크플로 실행 불가, 다시 열어 재시도",
        "The selected Workflow run is no longer available" => "선택한 워크플로 실행이 더는 없음",
        "Workflow inventory is incomplete; stop-all unavailable" => {
            "워크플로 목록 불완전, 모두 중지 불가"
        }
        "Workflow stop unavailable" => "워크플로 중지 불가",
        "Run no longer available; last observed record" => "실행이 더는 없음, 마지막 관찰 기록",
        "x stop workflow · Esc back" => "x 워크플로 중지 · Esc 뒤로",
        "Run" => "실행",
        "Source" => "출처",
        "Stages" => "단계",
        "result" => "결과",
        "Log" => "로그",
        "Report" => "보고서",
        "waiting" => "대기 중",
        "launching" => "시작 중",
        "completed" => "완료",
        "failed" => "실패",
        "stopped" => "중지됨",
        "skipped" => "건너뜀",
        "lost" => "유실됨",
        "none" => "없음",
        "pending" => "대기 중",
        "delivered" => "전달됨",
        "undeliverable" => "전달 불가",
        "Agent is starting; no member Session to stop yet" => {
            "에이전트 시작 중, 아직 중지할 멤버 세션 없음"
        }
        "Provider members are controlled by the leader; no stop here" => {
            "프로바이더 멤버는 리더가 제어, 여기서 중지 불가"
        }
        "Provider members have no conversation to open here" => "프로바이더 멤버는 열 대화 없음",
        "That delegation is no longer in this transcript" => "해당 위임이 대화 기록에 더는 없음",
        "Member model" => "멤버 모델",
        "No model observed" => "관찰된 모델 없음",
        "Observing member..." => "멤버 관찰 중…",
        "{model} (observation unavailable)" => "{model} (관찰 불가)",
        "{model} (Session closed)" => "{model} (세션 종료됨)",
        "{model} (last observed)" => "{model} (마지막 관찰)",
        "Agent: {name}" => "에이전트: {name}",
        "Provider member: {name}" => "프로바이더 멤버: {name}",
        "Task: {task}" => "작업: {task}",
        "Activity: {activity}" => "활동: {activity}",
        "Activity: not observed on this attachment" => "활동: 이번 연결에서 미관찰",
        "Result: {result}" => "결과: {result}",
        "Controlled by the leader" => "리더가 제어",
        "› Open member conversation" => "› 멤버 대화 열기",
        "Not started" => "시작 안 함",
        "Reason: {reason}" => "이유: {reason}",
        "Copied to clipboard" => "클립보드에 복사됨",
        "Opened link" => "링크 열림",
        "Link: {url}" => "링크: {url}",
        "Not available" => "사용 불가",
        "files changed  {count}" => "변경된 파일  {count}",
        "active session" => "활성 세션",
        "new-session draft" => "새 세션 초안",
        "Tool approvals: {title} ({target})" => "도구 승인: {title} ({target})",
        "Disconnected · draft kept" => "연결 끊김 · 초안 보존됨",
        "The App Server closed this interface: {reason}" => "App Server가 화면을 닫음: {reason}",
        "Disconnected; draft kept. Exit and resume this Session." => {
            "연결 끊김, 초안 보존됨. 종료 후 세션 재개"
        }
        "Disconnected; draft kept. Exit and run: {command}" => {
            "연결 끊김, 초안 보존됨. 종료 후 실행: {command}"
        }
        "Disconnected; command outcome unknown" => "연결 끊김, 명령 결과 알 수 없음",
        "Message queued" => "메시지 대기 중",
        "Run a shell command · Esc to exit" => "shell 명령 실행 · Esc 나가기",
        "! shell mode" => "! shell 모드",
        "!! shell mode · not sent to the model" => "!! shell 모드 · 모델에 전송 안 됨",
        "Leave shell mode to quote a message" => "인용하려면 shell 모드 종료",
        "A shell command is already running" => "shell 명령 이미 실행 중",
        "Running… · Ctrl+C to interrupt · {seconds}s" => "실행 중… · Ctrl+C 중단 · {seconds}s",
        "exit code {code}" => "종료 코드 {code}",
        "killed by {signal}" => "{signal}로 종료됨",
        "interrupted" => "중단됨",
        "done" => "완료",
        "failed: {reason}" => "실패: {reason}",
        "Shell output was not recorded: {reason}" => "Shell 출력 기록 안 됨: {reason}",
        "Shell: {reason}" => "Shell: {reason}",
        "Reading clipboard…" => "클립보드 읽는 중…",
        "Reading image…" => "이미지 읽는 중…",
        "Wait for the image insertion before sending" => "이미지 삽입 후 전송하세요",
        "The image could not be read" => "이미지를 읽을 수 없습니다",
        "An image is still being inserted; paste again when it is in" => {
            "이미지 삽입 중입니다. 완료 후 다시 붙여넣으세요"
        }
        "The clipboard is still being inserted; paste again when it is in" => {
            "클립보드 삽입 중입니다. 완료 후 다시 붙여넣으세요"
        }
        "Clipboard: {reason}" => "클립보드: {reason}",
        "Wait for clipboard insertion before sending" => "클립보드 삽입 완료 후 전송",
        "A submission accepts at most 20 images" => "한 번에 이미지 최대 20개",
        "first frame" => "첫 프레임만",
        "re-encoded" => "재인코딩됨",
        "The current model does not accept images; remove {label} to send" => {
            "현재 모델은 이미지 미지원, {label} 삭제 후 전송"
        }
        "Files" => "파일",
        "Searching…" => "검색 중…",
        "↑↓ select · enter/tab insert · esc close" => "↑↓ 선택 · enter/tab 삽입 · esc 닫기",
        "not sent to the model" => "모델에 전송 안 됨",
        "… {count} more lines" => "… {count}줄 더",
        "… {count} earlier lines" => "… 이전 {count}줄",
        "Interrupt requested; Ctrl+C again to exit" => "중단 요청됨, Ctrl+C 다시 누르면 종료",
        "Press Ctrl+C again to exit" => "Ctrl+C 다시 누르면 종료",
        "User interrupted the run." => "사용자가 실행을 중단함",
        "Could not save the language: {error}" => "언어 저장 실패: {error}",
        "Language set to {locale}" => "언어 설정됨: {locale}",
        "Language set to system default" => "언어 설정됨: 시스템 기본값",
        "Connected {provider}" => "{provider} 연결됨",
        "Opened {url}" => "{url} 열림",
        "Tool details not saved: {error}" => "도구 상세 저장 실패: {error}",
        "/{name} needs a session — send a message first" => {
            "/{name}에는 세션 필요 — 먼저 메시지 전송"
        }
        "Nothing is running" => "실행 중인 작업 없음",
        "No queued message" => "대기 중인 메시지 없음",
        "Only in fullscreen (/tui)" => "전체 화면에서만 가능 (/tui)",
        "new session" => "새 세션",
        "resumed {id} · {count} messages" => "{id} 재개됨 · 메시지 {count}개",
        " · earlier history: /timeline" => " · 이전 기록: /timeline",
        "Error: {message}" => "오류: {message}",
        "working…  {elapsed}" => "작업 중…  {elapsed}",
        "thinking…  {elapsed}" => "생각 중…  {elapsed}",
        "Ask anything..." => "무엇이든 물어보세요…",
        "Earlier history · {id} · {count} messages" => "이전 기록 · {id} · 메시지 {count}개",
        "No active session" => "활성 세션 없음",
        "answer required" => "답변 필요",
        "permission required" => "권한 필요",
        "esc interrupt" => "esc 중단",
        "shift+← edit last · ctrl+s steer now" => "shift+← 마지막 편집 · ctrl+s 지금 steer",
        "+{count} more · shift+← edit last · ctrl+s steer now" => {
            "+{count}개 더 · shift+← 마지막 편집 · ctrl+s 지금 steer"
        }
        "Queue ({count})" => "대기열 ({count})",
        "Thinking…" => "생각 중…",
        "Thought" => "생각함",
        "Thought for {duration}" => "{duration} 생각함",
        "Welcome to DIM {version}!" => "DIM {version}에 오신 것을 환영합니다!",
        "Type / for commands · Ctrl+O for the palette" => "/ 명령 · Ctrl+O 팔레트",
        "Workspace" => "작업 공간",
        "Git branch" => "Git 브랜치",
        "Bye!" => "안녕히 가세요!",
        "DIM Web is ready" => "DIM Web 준비됨",
        "Opened in your browser" => "브라우저에서 열림",
        "Open the sign-in link below" => "아래 로그인 링크 열기",
        "Web" => "Web",
        "Sign-in link:" => "로그인 링크:",
        "Resume" => "재개",
        "Press Ctrl+C to stop the Web workspace." => "Ctrl+C로 Web 작업 공간 중지",
        "To resume this session:" => "세션 재개:",

        // composer/commands.rs: categories
        "session" => "세션",
        "run" => "실행",
        "view" => "보기",
        "model" => "모델",
        "app" => "앱",
        "skill" => "스킬",
        // composer/commands.rs: descriptions
        "start a new session" => "새 세션 시작",
        "show or set the reasoning effort" => "추론 강도 보기/설정",
        "show or set the priority service tier (on|off)" => "우선 서비스 등급 보기/설정 (on|off)",
        "list agents or switch this session's agent" => "에이전트 목록 또는 세션 에이전트 전환",
        "list MCP servers, sign in or out (login|logout ID)" => {
            "MCP 서버 목록, 로그인/로그아웃 (login|logout ID)"
        }
        "resume a previous session" => "이전 세션 재개",
        "open this session in the browser" => "브라우저에서 이 세션 열기",
        "show the context window usage" => "컨텍스트 창 사용량 보기",
        "interrupt the run" => "실행 중단",
        "steer with the next queued message" => "다음 대기 메시지로 steer",
        "scroll to the top" => "맨 위로 스크롤",
        "scroll to the bottom" => "맨 아래로 스크롤",
        "pick an earlier message to edit or quote" => "편집/인용할 이전 메시지 선택",
        "collapse or expand tool details, thoughts and compaction summaries" => {
            "도구 상세·생각·압축 요약 접기/펼치기"
        }
        "switch between fullscreen and scrollback" => "전체 화면/스크롤백 전환",
        "switch theme" => "테마 전환",
        "switch language" => "언어 전환",
        "select a model" => "모델 선택",
        "connect a provider" => "프로바이더 연결",
        "sign in to DimAgent" => "DimAgent에 로그인",
        "DimAgent account · usage · sign out" => "DimAgent 계정 · 사용량 · 로그아웃",
        "configure tool approvals" => "도구 승인 설정",
        "choose ask or full access" => "확인 또는 전체 액세스 선택",
        "open settings" => "설정 열기",
        "exit" => "종료",

        // composer/palette.rs
        "Enter select · ←/→ effort · Ctrl+X clear saved · Esc clear/close" => {
            "Enter 선택 · ←/→ 추론 강도 · Ctrl+X 저장값 지우기 · Esc 지우기/닫기"
        }
        "Enter select · Esc clear/close" => "Enter 선택 · Esc 지우기/닫기",
        "esc close" => "esc 닫기",
        "Suggestions" => "추천",
        "Skills" => "스킬",
        "Commands" => "명령",
        "Command Palette" => "명령 팔레트",
        "No matches" => "결과 없음",
        " (unavailable)" => " (사용 불가)",

        // composer/mod.rs
        "[Text: {preview} ({count} chars)]" => "[텍스트: {preview} ({count}자)]",
        "[... {count} lines]" => "[… {count}줄]",
        "pasted text" => "붙여넣은 텍스트",

        // list_panel.rs

        // settings_panel.rs
        "on" => "켜짐",
        "off" => "꺼짐",
        "Interface" => "인터페이스",
        "Theme" => "테마",
        "Language" => "언어",
        "Startup UI form" => "시작 UI 형태",
        "fullscreen" => "전체 화면",
        "scrollback" => "스크롤백",
        "Tool details" => "도구 상세",
        "expanded" => "펼침",
        "collapsed" => "접힘",
        "Mouse (fullscreen)" => "마우스 (전체 화면)",
        "Permissions" => "권한",
        "Tool approvals" => "도구 승인",
        "Execution" => "실행",
        "Models" => "모델",
        "Settings" => "설정",
        "↑↓ move · Enter select · ←→/Space change · Esc close" => {
            "↑↓ 이동 · Enter 선택 · ←→/Space 변경 · Esc 닫기"
        }

        // language_panel.rs
        "↑↓ preview · Enter save · Esc restore" => "↑↓ 미리보기 · Enter 저장 · Esc 복원",
        "System default ({locale})" => "시스템 기본값 ({locale})",

        // theme_panel.rs
        "↑↓ preview · Enter apply · Esc cancel" => "↑↓ 미리보기 · Enter 적용 · Esc 취소",
        "System" => "시스템",
        "Could not save the theme: {error}" => "테마를 저장할 수 없음: {error}",

        // sessions_panel.rs
        "Sessions" => "세션",
        "Enter open · Ctrl+R refresh · Esc close" => "Enter 열기 · Ctrl+R 새로고침 · Esc 닫기",
        "Loading sessions..." => "세션 불러오는 중…",
        "No saved sessions yet." => "저장된 세션 없음",
        "No matching sessions." => "일치하는 세션 없음",
        "now" => "방금",
        "{count}m ago" => "{count}분 전",
        "{count}h ago" => "{count}시간 전",
        "{count}d ago" => "{count}일 전",

        // models_panel.rs
        "Select a model" => "모델 선택",
        "Loading models..." => "모델 불러오는 중…",
        "No models available. Connect a provider first." => {
            "사용 가능한 모델 없음. 먼저 프로바이더 연결"
        }
        "No matching models." => "일치하는 모델 없음",
        "current" => "현재",
        "Edit models" => "모델 편집",
        "Editing models is not available yet" => "모델 편집 아직 미지원",
        "{provider}: 1 model unavailable · dimcode models {id}" => {
            "{provider}: 모델 1개 사용 불가 · dimcode models {id}"
        }
        "{provider}: {count} models unavailable · dimcode models {id}" => {
            "{provider}: 모델 {count}개 사용 불가 · dimcode models {id}"
        }
        "{provider}: 1 model needs a window · dimcode models {id} --set-missing-limits" => {
            "{provider}: 모델 1개에 윈도가 필요합니다 · dimcode models {id} --set-missing-limits"
        }
        "{provider}: {count} models need a window · dimcode models {id} --set-missing-limits" => {
            "{provider}: 모델 {count}개에 윈도가 필요합니다 · dimcode models {id} --set-missing-limits"
        }
        "{provider}: no models · dimcode models {id} --refresh" => {
            "{provider}: 모델 없음 · dimcode models {id} --refresh"
        }
        "{provider}: no models · add a model with provider.models.create" => {
            "{provider}: 모델 없음 · provider.models.create로 모델 추가"
        }
        "{provider}: {unavailable} unavailable and {disabled} disabled · dimcode models {id}" => {
            "{provider}: {unavailable}개 사용 불가, {disabled}개 비활성 · dimcode models {id}"
        }
        "{provider}: catalog unreadable · dimcode models {id}" => {
            "{provider}: 카탈로그를 읽을 수 없음 · dimcode models {id}"
        }
        "A model whose limits are undeclared cannot be selected; declare them with dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N." => {
            "한도가 선언되지 않은 모델은 선택할 수 없습니다. dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N로 선언하세요."
        }
        "Declare them with dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N." => {
            "dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N로 선언하세요."
        }
        "No cached catalog. Sign in and run models PROVIDER --refresh." => {
            "캐시된 카탈로그가 없습니다. 로그인한 뒤 models PROVIDER --refresh를 실행하세요."
        }
        "Saved model preferences cleared" => "저장된 모델 환경설정을 지웠습니다",
        "{provider}: 1 model disabled · dimcode models {id}" => {
            "{provider}: 모델 1개 사용 안 함 · dimcode models {id}"
        }
        "{provider}: {count} models disabled · dimcode models {id}" => {
            "{provider}: 모델 {count}개 사용 안 함 · dimcode models {id}"
        }
        "No models. Run dimcode models {id} --refresh to list them." => {
            "모델이 없습니다. dimcode models {id} --refresh 로 확인하세요."
        }
        "No models. The provider listed none." => {
            "모델이 없습니다. 이 제공자는 아무것도 나열하지 않았습니다."
        }
        "Enter select · Esc back" => "Enter 선택 · Esc 뒤로",
        "Subagent models" => "서브에이전트 모델",
        "Subagent models: {policy}" => "서브에이전트 모델: {policy}",
        "Fixed subagent model" => "고정 서브에이전트 모델",
        "Agent choice (default)" => "에이전트가 선택 (기본값)",
        "Agent choice" => "에이전트가 선택",
        "Inherit requesting branch" => "요청한 브랜치 상속",
        "Fixed model" => "고정 모델",
        "Applies to new members, including named Agents." => {
            "이름 있는 에이전트 포함, 새 멤버에 적용"
        }
        "Existing members keep their models." => "기존 멤버는 모델 유지",
        "unknown" => "알 수 없음",
        "Subagent model policy saved" => "서브에이전트 모델 정책 저장됨",
        "Subagent model policy changed elsewhere; choose again to reread it" => {
            "서브에이전트 모델 정책이 외부에서 변경됨, 다시 선택해 새로 읽기"
        }
        "Subagent model policy: {error}" => "서브에이전트 모델 정책: {error}",
        "drawing diagram… {count} lines" => "다이어그램 그리는 중… {count}줄",
        // notify.rs
        "Task complete" => "작업 완료",
        "Permission needed: {tool}" => "권한 필요: {tool}",
        "Question waiting for your answer" => "답변 대기 중인 질문",
        "Notify when agent needs you" => "에이전트 요청 시 알림",
        "Show ACP sessions" => "ACP 세션 표시",
        // keys.rs
        "show the key bindings" => "단축키 보기",
        "Application keys" => "앱 단축키",
        "Enter submit or queue · empty draft: steer first queued · Shift+Enter or Ctrl+J newline · ↑↓ history" => {
            "Enter 제출 또는 대기열 추가 · 빈 입력: 첫 대기 메시지 steer · Shift+Enter/Ctrl+J 줄바꿈 · ↑↓ 기록"
        }
        "Ctrl+O or Ctrl+P commands · Esc interrupt, queued messages return · Ctrl+S steer draft or first queued · Shift+← edit last queued" => {
            "Ctrl+O/Ctrl+P 명령 · Esc 중단, 대기 메시지는 입력란으로 복귀 · Ctrl+S 입력 또는 첫 대기 메시지 steer · Shift+← 마지막 대기 메시지 편집"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C 지우기, 두 번 누르면 종료 · 빈 초안에서 Ctrl+D 종료"
        }
        "Fullscreen" => "전체 화면",
        "PageUp/PageDown scroll · Home/End top or bottom (empty draft) · Tab pick a message, e edit it, q quote it" => {
            "PageUp/PageDown 스크롤 · Home/End 맨 위/아래 (초안이 비어 있을 때) · Tab 메시지 선택, e 편집, q 인용"
        }
        "wheel scroll · drag select and copy · click a link to open it, a tool group to expand it" => {
            "휠 스크롤 · 드래그 선택·복사 · 링크 클릭 시 열기, 도구 그룹 클릭 시 펼치기"
        }
        "mouse off (/settings): the terminal selects and copies" => {
            "마우스 꺼짐 (/settings): 터미널이 선택·복사"
        }
        "Terminal-owned history" => "터미널 관리 기록",
        "scroll: {value}" => "스크롤: {value}",
        "search: {value}" => "검색: {value}",
        "copy: {value}" => "복사: {value}",
        "terminal search" => "터미널 검색",
        "terminal scrollback" => "터미널 스크롤백",
        "terminal copy" => "터미널 복사",
        "drag to select" => "드래그 선택",
        "drag or {shortcut}" => "드래그 또는 {shortcut}",
        "Compacting context…" => "컨텍스트 압축 중…",
        "Context compacted" => "컨텍스트가 압축되었습니다",
        "Context compaction skipped" => "컨텍스트 압축을 건너뜀",
        "Context compaction failed" => "컨텍스트 압축 실패",
        "Context compaction cancelled" => "컨텍스트 압축 취소됨",
        "near the compaction threshold" => "압축 임계값에 근접",
        "This daemon does not support context compaction" => {
            "이 데몬은 컨텍스트 압축을 지원하지 않습니다"
        }
        "compact the context of the shown line" => "표시된 라인의 컨텍스트 압축",
        "follow or change the shown line's goal" => "표시된 라인의 목표 추적 또는 변경",
        "◎ goal active" => "◎ 목표 진행 중",
        "◎ goal paused" => "◎ 목표 일시정지",
        "Kept verbatim" => "원문 그대로 유지",
        "Tool effects" => "도구 작업 기록",
        "insufficient space: {parts}" => "공간 부족: {parts}",
        "system prompt" => "시스템 프롬프트",
        "tools" => "도구",
        "appended messages" => "추가 메시지",
        "keyed baseline" => "키가 지정된 기준선",
        "pinned inputs" => "고정된 입력",
        "digest" => "다이제스트",
        "summary" => "요약",
        _ => return None,
    })
}
