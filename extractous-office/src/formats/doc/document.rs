//! POI's `HWPFDocument`: the streams of a Word 97–2003 document, decrypted where it is
//! encrypted, read into its text, its character, paragraph and section runs, its styles and
//! lists, the tables Tika's extractor reads, and every other structure POI reads on opening the
//! document, failing where it fails.

use super::bins::{
    bin_pages, character_runs, paragraph_runs, rebuild_character_runs, rebuild_paragraph_runs,
    PageNumber, PropertyRun,
};
use super::bytes::{index, safely_clone, utf16_units};
use super::fib::{
    field, is_encrypted, Fib, Story, ENCRYPTION_KEY_LENGTH, OBFUSCATED, TABLE_ONE_FLAG,
};
use super::fields::Fields;
use super::lists::ListTables;
use super::old::OldDocument;
use super::range::Span;
use super::styles::StyleSheet;
use super::text::{ComplexFileTable, Plex};
use crate::config::ParserConfigs;
use crate::formats::crypt::xor::{WordXorKey, Xor};
use crate::formats::crypt::{self, BlockKeys, EncryptionInfo, Mode, Rc4Keys};
use crate::formats::escher::{self, Escher};
use crate::formats::poi::{i16_at, i32_at, u16_at, u8_at, Read, Thrown};
use crate::formats::{Decline, Unported};
use crate::ole2::{EntryId, EntryKind, Ole2, Unreadable};

const WORD_DOCUMENT: &str = "WordDocument";
const TABLE_ZERO: &str = "0Table";
const TABLE_ONE: &str = "1Table";
const DATA: &str = "Data";
const OBJECT_POOL: &str = "ObjectPool";
/// The first `nFib` of Word 97, before which POI reads a document as `HWPFOldDocument`.
const WORD_97: i16 = 106;
/// `HWPFDocument.getMaxRecordLength`.
const MAX_RECORD_LENGTH: i64 = 100_000;
/// `FIB_BASE_LEN`: the bytes of the main stream that stay plain in an encrypted document.
const FIB_BASE: usize = 68;
/// `RC4_REKEYING_INTERVAL`: the bytes each block key decrypts.
const RC4_REKEYING_INTERVAL: usize = 512;
/// The exception POI throws where the password does not open the document.
const WRONG_PASSWORD: &str = "org.apache.poi.EncryptedDocumentException: document is encrypted, \
                              password is invalid - use \
                              Biff8EncryptionKey.setCurrentUserPasswort() to set password before \
                              opening";
const DOP_SIZE: i32 = 500;
const SED_SIZE: i32 = 12;
const FSPA_SIZE: i32 = 26;
/// `FTXBXS`'s size, and where it holds `fReusable` and `lid`.
const FTXBXS_SIZE: i32 = 22;
const FTXBXS_REUSABLE: usize = 8;
const FTXBXS_LID: usize = 14;
const BKF_SIZE: i32 = 4;
const FRD_SIZE: i32 = 2;
const BSTORE_CONTAINER: u16 = 0xF001;
const DG_CONTAINER: u16 = 0xF002;

/// A Word document as Tika's `WordExtractor` reads it: with `HWPFDocument`, or, before Word 97,
/// with `HWPFOldDocument` in `parseWord6`.
pub(super) enum Word {
    Modern(Box<Document>),
    Old(OldDocument),
}

/// A document as Tika's extractor reads it.
pub(super) struct Document {
    pub(super) fib: Fib,
    pub(super) main: Vec<u8>,
    pub(super) data: Vec<u8>,
    /// The text, in UTF-16 units, as POI indexes it.
    pub(super) text: Vec<u16>,
    pub(super) characters: Vec<PropertyRun>,
    pub(super) paragraphs: Vec<PropertyRun>,
    /// The character ranges of the sections, in the order of their starts.
    pub(super) sections: Vec<(i32, i32)>,
    pub(super) styles: StyleSheet,
    pub(super) lists: Option<ListTables>,
    /// The places the document was saved to, by `SavedByTable`.
    pub(super) save_locations: Option<Vec<Option<String>>>,
    /// The authors of its revisions, by `RevisionMarkAuthorTable`.
    pub(super) revision_authors: Option<Vec<Option<String>>>,
    pub(super) fields: Fields,
    /// The stored pictures of the drawing group, `getBStoreContainer`.
    pub(super) picture_store: Option<Escher>,
    table: Vec<u8>,
}

/// Why the document is left to Tika rather than read.
pub(super) fn unported(Thrown(reason): Thrown) -> Unported {
    Unported(format!("POI throws reading the Word document: {reason}"))
}

impl Document {
    /// `new HWPFDocument(directory)`: the document, decrypted with the password where it is
    /// encrypted, or, for one before Word 97, on which it throws `OldWordFileFormatException`,
    /// `new HWPFOldDocument(directory)`; or why it is left to Tika, or fails as Tika's path does
    /// where the password does not open it.
    pub(super) fn read(
        file: &Ole2<'_>,
        root: EntryId,
        configs: &ParserConfigs,
    ) -> Result<Word, Decline<'static>> {
        let mut main = document_bytes(file, root, WORD_DOCUMENT)?;
        let encryption = encryption(file, root, &main, configs)?;
        if let Some(key) = &encryption {
            key.decrypt(&mut main, FIB_BASE);
        }
        let fib = Fib::read(&main).map_err(unported)?;
        if let Some(pool) = file.child_ignoring_case(root, OBJECT_POOL) {
            if file.kind(pool) == EntryKind::Document {
                return Err(Unported(
                    "the document's object pool is no directory, which POI throws on".to_owned(),
                )
                .into());
            }
        }
        if fib.n_fib < WORD_97 {
            return Ok(Word::Old(OldDocument::read(&main)?));
        }
        let table_name = if fib.uses_table_one() {
            TABLE_ONE
        } else {
            TABLE_ZERO
        };
        if file.child_ignoring_case(root, table_name).is_none() {
            return Err(Unported(format!(
                "the document has no table stream {table_name}, which POI throws on"
            ))
            .into());
        }
        let mut table = document_bytes(file, root, table_name)?;
        let mut data = match file.child_ignoring_case(root, DATA) {
            Some(_) => document_bytes(file, root, DATA)?,
            None => Vec::new(),
        };
        if let Some(key) = &encryption {
            let header = key.plain_table();
            if table.len() < header {
                return Err(Unported(
                    "the table stream is shorter than its plain encryption header, which POI \
                     throws on"
                        .to_owned(),
                )
                .into());
            }
            key.decrypt(&mut table, header);
            key.decrypt(&mut data, 0);
        }
        Ok(Word::Modern(Box::new(
            Self::from_streams(fib, main, table, data).map_err(unported)?,
        )))
    }

    fn from_streams(fib: Fib, main: Vec<u8>, table: Vec<u8>, data: Vec<u8>) -> Read<Document> {
        fib.check_unread_fields(&table)?;
        check_document_properties(&table, fib.offset(field::DOP)?, fib.size(field::DOP)?)?;
        let clx = ComplexFileTable::read(&main, &table, fib.offset(field::CLX)?)?;
        let character_pages = bin_pages(
            &table,
            fib.offset(field::PLCFBTECHPX)?,
            fib.size(field::PLCFBTECHPX)?,
            PageNumber::Int,
        )?;
        let characters = character_runs(&main, &character_pages, &clx)?;
        let paragraph_pages = bin_pages(
            &table,
            fib.offset(field::PLCFBTEPAPX)?,
            fib.size(field::PLCFBTEPAPX)?,
            PageNumber::Int,
        )?;
        let paragraphs = paragraph_runs(&main, &data, &paragraph_pages, &clx)?;
        let text = clx.text()?;
        let characters = rebuild_character_runs(characters, &clx)?;
        let paragraphs = rebuild_paragraph_runs(paragraphs, &text, &clx)?;
        for part in [field::PLCSPAHDR, field::PLCSPAMOM] {
            Plex::read(&table, fib.offset(part)?, fib.size(part)?, FSPA_SIZE)?;
        }
        let picture_store = read_office_art(
            &table,
            fib.offset(field::DGGINFO)?,
            fib.size(field::DGGINFO)?,
        )?;
        let sections = read_sections(&main, &table, &fib)?;
        let styles = StyleSheet::read(&table, fib.offset(field::STSHF)?)?;
        check_font_table(&table, fib.offset(field::STTBFFFN)?)?;
        let lists = match (fib.offset(field::PLFLST)?, fib.size(field::PLFLST)?) {
            (0, _) | (_, 0) => None,
            (offset, _) => Some(ListTables::read(
                &table,
                offset,
                fib.offset(field::PLFLFO)?,
            )?),
        };
        let save_locations = read_string_table(&table, &fib, field::STTBSAVEDBY)?.map(|strings| {
            strings
                .as_chunks::<2>()
                .0
                .iter()
                .map(|[_, location]| location.clone())
                .collect()
        });
        let revision_authors = read_string_table(&table, &fib, field::STTBFRMARK)?;
        check_bookmarks(&table, &fib)?;
        for (references, texts) in [
            (field::PLCFENDREF, field::PLCFENDTXT),
            (field::PLCFFNDREF, field::PLCFFNDTXT),
        ] {
            check_plex(&table, &fib, references, FRD_SIZE)?;
            check_plex(&table, &fib, texts, 0)?;
        }
        let fields = Fields::read(&table, &fib)?;
        Ok(Document {
            fib,
            main,
            data,
            text,
            characters,
            paragraphs,
            sections,
            styles,
            lists,
            save_locations,
            revision_authors,
            fields,
            picture_store,
            table,
        })
    }

    /// The text boxes anchored in the main text: for each shape anchored there (`PlcSpaMom`)
    /// that a text box of the text box story is in (`PlcftxbxTxt`, whose `lid` is the shape's
    /// id), the position of its anchor and the text box's range of the text, in the order of
    /// the anchors.
    pub(super) fn anchored_textboxes(&self) -> Read<Vec<(i32, Span)>> {
        let anchors = Plex::read(
            &self.table,
            self.fib.offset(field::PLCSPAMOM)?,
            self.fib.size(field::PLCSPAMOM)?,
            FSPA_SIZE,
        )?;
        let textboxes = Plex::read(
            &self.table,
            self.fib.offset(field::PLCFTXBXTXT)?,
            self.fib.size(field::PLCFTXBXTXT)?,
            FTXBXS_SIZE,
        )?;
        let (story, _) = self.story(Story::Textbox);
        let mut shapes = Vec::with_capacity(textboxes.entries.len());
        for (start, end, ftxbxs) in &textboxes.entries {
            if i16_at(ftxbxs, FTXBXS_REUSABLE)? != 0 {
                continue;
            }
            let span = Span {
                start: story.wrapping_add(*start),
                end: story.wrapping_add(*end),
            };
            shapes.push((i32_at(ftxbxs, FTXBXS_LID)?, span));
        }
        let mut anchored = Vec::new();
        for (position, _, fspa) in &anchors.entries {
            let id = i32_at(fspa, 0)?;
            if let Some(at) = shapes.iter().position(|(shape, _)| *shape == id) {
                anchored.push((*position, shapes.swap_remove(at).1));
            }
        }
        Ok(anchored)
    }

    /// The `PlcfHdd` `new HeaderStories(document)` reads where the header story is not empty.
    pub(super) fn header_plex(&self) -> Read<Option<Plex>> {
        if self.fib.story_length(Story::Header) == 0 {
            return Ok(None);
        }
        let size = self.fib.size(field::PLCFHDD)?;
        if size == 0 {
            return Ok(None);
        }
        Plex::read(&self.table, self.fib.offset(field::PLCFHDD)?, size, 0).map(Some)
    }

    /// The character range of a story, as `HWPFDocument.getRange(SubdocumentType)` gives it.
    pub(super) fn story(&self, story: Story) -> (i32, i32) {
        let mut start: i32 = 0;
        for known in Story::ORDERED {
            let length = self.fib.story_length(known);
            if known == story {
                return (start, start.wrapping_add(length));
            }
            start = start.wrapping_add(length);
        }
        unreachable!("every story is ordered")
    }
}

/// What decrypts the streams of an encrypted document.
enum DocumentKey {
    /// RC4, binary or CryptoAPI, past the encryption header at the table stream's start, of
    /// the length `lKey` gives.
    Rc4 { keys: Rc4Keys, header: usize },
    /// XOR obfuscation, of the whole table stream.
    Xor(WordXorKey),
}

impl DocumentKey {
    /// The bytes of the table stream that stay plain.
    fn plain_table(&self) -> usize {
        match self {
            DocumentKey::Rc4 { header, .. } => *header,
            DocumentKey::Xor(_) => 0,
        }
    }

    /// `data` decrypted in place past its first `from` bytes, which stay plain.
    fn decrypt(&self, data: &mut [u8], from: usize) {
        match self {
            DocumentKey::Rc4 { keys, .. } => keys.decrypt_chunks(data, RC4_REKEYING_INTERVAL, from),
            DocumentKey::Xor(key) => {
                if let Some(obfuscated) = data.get_mut(from..) {
                    key.decrypt(obfuscated, from);
                }
            }
        }
    }
}

/// `getEncryptionInfo` and its password check: the key of an encrypted document, `None` for one
/// that is not, failing as Tika's path does where the password does not open it.
///
/// An obfuscated document holds its XOR key and verifier in `lKey` ([MS-DOC] 2.2.6.1). POI reads
/// them from the table stream's first bytes instead, and so opens no such document (#164); this
/// reads them where they are.
fn encryption(
    file: &Ole2<'_>,
    root: EntryId,
    main: &[u8],
    configs: &ParserConfigs,
) -> Result<Option<DocumentKey>, Decline<'static>> {
    if !is_encrypted(main).map_err(unported)? {
        return Ok(None);
    }
    let flags = u16_at(main, 0xA).map_err(unported)?;
    let password = crypt::password(configs);
    let wrong_password = || Decline::Fails(crypt::encrypted(Some(WRONG_PASSWORD)));
    if flags & OBFUSCATED != 0 {
        let verifier = i32_at(main, ENCRYPTION_KEY_LENGTH).map_err(unported)? as u32;
        return Xor::of_word_verifier(verifier)
            .verify_word(password)
            .map(|key| Some(DocumentKey::Xor(key)))
            .ok_or_else(wrong_password);
    }
    let table_name = if flags & TABLE_ONE_FLAG != 0 {
        TABLE_ONE
    } else {
        TABLE_ZERO
    };
    let header_length = encryption_header_length(main)?;
    let table = document_bytes(file, root, table_name)?;
    let header = &table[..header_length.min(table.len())];
    let info = EncryptionInfo::read(header, Mode::Unset).map_err(|unopened| unopened.unported())?;
    info.verify(password)
        .map(|keys| {
            Some(DocumentKey::Rc4 {
                keys,
                header: header_length,
            })
        })
        .ok_or_else(wrong_password)
}

/// `lKey` of the FIB's base: the length of the encryption header at the table stream's start,
/// which stays plain.
fn encryption_header_length(main: &[u8]) -> Result<usize, Unported> {
    let length = i32_at(main, ENCRYPTION_KEY_LENGTH).map_err(unported)?;
    usize::try_from(length).map_err(|_| {
        Unported("a negative encryption header length, which POI throws on".to_owned())
    })
}

/// `getDocumentEntryBytes`: the stream named in any case, read whole.
fn document_bytes(file: &Ole2<'_>, root: EntryId, name: &str) -> Result<Vec<u8>, Unported> {
    let entry = file
        .child_ignoring_case(root, name)
        .filter(|&entry| file.kind(entry) == EntryKind::Document)
        .ok_or_else(|| {
            Unported(format!(
                "the document has no stream {name}, which POI throws on"
            ))
        })?;
    file.read(entry).map_err(|Unreadable(reason)| {
        Unported(format!(
            "POI cannot read the document's stream {name}: {reason}"
        ))
    })
}

/// `new DocumentProperties(tableStream, fcDop, lcbDop)`: its fields, and the bytes past them.
fn check_document_properties(table: &[u8], offset: i32, size: i32) -> Read<()> {
    let at = index(offset)?;
    i16_at(table, at + DOP_SIZE as usize - 2)?;
    if size != DOP_SIZE {
        safely_clone(
            table,
            offset.wrapping_add(DOP_SIZE),
            size.wrapping_sub(DOP_SIZE),
            MAX_RECORD_LENGTH,
        )?;
    }
    Ok(())
}

/// `new OfficeArtContent(tableStream, fcDggInfo, lcbDggInfo)`: the drawing group's store of
/// pictures, after its drawings are read as POI reads them.
fn read_office_art(table: &[u8], offset: i32, size: i32) -> Read<Option<Escher>> {
    if size == 0 {
        return Ok(None);
    }
    let mut position = index(offset)?;
    let (group, length) = escher::container_at(table, position)?;
    position = advance(position, length)?;
    let end = i64::from(offset) + i64::from(size);
    while (position as i64) < end {
        let label = u8_at(table, position)?;
        if label > 1 {
            return Err(Thrown(format!(
                "Invalid dgglbl when filling Escher records: {}",
                label as i8
            )));
        }
        position += 1;
        let (drawing, length) = escher::container_at(table, position)?;
        position = advance(position, length)?;
        if drawing.record_id != DG_CONTAINER {
            return Err(Thrown(format!(
                "Did have an invalid record-type: {} when filling Escher records",
                drawing.record_id as i16
            )));
        }
    }
    if position as i64 != end {
        return Err(Thrown(format!(
            "Did not read all data when filling Escher records: pos: {position}, offset: \
             {offset}, size: {size}"
        )));
    }
    Ok(group.child(BSTORE_CONTAINER).cloned())
}

/// A position moved on by the length a record says it read.
fn advance(position: usize, length: isize) -> Read<usize> {
    position
        .checked_add_signed(length)
        .ok_or_else(|| Thrown::new("Escher records run back before the data"))
}

/// `new SectionTable(...)`: the sections' character ranges, sorted by their starts.
fn read_sections(main: &[u8], table: &[u8], fib: &Fib) -> Read<Vec<(i32, i32)>> {
    let plex = Plex::read(
        table,
        fib.offset(field::PLCFSED)?,
        fib.size(field::PLCFSED)?,
        SED_SIZE,
    )?;
    let mut sections = Vec::with_capacity(plex.entries.len());
    for (start, end, descriptor) in &plex.entries {
        i16_at(descriptor, 0)?;
        let sepx = i32_at(descriptor, 2)?;
        i16_at(descriptor, 6)?;
        i32_at(descriptor, 8)?;
        if sepx != -1 {
            let size = i32::from(i16_at(main, index(sepx)?)?);
            safely_clone(main, sepx.wrapping_add(2), size, MAX_RECORD_LENGTH)?;
        }
        sections.push((*start, *end));
    }
    sections.sort_by_key(|&(start, _)| start);
    Ok(sections)
}

/// `new FontTable(tableStream, fcSttbfffn, lcbSttbfffn)`: its fonts read as POI reads them.
fn check_font_table(table: &[u8], offset: i32) -> Read<()> {
    const FIXED_SIZE: i32 = 40;
    let at = index(offset)?;
    let count = i32::from(i16_at(table, at)?);
    i16_at(table, at + 2)?;
    if count < 0 {
        return Err(Thrown(format!("Cannot create an array of {count} fonts")));
    }
    let mut position = at + 4;
    for _ in 0..count {
        let size = i32::from(u8_at(table, position)?) + 1;
        u8_at(table, position + 1)?;
        i16_at(table, position + 2)?;
        u8_at(table, position + 4)?;
        u8_at(table, position + 5)?;
        if position + FIXED_SIZE as usize > table.len() {
            return Err(Thrown::new(
                "a font's panose and signature run past the table",
            ));
        }
        let name_length = (size - FIXED_SIZE) / 2;
        if name_length < 0 {
            return Err(Thrown(format!(
                "Had invalid computed size: {name_length} with size {size} and offsetTmp: 40"
            )));
        }
        utf16_units(table, position + FIXED_SIZE as usize, name_length as usize)?;
        position += size as usize;
    }
    Ok(())
}

/// `Sttb` read where the FIB places one: the strings of extended characters, `None` for each
/// of negative length, and no strings where the table is not of extended characters.
fn read_string_table(table: &[u8], fib: &Fib, which: usize) -> Read<Option<Vec<Option<String>>>> {
    let (offset, size) = (fib.offset(which)?, fib.size(which)?);
    if offset == 0 || size == 0 {
        return Ok(None);
    }
    read_sttb(table, offset).map(Some)
}

/// `new Sttb(2, buffer, startOffset)`.
fn read_sttb(table: &[u8], offset: i32) -> Read<Vec<Option<String>>> {
    let at = index(offset)?;
    if i16_at(table, at)? != -1 {
        return Ok(Vec::new());
    }
    let count = usize::from(u16_at(table, at + 2)?);
    let extra = usize::from(u16_at(table, at + 4)?);
    let mut position = at + 6;
    let mut strings = Vec::with_capacity(count);
    for _ in 0..count {
        let length = i16_at(table, position)?;
        position += 2;
        let Ok(length) = usize::try_from(length) else {
            strings.push(None);
            continue;
        };
        if position >= table.len() {
            return Err(Thrown(format!(
                "Illegal offset {position} (String data is of length {})",
                table.len()
            )));
        }
        let units = utf16_units(table, position, length)?;
        strings.push(Some(String::from_utf16_lossy(&units)));
        position += length * 2;
        if position > table.len() {
            return Err(Thrown(format!(
                "Index {position} out of bounds for length {}",
                table.len()
            )));
        }
        position += extra;
    }
    Ok(strings)
}

/// `new BookmarksTables(tableStream, fib)`.
fn check_bookmarks(table: &[u8], fib: &Fib) -> Read<()> {
    if fib.offset(field::STTBFBKMK)? != 0 && fib.size(field::STTBFBKMK)? != 0 {
        read_sttb(table, fib.offset(field::STTBFBKMK)?)?;
    }
    check_plex(table, fib, field::PLCFBKF, BKF_SIZE)?;
    check_plex(table, fib, field::PLCFBKL, 0)
}

/// A `PlexOfCps` read where the FIB places one.
fn check_plex(table: &[u8], fib: &Fib, which: usize, struct_size: i32) -> Read<()> {
    let (offset, size) = (fib.offset(which)?, fib.size(which)?);
    if offset != 0 && size != 0 {
        Plex::read(table, offset, size, struct_size)?;
    }
    Ok(())
}
