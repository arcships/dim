use crate::{
    AgentError, BoxFuture, PublishCallback, Result, SessionConfig, StoredBranch, StoredEvent,
    TimelineInfo, TimelineKey, TimelineStore, TimelineTrace,
};
use dim_agent_types::{BranchCreateAt, EventKind, RecoverySnapshot};
use std::{collections::BTreeMap, ops::RangeInclusive, sync::Mutex};

/// In-memory storage for standalone Agents, tests and development profiles.
#[derive(Default)]
pub struct MemoryTimelineStore(Mutex<BTreeMap<String, MemorySession>>);
struct MemorySession {
    config: SessionConfig,
    branches: BTreeMap<String, MemoryBranch>,
}
struct MemoryBranch {
    info: TimelineInfo,
    events: Vec<StoredEvent>,
    snapshots: BTreeMap<(String, u64), RecoverySnapshot>,
}
impl MemoryTimelineStore {
    pub(crate) fn remove_session(&self, id: &str) {
        self.0.lock().unwrap().remove(id);
    }
}
impl TimelineStore for MemoryTimelineStore {
    fn create_session<'a>(
        &'a self,
        session_id: &'a str,
        config: &'a SessionConfig,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut sessions = self.0.lock().unwrap();
            if sessions.contains_key(session_id) {
                return Err(AgentError::Storage(format!(
                    "session {session_id} already exists"
                )));
            }
            let mut session = MemorySession {
                config: config.clone(),
                branches: BTreeMap::new(),
            };
            session.branches.insert(
                "main".into(),
                MemoryBranch {
                    info: TimelineInfo {
                        id: "main".into(),
                        parent: None,
                        branch_point: None,
                        create_anchor: None,
                    },
                    events: vec![],
                    snapshots: BTreeMap::new(),
                },
            );
            sessions.insert(session_id.into(), session);
            Ok(())
        })
    }
    fn rollback_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.remove_session(session_id);
            Ok(())
        })
    }
    fn session_config<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<SessionConfig>>> {
        Box::pin(async move {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(session_id)
                .map(|s| s.config.clone()))
        })
    }
    fn branches<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<Vec<StoredBranch>>> {
        Box::pin(async move {
            let sessions = self.0.lock().unwrap();
            let session = sessions
                .get(session_id)
                .ok_or_else(|| AgentError::Storage(format!("unknown session {session_id}")))?;
            Ok(session
                .branches
                .values()
                .map(|branch| StoredBranch {
                    info: branch.info.clone(),
                    sequence: branch.events.len() as u64,
                })
                .collect())
        })
    }
    fn head<'a>(&'a self, key: &'a TimelineKey) -> BoxFuture<'a, Result<u64>> {
        Box::pin(async move {
            let sessions = self.0.lock().unwrap();
            let session = sessions.get(&key.session_id).ok_or_else(|| {
                AgentError::Storage(format!("unknown session {}", key.session_id))
            })?;
            let branch = session
                .branches
                .get(&key.branch_id)
                .ok_or_else(|| AgentError::Storage(format!("unknown branch {}", key.branch_id)))?;
            Ok(branch.events.len() as u64)
        })
    }
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> Result<()> {
        let mut sessions = self.0.lock().unwrap();
        let session = sessions
            .get_mut(&key.session_id)
            .ok_or_else(|| AgentError::Storage(format!("unknown session {}", key.session_id)))?;
        if !session.branches.contains_key(&key.branch_id) {
            return Err(AgentError::Storage(format!(
                "unknown branch {}",
                key.branch_id
            )));
        }
        for record in &events {
            if let EventKind::BranchCreate(data) = &record.event.kind {
                session.branches.insert(
                    data.branch.clone(),
                    MemoryBranch {
                        info: TimelineInfo {
                            id: data.branch.clone(),
                            parent: Some(key.branch_id.clone()),
                            branch_point: Some(match data.at {
                                BranchCreateAt::Create {} => record.event.sequence,
                                BranchCreateAt::Sequence { sequence } => sequence,
                            }),
                            create_anchor: Some(record.event.sequence),
                        },
                        events: vec![],
                        snapshots: BTreeMap::new(),
                    },
                );
            }
        }
        session
            .branches
            .get_mut(&key.branch_id)
            .unwrap()
            .events
            .extend(events);
        drop(sessions);
        publish(Ok(()));
        Ok(())
    }
    fn read_segments<'a>(
        &'a self,
        session_id: &'a str,
        segments: Vec<(String, RangeInclusive<u64>)>,
    ) -> BoxFuture<'a, Result<Vec<Vec<StoredEvent>>>> {
        Box::pin(async move {
            let sessions = self.0.lock().unwrap();
            segments
                .into_iter()
                .map(|(branch, range)| {
                    let session = sessions.get(session_id).ok_or_else(|| {
                        AgentError::Storage(format!("unknown session {session_id}"))
                    })?;
                    let events = &session
                        .branches
                        .get(&branch)
                        .ok_or_else(|| AgentError::Storage(format!("unknown branch {branch}")))?
                        .events;
                    let end = *range.end();
                    if end == 0 {
                        return Ok(vec![]);
                    }
                    if events
                        .get((end - 1) as usize)
                        .is_some_and(|r| r.batch_end > end)
                    {
                        return Err(AgentError::InvalidInput(format!(
                            "sequence {end} is inside an atomic batch"
                        )));
                    }
                    Ok(events
                        .iter()
                        .filter(|r| range.contains(&r.event.sequence))
                        .cloned()
                        .collect())
                })
                .collect()
        })
    }
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, Result<Option<TimelineTrace>>> {
        Box::pin(async move {
            if sequence == 0 {
                return Ok(None);
            }
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(&key.session_id)
                .and_then(|s| s.branches.get(&key.branch_id))
                .and_then(|b| b.events.get((sequence - 1) as usize))
                .and_then(|e| e.trace.clone()))
        })
    }
    fn save_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        snapshot: RecoverySnapshot,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut sessions = self.0.lock().unwrap();
            let branch = sessions
                .get_mut(&key.session_id)
                .and_then(|session| session.branches.get_mut(&key.branch_id))
                .ok_or_else(|| AgentError::Storage("snapshot branch is missing".into()))?;
            let valid = snapshot
                .sequence
                .checked_sub(1)
                .and_then(|index| branch.events.get(index as usize))
                .is_some_and(|record| record.event.id == snapshot.event_id);
            if !valid {
                return Err(AgentError::InvalidInput(
                    "snapshot anchor is not the identified event".into(),
                ));
            }
            branch
                .snapshots
                .insert((snapshot.interpreter.clone(), snapshot.sequence), snapshot);
            Ok(())
        })
    }
    fn latest_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        through: u64,
        interpreter: &'a str,
    ) -> BoxFuture<'a, Result<Option<RecoverySnapshot>>> {
        Box::pin(async move {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(&key.session_id)
                .and_then(|s| s.branches.get(&key.branch_id))
                .and_then(|b| {
                    b.snapshots
                        .range((interpreter.to_owned(), 0)..=(interpreter.to_owned(), through))
                        .next_back()
                        .map(|(_, snapshot)| snapshot.clone())
                }))
        })
    }
}
