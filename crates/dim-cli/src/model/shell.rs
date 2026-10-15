//! A shell-mode run (`!command` / `!!command`) as the transcript and the
//! model see it.
//!
//! A recorded `!` run is sent as an append input whose text is
//! [`ShellRun::context_text`]; the projection recognises that text with
//! [`ShellRun::parse`] and draws it as a shell block instead of a user
//! message, so a resumed session shows it the same way.

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

impl ShellRun {
    /// The text a recorded (`!`) run is sent to the model as.
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
    fn the_model_reads_the_command_and_its_bounded_output() {
        let text = run("ls", Outcome::Exited(1), &["b"], 2).context_text();
        assert!(text.contains(
            "<dim-shell-input kind=\"command\" status=\"completed\" exit=\"1\" truncated=\"true\""
        ));
        assert!(text.ends_with("$ ls\nb\n</dim-shell-input>"));
        assert_eq!(ShellRun::parse("!ls"), None);
        assert_eq!(ShellRun::parse("<dim-shell-input >"), None);
    }
}
