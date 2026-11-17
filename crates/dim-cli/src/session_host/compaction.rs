use std::collections::{BTreeSet, VecDeque};

use dim_agent_types::{CompactionDetail, ContextCompactionData, ContextCompactionResult};

use super::*;
use crate::model::view::{CompactionDetailState, CompactionTarget};

#[derive(Default)]
pub(super) struct CompactionReads {
    wanted: BTreeSet<CompactionTarget>,
    waiting: VecDeque<CompactionTarget>,
    request: Option<Read>,
}

struct Read {
    id: u64,
    target: CompactionTarget,
    generation: u64,
    current: bool,
}

impl Worker {
    fn compaction_projection(&mut self, target: &CompactionTarget) -> Option<&mut Projection> {
        if target.session_id == self.session_id {
            if target.branch_id == "main" {
                Some(&mut self.projection)
            } else {
                self.lines
                    .get_mut(&target.branch_id)
                    .map(|line| &mut line.projection)
            }
        } else if target.branch_id == "main" {
            self.member_projection(&target.session_id)
        } else {
            None
        }
    }

    fn compaction_visible(&self, target: &CompactionTarget) -> bool {
        if target.session_id == self.session_id {
            target.branch_id == self.focus || self.side.as_ref() == Some(&target.branch_id)
        } else {
            target.branch_id == "main"
                && self
                    .members
                    .selected
                    .as_ref()
                    .is_some_and(|(generation, session)| {
                        *generation == self.projection_id && *session == target.session_id
                    })
        }
    }

    fn compaction_changed(
        &mut self,
        target: &CompactionTarget,
        state: CompactionDetailState,
        data: Option<&ContextCompactionData>,
    ) {
        if self
            .compaction_projection(target)
            .is_some_and(|projection| {
                projection.compaction_detail(&target.event_id, target.sequence, state, data)
            })
        {
            self.changed();
        }
    }

    pub(super) fn compaction_targets(&mut self, targets: BTreeSet<CompactionTarget>) {
        self.compaction
            .waiting
            .retain(|target| targets.contains(target));
        self.compaction
            .waiting
            .extend(targets.difference(&self.compaction.wanted).cloned());
        let removed: Vec<_> = self
            .compaction
            .wanted
            .difference(&targets)
            .cloned()
            .collect();
        for target in removed {
            self.compaction_changed(&target, CompactionDetailState::Unloaded, None);
        }
        if let Some(read) = &mut self.compaction.request
            && !targets.contains(&read.target)
        {
            read.current = false;
        }
        self.compaction.wanted = targets;
    }

    pub(super) async fn poll_compaction(&mut self) {
        if self.compaction.request.is_some() {
            return;
        }
        while let Some(target) = self.compaction.waiting.pop_front() {
            if !self.compaction_visible(&target) {
                continue;
            }
            let unloaded = self
                .compaction_projection(&target)
                .is_some_and(|projection| {
                    projection.rows().iter().any(|row| {
                        row.compaction
                            .as_ref()
                            .and_then(|view| view.detail.as_ref())
                            .is_some_and(|detail| {
                                detail.event_id == target.event_id
                                    && detail.sequence == target.sequence
                                    && detail.state == CompactionDetailState::Unloaded
                            })
                    })
                });
            if !unloaded {
                continue;
            }
            if !self.client.supports(&["session.history.detail"]) {
                self.compaction_changed(
                    &target,
                    CompactionDetailState::Failed(
                        "This daemon cannot read compaction details".into(),
                    ),
                    None,
                );
                continue;
            }
            let params = json!({"sessionId":target.session_id,"branchId":target.branch_id,"eventId":target.event_id,"sequence":target.sequence});
            match self.client.send("session.history.detail", params).await {
                Ok(id) => {
                    self.compaction_changed(&target, CompactionDetailState::Loading, None);
                    self.compaction.request = Some(Read {
                        id,
                        target,
                        generation: self.projection_id,
                        current: true,
                    });
                    return;
                }
                Err(error) => self.compaction_changed(
                    &target,
                    CompactionDetailState::Failed(error.to_string()),
                    None,
                ),
            }
        }
    }

    pub(super) fn owns_compaction(&self, id: u64) -> bool {
        self.compaction
            .request
            .as_ref()
            .is_some_and(|read| read.id == id)
    }

    pub(super) fn compaction_reply(&mut self, result: Result<serde_json::Value, client::RpcError>) {
        let read = self
            .compaction
            .request
            .take()
            .expect("matched detail request");
        if read.generation != self.projection_id
            || !read.current
            || !self.compaction.wanted.contains(&read.target)
            || !self.compaction_visible(&read.target)
        {
            return;
        }
        let detail = result.map_err(|error| error.to_string()).and_then(|value| {
            serde_json::from_value::<CompactionDetail>(value)
                .map_err(|error| format!("Invalid compaction details: {error}"))
        });
        match detail {
            Ok(detail)
                if detail.event_id == read.target.event_id
                    && detail.sequence == read.target.sequence
                    && matches!(
                        detail.data.outcome,
                        ContextCompactionResult::Completed { .. }
                    ) =>
            {
                self.compaction_changed(
                    &read.target,
                    CompactionDetailState::Loaded,
                    Some(&detail.data),
                );
            }
            Ok(_) => self.compaction_changed(
                &read.target,
                CompactionDetailState::Failed(
                    "Compaction detail does not match its requested completed event".into(),
                ),
                None,
            ),
            Err(error) => {
                self.compaction_changed(&read.target, CompactionDetailState::Failed(error), None)
            }
        }
    }
}
