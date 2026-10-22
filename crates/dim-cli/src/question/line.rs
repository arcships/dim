//! Line-mode answers to `ask_user` (Questions design A.5): the turn loop of
//! `session.rs` feeds this driver every notice and reply of the attached
//! Session, and it prompts for one waiting call at a time, in arrival order,
//! on any branch (A9).
//!
//! ```text
//! [question] Database
//! 1. Which database should the service use?
//!    1. PostgreSQL  managed, relational
//!    2. SQLite      single file
//!    0. Other (type your answer)
//! Choose 1-2, 0 for other, empty to decline: 1
//! [question answered]
//! ```
//!
//! An empty line on the first question declines the call; an empty or
//! invalid line on a later one asks again. The prompt closes on the reply to
//! `session.question.respond`, or on the call's `step/tool-result` when
//! another client answered or the call was cancelled. `question_not_pending`
//! before that result keeps the answers for a resend.
use super::{Answer, Call, Pending, RESPOND, Reply, Settled};
use crate::{
    Result,
    client::{Client, RpcError},
    display::{Display, safe},
    input::Input,
    session::History,
};
use serde_json::{Value, json};
use std::collections::HashSet;
use unicode_width::UnicodeWidthStr;

/// The questions of one attached Session in a line-mode turn loop.
pub struct Questions {
    pending: Pending,
    /// Calls this client prompted for or gave up on, by branch, request and
    /// call id; the rest are asked.
    handled: HashSet<(String, String, String)>,
    asking: Option<Asking>,
    /// Branches whose history was read or is being read.
    branches: HashSet<String>,
    reads: Vec<Read>,
    /// Replies this client waits for and then ignores.
    drained: HashSet<u64>,
}

struct Asking {
    call: Call,
    index: usize,
    answers: Vec<Answer>,
    /// Labels chosen with "Other", while its text is typed.
    other: Option<Vec<String>>,
    /// The params sent last, kept for a resend.
    params: Option<Value>,
    sent: Option<u64>,
    /// Waiting for Enter to send `params` again.
    resend: bool,
    /// The batch settled while a reply was outstanding: answered?
    settled: Option<bool>,
}

/// One paged `session.history` read of a branch other than `main`.
struct Read {
    branch_id: String,
    request: u64,
    after: u64,
    through: Option<u64>,
}

impl Default for Questions {
    fn default() -> Self {
        Self {
            pending: Pending::default(),
            handled: HashSet::new(),
            asking: None,
            // `main` is read when the Session is attached.
            branches: HashSet::from(["main".to_owned()]),
            reads: Vec::new(),
            drained: HashSet::new(),
        }
    }
}

fn key(call: &Call) -> (String, String, String) {
    (
        call.branch_id.clone(),
        call.request_id.clone(),
        call.call_id.clone(),
    )
}

impl Questions {
    /// Reads one stored event of `branch_id`, before the turn loop runs.
    pub fn history(&mut self, branch_id: &str, event: &Value) {
        self.pending.event(branch_id, event);
    }

    /// Whether a question prompt owns the terminal input.
    pub fn asking(&self) -> bool {
        self.asking.is_some()
    }

    /// Nothing is asked, read or awaited; with `waiting`, no call is left
    /// for this client to ask or give up on either.
    pub fn idle(&self, waiting: bool) -> bool {
        self.asking.is_none()
            && self.reads.is_empty()
            && self.drained.is_empty()
            && !(waiting && self.unhandled().is_some())
    }

    pub fn owns(&self, id: u64) -> bool {
        self.drained.contains(&id)
            || self.reads.iter().any(|read| read.request == id)
            || self
                .asking
                .as_ref()
                .is_some_and(|asking| asking.sent == Some(id))
    }

    fn unhandled(&self) -> Option<&Call> {
        self.pending
            .calls()
            .find(|call| !self.handled.contains(&key(call)))
    }

    /// Gives up the waiting calls nobody here can answer and returns their
    /// branches, each once.
    pub fn unanswerable(&mut self) -> Vec<String> {
        let mut branches = Vec::new();
        for call in self.pending.calls() {
            if self.handled.insert(key(call)) && !branches.contains(&call.branch_id) {
                branches.push(call.branch_id.clone());
            }
        }
        branches
    }

    /// Stops `branch_id` for a question this client gave up on; `main` is
    /// stopped by the turn loop.
    pub async fn stop(
        &mut self,
        client: &mut Client,
        session_id: &str,
        branch_id: &str,
        reason: &str,
    ) -> Result<()> {
        if branch_id != "main" {
            let id = client
                .send(
                    "session.stop",
                    json!({"sessionId":session_id,"branchId":branch_id,"reason":reason}),
                )
                .await?;
            self.drained.insert(id);
        }
        Ok(())
    }

    /// Ctrl+C or EOF: closes the prompt and stops its branch, which ends
    /// every call waiting there; a reply in flight is drained. Returns
    /// whether a prompt was open.
    pub async fn interrupt(&mut self, client: &mut Client, session_id: &str) -> Result<bool> {
        let Some(asking) = self.asking.take() else {
            return Ok(false);
        };
        self.drained.extend(asking.sent);
        let stopped: Vec<_> = self
            .pending
            .calls()
            .filter(|call| call.branch_id == asking.call.branch_id)
            .map(key)
            .collect();
        self.handled.extend(stopped);
        self.stop(
            client,
            session_id,
            &asking.call.branch_id,
            "CLI interrupted",
        )
        .await?;
        Ok(true)
    }

    /// Opens a prompt for the first waiting call nobody answered here yet.
    pub fn open(&mut self, display: &mut Display, input: &mut Input) -> Result<()> {
        if self.asking.is_some() {
            return Ok(());
        }
        let Some(call) = self.unhandled().cloned() else {
            return Ok(());
        };
        self.handled.insert(key(&call));
        let asking = Asking {
            call,
            index: 0,
            answers: Vec::new(),
            other: None,
            params: None,
            sent: None,
            resend: false,
            settled: None,
        };
        asking.show(display, input)?;
        self.asking = Some(asking);
        Ok(())
    }

    /// One typed line while a prompt is open.
    pub async fn line(
        &mut self,
        client: &mut Client,
        session_id: &str,
        text: &str,
        display: &mut Display,
        input: &mut Input,
    ) -> Result<()> {
        let Some(asking) = self.asking.as_mut() else {
            return Ok(());
        };
        if asking.sent.is_some() {
            return Ok(());
        }
        if asking.resend {
            asking.resend = false;
        } else {
            let params = match asking.answer(text.trim()) {
                Step::Again => return input.prompt(&asking.prompt()),
                Step::Next => return asking.show(display, input),
                Step::Decline => super::respond(session_id, &asking.call, None),
                Step::Answered => super::respond(session_id, &asking.call, Some(&asking.answers)),
            };
            asking.params = Some(params);
        }
        let params = asking.params.clone().expect("answers to send");
        asking.sent = Some(client.send(RESPOND, params).await?);
        Ok(())
    }

    /// A reply this driver owns.
    pub async fn response(
        &mut self,
        client: &mut Client,
        session_id: &str,
        id: u64,
        result: std::result::Result<Value, RpcError>,
        display: &mut Display,
        input: &mut Option<Input>,
    ) -> Result<()> {
        if self.drained.remove(&id) {
            return Ok(());
        }
        if let Some(index) = self.reads.iter().position(|read| read.request == id) {
            let read = self.reads.remove(index);
            return self.page(client, session_id, read, result, display).await;
        }
        let asking = self.asking.as_mut().expect("question reply");
        asking.sent = None;
        match (super::reply(&result), asking.settled) {
            (Reply::Accepted, _) => {
                display.record("[question answered]")?;
                self.asking = None;
            }
            // The batch settled while the reply was outstanding: nothing
            // waits any more, and its result tells what happened.
            (_, Some(answered)) => {
                display.record(closed(answered))?;
                self.asking = None;
            }
            (Reply::NotPending, None) => {
                // Typeahead is dropped before the line that asks for Enter.
                if let Some(input) = input {
                    input.prompt("")?;
                }
                display.record("[question not ready; press Enter to send again]")?;
                asking.resend = true;
            }
            (Reply::Rejected(reason), None) => {
                display.record(&format!("[question rejected] {reason}"))?;
                asking.index = 0;
                asking.answers.clear();
                asking.other = None;
                if let Some(input) = input {
                    asking.show(display, input)?;
                }
            }
            (Reply::Failed(error), None) => {
                if let Some(input) = input {
                    input.prompt("")?;
                }
                display.record(&format!(
                    "[question error] {error}; press Enter to send again"
                ))?;
                asking.resend = true;
            }
        }
        Ok(())
    }

    /// A notice of the attached Session: events update the waiting calls;
    /// a branch seen for the first time has its history read.
    pub async fn notice(
        &mut self,
        client: &mut Client,
        session_id: &str,
        method: &str,
        params: &Value,
        display: &mut Display,
    ) -> Result<()> {
        let Some(branch_id) = params["branchId"].as_str() else {
            return Ok(());
        };
        match method {
            "session.event" => {
                let settled = self.pending.event(branch_id, &params["event"]);
                self.settle(settled, display)?;
            }
            "session.state" if self.branches.insert(branch_id.to_owned()) => {
                self.read(client, session_id, branch_id, 0, None).await?;
            }
            _ => {}
        }
        Ok(())
    }

    fn settle(&mut self, settled: Vec<Settled>, display: &mut Display) -> Result<()> {
        for Settled { call, answered } in settled {
            self.handled.remove(&key(&call));
            let Some(asking) = self.asking.as_mut().filter(|asking| asking.call == call) else {
                continue;
            };
            if asking.sent.is_some() {
                // Its reply decides what the prompt says.
                asking.settled = Some(answered);
                continue;
            }
            if !asking.resend {
                println!();
            }
            display.record(closed(answered))?;
            self.asking = None;
        }
        Ok(())
    }

    async fn read(
        &mut self,
        client: &mut Client,
        session_id: &str,
        branch_id: &str,
        after: u64,
        through: Option<u64>,
    ) -> Result<()> {
        let mut params =
            json!({"sessionId":session_id,"branchId":branch_id,"after":after,"limit":256});
        if let Some(through) = through {
            params["through"] = json!(through);
        }
        let request = client.send("session.history", params).await?;
        self.reads.push(Read {
            branch_id: branch_id.to_owned(),
            request,
            after,
            through,
        });
        Ok(())
    }

    async fn page(
        &mut self,
        client: &mut Client,
        session_id: &str,
        read: Read,
        result: std::result::Result<Value, RpcError>,
        display: &mut Display,
    ) -> Result<()> {
        let page = match result {
            // A branch that closed since its state notice asks nothing.
            Err(error)
                if error
                    .data
                    .as_ref()
                    .and_then(|data| data.get("appError"))
                    .and_then(Value::as_str)
                    == Some("branch_missing") =>
            {
                return Ok(());
            }
            Err(error) => {
                return display.record(&format!(
                    "[question] Cannot read the questions of branch {}: {error}",
                    read.branch_id
                ));
            }
            Ok(value) => {
                serde_json::from_value::<History>(value).map_err(|_| "Invalid history page")?
            }
        };
        let next = page.next(read.after, read.through)?;
        for event in &page.events {
            let settled = self.pending.event(&read.branch_id, event);
            self.settle(settled, display)?;
        }
        if let Some(after) = next {
            self.read(
                client,
                session_id,
                &read.branch_id,
                after,
                Some(page.through),
            )
            .await?;
        }
        Ok(())
    }
}

fn closed(answered: bool) -> &'static str {
    if answered {
        "[question answered elsewhere]"
    } else {
        "[question cancelled]"
    }
}

enum Step {
    /// Ask the same question again.
    Again,
    /// Show the next question.
    Next,
    Decline,
    Answered,
}

impl Asking {
    fn question(&self) -> &super::Question {
        &self.call.questions[self.index]
    }

    fn prompt(&self) -> String {
        if self.other.is_some() {
            return "Other: ".into();
        }
        let question = self.question();
        let other = if question.allow_custom {
            ", 0 for other"
        } else {
            ""
        };
        let decline = if self.index == 0 {
            ", empty to decline"
        } else {
            ""
        };
        if question.multi_select {
            format!("Choose numbers, comma separated{other}{decline}: ")
        } else {
            format!("Choose 1-{}{other}{decline}: ", question.options.len())
        }
    }

    /// Prints the current question and prompts for it.
    fn show(&self, display: &mut Display, input: &mut Input) -> Result<()> {
        let question = self.question();
        if self.index == 0 || question.header.is_some() {
            let mut title = String::from("[question]");
            if let Some(header) = &question.header {
                title.push(' ');
                title.push_str(header);
            }
            if self.call.branch_id != "main" {
                title.push_str(&format!(" (branch {})", self.call.branch_id));
            }
            display.record(&title)?;
        }
        display.record(&format!("{}. {}", self.index + 1, question.question))?;
        let width = question
            .options
            .iter()
            .map(|option| safe(&option.label).width())
            .max()
            .unwrap_or(0);
        for (number, option) in question.options.iter().enumerate() {
            let mut line = format!("   {}. {}", number + 1, option.label);
            if let Some(description) = &option.description {
                let pad = width - safe(&option.label).width();
                line.push_str(&format!("{}  {description}", " ".repeat(pad)));
            }
            display.record(&line)?;
        }
        if question.allow_custom {
            display.record("   0. Other (type your answer)")?;
        }
        input.prompt(&self.prompt())
    }

    /// Applies one typed line to the current question.
    fn answer(&mut self, text: &str) -> Step {
        let question = self.question().clone();
        let (selected, custom) = if let Some(selected) = self.other.take() {
            if text.is_empty() {
                self.other = Some(selected);
                return Step::Again;
            }
            (selected, Some(text.to_owned()))
        } else {
            if text.is_empty() {
                return if self.index == 0 {
                    Step::Decline
                } else {
                    Step::Again
                };
            }
            let mut numbers = Vec::new();
            for token in text
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|token| !token.is_empty())
            {
                match token.parse::<usize>() {
                    Ok(number)
                        if number <= question.options.len()
                            && (number > 0 || question.allow_custom)
                            && !numbers.contains(&number) =>
                    {
                        numbers.push(number)
                    }
                    _ => return Step::Again,
                }
            }
            if numbers.is_empty() || numbers.len() > 1 && !question.multi_select {
                return Step::Again;
            }
            let selected: Vec<_> = numbers
                .iter()
                .filter(|number| **number > 0)
                .map(|number| question.options[number - 1].label.clone())
                .collect();
            if numbers.contains(&0) {
                self.other = Some(selected);
                return Step::Again;
            }
            (selected, None)
        };
        self.answers.push(Answer {
            question_id: question.id,
            selected,
            custom,
        });
        self.index += 1;
        if self.index < self.call.questions.len() {
            Step::Next
        } else {
            Step::Answered
        }
    }
}
