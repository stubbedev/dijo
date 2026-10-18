#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

use super::*;
use crate::compress::zip::file::tests::zip;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
};

#[cfg(feature = "tika")]
const TIKA_NAME: &str = "org.apache.tika.parser.iwork.IWorkPackageParser";
const HISTORY: (&str, &[u8]) = (
    "buildVersionHistory.plist",
    b"<plist version=\"1.0\"><array/></plist>",
);

/// A Keynote '09 presentation of one slide with a title, a body, notes and a table.
const KEYNOTE: &[u8] = br#"<?xml version="1.0"?>
<key:presentation xmlns:key="http://developer.apple.com/namespaces/keynote2" xmlns:sf="http://developer.apple.com/namespaces/sf" xmlns:sfa="http://developer.apple.com/namespaces/sfa">
<key:size sfa:w="1024" sfa:h="768"/>
<key:metadata><key:title><key:string sfa:string="Deck"/></key:title><key:authors><key:string sfa:string="Ann"/><key:string sfa:string="Bo"/></key:authors></key:metadata>
<key:slide-list><key:slide>
<key:title-placeholder><sf:text-body><sf:p>Title &amp; more</sf:p></sf:text-body></key:title-placeholder>
<key:body-placeholder><sf:text-body><sf:p>Body</sf:p></sf:text-body></key:body-placeholder>
<sf:tabular-model sfa:ID="t1"><sf:columns sf:count="2"/><sf:ct sfa:s="a"/><sf:n sf:v="1"/><sf:ct sfa:s="c"/></sf:tabular-model>
<key:notes><sf:text-body><sf:p>Notes</sf:p></sf:text-body></key:notes>
</key:slide></key:slide-list>
</key:presentation>"#;

/// A Pages '09 document of two pages with headers, a footnote and a table of one row of text,
/// with no text between its headers or footers and what follows them: one Tika's handler reads
/// as it is written.
const PAGES: &[u8] = br#"<?xml version="1.0"?>
<sl:document xmlns:sl="http://developer.apple.com/namespaces/sl" xmlns:sf="http://developer.apple.com/namespaces/sf" xmlns:sfa="http://developer.apple.com/namespaces/sfa">
<sl:publication-info><sl:SLCreationDateProperty><sl:date sf:val="2009-01-02"/></sl:SLCreationDateProperty><sl:kind><sl:string sfa:string="wp"/></sl:kind></sl:publication-info>
<sf:metadata><sf:title><sf:string sfa:string="Doc"/></sf:title><sf:authors><sf:string sfa:string="Ann"/></sf:authors></sf:metadata>
<sf:headers><sf:header sf:name="SFWPDefaultOddHeaderIdentifier"><sf:p>Odd head<sf:page-number sf:format="upper-roman"/></sf:p></sf:header><sf:header sf:name="SFWPDefaultFirstHeaderIdentifier"><sf:p>First head</sf:p></sf:header></sf:headers><sf:footers><sf:footer sf:name="SFWPDefaultEvenFooterIdentifier"><sf:p>Even foot<sf:page-number sf:format="lower-alpha"/></sf:p></sf:footer></sf:footers><sf:footnotes><sf:footnote-mark sf:mark="1"/><sf:p>Note one</sf:p></sf:footnotes>
<sf:attachments><sf:attachment sf:kind="tabular-attachment" sfa:ID="tab"><sf:grid sf:numcols="3"/><sf:datasource><sf:t><sf:ct sfa:s="x"/></sf:t><sf:t><sf:ct sfa:s="y"/></sf:t><sf:t><sf:ct sfa:s="z"/></sf:t></sf:datasource></sf:attachment></sf:attachments>
<sf:text-storage><sf:page-start/><sf:p>Hello<sf:footnote sf:autonumber="1"/> world<sf:ghost-text>ghost</sf:ghost-text></sf:p>
<sf:p>Table:<sf:attachment-ref sfa:IDREF="tab"/></sf:p><sf:page-start/><sf:p>Second</sf:p></sf:text-storage>
</sl:document>"#;

/// A Pages '09 document Tika's handler misreads (#132): a header written in pieces and followed
/// by text, a page group before the first page, a table of numbers, dates and empty cells in two
/// columns, a table without an ID, and an annotation without a target.
const MISREAD_PAGES: &[u8] = br#"<?xml version="1.0"?>
<sl:document xmlns:sl="http://developer.apple.com/namespaces/sl" xmlns:sf="http://developer.apple.com/namespaces/sf" xmlns:sfa="http://developer.apple.com/namespaces/sfa">
<sf:headers><sf:header sf:name="SFWPDefaultOddHeaderIdentifier"><sf:p>Havn &amp; b&#248;lge<sf:page-number sf:format="upper-roman"/></sf:p></sf:header>
</sf:headers>
<sf:annotations><sf:annotation><sf:p>Loose</sf:p></sf:annotation></sf:annotations>
<sf:attachments><sf:attachment sf:kind="tabular-attachment" sfa:ID="tab"><sf:grid sf:numcols="2"/><sf:datasource><sf:t><sf:ct sfa:s="a"/></sf:t><sf:n sf:v="1"/><sf:e/><sf:d sf:cell-date="2009-01-02"/><sf:t><sf:ct sfa:s="b"/></sf:t></sf:datasource></sf:attachment>
<sf:attachment sf:kind="tabular-attachment"><sf:grid sf:numcols="1"/><sf:datasource><sf:t><sf:ct sfa:s="lost"/></sf:t></sf:datasource></sf:attachment></sf:attachments>
<sl:drawables><sl:page-group sl:page="1"/></sl:drawables>
<sf:text-storage><sf:page-start/><sf:p>Table:<sf:attachment-ref sfa:IDREF="tab"/><sf:attachment-ref/></sf:p><sf:p><sf:annotation-field>Marked</sf:annotation-field></sf:p></sf:text-storage>
</sl:document>"#;

#[test]
fn types_the_document_by_its_first_content_entry() {
    let (text, metadata) = crate::Extractor::new()
        .extract_bytes_to_string(&zip(&[
            HISTORY,
            ("index.apxl", KEYNOTE),
            ("index.xml", PAGES),
        ]))
        .unwrap();
    assert!(text.contains("Body") && text.contains("Second"), "{text}");
    assert_eq!(metadata["Content-Type"], ["application/vnd.apple.keynote"]);
}

#[test]
fn reads_pages_documents_as_they_are_written() {
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(&zip(&[HISTORY, ("index.xml", MISREAD_PAGES)]))
        .unwrap();
    let body = &xml[xml.find("<body>").unwrap()..];
    assert_eq!(
        body,
        "<body>0<div><div class=\"header\">Havn &amp; bølge\tI</div>\n</div>\n\
         <div><div class=\"header\">Havn &amp; bølge\tI</div>\n\
         <p>Table:</p><table><tr>\t<td>a</td>\t<td>1</td></tr>\n\
         <tr>\t<td />\t<td>2009-01-02</td></tr>\n<tr>\t<td>b</td></tr>\n</table>\n\n\
         <p><div style=\"annotated\">Marked</div>\n</p>\n</div>\n</body></html>"
    );
    assert_eq!(metadata["meta:page-count"], ["1"]);
}

/// A Numbers '09 document of one sheet with a table and a pop-up menu cell.
#[cfg(feature = "tika")]
const NUMBERS: &[u8] = br#"<?xml version="1.0"?>
<ls:document xmlns:ls="http://developer.apple.com/namespaces/ls" xmlns:sf="http://developer.apple.com/namespaces/sf" xmlns:sfa="http://developer.apple.com/namespaces/sfa">
<sf:metadata><sf:title><sf:string sfa:string="Sums"/></sf:title><sf:keywords><sf:string sfa:string="k"/></sf:keywords></sf:metadata>
<ls:workspace ls:workspace-name="Sheet 1"><sf:tabular-model sf:name="Table 1"><sf:grid sf:numcols="2"/>
<sf:menu-choices><sf:t sfa:ID="m1"><sf:ct sfa:s="Yes"/></sf:t></sf:menu-choices>
<sf:ct sfa:s="a"/><sf:n sf:v="2"/><sf:rn sf:v="3"/><sf:proxied-cell-ref sfa:IDREF="m1"/></sf:tabular-model>
<sf:text><sf:p>Box</sf:p></sf:text><sf:chart-name sfa:string="Chart"/></ls:workspace>
</ls:document>"#;

#[test]
fn reads_the_kind_of_each_content_entry() {
    let package = read(&zip(&[
        HISTORY,
        ("index.apxl", KEYNOTE),
        ("other.xml", PAGES),
    ]))
    .unwrap();
    assert!(package.failure.is_none());
    assert!(matches!(
        package.entries.as_slice(),
        [Entry::Document(IworkDocument::Keynote, _)]
    ));
    assert!(read(&zip(&[HISTORY])).unwrap().entries.is_empty());
}

#[cfg(feature = "tika")]
#[test]
fn extracts_synthetic_packages_as_tikas_path_does() {
    let scratch = Scratch::new("iwork-package");
    let read = [
        scratch.write("slides.key", &zip(&[HISTORY, ("index.apxl", KEYNOTE)])),
        scratch.write("text.pages", &zip(&[HISTORY, ("index.xml", PAGES)])),
        scratch.write("sums.numbers", &zip(&[HISTORY, ("index.xml", NUMBERS)])),
        scratch.write("empty.key", &zip(&[HISTORY])),
    ];
    let failing = [scratch.write(
        "broken.key",
        &zip(&[HISTORY, ("index.apxl", &KEYNOTE[..KEYNOTE.len() - 40])]),
    )];
    let (parsed, deferred) = documents(&IworkPackageParser, &[scratch.directory()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), read.len() + failing.len());
    assert_same_as_tika(
        &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
        TIKA_NAME,
    );
    assert_fails_as_tika(&failing.iter().map(PathBuf::as_path).collect::<Vec<_>>());
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_packages_as_tikas_path_does() {
    let (parsed, deferred) = documents(&IworkPackageParser, &[&conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 18, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&paths, TIKA_NAME);
}
