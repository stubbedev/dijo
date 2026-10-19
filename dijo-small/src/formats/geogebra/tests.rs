use super::*;

#[test]
fn orders_slides_as_tika_does() {
    let mut ids = vec!["_slide10", "_slide2", "_slide02", "_slide1"];
    ids.sort_by(|a, b| compare_slides(a, b));
    assert_eq!(ids, ["_slide1", "_slide02", "_slide2", "_slide10"]);
    assert_eq!(slide_id("_slide12/geogebra.xml"), Some("_slide12"));
    assert_eq!(slide_id("_slide/geogebra.xml"), None);
    assert_eq!(slide_id("x/_slide1/geogebra.xml"), None);
    let json =
        json::read(br#"{"a":{"text":"1"},"b":[{"text":"2"}],"a":{"text":"3"}} trailing"#).unwrap();
    let mut texts = Vec::new();
    json.find_values_as_text("text", &mut texts).unwrap();
    assert_eq!(texts, ["3", "2"]);
}

/// A GeoGebra file too large to read whole, after its worksheet an entry stored past what is
/// read whole and one deflated past what a spool holds in memory: each entry is read in place
/// or spooled as it is handed on, under a budget far below them.
#[test]
fn reads_files_too_large_to_read_whole_an_entry_at_a_time() {
    use dijo_core::large::{Binary, LargeFile, SPOOLING_BUDGET};
    use zip::CompressionMethod::{Deflated, Stored};

    use crate::input::MAX_READ_WHOLE;
    use dijo_core::large::{bytes, zip_archive, END, LARGE_PART};

    let worksheet = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><geogebra format="5.0"><construction title="{END}"><element type="text"><caption val="{END}"/></element></construction></geogebra>"#
    );
    let file = LargeFile::written("large.ggb", |out| {
        zip_archive(
            out,
            vec![
                ("geogebra.xml", Deflated, bytes(worksheet.as_bytes())),
                (
                    "stored.bin",
                    Stored,
                    Box::new(|out| {
                        std::io::copy(&mut Binary::new(MAX_READ_WHOLE + (8 << 20)), out).map(drop)
                    }),
                ),
                (
                    "deflated.bin",
                    Deflated,
                    Box::new(|out| std::io::copy(&mut Binary::new(LARGE_PART), out).map(drop)),
                ),
            ],
        )
    });
    assert!(std::fs::metadata(file.path()).unwrap().len() > MAX_READ_WHOLE);
    let (text, metadata) = file.extract_with(
        &crate::Extractor::new(),
        SPOOLING_BUDGET,
        GeoGebraParser.name(),
    );
    assert!(text.contains(END), "{text}");
    assert_eq!(metadata["dc:title"], [END]);
}
