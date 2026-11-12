//! Microsoft Cabinet archives, which Tika reads with no parser: each file handed to the embedded
//! document extractor in the order the cabinet lists it, with its name, length and DOS
//! modification time, as the other archive parsers hand theirs on. The headers ([`cabinet`])
//! are read first; the folders' data ([`folder`]), stored, MSZIP or LZX, is then decoded a
//! block at a time as the files are reached, each file spooled and parsed before the next is
//! decoded, so the parser's memory stays bounded whatever the cabinet's size, and all it
//! decodes stays within [`decode_limit`] of the cabinet's length.
//!
//! A file of a Quantum folder, of a folder that continues the cabinet before in a set, or that
//! goes on in the cabinet after, is not read: it is recorded as an archive stream that cannot
//! be read, and the cabinets before and after are named in the metadata.

mod cabinet;
mod folder;

use cabinet::{Cabinet, File, Place};
use folder::{Copied, End, FolderStream, Uncopied};

use super::{entry_metadata, record_unreadable, within_limit, write_file, Cut};
use crate::compress::{decode_limit, ArchiveFile, Decoding, Limited};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Input, Pages, Paging};

const CAB: &str = "application/vnd.ms-cab-compressed";
/// The cabinet, as a failure names it.
const WHAT: &str = "the cabinet";

/// The file name of the cabinet a cabinet continues in its set.
pub const PREVIOUS_CABINET: &str = "cab:previous-cabinet";
/// The file name of the cabinet that continues a cabinet in its set.
pub const NEXT_CABINET: &str = "cab:next-cabinet";

/// Microsoft Cabinet archives, and the types Tika's detection takes for cabinets of their own,
/// OneNote packages among them.
pub struct CabParser;

impl Parser for CabParser {
    fn name(&self) -> &'static str {
        "dijo::CabParser"
    }
    fn tika_name(&self) -> &'static str {
        "dijo::CabParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[CAB]
    }

    fn media_types_with_subtypes(&self) -> &'static [&'static str] {
        &[CAB]
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
        let cabinet = cabinet::read(input).map_err(|malformed| {
            Decline::Fails(Error::ParseError(format!("{WHAT}: {malformed}")))
        })?;
        let paging = Paging::new(pages);
        paging.release();
        Ok(Box::new(move |xhtml| {
            let written = write(input, &cabinet, &paging, xhtml);
            xhtml.end_document()?;
            written
        }))
    }
}

/// Names the cabinets before and after `cabinet` in its set, and hands its files on in the
/// order it lists them, each read out of `input` as it is reached.
fn write<'a>(
    input: &'a [u8],
    cabinet: &Cabinet,
    paging: &Paging<'a>,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let metadata = xhtml.metadata_mut();
    if let Some(previous) = &cabinet.previous {
        metadata.set(PREVIOUS_CABINET, previous.clone());
    }
    if let Some(next) = &cabinet.next {
        metadata.set(NEXT_CABINET, next.clone());
    }
    let mut reader = Reader::new(input, cabinet, paging);
    for file in &cabinet.files {
        let listed = |content| ArchiveFile {
            name: file.name.clone(),
            size: i64::from(file.size),
            modified_millis: file.modified_millis,
            created_millis: None,
            directory: false,
            content,
        };
        match reader.read(file)? {
            Read::Data(limited) => {
                let (content, cut) = within_limit(format!("{WHAT}'s file {}", file.name), limited);
                write_file(&listed(content), xhtml)?;
                if cut.is_some() {
                    return Cut::end(cut);
                }
            }
            Read::Unread(reason) => {
                entry_metadata(&listed(Input::Borrowed(&[])), xhtml)?;
                record_unreadable(xhtml.metadata_mut(), &file.name, &reason);
            }
        }
    }
    Ok(())
}

/// What is read of a file.
enum Read {
    /// All its data, or its data cut at the limit of what the cabinet is decoded into.
    Data(Limited<Input<'static>>),
    /// Nothing, for this reason.
    Unread(String),
}

/// The files of a cabinet read out of their folders, the folder last read kept decoding where it
/// stands for the files after.
struct Reader<'a, 'p> {
    input: &'a [u8],
    cabinet: &'p Cabinet,
    paging: &'p Paging<'a>,
    continues_previous: bool,
    /// The folder last read, by its index.
    stream: Option<(u16, FolderStream<'a, 'p>)>,
    /// The bytes left to decode the cabinet into.
    budget: u64,
}

impl<'a, 'p> Reader<'a, 'p> {
    /// The reader of the files of `cabinet`, whose headers `input` holds, its blocks counted
    /// read by `paging`.
    fn new(input: &'a [u8], cabinet: &'p Cabinet, paging: &'p Paging<'a>) -> Self {
        Self {
            input,
            cabinet,
            paging,
            continues_previous: cabinet.continues_previous(),
            stream: None,
            budget: decode_limit(input.len()),
        }
    }

    /// What is read of `file`, or the failure of the cabinet where what it decodes into cannot
    /// be held.
    fn read(&mut self, file: &File) -> Result<Read, Failure> {
        let in_previous = |what: &str| match &self.cabinet.previous {
            Some(name) => format!("{what} the previous cabinet, {name}"),
            None => format!("{what} the previous cabinet"),
        };
        let in_next = |what: &str| match &self.cabinet.next {
            Some(name) => format!("{what} the next cabinet, {name}"),
            None => format!("{what} the next cabinet"),
        };
        let index = match file.place {
            Place::ContinuedFromPrevious | Place::ContinuedBoth => {
                return Ok(Read::Unread(in_previous("it starts in")))
            }
            Place::ContinuedToNext => return Ok(Read::Unread(in_next("it ends in"))),
            Place::Folder(0) if self.continues_previous => {
                return Ok(Read::Unread(in_previous("its folder starts in")))
            }
            Place::Folder(index) => index,
        };
        if file.size == 0 {
            return Ok(Read::Data(Limited::Whole(Input::Borrowed(&[]))));
        }
        let offset = u64::from(file.offset);
        let stream = match self.stream.take() {
            Some((current, stream)) if current == index && stream.start() <= offset => stream,
            _ => match FolderStream::new(
                self.input,
                self.paging,
                &self.cabinet.folders[usize::from(index)],
                self.cabinet.block_reserve,
            ) {
                Ok(stream) => stream,
                Err(reason) => return Ok(Read::Unread(reason)),
            },
        };
        let stream = &mut self.stream.insert((index, stream)).1;
        let mut out = Decoding::spooled(self.input.len(), Some(u64::from(file.size)));
        let unheld = |undecoded| {
            Failure::Extraction(Error::IoError(format!(
                "{WHAT}'s file {}: {undecoded}",
                file.name
            )))
        };
        let copied = match stream.copy(offset, u64::from(file.size), &mut out, &mut self.budget) {
            Ok(copied) => copied,
            Err(Uncopied::Corrupt(reason)) => return Ok(Read::Unread(reason)),
            Err(Uncopied::Unheld(undecoded)) => return Err(unheld(undecoded)),
        };
        let content = match copied {
            Copied::Whole | Copied::Cut => out.finish().map_err(unheld)?,
            Copied::Short(End::Split) => return Ok(Read::Unread(in_next("its data goes on in"))),
            Copied::Short(End::Folder) => {
                return Ok(Read::Unread(format!(
                    "its folder's data ends before the file's end, {} bytes into the folder",
                    offset + u64::from(file.size)
                )))
            }
        };
        Ok(Read::Data(match (copied, content) {
            (Copied::Cut, Limited::Whole(content) | Limited::Cut(content)) => Limited::Cut(content),
            (_, content) => content,
        }))
    }
}

#[cfg(test)]
mod tests;
