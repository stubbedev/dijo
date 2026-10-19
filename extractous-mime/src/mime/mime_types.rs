//! Tika's `MimeTypes`: the media types of `tika-mimetypes.xml` and extractous's
//! `custom-mimetypes.xml`, with their magic, names and XML roots, and the detection from a
//! document's first 64 KiB and its resource name. The tables are generated from the XML into
//! static data, so a process reads none of it.

use super::java_regex::JavaRegex;
use super::magic::{Clause, Haystack};
use super::media_type::MediaType;
use super::registry::Registry;
use super::sorted_get;
use super::text;
use super::uri;
use crate::java;
use crate::xml::{self, Event, Flow, XmlError};

#[cfg(test)]
mod load;
#[rustfmt::skip]
mod tables;

/// `MimeTypes.getMinLength()`: how much of a document the magic sees.
pub(crate) const MIN_LENGTH: usize = 64 * 1024;

/// A type `MimeTypes` knows, by its index.
type TypeId = usize;

#[derive(Debug, PartialEq)]
struct MimeType {
    /// The type's printed form.
    name: &'static str,
    interpreted: bool,
    /// The namespaces and local names of its XML root elements.
    root_xml: &'static [(&'static str, &'static str)],
    /// The extensions of its extension patterns, with their dots, in the order they were read.
    extensions: &'static [&'static str],
}

impl MimeType {
    /// `MimeType.matchesXML`: an empty namespace or name only matches an empty one.
    fn matches_xml(&self, namespace: &str, local: &str) -> bool {
        self.root_xml
            .iter()
            .any(|&(ns, name)| ns == namespace && name == local)
    }
}

/// A `<magic>` clause of a type, at its priority.
#[derive(Debug, PartialEq)]
struct Magic {
    type_id: TypeId,
    priority: i32,
    clause: Clause,
}

/// The default types.
pub(crate) fn mime_types() -> &'static MimeTypes {
    &tables::MIME_TYPES
}

#[derive(Debug, PartialEq)]
pub(crate) struct MimeTypes {
    registry: Registry,
    types: &'static [MimeType],
    /// Each type's printed form, and each name `forName` normalized to a type, with the type,
    /// sorted.
    by_type: &'static [(&'static str, TypeId)],
    /// In `Magic.compareTo` order.
    magics: &'static [Magic],
    /// The types with XML root elements, in their order.
    xmls: &'static [TypeId],
    patterns: Patterns,
    octet_stream: TypeId,
    text_plain: TypeId,
    text_html: TypeId,
}

impl MimeTypes {
    pub(crate) fn registry(&self) -> &Registry {
        &self.registry
    }

    fn type_of(&self, media_type: &MediaType) -> Option<TypeId> {
        sorted_get(self.by_type, media_type.as_str())
    }

    /// `forName(type).getExtension()`: the first extension of the type a name normalizes to,
    /// with its dot, or nothing for a type without one or one `forName` would make anew.
    pub(crate) fn extension(&self, media_type: &MediaType) -> &'static str {
        self.type_of(&self.registry.normalize(media_type))
            .and_then(|id| self.types[id].extensions.first())
            .copied()
            .unwrap_or("")
    }

    /// `getRegisteredMimeType(type).getExtension()`: the first extension of the registered type
    /// a name normalizes to, else of its base type where it has parameters, with its dot, or
    /// nothing for a type that is not registered.
    pub(crate) fn registered_extension(&self, media_type: &MediaType) -> &'static str {
        let normalized = self.registry.normalize(media_type);
        let id = self.type_of(&normalized).or_else(|| {
            normalized
                .has_parameters()
                .then(|| sorted_get(self.by_type, normalized.base_str()))
                .flatten()
        });
        id.and_then(|id| self.types[id].extensions.first())
            .copied()
            .unwrap_or("")
    }

    fn media_type(&self, id: TypeId) -> MediaType {
        MediaType::named(self.types[id].name)
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
        for magic in self.magics {
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
            let name = self.types[*slot].name;
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
            .map(|id| self.media_type(id))
            .collect();
        if let Some(hint) = resource_name.and_then(uri::name_hint) {
            let hinted = self.by_name(&hint.name);
            if !(hint.http && self.types[hinted].interpreted) {
                possible = self.apply_hint(possible, &self.media_type(hinted));
            }
        }
        if let Some(hint) = content_type.and_then(MediaType::parse) {
            let known = self.type_of(&self.registry.normalize(&hint));
            let hint = known.map_or(hint, |id| self.media_type(id));
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

/// Tika's `Patterns`: exact names, extensions and globs.
#[derive(Debug, PartialEq)]
struct Patterns {
    /// Sorted.
    names: &'static [(&'static str, TypeId)],
    /// Without their `*`, sorted.
    extensions: &'static [(&'static str, TypeId)],
    /// Longer patterns first, then in Java's string order.
    globs: &'static [(TypeId, JavaRegex)],
    /// The shortest and longest extension, in UTF-16 units.
    min_extension: usize,
    max_extension: usize,
}

impl Patterns {
    /// `Patterns.matches`.
    fn matches(&self, name: &str) -> Option<TypeId> {
        if let Some(id) = sorted_get(self.names, name) {
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
                if let Some(id) = sorted_get(self.extensions, suffix) {
                    return Some(id);
                }
            }
        }
        self.globs
            .iter()
            .find(|(_, regex)| regex.is_match(name))
            .map(|(id, _)| *id)
    }
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
    fn holds_the_tables() {
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
        for magic in types.magics {
            magic.clause.for_each_regex(&mut force);
        }
        for (_, regex) in types.patterns.globs {
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

    /// A `Content-Type` hint is the declared type of the document, kept whole with its
    /// parameters, such as the charset a text parser reads, where it is or specializes what the
    /// bytes show; one the bytes contradict is passed over. Tika's `forName` also adds a type
    /// it does not know to its registry, which changes no later detection; the port keeps no
    /// such state (#113).
    #[test]
    fn applies_a_content_type_hint_whole_where_the_bytes_allow_it() {
        let types = mime_types();
        let hinted = |data: &[u8], hint: &str| types.detect(data, None, Some(hint)).to_string();
        assert_eq!(
            hinted(b"just some words", "text/html; charset=ISO-8859-1"),
            "text/html; charset=ISO-8859-1"
        );
        assert_eq!(
            hinted(
                &[0, 1, 2, 3, 0xFF],
                "application/x-unheard-of; name=\"a.bin\""
            ),
            "application/x-unheard-of; name=a.bin"
        );
        assert_eq!(
            hinted(b"%PDF-1.7\n", "application/octet-stream; name=\"a.bin\""),
            "application/pdf"
        );
        assert_eq!(hinted(b"%PDF-1.7\n", "text/html"), "application/pdf");
        assert_eq!(
            detect(&[0, 1, 2, 3, 0xFF], None),
            "application/octet-stream"
        );
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
}
