//! `/loop` forms and the words its replies use. The record, the rounds and the
//! cadences belong to [Scheduler](../../../docs/design/plugins/Scheduler.md#12-loop);
//! this module owns what the native client does with them.

/// The words after `/loop`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Usage, then the loops of the shown branch.
    Show,
    /// A loop whose prompt runs now: a fixed one when an interval is named,
    /// self-paced otherwise.
    Start {
        prompt: String,
        /// The interval in seconds; `None` is a self-paced loop.
        every_seconds: Option<u64>,
    },
    /// Stop one loop; absent, the branch's only live one.
    Stop(Option<String>),
}

/// An interval token: a number and one of `s`, `m`, `h`, `d`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval(pub u64);

impl Interval {
    /// Reads `30s`, `5m`, `2h` or `1d`; anything else is `None`, so the token
    /// stays part of the prompt.
    pub fn parse(token: &str) -> Option<Self> {
        let (digits, scale) = [("s", 1), ("m", 60), ("h", 3_600), ("d", 86_400)]
            .into_iter()
            .find_map(|(unit, scale)| token.strip_suffix(unit).map(|digits| (digits, scale)))?;
        let value: u64 = digits.parse().ok()?;
        let seconds = value.checked_mul(scale)?;
        (seconds > 0).then_some(Self(seconds))
    }
}

/// Reads the words after `/loop` (design §12.6). The interval counts only as
/// the first token, and only when it parses; otherwise the whole line is the
/// prompt. `None` is a usage error.
pub fn request(words: &str) -> Option<Request> {
    let words = words.trim();
    if words.is_empty() {
        return Some(Request::Show);
    }
    if words == "stop" {
        return Some(Request::Stop(None));
    }
    if let Some(rest) = words
        .strip_prefix("stop")
        .filter(|rest| rest.starts_with(char::is_whitespace))
    {
        let rest = rest.trim();
        if rest.is_empty() {
            return Some(Request::Stop(None));
        }
        if !rest.contains(char::is_whitespace) {
            return Some(Request::Stop(Some(rest.to_owned())));
        }
        return None;
    }
    if let Some((first, rest)) = words.split_once(char::is_whitespace) {
        let rest = rest.trim();
        if let Some(interval) = Interval::parse(first)
            && !rest.is_empty()
        {
            return Some(Request::Start {
                prompt: rest.to_owned(),
                every_seconds: Some(interval.0),
            });
        }
    }
    Some(Request::Start {
        prompt: words.to_owned(),
        every_seconds: None,
    })
}

/// `every 5 minutes`, or `self-paced`, as a reply spells a loop's cadence.
pub fn cadence_text(every_seconds: Option<u64>) -> String {
    match every_seconds {
        None => "self-paced".into(),
        Some(seconds) if seconds % 86_400 == 0 => {
            format!("every {} day(s)", seconds / 86_400)
        }
        Some(seconds) if seconds % 3_600 == 0 => {
            format!("every {} hour(s)", seconds / 3_600)
        }
        Some(seconds) => format!("every {} minute(s)", seconds / 60),
    }
}

/// The words `/loop` prints for its forms.
pub const USAGE: &str = "Usage: /loop [PROMPT | INTERVAL PROMPT | stop [ID]]";

/// What `/loop` says when this daemon installs no Scheduler loops.
pub const NOT_AVAILABLE: &str = "Loops are not available on this daemon.";

/// One loop as `session.loop.list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loop {
    pub loop_id: String,
    pub state: String,
    pub round: u64,
    pub next_at: Option<String>,
    pub prompt: String,
}

impl Loop {
    pub fn from_wire(value: &serde_json::Value) -> Option<Self> {
        Some(Self {
            loop_id: value.get("loopId")?.as_str()?.to_owned(),
            state: value.get("state")?.as_str()?.to_owned(),
            round: value.get("round")?.as_u64()?,
            next_at: value
                .get("nextAt")
                .and_then(|next| next.as_str())
                .map(str::to_owned),
            prompt: value
                .get("prompt")
                .and_then(|prompt| prompt.as_str())
                .unwrap_or_default()
                .chars()
                .take(120)
                .collect(),
        })
    }
}

/// The report a request prints: usage and the branch's loops for `Show`, and a
/// one-line result for a write.
pub fn report(branch: &str, loops: &[Loop]) -> Vec<String> {
    let mut lines = vec![USAGE.to_owned()];
    if loops.is_empty() {
        lines.push(format!("No loop on {branch}."));
        return lines;
    }
    lines.push(format!("Loops on {branch} ({}):", loops.len()));
    for record in loops {
        let next = record
            .next_at
            .as_deref()
            .map(|at| format!(" next {at}"))
            .unwrap_or_default();
        lines.push(format!(
            "{} {} round {}{} {}",
            record.loop_id,
            record.state,
            record.round,
            next,
            record.prompt.replace('\n', " ")
        ));
    }
    lines
}
#[cfg(test)]
mod tests {
    use super::*;

    /// The four forms `/loop` accepts (design §12.6).
    #[test]
    fn the_four_forms_read_as_the_design_states() {
        assert_eq!(request(""), Some(Request::Show));
        assert_eq!(request("   "), Some(Request::Show));

        assert_eq!(
            request("check the deploy"),
            Some(Request::Start {
                prompt: "check the deploy".into(),
                every_seconds: None,
            })
        );

        // An interval counts only as the first token, and only when it parses;
        // the token itself is dropped from the prompt.
        assert_eq!(
            request("5m check the deploy"),
            Some(Request::Start {
                prompt: "check the deploy".into(),
                every_seconds: Some(300),
            })
        );
        assert_eq!(
            request("30s watch"),
            Some(Request::Start {
                prompt: "watch".into(),
                every_seconds: Some(30)
            })
        );
        assert_eq!(
            request("2h triage"),
            Some(Request::Start {
                prompt: "triage".into(),
                every_seconds: Some(7_200)
            })
        );
        assert_eq!(
            request("1d report"),
            Some(Request::Start {
                prompt: "report".into(),
                every_seconds: Some(86_400)
            })
        );

        assert_eq!(request("stop"), Some(Request::Stop(None)));
        assert_eq!(request("stop 7f2"), Some(Request::Stop(Some("7f2".into()))));
        assert_eq!(request("stop two words"), None);
    }

    /// A token that is not an interval stays part of the prompt.
    #[test]
    fn a_token_that_is_not_an_interval_is_prompt_text() {
        for words in ["check 5m later", "5x watch", "0m nothing", "5m", "stops"] {
            assert_eq!(
                request(words),
                Some(Request::Start {
                    prompt: words.into(),
                    every_seconds: None,
                }),
                "{words} stays a prompt with no interval"
            );
        }
    }

    /// A first token ending in a multibyte character has no ASCII unit, so it
    /// is prompt text like any other; reading the unit from the last byte
    /// would split inside that character.
    #[test]
    fn a_multibyte_first_token_is_prompt_text() {
        for words in ["检查 部署", "🙂 check", "5分 check"] {
            assert_eq!(
                request(words),
                Some(Request::Start {
                    prompt: words.into(),
                    every_seconds: None,
                }),
                "{words} stays a prompt with no interval"
            );
        }
        assert_eq!(
            request("5m 检查"),
            Some(Request::Start {
                prompt: "检查".into(),
                every_seconds: Some(300),
            })
        );
    }

    #[test]
    fn intervals_parse_in_the_four_units() {
        assert_eq!(Interval::parse("30s"), Some(Interval(30)));
        assert_eq!(Interval::parse("5m"), Some(Interval(300)));
        assert_eq!(Interval::parse("2h"), Some(Interval(7_200)));
        assert_eq!(Interval::parse("1d"), Some(Interval(86_400)));
        assert_eq!(Interval::parse("0s"), None);
        assert_eq!(Interval::parse("5x"), None);
        assert_eq!(Interval::parse("m"), None);
        assert_eq!(Interval::parse(""), None);
    }

    /// The report prefixes usage, then the branch's loops or that it has none.
    #[test]
    fn the_report_lists_the_branch_loops() {
        assert_eq!(
            report("main", &[]),
            vec![USAGE.to_owned(), "No loop on main.".to_owned()]
        );
        let record = Loop {
            loop_id: "7f2".into(),
            state: "waiting".into(),
            round: 3,
            next_at: None,
            prompt: "check the deploy".into(),
        };
        let lines = report("main", &[record]);
        assert_eq!(lines[0], USAGE);
        assert!(lines[1].starts_with("Loops on main (1):"));
        assert!(lines[2].contains("7f2 waiting round 3"));
    }
}
