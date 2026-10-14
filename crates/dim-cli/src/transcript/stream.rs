//! Commits a streaming answer one Markdown block at a time (port of the
//! `createStreamingCommitPlanner` contract).
//!
//! The planner remembers, per stream, how much of the text already went into
//! scrollback (`committed_len`). Each frame it reports the newly closed
//! batches of live streams, and for streams that finished (their settled twin
//! arrived) only the remainder — never the part already written.

use super::blocks::gutter_width;

/// Lays out one batch of Markdown source: `(source, content_width, gutter,
/// first)`; `first` batches carry the answer mark.
pub type RenderFn<'a> = dyn Fn(&str, usize, usize, bool) -> Vec<String> + 'a;
use super::split::split_streaming_markdown;
use std::collections::HashMap;

/// An assistant text block as the projection sees it.
#[derive(Debug, Clone)]
pub struct TextBlock {
    pub id: String,
    pub text: String,
}

/// What a committed batch was rendered from, kept so it can be laid out again
/// at another width (resize, 2.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchSource {
    pub text: String,
    /// The batch opened its message, which decides the gutter mark.
    pub first: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Batch {
    pub lines: Vec<String>,
    pub source: Option<BatchSource>,
}

#[derive(Debug, Default)]
pub struct Plan {
    /// Settled block id -> the remainder to write for it.
    pub finalized: HashMap<String, Batch>,
    /// Batches of live streams that closed this frame, in stream order.
    pub active: Vec<Batch>,
}

#[derive(Debug, Clone)]
struct ActiveStream {
    id: String,
    text: String,
    committed_len: usize,
    committed_rows: usize,
}

#[derive(Default)]
pub struct StreamPlanner {
    active: Vec<ActiveStream>,
    /// Streams whose live row is gone but whose settled twin has not arrived:
    /// forgetting them would commit the settled block whole on top of the rows
    /// already streamed.
    parked: Vec<ActiveStream>,
    revision: Option<u64>,
}

impl StreamPlanner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.active.clear();
        self.parked.clear();
    }

    pub fn plan(
        &mut self,
        revision: u64,
        live: &[TextBlock],
        settled: &[TextBlock],
        width: usize,
        render: &RenderFn<'_>,
    ) -> Plan {
        if self.revision != Some(revision) {
            self.revision = Some(revision);
            self.reset();
        }
        self.transfer_reidentified(live);

        // Streams whose live row disappeared.
        let live_ids: Vec<&str> = live.iter().map(|b| b.id.as_str()).collect();
        let (still, gone): (Vec<_>, Vec<_>) = std::mem::take(&mut self.active)
            .into_iter()
            .partition(|s| live_ids.contains(&s.id.as_str()));
        self.active = still;
        for stream in gone {
            // Nothing went out: the settled twin takes the normal path.
            if stream.committed_len > 0 {
                self.parked.push(stream);
            }
        }

        let mut plan = Plan::default();
        let mut used_settled: Vec<&str> = Vec::new();
        for mut stream in std::mem::take(&mut self.parked) {
            let prefix = &stream.text[..stream.committed_len];
            let twin = settled
                .iter()
                .find(|b| !used_settled.contains(&b.id.as_str()) && b.text.starts_with(prefix));
            match twin {
                None => self.parked.push(stream),
                Some(block) => {
                    used_settled.push(&block.id);
                    // The persisted text is the authority on the remainder.
                    stream.text = block.text.clone();
                    let batch = plan_blocks(&mut stream, width, true, render);
                    plan.finalized.insert(block.id.clone(), batch);
                }
            }
        }

        for block in live {
            let existing = self.take_active(&block.id).or_else(|| self.unpark(block));
            let mut stream = match existing {
                Some(mut stream) if block.text.starts_with(&stream.text) => {
                    stream.text = block.text.clone();
                    stream
                }
                other => {
                    // A fresh stream: a parked one is never going to settle
                    // (an interrupt discarded its buffer).
                    drop(other);
                    self.parked.clear();
                    ActiveStream {
                        id: block.id.clone(),
                        text: block.text.clone(),
                        committed_len: 0,
                        committed_rows: 0,
                    }
                }
            };
            let batch = plan_blocks(&mut stream, width, false, render);
            if !batch.lines.is_empty() {
                plan.active.push(batch);
            }
            self.active.push(stream);
        }
        plan
    }

    /// The part of a live stream's text that has not been committed yet and
    /// whether it opens the message. The live region renders this through the
    /// same renderer the commit uses, so a row never changes shape when it
    /// moves from the live region into scrollback.
    pub fn pending(&self, id: &str) -> Option<(&str, bool)> {
        let stream = self.active.iter().find(|s| s.id == id)?;
        Some((
            &stream.text[stream.committed_len..],
            stream.committed_rows == 0,
        ))
    }

    fn take_active(&mut self, id: &str) -> Option<ActiveStream> {
        let index = self.active.iter().position(|s| s.id == id)?;
        Some(self.active.remove(index))
    }

    fn unpark(&mut self, block: &TextBlock) -> Option<ActiveStream> {
        let index = self.parked.iter().position(|s| s.id == block.id)?;
        let prefix = &self.parked[index].text[..self.parked[index].committed_len];
        if !block.text.starts_with(prefix) {
            return None;
        }
        Some(self.parked.remove(index))
    }

    /// A live block under a new id continues the stream that already wrote
    /// part of the same text: take over its watermark.
    fn transfer_reidentified(&mut self, live: &[TextBlock]) {
        let live_ids: Vec<&str> = live.iter().map(|b| b.id.as_str()).collect();
        for block in live {
            if self.active.iter().any(|s| s.id == block.id) {
                continue;
            }
            let candidate = self
                .active
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    !live_ids.contains(&s.id.as_str())
                        && s.committed_len > 0
                        && block.text.starts_with(&s.text[..s.committed_len])
                })
                .max_by_key(|(_, s)| s.committed_len)
                .map(|(i, _)| i);
            if let Some(index) = candidate {
                let stream = &mut self.active[index];
                stream.id = block.id.clone();
                stream.text = block.text.clone();
            }
        }
    }
}

fn plan_blocks(
    stream: &mut ActiveStream,
    width: usize,
    finalize: bool,
    render: &RenderFn<'_>,
) -> Batch {
    let gutter = gutter_width(width);
    let content_width = width.saturating_sub(gutter).max(1);
    let end = if finalize {
        stream.text.len()
    } else {
        split_streaming_markdown(&stream.text).settled_len
    };
    if end <= stream.committed_len {
        return Batch::default();
    }
    let source = stream.text[stream.committed_len..end].to_owned();
    let first = stream.committed_rows == 0;
    let lines = render(&source, content_width, gutter, first);
    stream.committed_len = end;
    stream.committed_rows += lines.len();
    Batch {
        lines,
        source: Some(BatchSource {
            text: source,
            first,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::blocks::{ASSISTANT_MARK, apply_gutter_mark, render_plain};

    /// Test renderer: plain wrapped text with the unstyled answer mark.
    fn render_marked(source: &str, width: usize, gutter: usize, first: bool) -> Vec<String> {
        let rows = render_plain(source, width, gutter);
        if first {
            apply_gutter_mark(rows, ASSISTANT_MARK, gutter, &|m: &str| m.to_owned())
        } else {
            rows
        }
    }

    const W: usize = 40;

    fn block(id: &str, text: &str) -> TextBlock {
        TextBlock {
            id: id.into(),
            text: text.into(),
        }
    }

    fn plan(planner: &mut StreamPlanner, live: &[TextBlock], settled: &[TextBlock]) -> Plan {
        planner.plan(1, live, settled, W, &render_marked)
    }

    fn text_of(batches: &[Batch]) -> Vec<String> {
        batches
            .iter()
            .flat_map(|b| b.lines.iter().map(|l| l.trim().to_owned()))
            .collect()
    }

    #[test]
    fn only_closed_blocks_stream_out_and_only_once() {
        let mut p = StreamPlanner::new();
        assert!(
            plan(&mut p, &[block("a", "First para")], &[])
                .active
                .is_empty()
        );
        let out = plan(&mut p, &[block("a", "First para\n\nSecond")], &[]);
        assert_eq!(text_of(&out.active), vec!["◆ First para"]);
        // The same text again writes nothing.
        let again = plan(&mut p, &[block("a", "First para\n\nSecond")], &[]);
        assert!(again.active.is_empty());
        // The next block closes: only it is new, without the answer mark.
        let out = plan(&mut p, &[block("a", "First para\n\nSecond\n\nThird")], &[]);
        assert_eq!(text_of(&out.active), vec!["Second"]);
    }

    #[test]
    fn settling_writes_only_the_remainder() {
        let mut p = StreamPlanner::new();
        plan(&mut p, &[block("live", "One.\n\nTwo")], &[]); // "One." is out
        // The run ends: live row gone, settled twin arrives with the full text.
        let out = plan(&mut p, &[], &[block("msg", "One.\n\nTwo\n\nThree.")]);
        let batch = out.finalized.get("msg").expect("finalized");
        assert_eq!(
            text_of(std::slice::from_ref(batch)),
            vec!["Two", "", "Three."]
        );
    }

    #[test]
    fn a_parked_stream_waits_for_its_twin() {
        let mut p = StreamPlanner::new();
        plan(&mut p, &[block("live", "A.\n\nB")], &[]);
        // Live row gone, no twin yet: nothing is written, nothing is lost.
        let out = plan(&mut p, &[], &[]);
        assert!(out.finalized.is_empty() && out.active.is_empty());
        let out = plan(&mut p, &[], &[block("msg", "A.\n\nB")]);
        assert!(out.finalized.contains_key("msg"));
        assert!(
            !text_of(&[out.finalized["msg"].clone()])
                .iter()
                .any(|l| l.contains("A."))
        );
    }

    #[test]
    fn a_stream_that_never_wrote_anything_takes_the_normal_path() {
        let mut p = StreamPlanner::new();
        plan(&mut p, &[block("live", "short")], &[]);
        let out = plan(&mut p, &[], &[block("msg", "short")]);
        assert!(
            out.finalized.is_empty(),
            "the settled block commits whole elsewhere"
        );
    }

    #[test]
    fn a_reidentified_stream_keeps_its_watermark() {
        let mut p = StreamPlanner::new();
        plan(&mut p, &[block("old", "Done.\n\nMore")], &[]);
        let out = plan(&mut p, &[block("new", "Done.\n\nMore\n\nEven more")], &[]);
        let rows = text_of(&out.active);
        assert!(!rows.iter().any(|l| l.contains("Done.")), "{rows:?}");
        assert!(rows.iter().any(|l| l.contains("More")));
    }

    #[test]
    fn a_new_session_revision_forgets_everything() {
        let mut p = StreamPlanner::new();
        p.plan(1, &[block("a", "X.\n\nY")], &[], W, &render_marked);
        let out = p.plan(2, &[block("a", "X.\n\nY")], &[], W, &render_marked);
        assert_eq!(text_of(&out.active).len(), 1, "committed again from zero");
    }

    /// The whole path: a streamed answer fed one character at a time through
    /// the planner and the screen leaves every block in scrollback exactly
    /// once, in order, and the unclosed tail only in the live region.
    #[test]
    fn character_stream_commits_each_block_once_through_the_screen() {
        use crate::render::testterm::Emu;
        use crate::render::{Frame, Screen};

        let answer = "## Title\n\nFirst paragraph here.\n\n- one\n- two\n\n```ts\nlet x = 1;\n```\n\nLast line.";
        let (cols, rows) = (50u16, 14u16);
        let mut screen = Screen::new((cols, rows), true);
        let mut emu = Emu::new(cols as usize, rows as usize);
        let width = cols as usize - 3;
        let mut planner = StreamPlanner::new();
        let mut live_text = String::new();

        let mut step = |planner: &mut StreamPlanner,
                        live: &[TextBlock],
                        settled: &[TextBlock],
                        emu: &mut Emu,
                        screen: &mut Screen| {
            let plan = planner.plan(1, live, settled, width, &render_marked);
            let mut commits: Vec<String> = Vec::new();
            for (_, batch) in plan.finalized {
                commits.push(batch.lines.join("\n"));
            }
            for batch in plan.active {
                commits.push(batch.lines.join("\n"));
            }
            let mut tail = Vec::new();
            for block in live {
                if let Some((rest, _)) = planner.pending(&block.id) {
                    tail.extend(rest.lines().map(|l| format!("~{l}")));
                }
            }
            tail.push("composer".to_owned());
            let frame = Frame {
                commits,
                live: tail,
                cursor: None,
            };
            emu.feed(&screen.render(&frame, (cols, rows)));
        };

        step(&mut planner, &[], &[], &mut emu, &mut screen);
        for ch in answer.chars() {
            live_text.push(ch);
            step(
                &mut planner,
                &[block("s", &live_text)],
                &[],
                &mut emu,
                &mut screen,
            );
        }
        // Run ends: the live row is replaced by its settled twin.
        step(
            &mut planner,
            &[],
            &[block("s", answer)],
            &mut emu,
            &mut screen,
        );

        let transcript = emu.transcript();
        for phrase in [
            "Title",
            "First paragraph here.",
            "one",
            "two",
            "let x = 1;",
            "Last line.",
        ] {
            let count = transcript
                .iter()
                .filter(|l| l.contains(phrase) && !l.starts_with('~'))
                .count();
            assert_eq!(
                count, 1,
                "{phrase:?} must be committed exactly once: {transcript:#?}"
            );
        }
        // Order is preserved.
        let positions: Vec<usize> = [
            "Title",
            "First paragraph here.",
            "one",
            "let x = 1;",
            "Last line.",
        ]
        .iter()
        .map(|p| transcript.iter().position(|l| l.contains(p)).unwrap())
        .collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{positions:?}");
        // Nothing of the unclosed tail is left behind after settling.
        assert!(!emu.rows_text().iter().any(|r| r.starts_with('~')));
    }
}
