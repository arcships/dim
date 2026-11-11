use super::*;

impl Owner {
    pub(super) async fn write_guard(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, AgentError> {
        loop {
            let mut fresh = self.fresh.subscribe();
            loop {
                let status = fresh.borrow_and_update().clone()?;
                if status {
                    break;
                }
                tokio::select! {
                    result = fresh.changed() => { result.map_err(|_| AgentError::Reloading("Goal".into()))?; }
                    _ = self.stop.cancelled() => return Err(AgentError::Closed(self.branch_id.clone())),
                }
            }
            let write = tokio::select! {
                guard = self.write.lock() => guard,
                _ = self.stop.cancelled() => return Err(AgentError::Closed(self.branch_id.clone())),
            };
            self.await_fold().await?;
            let through = self.timeline.committed();
            let mut dispatched = self.dispatcher.watch();
            while self.dispatcher.sequence() < through {
                if let Some(error) = self.dispatcher.failure() {
                    return Err(error);
                }
                tokio::select! {result=dispatched.changed()=>result.map_err(|_|AgentError::Closed("Goal".into()))?,_=self.stop.cancelled()=>return Err(AgentError::Closed("Goal".into()))}
            }
            if self.fresh.borrow().clone()? {
                return Ok(write);
            }
        }
    }

    pub(super) fn refresh(self: &Arc<Self>) {
        let owner = self.clone();
        self.work.spawn(async move {
            if !owner.phase.activated().await {
                return;
            }
            let result = tokio::select! {
                _ = owner.stop.cancelled() => return,
                result = owner.refresh_state() => result,
            };
            if let Err(error) = result {
                tracing::warn!(%error, "Goal restore failed");
                let _ = owner.fresh.send_replace(Err(error));
            }
        });
    }

    async fn refresh_state(self: &Arc<Self>) -> Result<(), AgentError> {
        let write = self.write.lock().await;
        loop {
            let through = self.timeline.sequence();
            let opening = self.opening(through).await?;
            let query = |query| -> BoxFuture<'_, _> { Box::pin(self.timeline.query(query)) };
            let mut loaded = crate::read::load(
                &self.branch_id,
                opening.map(|(at, _)| at),
                through,
                &query,
                &self.stop,
            )
            .await?;
            if self.timeline.committed() > through {
                self.publication(self.timeline.committed()).await?;
                continue;
            }
            crate::read::pause_reloaded(&mut loaded.fold);
            let statement = statement_of_fold(&loaded.fold);
            {
                let mut state = self.state.lock().unwrap();
                if *self.stepped.borrow() > through {
                    continue;
                }
                if loaded.repaired {
                    state.fold = loaded.fold;
                }
                state.start = opening.map(|(at, _)| at);
                state.source_through = through;
                state.refresh_from = None;
                state.decisions = CancellationToken::new();
                state.repair_pending = loaded.repaired;
                self.stepped.send_replace(through);
                let _ = self.fresh.send_replace(Ok(true));
            }
            drop(write);
            if loaded.repaired {
                self.record(through, Some(statement.state), false, None)
                    .await?;
            } else {
                self.resume_decisions();
            }
            return Ok(());
        }
    }

    pub(super) fn resume_decisions(self: &Arc<Self>) {
        if !self.state.lock().unwrap().fold.running() {
            return;
        }
        let pending = self
            .state
            .lock()
            .unwrap()
            .fold
            .pending_conclusion()
            .is_some();
        if pending {
            self.settle_deferred();
        } else {
            self.decide_end();
        }
        self.settle();
        self.answer_change();
    }
}

impl Owner {
    pub(super) async fn load_current_work(&self) -> Result<(), AgentError> {
        let equal = |path: &str, value| QueryFilter::Compare {
            left: QueryOperand::Field { path: path.into() },
            comparison: QueryComparison::Eq,
            right: QueryOperand::Value { value },
        };
        loop {
            let through = self.timeline.committed();
            self.publication(through).await?;
            let mut sets = Vec::with_capacity(2);
            for (origin, terminal, inner, outer) in [
                (
                    "input/message",
                    vec!["input/consumed", "input/deleted"],
                    "data.input_id",
                    "id",
                ),
                (
                    "exec/started",
                    vec!["exec/ended"],
                    "data.task_id",
                    "data.task_id",
                ),
            ] {
                let mut query =
                    recovery::request(&self.branch_id, None, through, &[origin], &[outer]);
                let mut conditions = vec![
                    query.query.filter.take().expect("origin kind"),
                    QueryFilter::Not {
                        condition: Box::new(QueryFilter::Exists {
                            condition: Box::new(QueryFilter::And {
                                conditions: vec![
                                    QueryFilter::In {
                                        value: QueryOperand::Field {
                                            path: "kind".into(),
                                        },
                                        values: terminal
                                            .into_iter()
                                            .map(serde_json::Value::from)
                                            .collect(),
                                    },
                                    QueryFilter::Compare {
                                        left: QueryOperand::Field { path: inner.into() },
                                        comparison: QueryComparison::Eq,
                                        right: QueryOperand::Outer { path: outer.into() },
                                    },
                                ],
                            }),
                        }),
                    },
                ];
                if origin == "input/message" {
                    let present = |path: &str| QueryFilter::Present {
                        value: QueryOperand::Field { path: path.into() },
                    };
                    conditions.push(QueryFilter::Not {
                        condition: Box::new(present("data.handover")),
                    });
                    conditions.push(QueryFilter::Or {
                        conditions: vec![
                            QueryFilter::In {
                                value: QueryOperand::Field {
                                    path: "data.mode".into(),
                                },
                                values: vec!["queue".into(), "steer".into()],
                            },
                            QueryFilter::And {
                                conditions: vec![
                                    QueryFilter::Not {
                                        condition: Box::new(present("data.mode")),
                                    },
                                    present("data.user"),
                                    QueryFilter::Not {
                                        condition: Box::new(equal(
                                            "data.user",
                                            serde_json::Value::Null,
                                        )),
                                    },
                                ],
                            },
                        ],
                    });
                }
                query.query.filter = Some(QueryFilter::And { conditions });
                let mut ids = HashSet::new();
                loop {
                    let reply = self.timeline.query(query.clone()).await?;
                    let QueryOutput::Rows { rows } = reply.result else {
                        unreachable!("current identity fields")
                    };
                    for row in rows {
                        let [
                            QueryValue::Value {
                                value: serde_json::Value::String(id),
                            },
                        ] = row.values.as_slice()
                        else {
                            return Err(AgentError::fault(
                                "Goal",
                                "missing current execution identity",
                            ));
                        };
                        ids.insert(id.clone());
                    }
                    query.page.as_mut().expect("identity page").after = reply.next;
                    if query.page.as_ref().expect("identity page").after.is_none() {
                        break;
                    }
                }
                sets.push(ids);
            }
            let mut dispatched = self.dispatcher.watch();
            while self.dispatcher.sequence() < through {
                if let Some(error) = self.dispatcher.failure() {
                    return Err(error);
                }
                tokio::select! {result=dispatched.changed()=>result.map_err(|_|AgentError::Closed("Goal".into()))?,_=self.stop.cancelled()=>return Err(AgentError::Closed("Goal".into()))}
            }
            let mut state = self.state.lock().unwrap();
            if self.timeline.committed() != through {
                continue;
            }
            let tasks = sets.pop().expect("task identities");
            state.fold.driving = sets.pop().expect("input identities");
            state.fold.open_tasks = tasks;
            return Ok(());
        }
    }
}
