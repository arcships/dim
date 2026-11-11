//! Markup shared by the runtime's writers and readers: the escape for an
//! element attribute or its text, and the element names the runtime writes.

/// Escapes a value for one XML attribute or for element text, so text a
/// producer or a job wrote cannot open or close a tag.
pub fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The element names the runtime writes for its own content, each with the
/// capability that writes it. A name matches a whole element, so
/// `dim-branch-line` does not match `<dim-branch-lines>`.
const MARKERS: &[&str] = &[
    "dim-agent-approval",       // Collaboration: a member's pending tool action
    "dim-agent-approval-ended", // Collaboration: that action was answered
    "dim-agent-message",        // Collaboration: a message from another agent
    "dim-agent-report",         // Collaboration: a member's report
    "dim-branch-ended",         // BranchLine: a line the agent opened ended
    "dim-branch-idle",          // BranchLine: a line the agent opened went idle
    "dim-branch-line",          // TimelineTree: which line of the Session this is
    "dim-branch-lines",         // BranchLine: the roster of open lines on main
    "dim-branch-message",       // BranchLine: another line's message
    "dim-branch-parent",        // TimelineTree: the line this one was cut from
    "dim-digest",               // ContextCompaction: what tools did earlier
    "dim-event",                // EventHub: an event a source delivered
    "dim-event-guide",          // EventHub: what a source's events mean
    "dim-goal",                 // Goal: a branch's goal and its reports
    "dim-hooks",                // Hooks: a hook's context
    "dim-notification",         // BackgroundExec: a background command ended
    "dim-rule",                 // Prompt: an instruction in effect
    "dim-scheduler",            // Scheduler: a job's prompt or a wakeup
    "dim-shell-input",          // ShellInput: a command the operator ran
    "dim-skill",                // Skills: a loaded Skill's instructions
    "dim-state",                // Prompt: a piece of state
    "dim-summary",              // ContextCompaction: a summary of earlier history
    "dim-workflow",             // Workflow: a run's report and its stage markers
];

/// Whether the text block at `index` is runtime context a capability injected
/// rather than the operator's own words.
///
/// The first block is always the operator's: a message can start with a
/// marker because the operator pasted one, and it stays their text.
pub fn runtime_context(index: usize, text: &str) -> bool {
    if index == 0 {
        return false;
    }
    let Some(open) = text.strip_prefix('<') else {
        return false;
    };
    MARKERS.iter().any(|marker| {
        open.strip_prefix(marker)
            .is_some_and(|rest| rest.starts_with('>') || rest.starts_with(char::is_whitespace))
    })
}

#[cfg(test)]
mod tests {
    use super::{escaped, runtime_context};

    #[test]
    fn every_markup_character_is_escaped() {
        assert_eq!(
            escaped(r#"<a href="x">Tom & Jerry's</a>"#),
            "&lt;a href=&quot;x&quot;&gt;Tom &amp; Jerry&apos;s&lt;/a&gt;"
        );
    }

    #[test]
    fn the_first_block_is_the_operators_even_when_it_opens_a_marker() {
        for text in [
            r#"<dim-hooks kind="prompt-submit">hook context</dim-hooks>"#,
            r#"<dim-skill name="mine">as typed</dim-skill>"#,
            "<dim-goal kind=\"goal\" op=\"set\" branch=\"main\">\nkeep going\n</dim-goal>",
        ] {
            assert!(!runtime_context(0, text), "{text}");
        }
    }

    #[test]
    fn every_written_marker_after_the_first_is_runtime_context() {
        for text in [
            r#"<dim-hooks kind="prompt-submit">a hook's context</dim-hooks>"#,
            r#"<dim-skill name="check">instructions</dim-skill>"#,
            r#"<dim-goal kind="state" branch="main">a goal</dim-goal>"#,
            r#"<dim-workflow kind="report" run="wf-1" status="completed">done</dim-workflow>"#,
            "<dim-summary>\nan earlier stretch\n</dim-summary>",
            r#"<dim-branch-message from="C" to="main">hello</dim-branch-message>"#,
            r#"<dim-branch-line branch="C" parent="main">purpose</dim-branch-line>"#,
            r#"<dim-shell-input kind="command" status="completed">$ ls\n</dim-shell-input>"#,
            r#"<dim-rule source="dim/instructions/project" scope="branch" op="set">rule</dim-rule>"#,
            r#"<dim-state source="dim/environment/date">today</dim-state>"#,
            r#"<dim-scheduler kind="wakeup">wake</dim-scheduler>"#,
            r#"<dim-event source="im" type="im.message" id="1">an event</dim-event>"#,
            r#"<dim-event-guide source="im">a guide</dim-event-guide>"#,
            r#"<dim-notification source="exec" task="t">a command ended</dim-notification>"#,
            r#"<dim-digest>wrote src/cache.rs</dim-digest>"#,
            r#"<dim-agent-report agent="a" session="s">done</dim-agent-report>"#,
            r#"<dim-agent-message agent="a" session="s">hello</dim-agent-message>"#,
            "<dim-agent-approval>\n{}</dim-agent-approval>",
            "<dim-agent-approval-ended>\n{}</dim-agent-approval-ended>",
            r#"<dim-branch-lines branch="main">open lines</dim-branch-lines>"#,
            r#"<dim-branch-parent branch="C" cut="7">the line cut here</dim-branch-parent>"#,
            r#"<dim-branch-idle from="C">a line went idle</dim-branch-idle>"#,
            r#"<dim-branch-ended from="C">a line ended</dim-branch-ended>"#,
        ] {
            assert!(runtime_context(1, text), "{text}");
        }
    }

    #[test]
    fn text_the_operator_wrote_that_only_looks_like_a_marker_stays_text() {
        for text in [
            "<dimension>a word, not a tag</dimension>",
            "<dim-hooksx kind=\"prompt-submit\">no such element</dim-hooksx>",
            "a sentence that mentions <dim-hooks>",
            "<div>ordinary markup</div>",
            "<dim-",
            // A longer name is a different element, so a marker name that is a
            // prefix of it must not match it.
            "<dim-branch-lines-extra branch=\"main\">not the roster</dim-branch-lines-extra>",
        ] {
            assert!(!runtime_context(1, text), "{text}");
        }
    }
}
