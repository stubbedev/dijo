use super::*;

/// Stocks.nsf of Apache OpenOffice (`main/odk/examples/java/NotesAccess`, Apache-2.0), a
/// database Notes 5 wrote: three documents of shares and a profile document.
const STOCKS: &[u8] = include_bytes!("../../../../test_files/documents/lotus-notes-stocks.nsf");
/// The database `tests/fixtures/lotus_notes/log.py` writes from the documentation.
const LOG: &[u8] = include_bytes!("../../../../test_files/documents/lotus-notes-log.nsf");
/// The log of `log.py sealed`, its first entry encrypted.
const SEALED: &[u8] = include_bytes!("../../../tests/fixtures/lotus_notes/sealed.nsf");
/// The log of `log.py lz1-attachment`, its attachment compressed with LZ1.
const LZ1_ATTACHMENT: &[u8] =
    include_bytes!("../../../tests/fixtures/lotus_notes/lz1-attachment.nsf");

/// Where `log.py` writes the superblock and the data RRV bucket.
const LOG_SUPERBLOCK_AT: usize = 0x400;
const LOG_DATA_RRV_AT: usize = 0x9400;

/// The body of the XHTML extracted from `input`, without its line breaks and indentation, and
/// its metadata.
fn extract(input: &[u8]) -> (String, dijo_core::Metadata) {
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(input)
        .unwrap();
    assert_eq!(metadata["Content-Type"], [MEDIA_TYPE]);
    assert!(metadata["tk:parsed-by"].iter().any(|parser| parser == NAME));
    let body = crate::formats::testing::body(&xml).replace(['\n', '\t'], "");
    (body, metadata)
}

/// Why the parser declines `input`.
fn declined(input: &[u8]) -> String {
    let configs = ParserConfigs::default();
    let prepared = LotusNotesParser.prepare(input, &TikaMetadata::new(), &configs);
    let reason = match prepared {
        Ok(_) => panic!("the parser reads the database"),
        Err(decline) => format!("{decline:?}"),
    };
    reason
}

/// `database` with `bytes` at `at`.
fn with_bytes(database: &[u8], at: usize, bytes: &[u8]) -> Vec<u8> {
    let mut changed = database.to_vec();
    changed[at..at + bytes.len()].copy_from_slice(bytes);
    changed
}

/// A share of Stocks.nsf as written.
fn share(name: &str, number: &str, price: &str) -> String {
    format!(
        "<div class=\"note\"><dl><dt>Form</dt><dd>Share</dd><dt>Name</dt><dd>{name}</dd>\
         <dt>Number</dt><dd>{number}</dd><dt>SharePrice</dt><dd>{price}</dd></dl></div>"
    )
}

#[test]
fn reads_the_documents_of_a_database_notes_5_wrote() {
    let (body, metadata) = extract(STOCKS);
    assert_eq!(
        body,
        [
            share("BASF", "23", "41.23"),
            share("Daimler", "12", "43.76"),
            share("Bayer", "64", "32.34"),
        ]
        .concat()
    );
    assert_eq!(metadata["dc:title"], ["Share"]);
    assert_eq!(metadata[ODS_VERSION], ["41"]);
}

#[test]
fn writes_lists_times_numbers_rich_text_and_attachments_of_data_notes_alone() {
    let (body, metadata) = extract(LOG);
    assert_eq!(
        body,
        "<div class=\"note\"><dl><dt>Form</dt><dd>Entry</dd>\
         <dt>Subject</dt><dd>Storm on the north pier</dd>\
         <dt>Keepers</dt><dd>Anna Berg</dd><dd>Ove Gr\u{f8}n</dd>\
         <dt>Lit</dt><dd>2026-03-14T18:05:00Z</dd><dt>Ships</dt><dd>3</dd>\
         <dt>Wind</dt><dd>12.5</dd>\
         <dt>Body</dt><dd><p>The lamp was lit at dusk.</p>\
         <p>Waves broke over the north pier;<br />no ships passed.</p>\
         <table><tr><td><p>Hour</p></td><td><p>Wind</p></td></tr>\
         <tr><td><p>21:00</p></td><td><p>SW 9</p></td></tr></table>\
         <p>Ove signed the log.</p></dd></dl>\
         <div class=\"package-entry\"><h1>tide.txt</h1>\
         <p>High water at 05:12, low water at 11:30.</p></div></div>\
         <div class=\"note\"><dl><dt>Form</dt><dd>Entry</dd><dt>Subject</dt><dd>Calm night</dd>\
         <dt>Pressure</dt><dd>1013</dd><dd>1015.5</dd>\
         <dt>Watch</dt><dd>2026-03-15</dd><dd>18:00:00 - 06:30:00</dd>\
         <dt>Body</dt><dd><p>No ships, a clear sky.</p></dd></dl></div>"
    );
    assert_eq!(metadata["dc:title"], ["Lighthouse Log"]);
    assert_eq!(metadata[ODS_VERSION], ["43"]);
}

#[test]
fn fails_databases_of_encrypted_documents_as_encrypted() {
    let reason = declined(SEALED);
    assert!(reason.starts_with("Fails(Encrypted("), "{reason}");
}

#[test]
fn declines_what_no_documentation_describes() {
    let reason = declined(LZ1_ATTACHMENT);
    assert!(reason.contains("tide.txt compressed with LZ1"), "{reason}");
    let lz1_superblock = with_bytes(LOG, LOG_SUPERBLOCK_AT + 68, &2u16.to_le_bytes());
    let reason = declined(&lz1_superblock);
    assert!(reason.contains("a superblock of LZ1"), "{reason}");
    let notes_4 = with_bytes(LOG, 6, &20u32.to_le_bytes());
    let reason = declined(&notes_4);
    assert!(
        reason.starts_with("Unread(\"a database of ODS 20"),
        "{reason}"
    );
}

/// Two RRV entries of one slot would write its note twice, and as many times again as entries
/// name it: a database whose notes share a slot is no database Notes writes.
#[test]
fn fails_notes_that_share_a_slot() {
    let entries_at = LOG_DATA_RRV_AT + 32;
    let mut shared = LOG.to_vec();
    shared.copy_within(entries_at..entries_at + 8, entries_at + 8);
    let checksum = shared[entries_at..LOG_DATA_RRV_AT + 4096]
        .chunks(4)
        .fold(0, |sum, word| {
            sum ^ u32::from_le_bytes(word.try_into().unwrap())
        });
    let shared = with_bytes(&shared, LOG_DATA_RRV_AT + 18, &checksum.to_le_bytes());
    let reason = declined(&shared);
    assert!(reason.contains("two notes lie in slot 1"), "{reason}");
}

#[test]
fn fails_files_that_are_no_database() {
    let reason = declined(b"\x1a\x00\x00\x04\x00\x00");
    assert!(reason.starts_with("Fails(ParseError("), "{reason}");
    let reason = declined(&LOG[..LOG.len() / 2]);
    assert!(reason.starts_with("Fails(ParseError("), "{reason}");
}

/// A database cut short or of bytes changed anywhere fails or reads, and never takes the
/// process down.
#[test]
fn reads_damaged_databases_without_panicking() {
    let extractor = crate::Extractor::new();
    for end in (0..LOG.len()).step_by(997) {
        let _ = extractor.extract_bytes_to_string(&LOG[..end]);
    }
    for at in (6..LOG.len()).step_by(211) {
        let mut damaged = LOG.to_vec();
        damaged[at] ^= 0xA5;
        let _ = extractor.extract_bytes_to_string(&damaged);
    }
}
