use std::path::{Path, PathBuf};

use super::*;
use crate::compress::zip::file::tests::zip;
use crate::formats::parity::{
    assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
};

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

/// A Pages '09 document of two pages with headers, a footnote and a table.
const PAGES: &[u8] = br#"<?xml version="1.0"?>
<sl:document xmlns:sl="http://developer.apple.com/namespaces/sl" xmlns:sf="http://developer.apple.com/namespaces/sf" xmlns:sfa="http://developer.apple.com/namespaces/sfa">
<sl:publication-info><sl:SLCreationDateProperty><sl:date sf:val="2009-01-02"/></sl:SLCreationDateProperty><sl:kind><sl:string sfa:string="wp"/></sl:kind></sl:publication-info>
<sf:metadata><sf:title><sf:string sfa:string="Doc"/></sf:title><sf:authors><sf:string sfa:string="Ann"/></sf:authors></sf:metadata>
<sf:headers><sf:header sf:name="SFWPDefaultOddHeaderIdentifier"><sf:p>Odd head<sf:page-number sf:format="upper-roman"/></sf:p></sf:header><sf:header sf:name="SFWPDefaultFirstHeaderIdentifier"><sf:p>First head</sf:p></sf:header></sf:headers>
<sf:footers><sf:footer sf:name="SFWPDefaultEvenFooterIdentifier"><sf:p>Even foot<sf:page-number sf:format="lower-alpha"/></sf:p></sf:footer></sf:footers>
<sf:footnotes><sf:footnote-mark sf:mark="1"/><sf:p>Note one</sf:p></sf:footnotes>
<sf:attachments><sf:attachment sf:kind="tabular-attachment" sfa:ID="tab"><sf:grid sf:numcols="3"/><sf:ct sfa:s="x"/><sf:ct sfa:s="y"/><sf:ct sfa:s="z"/><sf:ct sfa:s="w"/></sf:attachment></sf:attachments>
<sf:text-storage><sf:page-start/><sf:p>Hello<sf:footnote sf:autonumber="1"/> world<sf:ghost-text>ghost</sf:ghost-text></sf:p>
<sf:p>Table:<sf:attachment-ref sfa:IDREF="tab"/></sf:p><sf:page-start/><sf:p>Second</sf:p></sf:text-storage>
</sl:document>"#;

/// A Numbers '09 document of one sheet with a table and a pop-up menu cell.
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

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_packages_as_tikas_path_does() {
    let (parsed, deferred) = documents(&IworkPackageParser, &[&conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 18, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&paths, TIKA_NAME);
}
