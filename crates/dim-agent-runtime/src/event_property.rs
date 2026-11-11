use crate::{AgentError, QueryValue, Result};
use dim_agent_types::{AgentEvent, TimelinePosition};
use serde::{Serialize, Serializer, ser};
use serde_json::Value;

/// Projects one public event property without serializing unrelated data.
/// The caller validates the path as part of the query contract.
pub fn event_property(event: &AgentEvent, branch_id: &str, path: &str) -> Result<QueryValue> {
    let value = match path {
        "kind" => QueryValue::Value {
            value: Value::String(event.kind.key().into()),
        },
        "id" => QueryValue::Value {
            value: Value::String(event.id.clone()),
        },
        "create_at" => QueryValue::Value {
            value: event.create_at.into(),
        },
        "tags" => match &event.tags {
            Some(tags) => project_property(tags, "")?,
            None => QueryValue::Missing,
        },
        "position" => project_property(
            &TimelinePosition {
                branch_id: branch_id.into(),
                sequence: event.sequence,
            },
            "",
        )?,
        "position.branch_id" => QueryValue::Value {
            value: branch_id.into(),
        },
        "position.sequence" => QueryValue::Value {
            value: event.sequence.into(),
        },
        "context.render" => QueryValue::Value {
            value: event.context_flags().0.into(),
        },
        "context.state" => QueryValue::Value {
            value: event.context_flags().1.into(),
        },
        "parent_call_id" => match &event.parent_call_id {
            Some(parent) => QueryValue::Value {
                value: Value::String(parent.clone()),
            },
            None => QueryValue::Missing,
        },
        _ if path.starts_with("tags[") => match &event.tags {
            Some(tags) => project_property(tags, &path[4..])?,
            None => QueryValue::Missing,
        },
        _ => project_property(&event.kind, path)?,
    };
    Ok(value)
}

/// Projects fields from a typed value, preserving absent fields and array positions.
/// An empty path selects the value itself; only selected values are serialized.
pub fn project_property(value: &impl Serialize, path: &str) -> Result<QueryValue> {
    select(value, path).map_err(|error| AgentError::fault("event property", error.to_string()))
}

/// Checks a validated event property path without serializing the property's value.
pub fn event_property_present(event: &AgentEvent, path: &str) -> Result<bool> {
    if path.starts_with("tags[") {
        return select_mode(&event.tags, &path[4..], Leaf::Presence)
            .map(|value| !matches!(value, QueryValue::Missing))
            .map_err(|error| AgentError::fault("event property", error.to_string()));
    }
    if path == "parent_call_id" {
        return Ok(event.parent_call_id.is_some());
    }
    if path == "data" || path.starts_with("data.") {
        return select_mode(&event.kind, path, Leaf::Presence)
            .map(|value| !matches!(value, QueryValue::Missing))
            .map_err(|error| AgentError::fault("event property", error.to_string()));
    }
    Ok(path != "tags" || event.tags.is_some())
}

pub(crate) fn array_length(event: &AgentEvent, path: &str) -> Result<Option<usize>> {
    if path == "tags" {
        return Ok(event.tags.as_ref().map(Vec::len));
    }
    let value = select_mode(&event.kind, path, Leaf::Length)
        .map_err(|error| AgentError::fault("event property", error.to_string()))?;
    Ok(match value {
        QueryValue::Value {
            value: Value::Number(length),
        } => Some(length.as_u64().expect("array lengths are integers") as usize),
        _ => None,
    })
}

pub(crate) fn elements(
    event: &AgentEvent,
    field: &str,
    path: &str,
    value: bool,
) -> Result<Vec<QueryValue>> {
    let suffix = if path.is_empty() {
        "[]".to_owned()
    } else {
        format!("[].{path}")
    };
    let leaf = if value { Leaf::Value } else { Leaf::Presence };
    let selected = if field == "tags" {
        select_mode(&event.tags, &suffix, leaf)
    } else {
        select_mode(&event.kind, &format!("{field}{suffix}"), leaf)
    }
    .map_err(|error| AgentError::fault("event property", error.to_string()))?;
    match selected {
        QueryValue::Elements { values } => Ok(values),
        _ => Err(AgentError::fault(
            "event property",
            "an array projection lost its array",
        )),
    }
}

#[derive(Clone, Copy)]
enum Leaf {
    Value,
    Presence,
    Length,
}

fn select<T: Serialize + ?Sized>(value: &T, path: &str) -> serde_json::Result<QueryValue> {
    select_mode(value, path, Leaf::Value)
}

fn select_mode<T: Serialize + ?Sized>(
    value: &T,
    path: &str,
    leaf: Leaf,
) -> serde_json::Result<QueryValue> {
    if path.is_empty() {
        match leaf {
            Leaf::Value => {
                return Ok(QueryValue::Value {
                    value: serde_json::to_value(value)?,
                });
            }
            Leaf::Presence => {
                return Ok(QueryValue::Value {
                    value: Value::Bool(true),
                });
            }
            Leaf::Length => {}
        }
    }
    value.serialize(PropertySerializer { path, leaf })
}

struct PropertySerializer<'a> {
    path: &'a str,
    leaf: Leaf,
}

struct PropertyMap<'a> {
    name: &'a str,
    rest: &'a str,
    pending: bool,
    value: QueryValue,
    leaf: Leaf,
    whole: bool,
}

struct PropertyArray<'a> {
    rest: Option<&'a str>,
    index: Option<usize>,
    offset: usize,
    values: Vec<QueryValue>,
    length: Option<usize>,
    leaf: Leaf,
    whole: bool,
}

impl<'a> PropertySerializer<'a> {
    fn map(self) -> PropertyMap<'a> {
        let end = self.path.find(['.', '[']).unwrap_or(self.path.len());
        PropertyMap {
            name: &self.path[..end],
            rest: self.path[end..]
                .strip_prefix('.')
                .unwrap_or(&self.path[end..]),
            pending: false,
            value: QueryValue::Missing,
            leaf: self.leaf,
            whole: false,
        }
    }

    fn array(self) -> PropertyArray<'a> {
        let subscript = self
            .path
            .strip_prefix('[')
            .and_then(|path| path.split_once(']'));
        PropertyArray {
            rest: subscript.map(|(_, rest)| rest.strip_prefix('.').unwrap_or(rest)),
            index: subscript.and_then(|(index, _)| index.parse().ok()),
            offset: 0,
            values: Vec::new(),
            length: (self.path.is_empty() && matches!(self.leaf, Leaf::Length)).then_some(0),
            leaf: self.leaf,
            whole: false,
        }
    }
}

macro_rules! missing_scalar {
    ($($method:ident($ty:ty)),* $(,)?) => {
        $(fn $method(self, _: $ty) -> serde_json::Result<QueryValue> {
            Ok(QueryValue::Missing)
        })*
    };
}

impl<'a> Serializer for PropertySerializer<'a> {
    type Ok = QueryValue;
    type Error = serde_json::Error;
    type SerializeSeq = PropertyArray<'a>;
    type SerializeTuple = PropertyArray<'a>;
    type SerializeTupleStruct = PropertyArray<'a>;
    type SerializeTupleVariant = PropertyArray<'a>;
    type SerializeMap = PropertyMap<'a>;
    type SerializeStruct = PropertyMap<'a>;
    type SerializeStructVariant = PropertyMap<'a>;

    missing_scalar! {
        serialize_bool(bool), serialize_i8(i8), serialize_i16(i16), serialize_i32(i32),
        serialize_i64(i64), serialize_i128(i128), serialize_u8(u8), serialize_u16(u16),
        serialize_u32(u32), serialize_u64(u64), serialize_u128(u128), serialize_f32(f32),
        serialize_f64(f64), serialize_char(char), serialize_str(&str), serialize_bytes(&[u8]),
    }

    fn serialize_none(self) -> serde_json::Result<QueryValue> {
        Ok(QueryValue::Missing)
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> serde_json::Result<QueryValue> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> serde_json::Result<QueryValue> {
        Ok(QueryValue::Missing)
    }

    fn serialize_unit_struct(self, _: &'static str) -> serde_json::Result<QueryValue> {
        Ok(QueryValue::Missing)
    }

    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
    ) -> serde_json::Result<QueryValue> {
        Ok(QueryValue::Missing)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> serde_json::Result<QueryValue> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> serde_json::Result<QueryValue> {
        let mut map = self.map();
        map.field(variant, value)?;
        Ok(map.value)
    }

    fn serialize_seq(self, _: Option<usize>) -> serde_json::Result<Self::SerializeSeq> {
        Ok(self.array())
    }

    fn serialize_tuple(self, _: usize) -> serde_json::Result<Self::SerializeTuple> {
        Ok(self.array())
    }

    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> serde_json::Result<Self::SerializeTupleStruct> {
        Ok(self.array())
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> serde_json::Result<Self::SerializeTupleVariant> {
        let map = self.map();
        let mut array = PropertySerializer {
            path: if map.name == variant { map.rest } else { "" },
            leaf: map.leaf,
        }
        .array();
        array.whole = map.name == variant && map.rest.is_empty();
        if array.whole && matches!(map.leaf, Leaf::Value) {
            array.rest = Some("");
        }
        if map.name != variant {
            array.length = None;
        }
        Ok(array)
    }

    fn serialize_map(self, _: Option<usize>) -> serde_json::Result<Self::SerializeMap> {
        Ok(self.map())
    }

    fn serialize_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> serde_json::Result<Self::SerializeStruct> {
        Ok(self.map())
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> serde_json::Result<Self::SerializeStructVariant> {
        let map = self.map();
        let mut selected = PropertySerializer {
            path: if map.name == variant { map.rest } else { "" },
            leaf: map.leaf,
        }
        .map();
        selected.whole = map.name == variant && map.rest.is_empty();
        if selected.whole {
            selected.value = match map.leaf {
                Leaf::Value => QueryValue::Value {
                    value: Value::Object(serde_json::Map::new()),
                },
                Leaf::Presence => QueryValue::Value {
                    value: Value::Bool(true),
                },
                Leaf::Length => QueryValue::Missing,
            };
        }
        Ok(selected)
    }
}

impl PropertyMap<'_> {
    fn field<T: Serialize + ?Sized>(&mut self, key: &str, value: &T) -> serde_json::Result<()> {
        if self.whole {
            if let QueryValue::Value {
                value: Value::Object(fields),
            } = &mut self.value
            {
                fields.insert(key.into(), serde_json::to_value(value)?);
            }
        } else if !self.name.is_empty() && key == self.name {
            self.value = select_mode(value, self.rest, self.leaf)?;
        }
        Ok(())
    }
}

impl ser::SerializeMap for PropertyMap<'_> {
    type Ok = QueryValue;
    type Error = serde_json::Error;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> serde_json::Result<()> {
        self.pending =
            !self.name.is_empty() && serde_json::to_value(key)?.as_str() == Some(self.name);
        Ok(())
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> serde_json::Result<()> {
        if self.pending {
            self.value = select_mode(value, self.rest, self.leaf)?;
        }
        self.pending = false;
        Ok(())
    }

    fn end(self) -> serde_json::Result<QueryValue> {
        Ok(self.value)
    }
}

macro_rules! map_serializer {
    ($trait:ident) => {
        impl ser::$trait for PropertyMap<'_> {
            type Ok = QueryValue;
            type Error = serde_json::Error;

            fn serialize_field<T: Serialize + ?Sized>(
                &mut self,
                key: &'static str,
                value: &T,
            ) -> serde_json::Result<()> {
                self.field(key, value)
            }

            fn end(self) -> serde_json::Result<QueryValue> {
                Ok(self.value)
            }
        }
    };
}

map_serializer!(SerializeStruct);
map_serializer!(SerializeStructVariant);

macro_rules! array_serializer {
    ($trait:ident, $method:ident) => {
        impl ser::$trait for PropertyArray<'_> {
            type Ok = QueryValue;
            type Error = serde_json::Error;

            fn $method<T: Serialize + ?Sized>(&mut self, value: &T) -> serde_json::Result<()> {
                if let Some(rest) = self.rest {
                    if self.index.is_none_or(|index| index == self.offset) {
                        self.values.push(select_mode(value, rest, self.leaf)?);
                    }
                    self.offset += 1;
                } else if let Some(length) = &mut self.length {
                    *length += 1;
                }
                Ok(())
            }

            fn end(mut self) -> serde_json::Result<QueryValue> {
                if self.index.is_some() {
                    return Ok(self.values.pop().unwrap_or(QueryValue::Missing));
                }
                if self.whole {
                    match self.leaf {
                        Leaf::Presence => {
                            return Ok(QueryValue::Value {
                                value: Value::Bool(true),
                            });
                        }
                        Leaf::Value => {
                            return Ok(QueryValue::Value {
                                value: Value::Array(
                                    self.values
                                        .into_iter()
                                        .map(|value| {
                                            let QueryValue::Value { value } = value else {
                                                unreachable!("whole tuple fields are JSON values")
                                            };
                                            value
                                        })
                                        .collect(),
                                ),
                            });
                        }
                        Leaf::Length => (),
                    }
                }
                if let Some(length) = self.length {
                    return Ok(QueryValue::Value {
                        value: length.into(),
                    });
                }
                Ok(match self.rest {
                    Some(_) => QueryValue::Elements {
                        values: self.values,
                    },
                    None => QueryValue::Missing,
                })
            }
        }
    };
}

array_serializer!(SerializeSeq, serialize_element);
array_serializer!(SerializeTuple, serialize_element);
array_serializer!(SerializeTupleStruct, serialize_field);
array_serializer!(SerializeTupleVariant, serialize_field);

#[cfg(test)]
mod tests {
    use super::*;
    use dim_agent_types::{AgentEvent, EventKind, ToolResultData};

    fn event(parent: Option<&str>) -> AgentEvent {
        AgentEvent {
            id: "event".into(),
            sequence: 1,
            create_at: 0,
            tags: None,
            parent_call_id: parent.map(str::to_owned),
            kind: EventKind::ToolResult(ToolResultData {
                turn_id: "turn".into(),
                request_id: "request".into(),
                call_event_id: None,
                results: Vec::new(),
            }),
        }
    }

    #[test]
    fn a_child_result_is_not_context_and_projects_its_parent() {
        let child = event(Some("c1"));
        assert_eq!(child.context_flags(), (false, false));
        assert!(matches!(
            event_property(&child, "main", "parent_call_id").unwrap(),
            QueryValue::Value {
                value: Value::String(parent)
            } if parent == "c1"
        ));
        assert!(event_property_present(&child, "parent_call_id").unwrap());

        let top = event(None);
        assert_eq!(top.context_flags(), (true, true));
        assert!(matches!(
            event_property(&top, "main", "parent_call_id").unwrap(),
            QueryValue::Missing
        ));
        assert!(!event_property_present(&top, "parent_call_id").unwrap());
    }
}
