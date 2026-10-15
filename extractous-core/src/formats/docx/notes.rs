//! The footnotes, endnotes and comments of a document, kept by id to be written where they are
//! referenced, as Tika's `OOXMLPartContentCollector` keeps them. Tika writes each note out as
//! XML and parses it again, which turns line ends in its text and whitespace in its attribute
//! values into what a parser makes of them; the kept events have the same done to them.

use std::collections::HashMap;

use super::W_NAMESPACE;
use crate::formats::xml::{attribute, Attribute, SaxHandler};
use crate::formats::Failure;
use crate::java::JavaHashMap;

/// The notes and comments of a document, and the relationships of the parts holding them.
#[derive(Debug, Default)]
pub(crate) struct InlineParts {
    pub(super) footnotes: HashMap<String, Vec<Recorded>>,
    pub(super) endnotes: HashMap<String, Vec<Recorded>>,
    /// The comments in the order of a `java.util.HashMap`, which is the order Tika writes the
    /// comments no paragraph refers to.
    pub(super) comments: JavaHashMap<Vec<Recorded>>,
    pub(super) relationships: HashMap<String, String>,
}

/// An event of a note, kept to be played to a handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Recorded {
    Start {
        namespace: String,
        local_name: String,
        attributes: Vec<Attribute>,
    },
    End {
        namespace: String,
        local_name: String,
    },
    Characters(String),
}

impl Recorded {
    /// Plays the event to `handler`.
    pub(super) fn play(&self, handler: &mut impl SaxHandler) -> Result<(), Failure> {
        match self {
            Recorded::Start {
                namespace,
                local_name,
                attributes,
            } => handler.start_element(namespace, local_name, attributes),
            Recorded::End {
                namespace,
                local_name,
            } => handler.end_element(namespace, local_name),
            Recorded::Characters(text) => handler.characters(text),
        }
    }
}

/// Collects the elements of one name, other than those with the ids it skips, by id.
pub(super) struct Collector {
    wrapper: &'static str,
    skipped: &'static [&'static str],
    /// The note being collected: its id and its events so far.
    current: Option<(String, Vec<Recorded>)>,
    depth: usize,
    collected: Vec<(String, Vec<Recorded>)>,
}

impl Collector {
    pub(super) fn new(wrapper: &'static str, skipped: &'static [&'static str]) -> Self {
        Self {
            wrapper,
            skipped,
            current: None,
            depth: 0,
            collected: Vec::new(),
        }
    }

    /// The collected notes by id, in the order they were found; a later one replaces an
    /// earlier one with its id.
    pub(super) fn into_notes(self) -> Vec<(String, Vec<Recorded>)> {
        self.collected
    }
}

/// The wrapper Tika gives each note: a `body` in the WordprocessingML namespace.
fn wrapper_start() -> Recorded {
    Recorded::Start {
        namespace: W_NAMESPACE.to_owned(),
        local_name: "body".to_owned(),
        attributes: Vec::new(),
    }
}

fn wrapper_end() -> Recorded {
    Recorded::End {
        namespace: W_NAMESPACE.to_owned(),
        local_name: "body".to_owned(),
    }
}

/// Text as a parser reads it back: line ends become line feeds.
fn reparsed_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// An attribute value as a parser reads it back: each line end, tab or line feed a space.
fn reparsed_value(value: &str) -> String {
    value.replace("\r\n", " ").replace(['\r', '\n', '\t'], " ")
}

impl SaxHandler for Collector {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if let Some((_, events)) = &mut self.current {
            self.depth += 1;
            events.push(Recorded::Start {
                namespace: namespace.to_owned(),
                local_name: local_name.to_owned(),
                attributes: attributes
                    .iter()
                    .map(|attribute| Attribute {
                        value: reparsed_value(&attribute.value),
                        ..attribute.clone()
                    })
                    .collect(),
            });
            return Ok(());
        }
        if local_name == self.wrapper {
            if let Some(id) = attribute(attributes, W_NAMESPACE, "id") {
                if !self.skipped.contains(&id) {
                    self.current = Some((id.to_owned(), vec![wrapper_start()]));
                    self.depth = 0;
                }
            }
        }
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        let Some((_, events)) = &mut self.current else {
            return Ok(());
        };
        if self.depth == 0 {
            events.push(wrapper_end());
            if let Some(note) = self.current.take() {
                self.collected.push(note);
            }
            return Ok(());
        }
        self.depth -= 1;
        events.push(Recorded::End {
            namespace: namespace.to_owned(),
            local_name: local_name.to_owned(),
        });
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if let Some((_, events)) = &mut self.current {
            events.push(Recorded::Characters(reparsed_text(text)));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::xml::parse_sax;

    #[test]
    fn collects_notes_by_id_as_tika_does() {
        let xml = "<w:footnotes xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
                   <w:footnote w:id=\"-1\"><w:p/></w:footnote>\
                   <w:footnote w:id=\"1\"><w:p w:x=\"a&#9;b\"><w:r><w:t>a&#13;b</w:t></w:r></w:p></w:footnote>\
                   </w:footnotes>";
        let mut collector = Collector::new("footnote", &["0", "-1"]);
        parse_sax(xml.as_bytes(), "footnotes", &mut collector).unwrap();
        let notes = collector.into_notes();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].0, "1");
        let events = &notes[0].1;
        let text: String = events
            .iter()
            .filter_map(|event| match event {
                Recorded::Characters(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "a\nb");
        let Recorded::Start { attributes, .. } = &events[1] else {
            panic!("{events:?}");
        };
        assert_eq!(attributes[0].value, "a b");
        assert_eq!(events.first(), Some(&wrapper_start()));
        assert_eq!(events.last(), Some(&wrapper_end()));
    }
}
