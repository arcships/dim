//! The UTC RFC 3339 form Dim persists and sends.
//!
//! chrono's serializer wrote 0, 3, 6 or 9 fractional digits. Jiff's default
//! serializer trims trailing zeros inside a group, so boundaries that already
//! carried chrono bytes use this module instead.
//!
//! Jiff cannot represent a leap second. [`parse`] normalizes it to the
//! following second, the instant chrono's millisecond conversion named;
//! [`parse_civil`] keeps jiff's clamped wall-clock fields for callers that
//! read calendar fields.

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Deserializer, Serializer};

pub fn serialize<S: Serializer>(instant: &Timestamp, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&format(*instant))
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Timestamp, D::Error> {
    let text = String::deserialize(deserializer)?;
    parse(&text).map_err(serde::de::Error::custom)
}

/// The UTC RFC 3339 form with 0, 3, 6 or 9 fractional digits.
pub fn format(instant: Timestamp) -> String {
    let nanos = instant.subsec_nanosecond();
    let format = if nanos == 0 {
        "%Y-%m-%dT%H:%M:%SZ"
    } else if nanos % 1_000_000 == 0 {
        "%Y-%m-%dT%H:%M:%S%.3fZ"
    } else if nanos % 1_000 == 0 {
        "%Y-%m-%dT%H:%M:%S%.6fZ"
    } else {
        "%Y-%m-%dT%H:%M:%S%.9fZ"
    };
    instant.strftime(format).to_string()
}

/// Parses an RFC 3339 instant, moving a leap second to the following second.
pub fn parse(text: &str) -> Result<Timestamp, String> {
    if let Some(adjusted) = leap_second_to_59(text) {
        let instant = adjusted
            .parse::<Timestamp>()
            .map_err(|error| error.to_string())?;
        return instant
            .checked_add(SignedDuration::from_secs(1))
            .map_err(|_| "timestamp is outside the supported range".to_owned());
    }
    text.parse::<Timestamp>().map_err(|error| error.to_string())
}

/// Parses an RFC 3339 instant with jiff's wall-clock handling of a leap second.
pub fn parse_civil(text: &str) -> Result<Timestamp, String> {
    text.parse::<Timestamp>().map_err(|error| error.to_string())
}

/// Whole milliseconds since the Unix epoch, rounding toward negative infinity.
pub fn millis(instant: Timestamp) -> i64 {
    i64::try_from(instant.as_nanosecond().div_euclid(1_000_000)).unwrap_or_else(|_| {
        if instant.as_nanosecond().is_negative() {
            i64::MIN
        } else {
            i64::MAX
        }
    })
}

/// The canonical millisecond form of the leap second that precedes `instant`.
pub fn leap_second_form(instant: Timestamp) -> Option<String> {
    let previous = instant.checked_sub(SignedDuration::from_secs(1)).ok()?;
    let text = previous.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let time_start = text.find('T')? + 1;
    let time = &text[time_start..];
    let first_colon = time.find(':')?;
    let second_colon = time[first_colon + 1..].find(':')? + first_colon + 1;
    let seconds = second_colon + 1;
    if text.get(time_start + seconds..time_start + seconds + 2)? != "59" {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..time_start + seconds]);
    out.push_str("60");
    out.push_str(&text[time_start + seconds + 2..]);
    Some(out)
}

fn leap_second_to_59(text: &str) -> Option<String> {
    let time_start = text.find(['T', 't', ' '])? + 1;
    let time_end = text[time_start..]
        .find(['Z', 'z', '+', '-'])
        .map_or(text.len(), |offset| time_start + offset);
    let mut parts = text[time_start..time_end].splitn(3, ':');
    let hour = parts.next()?;
    let minute = parts.next()?;
    let second = parts.next()?;
    let fraction = second.strip_prefix("60")?;
    if !fraction.is_empty() && !fraction.starts_with('.') {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..time_start]);
    out.push_str(hour);
    out.push(':');
    out.push_str(minute);
    out.push_str(":59");
    out.push_str(fraction);
    out.push_str(&text[time_end..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Wrapped(#[serde(with = "crate::timestamp")] Timestamp);

    #[test]
    fn writes_the_chrono_fraction_groups() {
        for (text, expected) in [
            ("2030-01-01T00:00:00Z", "2030-01-01T00:00:00Z"),
            ("2030-01-01T00:00:00.100Z", "2030-01-01T00:00:00.100Z"),
            ("2030-01-01T00:00:00.123400Z", "2030-01-01T00:00:00.123400Z"),
            (
                "2030-01-01T00:00:00.123456700Z",
                "2030-01-01T00:00:00.123456700Z",
            ),
        ] {
            let instant = parse(text).unwrap();
            assert_eq!(format(instant), expected);
            let wrapped = Wrapped(instant);
            let json = serde_json::to_string(&wrapped).unwrap();
            assert_eq!(json, format!(r#""{}""#, expected));
            assert_eq!(serde_json::from_str::<Wrapped>(&json).unwrap(), wrapped);
        }
    }

    #[test]
    fn normalizes_leap_seconds_to_the_following_second() {
        let instant = parse("2016-12-31T23:59:60Z").unwrap();
        assert_eq!(format(instant), "2017-01-01T00:00:00Z");
        assert_eq!(millis(instant), 1_483_228_800_000);
        assert_eq!(
            leap_second_form(instant).as_deref(),
            Some("2016-12-31T23:59:60.000Z")
        );
        assert_eq!(
            format(parse_civil("2016-12-31T23:59:60Z").unwrap()),
            "2016-12-31T23:59:59Z"
        );
    }

    #[test]
    fn floors_negative_milliseconds() {
        let instant = parse("1969-12-31T23:59:59.999999999Z").unwrap();
        assert_eq!(millis(instant), -1);
        let instant = parse("1969-12-31T23:59:59.000000001Z").unwrap();
        assert_eq!(millis(instant), -1000);
    }
}
