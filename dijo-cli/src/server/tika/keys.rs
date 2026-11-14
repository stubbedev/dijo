//! The metadata keys of tika-server 4 and of tika-server 3 before it. dijo names metadata as
//! Tika 4 does, and derives a key's 3.x name as Tika 4's `LegacyKeyMigrationFilter` rewrites
//! 4.x metadata for a 3.x client: by its row in Tika's own 3.x-to-4.x table, else by the first
//! of the filter's prefix rules that applies.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Tika 4.1.0's `metadata-migration-3x-4x.json`: a 3.x key and its 4.x successor per row.
const TABLE: &str = include_str!("metadata-migration-3x-4x.json");
/// The 4.x key of a row whose 3.x key has no successor.
const DROPPED: &str = "DROPPED";

#[derive(Deserialize)]
struct Row<'t> {
    v3: &'t str,
    v4: &'t str,
}

/// The 3.x key of each 4.x key the table renames; of rows that share a 4.x key, the last, as
/// the filter keeps it.
static RENAMED: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    serde_json::from_str::<Vec<Row<'static>>>(TABLE)
        .expect("Tika's 3.x-to-4.x metadata key table is a JSON array of rows")
        .into_iter()
        .filter(|row| row.v4 != DROPPED)
        .map(|row| (row.v4, row.v3))
        .collect()
});

/// A prefix rule of the filter, for a family of keys the table does not list one by one.
enum Rule {
    /// The 4.x prefix is replaced by the 3.x one, the rest of the key kept.
    Swap(&'static str, &'static str),
    /// The 4.x prefix is removed: 3.x wrote the family's keys bare.
    Strip(&'static str),
}

/// The filter's prefix rules, in its order, without its rules for the digest and GeoParser
/// keys, which dijo does not write.
const RULES: &[Rule] = &[
    Rule::Swap("ner:", "NER_"),
    Rule::Swap("envi:", "envi."),
    Rule::Swap("ogg:streams-", "streams-"),
    Rule::Swap("iwork:", "iworks:"),
    Rule::Swap("dif:DIF-", "DIF-"),
    Rule::Swap("mdb-prop:", "MDB_PROP:"),
    Rule::Swap("mdb-user-prop:", "MDB_USER_PROP:"),
    Rule::Swap("mdb-summary-prop:", "MDB_SUMMARY_PROP:"),
    Rule::Swap("message:raw-header:", "Message:Raw-Header:"),
    Rule::Swap("rtf:pict:", "rtf_pict:"),
    Rule::Swap("icc:", "ICC:"),
    Rule::Swap("mbox:", "MboxParser-"),
    Rule::Strip("hdf:"),
    Rule::Strip("netcdf:"),
    Rule::Strip("gdal:"),
    Rule::Strip("isatab:"),
    Rule::Strip("mp4:"),
    Rule::Strip("network:"),
    Rule::Strip("audio:"),
    Rule::Strip("html:"),
    Rule::Strip("img:"),
    Rule::Swap("iso19115:keywords:", "Keywords "),
    Rule::Swap("iso19115:keywords-type:", "KeywordsType "),
    Rule::Swap("iso19115:thesaurus-name-title:", "ThesaurusNameTitle "),
    Rule::Swap(
        "iso19115:thesaurus-name-alternative-title:",
        "ThesaurusNameAlternativeTitle ",
    ),
];

impl Rule {
    fn apply<'k>(&self, key: &'k str) -> Option<Cow<'k, str>> {
        match *self {
            Rule::Swap(v4, v3) => key
                .strip_prefix(v4)
                .map(|rest| Cow::Owned(format!("{v3}{rest}"))),
            Rule::Strip(v4) => key.strip_prefix(v4).map(Cow::Borrowed),
        }
    }
}

/// The key a 3.x client reads the values of the 4.x `key` under; None for a key 3.x spelled
/// alike.
fn v3_key(key: &str) -> Option<Cow<'_, str>> {
    let renamed = match RENAMED.get(key) {
        Some(v3) => Cow::Borrowed(*v3),
        None => RULES.iter().find_map(|rule| rule.apply(key))?,
    };
    (renamed != key).then_some(renamed)
}

/// Whose metadata keys an answer is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Keys {
    /// tika-server 4's, for an endpoint only tika-server 4 serves.
    Tika4,
    /// tika-server 3's, for an endpoint only tika-server 3 serves.
    Tika3,
    /// Each value under its 4.x key and its 3.x key, for an endpoint both serve, so that a
    /// client of either finds the keys it reads.
    Both,
}

/// A document's object with its keys named for a client. Values that come to share a 3.x key
/// are merged under it, as the filter adds them; with [`Keys::Both`] a 3.x key never adds to a
/// key the object has itself.
pub struct Named<'o>(BTreeMap<Cow<'o, str>, Vec<&'o Value>>);

impl<'o> Named<'o> {
    pub fn new(object: &'o BTreeMap<String, Value>, keys: Keys) -> Self {
        let mut named: BTreeMap<Cow<'o, str>, Vec<&'o Value>> = BTreeMap::new();
        for (key, value) in object {
            let v3 = match keys {
                Keys::Tika4 => None,
                Keys::Tika3 => v3_key(key),
                Keys::Both => v3_key(key).filter(|v3| !object.contains_key(&**v3)),
            };
            if keys != Keys::Tika3 || v3.is_none() {
                named
                    .entry(Cow::Borrowed(key.as_str()))
                    .or_default()
                    .push(value);
            }
            if let Some(v3) = v3 {
                named.entry(v3).or_default().push(value);
            }
        }
        Self(named)
    }

    /// The values of `key`.
    pub fn get(&self, key: &str) -> Option<&[&'o Value]> {
        self.0.get(key).map(Vec::as_slice)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &[&'o Value])> {
        self.0
            .iter()
            .map(|(key, values)| (&**key, values.as_slice()))
    }
}

impl Serialize for Named<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, values) in self.iter() {
            map.serialize_entry(key, &Field(values))?;
        }
        map.end()
    }
}

/// The values of one key: one value as it is, several merged into one array of strings.
pub struct Field<'f, 'v>(pub &'f [&'v Value]);

impl Serialize for Field<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            [value] => value.serialize(serializer),
            values => serializer.collect_seq(strings(values)),
        }
    }
}

/// The strings of `values`, each value a string or an array of strings, as Tika's JSON
/// serializer writes a key's values.
pub fn strings<'f, 'v>(values: &'f [&'v Value]) -> impl Iterator<Item = &'v str> + 'f {
    values.iter().flat_map(|value| {
        match *value {
            Value::Array(values) => values.as_slice(),
            value => std::slice::from_ref(value),
        }
        .iter()
        .filter_map(Value::as_str)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn json(object: &BTreeMap<String, Value>, keys: Keys) -> Value {
        serde_json::to_value(Named::new(object, keys)).unwrap()
    }

    #[test]
    fn keys_are_named_as_tika_3_named_them() {
        assert_eq!(v3_key("tk:content").as_deref(), Some("X-TIKA:content"));
        assert_eq!(v3_key("tk:parsed-by").as_deref(), Some("X-TIKA:Parsed-By"));
        assert_eq!(
            v3_key("pdf:chars-per-page").as_deref(),
            Some("pdf:charsPerPage")
        );
        assert_eq!(
            v3_key("pdf:unmapped-unicode-chars-per-page").as_deref(),
            Some("pdf:unmappedUnicodeCharsPerPage")
        );
        assert_eq!(
            v3_key("pdf:ocr-page-count").as_deref(),
            Some("pdf:ocrPageCount")
        );
        assert_eq!(
            v3_key("access-permission:can-print").as_deref(),
            Some("access_permission:can_print")
        );
        assert_eq!(v3_key("tk:resource-name").as_deref(), Some("resourceName"));
        assert_eq!(
            v3_key("tk:exception:container-exception").as_deref(),
            Some("X-TIKA:EXCEPTION:container_exception")
        );
        assert_eq!(
            v3_key("message:raw-header:X-Mailer").as_deref(),
            Some("Message:Raw-Header:X-Mailer")
        );
        assert_eq!(v3_key("html:viewport").as_deref(), Some("viewport"));
        assert_eq!(v3_key("dc:title"), None);
        assert_eq!(v3_key("Content-Type"), None);
        assert_eq!(v3_key("pdf:encrypted"), None);
    }

    #[test]
    fn every_row_of_the_table_is_read() {
        let rows: Vec<Row<'_>> = serde_json::from_str(TABLE).unwrap();
        let renamed = rows.iter().filter(|row| row.v4 != DROPPED).count();
        assert!(renamed > 200, "{renamed}");
        assert!(RENAMED.len() <= renamed);
        assert!(!RENAMED.contains_key(DROPPED));
        assert_eq!(RENAMED["tk:content"], "X-TIKA:content");
    }

    #[test]
    fn tika_3_keys_replace_tika_4_keys() {
        let named = json(
            &object(&[
                ("tk:content", Value::from("text")),
                ("pdf:chars-per-page", Value::from(vec!["1", "2"])),
                ("dc:title", Value::from("t")),
            ]),
            Keys::Tika3,
        );
        assert_eq!(
            named,
            serde_json::json!({
                "X-TIKA:content": "text",
                "pdf:charsPerPage": ["1", "2"],
                "dc:title": "t",
            })
        );
    }

    #[test]
    fn both_keys_carry_the_same_values() {
        let named = json(
            &object(&[
                ("tk:content", Value::from("text")),
                ("pdf:chars-per-page", Value::from(vec!["1", "2"])),
            ]),
            Keys::Both,
        );
        assert_eq!(
            named,
            serde_json::json!({
                "tk:content": "text",
                "X-TIKA:content": "text",
                "pdf:chars-per-page": ["1", "2"],
                "pdf:charsPerPage": ["1", "2"],
            })
        );
        let only_4 = object(&[("tk:content", Value::from("text"))]);
        assert_eq!(
            json(&only_4, Keys::Tika4),
            serde_json::json!({ "tk:content": "text" })
        );
    }

    #[test]
    fn values_that_share_a_tika_3_key_are_merged() {
        let colliding = object(&[
            ("html:version", Value::from("1")),
            ("mp3:version", Value::from(vec!["2", "3"])),
            ("version", Value::from("native")),
        ]);
        assert_eq!(
            json(&colliding, Keys::Tika3),
            serde_json::json!({ "version": ["1", "2", "3", "native"] })
        );
        assert_eq!(
            json(&colliding, Keys::Both),
            serde_json::json!({
                "html:version": "1",
                "mp3:version": ["2", "3"],
                "version": "native",
            })
        );
        let named = Named::new(&colliding, Keys::Tika3);
        let merged: Vec<&str> = strings(named.get("version").unwrap()).collect();
        assert_eq!(merged, ["1", "2", "3", "native"]);
    }
}
