//! A shell-mode run (`!command` / `!!command`) as the transcript and the
//! model see it.
//!
//! The App Server runs the command (`shell.run`) and writes a recorded `!`
//! run as an append input holding a `<dim-shell-input>` marker; the
//! projection recognises that marker with [`ShellRun::parse`] and draws it
//! as a shell block instead of a user message, so a resumed session shows it
//! the same way. [`ShellRun::context_text`] is the older client-written form,
//! still read back from sessions that hold it.

/// Opening tag of a recorded run.
const OPEN: &str = "<dim-shell-input ";
const CLOSE: &str = "</dim-shell-input>";
/// What the model reads ahead of the record.
const NOTE: &str = "The user ran this command in shell mode, directly rather than through your tools. Use its output as context; do not run it again unless asked.";
const NO_OUTPUT: &str = "(no output)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Exited(i32),
    /// Ended by a signal the user did not send (its name).
    Signaled(String),
    /// Interrupted by the user.
    Stopped,
    /// Could not be started (the reason).
    Failed(String),
}

impl Outcome {
    pub fn success(&self) -> bool {
        *self == Self::Exited(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellRun {
    pub command: String,
    pub outcome: Outcome,
    /// The newest output lines kept, without terminal escapes.
    pub output: Vec<String>,
    /// Output lines produced in all; more than `output` when the start was
    /// dropped.
    pub total_lines: usize,
    /// `!!`: shown here only, never sent to the model.
    pub private: bool,
}

/// What `shell.run` replied.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellResult {
    pub status: String,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub reason: Option<String>,
    /// The newest output, stdout and stderr interleaved.
    pub output: String,
    pub truncated: bool,
    /// Where a recorded run was written, or why it was not.
    #[serde(default)]
    pub recorded: Option<Recorded>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(untagged)]
pub enum Recorded {
    Written { sequence: u64 },
    Failed { error: RecordingError },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingError {
    pub app_error: Option<String>,
    pub message: String,
}

impl ShellResult {
    pub fn outcome(&self) -> Outcome {
        match self.status.as_str() {
            "stopped" => Outcome::Stopped,
            "completed" => match (&self.exit_code, &self.signal) {
                (Some(code), _) => Outcome::Exited(*code),
                (None, Some(signal)) => Outcome::Signaled(signal.clone()),
                (None, None) => Outcome::Exited(-1),
            },
            _ => Outcome::Failed(self.reason.clone().unwrap_or_else(|| self.status.clone())),
        }
    }

    /// The run as a transcript block; `seen` counts the output lines the
    /// progress showed, which exceeds the reply's when its start was cut.
    pub fn run(&self, command: String, private: bool, seen: usize) -> ShellRun {
        let output = lines(&self.output);
        let total_lines = seen.max(output.len() + usize::from(self.truncated));
        ShellRun {
            command,
            outcome: self.outcome(),
            output,
            total_lines,
            private,
        }
    }
}

/// Output text as rows, without the final newline's empty row.
fn lines(text: &str) -> Vec<String> {
    let text = text.trim_suffix('\n');
    if text.is_empty() {
        return Vec::new();
    }
    text.split('\n')
        .map(|line| line.trim_suffix('\r').to_owned())
        .collect()
}

/// Reverses the App Server's escaping of text inside a runtime tag.
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

impl ShellRun {
    /// A run the App Server recorded: `<dim-shell-input kind="command"
    /// status=".." exit=".." truncated="..">`, `$ command`, then the output.
    /// A multi-line command cannot be told from its output, so its first
    /// line is taken as the command.
    fn parse_recorded(text: &str) -> Option<Self> {
        let rest = text.strip_prefix(OPEN)?.strip_suffix(CLOSE)?;
        let (attributes, body) = rest.split_once(">\n")?;
        let attribute = |name: &str| {
            let start = attributes.find(&format!("{name}=\""))? + name.len() + 2;
            let end = attributes[start..].find('"')? + start;
            Some(unescape(&attributes[start..end]))
        };
        if attribute("kind")? != "command" {
            return None;
        }
        let body = unescape(body.strip_suffix('\n')?);
        let (command, output) = body.split_once('\n').unwrap_or((&body, ""));
        let command = command.strip_prefix("$ ")?.to_owned();
        let exit = attribute("exit");
        let outcome = match attribute("status")?.as_str() {
            "completed" => match exit {
                Some(exit) => match exit.parse() {
                    Ok(code) => Outcome::Exited(code),
                    Err(_) => Outcome::Signaled(exit),
                },
                None => Outcome::Exited(-1),
            },
            "stopped" => Outcome::Stopped,
            status => Outcome::Failed(status.to_owned()),
        };
        let output = lines(output);
        let total_lines = output.len() + usize::from(attribute("truncated")? == "true");
        Some(Self {
            command,
            outcome,
            output,
            total_lines,
            private: false,
        })
    }

    /// The older client-written form of a recorded run, which
    /// [`ShellRun::parse`] still reads.
    #[cfg(test)]
    pub fn context_text(&self) -> String {
        let (status, exit) = match &self.outcome {
            Outcome::Exited(code) => ("completed", code.to_string()),
            Outcome::Signaled(signal) => ("failed", signal.clone()),
            Outcome::Stopped => ("stopped", String::new()),
            Outcome::Failed(_) => ("failed", String::new()),
        };
        let command_lines = self.command.lines().count().max(1);
        let mut text = format!(
            "{NOTE}\n{OPEN}kind=\"command\" status=\"{status}\" exit=\"{exit}\" truncated=\"{}\" lines=\"{}\" command-lines=\"{command_lines}\">\n$ {}\n",
            self.total_lines > self.output.len(),
            self.total_lines,
            self.command,
        );
        if let Outcome::Failed(reason) = &self.outcome {
            text.push_str(&format!("failed to start: {reason}\n"));
        } else if self.output.is_empty() {
            text.push_str(NO_OUTPUT);
            text.push('\n');
        }
        for line in &self.output {
            text.push_str(line);
            text.push('\n');
        }
        text.push_str(CLOSE);
        text
    }

    /// A recorded run read back from an input's text; `None` for anything
    /// else, which stays an ordinary user message.
    pub fn parse(text: &str) -> Option<Self> {
        if text.starts_with(OPEN) {
            return Self::parse_recorded(text);
        }
        let rest = text.strip_prefix(NOTE)?.strip_prefix('\n')?;
        let rest = rest.strip_prefix(OPEN)?.strip_suffix(CLOSE)?;
        let (attributes, body) = rest.split_once(">\n")?;
        let attribute = |name: &str| {
            let start = attributes.find(&format!("{name}=\""))? + name.len() + 2;
            let end = attributes[start..].find('"')? + start;
            Some(&attributes[start..end])
        };
        let total_lines: usize = attribute("lines")?.parse().ok()?;
        let command_lines: usize = attribute("command-lines")?.parse().ok()?;
        let mut lines = body.strip_suffix('\n')?.split('\n');
        let command = lines
            .by_ref()
            .take(command_lines)
            .collect::<Vec<_>>()
            .join("\n");
        let command = command.strip_prefix("$ ")?.to_owned();
        let mut output: Vec<String> = lines.map(str::to_owned).collect();
        let exit = attribute("exit")?;
        let outcome = match attribute("status")? {
            "completed" => Outcome::Exited(exit.parse().ok()?),
            "stopped" => Outcome::Stopped,
            "failed" if !exit.is_empty() => Outcome::Signaled(exit.to_owned()),
            "failed" => {
                let reason = output
                    .first()?
                    .strip_prefix("failed to start: ")?
                    .to_owned();
                output.clear();
                Outcome::Failed(reason)
            }
            _ => return None,
        };
        if total_lines == 0 && output.first().is_some_and(|line| line == NO_OUTPUT) {
            output.clear();
        }
        Some(Self {
            command,
            outcome,
            output,
            total_lines,
            private: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(command: &str, outcome: Outcome, output: &[&str], total: usize) -> ShellRun {
        ShellRun {
            command: command.into(),
            outcome,
            output: output.iter().map(|s| s.to_string()).collect(),
            total_lines: total,
            private: false,
        }
    }

    #[test]
    fn a_recorded_run_reads_back_from_its_context_text() {
        for original in [
            run("git status", Outcome::Exited(0), &["On branch main"], 1),
            run(
                "for i in 1 2\ndo echo $i; done",
                Outcome::Exited(3),
                &["1", "", "2"],
                9,
            ),
            run("true", Outcome::Exited(0), &[], 0),
            run("sleep 9", Outcome::Stopped, &["partial"], 1),
            run("yes", Outcome::Signaled("SIGPIPE".into()), &["y"], 1),
            run("nope", Outcome::Failed("No such file".into()), &[], 0),
        ] {
            let text = original.context_text();
            assert_eq!(ShellRun::parse(&text), Some(original), "{text}");
        }
    }

    #[test]
    fn a_run_the_app_server_recorded_reads_back_from_its_marker() {
        let marker = "<dim-shell-input kind=\"command\" status=\"completed\" exit=\"2\" truncated=\"true\">\n$ grep &lt;a&gt; &amp; x\nfirst\n&lt;b&gt;\n\n</dim-shell-input>";
        assert_eq!(
            ShellRun::parse(marker),
            Some(run(
                "grep <a> & x",
                Outcome::Exited(2),
                &["first", "<b>"],
                3
            ))
        );
        let signaled = "<dim-shell-input kind=\"command\" status=\"completed\" exit=\"SIGTERM\" truncated=\"false\">\n$ yes\n\n</dim-shell-input>";
        assert_eq!(
            ShellRun::parse(signaled),
            Some(run("yes", Outcome::Signaled("SIGTERM".into()), &[], 0))
        );
        let stopped = "<dim-shell-input kind=\"command\" status=\"stopped\" truncated=\"false\">\n$ sleep 9\npartial\n</dim-shell-input>";
        assert_eq!(
            ShellRun::parse(stopped),
            Some(run("sleep 9", Outcome::Stopped, &["partial"], 1))
        );
    }

    #[test]
    fn a_shell_run_reply_becomes_its_block() {
        let result: ShellResult = serde_json::from_value(serde_json::json!({
            "runId": "r", "status": "completed", "exitCode": 0, "output": "a\nb\n",
            "truncated": false, "durationMs": 3, "recorded": {"sequence": 9}
        }))
        .unwrap();
        assert_eq!(result.recorded, Some(Recorded::Written { sequence: 9 }));
        assert_eq!(
            result.run("ls".into(), false, 2),
            run("ls", Outcome::Exited(0), &["a", "b"], 2)
        );
        let failed: ShellResult = serde_json::from_value(serde_json::json!({
            "runId": "r", "status": "failed", "reason": "Failed to wait", "output": "",
            "truncated": false, "durationMs": 3,
            "recorded": {"error": {"message": "closed", "appError": "session_closed"}}
        }))
        .unwrap();
        assert_eq!(failed.outcome(), Outcome::Failed("Failed to wait".into()));
        assert!(matches!(failed.recorded, Some(Recorded::Failed { .. })));
    }

    #[test]
    fn the_model_reads_the_command_and_its_bounded_output() {
        let text = run("ls", Outcome::Exited(1), &["b"], 2).context_text();
        assert!(text.contains(
            "<dim-shell-input kind=\"command\" status=\"completed\" exit=\"1\" truncated=\"true\""
        ));
        assert!(text.ends_with("$ ls\nb\n</dim-shell-input>"));
        assert_eq!(ShellRun::parse("!ls"), None);
        assert_eq!(
            ShellRun::parse("<dim-shell-input kind=\"note\">\n$ ls\n</dim-shell-input>"),
            None
        );
        assert_eq!(ShellRun::parse("<dim-shell-input >"), None);
    }
}
