use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};

pub(crate) fn serialize<S: serde::Serializer>(
    name: &impl AsRef<OsStr>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    let name = name.as_ref();
    match name.to_str() {
        Some(text) => serializer.serialize_str(text),
        #[cfg(any(unix, windows))]
        None => name.serialize(serializer),
        #[cfg(not(any(unix, windows)))]
        None => Err(serde::ser::Error::custom(
            "unsupported filesystem name encoding",
        )),
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OsValue {
    Text(String),
    #[cfg(any(unix, windows))]
    Native(OsString),
}

pub(crate) fn deserialize<'de, T: From<OsString>, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<T, D::Error> {
    Ok(match OsValue::deserialize(deserializer)? {
        OsValue::Text(text) => text.into(),
        #[cfg(any(unix, windows))]
        OsValue::Native(name) => name,
    }
    .into())
}
