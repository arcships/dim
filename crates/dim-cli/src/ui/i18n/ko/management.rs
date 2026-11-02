pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Loading..." => "불러오는 중…",
        "No entries." => "항목이 없습니다.",
        "No matching entry." => "일치하는 항목이 없습니다.",
        "Issues" => "문제",
        "sources" => "소스",
        "skills" => "스킬",
        "hooks" => "훅",
        "scheduled jobs" => "예약된 작업",
        "command rules" => "명령 규칙",
        "enabled" => "활성",
        "shadowed by {name}" => "{name}에 의해 가려짐",
        "Reloaded." => "다시 로드했습니다.",
        "Enabled {name}." => "{name}을(를) 활성화했습니다.",
        "Disabled {name}." => "{name}을(를) 비활성화했습니다.",
        "Reloaded {area}: {entries} entries, {disabled} disabled." => {
            "{area} 다시 로드됨: 항목 {entries}개, 비활성 {disabled}개."
        }
        "Reloaded {area}: {entries} entries." => "{area} 다시 로드됨: 항목 {entries}개.",
        "Hooks" => "훅",
        "invalid" => "유효하지 않음",
        "Saved." => "저장했습니다.",
        "Saved {name}." => "{name}을(를) 저장했습니다.",
        "No change." => "변경 사항이 없습니다.",
        "Your text is kept in {path}." => "작성한 내용은 {path}에 보관됩니다.",
        "The user's own document; always in effect." => "사용자 자신의 문서이며 항상 적용됩니다.",
        "There is no hooks file there." => "거기에 훅 파일이 없습니다.",
        "Owned by plugin {plugin}." => "플러그인 {plugin} 소유입니다.",
        "The user document turns every hook off (disableAllHooks)." => {
            "사용자 문서가 모든 훅을 끕니다(disableAllHooks)."
        }
        "Enter toggle" => "Enter 전환",
        "Ctrl+E edit" => "Ctrl+E 편집",
        "Ctrl+N add" => "Ctrl+N 추가",
        "Ctrl+R reload" => "Ctrl+R 다시 로드",
        "Ctrl+T run" => "Ctrl+T 실행",
        "Ctrl+X remove" => "Ctrl+X 삭제",
        "Tab inspect" => "Tab 상세",
        "Remove {name}? Enter confirm · Esc cancel" => {
            "{name}을(를) 삭제할까요? Enter 확인 · Esc 취소"
        }
        "↑↓ scroll · Esc back" => "↑↓ 스크롤 · Esc 뒤로",
        "Added." => "추가했습니다.",
        "Removed {name}." => "{name}을(를) 삭제했습니다.",
        "Ran {name}." => "{name}을(를) 실행했습니다.",
        "The user document changed since it was read; nothing was written." => {
            "사용자 문서가 읽은 뒤에 변경되었습니다. 아무것도 쓰지 않았습니다."
        }
        "The job changed since it was read; nothing was written." => {
            "작업이 읽은 뒤에 변경되었습니다. 아무것도 쓰지 않았습니다."
        }
        "The jobs changed since they were read; nothing was written." => {
            "작업이 읽은 뒤에 변경되었습니다. 아무것도 쓰지 않았습니다."
        }
        "Your next edit opens the current document." => "다음 편집은 현재 문서를 엽니다.",
        "Your next add reopens your text." => "다음 추가는 작성한 내용을 다시 엽니다.",
        "Scheduler" => "스케줄러",
        "jobs" => "작업",
        "Added {name}." => "{name}을(를) 추가했습니다.",
        "Run {name}: {outcome}." => "{name}을(를) 실행했습니다: {outcome}.",
        "Recent runs of {name}" => "{name}의 최근 실행",
        "No run yet." => "아직 실행 기록이 없습니다.",
        "No next run." => "다음 실행이 없습니다.",
        "Next run {instant}." => "다음 실행 {instant}.",
        "Last run {outcome} at {instant}." => "마지막 실행 {outcome}, {instant}.",
        "every {minutes} minutes" => "{minutes}분마다",
        "at {instant}" => "{instant}에",
        "cron {expression}" => "cron {expression}",
        "cron {expression} in {zone}" => "cron {expression} ({zone})",
        "The job changed since it was read; nothing was removed." => {
            "작업이 읽은 뒤에 변경되었습니다. 아무것도 삭제하지 않았습니다."
        }
        "The Scheduler has no area document." => "스케줄러에는 영역 문서가 없습니다.",
        "No scheduled job has that id." => "해당 id의 예약 작업이 없습니다.",
        "This daemon cannot manage {area}." => "{area}을(를) 관리할 수 없습니다.",
        "This daemon lists {area} but cannot toggle them." => {
            "{area} 목록은 표시할 수 있지만 전환할 수 없습니다."
        }
        "This daemon lists {area} but cannot edit them." => {
            "{area} 목록은 표시할 수 있지만 편집할 수 없습니다."
        }
        "This daemon lists {area} but cannot reload them." => {
            "{area} 목록은 표시할 수 있지만 다시 로드할 수 없습니다."
        }
        "This daemon lists {area} but cannot add them." => {
            "{area} 목록은 표시할 수 있지만 추가할 수 없습니다."
        }
        "This daemon lists {area} but cannot remove them." => {
            "{area} 목록은 표시할 수 있지만 삭제할 수 없습니다."
        }
        "This daemon lists {area} but cannot run them." => {
            "{area} 목록은 표시할 수 있지만 실행할 수 없습니다."
        }
        "This daemon lists {area} but cannot inspect them." => {
            "{area} 목록은 표시할 수 있지만 세부 정보나 기록을 볼 수 없습니다."
        }
        "The document deleted {field}, which the owner cannot clear; nothing was written." => {
            "{field}이(가) 문서에서 삭제되었지만 소유자가 지울 수 없습니다. 아무것도 쓰지 않았습니다."
        }
        "The document changes no field; nothing was written." => {
            "문서가 어떤 필드도 바꾸지 않았습니다. 아무것도 쓰지 않았습니다."
        }
        "This workspace's hook settings changed since they were read; nothing was written." => {
            "이 작업 공간의 훅 설정이 읽은 뒤에 변경되었습니다. 아무것도 쓰지 않았습니다."
        }
        "Command rules" => "명령 규칙",
        "user" => "사용자",
        "Matches only this command." => "이 명령만 일치합니다.",
        "Matches commands whose first words are these." => {
            "앞부분이 이 단어들인 명령과 일치합니다."
        }
        "Allows a simple command that would otherwise ask." => {
            "원래 확인을 요청하는 단순 명령을 허용합니다."
        }
        "Denies the command under every preset." => "모든 프리셋에서 이 명령을 거부합니다.",
        "Applies in every Session, in every workspace." => {
            "모든 세션, 모든 워크스페이스에서 적용됩니다."
        }
        "Rule added: {rule}." => "규칙을 추가했습니다: {rule}.",
        "Rule already present: {rule}." => "규칙이 이미 있습니다: {rule}.",
        "Rule removed: {rule}." => "규칙을 삭제했습니다: {rule}.",
        "That command rule is no longer listed; nothing was removed." => {
            "해당 명령 규칙이 더 이상 목록에 없습니다. 아무것도 삭제되지 않았습니다."
        }
        "Command rules changed since they were read; nothing was removed." => {
            "명령 규칙이 읽은 뒤 변경되었습니다. 아무것도 삭제되지 않았습니다."
        }
        "A command rule is DECISION MATCH COMMAND, such as deny prefix git push." => {
            "명령 규칙은 DECISION MATCH COMMAND 형식입니다. 예: deny prefix git push."
        }
        "Command: {command}" => "명령: {command}",
        "Use the line prompt or dimcode permissions." => {
            "줄 프롬프트 또는 dimcode permissions에서 사용하세요."
        }
        _ => return None,
    })
}
