use crate::{
    Error, Migration, ObjectResult, Outcome, Result, TaskState,
    native::{Native, failure},
    source::{ScheduledJob, Source, fingerprint},
};
use jiff::{Timestamp, Zoned, tz::TimeZone};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) async fn import(
    owner: &Migration,
    task: &mut TaskState,
    source: &Source,
    target: &Native,
) -> Result<()> {
    task.processed = 0;
    task.total = None;
    for issue in &source.inventory.issues {
        owner
            .ledger
            .object(
                &task.id,
                &ObjectResult {
                    kind: "file".into(),
                    id: "schedule-tui.sqlite".into(),
                    outcome: Outcome::Failed,
                    target_id: None,
                    reason: Some(issue.clone()),
                    omitted: Vec::new(),
                },
            )
            .await?;
    }
    let sessions = owner
        .ledger
        .report(&task.id)
        .await?
        .objects
        .into_iter()
        .filter(|r| r.kind == "session")
        .map(|r| (r.id.clone(), r))
        .collect::<BTreeMap<_, _>>();
    for terminal in [false, true] {
        let ids = match source.job_ids(terminal) {
            Ok(ids) => ids,
            Err(Error::Object(reason)) => {
                owner
                    .ledger
                    .object(
                        &task.id,
                        &ObjectResult {
                            kind: "scheduled_jobs".into(),
                            id: if terminal {
                                "legacy-cli"
                            } else {
                                "legacy-desktop"
                            }
                            .into(),
                            outcome: Outcome::Failed,
                            target_id: None,
                            reason: Some(reason),
                            omitted: Vec::new(),
                        },
                    )
                    .await?;
                continue;
            }
            Err(error) => return Err(error),
        };
        for id in ids {
            crate::engine::check(owner, &task.id).await?;
            let source_id = format!("legacy-{}:{id}", if terminal { "cli" } else { "desktop" });
            task.current = Some(source_id.clone());
            crate::engine::publish(owner, task).await?;
            let result=async {
                let job=source.job(terminal,&id)?;let digest=fingerprint(&job)?;
                let key=format!("migration/schedule/{}",fingerprint(&source_id)?);
                if let Some(previous)=target.document(&key)? {
                    if previous["digest"]!=digest {return Ok(Outcome::Conflict);}
                    let document=target.settings("schedule",&owner.stopping).await?;
                    if document.as_object().is_some_and(|document|document.is_empty()) {return Ok(Outcome::Deleted);}
                    let jobs=document["jobs"].as_array().ok_or_else(||Error::Object("Invalid native schedule document is preserved".into()))?;
                    return Ok(if jobs.iter().any(|job|job["id"]==source_id) {Outcome::AlreadyImported}else{Outcome::Deleted});
                }
                let schedule=recurrence(&job)?;
                if job.prompt.len()>32768 || job.prompt.is_empty()||job.name.is_empty()||job.cwd.is_empty(){return Err(Error::Object("Legacy schedule fields are invalid".into()));}
                let mut native=json!({"id":source_id,"name":job.name,"schedule":schedule,"prompt":job.prompt,"cwd":job.cwd,"model":job.model,"mode":if job.session_mode=="new"{"create"}else{"reuse"},"max_run_minutes":60,"enabled":job.enabled,"policy_cap":{"preset":"read-only","allowedTools":[],"disallowedTools":[]},"created_by":"user","revision":1});
                if job.enabled {native["enabled_at"]=json!(stamp(Timestamp::now()));}
                if let Some(bound)=job.bound_session {
                    let imported=sessions.get(&bound).filter(|s|matches!(s.outcome,Outcome::Imported|Outcome::AlreadyImported)).and_then(|s|s.target_id.as_ref()).ok_or_else(||Error::Deferred("Bound Session is unavailable; job retained in source".into()))?;
                    native["session_id"]=json!(imported);
                }
                let mut document=target.settings("schedule",&owner.stopping).await?;
                if document.as_object().is_some_and(|d|d.is_empty()) {document=json!({"jobs":[],"max_concurrent_runs":4});}
                let jobs=document["jobs"].as_array_mut().ok_or_else(||Error::Object("Invalid native schedule document is preserved".into()))?;
                if jobs.iter().any(|job|job["id"]==source_id){return Ok(Outcome::Conflict);}
                jobs.push(native);
                let mut db=target.db.lock().unwrap();let tx=db.transaction().map_err(failure)?;
                Native::save_document(&tx,"user/schedule",&document)?;
                Native::save_document(&tx,&key,&json!({"digest":digest}))?;
                tx.commit().map_err(failure)?;
                Ok(Outcome::Imported)
            }.await;
            let (outcome, reason) = match result {
                Ok(outcome) => (outcome, None),
                Err(Error::Object(reason)) => (Outcome::Failed, Some(reason)),
                Err(Error::Unsupported(reason)) => (Outcome::Unsupported, Some(reason)),
                Err(Error::Deferred(reason)) => (Outcome::Deferred, Some(reason)),
                Err(e) => return Err(e),
            };
            owner
                .ledger
                .object(
                    &task.id,
                    &ObjectResult {
                        kind: "scheduled_job".into(),
                        id: source_id.clone(),
                        outcome,
                        target_id: Some(source_id),
                        reason,
                        omitted: Vec::new(),
                    },
                )
                .await?;
            task.processed += 1;
            crate::engine::publish(owner, task).await?;
        }
    }
    Ok(())
}

fn stamp(instant: Timestamp) -> String {
    instant.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn recurrence(job: &ScheduledJob) -> Result<Value> {
    let invalid =
        || Error::Unsupported("Legacy recurrence has no exact native calendar equivalent".into());
    let created: Zoned = dim_agent_capabilities::timestamp::parse_civil(&job.created_at)
        .map_err(|_| invalid())?
        .to_zoned(TimeZone::system());
    let mut fields = BTreeMap::new();
    let upper = job.rrule.trim().to_uppercase();
    let rule = upper.trim_prefix("RRULE:");
    for pair in rule.split(';') {
        let (key, value) = pair.split_once('=').ok_or_else(invalid)?;
        if ![
            "FREQ",
            "INTERVAL",
            "BYHOUR",
            "BYMINUTE",
            "BYDAY",
            "BYMONTH",
            "BYMONTHDAY",
            "COUNT",
        ]
        .contains(&key)
            || value.is_empty()
            || fields.insert(key, value).is_some()
        {
            return Err(invalid());
        }
    }
    let number = |key: &str, min: u32, max: u32, default: u32| -> Result<u32> {
        let n = match fields.get(key) {
            Some(s) if s.bytes().all(|b| b.is_ascii_digit()) => s.parse().map_err(|_| invalid())?,
            Some(_) => return Err(invalid()),
            None => default,
        };
        if n < min || n > max {
            return Err(invalid());
        }
        Ok(n)
    };
    let freq = *fields.get("FREQ").ok_or_else(invalid)?;
    let (required, rejected): (&[&str], &[&str]) = match freq {
        "MINUTELY" => (
            &[],
            &["BYHOUR", "BYMINUTE", "BYDAY", "BYMONTH", "BYMONTHDAY"],
        ),
        "HOURLY" => (&[], &["BYHOUR", "BYDAY", "BYMONTH", "BYMONTHDAY"]),
        "DAILY" => (&["BYHOUR", "BYMINUTE"], &["BYDAY", "BYMONTH", "BYMONTHDAY"]),
        "WEEKLY" => (&["BYDAY", "BYHOUR", "BYMINUTE"], &["BYMONTH", "BYMONTHDAY"]),
        "MONTHLY" => (&["BYMONTHDAY", "BYHOUR", "BYMINUTE"], &["BYDAY", "BYMONTH"]),
        "YEARLY" => (&["BYMONTH", "BYMONTHDAY", "BYHOUR", "BYMINUTE"], &["BYDAY"]),
        _ => return Err(invalid()),
    };
    if required.iter().any(|k| !fields.contains_key(k))
        || rejected.iter().any(|k| fields.contains_key(k))
    {
        return Err(invalid());
    }
    let interval = number("INTERVAL", 1, 999, 1)?;
    let hour = number("BYHOUR", 0, 23, created.hour() as u32)?;
    let minute = number(
        "BYMINUTE",
        0,
        59,
        if freq == "HOURLY" {
            0
        } else {
            created.minute() as u32
        },
    )?;
    let month = number("BYMONTH", 1, 12, created.month() as u32)?;
    let day = number("BYMONTHDAY", 1, 31, created.day() as u32)?;
    if freq == "YEARLY"
        && day > [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][month as usize - 1]
    {
        return Err(invalid());
    }
    let mut weekdays = Vec::new();
    if let Some(days) = fields.get("BYDAY") {
        for day in days.split(',') {
            let index = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"]
                .iter()
                .position(|d| *d == day)
                .ok_or_else(invalid)?;
            if weekdays.contains(&index) {
                return Err(invalid());
            }
            weekdays.push(index);
        }
        weekdays.sort();
    }
    if fields.contains_key("COUNT") {
        number("COUNT", 1, 1, 1)?;
        let occurrence = job
            .next_run_at
            .as_ref()
            .or(if !job.enabled {
                job.last_run_at.as_ref()
            } else {
                None
            })
            .ok_or_else(invalid)?;
        let at = dim_agent_capabilities::timestamp::parse(occurrence).map_err(|_| invalid())?;
        return Ok(json!({"at": stamp(at)}));
    }
    let slots = |first: u32, end: u32, step: u32| {
        (first..end)
            .step_by(step as usize)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    let cron = match freq {
        "MINUTELY" if 60 % interval == 0 => format!(
            "{} * * * *",
            slots(created.minute() as u32 % interval, 60, interval)
        ),
        "HOURLY" if 24 % interval == 0 => format!(
            "{minute} {} * * *",
            slots(created.hour() as u32 % interval, 24, interval)
        ),
        "DAILY" if interval == 1 => format!("{minute} {hour} * * *"),
        "WEEKLY" if interval == 1 => format!(
            "{minute} {hour} * * {}",
            weekdays
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(",")
        ),
        "MONTHLY" if 12 % interval == 0 => format!(
            "{minute} {hour} {day} {} *",
            slots((created.month() as u32 - 1) % interval + 1, 13, interval)
        ),
        "YEARLY" if interval == 1 => format!("{minute} {hour} {day} {month} *"),
        _ => return Err(invalid()),
    };
    Ok(json!({"cron":cron}))
}
