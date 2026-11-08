use crate::storage::SqliteStorage;
use dim_agent_runtime::{AgentError, Result};
use dim_agent_types::AgentProfile;
use rusqlite::{OptionalExtension, types::Value};
use std::sync::Arc;

pub(crate) struct AgentRecord {
    pub id: String,
    pub profile: std::result::Result<AgentProfile, String>,
    pub default: bool,
    pub owner: Option<String>,
}

pub(crate) struct SessionOwner {
    pub session_id: String,
    pub owner: Option<(String, u32)>,
    pub error: Option<String>,
}

pub(crate) struct AgentRecords(pub Arc<SqliteStorage>);

impl AgentRecords {
    pub async fn agents(&self) -> Result<Vec<AgentRecord>> {
        self.0
            .run_read(|connection| {
                let mut query = connection
                    .prepare(
                        "SELECT agent_id, profile, is_default, owner_session_id FROM agents \
                 ORDER BY is_default DESC, agent_id",
                    )
                    .map_err(storage_error)?;
                let records = query
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Value>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, Option<String>>(3)?,
                        ))
                    })
                    .map_err(storage_error)?;
                let records: Vec<_> = records
                    .map(|row| {
                        let (id, profile, default, owner) = row.map_err(storage_error)?;
                        let default = match default {
                            0 => false,
                            1 => true,
                            _ => {
                                return Err(AgentError::Storage(format!(
                                    "agent {id} has an invalid default marker"
                                )));
                            }
                        };
                        let profile = match profile {
                            Value::Text(profile) => {
                                serde_json::from_str(&profile).map_err(|error| error.to_string())
                            }
                            _ => Err("profile is not JSON text".into()),
                        };
                        Ok(AgentRecord {
                            id,
                            profile,
                            default,
                            owner,
                        })
                    })
                    .collect::<Result<_>>()?;
                if !records.is_empty()
                    && records.iter().filter(|record| record.default).count() != 1
                {
                    return Err(AgentError::Storage(
                        "Agent directory has no unique default Agent".into(),
                    ));
                }
                Ok(records)
            })
            .await
    }

    pub async fn save(
        &self,
        id: &str,
        profile: &AgentProfile,
        default: bool,
        owner: Option<&str>,
    ) -> Result<()> {
        let id = id.to_owned();
        let expected_profile = profile.clone();
        let owner = owner.map(str::to_owned);
        let profile = serde_json::to_string(profile)
            .map_err(|error| AgentError::Storage(error.to_string()))?;
        self.0.run(move |connection| {
            let existing = connection.query_row(
                "SELECT profile, is_default, owner_session_id FROM agents WHERE agent_id = ?1", [&id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, Option<String>>(2)?)),
            ).optional().map_err(storage_error)?;
            match existing {
                Some((stored, is_default, stored_owner)) => {
                    let stored: AgentProfile = serde_json::from_str(&stored)
                        .map_err(|error| AgentError::Storage(format!("agent {id}: {error}")))?;
                    if stored == expected_profile
                        && is_default == i64::from(default)
                        && stored_owner == owner
                    {
                        Ok(())
                    } else if default
                        && is_default == 1
                        && stored_owner == owner
                    {
                        connection.execute("UPDATE agents SET profile = ?1 WHERE agent_id = ?2",
                            rusqlite::params![profile, id]).map_err(storage_error)?;
                        Ok(())
                    } else {
                        Err(AgentError::Storage(format!("agent {id} differs from its stored profile")))
                    }
                },
                None => {
                    connection.execute(
                        "INSERT INTO agents(agent_id, profile, is_default, owner_session_id) \
                         VALUES(?1, ?2, ?3, ?4)",
                        rusqlite::params![id, profile, default, owner]).map_err(storage_error)?;
                    Ok(())
                }
            }
        }).await
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.0
            .run(move |connection| {
                connection
                    .execute("DELETE FROM agents WHERE agent_id = ?1", [&id])
                    .map_err(storage_error)?;
                Ok(())
            })
            .await
    }

    /// The Agents a leader Session owns, in id order.
    pub async fn owned_agents(&self, owner: &str) -> Result<Vec<String>> {
        let owner = owner.to_owned();
        self.0
            .run_read(move |connection| {
                let mut query = connection
                    .prepare(
                        "SELECT agent_id FROM agents WHERE owner_session_id = ?1 ORDER BY agent_id",
                    )
                    .map_err(storage_error)?;
                let ids = query
                    .query_map([&owner], |row| row.get::<_, String>(0))
                    .map_err(storage_error)?;
                ids.collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage_error)
            })
            .await
    }

    /// The member Sessions a leader Session leads, in id order.
    pub async fn led_members(&self, leader: &str) -> Result<Vec<String>> {
        let leader = leader.to_owned();
        self.0
            .run_read(move |connection| {
                let mut query = connection
                    .prepare(
                        "SELECT session_id FROM agent_sessions WHERE leader_session_id = ?1 \
                         AND CASE WHEN json_valid(config) THEN json_extract(config, '$.collaborationRole') END = 'member' \
                         ORDER BY session_id",
                    )
                    .map_err(storage_error)?;
                let ids = query
                    .query_map([&leader], |row| row.get::<_, String>(0))
                    .map_err(storage_error)?;
                ids.collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage_error)
            })
            .await
    }

    /// The stored Sessions of an Agent, in id order.
    pub async fn member_sessions(&self, agent: &str) -> Result<Vec<String>> {
        let agent = agent.to_owned();
        self.0
            .run_read(move |connection| {
                let mut query = connection
                    .prepare("SELECT session_id FROM session_owners WHERE agent_id = ?1 ORDER BY session_id")
                    .map_err(storage_error)?;
                let ids = query
                    .query_map([&agent], |row| row.get::<_, String>(0))
                    .map_err(storage_error)?;
                ids.collect::<rusqlite::Result<Vec<_>>>().map_err(storage_error)
            })
            .await
    }

    /// An Agent's `owner_session_id`: `None` when the Agent is public or
    /// absent, `Some(owner)` when a leader Session owns it.
    pub async fn agent_owner(&self, agent: &str) -> Result<Option<String>> {
        let agent = agent.to_owned();
        self.0
            .run_read(move |connection| {
                connection
                    .query_row(
                        "SELECT owner_session_id FROM agents WHERE agent_id = ?1",
                        [&agent],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .optional()
                    .map(Option::flatten)
                    .map_err(storage_error)
            })
            .await
    }

    /// Every stored member Session and its leader, in Session id order.
    pub async fn members(&self) -> Result<Vec<(String, String)>> {
        self.0
            .run_read(|connection| {
                let mut query = connection
                    .prepare(
                        "SELECT session_id, leader_session_id FROM agent_sessions \
                         WHERE leader_session_id IS NOT NULL \
                         AND CASE WHEN json_valid(config) THEN json_extract(config, '$.collaborationRole') END = 'member' \
                         ORDER BY session_id",
                    )
                    .map_err(storage_error)?;
                let rows = query
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .map_err(storage_error)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage_error)
            })
            .await
    }

    /// Owned Agents left without a member Session, and owned Agents whose
    /// owner row is gone. A completed delegation leaves neither, so startup
    /// removes them.
    pub async fn unfinished_owned(&self) -> Result<(Vec<String>, Vec<String>)> {
        self.0
            .run_read(|connection| {
                let mut query = connection
                    .prepare(
                        "SELECT a.agent_id, \
                         EXISTS (SELECT 1 FROM agent_sessions s WHERE s.session_id = a.owner_session_id) \
                         FROM agents a WHERE a.owner_session_id IS NOT NULL \
                         AND (NOT EXISTS (SELECT 1 FROM agent_sessions s WHERE s.session_id = a.owner_session_id) \
                              OR NOT EXISTS (SELECT 1 FROM session_owners o WHERE o.agent_id = a.agent_id)) \
                         ORDER BY a.agent_id",
                    )
                    .map_err(storage_error)?;
                let rows = query
                    .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)))
                    .map_err(storage_error)?;
                let rows: Vec<_> = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(storage_error)?;
                let (orphaned, incomplete): (Vec<_>, Vec<_>) =
                    rows.into_iter().partition(|(_, owner_exists)| !owner_exists);
                Ok((
                    incomplete.into_iter().map(|(id, _)| id).collect(),
                    orphaned.into_iter().map(|(id, _)| id).collect(),
                ))
            })
            .await
    }

    pub async fn owners(&self) -> Result<Vec<SessionOwner>> {
        self.0.run_read(|connection| {
            let mut query = connection.prepare(
                "SELECT ids.session_id, s.session_id IS NOT NULL, o.agent_id, o.protocol_version, \
                 a.agent_id IS NOT NULL FROM (\
                   SELECT session_id FROM agent_sessions UNION SELECT session_id FROM session_owners\
                 ) ids LEFT JOIN agent_sessions s ON s.session_id = ids.session_id \
                 LEFT JOIN session_owners o ON o.session_id = ids.session_id \
                 LEFT JOIN agents a ON a.agent_id = o.agent_id ORDER BY ids.session_id",
            ).map_err(storage_error)?;
            query.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?, row.get::<_, Value>(2)?,
                    row.get::<_, Value>(3)?, row.get::<_, bool>(4)?))
            }).map_err(storage_error)?.map(|row| {
                let (session_id, has_session, agent, version, has_agent) = row.map_err(storage_error)?;
                let agent_id = match agent {
                    Value::Text(id) => Some(id),
                    _ => None,
                };
                let protocol_version = match version {
                    Value::Integer(version) if (1..=3).contains(&version) => Some(version as u32),
                    _ => None,
                };
                let error = if !has_session {
                    Some("Session directory is missing".into())
                } else if agent_id.is_none() {
                    Some("owning Agent is missing or invalid".into())
                } else if protocol_version.is_none() {
                    Some("protocol version is invalid or unsupported".into())
                } else if !has_agent {
                    Some("owning Agent is missing".into())
                } else {
                    None
                };
                let owner = agent_id.zip(protocol_version);
                Ok(SessionOwner { session_id, owner, error })
            }).collect()
        }).await
    }
}

fn storage_error(error: rusqlite::Error) -> AgentError {
    AgentError::Storage(error.to_string())
}
