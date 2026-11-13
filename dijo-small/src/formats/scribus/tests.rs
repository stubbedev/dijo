use super::*;

/// The XHTML body and the metadata of the Scribus 1.5 document whose `DOCUMENT` has the
/// attributes `attributes` and holds `content`.
fn extract_with(attributes: &str, content: &str) -> (String, dijo_core::Metadata) {
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<SCRIBUSUTF8NEW Version=\"1.5.8\">\n\
         <DOCUMENT ANZPAGES=\"1\" {attributes}>\n{content}\n</DOCUMENT>\n</SCRIBUSUTF8NEW>\n"
    );
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(document.as_bytes())
        .unwrap();
    assert_eq!(metadata["Content-Type"], [MEDIA_TYPE]);
    let body = crate::formats::testing::body(&xml).to_owned();
    (body, metadata)
}

fn extract(content: &str) -> String {
    extract_with("", content).0
}

/// A text frame on page `page` at `x`, `y` whose `StoryText` holds `story`.
fn frame(id: u32, page: i32, x: u32, y: u32, links: (i64, i64), story: &str) -> String {
    let (next, back) = links;
    format!(
        "<PAGEOBJECT XPOS=\"{x}\" YPOS=\"{y}\" OwnPage=\"{page}\" ItemID=\"{id}\" PTYPE=\"4\" \
         NEXTITEM=\"{next}\" BACKITEM=\"{back}\"><StoryText><DefaultStyle/>{story}</StoryText>\
         </PAGEOBJECT>"
    )
}

fn alone(id: u32, page: i32, x: u32, y: u32, story: &str) -> String {
    frame(id, page, x, y, (-1, -1), story)
}

#[test]
fn writes_paragraphs_and_headings_by_their_styles_and_parents() {
    let body = extract(&format!(
        "<STYLE NAME=\"Default Paragraph Style\"/><STYLE NAME=\"Heading 1\" \
         PARENT=\"Default Paragraph Style\"/><STYLE NAME=\"Chapter\" PARENT=\"Heading 2\"/>\
         <STYLE NAME=\"Loop A\" PARENT=\"Loop B\"/><STYLE NAME=\"Loop B\" PARENT=\"Loop A\"/>{}",
        alone(
            1,
            0,
            0,
            0,
            "<ITEXT CH=\"Log\"/><para PARENT=\"Heading 1\"/><ITEXT CH=\"Duties\"/>\
             <para PARENT=\"Chapter\"/><ITEXT CH=\"Plain\"/><para PARENT=\"Loop A\"/><para/>\
             <ITEXT CH=\"Deep\"/><trail PARENT=\"Heading 9\"/>"
        )
    ));
    assert_eq!(
        body,
        "<h1>Log</h1>\n<h2>Duties</h2>\n<p>Plain</p>\n<p>Deep</p>\n"
    );
}

#[test]
fn keeps_tabs_spaces_and_breaks_and_leaves_out_hyphenation_points() {
    let body = extract(&alone(
        1,
        0,
        0,
        0,
        "<ITEXT CH=\"Lamp\"/><tab/><ITEXT CH=\"oil\"/><nbspace/><ITEXT CH=\"40\u{ad} l\"/>\
         <breakline/><ITEXT CH=\"wick\u{ad}\u{ad}ed\"/><nbhyphen/><breakcol/><var name=\"pgno\"/>\
         <ITEXT Unicode=\"28\"/><ITEXT CH=\"end\"/><MARK label=\"m\" type=\"3\"/>",
    ));
    assert_eq!(
        body,
        "<p>Lamp\toil\u{a0}40 l<br />\nwick\u{ad}ed\u{2011}<br />\n<br />\nend</p>\n"
    );
}

#[test]
fn ends_the_paragraphs_of_older_documents_at_their_line_breaks() {
    let body = extract_with(
        "",
        "<STYLE NAME=\"Heading 2\"/><PAGEOBJECT XPOS=\"0\" YPOS=\"0\" OwnPage=\"0\" PTYPE=\"4\" \
         NEXTITEM=\"-1\" BACKITEM=\"-1\"><ITEXT PSTYLE=\"Heading 2\" CH=\"Title&#10;\"/>\
         <ITEXT CH=\"One&#13;Two&#9;tab\"/></PAGEOBJECT>",
    )
    .0;
    assert_eq!(body, "<h2>Title</h2>\n<p>One</p>\n<p>Two\ttab</p>\n");
}

#[test]
fn orders_frames_by_master_page_page_and_place_then_the_pasteboard() {
    let body = extract(&format!(
        "<MASTEROBJECT XPOS=\"0\" YPOS=\"900\" OwnPage=\"0\" ItemID=\"9\" PTYPE=\"4\" \
         NEXTITEM=\"-1\" BACKITEM=\"-1\"><StoryText><ITEXT CH=\"Running head\"/></StoryText>\
         </MASTEROBJECT>{}{}{}{}{}",
        alone(1, -1, 0, 0, "<ITEXT CH=\"Pasteboard\"/>"),
        alone(2, 1, 0, 10, "<ITEXT CH=\"Second page\"/>"),
        alone(3, 0, 300, 50, "<ITEXT CH=\"Right\"/>"),
        alone(4, 0, 20, 50, "<ITEXT CH=\"Left\"/>"),
        alone(5, 0, 400, 20, "<ITEXT CH=\"Top\"/>"),
    ));
    assert_eq!(
        body,
        "<p>Running head</p>\n<p>Top</p>\n<p>Left</p>\n<p>Right</p>\n<p>Second page</p>\n\
         <p>Pasteboard</p>\n"
    );
}

#[test]
fn writes_a_threaded_story_once_where_its_first_frame_is() {
    let body = extract(&format!(
        "{}{}{}{}",
        frame(7, 1, 0, 0, (-1, 6), ""),
        alone(5, 0, 0, 0, "<ITEXT CH=\"Before\"/>"),
        frame(
            6,
            0,
            0,
            500,
            (7, -1),
            "<ITEXT CH=\"Begun here\"/><para/><ITEXT CH=\"and run on\"/>"
        ),
        frame(8, 0, 0, 100, (-1, -1), "<ITEXT CH=\"Between\"/>"),
    ));
    assert_eq!(
        body,
        "<p>Before</p>\n<p>Between</p>\n<p>Begun here</p>\n<p>and run on</p>\n"
    );
}

#[test]
fn threads_older_documents_by_their_frames_places_in_the_list() {
    let body = extract(
        "<PAGEOBJECT XPOS=\"0\" YPOS=\"0\" OwnPage=\"1\" PTYPE=\"4\" NEXTITEM=\"-1\" \
         BACKITEM=\"1\"/><PAGEOBJECT XPOS=\"0\" YPOS=\"0\" OwnPage=\"0\" PTYPE=\"4\" \
         NEXTITEM=\"0\" BACKITEM=\"-1\"><ITEXT CH=\"Story\"/><para/></PAGEOBJECT>\
         <PAGEOBJECT XPOS=\"0\" YPOS=\"9\" OwnPage=\"0\" PTYPE=\"4\" NEXTITEM=\"-1\" \
         BACKITEM=\"-1\"><ITEXT CH=\"After\"/></PAGEOBJECT>",
    );
    assert_eq!(body, "<p>Story</p>\n<p>After</p>\n");
}

#[test]
fn writes_threads_in_a_circle_once() {
    let body = extract(&format!(
        "{}{}",
        frame(1, 0, 0, 0, (2, 2), "<ITEXT CH=\"One\"/>"),
        frame(2, 0, 0, 9, (1, 1), "<ITEXT CH=\"Two\"/>"),
    ));
    assert_eq!(body, "<p>One</p>\n<p>Two</p>\n");
}

#[test]
fn writes_groups_in_place_and_tables_by_rows() {
    let body = extract(&format!(
        "<PAGEOBJECT XPOS=\"0\" YPOS=\"50\" OwnPage=\"0\" ItemID=\"1\" PTYPE=\"12\">{}{}\
         </PAGEOBJECT>{}<PAGEOBJECT XPOS=\"0\" YPOS=\"90\" OwnPage=\"0\" ItemID=\"4\" \
         PTYPE=\"16\"><TableData Style=\"\"><Cell Row=\"1\" Column=\"0\"><StoryText>\
         <ITEXT CH=\"c\"/></StoryText></Cell><Cell Row=\"0\" Column=\"1\"><StoryText>\
         <ITEXT CH=\"b\"/></StoryText></Cell><Cell Row=\"0\" Column=\"0\"><StoryText>\
         <ITEXT CH=\"a\"/></StoryText></Cell></TableData></PAGEOBJECT>\
         <PAGEOBJECT XPOS=\"0\" YPOS=\"95\" OwnPage=\"0\" ItemID=\"5\" PTYPE=\"6\"/>",
        alone(2, 0, 0, 70, "<ITEXT CH=\"Grouped lower\"/>"),
        alone(3, 0, 0, 60, "<ITEXT CH=\"Grouped upper\"/>"),
        alone(6, 0, 0, 10, "<ITEXT CH=\"Above\"/>"),
    ));
    assert_eq!(
        body,
        "<p>Above</p>\n<p>Grouped upper</p>\n<p>Grouped lower</p>\n\
         <table><tr>\t<td><p>a</p>\n</td>\t<td><p>b</p>\n</td></tr>\n\
         <tr>\t<td><p>c</p>\n</td></tr>\n</table>\n"
    );
}

#[test]
fn writes_anchored_frames_after_their_paragraphs_and_hands_on_inline_images() {
    let image = {
        let mut data = 13u32.to_be_bytes().to_vec();
        data.extend(crate::compress::zlib::tests::zlib(b"Lamp-room log"));
        let mut encoded = Vec::new();
        dijo_core::large::write_base64(&mut encoded, data.as_slice(), b"").unwrap();
        String::from_utf8(encoded).unwrap()
    };
    let body = extract(&format!(
        "<FRAMEOBJECT InID=\"3\" XPOS=\"0\" YPOS=\"0\" OwnPage=\"0\" ItemID=\"30\" PTYPE=\"4\" \
         NEXTITEM=\"-1\" BACKITEM=\"-1\"><StoryText><ITEXT CH=\"Anchored\"/></StoryText>\
         </FRAMEOBJECT><FRAMEOBJECT InID=\"4\" XPOS=\"0\" YPOS=\"0\" OwnPage=\"0\" ItemID=\"40\" \
         PTYPE=\"2\" ANNAME=\"log\" isInlineImage=\"1\" inlineImageExt=\"txt\" \
         ImageData=\"{image}\"/>{}",
        alone(
            1,
            0,
            0,
            0,
            "<ITEXT CH=\"Text \"/><ITEXT Unicode=\"25\" COBJ=\"3\"/><ITEXT CH=\"on\"/><para/>\
             <ITEXT Unicode=\"25\" COBJ=\"4\"/><para/><ITEXT Unicode=\"25\" COBJ=\"8\"/>"
        ),
    ));
    assert_eq!(
        body,
        "<p>Text on</p>\n<p>Anchored</p>\n\
         <div class=\"package-entry\"><h1>log.txt</h1>\n<p>Lamp-room log</p>\n\n</div>\n"
    );
}

#[test]
fn records_inline_images_that_do_not_decompress() {
    let (body, metadata) = extract_with(
        "",
        &format!(
            "<PAGEOBJECT XPOS=\"0\" YPOS=\"0\" OwnPage=\"0\" ItemID=\"1\" PTYPE=\"2\" \
             ANNAME=\"Pic\" isInlineImage=\"1\" inlineImageExt=\"png\" \
             ImageData=\"AAAADXh4eHh4\"/>{}",
            alone(2, 0, 0, 9, "<ITEXT CH=\"Caption\"/>")
        ),
    );
    assert_eq!(body, "<p>Caption</p>\n");
    let exception = &metadata["tk:exception:embedded-stream-exception"][0];
    assert!(exception.contains("\"Pic.png\""), "{exception}");
}

#[test]
fn reads_the_document_information_as_metadata() {
    let (_, metadata) = extract_with(
        "TITLE=\"The Harbour Gazette\" AUTHOR=\"Anna Berg\" SUBJECT=\"Tides\" \
         COMMENTS=\"Printed&#10;weekly\" KEYWORDS=\"harbour, tides\" PUBLISHER=\"Harbour Office\" \
         DOCDATE=\"2026-11-02\" DOCTYPE=\"Text\" DOCIDENT=\" \" LANGUAGE=\"en_GB\"",
        &alone(1, 0, 0, 0, "<ITEXT CH=\"x\"/>"),
    );
    assert_eq!(metadata["dc:title"], ["The Harbour Gazette"]);
    assert_eq!(metadata["dc:creator"], ["Anna Berg"]);
    assert_eq!(metadata["dc:subject"], ["Tides"]);
    assert_eq!(metadata["dc:description"], ["Printed\nweekly"]);
    assert_eq!(metadata["meta:keyword"], ["harbour, tides"]);
    assert_eq!(metadata["dc:publisher"], ["Harbour Office"]);
    assert_eq!(metadata["dcterms:created"], ["2026-11-02"]);
    assert_eq!(metadata["dc:type"], ["Text"]);
    assert_eq!(metadata["dc:language"], ["en-GB"]);
    assert_eq!(metadata["xmpTPg:NPages"], ["1"]);
    assert_eq!(metadata["xmp:CreatorTool"], ["Scribus 1.5.8"]);
    assert!(!metadata.contains_key("dc:identifier"));
    let (_, metadata) = extract_with(
        "DOCLANGINFO=\"da\" LANGUAGE=\"en_GB\"",
        &alone(1, 0, 0, 0, "<ITEXT CH=\"x\"/>"),
    );
    assert_eq!(metadata["dc:language"], ["da"]);
}

#[test]
fn reads_compressed_documents_through_the_gzip_parser() {
    let document = format!(
        "<?xml version=\"1.0\"?>\n<SCRIBUSUTF8NEW Version=\"1.5.8\"><DOCUMENT>{}</DOCUMENT>\
         </SCRIBUSUTF8NEW>",
        alone(1, 0, 0, 0, "<ITEXT CH=\"Compressed\"/>")
    );
    let gzipped = crate::compress::gzip::tests::gzip(document.as_bytes(), None);
    let (text, metadata) = crate::Extractor::new()
        .extract_bytes_to_string(&gzipped)
        .unwrap();
    assert_eq!(metadata["Content-Type"], ["application/gzip"]);
    assert!(text.contains("Compressed"), "{text}");
}

#[test]
fn tells_the_versions_that_write_raw_control_characters() {
    for version in ["1.2.5", "1.3.3.14", "1.3", "1.3.3svn"] {
        assert!(writes_raw_text(version), "{version}");
    }
    for version in ["1.3.4", "1.4.8", "1.5.8", "1.6.0.svn", "2"] {
        assert!(!writes_raw_text(version), "{version}");
    }
}

#[test]
fn leaves_unread_older_documents_with_raw_control_characters() {
    let document = "<?xml version=\"1.0\"?>\n<SCRIBUSUTF8NEW Version=\"1.3.3.14\"><DOCUMENT>\
                    <PAGEOBJECT PTYPE=\"4\" CH=\"One\u{5}Two\"/></DOCUMENT></SCRIBUSUTF8NEW>";
    match Document::read(document.as_bytes()) {
        Err(Decline::Unread(reason)) => assert!(reason.contains("1.3.3.14"), "{reason}"),
        other => panic!("{other:?}"),
    }
    let newer = document.replace("1.3.3.14", "1.5.8");
    assert!(matches!(
        Document::read(newer.as_bytes()),
        Err(Decline::Fails(_))
    ));
}

#[test]
fn fails_on_what_is_not_a_scribus_document() {
    for document in [
        "<abiword/>",
        "<SCRIBUSUTF8NEW><DOCUMENT><PAGEOBJECT XPOS=\"0\"/></DOCUMENT></SCRIBUSUTF8NEW>",
        "<SCRIBUSUTF8NEW><DOCUMENT><PAGEOBJECT PTYPE=\"4\" XPOS=\"left\"/></DOCUMENT>\
         </SCRIBUSUTF8NEW>",
        "<SCRIBUSUTF8NEW><DOCUMENT><PAGEOBJECT PTYPE=\"4\"><PAGEOBJECT PTYPE=\"4\"/>\
         </PAGEOBJECT></DOCUMENT></SCRIBUSUTF8NEW>",
        "<SCRIBUSUTF8NEW><DOCUMENT><PAGEOBJECT PTYPE=\"4\"><Cell Row=\"0\" Column=\"0\"/>\
         </PAGEOBJECT></DOCUMENT></SCRIBUSUTF8NEW>",
    ] {
        assert!(
            matches!(Document::read(document.as_bytes()), Err(Decline::Fails(_))),
            "{document}"
        );
    }
}

#[test]
fn caps_how_deep_frames_nest() {
    let mut content = String::new();
    for _ in 0..=MAX_NESTING + 1 {
        content.push_str("<PAGEOBJECT XPOS=\"0\" YPOS=\"0\" OwnPage=\"0\" PTYPE=\"12\">");
    }
    for _ in 0..=MAX_NESTING + 1 {
        content.push_str("</PAGEOBJECT>");
    }
    let document = format!(
        "<?xml version=\"1.0\"?>\n<SCRIBUSUTF8NEW><DOCUMENT>{content}</DOCUMENT></SCRIBUSUTF8NEW>"
    );
    let extracted = crate::Extractor::new().extract_bytes_to_string(document.as_bytes());
    assert!(extracted.is_err(), "{extracted:?}");
}

/// `gazette.sla` was written by `dijo-small/tests/fixtures/scribus/gazette.py`.
#[test]
fn reads_the_gazette_in_the_order_of_its_pages() {
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_file_to_string("../test_files/documents/gazette.sla")
        .unwrap();
    assert_eq!(metadata["Content-Type"], [MEDIA_TYPE]);
    assert_eq!(metadata["dc:title"], ["The Harbour Gazette"]);
    let mut from = 0;
    for expected in [
        "<p>The Harbour Gazette, issue 12</p>",
        "<h1>The Harbour Gazette</h1>",
        "<h2>Storm warning</h2>",
        "<p>Gales are expected from the north-west on Thursday. The lighthouse keeper",
        "<p>Moorings\tPier\u{a0}3</p>",
        "<p>Harbour Office<br />\nQuay Street 1</p>",
        "<p>The lamp room after repairs.</p>",
        "<h1>lamp-room.png</h1>",
        "<p>Repairs continue next week.</p>",
        "<p>Photo: Anna Berg</p>",
        "<td><p>Tide</p>\n</td>\t<td><p>Time</p>",
        "<td><p>Low tide</p>\n</td>\t<td><p>12:31</p>",
        "<h2>Notices</h2>",
        "<p>The fish market opens at five.</p>",
        "<p>Draft: check the tide times with the harbour master.</p>",
    ] {
        let at = xml[from..]
            .find(expected)
            .unwrap_or_else(|| panic!("{expected:?} after {from} in {xml}"));
        from += at + expected.len();
    }
    assert_eq!(xml.matches("Repairs continue").count(), 1);
}
