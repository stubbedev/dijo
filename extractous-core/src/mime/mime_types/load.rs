//! The generator of the tables: `MimeTypesReader` over `tika-mimetypes.xml` and
//! `custom-mimetypes.xml`, building what `MimeTypes` holds as Tika builds it, then written out as
//! the Rust source of `tables.rs`. `just mime-tables` regenerates that file, and a test holds it
//! to the XML.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write;

use super::{Magic, MimeType, MimeTypes, Patterns, TypeId};
use crate::hash::FastMap;
use crate::java::{self, JavaString};
use crate::mime::java_regex::translate::{Anchor, Text};
use crate::mime::java_regex::JavaRegex;
use crate::mime::magic::parse::{MatchAttributes, Read};
use crate::mime::magic::{Clause, Kind, MagicMatch};
use crate::mime::media_type::MediaType;
use crate::mime::registry::Registry;
use crate::xml::{self, Attribute, Event, Flow};

const TIKA_MIMETYPES: &str = include_str!("../../../models/tika-mimetypes.xml");
const CUSTOM_MIMETYPES: &str =
    include_str!("../../../tika-native/src/main/resources/custom-mimetypes.xml");
const TABLES: &str = "src/mime/mime_types/tables.rs";

/// A type as the reader builds it.
struct Building {
    media_type: MediaType,
    interpreted: bool,
    root_xml: Vec<(String, String)>,
    extensions: Vec<String>,
}

/// A magic as the reader builds it, with its `toString()`, which orders magics of equal
/// priority, size and type.
struct ReadMagic {
    magic: Magic,
    string: String,
}

/// `MimeTypes` as the reader builds it.
struct Builder {
    registry: Registry,
    types: Vec<Building>,
    by_type: FastMap<MediaType, TypeId>,
    patterns: PatternsBuilder,
}

/// The state of `MimeTypesReader` between elements.
struct Reading {
    current: Option<TypeId>,
    priority: i32,
    records: Vec<Record>,
    magics: Vec<ReadMagic>,
    error: Option<String>,
}

/// `MimeTypesReader.ClauseRecord`: an open `<match>` and the clauses nested in it.
struct Record {
    clause: Option<Pending>,
    subclauses: Vec<Read>,
}

enum Pending {
    Match(Read),
    MinShouldMatch(usize),
}

fn attribute<'a>(attributes: &'a [Attribute], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|a| a.qualified_name == name)
        .map(|a| a.value.as_str())
}

fn leak(text: &str) -> &'static str {
    text.to_owned().leak()
}

impl Builder {
    fn new() -> Builder {
        let mut types = Builder {
            registry: Registry::empty(),
            types: Vec::new(),
            by_type: FastMap::default(),
            patterns: PatternsBuilder::new(),
        };
        types.add(MediaType::named("application/octet-stream"));
        types.add(MediaType::named("text/plain"));
        types.add(MediaType::named("application/xml"));
        types
    }

    fn add(&mut self, media_type: MediaType) -> TypeId {
        let id = self.types.len();
        self.registry.add_type(&media_type);
        self.by_type.insert(media_type.clone(), id);
        self.types.push(Building {
            media_type,
            interpreted: false,
            root_xml: Vec::new(),
            extensions: Vec::new(),
        });
        id
    }

    /// `MimeTypes.forName`: the type a name normalizes to, created if it is new.
    fn for_name(&mut self, name: &str) -> Result<TypeId, String> {
        let media_type =
            MediaType::parse(name).ok_or_else(|| format!("invalid media type name {name}"))?;
        let normalized = self.registry.normalize(&media_type);
        if let Some(&id) = self.by_type.get(&normalized) {
            return Ok(id);
        }
        let id = self.add(media_type);
        self.by_type.entry(normalized).or_insert(id);
        Ok(id)
    }

    /// `MimeTypeReader.read` of each document, in order.
    fn load(documents: &[&str]) -> Result<MimeTypes, String> {
        let mut types = Builder::new();
        let mut reading = Reading {
            current: None,
            priority: 50,
            records: Vec::new(),
            magics: Vec::new(),
            error: None,
        };
        for document in documents {
            let result = xml::parse(document.as_bytes(), &mut |event| {
                let outcome = match event {
                    Event::Start(name, attributes) => {
                        types.start(&mut reading, name.qualified_name(), attributes)
                    }
                    Event::End(name) => types.end(&mut reading, name.qualified_name()),
                    Event::Text(..)
                    | Event::DocumentType
                    | Event::Comment(_)
                    | Event::ProcessingInstruction(..) => Ok(()),
                };
                match outcome {
                    Ok(()) => Flow::Continue,
                    Err(error) => {
                        reading.error = Some(error);
                        Flow::Stop
                    }
                }
            });
            if let Some(error) = reading.error.take() {
                return Err(error);
            }
            result.map_err(|error| format!("{error:?}"))?;
        }
        let text_html = types.for_name("text/html")?;
        Ok(types.finish(reading.magics, text_html))
    }

    /// The tables, the magics in `Magic.compareTo` order and the XML types in theirs.
    fn finish(self, mut magics: Vec<ReadMagic>, text_html: TypeId) -> MimeTypes {
        magics.sort_by(|a, b| self.compare_magics(a, b));
        let mut xmls: Vec<TypeId> = (0..self.types.len())
            .filter(|&id| !self.types[id].root_xml.is_empty())
            .collect();
        xmls.sort_by(|&a, &b| self.types[a].media_type.cmp(&self.types[b].media_type));
        let types: Vec<MimeType> = self
            .types
            .iter()
            .map(|building| MimeType {
                name: leak(building.media_type.as_str()),
                interpreted: building.interpreted,
                root_xml: building
                    .root_xml
                    .iter()
                    .map(|(ns, local)| (leak(ns), leak(local)))
                    .collect::<Vec<_>>()
                    .leak(),
                extensions: building
                    .extensions
                    .iter()
                    .map(|extension| leak(extension))
                    .collect::<Vec<_>>()
                    .leak(),
            })
            .collect();
        MimeTypes {
            registry: self.registry,
            types: types.leak(),
            by_type: sorted(
                self.by_type
                    .iter()
                    .map(|(media_type, &id)| (leak(media_type.as_str()), id)),
            ),
            magics: magics
                .into_iter()
                .map(|read| read.magic)
                .collect::<Vec<_>>()
                .leak(),
            xmls: xmls.leak(),
            patterns: self.patterns.finish(),
            octet_stream: 0,
            text_plain: 1,
            text_html,
        }
    }

    /// `Magic.compareTo`: higher priority first, then the longer pattern, then by type and
    /// printed clause, both descending.
    fn compare_magics(&self, a: &ReadMagic, b: &ReadMagic) -> Ordering {
        b.magic
            .priority
            .cmp(&a.magic.priority)
            .then_with(|| b.magic.clause.size().cmp(&a.magic.clause.size()))
            .then_with(|| {
                self.types[b.magic.type_id]
                    .media_type
                    .cmp(&self.types[a.magic.type_id].media_type)
            })
            .then_with(|| java::compare(&b.string, &a.string))
    }

    fn start(
        &mut self,
        reading: &mut Reading,
        element: &str,
        attributes: &[Attribute],
    ) -> Result<(), String> {
        let Some(current) = reading.current else {
            if element == "mime-type" {
                let name = attribute(attributes, "type").ok_or("a mime-type without a type")?;
                let id = self.for_name(name)?;
                self.types[id].interpreted = attribute(attributes, "interpreted") == Some("true");
                reading.current = Some(id);
            }
            return Ok(());
        };
        let media_type = self.types[current].media_type.clone();
        match element {
            "alias" => {
                let alias = attribute(attributes, "type").ok_or("an alias without a type")?;
                let alias =
                    MediaType::parse(alias).ok_or_else(|| format!("invalid alias {alias}"))?;
                self.registry.add_alias(&media_type, &alias);
            }
            "sub-class-of" => {
                let parent =
                    attribute(attributes, "type").ok_or("a sub-class-of without a type")?;
                let parent = MediaType::parse(parent)
                    .ok_or_else(|| format!("invalid supertype {parent}"))?;
                self.registry.add_supertype(&media_type, &parent);
            }
            "glob" => {
                if let Some(pattern) = attribute(attributes, "pattern") {
                    let regex = attribute(attributes, "isregex") == Some("true");
                    let extension =
                        self.patterns
                            .add(pattern, regex, current, &self.types, &self.registry)?;
                    if let Some(extension) = extension {
                        let extensions = &mut self.types[current].extensions;
                        if !extensions.iter().any(|known| known == extension) {
                            extensions.push(extension.to_owned());
                        }
                    }
                }
            }
            "root-XML" => {
                let namespace = attribute(attributes, "namespaceURI").unwrap_or("");
                let local = attribute(attributes, "localName").unwrap_or("");
                if namespace.is_empty() && local.is_empty() {
                    return Err(format!("a root-XML of {media_type} names nothing"));
                }
                self.types[current]
                    .root_xml
                    .push((namespace.to_owned(), local.to_owned()));
            }
            "match" => {
                let clause = match attribute(attributes, "minShouldMatch") {
                    Some(min) => Pending::MinShouldMatch(
                        min.parse()
                            .map_err(|_| format!("invalid minShouldMatch {min}"))?,
                    ),
                    None => Pending::Match(MagicMatch::parse(
                        &media_type,
                        &MatchAttributes {
                            kind: attribute(attributes, "type"),
                            offset: attribute(attributes, "offset"),
                            value: attribute(attributes, "value"),
                            mask: attribute(attributes, "mask"),
                        },
                    )?),
                };
                reading.records.push(Record {
                    clause: Some(clause),
                    subclauses: Vec::new(),
                });
            }
            "magic" => {
                reading.priority = match attribute(attributes, "priority") {
                    Some(value) if !value.is_empty() => value
                        .parse()
                        .map_err(|_| format!("invalid priority {value}"))?,
                    _ => 50,
                };
                reading.records = vec![Record {
                    clause: None,
                    subclauses: Vec::new(),
                }];
            }
            _ => {}
        }
        Ok(())
    }

    fn end(&mut self, reading: &mut Reading, element: &str) -> Result<(), String> {
        let Some(current) = reading.current else {
            return Ok(());
        };
        match element {
            "mime-type" => reading.current = None,
            "match" => {
                let record = reading.records.pop().ok_or("a match outside a magic")?;
                let clause = match record.clause {
                    Some(Pending::MinShouldMatch(min)) => {
                        if record.subclauses.is_empty() || min == 0 || min > record.subclauses.len()
                        {
                            return Err(format!(
                                "minShouldMatch {min} of {} clauses",
                                record.subclauses.len()
                            ));
                        }
                        Read::min_should_match(min, record.subclauses)
                    }
                    Some(Pending::Match(magic)) => {
                        let mut subclauses = record.subclauses;
                        match subclauses.len() {
                            0 => magic,
                            1 => Read::and(vec![magic, subclauses.remove(0)]),
                            _ => Read::and(vec![magic, Read::or(subclauses)]),
                        }
                    }
                    None => return Err("a match closes its magic".to_owned()),
                };
                reading
                    .records
                    .last_mut()
                    .ok_or("a match outside a magic")?
                    .subclauses
                    .push(clause);
            }
            "magic" => {
                let record = reading.records.pop().ok_or("a magic closes twice")?;
                if record.subclauses.is_empty() {
                    return Err(format!(
                        "a magic of {} has no match",
                        self.types[current].media_type
                    ));
                }
                for read in record.subclauses {
                    reading.magics.push(ReadMagic {
                        string: format!("[{}/{}]", reading.priority, read.string),
                        magic: Magic {
                            type_id: current,
                            priority: reading.priority,
                            clause: read.clause,
                        },
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Pairs sorted by their keys.
fn sorted<V>(pairs: impl Iterator<Item = (&'static str, V)>) -> &'static [(&'static str, V)] {
    let mut pairs: Vec<_> = pairs.collect();
    pairs.sort_by(|a, b| a.0.cmp(b.0));
    pairs.leak()
}

/// A glob's place in `Patterns`: longer patterns first, then in Java's string order.
#[derive(Debug, PartialEq, Eq)]
struct GlobKey(JavaString);

impl Ord for GlobKey {
    fn cmp(&self, other: &Self) -> Ordering {
        java::utf16_length(&other.0 .0)
            .cmp(&java::utf16_length(&self.0 .0))
            .then_with(|| self.0.cmp(&other.0))
    }
}

impl PartialOrd for GlobKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Tika's `Patterns` as the reader builds it.
struct PatternsBuilder {
    names: FastMap<String, TypeId>,
    extensions: FastMap<String, TypeId>,
    globs: BTreeMap<GlobKey, (TypeId, JavaRegex)>,
    min_extension: usize,
    max_extension: usize,
}

/// Where a pattern goes when it is new or narrows the type it had, as `Patterns` decides: kept
/// when the new type is the same or more general, a conflict otherwise.
fn replaces(
    previous: Option<TypeId>,
    id: TypeId,
    types: &[Building],
    registry: &Registry,
    pattern: &str,
) -> Result<bool, String> {
    let Some(previous) = previous else {
        return Ok(true);
    };
    let (old, new) = (&types[previous].media_type, &types[id].media_type);
    if registry.is_specialization_of(old, new) {
        Ok(true)
    } else if previous == id || registry.is_specialization_of(new, old) {
        Ok(false)
    } else {
        Err(format!("the pattern {pattern} names both {old} and {new}"))
    }
}

impl PatternsBuilder {
    fn new() -> PatternsBuilder {
        PatternsBuilder {
            names: FastMap::default(),
            extensions: FastMap::default(),
            globs: BTreeMap::new(),
            min_extension: usize::MAX,
            max_extension: 0,
        }
    }

    /// `Patterns.add`: the extension of an extension pattern, which the type takes too.
    fn add<'p>(
        &mut self,
        pattern: &'p str,
        regex: bool,
        id: TypeId,
        types: &[Building],
        registry: &Registry,
    ) -> Result<Option<&'p str>, String> {
        if regex {
            self.add_glob(pattern.to_owned(), id, types, registry)?;
            return Ok(None);
        }
        let wild = |c: char| matches!(c, '*' | '?' | '[');
        if !pattern.contains(wild) {
            if replaces(
                self.names.get(pattern).copied(),
                id,
                types,
                registry,
                pattern,
            )? {
                self.names.insert(pattern.to_owned(), id);
            }
        } else if pattern.starts_with('*') && !pattern[1..].contains(wild) {
            let extension = &pattern[1..];
            if replaces(
                self.extensions.get(extension).copied(),
                id,
                types,
                registry,
                pattern,
            )? {
                self.extensions.insert(extension.to_owned(), id);
                let length = java::utf16_length(extension);
                self.min_extension = self.min_extension.min(length);
                self.max_extension = self.max_extension.max(length);
            }
            return Ok(Some(extension));
        } else {
            self.add_glob(compile_glob(pattern), id, types, registry)?;
        }
        Ok(None)
    }

    fn add_glob(
        &mut self,
        glob: String,
        id: TypeId,
        types: &[Building],
        registry: &Registry,
    ) -> Result<(), String> {
        let key = GlobKey(JavaString(glob));
        let previous = self.globs.get(&key).map(|(previous, _)| *previous);
        if replaces(previous, id, types, registry, &key.0 .0)? {
            let regex = JavaRegex::compile(&key.0 .0, Anchor::Matches, Text::Unicode)?;
            self.globs.insert(key, (id, regex));
        }
        Ok(())
    }

    fn finish(self) -> Patterns {
        Patterns {
            names: sorted(self.names.iter().map(|(name, &id)| (leak(name), id))),
            extensions: sorted(
                self.extensions
                    .iter()
                    .map(|(extension, &id)| (leak(extension), id)),
            ),
            globs: self.globs.into_values().collect::<Vec<_>>().leak(),
            min_extension: self.min_extension,
            max_extension: self.max_extension,
        }
    }
}

/// `Patterns.compile`: a glob as a Java regex.
fn compile_glob(glob: &str) -> String {
    let mut pattern = String::from("\\A");
    for c in glob.chars() {
        match c {
            '?' => pattern.push('.'),
            '*' => pattern.push_str(".*"),
            c if "\\[]^.-$+(){}|".contains(c) => {
                pattern.push('\\');
                pattern.push(c);
            }
            c => pattern.push(c),
        }
    }
    pattern.push_str("\\z");
    pattern
}

/// The tables read from the XML.
fn load() -> MimeTypes {
    Builder::load(&[TIKA_MIMETYPES, CUSTOM_MIMETYPES])
        .unwrap_or_else(|error| panic!("The media type tables do not load: {error}"))
}

/// The Rust source of `tables.rs` for `types`.
fn emit(types: &MimeTypes) -> String {
    let mut out = Source::default();
    out.line("//! The media type tables of `tika-mimetypes.xml` and `custom-mimetypes.xml`, generated by");
    out.line("//! `just mime-tables`.");
    out.line("");
    out.line("use std::sync::OnceLock;");
    out.line("");
    out.line("use fancy_regex::Regex;");
    out.line("");
    out.line("use super::{Magic, MimeType, MimeTypes, Patterns};");
    out.line("use crate::mime::java_regex::JavaRegex;");
    out.line("use crate::mime::magic::{Clause, Kind, MagicMatch};");
    out.line("use crate::mime::registry::Registry;");
    out.line("");
    let mut regexes = 0;
    types.magics.iter().for_each(|magic| {
        magic.clause.for_each_regex(&mut |_| regexes += 1);
    });
    regexes += types.patterns.globs.len();
    out.line(&format!(
        "static REGEXES: [OnceLock<Regex>; {regexes}] = [const {{ OnceLock::new() }}; {regexes}];"
    ));
    out.line("");
    out.line("pub(super) static MIME_TYPES: MimeTypes = MimeTypes {");
    let [canonical, supertypes] = types.registry.pairs();
    out.line("    registry: Registry::new(");
    for pairs in [canonical, supertypes] {
        out.line("        &[");
        out.pairs("            ", pairs, |value| format!("{value:?}"));
        out.line("        ],");
    }
    out.line("    ),");
    out.line("    types: &[");
    for kind in types.types {
        out.line(&format!(
            "        MimeType {{ name: {:?}, interpreted: {}, root_xml: &{:?}, extensions: &{:?} }},",
            kind.name, kind.interpreted, kind.root_xml, kind.extensions
        ));
    }
    out.line("    ],");
    out.line("    by_type: &[");
    out.pairs("        ", types.by_type, |id| id.to_string());
    out.line("    ],");
    let mut regex = 0;
    out.line("    magics: &[");
    for magic in types.magics {
        let clause = clause(&magic.clause, &mut regex);
        out.line(&format!(
            "        Magic {{ type_id: {}, priority: {}, clause: {clause} }},",
            magic.type_id, magic.priority
        ));
    }
    out.line("    ],");
    out.line(&format!("    xmls: &{:?},", types.xmls));
    out.line("    patterns: Patterns {");
    out.line("        names: &[");
    out.pairs("            ", types.patterns.names, |id| id.to_string());
    out.line("        ],");
    out.line("        extensions: &[");
    out.pairs("            ", types.patterns.extensions, |id| {
        id.to_string()
    });
    out.line("        ],");
    out.line("        globs: &[");
    for (id, glob) in types.patterns.globs {
        out.line(&format!(
            "            ({id}, {}),",
            java_regex(glob, &mut regex)
        ));
    }
    out.line("        ],");
    out.line(&format!(
        "        min_extension: {},",
        types.patterns.min_extension
    ));
    out.line(&format!(
        "        max_extension: {},",
        types.patterns.max_extension
    ));
    out.line("    },");
    out.line(&format!("    octet_stream: {},", types.octet_stream));
    out.line(&format!("    text_plain: {},", types.text_plain));
    out.line(&format!("    text_html: {},", types.text_html));
    out.line("};");
    out.0
}

/// Rust source, line by line.
#[derive(Default)]
struct Source(String);

impl Source {
    fn line(&mut self, line: &str) {
        self.0.push_str(line);
        self.0.push('\n');
    }

    /// One `(key, value),` line per pair, inside a slice the caller opens and closes.
    fn pairs<V>(&mut self, indent: &str, pairs: &[(&str, V)], value: impl Fn(&V) -> String) {
        for (key, item) in pairs {
            self.line(&format!("{indent}({key:?}, {}),", value(item)));
        }
    }
}

/// A clause as a Rust expression, its regexes numbered from `regex` on.
fn clause(clause: &Clause, regex: &mut usize) -> String {
    let list = |clauses: &[Clause], regex: &mut usize| {
        let items: Vec<String> = clauses.iter().map(|c| self::clause(c, regex)).collect();
        format!("&[{}]", items.join(", "))
    };
    match clause {
        Clause::Match(m) => {
            let kind = match &m.kind {
                Kind::Bytes => "Kind::Bytes".to_owned(),
                Kind::IgnoreCase => "Kind::IgnoreCase".to_owned(),
                Kind::Regex(found) => format!("Kind::Regex({})", java_regex(found, regex)),
            };
            format!(
                "Clause::Match(MagicMatch {{ pattern: {}, mask: {}, kind: {kind}, begin: {}, end: {} }})",
                bytes(m.pattern),
                bytes(m.mask),
                m.begin,
                m.end
            )
        }
        Clause::And(clauses) => format!("Clause::And({})", list(clauses, regex)),
        Clause::Or(clauses) => format!("Clause::Or({})", list(clauses, regex)),
        Clause::MinShouldMatch(min, clauses) => {
            format!("Clause::MinShouldMatch({min}, {})", list(clauses, regex))
        }
    }
}

/// A regex as a Rust expression, compiled into the next of `REGEXES`.
fn java_regex(found: &JavaRegex, regex: &mut usize) -> String {
    let expression = format!(
        "JavaRegex::new({:?}, {:?}, &REGEXES[{regex}])",
        found.pattern(),
        found.first()
    );
    *regex += 1;
    expression
}

/// Bytes as a byte string literal.
fn bytes(bytes: &[u8]) -> String {
    let mut literal = String::from("b\"");
    for &byte in bytes {
        match byte {
            b'"' | b'\\' => {
                literal.push('\\');
                literal.push(char::from(byte));
            }
            0x20..=0x7E => literal.push(char::from(byte)),
            _ => write!(literal, "\\x{byte:02X}").expect("writing to a String"),
        }
    }
    literal.push('"');
    literal
}

#[test]
fn the_tables_are_generated_from_the_xml() {
    let loaded = load();
    if std::env::var_os("MIME_TABLES_BLESS").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(TABLES);
        std::fs::write(&path, emit(&loaded))
            .unwrap_or_else(|error| panic!("writing {}: {error}", path.display()));
        return;
    }
    assert!(
        loaded == *super::mime_types(),
        "{TABLES} is not what the XML generates: run `just mime-tables`"
    );
}

#[test]
fn every_printed_type_parses_to_itself() {
    let loaded = load();
    let [canonical, supertypes] = loaded.registry.pairs();
    let names = loaded
        .types
        .iter()
        .map(|kind| kind.name)
        .chain(
            canonical
                .iter()
                .chain(supertypes)
                .flat_map(|&(a, b)| [a, b]),
        )
        .chain(loaded.by_type.iter().map(|&(name, _)| name));
    for name in names {
        assert_eq!(MediaType::named(name).as_str(), name);
    }
}

#[test]
fn compiles_globs_as_tika_does() {
    assert_eq!(compile_glob("i_*.txt"), "\\Ai_.*\\.txt\\z");
}
