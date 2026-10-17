//! JSON as Jackson's `ObjectMapper.readTree` builds it for GeoGebra's parser: objects keep their
//! members in the order they are read, and a repeated name replaces the value where the name
//! first stood. The document ends with its first value, whatever follows it.

use std::collections::HashMap;
use std::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// A JSON value, as far as Jackson's tree tells values apart.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Json {
    Text(String),
    Boolean(bool),
    Null,
    /// A number, whose text Jackson writes as its own number types do.
    Number,
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

/// Why a JSON document is left to Tika's path: serde_json does not read it, which Jackson may,
/// or a value Jackson writes as text the port does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Unread(pub(super) &'static str);

/// `readTree`: the first value of `text`.
pub(super) fn read(text: &[u8]) -> Result<Json, Unread> {
    let mut deserializer = serde_json::Deserializer::from_slice(text);
    Json::deserialize(&mut deserializer).map_err(|_| Unread("JSON serde_json does not read"))
}

impl Json {
    /// `path(name)` of an object: the value of the member `name`, if this is an object with one.
    pub(super) fn member(&self, name: &str) -> Option<&Json> {
        match self {
            Json::Object(members) => members
                .iter()
                .find(|(member, _)| member == name)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// The values iterating a node yields: an array's elements, an object's member values.
    pub(super) fn children(&self) -> Box<dyn Iterator<Item = &Json> + '_> {
        match self {
            Json::Array(elements) => Box::new(elements.iter()),
            Json::Object(members) => Box::new(members.iter().map(|(_, value)| value)),
            _ => Box::new(std::iter::empty()),
        }
    }

    /// `asText()`: a value's text, the empty text of a container.
    pub(super) fn as_text(&self) -> Result<String, Unread> {
        match self {
            Json::Text(text) => Ok(text.clone()),
            Json::Boolean(value) => Ok(value.to_string()),
            Json::Null => Ok("null".to_owned()),
            Json::Number => Err(Unread("a number Jackson writes as text")),
            Json::Array(_) | Json::Object(_) => Ok(String::new()),
        }
    }

    /// `findValuesAsText(name)`: the text of each member named `name`, in document order, not
    /// looking inside a member it takes.
    pub(super) fn find_values_as_text(
        &self,
        name: &str,
        found: &mut Vec<String>,
    ) -> Result<(), Unread> {
        match self {
            Json::Object(members) => {
                for (member, value) in members {
                    if member == name {
                        found.push(value.as_text()?);
                    } else {
                        value.find_values_as_text(name, found)?;
                    }
                }
            }
            Json::Array(elements) => {
                for element in elements {
                    element.find_values_as_text(name, found)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Json, E> {
        Ok(Json::Boolean(value))
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Json, E> {
        Ok(Json::Number)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Json, E> {
        Ok(Json::Number)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Json, E> {
        Ok(Json::Number)
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Json, E> {
        Ok(Json::Text(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Json, E> {
        Ok(Json::Text(value))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Json, A::Error> {
        let mut elements = Vec::new();
        while let Some(element) = sequence.next_element()? {
            elements.push(element);
        }
        Ok(Json::Array(elements))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut members: Vec<(String, Json)> = Vec::new();
        let mut positions: HashMap<String, usize> = HashMap::new();
        while let Some((name, value)) = map.next_entry::<String, Json>()? {
            match positions.get(&name) {
                Some(&position) => members[position].1 = value,
                None => {
                    positions.insert(name.clone(), members.len());
                    members.push((name, value));
                }
            }
        }
        Ok(Json::Object(members))
    }
}
