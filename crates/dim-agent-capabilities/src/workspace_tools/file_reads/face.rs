use super::{FileReads, TextFileBinding};
use crate::port_faces::{request, unknown_method};
use rutis_bridge::session::{Error, HostDispatch, Reply, Value, arg};
use serde::Deserialize;
use std::{ffi::OsString, path::Path};

const NAME: &str = "fileReads";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingPath {
    #[serde(deserialize_with = "crate::file_storage::deserialize_name")]
    path: OsString,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    #[serde(deserialize_with = "crate::file_storage::deserialize_name")]
    path: OsString,
    binding: TextFileBinding,
    established: u64,
}

impl HostDispatch for FileReads {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({"binding":"sync","writeBinding":"async","record":"sync"}))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        match method {
            "binding" => {
                let BindingPath { path } = request(NAME, args)?;
                arg(&self.binding(Path::new(&path)))
            }
            "writeBinding" => {
                let BindingPath { path } = request(NAME, args)?;
                let read = self.write_binding(Path::new(&path));
                Ok(Value::cancellable_future(async move {
                    arg(&read
                        .await
                        .map_err(|error| Error::value(error.to_string()))?)
                }))
            }
            "record" => {
                let Record {
                    path,
                    binding,
                    established,
                } = request(NAME, args)?;
                self.record(path.into(), binding, established);
                Ok(Value::Undefined)
            }
            _ => Err(unknown_method(NAME, method)),
        }
    }
}
