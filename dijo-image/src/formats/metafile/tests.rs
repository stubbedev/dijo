use super::testing::*;
use super::*;

/// The body of the XHTML the native parser extracts of `picture`.
fn body(picture: &[u8]) -> String {
    let (xml, _) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(picture)
        .unwrap();
    xml[xml.find("<body>").unwrap()..].to_owned()
}

/// Where Tika fails on a record POI cannot read, decodes ANSI text as UTF-8 or a WMF's
/// text in the last font made, the port reads on and decodes text in the font selected (#127).
#[test]
fn reads_records_tika_fails_on_and_text_in_the_font_selected() {
    let unknown = emf(&[
        emf_record(0x45, &[]),
        emf_record(0x08, &[&[0; 16][..], &int(1), &int(0), &int(0)].concat()),
        emf_text("After them", 0, 0, true),
    ]);
    assert_eq!(body(&unknown), "<body><p>After them</p>\n</body></html>");
    let ansi = emf(&[
        emf_text("caf\u{e9}", 0, 0, false),
        emf_font(1, 0xCC),
        emf_select(1),
        emf_text("\u{cf}\u{f0}\u{e8}\u{e2}\u{e5}\u{f2}", 0, 20, false),
    ]);
    assert_eq!(
        body(&ansi),
        "<body><p>caf\u{e9}</p>\n<p>\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}</p>\n</body></html>"
    );
    let selected = wmf(&[
        wmf_font(0xCC),
        wmf_font(0xEE),
        wmf_select(0),
        wmf_text_out(b"\xcf\xf0\xe8\xe2\xe5\xf2"),
        wmf_record(0x0999, &[]),
    ]);
    assert_eq!(
        body(&selected),
        "<body><p>\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}</p>\n</body></html>"
    );
}

#[test]
fn decodes_every_charset_a_font_names() {
    for id in 0..=u8::MAX {
        if let Some(name) = font_charset(id) {
            assert!(
                Charset::named(name).decode(b"abc").is_some(),
                "{name} is not decoded"
            );
        }
    }
}

#[test]
fn reads_the_name_an_icon_only_comment_gives_up_to_its_end() {
    let name = utf16("Report.docx\0");
    assert_eq!(unicode_string(&name).as_deref(), Some("Report.docx"));
    assert_eq!(
        unicode_string(&utf16("Report.docx")).as_deref(),
        Some("Report.docx")
    );
    let marked = [&[0u8, 0][..], &name].concat();
    assert_eq!(unicode_string(&marked).as_deref(), Some(""));
    assert_eq!(unicode_string(&[0]), None);
}
