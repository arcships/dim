//! The run's state as the terminal keeps it: OSC 7501, the Program Status
//! Protocol (<https://www.superlogical.com/rex/docs/build/program-status>).
//!
//! A notification (`notify`) is a one-time event; this is the terminal's
//! record of what is true now: at the prompt, running, waiting on a
//! permission or a question, or done with or failed in a run the interface
//! watched. Tab indicators, session pickers and agent inboxes read the record
//! instead of the screen. It does not interrupt the user, so it is on
//! independently of `tuiNotifyOnComplete`.
//!
//! dim reports on the root record (no `id`) and names itself in every report,
//! since a report replaces its record whole. A report is written only when
//! the status changes; the first one always is, so the terminal learns that
//! dim reports. Once one is written, handing the terminal back clears the
//! record (`terminal::modes::Mode::Status`): on exit, the restore after a
//! panic or a signal, so no record outlives the interface.
//!
//! The clear names no `id`, and so removes every record on the terminal: the
//! root record has no id, and the protocol has no narrower way to remove it.
//! Records other programs left under their own ids go too. None of dim's own
//! processes report meanwhile: the App Server, and the tools it runs, have a
//! session of their own and no terminal. Leaving the record to the terminal would not do:
//! `idle`, `done` and `error` may outlive the process, and a terminal drops
//! `working` and `blocked` only when the process it watches exits, which
//! over SSH is `ssh`, not dim.
//!
//! Terminals must ignore an OSC they do not know, so every terminal gets the
//! reports. Inside tmux they stop at tmux, which keeps an unknown OSC to
//! itself. They are not wrapped for passthrough as OSC 52 is: the outer
//! terminal would hold one record for every pane of the tmux client, and
//! would keep a killed pane's `working`, since its own process lives on.
//! `DIM_TERMINAL_STATUS=off` turns the reports off for a terminal that
//! shows them as text.

use std::collections::HashSet;

use base64::Engine;

use crate::model::view::{TurnEnd, ViewSnapshot};
use crate::ui::i18n::tf;

/// The environment switch: `off` writes no report.
const ENV_KEY: &str = "DIM_TERMINAL_STATUS";
/// The protocol's `app`: the program's stable identifier.
const APP: &str = "dim";
/// Longest `msg` in UTF-8 bytes, which is at most 2732 in base64; the
/// protocol discards a report above either.
const MSG_MAX: usize = 2048;

/// The run as the protocol names it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Status {
    /// At the prompt with nothing running; also after an interrupted run.
    #[default]
    Idle,
    Working,
    /// `blocked`: a permission request for the named tool waits. The
    /// report's `msg` names the tool, the one thing the state does not say.
    Permission(String),
    /// `blocked`: a question waits for the user's answer.
    Question,
    /// A run the interface watched finished.
    Done,
    /// A run the interface watched failed, or the interface lost its App
    /// Server.
    Error,
}

impl Status {
    /// The report: `OSC 7501 ; pairs ST`, with ST as `ESC \`.
    pub fn sequence(&self) -> Vec<u8> {
        let (state, kind, msg) = match self {
            Self::Idle => ("idle", None, None),
            Self::Working => ("working", None, None),
            Self::Permission(tool) => (
                "blocked",
                Some("permission"),
                Some(tf("Permission needed: {tool}", &[("tool", tool)])),
            ),
            Self::Question => ("blocked", Some("question"), None),
            Self::Done => ("done", None, None),
            Self::Error => ("error", None, None),
        };
        let mut report = format!("\x1b]7501;state={state}");
        if let Some(kind) = kind {
            report.push_str(":kind=");
            report.push_str(kind);
        }
        report.push_str(":app=");
        report.push_str(APP);
        if let Some(msg) = msg {
            report.push_str(":msg=");
            report.push_str(&encode(&msg));
        }
        report.push_str("\x1b\\");
        report.into_bytes()
    }
}

/// A `msg` value: one line of UTF-8 within the limit, in standard base64. A
/// control character, newline and tab included, would have the terminal
/// discard the whole report.
fn encode(text: &str) -> String {
    let mut line = String::new();
    for c in text.chars().filter(|c| !c.is_control()) {
        if line.len() + c.len_utf8() > MSG_MAX {
            break;
        }
        line.push(c);
    }
    base64::engine::general_purpose::STANDARD.encode(line)
}

/// Follows one Session's runs to tell which turn ends are results. A switch
/// to another Session starts a new one.
#[derive(Debug, Default)]
pub struct Runs {
    /// The newest turn end accounted for; `None` before the Session's first
    /// snapshot, whose history is not a result.
    seen: Option<u64>,
    running: bool,
    /// What the last run came to: `Idle`, `Done` or `Error`.
    settled: Status,
}

impl Runs {
    /// Folds in the Session's next snapshot. A turn end is a result once it
    /// is newer than the first snapshot, wherever it falls against the
    /// running flag: the two arrive separately and a snapshot may carry a
    /// whole short run.
    pub fn observe(&mut self, snapshot: &ViewSnapshot) {
        if snapshot.running && !self.running {
            self.settled = Status::Idle;
        }
        self.running = snapshot.running;
        match (self.seen, snapshot.ended) {
            (None, ended) => self.seen = Some(ended.map_or(0, |(sequence, _)| sequence)),
            (Some(seen), Some((sequence, end))) if sequence > seen => {
                self.seen = Some(sequence);
                self.settled = match end {
                    TurnEnd::Completed => Status::Done,
                    TurnEnd::Failed => Status::Error,
                    TurnEnd::Cancelled => Status::Idle,
                };
            }
            _ => {}
        }
    }

    /// The status while `snapshot` is the Session's newest: a request waiting
    /// for the user outranks the run it holds up. `answered` holds the
    /// permission requests answered here that the App Server has not
    /// confirmed yet. A question waits until its call's result arrives, as
    /// its prompt does.
    pub fn status(&self, snapshot: &ViewSnapshot, answered: &HashSet<(String, String)>) -> Status {
        if let Some(approval) = snapshot
            .approvals
            .iter()
            .find(|a| !answered.contains(&(a.branch_id.clone(), a.request_id.clone())))
        {
            return Status::Permission(approval.tool_name.clone());
        }
        if !snapshot.questions.is_empty() {
            return Status::Question;
        }
        if snapshot.running {
            Status::Working
        } else {
            self.settled.clone()
        }
    }
}

/// Writes a report only when the status changes.
#[derive(Debug)]
pub struct Reporter {
    enabled: bool,
    /// What the terminal was last told: `None` before the first report and
    /// once the record was cleared.
    told: Option<Status>,
}

impl Reporter {
    /// Reports unless `DIM_TERMINAL_STATUS` is `off`.
    pub fn from_env() -> Self {
        let off = std::env::var(ENV_KEY).is_ok_and(|v| v.trim().eq_ignore_ascii_case("off"));
        Self::new(!off)
    }

    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            told: None,
        }
    }

    /// The report that tells the terminal `status`, unless it was told so
    /// already.
    pub fn report(&mut self, status: Status) -> Option<Vec<u8>> {
        if !self.enabled || self.told.as_ref() == Some(&status) {
            return None;
        }
        let report = status.sequence();
        self.told = Some(status);
        Some(report)
    }

    /// The terminal was handed back, which cleared the record: the next
    /// status is reported again.
    pub fn forget(&mut self) {
        self.told = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::view::ApprovalView;

    fn report(status: Status) -> String {
        String::from_utf8(status.sequence()).unwrap()
    }

    /// The `msg` of a report, decoded.
    fn msg(report: &str) -> String {
        let value = report
            .strip_suffix("\x1b\\")
            .unwrap()
            .split(':')
            .find_map(|pair| pair.strip_prefix("msg="))
            .unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(value)
            .unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn reports_name_dims_root_record_and_end_with_st() {
        assert_eq!(report(Status::Idle), "\x1b]7501;state=idle:app=dim\x1b\\");
        assert_eq!(
            report(Status::Working),
            "\x1b]7501;state=working:app=dim\x1b\\"
        );
        assert_eq!(report(Status::Done), "\x1b]7501;state=done:app=dim\x1b\\");
        assert_eq!(report(Status::Error), "\x1b]7501;state=error:app=dim\x1b\\");
        assert_eq!(
            report(Status::Permission("exec".into())),
            "\x1b]7501;state=blocked:kind=permission:app=dim:msg=UGVybWlzc2lvbiBuZWVkZWQ6IGV4ZWM=\x1b\\"
        );
        assert_eq!(
            msg(&report(Status::Permission("exec".into()))),
            "Permission needed: exec"
        );
        assert_eq!(
            report(Status::Question),
            "\x1b]7501;state=blocked:kind=question:app=dim\x1b\\"
        );
    }

    #[test]
    fn msg_is_one_line_within_the_protocols_limits() {
        let tool = format!("a\nb\tc\u{1b}[2J\u{7f}\u{85}{}", "工".repeat(2000));
        let report = report(Status::Permission(tool));
        let value = report
            .split(':')
            .find_map(|pair| pair.strip_prefix("msg="))
            .unwrap()
            .trim_end_matches("\x1b\\");
        assert!(value.len() <= 2732, "{}", value.len());
        assert!(report.len() <= 4096, "{}", report.len());
        let line = msg(&report);
        assert!(line.len() <= 2048, "{}", line.len());
        assert!(line.starts_with("Permission needed: abc[2J工"), "{line:?}");
        assert!(!line.chars().any(char::is_control), "{line:?}");
    }

    /// A Session as a snapshot shows it.
    struct Session {
        snapshot: ViewSnapshot,
        answered: HashSet<(String, String)>,
        runs: Runs,
    }

    impl Session {
        /// A Session whose history ended a completed turn at `ended`.
        fn opened(ended: Option<u64>) -> Self {
            let mut session = Self {
                snapshot: ViewSnapshot {
                    ended: ended.map(|sequence| (sequence, TurnEnd::Completed)),
                    ..ViewSnapshot::default()
                },
                answered: HashSet::new(),
                runs: Runs::default(),
            };
            session.publish();
            session
        }

        fn publish(&mut self) -> Status {
            self.runs.observe(&self.snapshot);
            self.status()
        }

        fn status(&self) -> Status {
            self.runs.status(&self.snapshot, &self.answered)
        }

        fn running(&mut self, running: bool) -> Status {
            self.snapshot.running = running;
            self.publish()
        }

        fn end(&mut self, sequence: u64, end: TurnEnd) -> Status {
            self.snapshot.ended = Some((sequence, end));
            self.publish()
        }

        fn approval(&mut self, request_id: &str, tool: &str) -> Status {
            self.snapshot.approvals.push(ApprovalView {
                member: None,
                request_id: request_id.into(),
                branch_id: "main".into(),
                reason: String::new(),
                tool_name: tool.into(),
                tool_input: serde_json::Value::Null,
                always: None,
            });
            self.publish()
        }

        fn question(&mut self, request_id: &str) -> Status {
            self.snapshot.questions.push(crate::question::Call {
                branch_id: "main".into(),
                request_id: request_id.into(),
                call_id: "call".into(),
                questions: Vec::new(),
            });
            self.publish()
        }
    }

    #[test]
    fn a_watched_run_goes_through_its_states_to_its_result() {
        let mut session = Session::opened(None);
        assert_eq!(session.status(), Status::Idle);
        assert_eq!(session.running(true), Status::Working);
        assert_eq!(
            session.approval("approval", "exec"),
            Status::Permission("exec".into())
        );
        assert_eq!(
            session.question("question"),
            Status::Permission("exec".into())
        );
        session.answered.insert(("main".into(), "approval".into()));
        assert_eq!(session.status(), Status::Question);
        session.answered.insert(("main".into(), "question".into()));
        assert_eq!(session.status(), Status::Question);
        session.snapshot.approvals.clear();
        session.snapshot.questions.clear();
        assert_eq!(session.publish(), Status::Working);
        assert_eq!(session.end(10, TurnEnd::Completed), Status::Working);
        assert_eq!(session.running(false), Status::Done);
        assert_eq!(session.running(true), Status::Working);
        assert_eq!(session.running(false), Status::Idle);
        assert_eq!(session.end(20, TurnEnd::Failed), Status::Error);
        session.running(true);
        session.end(30, TurnEnd::Cancelled);
        assert_eq!(session.running(false), Status::Idle);
        session.snapshot.ended = Some((40, TurnEnd::Completed));
        assert_eq!(session.running(true), Status::Working);
        assert_eq!(session.running(false), Status::Done);
    }

    #[test]
    fn a_turn_ended_before_the_session_was_opened_is_no_result() {
        let mut session = Session::opened(Some(8));
        assert_eq!(session.status(), Status::Idle);
        assert_eq!(session.end(8, TurnEnd::Completed), Status::Idle);
        assert_eq!(session.running(true), Status::Working);
        assert_eq!(session.end(9, TurnEnd::Completed), Status::Working);
        assert_eq!(session.running(false), Status::Done);
        let switched = Session::opened(Some(9));
        assert_eq!(switched.status(), Status::Idle);
    }

    #[test]
    fn the_terminal_is_told_each_status_once() {
        let mut reporter = Reporter::new(true);
        let written: Vec<_> = [
            Status::Idle,
            Status::Idle,
            Status::Permission("exec".into()),
            Status::Permission("exec".into()),
            Status::Permission("write".into()),
            Status::Done,
        ]
        .into_iter()
        .filter_map(|status| reporter.report(status))
        .collect();
        assert_eq!(
            written,
            [
                Status::Idle.sequence(),
                Status::Permission("exec".into()).sequence(),
                Status::Permission("write".into()).sequence(),
                Status::Done.sequence(),
            ]
        );
        reporter.forget();
        assert_eq!(reporter.report(Status::Done), Some(Status::Done.sequence()));
        let mut off = Reporter::new(false);
        assert_eq!(off.report(Status::Idle), None);
        assert_eq!(off.report(Status::Working), None);
    }
}
