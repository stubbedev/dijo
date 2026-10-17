//! The records of an EMF picture, read as [MS-EMF] lays them out: each by its type and size, the
//! text records, the fonts their ANSI text is written in, and the comments Tika takes documents
//! and the name of an icon from. Every other record is skipped by its size. POI's `HemfPicture`,
//! which Tika reads the picture with, parses every record and fails the picture on many it
//! cannot read, EMF+ records and bitmaps among them, none of which give text (#127).

use super::{slice, u32_at, Picture, Rect};

/// `EMR_HEADER`.
const HEADER: u32 = 0x01;
/// `EMR_EOF`.
const EOF: u32 = 0x0E;
/// `EMR_SELECTOBJECT`.
const SELECT_OBJECT: u32 = 0x25;
/// `EMR_DELETEOBJECT`.
const DELETE_OBJECT: u32 = 0x28;
/// `EMR_COMMENT`.
const COMMENT: u32 = 0x46;
/// `EMR_EXTCREATEFONTINDIRECTW`.
const EXT_CREATE_FONT_INDIRECT_W: u32 = 0x52;
/// `EMR_EXTTEXTOUTA`.
const EXT_TEXT_OUT_A: u32 = 0x53;
/// `EMR_EXTTEXTOUTW`.
const EXT_TEXT_OUT_W: u32 = 0x54;
/// Where the header record's ` EMF` signature stands.
const SIGNATURE_AT: usize = 40;
const SIGNATURE: u32 = 0x464D_4520;
/// The identifier of a public comment, `GDIC`.
const PUBLIC_COMMENT: u32 = 0x4349_4447;
/// `EMR_COMMENT_MULTIFORMATS`.
const MULTIFORMATS: u32 = 0x4000_0004;
/// `EMR_COMMENT_WINDOWS_METAFILE`.
const WINDOWS_METAFILE: u32 = 0x8000_0001;
/// `ETO_OPAQUE` and `ETO_CLIPPED`, with which a text record's rectangle bounds its text.
const BOUNDED: u32 = 0x0002 | 0x0004;
/// Where a `LogFont`'s charset stands in an `EMR_EXTCREATEFONTINDIRECTW` record's body.
const FONT_CHARSET_AT: usize = 4 + 23;

/// A record Tika's `EMFParser` reads something of.
#[derive(Debug)]
pub(super) enum Record<'a> {
    Comment(Comment<'a>),
    Text(TextOut<'a>),
    /// A font made at an index of the object table, in a `LogFont` charset.
    Font {
        index: u32,
        charset: u8,
    },
    /// The object at an index of the object table selected.
    Select(u32),
    /// The object at an index of the object table deleted.
    Delete(u32),
}

/// What an `EMR_COMMENT` holds, as far as Tika tells comments apart.
#[derive(Debug)]
pub(super) enum Comment<'a> {
    /// `EMR_COMMENT_MULTIFORMATS`: the data of each format.
    Formats(Vec<&'a [u8]>),
    /// `EMR_COMMENT_WINDOWS_METAFILE`: an embedded WMF picture.
    Wmf(&'a [u8]),
    /// A private comment's data.
    Generic(&'a [u8]),
    /// Any other comment.
    Other,
}

/// An `EMR_EXTTEXTOUTA` or `EMR_EXTTEXTOUTW`.
#[derive(Debug)]
pub(super) struct TextOut<'a> {
    /// Whether the text is UTF-16LE (`EMR_EXTTEXTOUTW`), rather than in the selected font's
    /// charset.
    pub(super) unicode: bool,
    /// The rectangle that bounds the text, where the record clips or opaques it, else zero.
    pub(super) bounds: Rect,
    /// The bounds of the record, which it says to ignore.
    pub(super) bounds_ignored: Rect,
    /// The bytes of the string, as many as its characters take.
    pub(super) raw: &'a [u8],
}

/// The records of the EMF picture `data` up to its end-of-file record or its end.
pub(super) fn records(data: &[u8]) -> Picture<Record<'_>> {
    let mut picture = Picture::default();
    if u32_at(data, 0, "the header record's type") != Ok(HEADER)
        || u32_at(data, SIGNATURE_AT, "the header's signature") != Ok(SIGNATURE)
    {
        picture.broken = Some("the data starts with no EMF header record".to_owned());
        return picture;
    }
    let mut at = 0;
    while at < data.len() {
        let read = u32_at(data, at, "a record's type").and_then(|kind| {
            let size = u32_at(data, at + 4, "a record's size")? as usize;
            if size < 8 {
                return Err(format!("a record of type {kind:#x} of {size} bytes"));
            }
            let body = slice(data, at + 8, size - 8, "a record")?;
            Ok((kind, size, record(kind, &data[at..at + size], body)?))
        });
        match read {
            Ok((kind, size, record)) => {
                picture.records.extend(record);
                if kind == EOF {
                    break;
                }
                at += size;
            }
            Err(reason) => {
                picture.broken = Some(format!("the EMF record at {at:#x}: {reason}"));
                break;
            }
        }
    }
    picture
}

/// What Tika reads of the record of type `kind`, whose bytes are `whole`, `body` without its type
/// and size.
fn record<'a>(kind: u32, whole: &'a [u8], body: &'a [u8]) -> Result<Option<Record<'a>>, String> {
    Ok(Some(match kind {
        COMMENT => Record::Comment(comment(body)?),
        EXT_TEXT_OUT_A | EXT_TEXT_OUT_W => {
            Record::Text(text_out(whole, body, kind == EXT_TEXT_OUT_W)?)
        }
        EXT_CREATE_FONT_INDIRECT_W => Record::Font {
            index: u32_at(body, 0, "the font's index")?,
            charset: slice(body, FONT_CHARSET_AT, 1, "the font's charset")?[0],
        },
        SELECT_OBJECT => Record::Select(u32_at(body, 0, "the selected object's index")?),
        DELETE_OBJECT => Record::Delete(u32_at(body, 0, "the deleted object's index")?),
        _ => return Ok(None),
    }))
}

/// A rectangle of four 32-bit coordinates at `at`, left, top, right and bottom.
fn rect_l(body: &[u8], at: usize) -> Result<Rect, String> {
    let value =
        |offset| u32_at(body, at + offset, "a rectangle").map(|value| f64::from(value as i32));
    let (left, top, right, bottom) = (value(0)?, value(4)?, value(8)?, value(12)?);
    Ok(Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

/// An `EMR_EXTTEXTOUTA` or, `unicode`, `EMR_EXTTEXTOUTW`: its bounds, then its `EmrText`
/// after the graphics mode and scales, whose string offset counts from the start of the record.
fn text_out<'a>(whole: &'a [u8], body: &[u8], unicode: bool) -> Result<TextOut<'a>, String> {
    const TEXT: usize = 16 + 4 + 4 + 4;
    let bounds_ignored = rect_l(body, 0)?;
    let chars = u32_at(body, TEXT + 8, "the number of characters")? as usize;
    let offset = u32_at(body, TEXT + 12, "the string's offset")? as usize;
    let options = u32_at(body, TEXT + 16, "the text options")?;
    let bounds = if options & BOUNDED == 0 {
        Rect::default()
    } else {
        rect_l(body, TEXT + 20)?
    };
    let length = chars
        .checked_mul(if unicode { 2 } else { 1 })
        .ok_or_else(|| format!("a string of {chars} characters"))?;
    let raw = if chars == 0 {
        &[][..]
    } else {
        slice(whole, offset, length, "the string")?
    };
    Ok(TextOut {
        unicode,
        bounds,
        bounds_ignored,
        raw,
    })
}

/// An `EMR_COMMENT`: its data, after its size, told apart by its identifier.
fn comment(body: &[u8]) -> Result<Comment<'_>, String> {
    let size = u32_at(body, 0, "the comment's size")? as usize;
    let data = slice(body, 4, size, "the comment's data")?;
    if data.len() < 4 {
        return Ok(Comment::Generic(data));
    }
    if u32_at(data, 0, "the comment's identifier")? != PUBLIC_COMMENT {
        return Ok(Comment::Generic(data));
    }
    Ok(match u32_at(data, 4, "the public comment's type")? {
        MULTIFORMATS => {
            let count = u32_at(data, 24, "the number of formats")? as usize;
            let mut formats = Vec::new();
            for index in 0..count {
                let format = 28 + index * 16;
                let size = u32_at(data, format + 8, "a format's size")? as usize;
                let offset = u32_at(data, format + 12, "a format's offset")? as usize;
                formats.push(slice(data, offset, size, "a format's data")?);
            }
            Comment::Formats(formats)
        }
        WINDOWS_METAFILE => {
            let size = u32_at(data, 20, "the metafile's size")? as usize;
            Comment::Wmf(slice(data, 24, size, "the metafile")?)
        }
        _ => Comment::Other,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::{emf, emf_record, emf_text, int};
    use super::*;

    #[test]
    fn reads_records_up_to_the_end_of_file_record_and_skips_the_rest() {
        let mut data = emf(&[emf_record(0x21, &[]), emf_record(0x99, &[1, 2, 3, 4])]);
        data.extend(emf_record(0x54, &[]));
        let picture = records(&data);
        assert!(picture.records.is_empty(), "{:?}", picture.records);
        assert_eq!(picture.broken, None);
    }

    #[test]
    fn stops_where_a_record_runs_past_the_end_or_is_too_small() {
        let data = emf(&[emf_text("ok", 0, 0, true), emf_text("cut", 0, 0, true)]);
        let cut = &data[..data.len() - 30];
        let picture = records(cut);
        assert_eq!(picture.records.len(), 1);
        assert!(picture.broken.unwrap().contains("a record"));
        let mut small = emf(&[]);
        small.truncate(small.len() - 20);
        small.extend_from_slice(&0x21u32.to_le_bytes());
        small.extend_from_slice(&4u32.to_le_bytes());
        assert!(records(&small).broken.unwrap().contains("of 4 bytes"));
        assert!(records(b"not an EMF picture at all, not at all, nope")
            .broken
            .is_some());
    }

    #[test]
    fn reads_the_text_of_a_text_record_by_its_count_of_characters() {
        let mut record = emf_text("Hi there", 1, 2, true);
        record[8 + 36..8 + 40].copy_from_slice(&int(2));
        let data = emf(&[record]);
        let picture = records(&data);
        let [Record::Text(text)] = picture.records.as_slice() else {
            panic!("one text record: {:?}", picture.records);
        };
        assert!(text.unicode);
        assert_eq!(text.raw, b"H\0i\0");
        assert_eq!(text.bounds, Rect::default());
        assert_eq!(
            text.bounds_ignored,
            Rect {
                x: 1.0,
                y: 2.0,
                width: 50.0,
                height: 10.0
            }
        );
    }
}
