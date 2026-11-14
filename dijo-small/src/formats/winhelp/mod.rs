//! Windows Help files (`.hlp`) of WinHelp 3.0, 3.1 and 4, and Multimedia Viewer files laid out
//! as they are, which Tika does not read, read for their text as Manfred Winterhoff's notes on
//! the format and his helpdeco read it.
//!
//! A help file is a file system of internal files ([`file_system`]). `|SYSTEM` ([`system`])
//! gives the compiler's version, the help file's title (`dc:title`) and copyright notice
//! (`dc:rights`), and in WinHelp 4 the language identifier whose ANSI code page the text is in;
//! files without one are read in Windows-1252. The keywords of `|KWBTREE`, those of the help
//! file's index, are its `meta:keyword` values, in their order.
//!
//! The text is that of `|TOPIC` ([`topic`]), whose blocks are LZ77-compressed ([`lz77`]) in
//! most files since WinHelp 3.1 and whose records' text is phrase-compressed where the help file
//! has phrases, the old way or by Hall compression ([`phrases`]). Its records are written in
//! order ([`text`]): each topic's title as a heading, its paragraphs, with their line breaks,
//! tabs and non-breaking spaces, and its tables, a row of cells by each table record. Pictures,
//! those embedded in the text and the `|bm` files, embedded windows, hotspots' targets, macros,
//! fonts and the full-text search index are not read.

mod file_system;
mod lz77;
mod phrases;
mod reader;
mod system;
mod text;
mod topic;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::text::charset::Charset;
use file_system::{BTree, FileSystem};
use phrases::Phrases;
use system::{Layout, System};
use topic::TopicFile;

const NAME: &str = "dijo::WinHelpParser";
const MEDIA_TYPE: &str = "application/winhlp";
const KEYWORDS: &str = "meta:keyword";

/// The text of Windows Help files.
pub struct WinHelpParser;

impl Parser for WinHelpParser {
    fn name(&self) -> &'static str {
        NAME
    }
    fn tika_name(&self) -> &'static str {
        NAME
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// It reads the internal files where they lie, and holds the phrases, the data of one topic
    /// block and one topic record at a time.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    /// Reads the file system, `|SYSTEM`, the phrases and the keywords before anything is
    /// written; a topic record that cannot be read fails the parse after the records before it.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let help = HelpFile::open(input)?;
        Ok(Box::new(move |xhtml| help.write(xhtml)))
    }
}

/// Why a help file is not read: it is not well-formed, as said.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Malformed(String);

impl From<Malformed> for Decline<'_> {
    fn from(Malformed(reason): Malformed) -> Self {
        Decline::Fails(Error::ParseError(format!("Windows Help file: {reason}")))
    }
}

impl From<Malformed> for Failure {
    fn from(Malformed(reason): Malformed) -> Self {
        Failure::Malformed(format!("Windows Help file: {reason}"))
    }
}

/// What a help file holds, read as far as its topics.
struct HelpFile<'a> {
    layout: Layout,
    charset: Charset,
    title: String,
    copyright: String,
    keywords: Vec<String>,
    phrases: Phrases<'a>,
    topic: &'a [u8],
}

impl<'a> HelpFile<'a> {
    fn open(input: &'a [u8]) -> Result<Self, Malformed> {
        let files = FileSystem::open(input)?;
        let required = |name: &str| {
            files
                .file(name)?
                .ok_or_else(|| Malformed(format!("the help file has no {name} file")))
        };
        let system = System::read(required("|SYSTEM")?)?;
        let topic = required("|TOPIC")?;
        let charset = system.charset();
        let decode = |bytes: &[u8]| {
            charset
                .decode(bytes)
                .expect("the charset of a help file decodes")
        };
        let keywords = match files.file("|KWBTREE")? {
            Some(tree) => keywords(tree, &decode)?,
            None => Vec::new(),
        };
        Ok(Self {
            layout: system.layout,
            charset,
            title: decode(system.title),
            copyright: decode(system.copyright),
            keywords,
            phrases: Phrases::read(&files, system.layout)?,
            topic,
        })
    }

    fn write(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let metadata = xhtml.metadata_mut();
        if !self.title.is_empty() {
            metadata.set("dc:title", self.title.as_str());
        }
        if !self.copyright.is_empty() {
            metadata.set("dc:rights", self.copyright.as_str());
        }
        for keyword in &self.keywords {
            metadata.add(KEYWORDS, keyword.as_str());
        }
        let mut writer = text::Writer::new(xhtml, self.charset, &self.phrases);
        for link in TopicFile::new(self.topic, self.layout).links() {
            writer.write(&link?)?;
        }
        writer.finish()
    }
}

/// The keywords of the B+ tree `tree`, each leaf entry a keyword, the times it is used and
/// where the topics it leads to are listed.
fn keywords(tree: &[u8], decode: &impl Fn(&[u8]) -> String) -> Result<Vec<String>, Malformed> {
    let mut keywords = Vec::new();
    for leaf in BTree::read(tree, "the |KWBTREE file")?.leaves() {
        let mut entries = leaf?;
        while let Some(entry) = entries.next_entry() {
            let keyword = entry.string();
            let _count = entry.i16()?;
            let _offset = entry.i32()?;
            keywords.push(decode(keyword));
        }
    }
    Ok(keywords)
}

#[cfg(test)]
mod tests;
