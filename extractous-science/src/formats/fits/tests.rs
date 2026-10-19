use super::*;

/// A header of `cards`, each padded to 80 characters, ended and padded to a whole block.
fn header(cards: &[&str]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for card in cards.iter().chain(&["END"]) {
        bytes.extend_from_slice(format!("{card:<80}").as_bytes());
    }
    bytes.resize(bytes.len().next_multiple_of(BLOCK as usize), b' ');
    bytes
}

/// A primary image of 3 by 2 bytes of the galaxy M31, then a binary table of events.
fn file() -> Vec<u8> {
    let mut bytes = header(&[
        "SIMPLE  =                    T / conforms to FITS",
        "BITPIX  =                    8",
        "NAXIS   =                    2",
        "NAXIS1  =                    3",
        "NAXIS2  =                    2",
        "OBJECT  = 'M31 / Andromeda''s galaxy' / the target",
        "COMMENT   Observed from Brorfelde.",
    ]);
    bytes.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
    bytes.resize(2 * BLOCK as usize, 0);
    bytes.extend(header(&[
        "XTENSION= 'BINTABLE'           / binary table",
        "BITPIX  =                    8",
        "NAXIS   =                    2",
        "NAXIS1  =                    4",
        "NAXIS2  =                    1",
        "PCOUNT  =                    0",
        "GCOUNT  =                    1",
        "TFIELDS =                    1",
        "TTYPE1  = 'FLUX    '",
        "TUNIT1  = 'Jy      '",
        "EXTNAME = 'EVENTS  '",
    ]));
    bytes.extend_from_slice(&[0, 0, 0, 1]);
    bytes.resize(4 * BLOCK as usize, 0);
    bytes
}

#[test]
fn lists_the_cards_of_every_header() {
    let (text, metadata) = crate::Extractor::new()
        .extract_bytes_to_string(&file())
        .unwrap();
    assert!(text.contains("Primary HDU"), "{text}");
    assert!(text.contains("NAXIS1  =                    3"), "{text}");
    assert!(text.contains("Extension 1: BINTABLE EVENTS"), "{text}");
    assert!(text.contains("TUNIT1  = 'Jy      '"), "{text}");
    assert_eq!(metadata["fits:OBJECT"], ["M31 / Andromeda's galaxy"]);
    assert_eq!(metadata["fits:NAXIS2"], ["2"]);
    assert_eq!(metadata["fits:COMMENT"], ["Observed from Brorfelde."]);
    assert!(!metadata.contains_key("fits:TTYPE1"), "{metadata:?}");
}

#[test]
fn fails_on_a_header_without_an_end() {
    let whole = file();
    let unended = &whole[..BLOCK as usize - 80];
    assert!(units(Bytes::new(unended)).is_err());
    let mut cut = header(&[
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
    ]);
    cut.truncate(2 * 80);
    assert!(units(Bytes::new(&cut)).is_err());
}

#[test]
fn stops_at_what_follows_the_last_extension() {
    let mut bytes = file();
    bytes.extend_from_slice(&[0; 2880]);
    assert_eq!(units(Bytes::new(&bytes)).unwrap().len(), 2);
}

#[test]
fn reads_the_headers_of_a_file_too_large_to_read_whole() {
    let file = extractous_core::large::LargeFile::padded("andromeda.fits", &file());
    let (text, _) = file.extract(FitsParser.name());
    assert!(text.contains("Extension 1: BINTABLE EVENTS"), "{text}");
}
