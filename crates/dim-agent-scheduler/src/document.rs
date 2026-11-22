//! The `schedule` settings document: the jobs a user writes, and their
//! validation at the document boundary ([design](../../../docs/design/plugins/Scheduler.md),
//! A.1, A.4, §2.3, §7.1).
use croner::Cron;
use dim_agent_types::{DEFAULT_BRANCH_ID, InputMode, PermissionPolicy, PermissionPreset};
use dim_app_server::settings::{BusinessDocument, SettingsError};
use jiff::{
    SignedDuration, Timestamp,
    tz::{AmbiguousOffset, TimeZone},
};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{MapAccess, Visitor, value::MapAccessDeserializer},
};
use std::{collections::HashSet, marker::PhantomData, str::FromStr};

pub const MAX_PROMPT_BYTES: usize = 32 * 1024;
pub const DEFAULT_MAX_RUN_MINUTES: u32 = 60;
pub const DEFAULT_MAX_CONCURRENT_RUNS: u32 = 4;
/// The most jobs one Session may create through the `schedule` tool (A13).
pub const MAX_TOOL_JOBS_PER_SESSION: usize = 16;

/// The `schedule` document. `Default` is what Settings uses when no file
/// exists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ScheduleDocument {
    pub jobs: Vec<Job>,
    /// Retained for document compatibility; it does not control run admission.
    pub max_concurrent_runs: u32,
}

impl Default for ScheduleDocument {
    fn default() -> Self {
        Self {
            jobs: Vec::new(),
            max_concurrent_runs: DEFAULT_MAX_CONCURRENT_RUNS,
        }
    }
}

impl BusinessDocument for ScheduleDocument {
    const NAMESPACE: &'static str = "schedule";
    const USER_FILE: &'static str = "schedule.json";
    fn validate(&self) -> Result<(), SettingsError> {
        if self.max_concurrent_runs < 1 {
            return Err(SettingsError::Invalid(
                "max_concurrent_runs must be at least 1",
            ));
        }
        let mut ids = HashSet::new();
        for job in &self.jobs {
            if !ids.insert(job.id.as_str()) {
                return Err(SettingsError::Invalid("duplicate job id"));
            }
            job.validate()?;
        }
        Ok(())
    }
}

/// A legacy job's `mode` (§2.3). New jobs store a target instead and never
/// write this field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Create,
    Reuse,
}

/// Who created a job (A.1). It decides a legacy job's target (§2.3) and is
/// otherwise provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "tool")]
    Tool {
        session_id: String,
        branch_id: String,
    },
}

/// A job's target (§2.1): the branch that saved the job, or a Session each
/// window launches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    Session {
        session_id: String,
        #[serde(default = "default_branch_id")]
        branch_id: String,
        #[serde(default)]
        input_mode: InputMode,
    },
    Launch {
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        policy: Option<PermissionPreset>,
        #[serde(default = "default_max_run_minutes")]
        max_run_minutes: u32,
    },
}

/// Why a delivery's target-gone failure disabled the job (A.1, A.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisabledReason {
    pub code: String,
    /// The UTC instant the delivery settled at.
    pub at: String,
}

fn default_branch_id() -> String {
    DEFAULT_BRANCH_ID.to_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub schedule: Schedule,
    pub prompt: String,
    /// The job's target. A job saved before this change has none and is
    /// mapped by [`Job::effective_target`] (A.1, §2.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<DisabledReason>,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_at: Option<String>,
    pub revision: u64,
    pub created_by: Origin,
    // Legacy fields (A.1, §2.3): read and mapped, preserved by every write,
    // never reported, and never written by a save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PermissionPreset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_cap: Option<PermissionPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_run_minutes: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelSelection {
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    untagged,
    expecting = r#"exactly one schedule object: {"cron":"*/30 * * * *","tz":"Asia/Shanghai"} (five-field cron; optional IANA tz, host-local if omitted), {"every_minutes":30} (positive integer), or {"at":"2030-01-01T00:00:00Z"} (RFC 3339 instant with UTC offset); no mixed forms or unknown fields"#
)]
pub enum Schedule {
    Cron(#[serde(deserialize_with = "schedule_object")] CronSchedule),
    Every(#[serde(deserialize_with = "schedule_object")] EverySchedule),
    At(#[serde(deserialize_with = "schedule_object")] AtSchedule),
}

fn schedule_object<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Object<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for Object<T> {
        type Value = T;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a schedule object")
        }

        fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<T, M::Error> {
            T::deserialize(MapAccessDeserializer::new(map))
        }
    }

    deserializer.deserialize_map(Object(PhantomData))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CronSchedule {
    pub cron: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EverySchedule {
    pub every_minutes: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AtSchedule {
    #[serde(with = "dim_agent_capabilities::timestamp")]
    pub at: Timestamp,
}

/// The time of day a timestamp field holds: UTC with three fractional digits.
pub fn canonical_instant(text: &str) -> Option<Timestamp> {
    let instant = dim_agent_capabilities::timestamp::parse(text).ok()?;
    (format_instant(instant) == text
        || dim_agent_capabilities::timestamp::leap_second_form(instant).as_deref() == Some(text))
    .then_some(instant)
}

pub(crate) fn default_max_run_minutes() -> u32 {
    DEFAULT_MAX_RUN_MINUTES
}

pub fn format_instant(instant: Timestamp) -> String {
    instant.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

impl Job {
    /// The target this job delivers to: its stored target, or the fixed
    /// legacy mapping of §2.3. `None` is a document the mapping cannot
    /// express, which validation rejects.
    pub fn effective_target(&self) -> Option<Target> {
        if let Some(target) = &self.target {
            return Some(target.clone());
        }
        match &self.created_by {
            Origin::Tool {
                session_id,
                branch_id,
            } => Some(Target::Session {
                session_id: session_id.clone(),
                branch_id: branch_id.clone(),
                input_mode: InputMode::Queue,
            }),
            Origin::User => {
                if self.mode == Some(Mode::Reuse)
                    && let Some(session_id) = &self.session_id
                {
                    return Some(Target::Session {
                        session_id: session_id.clone(),
                        branch_id: default_branch_id(),
                        input_mode: InputMode::Queue,
                    });
                }
                Some(Target::Launch {
                    cwd: self.cwd.clone()?,
                    agent_id: self.agent_id.clone(),
                    policy: self.policy,
                    max_run_minutes: self.max_run_minutes.unwrap_or(DEFAULT_MAX_RUN_MINUTES),
                })
            }
        }
    }

    /// Whether the job's stored target replaced the Session its legacy
    /// `reuse` binding names (A.7): the one legacy form that releases the
    /// binding's launch ownership. A client that sends the mapped target
    /// explicitly has not replaced it, so nothing is released.
    pub fn replaced_reuse_binding(&self) -> bool {
        if self.mode != Some(Mode::Reuse) {
            return false;
        }
        let (Some(session_id), Some(target)) = (&self.session_id, &self.target) else {
            return false;
        };
        *target
            != Target::Session {
                session_id: session_id.clone(),
                branch_id: default_branch_id(),
                input_mode: InputMode::Queue,
            }
    }

    pub fn validate(&self) -> Result<(), SettingsError> {
        if self.name.is_empty() {
            return Err(SettingsError::Invalid("a job needs a name"));
        }
        if self.prompt.len() > MAX_PROMPT_BYTES {
            return Err(SettingsError::Invalid("a job prompt is at most 32 KiB"));
        }
        match &self.target {
            Some(Target::Session {
                session_id,
                branch_id,
                ..
            }) => {
                if session_id.is_empty() || branch_id.is_empty() {
                    return Err(SettingsError::Invalid(
                        "a session target needs a Session and a branch",
                    ));
                }
            }
            Some(Target::Launch {
                cwd,
                max_run_minutes,
                ..
            }) => {
                if cwd.is_empty() {
                    return Err(SettingsError::Invalid("a launch target needs a cwd"));
                }
                if *max_run_minutes < 1 {
                    return Err(SettingsError::Invalid("max_run_minutes must be at least 1"));
                }
            }
            None => match self.effective_target() {
                Some(Target::Session { .. }) => {}
                Some(Target::Launch {
                    cwd,
                    max_run_minutes,
                    ..
                }) if !cwd.is_empty() && max_run_minutes >= 1 => {}
                _ => {
                    return Err(SettingsError::Invalid(
                        "a job needs a target or the legacy fields it maps from",
                    ));
                }
            },
        }
        if let Some(reason) = &self.disabled_reason
            && (!matches!(
                reason.code.as_str(),
                "session_closed" | "branch_missing" | "branch_ended"
            ) || canonical_instant(&reason.at).is_none())
        {
            return Err(SettingsError::Invalid(
                "disabled_reason needs a target-gone code and an instant",
            ));
        }
        match (self.enabled, &self.enabled_at) {
            (true, Some(text)) if canonical_instant(text).is_some() => {}
            (false, None) => {}
            _ => return Err(SettingsError::Invalid("enabled_at matches enabled")),
        }
        self.schedule.validate()
    }

    /// The job as an owner reports it (A.1, A.3): the normalised record
    /// shape with its target, and none of the legacy fields.
    pub fn reported(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "schedule": self.schedule,
            "prompt": self.prompt,
            "target": self.effective_target(),
            "disabled_reason": self.disabled_reason,
            "enabled": self.enabled,
            "enabled_at": self.enabled_at,
            "revision": self.revision,
            "created_by": self.created_by,
        })
    }

    /// The latest window at or before `now`, if any has come due (A.4): after
    /// a gap only this one is considered.
    pub fn latest_window(&self, now: Timestamp) -> Result<Option<Timestamp>, String> {
        if !self.enabled {
            return Ok(None);
        }
        let anchor = self.enabled_at.as_deref().and_then(canonical_instant);
        match &self.schedule {
            Schedule::At(at) => Ok((at.at <= now).then_some(at.at)),
            Schedule::Every(every) => {
                let anchor = anchor.ok_or("an every schedule needs enabled_at")?;
                if anchor > now {
                    return Ok(None);
                }
                let step = i64::from(every.every_minutes) * 60;
                let k = now.duration_since(anchor).as_secs() / step;
                // The first window is one interval after the anchor (A.4), so
                // none has come due yet before then.
                if k == 0 {
                    return Ok(None);
                }
                let offset = k.checked_mul(step).ok_or("window overflow")?;
                anchor
                    .checked_add(SignedDuration::from_secs(offset))
                    .map(Some)
                    .map_err(|_| "window overflow".to_owned())
            }
            Schedule::Cron(cron) => {
                let parsed = cron_for(cron)?;
                let mut cursor = now
                    .checked_add(SignedDuration::from_secs(1))
                    .map_err(|_| "window overflow".to_owned())?;
                for _ in 0..MAX_CRON_SKIPS {
                    let previous = parsed
                        .cron
                        .find_previous_occurrence(&cursor.to_zoned(parsed.zone.clone()), false)
                        .map(|previous| previous.timestamp())
                        .map_err(|error| error.to_string())?;
                    if parsed.fires_at(previous) {
                        return Ok(Some(previous));
                    }
                    cursor = previous;
                }
                Ok(None)
            }
        }
    }

    /// The first window after `after` (A.4). It is not capped by `now`: a
    /// window already due is still next until the timer runs it.
    pub fn next_window(&self, after: Timestamp) -> Result<Option<Timestamp>, String> {
        if !self.enabled {
            return Ok(None);
        }
        self.schedule.next_after(
            after,
            self.enabled_at.as_deref().and_then(canonical_instant),
        )
    }
}

impl Schedule {
    fn validate(&self) -> Result<(), SettingsError> {
        match self {
            Self::Cron(cron) => cron_for(cron).map(|_| ()).map_err(SettingsError::Invalid),
            Self::Every(every) if every.every_minutes >= 1 => Ok(()),
            Self::Every(_) => Err(SettingsError::Invalid(
                "every_minutes must be a positive integer, e.g. {\"every_minutes\":30}",
            )),
            Self::At(_) => Ok(()),
        }
    }

    /// The first window after `after`; an `every` schedule anchors at the
    /// enable time, and an `at` schedule is one instant.
    fn next_after(
        &self,
        after: Timestamp,
        anchor: Option<Timestamp>,
    ) -> Result<Option<Timestamp>, String> {
        match self {
            Self::At(at) => Ok((at.at > after).then_some(at.at)),
            Self::Every(every) => {
                let anchor = anchor.ok_or("an every schedule needs enabled_at")?;
                let step = i64::from(every.every_minutes) * 60;
                // Windows are `anchor + k·n` for k >= 1: a saved job delivers
                // nothing at its anchor, and re-enabling resets the anchor the
                // same way (A.4).
                let k = if after > anchor {
                    after.duration_since(anchor).as_secs() / step + 1
                } else {
                    1
                };
                let offset = k.checked_mul(step).ok_or("window overflow")?;
                Ok(Some(
                    anchor
                        .checked_add(SignedDuration::from_secs(offset))
                        .map_err(|_| "window overflow".to_owned())?,
                ))
            }
            Self::Cron(cron) => {
                let parsed = cron_for(cron).map_err(|error| error.to_string())?;
                let mut cursor = after;
                for _ in 0..MAX_CRON_SKIPS {
                    let next = parsed
                        .cron
                        .find_next_occurrence(&cursor.to_zoned(parsed.zone.clone()), false)
                        .map(|next| next.timestamp())
                        .map_err(|error| error.to_string())?;
                    if parsed.fires_at(next) {
                        return Ok(Some(next));
                    }
                    cursor = next;
                }
                Ok(None)
            }
        }
    }
}

/// How many DST-shifted or repeated occurrences a cron search passes over
/// before giving up; each skip is at least one minute.
const MAX_CRON_SKIPS: usize = 8;

impl ParsedCron {
    /// Whether `instant` is a real window (A.4): its wall time in the zone
    /// matches the pattern (a time inside a DST gap does not exist, so the
    /// occurrence moved out of it is skipped), and it is the first instant
    /// with that wall time (a repeated fall-back time fires once).
    fn fires_at(&self, instant: Timestamp) -> bool {
        first_of_wall_time(&self.cron, instant, &self.zone)
    }
}

struct ParsedCron {
    cron: Cron,
    zone: TimeZone,
}

/// Whether the wall time of `instant` in `zone` matches `cron` and no earlier
/// instant has that wall time: the zone resolves it to `instant` or, when the
/// time repeats at a fall-back, to its earlier occurrence.
fn first_of_wall_time(cron: &Cron, instant: Timestamp, zone: &TimeZone) -> bool {
    let local = instant.to_zoned(zone.clone());
    if !cron.is_time_matching(&local).unwrap_or(false) {
        return false;
    }
    let ambiguous = zone.to_ambiguous_zoned(local.datetime());
    match ambiguous.offset() {
        AmbiguousOffset::Unambiguous { .. } => true,
        AmbiguousOffset::Gap { .. } => false,
        AmbiguousOffset::Fold { .. } => ambiguous
            .earlier()
            .is_ok_and(|earlier| earlier.timestamp() == instant),
    }
}

/// Five-field cron with an optional IANA time zone (A.4).
fn cron_for(cron: &CronSchedule) -> Result<ParsedCron, &'static str> {
    if cron.cron.split_whitespace().count() != 5 {
        return Err("cron needs five fields, e.g. {\"cron\":\"*/30 * * * *\"}");
    }
    let zone = match &cron.tz {
        Some(name) => TimeZone::get(name).map_err(|_| {
            "tz must be an IANA time zone, e.g. {\"cron\":\"*/30 * * * *\",\"tz\":\"Asia/Shanghai\"}, or omit tz for the host's local time zone"
        })?,
        None => TimeZone::system(),
    };
    let parsed = Cron::from_str(&cron.cron)
        .map_err(|_| "invalid cron expression, e.g. {\"cron\":\"*/30 * * * *\"}")?;
    Ok(ParsedCron { cron: parsed, zone })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn at_schedule_keeps_the_chrono_fraction_bytes() {
        for text in [
            "2030-01-01T00:00:00Z",
            "2030-01-01T00:00:00.100Z",
            "2030-01-01T00:00:00.123400Z",
        ] {
            let schedule: Schedule = serde_json::from_value(json!({ "at": text })).unwrap();
            assert_eq!(
                serde_json::to_value(&schedule).unwrap(),
                json!({ "at": text })
            );
        }
    }

    #[test]
    fn a_leap_second_schedule_normalizes_to_the_following_second() {
        let schedule: Schedule =
            serde_json::from_value(json!({ "at": "2016-12-31T23:59:60Z" })).unwrap();
        assert_eq!(
            serde_json::to_value(&schedule).unwrap(),
            json!({ "at": "2017-01-01T00:00:00Z" })
        );
        let instant = canonical_instant("2016-12-31T23:59:60.000Z").unwrap();
        assert_eq!(format_instant(instant), "2017-01-01T00:00:00.000Z");
    }

    #[test]
    fn an_every_window_beyond_the_timestamp_range_is_an_error() {
        let schedule = Schedule::Every(EverySchedule {
            every_minutes: u32::MAX,
        });
        let anchor: Timestamp = "2026-11-17T00:00:00Z".parse().unwrap();
        assert!(schedule.next_after(anchor, Some(anchor)).is_err());
    }
}
