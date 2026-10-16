//! Tika's `MimeTypes`: the media types of `tika-mimetypes.xml` and extractous's
//! `custom-mimetypes.xml`, with their magic, names and XML roots, and the detection from a
//! document's first 64 KiB and its resource name.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use super::java_regex::{Anchor, JavaRegex, Text};
use super::magic::{Clause, Haystack, MagicMatch, MatchAttributes};
use super::media_type::MediaType;
use super::registry::Registry;
use super::text;
use super::uri;
use crate::hash::FastMap;
use crate::java::{self, JavaString};
use crate::xml::{self, Attribute, Event, Flow, XmlError};

const TIKA_MIMETYPES: &str = include_str!("../../models/tika-mimetypes.xml");
const CUSTOM_MIMETYPES: &str =
    include_str!("../../tika-native/src/main/resources/custom-mimetypes.xml");

/// `MimeTypes.getMinLength()`: how much of a document the magic sees.
pub(crate) const MIN_LENGTH: usize = 64 * 1024;

/// A type `MimeTypes` knows, by its index.
type TypeId = usize;

struct MimeType {
    media_type: MediaType,
    interpreted: bool,
    root_xml: Vec<(String, String)>,
    /// The extensions of its extension patterns, with their dots, in the order they were read.
    extensions: Vec<String>,
}

impl MimeType {
    /// `MimeType.matchesXML`: an empty namespace or name only matches an empty one.
    fn matches_xml(&self, namespace: &str, local: &str) -> bool {
        self.root_xml
            .iter()
            .any(|(ns, name)| ns == namespace && name == local)
    }
}

/// A `<magic>` clause of a type, at its priority.
struct Magic {
    type_id: TypeId,
    priority: i32,
    clause: Clause,
    /// `Magic.toString()`.
    string: String,
}

/// The default types, loaded once.
pub(crate) fn mime_types() -> &'static MimeTypes {
    static TYPES: OnceLock<MimeTypes> = OnceLock::new();
    TYPES.get_or_init(|| {
        MimeTypes::load(&[TIKA_MIMETYPES, CUSTOM_MIMETYPES])
            .unwrap_or_else(|error| panic!("The media type tables do not load: {error}"))
    })
}

pub(crate) struct MimeTypes {
    registry: Registry,
    types: Vec<MimeType>,
    by_type: FastMap<MediaType, TypeId>,
    magics: Vec<Magic>,
    xmls: Vec<TypeId>,
    patterns: Patterns,
    octet_stream: TypeId,
    text_plain: TypeId,
    text_html: TypeId,
}

/// The state of `MimeTypesReader` between elements.
struct Reading {
    current: Option<TypeId>,
    priority: i32,
    records: Vec<Record>,
    magics: Vec<Magic>,
    error: Option<String>,
}

/// `MimeTypesReader.ClauseRecord`: an open `<match>` and the clauses nested in it.
struct Record {
    clause: Option<Pending>,
    subclauses: Vec<Clause>,
}

enum Pending {
    Match(Box<MagicMatch>),
    MinShouldMatch(usize),
}

fn attribute<'a>(attributes: &'a [Attribute], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|a| a.qualified_name == name)
        .map(|a| a.value.as_str())
}

impl MimeTypes {
    fn new() -> MimeTypes {
        let mut types = MimeTypes {
            registry: Registry::default(),
            types: Vec::new(),
            by_type: FastMap::default(),
            magics: Vec::new(),
            xmls: Vec::new(),
            patterns: Patterns::new(),
            octet_stream: 0,
            text_plain: 0,
            text_html: 0,
        };
        types.octet_stream = types.add(MediaType::named("application/octet-stream"));
        types.text_plain = types.add(MediaType::named("text/plain"));
        types.add(MediaType::named("application/xml"));
        types
    }

    fn add(&mut self, media_type: MediaType) -> TypeId {
        let id = self.types.len();
        self.registry.add_type(media_type.clone());
        self.by_type.insert(media_type.clone(), id);
        self.types.push(MimeType {
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

    fn load(documents: &[&str]) -> Result<MimeTypes, String> {
        let mut types = MimeTypes::new();
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
        let mut magics = reading.magics;
        magics.sort_by(|a, b| types.compare_magics(a, b));
        types.magics = magics;
        types.xmls = (0..types.types.len())
            .filter(|&id| !types.types[id].root_xml.is_empty())
            .collect();
        types
            .xmls
            .sort_by(|&a, &b| types.types[a].media_type.cmp(&types.types[b].media_type));
        types.text_html = types.for_name("text/html")?;
        Ok(types)
    }

    /// `Magic.compareTo`: higher priority first, then the longer pattern, then by type and
    /// printed clause, both descending.
    fn compare_magics(&self, a: &Magic, b: &Magic) -> Ordering {
        b.priority
            .cmp(&a.priority)
            .then_with(|| b.clause.size().cmp(&a.clause.size()))
            .then_with(|| {
                self.types[b.type_id]
                    .media_type
                    .cmp(&self.types[a.type_id].media_type)
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
                self.registry.add_alias(media_type, alias);
            }
            "sub-class-of" => {
                let parent =
                    attribute(attributes, "type").ok_or("a sub-class-of without a type")?;
                let parent = MediaType::parse(parent)
                    .ok_or_else(|| format!("invalid supertype {parent}"))?;
                self.registry.add_supertype(media_type, parent);
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
                    None => Pending::Match(Box::new(MagicMatch::parse(
                        &media_type,
                        &MatchAttributes {
                            kind: attribute(attributes, "type"),
                            offset: attribute(attributes, "offset"),
                            value: attribute(attributes, "value"),
                            mask: attribute(attributes, "mask"),
                        },
                    )?)),
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
                        Clause::MinShouldMatch(min, record.subclauses)
                    }
                    Some(Pending::Match(magic)) => {
                        let mut subclauses = record.subclauses;
                        let magic = Clause::Match(magic);
                        match subclauses.len() {
                            0 => magic,
                            1 => Clause::And(vec![magic, subclauses.remove(0)]),
                            _ => Clause::And(vec![magic, Clause::Or(subclauses)]),
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
                for clause in record.subclauses {
                    let string = format!("[{}/{clause}]", reading.priority);
                    reading.magics.push(Magic {
                        type_id: current,
                        priority: reading.priority,
                        clause,
                        string,
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn registry(&self) -> &Registry {
        &self.registry
    }

    /// `forName(type).getExtension()`: the first extension of the type a name normalizes to,
    /// with its dot, or nothing for a type without one or one `forName` would make anew.
    pub(crate) fn extension(&self, media_type: &MediaType) -> &str {
        self.by_type
            .get(&self.registry.normalize(media_type))
            .and_then(|&id| self.types[id].extensions.first())
            .map_or("", String::as_str)
    }

    /// `getRegisteredMimeType(type).getExtension()`: the first extension of the registered type
    /// a name normalizes to, else of its base type where it has parameters, with its dot, or
    /// nothing for a type that is not registered.
    pub(crate) fn registered_extension(&self, media_type: &MediaType) -> &str {
        let normalized = self.registry.normalize(media_type);
        let id = self.by_type.get(&normalized).or_else(|| {
            normalized
                .has_parameters()
                .then(|| self.by_type.get(&normalized.base_type()))
                .flatten()
        });
        id.and_then(|&id| self.types[id].extensions.first())
            .map_or("", String::as_str)
    }

    fn media_type(&self, id: TypeId) -> &MediaType {
        &self.types[id].media_type
    }

    /// `MimeTypes.getMimeType(byte[])`: the types the magic finds, highest priority first,
    /// generic XML or HTML narrowed by its root element, else plain text or nothing.
    fn by_magic(&self, data: &[u8]) -> Vec<TypeId> {
        if data.is_empty() {
            return vec![self.octet_stream];
        }
        let haystack = Haystack::new(data);
        let mut result = Vec::new();
        let mut current_priority = -1;
        for magic in &self.magics {
            if current_priority > 0 && current_priority > magic.priority {
                break;
            }
            if magic.clause.eval(&haystack) {
                result.push(magic.type_id);
                current_priority = magic.priority;
            }
        }
        if result.is_empty() {
            let detected = if text::is_text(data, MIN_LENGTH) {
                self.text_plain
            } else {
                self.octet_stream
            };
            return vec![detected];
        }
        for slot in &mut result {
            let name = self.media_type(*slot).as_str();
            if name != "application/xml" && name != "text/html" {
                continue;
            }
            match root_element(data) {
                Some((namespace, local)) => {
                    if let Some(&id) = self
                        .xmls
                        .iter()
                        .find(|&&id| self.types[id].matches_xml(&namespace, &local))
                    {
                        *slot = id;
                    }
                }
                None if name == "application/xml" => {
                    let html = self
                        .magics
                        .iter()
                        .filter(|m| m.type_id == self.text_html)
                        .any(|m| m.clause.eval(&haystack));
                    *slot = if html {
                        self.text_html
                    } else {
                        self.text_plain
                    };
                }
                None => {}
            }
        }
        result
    }

    /// `MimeTypes.getMimeType(String)`: by exact name, extension or glob, then the same in
    /// lower case.
    fn by_name(&self, name: &str) -> TypeId {
        self.patterns
            .matches(name)
            .or_else(|| self.patterns.matches(&java::to_lower_case(name)))
            .unwrap_or(self.octet_stream)
    }

    /// `MimeTypes.detect`: the magic of `prefix`, the first [`MIN_LENGTH`] bytes of a
    /// document, refined by the type its resource name implies, then by the `Content-Type` its
    /// metadata names.
    pub(crate) fn detect(
        &self,
        prefix: &[u8],
        resource_name: Option<&str>,
        content_type: Option<&str>,
    ) -> MediaType {
        let mut possible: Vec<MediaType> = self
            .by_magic(prefix)
            .into_iter()
            .map(|id| self.media_type(id).clone())
            .collect();
        if let Some(hint) = resource_name.and_then(uri::name_hint) {
            let hinted = self.by_name(&hint.name);
            if !(hint.http && self.types[hinted].interpreted) {
                possible = self.apply_hint(possible, self.media_type(hinted));
            }
        }
        if let Some(hint) = content_type.and_then(MediaType::parse) {
            let known = self.by_type.get(&self.registry.normalize(&hint));
            let hint = known.map_or(hint, |&id| self.media_type(id).clone());
            possible = self.apply_hint(possible, &hint);
        }
        possible.swap_remove(0)
    }

    /// `applyHint`: the hint alone when it is, or specializes, one of the possible types (a
    /// text-based message never specializing plain text), else the possible types.
    fn apply_hint(&self, possible: Vec<MediaType>, hint: &MediaType) -> Vec<MediaType> {
        let message = MediaType::named("text/x-tika-text-based-message");
        let specializes = |kind: &MediaType| {
            hint == kind
                || (self.registry.is_specialization_of(hint, kind)
                    && !(kind.as_str() == "text/plain"
                        && self.registry.is_specialization_of(hint, &message)))
        };
        if possible.iter().any(specializes) {
            return vec![hint.clone()];
        }
        possible
    }
}

/// `XmlRootExtractor.extractRootElement(byte[])`: the namespace and name of the root element,
/// retried on the first half of the bytes while the parse fails on a malformed character.
fn root_element(data: &[u8]) -> Option<(String, String)> {
    let mut data = data;
    loop {
        let mut root = None;
        let result = xml::parse(data, &mut |event| {
            if let Event::Start(name, _) = event {
                root = Some((name.namespace().to_owned(), name.local_name().to_owned()));
                return Flow::Stop;
            }
            Flow::Continue
        });
        match result {
            Err(XmlError::CharConversion) if root.is_none() => {
                let mut length = data.len() / 2;
                if length % 2 == 1 {
                    length -= 1;
                }
                if length == 0 {
                    return None;
                }
                data = &data[..length];
            }
            _ => return root,
        }
    }
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

/// Tika's `Patterns`: exact names, extensions and globs.
struct Patterns {
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
    types: &[MimeType],
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

impl Patterns {
    fn new() -> Patterns {
        Patterns {
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
        types: &[MimeType],
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
        types: &[MimeType],
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

    /// `Patterns.matches`.
    fn matches(&self, name: &str) -> Option<TypeId> {
        if let Some(&id) = self.names.get(name) {
            return Some(id);
        }
        if !self.extensions.is_empty() {
            let mut suffixes: Vec<(usize, &str)> = Vec::new();
            let mut units = 0;
            for (index, c) in name.char_indices().rev() {
                units += c.len_utf16();
                suffixes.push((units, &name[index..]));
            }
            let longest = self.max_extension.min(java::utf16_length(name));
            for (length, suffix) in suffixes.into_iter().rev() {
                if length > longest || length < self.min_extension {
                    continue;
                }
                if let Some(&id) = self.extensions.get(suffix) {
                    return Some(id);
                }
            }
        }
        self.globs
            .values()
            .find(|(_, regex)| regex.is_match(name))
            .map(|(id, _)| *id)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(data: &[u8], name: Option<&str>) -> String {
        mime_types().detect(data, name, None).to_string()
    }

    #[test]
    fn names_the_extension_of_a_type() {
        let types = mime_types();
        assert_eq!(
            types.extension(&MediaType::named("application/pdf")),
            ".pdf"
        );
        assert_eq!(
            types.extension(&MediaType::named("application/msword")),
            ".doc"
        );
        assert_eq!(types.extension(&MediaType::named("text/rtf")), ".rtf");
        assert_eq!(
            types.extension(&MediaType::named("application/x-unheard-of")),
            ""
        );
    }

    #[test]
    fn loads_the_tables() {
        let types = mime_types();
        assert!(types.types.len() > 1600, "{} types", types.types.len());
        assert!(types.magics.len() > 800, "{} magics", types.magics.len());
        assert!(types.xmls.len() > 30, "{} root types", types.xmls.len());
    }

    #[test]
    fn compiles_every_regex_of_the_tables() {
        let types = mime_types();
        let mut regexes = 0;
        let mut force = |regex: &JavaRegex| {
            regex.force();
            regexes += 1;
        };
        for magic in &types.magics {
            magic.clause.for_each_regex(&mut force);
        }
        for (_, regex) in types.patterns.globs.values() {
            force(regex);
        }
        assert!(regexes > 40, "{regexes} regexes");
    }

    #[test]
    fn detects_by_magic() {
        assert_eq!(detect(b"%PDF-1.7\n", None), "application/pdf");
        assert_eq!(detect(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR", None), "image/png");
        assert_eq!(detect(b"", None), "application/octet-stream");
        assert_eq!(detect(b"just some words", None), "text/plain");
        assert_eq!(
            detect(&[0u8, 1, 2, 3, 0xFF], None),
            "application/octet-stream"
        );
    }

    #[test]
    fn detects_custom_types() {
        assert_eq!(
            detect(b"-----BEGIN PGP MESSAGE-----\n", None),
            "application/pgp-encrypted"
        );
        assert_eq!(detect(b"[Script Info]\nTitle: x\n", None), "text/x-ssa");
    }

    #[test]
    fn narrows_xml_by_its_root() {
        assert_eq!(
            detect(
                b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>",
                None
            ),
            "image/svg+xml"
        );
        assert_eq!(
            detect(b"<?xml version=\"1.0\"?><unknown/>", None),
            "application/xml"
        );
        assert_eq!(
            detect(b"<?xml version=\"1.0\"?><a b=1>", None),
            "text/plain"
        );
    }

    #[test]
    fn refines_by_name() {
        assert_eq!(detect(b"", Some("report.pdf")), "application/pdf");
        assert_eq!(detect(b"words", Some("Main.java")), "text/x-java-source");
        assert_eq!(detect(b"words", Some("README")), "text/plain");
        assert_eq!(detect(b"%PDF-1.4", Some("x.docx")), "application/pdf");
        assert_eq!(detect(b"", Some("SCAN.PDF")), "application/pdf");
    }

    #[test]
    fn retries_the_root_on_halves_past_a_malformed_character() {
        let mut data =
            b"<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\">".to_vec();
        data.resize(200, b' ');
        data.extend(b"<!-- \xE9 -->");
        data.resize(400, b' ');
        assert_eq!(
            root_element(&data),
            Some(("http://www.w3.org/2000/svg".to_owned(), "svg".to_owned()))
        );
        assert_eq!(root_element(b"<!-- \xE9 --><svg/>"), None);
    }

    #[test]
    fn compiles_globs_as_tika_does() {
        assert_eq!(compile_glob("i_*.txt"), "\\Ai_.*\\.txt\\z");
    }
}
