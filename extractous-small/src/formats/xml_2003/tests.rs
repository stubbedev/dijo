use std::path::{Path, PathBuf};

use super::{SpreadsheetMlParser, WordMlParser};
use crate::formats::parity::{
    assert_fails_as_tika, assert_same_as_tika, assert_same_as_tika_departing, conformance_corpus,
    documents, Scratch, ANCHORED_BLOCKS, ANCHORED_BLOCK_DOCUMENTS,
};
use crate::formats::Parser;

const WORD: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<?mso-application progid="Word.Document"?>
<w:wordDocument xmlns:w="http://schemas.microsoft.com/office/word/2003/wordml" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office">
<o:DocumentProperties><o:Title>Havnens  bølger </o:Title><o:Author>Ane</o:Author><o:LastAuthor>Bo</o:LastAuthor><o:Revision>3</o:Revision><o:Created>2026-01-02T03:04:05Z</o:Created><o:Pages>2</o:Pages><o:Words>40</o:Words><o:Company>Havn &amp; Co</o:Company><o:Version>11.6568</o:Version></o:DocumentProperties>
<w:docOleData><w:binData w:name="oledata.mso">SGVqIGZyYSBPTEU=</w:binData></w:docOleData>
<w:body>
<w:p><w:r><w:t>Første afsnit</w:t></w:r><w:r><w:br/><w:t>efter skift</w:t></w:r></w:p>
<w:P><w:r><w:t>Stort P</w:t></w:r><w:p><w:r><w:t>indlejret</w:t></w:r></w:p></w:P>
<w:p><w:r><w:t>før tabel</w:t></w:r><w:tbl><w:tr><w:tc><w:p><w:r><w:t>celle 1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>celle 2</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:p>
<w:p><w:hlink w:dest=" https://example.dk/havn "><w:r><w:t> Havnen </w:t></w:r></w:hlink><w:hlink><w:r><w:t>uden mål</w:t></w:r></w:hlink></w:p>
<w:p><w:r><w:pict><v:shape><v:imagedata src="wordml://02000001.txt" o:title="noter"/></v:shape><w:binData w:name="wordml://02000001.txt">VGVrc3QgaSBiaWxsZWRldA==</w:binData>billedtekst</w:pict></w:r></w:p>
<w:p><w:r><w:pict><w:binData w:name="wordml://tom.txt"></w:binData></w:pict></w:r></w:p>
</w:body>
</w:wordDocument>
"#;

const SPREADSHEET: &str = r#"<?xml version="1.0"?>
<?mso-application progid="Excel.Sheet"?>
<Workbook xmlns="urn:schemas-microsoft-com:office:spreadsheet" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet">
 <DocumentProperties xmlns="urn:schemas-microsoft-com:office:office"><Author>Ane</Author><Author>Bo</Author><Title>Fangst</Title><LastSaved>2026-02-03T04:05:06Z</LastSaved></DocumentProperties>
 <Worksheet ss:Name="Fangst 2026">
  <Table>
   <Row><Cell><Data ss:Type="String"> Art </Data></Cell><Cell><Data ss:Type="String">Tons</Data></Cell></Row>
   <Row><Cell ss:HRef="https://example.dk/torsk"><Data ss:Type="String">Torsk</Data></Cell><Cell><Data ss:Type="Number">12.5</Data></Cell></Row>
   <Row><Cell><Data ss:Type="String">Sild &amp; brisling</Data></Cell><Cell/></Row>
  </Table>
 </Worksheet>
 <Worksheet>
  <Table><Row><Cell><Data ss:Type="String">Tom side</Data></Cell></Row></Table>
 </Worksheet>
</Workbook>
"#;

#[test]
fn extracts_office_2003_xml_as_tikas_path_does() {
    let roots = [
        Path::new("../test_files/documents"),
        Path::new("../quality/corpus"),
    ];
    for parser in [&WordMlParser as &dyn Parser, &SpreadsheetMlParser] {
        let (parsed, deferred) = documents(parser, &roots);
        assert!(
            parsed.is_empty() && deferred.is_empty(),
            "{parsed:?} {deferred:?}"
        );
    }

    let scratch = Scratch::new("xml-2003");
    let word = scratch.write("word.xml", WORD.as_bytes());
    let spreadsheet = scratch.write("spreadsheet.xml", SPREADSHEET.as_bytes());
    let typed = scratch.write(
        "typed.xml",
        WORD.replace(
            "<w:wordDocument ",
            "<!DOCTYPE w:wordDocument [<!ELEMENT w:p ANY>]><w:wordDocument ",
        )
        .as_bytes(),
    );
    let cut = scratch.write(
        "cut.xml",
        br#"<?xml version="1.0"?><w:wordDocument xmlns:w="http://schemas.microsoft.com/office/word/2003/wordml"><w:body><w:p><w:r><w:t>kort</w:t></w:r></w:p>"#,
    );
    let (parsed, deferred) = documents(&WordMlParser, &[scratch.directory()]);
    assert_eq!(deferred, [typed]);
    assert_eq!(parsed, [cut.clone(), word.clone()]);
    let (parsed, _) = documents(&SpreadsheetMlParser, &[scratch.directory()]);
    assert_eq!(parsed, std::slice::from_ref(&spreadsheet));
    assert_same_as_tika(&[word.as_path()], WordMlParser.tika_name());
    assert_same_as_tika(&[spreadsheet.as_path()], SpreadsheetMlParser.tika_name());
    assert_fails_as_tika(&[cut.as_path()]);
}

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_office_2003_xml_as_tikas_path_does() {
    let corpus = conformance_corpus();
    let (parsed, deferred) = documents(&WordMlParser, &[&corpus]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 3, "{parsed:?}");
    let (anchored, paths): (Vec<&Path>, Vec<&Path>) =
        parsed.iter().map(PathBuf::as_path).partition(|path| {
            ANCHORED_BLOCK_DOCUMENTS
                .iter()
                .any(|name| path.ends_with(name))
        });
    assert_eq!(anchored.len(), 1, "{parsed:?}");
    assert_same_as_tika(&paths, WordMlParser.tika_name());
    assert_same_as_tika_departing(&anchored, WordMlParser.tika_name(), ANCHORED_BLOCKS);

    let (parsed, deferred) = documents(&SpreadsheetMlParser, &[&corpus]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 1, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&paths, SpreadsheetMlParser.tika_name());
}
