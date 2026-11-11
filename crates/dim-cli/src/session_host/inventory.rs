use super::*;
use dim_client::wire::{
    PendingDelegations as Delegations, PendingInputs as Inputs, PendingQuestions as Questions,
    PendingTasks as Tasks,
};

pub(super) const METHODS: [&str; 6] = [
    "session.question.pending",
    "session.input.pending",
    "session.exec.pending",
    "session.collaboration.pending",
    "session.turn.get",
    "session.goal.get",
];

impl Worker {
    pub(super) fn apply_inventory(
        &mut self,
        branch: &str,
        through: u64,
        method: &str,
        value: serde_json::Value,
    ) -> crate::Result<Option<serde_json::Value>> {
        let (id, cut, next) = match method {
            "session.question.pending" => {
                let page: Questions = serde_json::from_value(value)?;
                for batch in page.batches {
                    self.projection.questions.asked(
                        branch,
                        &batch.event_id,
                        &batch.request_id,
                        &batch.calls,
                    );
                    if let Some(line) = self.lines.get_mut(branch) {
                        line.projection.questions.asked(
                            branch,
                            &batch.event_id,
                            &batch.request_id,
                            &batch.calls,
                        );
                    }
                }
                (page.branch_id, page.through, page.next)
            }
            "session.input.pending" => {
                let page: Inputs = serde_json::from_value(value)?;
                for input in page.inputs {
                    if let Some(projection) =
                        transcript_mut(&mut self.projection, &mut self.lines, branch)
                    {
                        projection.pending_input(&input);
                    }
                }
                (page.branch_id, page.through, page.next)
            }
            "session.exec.pending" => {
                let page: Tasks = serde_json::from_value(value)?;
                for task in page.tasks {
                    self.projection.pending_exec(branch, &task);
                    if let Some(line) = self.lines.get_mut(branch) {
                        line.projection.pending_exec(branch, &task);
                    }
                }
                (page.branch_id, page.through, page.next)
            }
            "session.collaboration.pending" => {
                let page: Delegations = serde_json::from_value(value)?;
                for request in page.requests {
                    self.projection.delegation_started(
                        branch,
                        &request.request,
                        &request.event_id,
                        request.started_at,
                    );
                    if let Some(delivery) = &request.delivery {
                        self.projection.delegation_delivered(
                            branch,
                            &dim_agent_types::CollaborationDeliveredData {
                                op_event_id: request.event_id.clone(),
                                delivery: delivery.clone(),
                            },
                            request.started_at,
                            true,
                        );
                    }
                    if let Some(line) = self.lines.get_mut(branch) {
                        line.projection.delegation_started(
                            branch,
                            &request.request,
                            &request.event_id,
                            request.started_at,
                        );
                        if let Some(delivery) = request.delivery {
                            line.projection.delegation_delivered(
                                branch,
                                &dim_agent_types::CollaborationDeliveredData {
                                    op_event_id: request.event_id,
                                    delivery,
                                },
                                request.started_at,
                                true,
                            );
                        }
                    }
                }
                (page.branch_id, page.through, page.next)
            }
            "session.turn.get" => {
                let turn: crate::wire::TurnRead = serde_json::from_value(value)?;
                if let Some(projection) =
                    transcript_mut(&mut self.projection, &mut self.lines, branch)
                {
                    projection.set_running(turn.turn.is_some_and(|turn| !turn.ended));
                    if let Some(output) = turn.output {
                        projection.active_output(&output);
                    }
                }
                (turn.branch_id, turn.through, None)
            }
            "session.goal.get" => {
                let state: dim_agent_types::GoalRead = serde_json::from_value(value)?;
                if let Some(projection) =
                    transcript_mut(&mut self.projection, &mut self.lines, branch)
                {
                    projection.goal = state.goal;
                }
                return Ok(None);
            }
            _ => unreachable!("inventory method"),
        };
        if id != branch || cut != through {
            return Err("Inventory cutoff or branch changed".into());
        }
        Ok(next)
    }
}
