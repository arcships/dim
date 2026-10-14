//! The `/usage` report (port of `formatUsageReport`).

use serde_json::Value;

/// `Context  ~12.3K / 200.0K` plus one bar per bucket, then the Dim plan
/// when the session runs on it.
pub fn format_report(context: &Value, plan: Option<&Result<Value, String>>, now_ms: u64) -> String {
    let number = |key: &str| context[key].as_f64().unwrap_or(0.0);
    let total_window = number("modelContextWindowTokens");
    let total = number("totalTokens");
    let free = (total_window - total).max(0.0);
    let mut lines = vec![format!(
        "Context  ~{} / {}",
        format_tokens(total),
        format_tokens(total_window)
    )];
    for (label, used) in [
        ("Messages", number("messageTokens")),
        ("Tools", number("toolTokens")),
        ("System", number("systemTokens")),
        ("Free", free),
    ] {
        let row = usage_row(label, used, total_window);
        lines.push(row.line());
        if label != "Free" {
            lines.push(format!("  {}", usage_bar(row.percentage, 42)));
        }
    }
    match plan {
        None => {}
        Some(Err(error)) => {
            lines.push(String::new());
            lines.push("Dim plan".to_owned());
            lines.push(format!("Unavailable: {error}"));
        }
        Some(Ok(summary)) => {
            lines.push(String::new());
            lines.push("Dim plan".to_owned());
            let rows = plan_rows(summary);
            let unlimited = summary["unlimited"].as_bool() == Some(true);
            if unlimited {
                lines.push("Credits  unlimited".to_owned());
            }
            if rows.is_empty() {
                if !unlimited {
                    lines.push("No usage data".to_owned());
                }
            } else {
                for (row, resets_at) in rows {
                    lines.push(row.line());
                    let reset = resets_at
                        .as_deref()
                        .and_then(|iso| format_reset(iso, now_ms))
                        .map(|r| format!("  resets in {r}"))
                        .unwrap_or_default();
                    lines.push(format!("  {}{reset}", usage_bar(row.percentage, 42)));
                }
            }
        }
    }
    lines.join("\n")
}

struct Row {
    label: String,
    value: String,
    percentage: f64,
}

impl Row {
    fn line(&self) -> String {
        let label: String = if self.label.chars().count() >= 12 {
            self.label.chars().take(12).collect()
        } else {
            format!("{:<12}", self.label)
        };
        let value = if self.value.chars().count() >= 17 {
            self.value.clone()
        } else {
            format!("{:>17}", self.value)
        };
        format!("{label} {value} {}", format_percentage(self.percentage))
    }
}

fn usage_row(label: &str, used: f64, total: f64) -> Row {
    let percentage = if total <= 0.0 {
        0.0
    } else {
        (used / total * 100.0).clamp(0.0, 100.0)
    };
    Row {
        label: label.to_owned(),
        value: format!("{} / {}", format_tokens(used), format_tokens(total)),
        percentage,
    }
}

fn plan_rows(summary: &Value) -> Vec<(Row, Option<String>)> {
    let mut rows = Vec::new();
    let mut windows: Vec<&Value> = summary["windowStates"]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    windows.sort_by(|a, b| {
        a["durationHours"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&b["durationHours"].as_f64().unwrap_or(0.0))
    });
    for window in windows {
        let hours = window["durationHours"].as_f64().unwrap_or(0.0);
        rows.push((
            usage_row(
                &window_label(hours),
                window["tokenUsed"].as_f64().unwrap_or(0.0),
                window["tokenCap"].as_f64().unwrap_or(0.0),
            ),
            window["expiresAt"].as_str().map(str::to_owned),
        ));
    }
    if summary["unlimited"].as_bool() != Some(true)
        && let Some(total) = summary["totalUnits"].as_f64()
    {
        let used = summary["usedUnits"].as_f64().unwrap_or_else(|| {
            let remaining = summary["remainingUnits"].as_f64().unwrap_or(total);
            (total - remaining).max(0.0)
        });
        rows.push((
            usage_row("Credits", used, total),
            summary["subscriptionExpiresAt"].as_str().map(str::to_owned),
        ));
    }
    rows
}

fn window_label(hours: f64) -> String {
    let whole = hours.fract() == 0.0;
    if hours >= 168.0 && whole && (hours as u64).is_multiple_of(168) {
        format!("{}w window", hours as u64 / 168)
    } else if hours >= 24.0 && whole && (hours as u64).is_multiple_of(24) {
        format!("{}d window", hours as u64 / 24)
    } else if whole {
        format!("{}h window", hours as u64)
    } else {
        format!("{hours:.1}h window")
    }
}

pub fn format_tokens(value: f64) -> String {
    if value >= 1_000_000.0 {
        format!("{:.1}M", (value / 100_000.0).floor() / 10.0)
    } else if value >= 1_000.0 {
        format!("{:.1}K", (value / 100.0).floor() / 10.0)
    } else {
        format!("{}", value as u64)
    }
}

fn format_percentage(value: f64) -> String {
    if value > 0.0 && value < 0.1 {
        "<0.1%".to_owned()
    } else {
        format!("{value:.1}%")
    }
}

pub fn usage_bar(percentage: f64, width: usize) -> String {
    let bar = width.clamp(8, 42);
    let normalized = if percentage.is_finite() {
        percentage.clamp(0.0, 100.0)
    } else {
        0.0
    };
    let proportional = (normalized / 100.0 * bar as f64).round() as usize;
    let filled = if normalized > 0.0 {
        proportional.max(1)
    } else {
        0
    };
    format!("{}{}", "█".repeat(filled), "░".repeat(bar - filled))
}

/// `3d 4h`, `2h 5m`, `7m` until `iso` (RFC 3339), `None` when unparsable.
fn format_reset(iso: &str, now_ms: u64) -> Option<String> {
    let target = parse_rfc3339_ms(iso)?;
    if target <= now_ms {
        return Some("0m".to_owned());
    }
    let minutes = (target - now_ms) / 60_000;
    let (days, hours, mins) = (minutes / 1440, (minutes % 1440) / 60, minutes % 60);
    Some(if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    })
}

/// `YYYY-MM-DDTHH:MM:SS(.fff)?(Z|±HH:MM)` as milliseconds since the epoch.
fn parse_rfc3339_ms(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let num = |from: usize, len: usize| -> Option<i64> { text.get(from..from + len)?.parse().ok() };
    let (year, month, day) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (hour, minute, second) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    let mut rest = &text[19..];
    let mut millis = 0i64;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits: String = fraction.chars().take_while(char::is_ascii_digit).collect();
        let padded = format!("{digits:0<3}");
        millis = padded[..3].parse().ok()?;
        rest = &fraction[digits.len()..];
    }
    let offset_minutes = match rest {
        "Z" | "z" => 0,
        other if other.len() == 6 => {
            let sign = if other.starts_with('-') { -1 } else { 1 };
            let h: i64 = other.get(1..3)?.parse().ok()?;
            let m: i64 = other.get(4..6)?.parse().ok()?;
            sign * (h * 60 + m)
        }
        _ => return None,
    };
    // Days from civil (Howard Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_minutes * 60;
    u64::try_from(seconds * 1000 + millis).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn context_buckets_and_bars_follow_the_report_layout() {
        let context = json!({
            "totalTokens": 12345.0, "modelContextWindowTokens": 200000.0,
            "messageTokens": 10000.0, "toolTokens": 2000.0, "systemTokens": 345.0
        });
        let text = format_report(&context, None, 0);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Context  ~12.3K / 200.0K");
        assert_eq!(lines[1], "Messages        10.0K / 200.0K 5.0%");
        assert_eq!(lines[2], format!("  {}{}", "█".repeat(2), "░".repeat(40)));
        assert!(lines.iter().any(|l| l.starts_with("Free ")));
    }

    #[test]
    fn plan_windows_are_sorted_and_reset_times_are_relative() {
        let summary = json!({
            "windowStates": [
                {"durationHours": 168.0, "tokenUsed": 50.0, "tokenCap": 100.0,
                 "expiresAt": "1970-01-02T00:00:00Z"},
                {"durationHours": 5.0, "tokenUsed": 1.0, "tokenCap": 100.0}
            ]
        });
        let text = format_report(
            &json!({"totalTokens": 0.0, "modelContextWindowTokens": 10.0}),
            Some(&Ok(summary)),
            3_600_000,
        );
        let at = text.find("Dim plan").unwrap();
        let plan: Vec<&str> = text[at..].lines().collect();
        assert!(plan[1].starts_with("5h window"), "{plan:?}");
        assert!(plan[3].starts_with("1w window"));
        assert!(plan[4].ends_with("resets in 23h 0m"), "{plan:?}");
    }

    #[test]
    fn an_error_is_reported_under_the_plan_heading() {
        let text = format_report(
            &json!({"totalTokens": 0.0, "modelContextWindowTokens": 10.0}),
            Some(&Err("not signed in".into())),
            0,
        );
        assert!(text.ends_with("Dim plan\nUnavailable: not signed in"));
    }
}
