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
pub struct TextBlock<'a> {
    pub id: &'a str,
    pub text: &'a str,
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
    /// The rows extend the block the previous batch began (an open code
    /// fence commits line by line): no blank row before them, and `source`
    /// is the whole block so far, replacing the earlier one.
    pub continues: bool,
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
    /// Rows of the block at `committed_len` already written (an open fence's
    /// finished lines).
    open_rows: usize,
    /// The marker run of the fence open at the end of the text.
    open_fence: Option<String>,
}

impl ActiveStream {
    /// Some of the stream is in scrollback: whole blocks, or the finished
    /// lines of an open fence.
    fn wrote(&self) -> bool {
        self.committed_len > 0 || self.open_rows > 0
    }
}

/// The live part of a stream (see [`StreamPlanner::pending`]).
pub struct Pending<'a> {
    pub text: &'a str,
    pub first: bool,
    /// Rendered rows of `text` already committed: the live region skips them.
    pub written: usize,
    /// The marker run of a fence still open at the end of `text`.
    pub open_fence: Option<&'a str>,
}

/// Lays out `text` whose last fence is still open as the code block it will
/// be: the fence is closed for the renderer (an unclosed fence would read as
/// plain text) and the closing row dropped again.
pub fn render_open(
    text: &str,
    open_fence: Option<&str>,
    render: &dyn Fn(&str) -> Vec<String>,
) -> Vec<String> {
    let Some(run) = open_fence else {
        return render(text);
    };
    let newline = if text.ends_with('\n') { "" } else { "\n" };
    let mut rows = render(&format!("{text}{newline}{run}"));
    rows.pop();
    rows
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
        let live_ids: Vec<&str> = live.iter().map(|b| b.id).collect();
        let (still, gone): (Vec<_>, Vec<_>) = std::mem::take(&mut self.active)
            .into_iter()
            .partition(|s| live_ids.contains(&s.id.as_str()));
        self.active = still;
        for stream in gone {
            // Nothing went out: the settled twin takes the normal path.
            if stream.wrote() {
                self.parked.push(stream);
            }
        }

        let mut plan = Plan::default();
        let mut used_settled: Vec<&str> = Vec::new();
        for mut stream in std::mem::take(&mut self.parked) {
            let prefix = &stream.text[..stream.committed_len];
            let twin = settled
                .iter()
                .find(|b| !used_settled.contains(&b.id) && b.text.starts_with(prefix));
            match twin {
                None => self.parked.push(stream),
                Some(block) => {
                    used_settled.push(block.id);
                    // The persisted text is the authority on the remainder.
                    stream.text = block.text.to_owned();
                    let batch = plan_blocks(&mut stream, width, true, render);
                    plan.finalized.insert(block.id.to_owned(), batch);
                }
            }
        }

        for block in live {
            let existing = self.take_active(block.id).or_else(|| self.unpark(block));
            let mut stream = match existing {
                Some(mut stream) if block.text.starts_with(&stream.text) => {
                    // An unchanged stream keeps its copy.
                    if block.text.len() != stream.text.len() {
                        stream.text = block.text.to_owned();
                    }
                    stream
                }
                other => {
                    // A fresh stream: a parked one is never going to settle
                    // (an interrupt discarded its buffer).
                    drop(other);
                    self.parked.clear();
                    ActiveStream {
                        id: block.id.to_owned(),
                        text: block.text.to_owned(),
                        committed_len: 0,
                        committed_rows: 0,
                        open_rows: 0,
                        open_fence: None,
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

    /// The part of a live stream's text whose block is not committed yet,
    /// whether it opens the message, and how many of its rendered rows are
    /// already written (they are skipped). The live region renders this
    /// through the same renderer the commit uses, so a row never changes
    /// shape when it moves from the live region into scrollback.
    pub fn pending(&self, id: &str) -> Option<Pending<'_>> {
        let stream = self.active.iter().find(|s| s.id == id)?;
        Some(Pending {
            text: &stream.text[stream.committed_len..],
            first: stream.committed_rows == 0,
            written: stream.open_rows,
            open_fence: stream.open_fence.as_deref(),
        })
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
        let live_ids: Vec<&str> = live.iter().map(|b| b.id).collect();
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
                        && s.wrote()
                        && block.text.starts_with(&s.text[..s.committed_len])
                })
                .max_by_key(|(_, s)| s.committed_len)
                .map(|(i, _)| i);
            if let Some(index) = candidate {
                let stream = &mut self.active[index];
                stream.id = block.id.to_owned();
                stream.text = block.text.to_owned();
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
    let split = (!finalize).then(|| split_streaming_markdown(&stream.text));
    // A diagram is laid out from its whole source: it neither streams its
    // lines out nor closes early, and its source shows until it closes.
    stream.open_fence = split.as_ref().and_then(|s| {
        let diagram = s
            .open_fence_info
            .as_deref()
            .and_then(|info| info.split_whitespace().next())
            .is_some_and(|lang| lang.eq_ignore_ascii_case("mermaid"));
        s.open_fence.clone().filter(|_| !diagram)
    });
    let end = split.as_ref().map_or(stream.text.len(), |s| s.settled_len);
    let first = stream.committed_rows == 0;
    let batch = |stream: &ActiveStream, source: &str, lines: Vec<String>| Batch {
        lines: lines[stream.open_rows.min(lines.len())..].to_vec(),
        source: Some(BatchSource {
            text: source.to_owned(),
            first,
        }),
        continues: stream.open_rows > 0,
    };
    if end > stream.committed_len {
        // An answer that ended inside a fence (cut off at max_tokens, say)
        // closes it, so the block reads as code to the end, like its rows
        // already written.
        let closed;
        let mut source = &stream.text[stream.committed_len..end];
        if finalize && let Some(run) = split_streaming_markdown(&stream.text).open_fence {
            let newline = if source.ends_with('\n') { "" } else { "\n" };
            closed = format!("{source}{newline}{run}");
            source = &closed;
        }
        let lines = render(source, content_width, gutter, first);
        let out = batch(stream, source, lines.clone());
        stream.committed_len = end;
        stream.committed_rows += lines.len();
        stream.open_rows = 0;
        return out;
    }
    // An open fence writes its finished lines as they come, so a long code
    // block never piles up in the live region: highlighting only looks back,
    // so a finished line's rows never change.
    if let Some(run) = stream.open_fence.clone() {
        let stable = stream.text.rfind('\n').map_or(0, |at| at + 1);
        if stable > stream.committed_len {
            let source = &stream.text[stream.committed_len..stable];
            let lines = render_open(source, Some(&run), &|text| {
                render(text, content_width, gutter, first)
            });
            if lines.len() > stream.open_rows {
                let out = batch(stream, source, lines.clone());
                stream.open_rows = lines.len();
                return out;
            }
        }
    }
    Batch::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::transcript::blocks::{ASSISTANT_MARK, apply_gutter_mark, render_plain};

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

    fn block<'a>(id: &'a str, text: &'a str) -> TextBlock<'a> {
        TextBlock { id, text }
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
    fn an_open_fence_writes_its_finished_lines_as_they_come() {
        let mut p = StreamPlanner::new();
        let fence = "Intro\n\n```rust\nfn a() {}\nfn b";
        // The intro closes; the fence's finished lines follow next frame.
        let out = plan(&mut p, &[block("a", fence)], &[]);
        assert_eq!(text_of(&out.active), vec!["◆ Intro"]);
        let out = plan(&mut p, &[block("a", fence)], &[]);
        assert_eq!(text_of(&out.active), vec!["```rust", "fn a() {}"]);
        assert!(!out.active[0].continues);
        // The unfinished line stays live, skipping the rows already out.
        assert_eq!(p.pending("a").map(|pending| pending.written), Some(2));
        let more = "Intro\n\n```rust\nfn a() {}\nfn b() {}\n";
        let out = plan(&mut p, &[block("a", more)], &[]);
        assert_eq!(text_of(&out.active), vec!["fn b() {}"]);
        assert!(out.active[0].continues);
        // The whole fence so far is the history source.
        let source = out.active[0].source.as_ref().unwrap();
        assert_eq!(source.text, "```rust\nfn a() {}\nfn b() {}\n");
        // Settling writes the closing fence only.
        let done = "Intro\n\n```rust\nfn a() {}\nfn b() {}\n```";
        let out = plan(&mut p, &[], &[block("msg", done)]);
        let batch = out.finalized.get("msg").expect("finalized");
        assert!(batch.continues);
        assert_eq!(text_of(std::slice::from_ref(batch)), vec!["```"]);
    }

    #[test]
    fn a_diagram_fence_waits_for_its_whole_source() {
        let mut p = StreamPlanner::new();
        let open = "```mermaid\nflowchart LR\nA --> B\n";
        assert!(plan(&mut p, &[block("a", open)], &[]).active.is_empty());
        let pending = p.pending("a").expect("live");
        assert_eq!((pending.written, pending.open_fence), (0, None));
    }

    #[test]
    fn an_answer_cut_off_inside_a_fence_closes_it() {
        let mut p = StreamPlanner::new();
        let cut = "```prolog\nfib(0, 0).\nfib(N, F) :- N >";
        plan(&mut p, &[block("a", cut)], &[]);
        let out = plan(&mut p, &[], &[block("msg", cut)]);
        let batch = out.finalized.get("msg").expect("finalized");
        assert_eq!(
            text_of(std::slice::from_ref(batch)),
            vec!["fib(N, F) :- N >", "```"]
        );
        assert!(batch.source.as_ref().unwrap().text.ends_with("N >\n```"));
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
        use crate::ui::render::testterm::Emu;
        use crate::ui::render::{Frame, Screen};

        let answer = "## Title\n\nFirst paragraph here.\n\n- one\n- two\n\n```ts\nlet x = 1;\n```\n\nLast line.";
        let (cols, rows) = (50u16, 14u16);
        let mut screen = Screen::new((cols, rows), true);
        let mut emu = Emu::new(cols as usize, rows as usize);
        let width = cols as usize - 3;
        let mut planner = StreamPlanner::new();
        let mut live_text = String::new();

        let step = |planner: &mut StreamPlanner,
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
                if let Some(Pending { text: rest, .. }) = planner.pending(block.id) {
                    tail.extend(rest.lines().map(|l| format!("~{l}")));
                }
            }
            tail.push("composer".to_owned());
            let frame = Frame {
                commits,
                live: tail,
                cursor: None,
                ..Frame::default()
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
