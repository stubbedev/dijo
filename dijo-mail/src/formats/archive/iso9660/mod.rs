//! ISO 9660 disc images, which Tika reads with no parser, read as the archive parsers read
//! theirs: each file but a directory, depth first in the order of the directories' records,
//! handed to the embedded document extractor with the metadata `handleEntryMetadata` gives an
//! archive's entry, its path, size and dates; the volume's identifier, publisher, data
//! preparer, application and dates as the image's metadata.
//!
//! The tree read is that of the primary volume descriptor ([`volume`]) with its Rock Ridge
//! names and times ([`rock_ridge`]) where it has them, else that of a Joliet supplementary
//! descriptor, else the primary's with its ISO 9660 names ([`directory`]). Symbolic links are
//! recorded, not followed, and a directory is read once, so that a directory pointing back to
//! another cannot loop. An image of UDF alone, as most DVDs and Blu-ray discs are, has no ISO
//! 9660 file system, which its metadata says.
//!
//! Files are read in place, but for one recorded in several extents apart, which is spooled.
//! A file that is interleaved, zisofs-compressed, or recorded past the image's end is
//! recorded as unreadable, as is a directory that cannot be read. As with a decoded archive,
//! the files handed on stop at [`decode_limit`] bytes in all, so that records pointing many
//! times at the same extent are no bomb.

mod dates;
mod directory;
mod rock_ridge;
mod volume;

use std::collections::BTreeSet;

use directory::{extent, span, Encoding, Record, Records};
use rock_ridge::RockRidge;
use volume::{Descriptor, Recognized};

use super::{record_unreadable, write_file, Cut};
use crate::compress::{decode_limit, ArchiveFile};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::EXCEPTION_WARNING;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Input, Pages, Paging};

/// The image's files, as a failure names them.
const WHAT: &str = "the files of the ISO 9660 image";
/// The symbolic links of the image, each as its path, ` -> ` and its target.
const SYMBOLIC_LINK: &str = "iso9660:symbolic-link";
/// What the metadata of an image of UDF alone says of it.
const UDF_ONLY: &str = "the image has no ISO 9660 file system, only a UDF one, which dijo does \
                        not read";

/// The parser of ISO 9660 images.
pub struct Iso9660Parser;

impl Parser for Iso9660Parser {
    fn name(&self) -> &'static str {
        "dijo::Iso9660Parser"
    }
    fn tika_name(&self) -> &'static str {
        "dijo::Iso9660Parser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-iso9660-image"]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let unread = |reason: String| {
            Decline::Fails(Error::ParseError(format!("the ISO 9660 image: {reason}")))
        };
        let (primary, joliet) = match volume::recognize(input).map_err(unread)? {
            Recognized::UdfOnly => {
                return Ok(Box::new(|xhtml| {
                    xhtml.metadata_mut().add(EXCEPTION_WARNING, UDF_ONLY);
                    Ok(())
                }))
            }
            Recognized::Iso9660 { primary, joliet } => (primary, joliet),
        };
        let tree = Tree::of(input, &primary, joliet.as_ref()).map_err(unread)?;
        let labels = joliet.unwrap_or(primary);
        Ok(Box::new(move |xhtml| {
            labels.set_metadata(xhtml.metadata_mut());
            tree.write(pages, xhtml)
        }))
    }
}

/// How the names of a tree are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Naming {
    /// From Rock Ridge's `NM` entries, after `skip` bytes of each system use field, else from
    /// the ISO 9660 identifiers.
    RockRidge { skip: usize },
    /// From the identifiers, encoded so.
    Identifiers(Encoding),
}

/// Where a directory's records are: their logical block, and their length, which a relocated
/// directory's `CL` entry leaves to its own record to give.
#[derive(Debug, Clone, Copy)]
struct Location {
    block: u64,
    length: Option<u64>,
}

/// A directory to read, and its path.
struct Directory {
    path: String,
    location: Location,
}

/// What a directory record, or the records of a file in several extents, stand for.
enum Kind<'a> {
    /// A file, its extents in order, adjacent ones joined.
    File {
        pieces: Vec<&'a [u8]>,
        size: u64,
    },
    Directory(Location),
    SymbolicLink(String),
    /// A relocated directory, read where its `CL` record stands.
    Relocated,
    /// A file whose data is not read, for this reason.
    Unread(String),
}

/// A file or directory of a directory, by its name.
struct Entry<'a> {
    name: String,
    created: Option<i64>,
    modified: Option<i64>,
    kind: Kind<'a>,
}

/// The tree of an image read.
struct Tree<'a> {
    input: &'a [u8],
    block_size: u64,
    naming: Naming,
    root: Location,
}

impl<'a> Tree<'a> {
    /// The tree of `input` read: the primary descriptor's with Rock Ridge, else the Joliet
    /// descriptor's, if any, else the primary's.
    fn of(
        input: &'a [u8],
        primary: &Descriptor<'a>,
        joliet: Option<&Descriptor<'a>>,
    ) -> Result<Self, String> {
        let block_size = primary.block_size()?;
        let root = primary.root()?;
        let own = Records::new(extent(input, root.data_block(), block_size, root.length)?)
            .next()
            .transpose()?
            .filter(Record::is_self_or_parent)
            .ok_or("the root directory does not start with its own record")?;
        let rock_ridge = rock_ridge::detect(input, block_size, own.system_use)?;
        let (descriptor, naming) = match (rock_ridge, joliet) {
            (Some(skip), _) => (primary, Naming::RockRidge { skip }),
            (None, Some(joliet)) => (joliet, Naming::Identifiers(Encoding::Joliet)),
            (None, None) => (primary, Naming::Identifiers(Encoding::Iso)),
        };
        let root = descriptor.root()?;
        Ok(Tree {
            input,
            block_size: descriptor.block_size()?,
            naming,
            root: Location {
                block: root.data_block(),
                length: Some(root.length),
            },
        })
    }

    /// Writes each file of the tree, the pages of the image released by paging as they are
    /// read, and records what cannot be read.
    fn write(&self, pages: Pages<'a>, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let paging = Paging::new(pages);
        let limit = decode_limit(self.input.len());
        let mut handed: u64 = 0;
        let mut read = BTreeSet::from([self.root.block]);
        let mut pending = vec![Directory {
            path: String::new(),
            location: self.root,
        }];
        while let Some(directory) = pending.pop() {
            let shown = if directory.path.is_empty() {
                "/"
            } else {
                &directory.path
            };
            let (entries, broken) = match self.entries(directory.location) {
                Ok(listed) => listed,
                Err(reason) => {
                    record_unreadable(xhtml.metadata_mut(), shown, &reason);
                    continue;
                }
            };
            let mut subdirectories = Vec::new();
            for entry in entries {
                let path = if directory.path.is_empty() {
                    entry.name
                } else {
                    format!("{}/{}", directory.path, entry.name)
                };
                match entry.kind {
                    Kind::File { pieces, size } => {
                        handed = handed.saturating_add(size);
                        if handed > limit {
                            return Cut::end(Some(Cut(WHAT.to_owned())));
                        }
                        let content = Input::joined(&pieces, pages).map_err(|error| {
                            Failure::Extraction(Error::IoError(format!(
                                "spooling the extents of {path} of the ISO 9660 image: {error}"
                            )))
                        })?;
                        let file = ArchiveFile {
                            name: path,
                            size: i64::try_from(size).expect("at most the decode limit"),
                            modified_millis: entry.modified,
                            created_millis: entry.created,
                            directory: false,
                            content,
                        };
                        write_file(&file, xhtml)?;
                        paging.read(file.content.len());
                    }
                    Kind::Directory(location) => subdirectories.push(Directory { path, location }),
                    Kind::SymbolicLink(target) => xhtml
                        .metadata_mut()
                        .add(SYMBOLIC_LINK, format!("{path} -> {target}")),
                    Kind::Relocated => {}
                    Kind::Unread(reason) => record_unreadable(xhtml.metadata_mut(), &path, &reason),
                }
            }
            if let Some(reason) = broken {
                record_unreadable(xhtml.metadata_mut(), shown, &reason);
            }
            for subdirectory in subdirectories.into_iter().rev() {
                if read.insert(subdirectory.location.block) {
                    pending.push(subdirectory);
                } else {
                    record_unreadable(
                        xhtml.metadata_mut(),
                        &subdirectory.path,
                        &format!(
                            "it is the directory at block {}, which is read already",
                            subdirectory.location.block
                        ),
                    );
                }
            }
        }
        Ok(())
    }

    /// The entries of the directory at `location`, and why the records after them cannot be
    /// read, if they cannot.
    fn entries(&self, location: Location) -> Result<(Vec<Entry<'a>>, Option<String>), String> {
        let length = match location.length {
            Some(length) => length,
            None => self.own_length(location.block)?,
        };
        let data = extent(self.input, location.block, self.block_size, length)?;
        let mut entries = Vec::new();
        let mut file = Vec::new();
        for record in Records::new(data) {
            let record = match record {
                Ok(record) => record,
                Err(reason) => return Ok((entries, Some(reason))),
            };
            if record.is_self_or_parent() {
                continue;
            }
            let continues = record.continues() && !record.is_directory();
            file.push(record);
            if !continues {
                entries.push(self.entry(&std::mem::take(&mut file)));
            }
        }
        let broken = (!file.is_empty()).then(|| {
            "its last file's record says it goes on in another, which does not follow".to_owned()
        });
        Ok((entries, broken))
    }

    /// The length of the directory at `block`, as its own record, its first, gives it.
    fn own_length(&self, block: u64) -> Result<u64, String> {
        let first = extent(self.input, block, self.block_size, self.block_size)?;
        let own = Records::new(first)
            .next()
            .transpose()?
            .filter(|own| own.is_self_or_parent() && own.data_block() == block)
            .ok_or_else(|| format!("the relocated directory at block {block} has no own record"))?;
        Ok(own.length)
    }

    /// The entry the `records` of one file or directory stand for, the last naming it.
    fn entry(&self, records: &[Record<'a>]) -> Entry<'a> {
        let last = records.last().expect("a file has a record");
        let (encoding, rock_ridge) = match self.naming {
            Naming::RockRidge { skip } => {
                let area = last.system_use.get(skip..).unwrap_or(&[]);
                (
                    Encoding::Iso,
                    rock_ridge::read(self.input, self.block_size, area),
                )
            }
            Naming::Identifiers(encoding) => (encoding, Ok(RockRidge::default())),
        };
        let identified = encoding.name(last.identifier, last.is_directory());
        let rock_ridge = match rock_ridge {
            Ok(rock_ridge) => rock_ridge,
            Err(reason) => {
                return Entry {
                    name: identified,
                    created: None,
                    modified: last.recorded,
                    kind: Kind::Unread(format!("its Rock Ridge entries are unread: {reason}")),
                }
            }
        };
        let kind = self.kind(records, &rock_ridge);
        Entry {
            name: rock_ridge.name.unwrap_or(identified),
            created: rock_ridge.created,
            modified: rock_ridge.modified.or(last.recorded),
            kind,
        }
    }

    /// What the `records` of one file or directory, whose Rock Ridge entries are `rock_ridge`,
    /// stand for.
    fn kind(&self, records: &[Record<'a>], rock_ridge: &RockRidge) -> Kind<'a> {
        let last = records.last().expect("a file has a record");
        if rock_ridge.relocated {
            return Kind::Relocated;
        }
        if let Some(block) = rock_ridge.child {
            return Kind::Directory(Location {
                block,
                length: None,
            });
        }
        if last.is_directory() {
            return Kind::Directory(Location {
                block: last.data_block(),
                length: Some(last.length),
            });
        }
        if let Some(target) = &rock_ridge.link {
            return Kind::SymbolicLink(target.clone());
        }
        if rock_ridge.compressed {
            return Kind::Unread(
                "its data is zisofs-compressed, which dijo does not decode".into(),
            );
        }
        if records.iter().any(Record::is_interleaved) {
            return Kind::Unread("it is recorded interleaved, which dijo does not read".into());
        }
        if records
            .iter()
            .any(|record| record.identifier != last.identifier)
        {
            return Kind::Unread(
                "its extents' records name it differently, so they are of no one file".into(),
            );
        }
        let mut spans: Vec<(u64, u64)> = Vec::with_capacity(records.len());
        for record in records.iter().filter(|record| record.length > 0) {
            let start = record.data_block().saturating_mul(self.block_size);
            match spans.last_mut() {
                Some((from, length)) if from.checked_add(*length) == Some(start) => {
                    *length += record.length;
                }
                _ => spans.push((start, record.length)),
            }
        }
        let pieces = spans
            .into_iter()
            .map(|(start, length)| span(self.input, start, length))
            .collect::<Result<Vec<_>, _>>();
        match pieces {
            Ok(pieces) => Kind::File {
                size: records.iter().map(|record| record.length).sum(),
                pieces,
            },
            Err(reason) => Kind::Unread(reason),
        }
    }
}

#[cfg(test)]
mod tests;
