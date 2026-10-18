#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

use super::*;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_same_as_tika, conformance_corpus, differing_reading, documents, Scratch,
};
#[cfg(feature = "tika")]
use crate::formats::Difference;

#[cfg(feature = "tika")]
const NOTES: &str = r#"# Mødenoter æøå

Første afsnit med *kursiv*, **fed**, ~~slettet~~, `kode` og en [henvisning](https://example.dk "Titel").
En blød linjeskift, og et hårdt
skift. Escaped \* stjerne &amp; entitet &copy;.

* punkt 1
* punkt 2
  * indlejret

1. en
2. to

7) syv

> citat
> fortsat

---

| Navn | Antal | Pris |
|:-----|:-----:|-----:|
| Torsk | 2 | 10,5 |
| Sild \| brisling | 3 |
| `a|b` |

```rust  ekstra info
fn main() {}
```

    indrykket kode

<div>
rå *html*
</div>

Tekst med <span>inline html</span> og <https://example.dk/auto> og <post@example.dk>.

![Billede *med* alt](billede.png "Billedtitel") ![tom](tom.png "") [tom titel](https://a.dk '')

[reference][ref] og [genvej]

[ref]: https://example.dk/ref "Ref-titel"
[genvej]: https://example.dk/genvej
"#;

#[test]
fn reads_titles_and_tables_as_commonmark_java_does() {
    assert_eq!(separator_columns("|:-|-:|---|"), Some(3));
    assert_eq!(separator_columns("--- | ---"), Some(2));
    assert_eq!(separator_columns("---"), None);
    assert_eq!(separator_columns("|| --"), None);
    assert_eq!(row_cells("| a | b |"), 2);
    assert_eq!(row_cells("a | b |  "), 3);
    assert_eq!(row_cells(r"| a \| b |"), 1);
    assert_eq!(lenient_table("a | b\n- | - | -\n"), Some(2));
    assert_eq!(lenient_table("a | b | c\n- | -\n"), None);
    assert!(parse("[a](b \"\") [c](d) [e](f\"\")").1.is_some());
    let (items, difference) = parse("[a](b \"\") [c](d)");
    assert_eq!(difference, None);
    let titles: Vec<Option<String>> = items
        .into_iter()
        .filter_map(|item| match item {
            Item::Link { title, .. } => Some(title),
            _ => None,
        })
        .collect();
    assert_eq!(titles, [Some(String::new()), None]);
}

#[cfg(feature = "tika")]
#[test]
fn extracts_markdown_as_tikas_path_does() {
    let roots = [
        Path::new("../test_files/documents"),
        Path::new("../quality/corpus"),
    ];
    let (parsed, deferred) = documents(&MarkdownParser, &roots);
    assert!(
        parsed.is_empty() && deferred.is_empty(),
        "{parsed:?} {deferred:?}"
    );

    let scratch = Scratch::new("markdown");
    let notes = scratch.write("noter.md", NOTES.as_bytes());
    let latin = scratch.write(
        "latin.md",
        &"# Overskrift\n\nKøbenhavn og Århus, ærlig talt, så længe det varer.\n"
            .chars()
            .map(|character| u8::try_from(character).unwrap())
            .collect::<Vec<u8>>(),
    );
    let data = scratch.write("data.md", b"![x](data:text/plain;base64,SGVq)\n");
    let lenient = scratch.write("lenient.md", b"a | b\n- | - | -\n1 | 2 | 3\n");
    let deep = scratch.write("deep.md", "> ".repeat(70).as_bytes());
    let (parsed, deferred) = documents(&MarkdownParser, &[scratch.directory()]);
    assert_eq!(deferred, [data.clone(), deep.clone(), lenient.clone()]);
    for (path, difference) in [
        (&data, "a data URI"),
        (&deep, "blocks nested as deep"),
        (&lenient, "the delimiter row on line 2"),
    ] {
        let (Difference(reason), _) = differing_reading(&MarkdownParser, path);
        assert!(reason.starts_with(difference), "{path:?}: {reason}");
    }
    let (_, text) = differing_reading(&MarkdownParser, &lenient);
    assert!(text.contains("1 | 2 | 3"), "{text}");
    assert_eq!(parsed, [latin.clone(), notes.clone()]);
    assert_same_as_tika(
        &[notes.as_path(), latin.as_path()],
        MarkdownParser.tika_name(),
    );
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_documents_as_tikas_path_does() {
    let (parsed, deferred) = documents(&MarkdownParser, &[&conformance_corpus()]);
    assert_eq!(deferred.len(), 1, "{deferred:?}");
    assert!(deferred[0].ends_with("testMARKDOWN_dataURI.md"));
    let (Difference(reason), text) = differing_reading(&MarkdownParser, &deferred[0]);
    assert!(reason.starts_with("a data URI"), "{reason}");
    assert!(text.contains("Embedded resources"), "{text}");
    assert_eq!(parsed.len(), 1, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&paths, MarkdownParser.tika_name());
}
