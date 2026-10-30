//! Typed wire contracts matching agent-v4 at commit 8565538d5ff70a63bbf10cb9ad9198107244bac0.

mod checkpoint;
mod content;
mod events;
pub mod isolation;
mod model;
mod permission;
mod profile;
mod prompt;
mod tool;
pub mod v1;

pub use checkpoint::*;
pub use content::*;
pub use events::*;
pub use isolation::*;
pub use model::*;
pub use permission::*;
pub use profile::*;
pub use prompt::*;
pub use tool::*;

/// Open JSON object fields at external extension boundaries.
pub type JsonObject = serde_json::Map<String, serde_json::Value>;
pub const JS_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Preserve absence while rejecting explicit null for optional typed fields.
fn optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// An absent or JavaScript-safe integer; explicit null is rejected.
mod optional_safe_integer {
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(value) => super::safe_integer::serialize(value, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<u64>, D::Error> {
        super::safe_integer::deserialize(deserializer).map(Some)
    }
}

mod safe_integer {
    use serde::{Deserializer, Serializer, de};

    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        if *value > crate::JS_MAX_SAFE_INTEGER {
            return Err(serde::ser::Error::custom(
                "integer exceeds JavaScript's safe range",
            ));
        }
        serializer.serialize_u64(*value)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        struct SafeInteger;
        impl<'de> de::Visitor<'de> for SafeInteger {
            type Value = u64;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a nonnegative JavaScript safe integer")
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<u64, E> {
                if value <= crate::JS_MAX_SAFE_INTEGER {
                    Ok(value)
                } else {
                    Err(E::custom("integer exceeds JavaScript's safe range"))
                }
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<u64, E> {
                let value = u64::try_from(value).map_err(E::custom)?;
                self.visit_u64(value)
            }

            fn visit_f64<E: de::Error>(self, value: f64) -> Result<u64, E> {
                if value.is_finite()
                    && value.fract() == 0.0
                    && (0.0..=crate::JS_MAX_SAFE_INTEGER as f64).contains(&value)
                {
                    Ok(value as u64)
                } else {
                    Err(E::custom(
                        "number is not a nonnegative JavaScript safe integer",
                    ))
                }
            }
        }
        deserializer.deserialize_any(SafeInteger)
    }
}

mod finite_number {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        if !value.is_finite() {
            return Err(serde::ser::Error::custom("JSON numbers must be finite"));
        }
        serializer.serialize_f64(*value)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        let value = f64::deserialize(deserializer)?;
        if !value.is_finite() {
            return Err(serde::de::Error::custom("JSON numbers must be finite"));
        }
        Ok(value)
    }

    pub fn serialize_optional<S: Serializer>(
        value: &Option<f64>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(value) if !value.is_finite() => {
                Err(serde::ser::Error::custom("JSON numbers must be finite"))
            }
            Some(value) => serializer.serialize_some(value),
            None => serializer.serialize_none(),
        }
    }
}

mod event_sequence {
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        if *value == 0 {
            return Err(serde::ser::Error::custom("event sequence starts at one"));
        }
        super::safe_integer::serialize(value, serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let value = super::safe_integer::deserialize(deserializer)?;
        if value == 0 {
            return Err(serde::de::Error::custom("event sequence starts at one"));
        }
        Ok(value)
    }
}
