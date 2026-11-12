use super::*;

/// The XHTML body and the metadata of the AbiWord document whose `<abiword>` holds `content`.
fn extract(content: &str) -> (String, dijo_core::Metadata) {
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<abiword xmlns=\"{AWML_NAMESPACE}\" \
         xmlns:xlink=\"{XLINK_NAMESPACE}\" xml:space=\"preserve\" fileformat=\"1.1\">\n\
         {content}\n</abiword>"
    );
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(document.as_bytes())
        .unwrap();
    assert_eq!(metadata["Content-Type"], ["application/x-abiword"]);
    let body =
        xml[xml.find("<body>").unwrap() + "<body>".len()..xml.find("</body>").unwrap()].to_owned();
    (body, metadata)
}

#[test]
fn writes_headings_by_their_styles_and_what_they_are_based_on() {
    let (body, _) = extract(
        r#"<styles><s type="P" name="Keeper Heading" basedon="Heading 3"/>
<s type="P" name="Loop A" basedon="Loop B"/><s type="P" name="Loop B" basedon="Loop A"/></styles>
<section><p style="Heading 1">Log</p><p style="Numbered Heading 2">Duties</p>
<p style="Keeper Heading">Lamp</p><p style="Loop A">Plain</p><p style="Heading 9">Deep</p></section>"#,
    );
    assert_eq!(
        body,
        "<h1>Log</h1>\n<h2>Duties</h2>\n<h3>Lamp</h3>\n<p>Plain</p>\n<p>Deep</p>\n"
    );
}

#[test]
fn nests_list_items_by_level_without_their_labels() {
    let (body, _) = extract(
        r#"<lists><l id="1" parentid="0" type="0" start-value="1" list-delim="%L." list-decimal="."/>
<l id="2" parentid="1" type="5" start-value="0" list-delim="%L" list-decimal="NULL"/></lists>
<section><p listid="1" level="1"><field type="list_label"/><c>	One</c></p>
<p listid="2" level="2" parentid="1"><field type="list_label"/><c>	Inside</c></p>
<p listid="1" level="1"><field type="list_label"/><c>	Two</c></p>
<p listid="9" level="1" props="list-style:Upper Roman List"><field type="list_label"/>	Three</p>
<p>After</p></section>"#,
    );
    assert_eq!(
        body,
        "<ol>\t<li>One<ul>\t<li>Inside</li>\n</ul>\n</li>\n\t<li>Two</li>\n</ol>\n\
         <ol>\t<li>Three</li>\n</ol>\n<p>After</p>\n"
    );
}

#[test]
fn writes_footnotes_after_their_paragraphs_and_links_and_breaks_inline() {
    let (body, _) = extract(
        r#"<section><p>Lit<field type="footnote_ref" footnote-id="1"/><foot footnote-id="1">
<p><field type="footnote_anchor" footnote-id="1"/> Oil came on Mondays.</p>
</foot> at dusk. <a xlink:href="https://example.org/">Harbour</a><br/>Next<pbr/>Page	tab</p>
<p></p><p><c> </c></p></section>"#,
    );
    assert_eq!(
        body,
        "<p>Lit at dusk. <a href=\"https://example.org/\">Harbour</a><br />\nNext<br />\n\
         Page\ttab</p><div class=\"footnote\"><p> Oil came on Mondays.</p>\n</div>\n\n"
    );
}

#[test]
fn orders_table_cells_into_rows_by_their_top_attach() {
    let (body, _) = extract(
        r#"<section><p>Before</p><table>
<cell props="bot-attach:1; left-attach:0; right-attach:1; top-attach:0"><p>a</p></cell>
<cell props="bot-attach:1; left-attach:1; right-attach:2; top-attach:0"><p>b</p></cell>
<cell props="bot-attach:2; left-attach:0; right-attach:2; top-attach:1"><p>c</p></cell>
</table></section>"#,
    );
    assert_eq!(
        body,
        "<p>Before</p>\n<table><tr>\t<td><p>a</p>\n</td>\t<td><p>b</p>\n</td></tr>\n\
         <tr>\t<td><p>c</p>\n</td></tr>\n</table>\n"
    );
}

#[test]
fn writes_headers_footers_and_text_boxes_in_divisions() {
    let (body, _) = extract(
        r#"<section header="0" footer="1"><p>Body</p>
<frame props="frame-type:textbox"><p>Boxed</p></frame>
<frame props="frame-type:image; strux-image-dataid:0"/></section>
<section id="0" type="header"><p>Top</p></section>
<section id="1" type="footer-even"><p>Bottom</p></section>"#,
    );
    assert_eq!(
        body,
        "<p>Body</p>\n<div class=\"textbox\"><p>Boxed</p>\n</div>\n\
         <div class=\"header\"><p>Top</p>\n</div>\n<div class=\"footer\"><p>Bottom</p>\n</div>\n"
    );
}

#[test]
fn reads_the_metadata_with_its_dates_as_local_date_times() {
    let (_, metadata) = extract(
        r#"<metadata><m key="dc.title">Log</m><m key="dc.creator">Anna Berg</m>
<m key="abiword.keywords">harbour, lamp</m><m key="abiword.generator">AbiWord</m>
<m key="dc.date">Thu Nov  5 09:04:05 2026
</m><m key="abiword.date_last_changed">last week</m><m key="custom.key">left out</m>
<m key="dc.subject"> </m></metadata><section><p>x</p></section>"#,
    );
    assert_eq!(metadata["dc:title"], ["Log"]);
    assert_eq!(metadata["dc:creator"], ["Anna Berg"]);
    assert_eq!(metadata["meta:keyword"], ["harbour, lamp"]);
    assert_eq!(metadata["xmp:CreatorTool"], ["AbiWord"]);
    assert_eq!(metadata["dcterms:created"], ["2026-11-05T09:04:05"]);
    assert_eq!(metadata["dcterms:modified"], ["last week"]);
    assert!(!metadata.contains_key("dc:subject"));
}

#[test]
fn reads_ctime_dates_and_nothing_else_as_dates() {
    assert_eq!(
        local_date_time("Sat Feb 29 23:59:60 2028").as_deref(),
        Some("2028-02-29T23:59:60")
    );
    for text in [
        "Thu Nov 12 21:04 2026",
        "Thu Nov 32 21:04:05 2026",
        "Thu Noe 12 21:04:05 2026",
        "Thu Nov 12 21:04:05 2026 UTC",
        "Thu Nov +1 21:04:05 2026",
    ] {
        assert_eq!(local_date_time(text), None, "{text}");
    }
}

#[test]
fn hands_each_data_item_to_the_embedded_document_extractor() {
    let (body, _) = extract(
        r#"<section><p><image dataid="lamp"/>Picture</p></section><data>
<d name="lamp" mime-type="text/plain" base64="yes">
TGFt
cCBvaWw=
</d><d name="wick.txt" mime-type="text/plain" base64="no"><![CDATA[Wick & lens]]></d></data>"#,
    );
    assert_eq!(
        body,
        "<p>Picture</p>\n<div class=\"package-entry\"><h1>lamp.txt</h1>\n<p>Lamp oil</p>\n\n</div>\n\
         <div class=\"package-entry\"><h1>wick.txt</h1>\n<p>Wick &amp; lens</p>\n\n</div>\n"
    );
}

#[test]
fn reads_compressed_documents_through_the_gzip_parser() {
    let document = format!(
        "<abiword xmlns=\"{AWML_NAMESPACE}\"><section><p>Compressed</p></section></abiword>"
    );
    let gzipped = crate::compress::gzip::tests::gzip(document.as_bytes(), None);
    let (text, metadata) = crate::Extractor::new()
        .extract_bytes_to_string(&gzipped)
        .unwrap();
    assert_eq!(metadata["Content-Type"], ["application/gzip"]);
    assert!(text.contains("Compressed"), "{text}");
}

#[test]
fn fails_on_cells_without_rows_and_unknown_sections() {
    for content in [
        r#"<section><table><cell props="left-attach:0"><p>a</p></cell></table></section>"#,
        r#"<section type="sidebar"><p>a</p></section>"#,
        r#"<section><p>unclosed</section>"#,
    ] {
        let document = format!(
            "<?xml version=\"1.0\"?>\n<abiword xmlns=\"{AWML_NAMESPACE}\">{content}</abiword>"
        );
        let extracted = crate::Extractor::new().extract_bytes_to_string(document.as_bytes());
        assert!(extracted.is_err(), "{content}: {extracted:?}");
    }
}

#[test]
fn caps_how_deep_lists_nest() {
    let (body, _) = extract(r#"<section><p listid="1" level="4000000000">Deep</p></section>"#);
    assert_eq!(body.matches("<ul>").count(), MAX_LIST_DEPTH);
}

/// A document too large to read whole is written a paragraph at a time under a memory budget
/// far below its size.
#[test]
fn writes_documents_too_large_to_read_whole_a_paragraph_at_a_time() {
    let paragraph =
        |index: u64| format!("<p style=\"Normal\"><c>Line {index}</c></p>\n").into_bytes();
    let count = dijo_core::large::bodies_past_limit(paragraph(0).len());
    let file = dijo_core::large::LargeFile::new(
        "long.abw",
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<abiword xmlns=\"http://www.abisource.com/awml.dtd\" xml:space=\"preserve\"><section>\n",
        count,
        paragraph,
        b"</section></abiword>\n",
    );
    let mut lines = 0;
    file.extract_lines(AbiWordParser.name(), |line| {
        if !line.is_empty() {
            assert_eq!(line, format!("Line {lines}"));
            lines += 1;
        }
    });
    assert_eq!(lines, count);
}
