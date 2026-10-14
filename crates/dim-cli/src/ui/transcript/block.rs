//! The scrollback block model: what one committed or retained row of the
//! transcript is, as data.
//!
//! A settled view row is projected into a [`Block`] once ([`project_row`]);
//! the commit path and the reflow path both draw a block through the same
//! renderer, so a row cannot look different when it is redrawn after a
//! resize. Blocks carry semantic [`Tone`]s, never colours: the palette is
//! applied at draw time.

use crate::model::view::{RowKind, RowTone, ViewRow};
use crate::ui::transcript::tools::{ToolBlock, ToolRow, ToolState, project_tool};

/// How a status line is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Muted,
    Warning,
    Danger,
}

#[derive(Debug, Clone)]
pub enum Block {
    /// The startup banner. Drawn at the terminal's full width, not from a row.
    Banner,
    User(String),
    Assistant {
        text: String,
        first: bool,
    },
    Thinking(String),
    Tool(ToolBlock),
    Error(String),
    Status {
        text: String,
        tone: Tone,
    },
    Failure(String),
    /// A centred rule with a label (session switches).
    Boundary(String),
    /// One muted line (earlier-history page header).
    Note(String),
}

/// The block a settled view row draws as.
///
/// Assistant text is not projected here: a finished answer may already be
/// laid out by the stream planner, which the caller decides.
pub fn project_row(row: &ViewRow) -> Block {
    match row.kind {
        RowKind::UserMessage => Block::User(row.text.clone()),
        RowKind::AssistantMessage => Block::Assistant {
            text: row.text.clone(),
            first: true,
        },
        RowKind::Thinking => {
            Block::Thinking(row.preview.clone().unwrap_or_else(|| row.text.clone()))
        }
        RowKind::ToolCall => Block::Tool(tool_block(row)),
        RowKind::Error => Block::Error(row.text.clone()),
        // A still-streaming answer met outside the live region (history
        // pages) reads as a status line, as it always has.
        RowKind::StreamingAssistant | RowKind::Status | RowKind::Other => Block::Status {
            text: row.text.clone(),
            tone: match row.tone {
                Some(RowTone::Danger) => Tone::Danger,
                Some(RowTone::Warning) => Tone::Warning,
                None => Tone::Muted,
            },
        },
    }
}

/// The tool block of a call row, also used for the live tail.
pub fn tool_block(row: &ViewRow) -> ToolBlock {
    project_tool(&ToolRow {
        tool_name: row.tool_name.as_deref().unwrap_or("tool"),
        input: row.tool_input.as_ref(),
        // A call parked on an approval is drawn as a settled one: it is
        // committed once, and the prompt owns the waiting.
        pending: row.is_live_tool(),
        result_text: row.result_text.as_deref(),
        result_is_error: row.result_is_error,
        diff: row.diff.as_deref(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: RowKind, text: &str) -> ViewRow {
        ViewRow {
            row_id: "r".into(),
            kind,
            text: text.into(),
            streaming: false,
            preview: None,
            tone: None,
            tool_name: None,
            tool_input: None,
            pending: false,
            parked: false,
            result_text: None,
            result_is_error: false,
            diff: None,
        }
    }

    #[test]
    fn unknown_and_streaming_rows_draw_as_muted_status() {
        for kind in [RowKind::Other, RowKind::StreamingAssistant, RowKind::Status] {
            let block = project_row(&row(kind, "x"));
            assert!(matches!(
                block,
                Block::Status {
                    tone: Tone::Muted,
                    ..
                }
            ));
        }
    }

    #[test]
    fn status_tone_follows_the_row() {
        let mut warn = row(RowKind::Status, "x");
        warn.tone = Some(RowTone::Warning);
        assert!(matches!(
            project_row(&warn),
            Block::Status {
                tone: Tone::Warning,
                ..
            }
        ));
    }

    #[test]
    fn thinking_prefers_the_preview() {
        let mut thinking = row(RowKind::Thinking, "full");
        thinking.preview = Some("short".into());
        assert!(matches!(project_row(&thinking), Block::Thinking(t) if t == "short"));
        thinking.preview = None;
        assert!(matches!(project_row(&thinking), Block::Thinking(t) if t == "full"));
    }

    #[test]
    fn a_parked_call_is_drawn_settled() {
        let mut call = row(RowKind::ToolCall, "");
        call.tool_name = Some("bash".into());
        call.pending = true;
        call.parked = true;
        assert!(!call.is_live_tool());
        assert_ne!(tool_block(&call).state, ToolState::Running);
    }
}
