//! The progress reminder the Loop adds to a request it starts: after a silent
//! stretch of tool calls it writes one input in that request's own batch, so
//! the request consumes it at once and no later turn can see it.
use crate::context_block;
use dim_agent_runtime::core::unix_millis;
use dim_agent_types::{InputMessageData, InputMode};
use std::time::Duration;
use tokio::time::Instant;

/// The marker the reminder carries.
pub const REMINDER: &str = "dim-progress-reminder";

/// The guide entry for [`REMINDER`].
pub(super) const ENTRY: &str = r#"<dim-progress-reminder seconds="61">: a note from the runtime that the user has seen no reply text from you for about the stated number of seconds while you worked through tool calls. Give the user a one- or two-sentence update in your reply text, then continue."#;

const BODY: &str = "The user has seen no text from you for about {seconds} seconds while you worked. Tell them in one or two short sentences what you are doing, then continue.";

/// One turn's silent stretch, folded from the turn's facts.
#[derive(Default)]
pub(super) struct Stretch {
    /// When the stretch started: the turn's start, the latest text or the
    /// latest reminder.
    since: Option<Instant>,
    /// The model made a tool call since the stretch started.
    tool: bool,
}

impl Stretch {
    pub(super) fn started(create_at: u64) -> Self {
        Self {
            since: Some(at(create_at)),
            tool: false,
        }
    }

    pub(super) fn text(&mut self, create_at: u64) {
        self.since = Some(at(create_at));
        self.tool = false;
    }

    pub(super) fn tool_call(&mut self) {
        self.tool = true;
    }

    /// The stretch's age in seconds when it earns a reminder, which also
    /// starts the next stretch.
    pub(super) fn reminder(&mut self, now: Instant, after: Duration) -> Option<u64> {
        if !self.tool {
            return None;
        }
        let since = self.since?;
        if now < since + after {
            return None;
        }
        let seconds = now
            .saturating_duration_since(since)
            .as_secs()
            .max(after.as_secs());
        self.since = Some(now);
        self.tool = false;
        Some(seconds)
    }
}

/// The reminder input for a request that starts `seconds` into the stretch.
pub(super) fn input(seconds: u64) -> InputMessageData {
    let body = BODY.replace("{seconds}", &seconds.to_string());
    let block = context_block::raw(
        context_block::element(REMINDER, &[("seconds", &seconds.to_string())], &body),
        None,
    );
    InputMessageData {
        blocks: vec![block],
        mode: Some(InputMode::Append),
        ..InputMessageData::default()
    }
}

/// Maps a fact's wall-clock time onto the runtime clock, so tests drive
/// the stretch through the paused clock.
fn at(create_at: u64) -> Instant {
    let age = unix_millis().saturating_sub(create_at);
    Instant::now() - Duration::from_millis(age)
}
